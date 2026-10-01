//! Bash timeout configuration.
//!
//! Maps to: CC `utils/timeouts.ts`.

use crate::utils::process_env::JsTruthy;

const DEFAULT_TIMEOUT_MS: u64 = 120_000;
const MAX_TIMEOUT_MS: u64 = 600_000;

fn js_parse_int_base10_positive(value: &str) -> Option<u64> {
    let value = value
        .trim_start_matches(|character: char| character.is_whitespace() || character == '\u{feff}');
    let (negative, digits_source) = match value.as_bytes().first() {
        Some(b'+') => (false, &value[1..]),
        Some(b'-') => (true, &value[1..]),
        _ => (false, value),
    };
    if negative {
        return None;
    }
    let digits = digits_source
        .bytes()
        .take_while(u8::is_ascii_digit)
        .collect::<Vec<_>>();
    if digits.is_empty() {
        return None;
    }
    let parsed = digits.into_iter().fold(0u64, |value, digit| {
        value
            .saturating_mul(10)
            .saturating_add(u64::from(digit - b'0'))
    });
    (parsed > 0).then_some(parsed)
}

pub fn get_default_bash_timeout_ms_from_value(value: Option<&str>) -> u64 {
    value
        .and_then(js_parse_int_base10_positive)
        .unwrap_or(DEFAULT_TIMEOUT_MS)
}

/// Maps to CC `getDefaultBashTimeoutMs(env)`.
pub fn get_default_bash_timeout_ms() -> u64 {
    get_default_bash_timeout_ms_from_value(
        crate::utils::process_env::var("BASH_DEFAULT_TIMEOUT_MS")
            .truthy()
            .as_deref(),
    )
}

pub fn get_max_bash_timeout_ms_from_values(maximum: Option<&str>, default: Option<&str>) -> u64 {
    let default = get_default_bash_timeout_ms_from_value(default);
    maximum
        .and_then(js_parse_int_base10_positive)
        .unwrap_or(MAX_TIMEOUT_MS)
        .max(default)
}

/// Maps to CC `getMaxBashTimeoutMs(env)`.
pub fn get_max_bash_timeout_ms() -> u64 {
    let maximum = crate::utils::process_env::var("BASH_MAX_TIMEOUT_MS").truthy();
    let default = crate::utils::process_env::var("BASH_DEFAULT_TIMEOUT_MS").truthy();
    get_max_bash_timeout_ms_from_values(maximum.as_deref(), default.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_values_match_js_parse_int_and_max_floor() {
        assert_eq!(get_default_bash_timeout_ms_from_value(None), 120_000);
        assert_eq!(
            get_default_bash_timeout_ms_from_value(Some("  +2500ms")),
            2_500
        );
        assert_eq!(get_default_bash_timeout_ms_from_value(Some("0")), 120_000);
        assert_eq!(get_default_bash_timeout_ms_from_value(Some("-1")), 120_000);
        assert_eq!(
            get_max_bash_timeout_ms_from_values(Some("1000"), Some("2500")),
            2_500
        );
        assert_eq!(
            get_max_bash_timeout_ms_from_values(None, Some("700000")),
            700_000
        );
    }
}
