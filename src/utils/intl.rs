//! Maps to: CC `utils/intl.ts:10-51` (the shared segmenter functions).
//!
//! CC's cached `Intl.Segmenter` is JavaScriptCore's, which wraps the ICU the
//! runtime was linked against. The Rust counterpart is ICU4X's `icu_segmenter`:
//! the same UAX #29 rules plus the dictionary models for the scripts that need
//! them (Chinese, Japanese, Khmer, Lao, Myanmar, Thai), compiled in as data
//! rather than resolved against a library on the host.
//!
//! That choice is deliberate and it is a deviation worth naming. The previous
//! implementation bound to the *installed* ICU through `rust_icu`, which made
//! behaviour a function of whichever ICU the machine happened to carry — the
//! tests below still record one such drift, where ICU 74 and ICU 78 disagree on
//! the colon rule. Compiled data trades "matches whatever the host has" for
//! "identical everywhere, including a Windows box with no ICU DLLs at all".
//! ICU4X is already this project's ICU for collation (`icu_collator`), so this
//! also collapses two ICU stacks into one.
//!
//! The segmenters are locale-invariant, so unlike the JSC path there is no
//! process-locale lookup: UAX #29 word and grapheme rules do not vary by
//! locale, and the dictionary models are selected by script, not by language.

use std::sync::OnceLock;

use icu_segmenter::options::WordBreakInvariantOptions;
use icu_segmenter::{GraphemeClusterSegmenter, WordSegmenter};

/// Native representation of Intl.SegmentData; indices are UTF-8 byte offsets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segment<'a> {
    pub segment: &'a str,
    pub index: usize,
    pub is_word_like: Option<bool>,
}

/// Which UAX #29 boundary set a [`Segmenter`] reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Granularity {
    Grapheme,
    Word,
}

/// Native runtime descriptor for Intl.Segmenter. Segmentation policy stays in
/// ICU4X; this only selects which of the two shared segmenters to drive.
#[derive(Debug)]
pub struct Segmenter {
    granularity: Granularity,
}

impl Segmenter {
    /// Native implementation of Intl.Segmenter.prototype.segment.
    ///
    /// ICU4X reports boundaries as UTF-8 byte offsets directly, so unlike the
    /// JSC/ICU path there is no UTF-16 index to translate back.
    ///
    /// The segmenters are constructed here rather than cached in a `static`:
    /// the `*Borrowed` constructors only take a reference to data already
    /// compiled into the binary, and the borrowed handles are not `Sync`.
    pub fn segment<'a>(&self, text: &'a str) -> Vec<Segment<'a>> {
        let mut segments = Vec::new();
        match self.granularity {
            Granularity::Word => {
                // `new_dictionary`, not `new_auto`. Both carry the dictionary
                // for Chinese and Japanese, but `auto` switches Thai, Lao,
                // Khmer and Burmese to the LSTM model, whose boundaries differ
                // from ICU4C's — measured here as `กาแฟสวัสดี` coming back
                // whole instead of splitting into `กาแฟ` / `สวัสดี`. The
                // dictionary matches what CC's ICU produces.
                let segmenter = WordSegmenter::new_dictionary(WordBreakInvariantOptions::default());
                let mut iterator = segmenter.segment_str(text);
                let mut start = match iterator.next() {
                    Some(first) => first,
                    None => return segments,
                };
                while let Some(end) = iterator.next() {
                    segments.push(Segment {
                        segment: &text[start..end],
                        index: start,
                        // Mirrors JSC IntlSegmentDataObject::create, which marks
                        // the number/letter/kana/ideographic rule ranges as word
                        // like and leaves whitespace and punctuation false.
                        is_word_like: Some(iterator.is_word_like()),
                    });
                    start = end;
                }
            }
            Granularity::Grapheme => {
                let segmenter = GraphemeClusterSegmenter::new();
                let mut iterator = segmenter.segment_str(text);
                let mut start = match iterator.next() {
                    Some(first) => first,
                    None => return segments,
                };
                for end in iterator {
                    segments.push(Segment {
                        segment: &text[start..end],
                        index: start,
                        is_word_like: None,
                    });
                    start = end;
                }
            }
        }
        segments
    }
}

