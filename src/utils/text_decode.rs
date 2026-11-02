//! Encoding detection and strict decoding shared by Read, Grep and the
//! retrieval index.
//!
//! @nyan: new module (docs/nyan/tool-io.md T5). CC decodes every file with
//! `String::from_utf8_lossy`, so a GBK/Big5 file comes back as replacement
//! characters that look like a successful read. This module never guesses
//! noisily: a file either decodes cleanly under a determinate encoding (with
//! a note naming it) or the caller gets a named error.
//!
//! Order of evidence (design T5):
//! 1. explicit `encoding` argument → strict decode, failure is an error;
//! 2. BOM (UTF-8 / UTF-16LE/BE / UTF-32LE/BE);
//! 3. NUL byte in the first 8 KiB → binary;
//! 4. strict UTF-8;
//! 5. binary magic numbers;
//! 6. one nominated legacy candidate that strict-decodes and round-trips;
//! 7. fallback list, adopted only when exactly one candidate decodes.

/// Bytes inspected for the NUL/binary sniff (design T5 step 3).
pub const SNIFF_LIMIT: usize = 8 * 1024;
/// Below this many non-ASCII bytes there is not enough evidence to trust a
/// detector nomination (design T5 step 6).
const MIN_NON_ASCII_BYTES: usize = 32;
/// chardetng is fed at most this much; beyond it the marginal accuracy is not
/// worth scanning a large file on every Read.
const DETECTOR_SAMPLE_LIMIT: usize = 128 * 1024;

