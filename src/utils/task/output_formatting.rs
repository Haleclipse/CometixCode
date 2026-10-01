//! Maps to: CC `utils/task/outputFormatting.ts`.

use crate::utils::env_validation::validate_bounded_int_env_var;

pub const TASK_MAX_OUTPUT_UPPER_LIMIT: usize = 160_000;
pub const TASK_MAX_OUTPUT_DEFAULT: usize = 32_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormattedTaskOutput {
    pub content: String,
    pub was_truncated: bool,
}

/// Maps to CC `utils/task/outputFormatting.ts#getMaxTaskOutputLength`.
pub fn get_max_task_output_length() -> usize {
    let value = crate::utils::process_env::var("TASK_MAX_OUTPUT_LENGTH");
    get_max_task_output_length_from_value(value.as_deref())
}

/// Test seam for the official `process.env.TASK_MAX_OUTPUT_LENGTH` call site.
pub fn get_max_task_output_length_from_value(value: Option<&str>) -> usize {
    validate_bounded_int_env_var(
        "TASK_MAX_OUTPUT_LENGTH",
        value,
        TASK_MAX_OUTPUT_DEFAULT,
        TASK_MAX_OUTPUT_UPPER_LIMIT,
    )
    .effective
}

/// Maps to CC `utils/task/outputFormatting.ts#formatTaskOutput`.
pub fn format_task_output(output: &str, task_id: &str) -> FormattedTaskOutput {
    let max_len = get_max_task_output_length();
    format_task_output_with_max_len(output, task_id, max_len)
}

pub fn format_task_output_with_max_len(
    output: &str,
    task_id: &str,
    max_len: usize,
) -> FormattedTaskOutput {
    if utf16_len(output) <= max_len {
        return FormattedTaskOutput {
            content: output.to_string(),
            was_truncated: false,
        };
    }

    let file_path = crate::utils::task::disk_output::get_task_output_path(task_id);
    let header = format!("[Truncated. Full output: {}]\n\n", file_path.display());
    let available_space = max_len.saturating_sub(utf16_len(&header));
    let truncated = utf16_suffix(output, available_space);

    FormattedTaskOutput {
        content: format!("{header}{truncated}"),
        was_truncated: true,
    }
}

fn utf16_len(value: &str) -> usize {
    value.encode_utf16().count()
}

fn utf16_suffix(value: &str, max_units: usize) -> String {
    let mut used = 0usize;
    let mut chars = Vec::new();
    for ch in value.chars().rev() {
        let len = ch.len_utf16();
        if used + len > max_units {
            break;
        }
        used += len;
        chars.push(ch);
    }
    chars.into_iter().rev().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_task_output_length_uses_official_default_cap_and_parse_int() {
        assert_eq!(
            get_max_task_output_length_from_value(None),
            TASK_MAX_OUTPUT_DEFAULT
        );
        assert_eq!(get_max_task_output_length_from_value(Some("99suffix")), 99);
        assert_eq!(
            get_max_task_output_length_from_value(Some("200000")),
            TASK_MAX_OUTPUT_UPPER_LIMIT
        );
        assert_eq!(
            get_max_task_output_length_from_value(Some("bad")),
            TASK_MAX_OUTPUT_DEFAULT
        );
    }

    #[test]
    fn format_task_output_preserves_small_output_and_marks_large_output() {
        assert_eq!(
            format_task_output_with_max_len("hello", "task-1", 100),
            FormattedTaskOutput {
                content: "hello".to_string(),
                was_truncated: false,
            }
        );

        let result = format_task_output_with_max_len("0123456789", "task-1", 8);
        assert!(result.was_truncated);
        assert!(result.content.starts_with("[Truncated. Full output:"));
    }
}
