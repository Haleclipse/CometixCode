//! Read tool output limits.
//!
//! Maps to: CC `tools/FileReadTool/limits.ts`.

use crate::utils::file::MAX_OUTPUT_SIZE;
use crate::utils::process_env::JsTruthy;
use std::sync::OnceLock;

/// Maps to: CC `DEFAULT_MAX_OUTPUT_TOKENS`.
pub const DEFAULT_MAX_OUTPUT_TOKENS: f64 = 25_000.0;

/// Maps to: CC `FileReadingLimits`.
///
/// JavaScript accepts every positive finite GrowthBook number, including
/// fractions. Keep those values as numbers until the concrete byte/token
/// comparison instead of truncating them at configuration load time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FileReadingLimits {
    pub max_tokens: f64,
    pub max_size_bytes: f64,
    pub include_max_size_in_prompt: Option<bool>,
    pub targeted_range_nudge: Option<bool>,
}

/// Env override for max output tokens.
/// Maps to: CC `getEnvMaxTokens` (:24-33), including `parseInt(..., 10)`
/// decimal-prefix behavior and JavaScript Number overflow.
fn get_env_max_tokens() -> Option<f64> {
    let override_value =
        crate::utils::process_env::var("CLAUDE_CODE_FILE_READ_MAX_OUTPUT_TOKENS").truthy()?;
    let trimmed = override_value.trim_start_matches(|character| {
        matches!(
            character,
            '\u{0009}'..='\u{000d}'
                | '\u{0020}'
                | '\u{00a0}'
                | '\u{1680}'
                | '\u{2000}'..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
        )
    });
    let (negative, digits_source) = match trimmed.as_bytes().first() {
        Some(b'+') => (false, &trimmed[1..]),
        Some(b'-') => (true, &trimmed[1..]),
        _ => (false, trimmed),
    };
    let digits = digits_source
        .as_bytes()
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .copied()
        .collect::<Vec<_>>();
    if digits.is_empty() || negative {
        return None;
    }
    let parsed = std::str::from_utf8(&digits).ok()?.parse::<f64>().ok()?;
    (!parsed.is_nan() && parsed > 0.0).then_some(parsed)
}

/// Maps to CC `getDefaultFileReadingLimits` (:57-90): memoized env > override
/// map > hardcoded precedence, with every feature field validated
/// independently. Cometix resolves the `tengu_amber_wren` override from the
/// source-controlled switch table instead of GrowthBook.
pub fn get_default_file_reading_limits() -> FileReadingLimits {
    static LIMITS: OnceLock<FileReadingLimits> = OnceLock::new();
    *LIMITS.get_or_init(|| {
        let override_value = crate::utils::feature_flags::feature_enabled(
            crate::utils::feature_flags::FeatureFlag::FileReadLimitsOverride,
        )
        .then(serde_json::Map::new);
        let override_value = override_value.as_ref();
        let max_size_bytes = override_value
            .and_then(|value| value.get("maxSizeBytes"))
            .and_then(serde_json::Value::as_f64)
            .filter(|number| number.is_finite() && *number > 0.0)
            .unwrap_or(MAX_OUTPUT_SIZE as f64);
        let max_tokens = get_env_max_tokens()
            .or_else(|| {
                override_value
                    .and_then(|value| value.get("maxTokens"))
                    .and_then(serde_json::Value::as_f64)
                    .filter(|number| number.is_finite() && *number > 0.0)
            })
            .unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS);
        let include_max_size_in_prompt = override_value
            .and_then(|value| value.get("includeMaxSizeInPrompt"))
            .and_then(serde_json::Value::as_bool);
        let targeted_range_nudge = override_value
            .and_then(|value| value.get("targetedRangeNudge"))
            .and_then(serde_json::Value::as_bool);
        FileReadingLimits {
            max_tokens,
            max_size_bytes,
            include_max_size_in_prompt,
            targeted_range_nudge,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[test]
    fn default_limits_match_official_positive_caps() {
        let limits = get_default_file_reading_limits();
        // Cached GrowthBook or env may override either cap in the process.
        assert!(limits.max_size_bytes > 0.0);
        assert!(limits.max_tokens > 0.0);
    }

    #[test]
    fn env_parser_matches_official_javascript_parse_int_prefix_semantics() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let _max = EnvVarGuard::preserve("CLAUDE_CODE_FILE_READ_MAX_OUTPUT_TOKENS");
        let parse = |value: &str| {
            crate::utils::process_env::set("CLAUDE_CODE_FILE_READ_MAX_OUTPUT_TOKENS", value);
            get_env_max_tokens()
        };
        assert_eq!(parse("  +123abc"), Some(123.0));
        assert_eq!(parse("1.5"), Some(1.0));
        assert_eq!(parse("0"), None);
        assert_eq!(parse("-3"), None);
        assert_eq!(parse("abc"), None);
        assert_eq!(parse("\u{feff}+7x"), Some(7.0));
        assert_eq!(parse("\u{0085}7"), None);
        assert!(parse(&"9".repeat(400)).is_some_and(f64::is_infinite));
    }
}