/// Fallback candidates tried, in order, when nomination is inconclusive.
const FALLBACK_LABELS: [&[u8]; 5] = [b"gbk", b"big5", b"shift_jis", b"euc-kr", b"windows-1252"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedText {
    pub text: String,
    /// WHATWG-style label of the source encoding (`utf-8`, `gbk`, ...).
    pub encoding: String,
    /// `(Note: decoded from gbk; output is UTF-8.)` when detection (rather
    /// than a BOM or strict UTF-8) chose the encoding.
    pub note: Option<String>,
}

impl DecodedText {
    /// CC's `String::from_utf8_lossy` behavior, kept for the switch-off path.
    pub fn lossy(bytes: &[u8]) -> Self {
        Self::utf8(String::from_utf8_lossy(bytes).into_owned())
    }

    fn utf8(text: String) -> Self {
        Self {
            text,
            encoding: "utf-8".to_string(),
            note: None,
        }
    }

    fn detected(text: String, encoding: &'static str) -> Self {
        Self {
            note: Some(format!("(Note: decoded from {encoding}; output is UTF-8.)")),
            text,
            encoding: encoding.to_string(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// The explicit label is not a WHATWG encoding label.
    UnsupportedEncoding { encoding: String },
    /// The explicit encoding produced invalid sequences (or a replacement).
    ExplicitFailed { encoding: String },
    /// Content sniffing says the file is not text.
    Binary { kind: &'static str },
    /// Several encodings decode cleanly and there is no way to choose.
    Ambiguous { candidates: Vec<&'static str> },
    /// No supported encoding decodes the bytes cleanly.
    Undecodable,
}

impl DecodeError {
    /// Message text with the file path substituted, so the caller does not
    /// have to reimplement the copy.
    pub fn message(&self, label: &str) -> String {
        match self {
            Self::UnsupportedEncoding { encoding } => format!(
                "Unsupported encoding \"{encoding}\" for {label}. Use a WHATWG encoding label (e.g. \"gbk\", \"utf-16le\")."
            ),
            Self::ExplicitFailed { encoding } => format!(
                "Cannot decode {label} as {encoding} without replacement characters. Retry with another encoding, or use view=\"hex\" to inspect the bytes."
            ),
            Self::Binary { kind } => format!(
                "Cannot read {label}: it appears to be a binary file ({kind}). Use view=\"hex\" to inspect it."
            ),
            Self::Ambiguous { candidates } => format!(
                "Cannot determine the encoding of {label} with confidence: decodes cleanly as {}. Retry with encoding=\"{}\".",
                candidates.join(", "),
                candidates.first().copied().unwrap_or("gbk"),
            ),
            Self::Undecodable => format!(
                "Cannot determine the encoding of {label}: it does not decode cleanly as UTF-8 or any supported legacy encoding. Use view=\"hex\" to inspect it."
            ),
        }
    }
}

/// Decode `bytes` per the T5 order. `explicit` is the caller's encoding label
/// (strict, failure is an error); `label` is the path used in messages.
pub fn decode_bytes(
    bytes: &[u8],
    explicit: Option<&str>,
    label: &str,
) -> Result<DecodedText, DecodeError> {
    decode_bytes_with(bytes, explicit, label, true)
}

/// [`decode_bytes`] with the content-binary sniff switchable
/// (`nyan.toolIo.contentBinaryCheck`). `label` currently only reaches the
/// caller through its own error copy; keep the parameter so the signature
/// does not churn when a step starts naming the file.
pub fn decode_bytes_with(
    bytes: &[u8],
    explicit: Option<&str>,
    _label: &str,
    check_binary: bool,
) -> Result<DecodedText, DecodeError> {
    if let Some(explicit) = explicit {
        let encoding =
            encoding_rs::Encoding::for_label(explicit.trim().as_bytes()).ok_or_else(|| {
                DecodeError::UnsupportedEncoding {
                    encoding: explicit.to_string(),
                }
            })?;
        let text = encoding
            .decode_without_bom_handling_and_without_replacement(bytes)
            .ok_or_else(|| DecodeError::ExplicitFailed {
                encoding: explicit.to_string(),
            })?;
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
        return Ok(DecodedText::utf8(text.to_string()));
    }

    if let Some(decoded) = decode_bom(bytes)? {
        return Ok(decoded);
    }

    let sniff = &bytes[..bytes.len().min(SNIFF_LIMIT)];
    if check_binary {
        // @nyan: magic runs before the strict-UTF-8 check. Some binary
        // prefixes (ELF's `\x7fELF\x02\x01`, PE's `MZ`) are valid UTF-8
        // control sequences, so the design's step order (UTF-8 at 4, magic at
        // 5) would classify a short binary header as text.
        if let Some(kind) = binary_kind(bytes) {
            return Err(DecodeError::Binary { kind });
        }
    } else if sniff.contains(&0) {
        // Even with content binary checking off, a NUL in the sniff window
        // means the strict decoders below cannot succeed; report it as binary
        // rather than an encoding guess.
        return Err(DecodeError::Binary { kind: "NUL bytes" });
    }

    if let Ok(text) = std::str::from_utf8(bytes) {
        return Ok(DecodedText::utf8(text.to_string()));
    }

    // The caller has already excluded binary content; from here on we only
    // adopt an encoding for which every byte decodes and re-encodes exactly.
    if non_ascii_bytes(&sniff[..sniff.len().min(DETECTOR_SAMPLE_LIMIT)]) >= MIN_NON_ASCII_BYTES {
        let mut detector = chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Deny);
        detector.feed(&bytes[..bytes.len().min(DETECTOR_SAMPLE_LIMIT)], true);
        let nominated = detector.guess(None, chardetng::Utf8Detection::Deny);
        if nominated != encoding_rs::UTF_8 {
            let name = canonical_label(nominated);
            if let Some(text) = strict_roundtrip(nominated, bytes) {
                return Ok(DecodedText::detected(text, name));
            }
        }
    }

    let mut candidates = Vec::new();
    for label in FALLBACK_LABELS {
        let Some(encoding) = encoding_rs::Encoding::for_label(label) else {
            continue;
        };
        if strict_roundtrip(encoding, bytes).is_some() {
            candidates.push(canonical_label(encoding));
        }
    }
    match candidates.as_slice() {
        [] => Err(DecodeError::Undecodable),
        [only] => {
            let encoding = encoding_rs::Encoding::for_label(only.as_bytes())
                .expect("canonical label round-trips through encoding_rs");
            Ok(DecodedText::detected(
                strict_roundtrip(encoding, bytes).expect("candidate decoded above"),
                only,
            ))
        }
        many => Err(DecodeError::Ambiguous {
            candidates: many.to_vec(),
        }),
    }
}

/// Decode one line of ripgrep output (already isolated by rg). Returns `None`
/// when the bytes cannot be decoded determinately; Grep then counts the file
/// under `skipped (encoding)` rather than printing replacement characters.
pub fn decode_line(bytes: &[u8]) -> Option<DecodedText> {
    if let Ok(text) = std::str::from_utf8(bytes) {
        return Some(DecodedText::utf8(text.to_string()));
    }
    decode_bytes(bytes, None, "match line").ok()
}

/// Incremental decoder for the streaming Read path (files ≥ 10 MiB and FIFOs).
///
/// The encoding is settled from the first chunk: an explicit label, a BOM, a
/// binary sniff, a clean UTF-8 prefix, or a nominated legacy encoding that
/// decodes the prefix without a malformed sequence. Once chosen it does not
/// change, and a malformed sequence later in the stream is an error rather
/// than a replacement character — the "never emit mojibake" rule holds here
/// too.
pub struct StreamingDecoder {
    inner: StreamingInner,
    encoding: &'static str,
    note: Option<String>,
    first_chunk: bool,
}

enum StreamingInner {
    /// Every encoding `encoding_rs` implements except UTF-32.
    Encoding(encoding_rs::Decoder),
    /// UTF-32 has no `encoding_rs` decoder; a fixed-width decoder buffers a
    /// partial code unit across chunk boundaries.
    Utf32 {
        little_endian: bool,
        pending: Vec<u8>,
    },
}

impl StreamingDecoder {
    /// `chunk` is the first non-empty read. `explicit` wins outright; `label`
    /// is the path used in error messages.
    pub fn new(
        chunk: &[u8],
        explicit: Option<&str>,
        label: &str,
        check_binary: bool,
    ) -> Result<Self, DecodeError> {
        if let Some(explicit) = explicit {
            let encoding = encoding_rs::Encoding::for_label(explicit.trim().as_bytes())
                .ok_or_else(|| DecodeError::UnsupportedEncoding {
                    encoding: explicit.to_string(),
                })?;
            return Ok(Self {
                inner: StreamingInner::Encoding(encoding.new_decoder_without_bom_handling()),
                encoding: canonical_label(encoding),
                note: None,
                first_chunk: true,
            });
        }

        if chunk.starts_with(&[0xFF, 0xFE, 0x00, 0x00]) {
            return Ok(Self::utf32(true));
        }
        if chunk.starts_with(&[0x00, 0x00, 0xFE, 0xFF]) {
            return Ok(Self::utf32(false));
        }
        if chunk.starts_with(&[0xFF, 0xFE]) {
            return Ok(Self::with_encoding(encoding_rs::UTF_16LE, "utf-16le"));
        }
        if chunk.starts_with(&[0xFE, 0xFF]) {
            return Ok(Self::with_encoding(encoding_rs::UTF_16BE, "utf-16be"));
        }
        if check_binary && let Some(kind) = binary_kind(chunk) {
            return Err(DecodeError::Binary { kind });
        }
        if valid_utf8_prefix(chunk) {
            return Ok(Self::with_encoding(encoding_rs::UTF_8, "utf-8"));
        }
        let encoding = nominate(chunk, label)?;
        Ok(Self {
            inner: StreamingInner::Encoding(encoding.new_decoder_without_bom_handling()),
            encoding: canonical_label(encoding),
            note: Some(format!(
                "(Note: decoded from {}; output is UTF-8.)",
                canonical_label(encoding)
            )),
            first_chunk: true,
        })
    }

    fn with_encoding(encoding: &'static encoding_rs::Encoding, label: &'static str) -> Self {
        Self {
            inner: StreamingInner::Encoding(encoding.new_decoder_without_bom_handling()),
            encoding: label,
            note: None,
            first_chunk: true,
        }
    }

    fn utf32(little_endian: bool) -> Self {
        Self {
            inner: StreamingInner::Utf32 {
                little_endian,
                pending: Vec::new(),
            },
            encoding: if little_endian {
                "utf-32le"
            } else {
                "utf-32be"
            },
            note: None,
            first_chunk: true,
        }
    }

    pub fn note(&self) -> Option<&str> {
        self.note.as_deref()
    }

    pub fn encoding_label(&self) -> &'static str {
        self.encoding
    }

    pub fn decode(&mut self, bytes: &[u8]) -> Result<String, DecodeError> {
        if self.first_chunk {
            self.first_chunk = false;
            // The UTF-8 BOM belongs to the stream, not the text. For every
            // other source encoding the BOM was consumed by `new`.
            let bytes = if self.encoding == "utf-8" {
                bytes.strip_prefix(&[0xEF, 0xBB, 0xBF][..]).unwrap_or(bytes)
            } else {
                bytes
            };
            return self.decode_inner(bytes, false);
        }
        self.decode_inner(bytes, false)
    }

    pub fn finish(&mut self) -> Result<String, DecodeError> {
        self.decode_inner(&[], true)
    }

    fn decode_inner(&mut self, bytes: &[u8], last: bool) -> Result<String, DecodeError> {
        match &mut self.inner {
            StreamingInner::Encoding(decoder) => {
                let mut output = String::new();
                let mut remaining = bytes;
                loop {
                    let worst_case = decoder
                        .max_utf8_buffer_length(remaining.len())
                        .ok_or(DecodeError::Undecodable)?;
                    output.reserve(worst_case.max(8));
                    let (result, read) =
                        decoder.decode_to_string_without_replacement(remaining, &mut output, last);
                    remaining = &remaining[read..];
                    match result {
                        encoding_rs::DecoderResult::InputEmpty => return Ok(output),
                        encoding_rs::DecoderResult::OutputFull => continue,
                        encoding_rs::DecoderResult::Malformed(_, _) => {
                            return Err(DecodeError::Undecodable);
                        }
                    }
                }
            }
            StreamingInner::Utf32 {
                little_endian,
                pending,
            } => {
                pending.extend_from_slice(bytes);
                let complete = pending.len() / 4 * 4;
                let mut output = String::with_capacity(complete / 2);
                for chunk in pending[..complete].as_chunks::<4>().0 {
                    let value = if *little_endian {
                        u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]])
                    } else {
                        u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]])
                    };
                    let character = char::from_u32(value).ok_or(DecodeError::Undecodable)?;
                    output.push(character);
                }
                pending.drain(..complete);
                if last && !pending.is_empty() {
                    return Err(DecodeError::Undecodable);
                }
                Ok(output)
            }
        }
    }
}

