//! Maps to: CC `utils/envValidation.ts`.
//!
//! Environment variable validation helpers shared by Doctor, shell output
//! limits, and task output formatting. Invalid and capped values are logged
//! as `${name} ${message}` to the debug log, as CC does.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnvVarValidationStatus {
    Valid,
    Capped,
    Invalid,
}

impl EnvVarValidationStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Valid => "valid",
            Self::Capped => "capped",
            Self::Invalid => "invalid",
        }
    }
}

/// Maps to CC `utils/envValidation.ts#EnvVarValidationResult`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvVarValidationResult {
    pub effective: usize,
    pub status: EnvVarValidationStatus,
    pub message: Option<String>,
}

/// Maps to CC `utils/envValidation.ts#validateBoundedIntEnvVar`.
pub fn validate_bounded_int_env_var(
    name: &str,
    value: Option<&str>,
    default_value: usize,
    upper_limit: usize,
) -> EnvVarValidationResult {
    let Some(value) = value else {
        return EnvVarValidationResult {
            effective: default_value,
            status: EnvVarValidationStatus::Valid,
            message: None,
        };
    };

    // JS `if (!value)` treats the empty string as absent; whitespace-only
    // strings remain truthy and then fail parseInt below.
    if value.is_empty() {
        return EnvVarValidationResult {
            effective: default_value,
            status: EnvVarValidationStatus::Valid,
            message: None,
        };
    }

    // `isNaN(parsed) || parsed <= 0`.
    let Some(parsed) = parse_js_decimal_int_prefix(value).filter(|parsed| *parsed > 0) else {
        let message = format!("Invalid value \"{value}\" (using default: {default_value})");
        crate::utils::debug::log_for_debugging(&format!("{name} {message}"));
        return EnvVarValidationResult {
            effective: default_value,
            status: EnvVarValidationStatus::Invalid,
            message: Some(message),
        };
    };

    if parsed > upper_limit as i64 {
        let message = format!("Capped from {parsed} to {upper_limit}");
        crate::utils::debug::log_for_debugging(&format!("{name} {message}"));
        return EnvVarValidationResult {
            effective: upper_limit,
            status: EnvVarValidationStatus::Capped,
            message: Some(message),
        };
    }

    EnvVarValidationResult {
        effective: parsed as usize,
        status: EnvVarValidationStatus::Valid,
        message: None,
    }
}

/// Mirrors JS `parseInt(value, 10)` for the decimal-prefix cases used by the
/// official validator: leading whitespace and sign are accepted, parsing stops
/// at the first non-digit, and missing digits yields NaN.
fn parse_js_decimal_int_prefix(value: &str) -> Option<i64> {
    let trimmed = value.trim_start();
    let mut chars = trimmed.chars().peekable();
    let sign = match chars.peek().copied() {
        Some('+') => {
            chars.next();
            1i64
        }
        Some('-') => {
            chars.next();
            -1i64
        }
        _ => 1i64,
    };

    let mut digits = String::new();
    while let Some(ch) = chars.peek().copied() {
        if ch.is_ascii_digit() {
            digits.push(ch);
            chars.next();
        } else {
            break;
        }
    }

    if digits.is_empty() {
        return None;
    }
    digits.parse::<i64>().ok().map(|value| value * sign)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_bounded_int_env_var_matches_official_absent_and_empty_behavior() {
        assert_eq!(
            validate_bounded_int_env_var("X", None, 30_000, 150_000),
            EnvVarValidationResult {
                effective: 30_000,
                status: EnvVarValidationStatus::Valid,
                message: None,
            }
        );
        assert_eq!(
            validate_bounded_int_env_var("X", Some(""), 30_000, 150_000).status,
            EnvVarValidationStatus::Valid
        );
    }

    #[test]
    fn validate_bounded_int_env_var_matches_official_invalid_and_cap_messages() {
        assert_eq!(
            validate_bounded_int_env_var("X", Some("abc"), 30_000, 150_000),
            EnvVarValidationResult {
                effective: 30_000,
                status: EnvVarValidationStatus::Invalid,
                message: Some("Invalid value \"abc\" (using default: 30000)".to_string()),
            }
        );
        assert_eq!(
            validate_bounded_int_env_var("X", Some("0"), 30_000, 150_000).message,
            Some("Invalid value \"0\" (using default: 30000)".to_string())
        );
        assert_eq!(
            validate_bounded_int_env_var("X", Some("200000"), 30_000, 150_000),
            EnvVarValidationResult {
                effective: 150_000,
                status: EnvVarValidationStatus::Capped,
                message: Some("Capped from 200000 to 150000".to_string()),
            }
        );
    }

    #[test]
    fn validate_bounded_int_env_var_uses_js_parse_int_prefix() {
        assert_eq!(
            validate_bounded_int_env_var("X", Some("42abc"), 30_000, 150_000).effective,
            42
        );
        assert_eq!(
            validate_bounded_int_env_var("X", Some("  +7"), 30_000, 150_000).effective,
            7
        );
        assert_eq!(
            validate_bounded_int_env_var("X", Some("  -7"), 30_000, 150_000).status,
            EnvVarValidationStatus::Invalid
        );
        assert_eq!(
            validate_bounded_int_env_var("X", Some("   "), 30_000, 150_000).status,
            EnvVarValidationStatus::Invalid
        );
    }
}
