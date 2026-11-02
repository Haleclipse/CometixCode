//! Node BufferEncoding carrier used by FsOperations string reads.
use base64::{Engine as _, engine::general_purpose};

/// All distinct Node BufferEncoding decoders. Alias parsing preserves utf8/utf-8,
/// ucs2/ucs-2/utf16le/utf-16le and binary/latin1 equivalence.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BufferEncoding {
    #[default]
    Utf8,
    Utf16Le,
    Latin1,
    Ascii,
    Base64,
    Base64Url,
    Hex,
}
impl BufferEncoding {
    pub fn parse(value: &str) -> std::io::Result<Self> {
        match value.to_ascii_lowercase().as_str() {
            "utf8" | "utf-8" => Ok(Self::Utf8),
            "utf16le" | "utf-16le" | "ucs2" | "ucs-2" => Ok(Self::Utf16Le),
            "latin1" | "binary" => Ok(Self::Latin1),
            "ascii" => Ok(Self::Ascii),
            "base64" => Ok(Self::Base64),
            "base64url" => Ok(Self::Base64Url),
            "hex" => Ok(Self::Hex),
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("Unknown encoding: {value}"),
            )),
        }
    }
    pub(super) fn decode(self, bytes: &[u8]) -> String {
        match self {
            Self::Utf8 => String::from_utf8_lossy(bytes).into_owned(),
            Self::Utf16Le => String::from_utf16_lossy(
                &bytes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|p| u16::from_le_bytes([p[0], p[1]]))
                    .collect::<Vec<_>>(),
            ),
            Self::Latin1 => bytes.iter().map(|b| char::from(*b)).collect(),
            Self::Ascii => bytes.iter().map(|b| char::from(b & 0x7f)).collect(),
            Self::Base64 => general_purpose::STANDARD.encode(bytes),
            Self::Base64Url => general_purpose::URL_SAFE_NO_PAD.encode(bytes),
            Self::Hex => bytes.iter().map(|b| format!("{b:02x}")).collect(),
        }
    }
}

/// Lossless JavaScript string carrier. Node's utf16le decoder preserves lone
/// surrogates, which cannot be represented by a Rust String.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FsText(pub Vec<u16>);
impl FsText {
    pub fn code_units(&self) -> &[u16] {
        &self.0
    }
    pub fn to_string_lossy(&self) -> String {
        String::from_utf16_lossy(&self.0)
    }
    pub fn into_string(self) -> Result<String, std::string::FromUtf16Error> {
        String::from_utf16(&self.0)
    }
}
impl From<&str> for FsText {
    fn from(value: &str) -> Self {
        Self(value.encode_utf16().collect())
    }
}
impl From<String> for FsText {
    fn from(value: String) -> Self {
        Self::from(value.as_str())
    }
}
impl PartialEq<&str> for FsText {
    fn eq(&self, value: &&str) -> bool {
        self.0.iter().copied().eq(value.encode_utf16())
    }
}
impl BufferEncoding {
    pub(super) fn decode_text(self, bytes: &[u8]) -> FsText {
        if self == Self::Utf16Le {
            FsText(
                bytes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                    .collect(),
            )
        } else {
            FsText::from(self.decode(bytes))
        }
    }
}