fn valid_utf8_prefix(chunk: &[u8]) -> bool {
    match std::str::from_utf8(chunk) {
        Ok(_) => true,
        Err(error) => {
            // An incomplete sequence at the very end of the prefix is not
            // evidence against UTF-8; anything else is.
            error.error_len().is_none() && error.valid_up_to() + 3 >= chunk.len()
        }
    }
}

fn nominate(chunk: &[u8], label: &str) -> Result<&'static encoding_rs::Encoding, DecodeError> {
    let sample = &chunk[..chunk.len().min(DETECTOR_SAMPLE_LIMIT)];
    if non_ascii_bytes(sample) >= MIN_NON_ASCII_BYTES {
        let mut detector = chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Deny);
        detector.feed(sample, true);
        let nominated = detector.guess(None, chardetng::Utf8Detection::Deny);
        if nominated != encoding_rs::UTF_8 && decodes_cleanly(nominated, sample) {
            return Ok(nominated);
        }
    }
    let mut candidates = Vec::new();
    for candidate in FALLBACK_LABELS {
        let Some(encoding) = encoding_rs::Encoding::for_label(candidate) else {
            continue;
        };
        if decodes_cleanly(encoding, sample) {
            candidates.push(encoding);
        }
    }
    match candidates.as_slice() {
        [] => {
            let _ = label;
            Err(DecodeError::Undecodable)
        }
        [only] => Ok(only),
        many => Err(DecodeError::Ambiguous {
            candidates: many
                .iter()
                .map(|encoding| canonical_label(encoding))
                .collect(),
        }),
    }
}