/// Maps to: CC `utils/intl.ts:13-21` getGraphemeSegmenter.
pub fn get_grapheme_segmenter() -> &'static Segmenter {
    static SEGMENTER: OnceLock<Segmenter> = OnceLock::new();
    SEGMENTER.get_or_init(|| Segmenter {
        granularity: Granularity::Grapheme,
    })
}

/// Maps to: CC `utils/intl.ts:26-31` firstGrapheme.
pub fn first_grapheme(text: &str) -> &str {
    if text.is_empty() {
        return "";
    }
    get_grapheme_segmenter()
        .segment(text)
        .first()
        .map_or("", |segment| segment.segment)
}

/// Maps to: CC `utils/intl.ts:37-44` lastGrapheme.
pub fn last_grapheme(text: &str) -> &str {
    if text.is_empty() {
        return "";
    }
    get_grapheme_segmenter()
        .segment(text)
        .last()
        .map_or("", |segment| segment.segment)
}

/// Maps to: CC `utils/intl.ts:46-51` getWordSegmenter.
pub fn get_word_segmenter() -> &'static Segmenter {
    static SEGMENTER: OnceLock<Segmenter> = OnceLock::new();
    SEGMENTER.get_or_init(|| Segmenter {
        granularity: Granularity::Word,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::EnvVarGuard;

    /// Segmentation must not vary with the process locale.
    ///
    /// The JSC path read the process C locale because `Intl.Segmenter(undefined)`
    /// resolves one; ICU4X's segmenters are locale-invariant by construction, so
    /// what used to be a locale-matching test is now an independence test. UAX #29
    /// word rules do not vary by language, and the dictionary models are chosen by
    /// script.
    #[test]
    fn word_segmentation_is_independent_of_the_process_locale() {
        let baseline = describe(get_word_segmenter().segment("hello:world 你好世界"));
        let _guard = EnvVarGuard::set("LC_ALL", "ja_JP.UTF-8");
        assert_eq!(
            describe(get_word_segmenter().segment("hello:world 你好世界")),
            baseline
        );
    }

    fn describe(segments: Vec<Segment<'_>>) -> Vec<(String, usize, Option<bool>)> {
        segments
            .into_iter()
            .map(|part| (part.segment.to_string(), part.index, part.is_word_like))
            .collect()
    }

    #[test]
    fn cached_segmenters_have_independent_iteration_state_across_threads() {
        assert!(std::ptr::eq(get_word_segmenter(), get_word_segmenter()));
        assert!(std::ptr::eq(
            get_grapheme_segmenter(),
            get_grapheme_segmenter()
        ));
        let first = get_word_segmenter().segment("你好 世界");
        let other = std::thread::spawn(|| {
            get_word_segmenter()
                .segment("こんにちは世界")
                .into_iter()
                .map(|part| (part.segment.to_string(), part.index, part.is_word_like))
                .collect::<Vec<_>>()
        })
        .join()
        .unwrap();
        assert_eq!(first[0].segment, "你好");
        assert_eq!(first[2].index, 7);
        assert_eq!(
            other,
            vec![
                ("こんにちは".into(), 0, Some(true)),
                ("世界".into(), 15, Some(true))
            ]
        );
    }

    #[test]
    fn first_last_grapheme_use_original_empty_combining_and_emoji_contract() {
        assert_eq!(first_grapheme(""), "");
        assert_eq!(last_grapheme(""), "");
        assert_eq!(first_grapheme("e\u{301} x"), "e\u{301}");
        assert_eq!(last_grapheme("x👩‍💻"), "👩‍💻");
        assert_eq!(
            get_grapheme_segmenter()
                .segment("A👩‍💻B")
                .iter()
                .map(|s| s.index)
                .collect::<Vec<_>>(),
            vec![0, 1, 12]
        );
    }
    #[test]
    fn word_segments_match_bun_oracle_with_accepted_icu78_differences() {
        // Executed rebuild/src/utils/intl.ts on Bun/system ICU; every
        // segment (including non-words) is checked, not a script heuristic.
        // Accepted 2026-09-13: the colon case pins native ICU78 instead;
        // all other fixtures retain the actual Bun source result.
        let cases: &[(&str, &[(&str, usize, bool)])] = &[
            ("", &[]),
            (
                "你好 世界",
                &[("你好", 0, true), (" ", 6, false), ("世界", 7, true)],
            ),
            ("你好世界", &[("你好", 0, true), ("世界", 6, true)]),
            (
                "私は学生です",
                &[
                    ("私", 0, true),
                    ("は", 3, true),
                    ("学生", 6, true),
                    ("です", 12, true),
                ],
            ),
            (
                "こんにちは世界",
                &[("こんにちは", 0, true), ("世界", 15, true)],
            ),
            (
                "안녕하세요 세계",
                &[
                    ("안녕하세요", 0, true),
                    (" ", 15, false),
                    ("세계", 16, true),
                ],
            ),
            (
                "alpha-beta",
                &[("alpha", 0, true), ("-", 5, false), ("beta", 6, true)],
            ),
            // Bun/ICU74: [("hello", 0, true), (":", 5, false), ("world", 6, true)].
            // User-accepted native ICU78 word rule.
            ("hello:world", &[("hello:world", 0, true)]),
            (
                "can't foo_bar 3.14",
                &[
                    ("can't", 0, true),
                    (" ", 5, false),
                    ("foo_bar", 6, true),
                    (" ", 13, false),
                    ("3.14", 14, true),
                ],
            ),
            (
                "élan café",
                &[("élan", 0, true), (" ", 6, false), ("café", 7, true)],
            ),
            ("A👩‍💻B", &[("A", 0, true), ("👩‍💻", 1, false), ("B", 12, true)]),
            (
                "👨‍👩‍👧‍👦 hello",
                &[("👨‍👩‍👧‍👦", 0, false), (" ", 25, false), ("hello", 26, true)],
            ),
            (
                "👩‍💻你好，世界！",
                &[
                    ("👩‍💻", 0, false),
                    ("你好", 11, true),
                    ("，", 17, false),
                    ("世界", 20, true),
                    ("！", 26, false),
                ],
            ),
            (
                "[Image #12] x",
                &[
                    ("[", 0, false),
                    ("Image", 1, true),
                    (" ", 6, false),
                    ("#", 7, false),
                    ("12", 8, true),
                    ("]", 10, false),
                    (" ", 11, false),
                    ("x", 12, true),
                ],
            ),
            (
                "foo\nbar baz",
                &[
                    ("foo", 0, true),
                    ("\n", 3, false),
                    ("bar", 4, true),
                    (" ", 7, false),
                    ("baz", 8, true),
                ],
            ),
            (
                "ภาษาไทยภาษาไทย",
                &[
                    ("ภาษา", 0, true),
                    ("ไทย", 12, true),
                    ("ภาษา", 21, true),
                    ("ไทย", 33, true),
                ],
            ),
            ("កម្ពុជាភាសាខ្មែរ", &[("កម្ពុជា", 0, true), ("ភាសាខ្មែរ", 21, true)]),
            (
                "… — 👩‍💻",
                &[
                    ("…", 0, false),
                    (" ", 3, false),
                    ("—", 4, false),
                    (" ", 7, false),
                    ("👩‍💻", 8, false),
                ],
            ),
            (
                "中文abc def",
                &[
                    ("中文", 0, true),
                    ("abc", 6, true),
                    (" ", 9, false),
                    ("def", 10, true),
                ],
            ),
            ("กาแฟสวัสดี", &[("กาแฟ", 0, true), ("สวัสดี", 12, true)]),
        ];
        let mut differences = Vec::new();
        for (text, expected) in cases {
            let actual = get_word_segmenter()
                .segment(text)
                .into_iter()
                .map(|s| (s.segment, s.index, s.is_word_like.unwrap()))
                .collect::<Vec<_>>();
            if &actual != expected {
                differences.push(format!(
                    "text={text:?}: actual={actual:?}, Bun oracle / accepted ICU78 expectation={expected:?}"
                ));
            }
        }
        assert!(differences.is_empty(), "{}", differences.join("\n"));
    }
}
