//! Maps to: CC `utils/shell/outputLimits.ts`.

use crate::utils::env_validation::validate_bounded_int_env_var;

pub const BASH_MAX_OUTPUT_UPPER_LIMIT: usize = 150_000;
pub const BASH_MAX_OUTPUT_DEFAULT: usize = 30_000;

/// Maps to CC `utils/shell/outputLimits.ts#getMaxOutputLength`.
pub fn get_max_output_length() -> usize {
    let value = crate::utils::process_env::var("BASH_MAX_OUTPUT_LENGTH");
    validate_bounded_int_env_var(
        "BASH_MAX_OUTPUT_LENGTH",
        value.as_deref(),
        BASH_MAX_OUTPUT_DEFAULT,
        BASH_MAX_OUTPUT_UPPER_LIMIT,
    )
    .effective
}

/// Test seam for the official `process.env.BASH_MAX_OUTPUT_LENGTH` call site.
pub fn get_max_output_length_from_value(value: Option<&str>) -> usize {
    validate_bounded_int_env_var(
        "BASH_MAX_OUTPUT_LENGTH",
        value,
        BASH_MAX_OUTPUT_DEFAULT,
        BASH_MAX_OUTPUT_UPPER_LIMIT,
    )
    .effective
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_output_length_uses_official_default_cap_and_parse_int() {
        assert_eq!(
            get_max_output_length_from_value(None),
            BASH_MAX_OUTPUT_DEFAULT
        );
        assert_eq!(get_max_output_length_from_value(Some("42abc")), 42);
        assert_eq!(
            get_max_output_length_from_value(Some("999999")),
            BASH_MAX_OUTPUT_UPPER_LIMIT
        );
        assert_eq!(
            get_max_output_length_from_value(Some("bad")),
            BASH_MAX_OUTPUT_DEFAULT
        );
    }
}