/// True when every complete sequence decodes. A trailing partial sequence is
/// buffered by the decoder instead of being reported malformed, so a chunk cut
/// through a multi-byte character does not disqualify its encoding.
fn decodes_cleanly(encoding: &'static encoding_rs::Encoding, bytes: &[u8]) -> bool {
    let mut decoder = encoding.new_decoder_without_bom_handling();
    let mut output = String::new();
    let mut remaining = bytes;
    loop {
        let Some(worst_case) = decoder.max_utf8_buffer_length(remaining.len()) else {
            return false;
        };
        output.reserve(worst_case.max(8));
        let (result, read) =
            decoder.decode_to_string_without_replacement(remaining, &mut output, false);
        remaining = &remaining[read..];
        match result {
            encoding_rs::DecoderResult::InputEmpty => return true,
            encoding_rs::DecoderResult::OutputFull => continue,
            encoding_rs::DecoderResult::Malformed(_, _) => return false,
        }
    }
}

/// Content-level binary sniff used by Read (design T6). `Some(kind)` means
/// the content is binary and `view="hex"` is the way to inspect it.
pub fn binary_kind(bytes: &[u8]) -> Option<&'static str> {
    // A known magic number is more specific than the NUL heuristic: WebAssembly
    // starts with `\0asm` and SQLite embeds NUL in its header.
    if let Some(kind) = binary_magic(bytes) {
        return Some(kind);
    }
    if bytes[..bytes.len().min(SNIFF_LIMIT)].contains(&0) {
        return Some("NUL bytes");
    }
    None
}

fn binary_magic(bytes: &[u8]) -> Option<&'static str> {
    const PREFIXES: &[(&[u8], &str)] = &[
        (b"PK\x03\x04", "ZIP archive"),
        (b"PK\x05\x06", "ZIP archive"),
        (b"PK\x07\x08", "ZIP archive"),
        (b"\x1f\x8b", "gzip stream"),
        (b"\x7fELF", "ELF executable"),
        (b"MZ", "PE executable"),
        (b"\xfe\xed\xfa\xce", "Mach-O binary"),
        (b"\xce\xfa\xed\xfe", "Mach-O binary"),
        (b"\xfe\xed\xfa\xcf", "Mach-O binary"),
        (b"\xcf\xfa\xed\xfe", "Mach-O binary"),
        (b"\xca\xfe\xba\xbe", "Mach-O universal binary"),
        (b"SQLite format 3\0", "SQLite database"),
        (b"\0asm", "WebAssembly module"),
    ];
    for (prefix, kind) in PREFIXES {
        if bytes.starts_with(prefix) {
            return Some(kind);
        }
    }
    if bytes.len() >= 262 && &bytes[257..262] == b"ustar" {
        return Some("tar archive");
    }
    None
}

fn non_ascii_bytes(bytes: &[u8]) -> usize {
    bytes.iter().filter(|byte| **byte >= 0x80).count()
}

fn strict_roundtrip(encoding: &'static encoding_rs::Encoding, bytes: &[u8]) -> Option<String> {
    let text = encoding.decode_without_bom_handling_and_without_replacement(bytes)?;
    let (encoded, _, had_errors) = encoding.encode(&text);
    (!had_errors && encoded.as_ref() == bytes).then(|| text.into_owned())
}

/// Lowercase WHATWG spelling used in notes and error copy.
fn canonical_label(encoding: &'static encoding_rs::Encoding) -> &'static str {
    match encoding.name() {
        "UTF-8" => "utf-8",
        "UTF-16LE" => "utf-16le",
        "UTF-16BE" => "utf-16be",
        "GBK" => "gbk",
        "Big5" => "big5",
        "Shift_JIS" => "shift_jis",
        "EUC-KR" => "euc-kr",
        "windows-1252" => "windows-1252",
        "GB18030" => "gb18030",
        other => other,
    }
}

fn decode_bom(bytes: &[u8]) -> Result<Option<DecodedText>, DecodeError> {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        let text = std::str::from_utf8(&bytes[3..]).map_err(|_| DecodeError::Undecodable)?;
        return Ok(Some(DecodedText::utf8(text.to_string())));
    }
    // UTF-32 BOMs must be checked before the UTF-16LE/BE prefixes because
    // `FF FE 00 00` starts with the UTF-16LE BOM.
    if bytes.starts_with(&[0xFF, 0xFE, 0x00, 0x00]) {
        return Ok(Some(decode_utf32(&bytes[4..], true)?));
    }
    if bytes.starts_with(&[0x00, 0x00, 0xFE, 0xFF]) {
        return Ok(Some(decode_utf32(&bytes[4..], false)?));
    }
    if bytes.starts_with(&[0xFF, 0xFE]) {
        let text = encoding_rs::UTF_16LE
            .decode_without_bom_handling_and_without_replacement(&bytes[2..])
            .ok_or(DecodeError::Undecodable)?;
        return Ok(Some(DecodedText::utf8(text.into_owned())));
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        let text = encoding_rs::UTF_16BE
            .decode_without_bom_handling_and_without_replacement(&bytes[2..])
            .ok_or(DecodeError::Undecodable)?;
        return Ok(Some(DecodedText::utf8(text.into_owned())));
    }
    Ok(None)
}

fn decode_utf32(bytes: &[u8], little_endian: bool) -> Result<DecodedText, DecodeError> {
    if !bytes.len().is_multiple_of(4) {
        return Err(DecodeError::Undecodable);
    }
    let mut text = String::with_capacity(bytes.len() / 2);
    for chunk in bytes.as_chunks::<4>().0 {
        let value = if little_endian {
            u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]])
        } else {
            u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]])
        };
        let character = char::from_u32(value).ok_or(DecodeError::Undecodable)?;
        text.push(character);
    }
    Ok(DecodedText::utf8(text))
}

/// Content-level binary sniff used by Read (design T6). `Some(kind)` means

#[cfg(test)]
mod tests {
    use super::*;
    use encoding_rs::{BIG5, GBK, SHIFT_JIS};

    fn encode(encoding: &'static encoding_rs::Encoding, text: &str) -> Vec<u8> {
        encoding.encode(text).0.into_owned()
    }

    #[test]
    fn strict_utf8_and_bom_paths() {
        let decoded = decode_bytes(b"hello\n", None, "a.txt").unwrap();
        assert_eq!(decoded.text, "hello\n");
        assert_eq!(decoded.encoding, "utf-8");
        assert_eq!(decoded.note, None);

        let decoded = decode_bytes(b"\xEF\xBB\xBFhello", None, "a.txt").unwrap();
        assert_eq!(decoded.text, "hello");

        let mut utf16le = vec![0xFF, 0xFE];
        utf16le.extend("hi".encode_utf16().flat_map(u16::to_le_bytes));
        let decoded = decode_bytes(&utf16le, None, "a.txt").unwrap();
        assert_eq!(decoded.text, "hi");

        let mut utf16be = vec![0xFE, 0xFF];
        utf16be.extend("hi".encode_utf16().flat_map(u16::to_be_bytes));
        let decoded = decode_bytes(&utf16be, None, "a.txt").unwrap();
        assert_eq!(decoded.text, "hi");

        let mut utf32le = vec![0xFF, 0xFE, 0x00, 0x00];
        utf32le.extend("hi".chars().flat_map(|c| (c as u32).to_le_bytes()));
        let decoded = decode_bytes(&utf32le, None, "a.txt").unwrap();
        assert_eq!(decoded.text, "hi");
    }

    #[test]
    fn binary_sniffing_reports_magic_and_nul() {
        assert_eq!(
            decode_bytes(b"\x7fELF\x02\x01", None, "a.bin").unwrap_err(),
            DecodeError::Binary {
                kind: "ELF executable"
            }
        );
        assert_eq!(
            decode_bytes(b"abc\0def", None, "a.bin").unwrap_err(),
            DecodeError::Binary { kind: "NUL bytes" }
        );
        assert_eq!(
            decode_bytes(b"PK\x03\x04rest", None, "a.zip").unwrap_err(),
            DecodeError::Binary {
                kind: "ZIP archive"
            }
        );
        assert_eq!(binary_kind(b"\0asm\x01\0\0\0"), Some("WebAssembly module"));
        assert_eq!(binary_kind(b"plain text"), None);
    }

    #[test]
    fn legacy_encodings_decode_with_a_note() {
        let text = "你好，世界。这是一段用于检测编码的测试文本，需要足够的非 ASCII 字节。";
        let gbk = encode(GBK, text);
        let decoded = decode_bytes(&gbk, None, "a.txt").unwrap();
        assert_eq!(decoded.text, text);
        assert_eq!(decoded.encoding, "gbk");
        assert_eq!(
            decoded.note.as_deref(),
            Some("(Note: decoded from gbk; output is UTF-8.)")
        );

        let big5_text = "你好，世界。這是一段用於偵測編碼的測試文字，需要足夠的非 ASCII 位元組。";
        let big5 = encode(BIG5, big5_text);
        let decoded = decode_bytes(&big5, None, "a.txt").unwrap();
        assert_eq!(decoded.text, big5_text);
        assert_eq!(decoded.encoding, "big5");

        let sjis_text = "こんにちは世界。これは文字コード判定のためのテスト文章です。十分な非ASCIIバイトが必要です。";
        let sjis = encode(SHIFT_JIS, sjis_text);
        let decoded = decode_bytes(&sjis, None, "a.txt").unwrap();
        assert_eq!(decoded.text, sjis_text);
        assert_eq!(decoded.encoding, "shift_jis");
    }

    #[test]
    fn explicit_encoding_is_strict_and_never_replaces() {
        let text = "你好，世界";
        let gbk = encode(GBK, text);
        let decoded = decode_bytes(&gbk, Some("gbk"), "a.txt").unwrap();
        assert_eq!(decoded.text, text);
        // Explicit decoding carries no detection note.
        assert_eq!(decoded.note, None);

        let error = decode_bytes(&gbk, Some("utf-8"), "a.txt").unwrap_err();
        assert!(matches!(error, DecodeError::ExplicitFailed { .. }));
        assert!(error.message("a.txt").contains("view=\"hex\""));

        let error = decode_bytes(b"x", Some("not-an-encoding"), "a.txt").unwrap_err();
        assert!(matches!(
            error,
            DecodeError::UnsupportedEncoding { ref encoding } if encoding == "not-an-encoding"
        ));
    }

    #[test]
    fn insufficient_evidence_reports_ambiguity_or_single_candidate() {
        // Too few non-ASCII bytes for the detector; several legacy encodings
        // decode this cleanly, so the answer is the named-candidate error.
        let bytes = [0xA1u8, 0xA1].repeat(8);
        let error = decode_bytes(&bytes, None, "a.txt").unwrap_err();
        match error {
            DecodeError::Ambiguous { candidates } => {
                assert!(candidates.contains(&"gbk"), "{candidates:?}");
                assert!(candidates.len() > 1);
            }
            other => panic!("expected ambiguity, got {other:?}"),
        }

        // 0x81 followed by 0x20 fails every multi-byte candidate; only
        // windows-1252 accepts it, so it is adopted with the source note.
        let bytes = [0x81u8, 0x20].repeat(2);
        let decoded = decode_bytes(&bytes, None, "a.txt").unwrap();
        assert_eq!(decoded.encoding, "windows-1252");
        assert_eq!(
            decoded.note.as_deref(),
            Some("(Note: decoded from windows-1252; output is UTF-8.)")
        );
    }

    #[test]
    fn decode_line_skips_ambiguous_bytes() {
        assert_eq!(decode_line(b"plain").unwrap().text, "plain");
        let long = "你好，世界。这是一段用于检测编码的测试文本，需要足够的非 ASCII 字节。";
        assert_eq!(decode_line(&encode(GBK, long)).unwrap().text, long);
        // Same ambiguity rule as whole-file decoding: too little evidence and
        // several clean decodes means no guess at all.
        assert!(decode_line(&encode(GBK, "你好世界")).is_none());
        assert!(decode_line(&[0xA1u8, 0xA1].repeat(8)).is_none());
    }

    #[test]
    fn error_messages_name_the_path_and_next_step() {
        let message = DecodeError::Ambiguous {
            candidates: vec!["gbk", "big5"],
        }
        .message("src/a.txt");
        assert_eq!(
            message,
            "Cannot determine the encoding of src/a.txt with confidence: decodes cleanly as gbk, big5. Retry with encoding=\"gbk\"."
        );
        assert!(
            DecodeError::Binary {
                kind: "ELF executable"
            }
            .message("a.out")
            .contains("view=\"hex\"")
        );
    }
}
