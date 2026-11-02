//! CLI argument helpers.
//!
//! Maps to: CC `src/utils/cliArgs.ts:eagerParseCliFlag`.
//! These helpers intentionally run before the normal TUI startup path so flags
//! like `--settings` can affect configuration loading from process start.

/// Parse a CLI flag value before normal argument processing.
///
/// Supports both `--flag value` and `--flag=value` forms. The iterator should
/// include the executable name if available; it is ignored unless it matches the
/// requested flag, matching CC's simple process.argv scan.
///
/// Maps to: CC `src/utils/cliArgs.ts:1-25`.
pub fn eager_parse_cli_flag<I, S>(flag_name: &str, argv: I) -> Option<String>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let args: Vec<String> = argv.into_iter().map(Into::into).collect();
    for (index, arg) in args.iter().enumerate() {
        let equals_prefix = format!("{flag_name}=");
        if let Some(value) = arg.strip_prefix(&equals_prefix) {
            return Some(value.to_string());
        }
        if arg == flag_name {
            return args.get(index + 1).cloned();
        }
    }
    None
}

/// Parse a CLI flag that accepts one or more values, preserving repeated flag
/// occurrences and `--flag=value` forms.
///
/// Maps to CC `main.tsx` `--add-dir <directories...>` startup parsing. Values
/// after `--flag` are consumed until the next flag-looking argument.
pub fn eager_parse_cli_flag_values<I, S>(flag_name: &str, argv: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let args: Vec<String> = argv.into_iter().map(Into::into).collect();
    let equals_prefix = format!("{flag_name}=");
    let mut values = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if let Some(value) = arg.strip_prefix(&equals_prefix) {
            if !value.is_empty() {
                values.push(value.to_string());
            }
            index += 1;
            continue;
        }
        if arg == flag_name {
            index += 1;
            while index < args.len() {
                let value = &args[index];
                if value.starts_with("--") {
                    break;
                }
                values.push(value.clone());
                index += 1;
            }
            continue;
        }
        index += 1;
    }
    values
}

/// Parse a repeatable flag whose declaration consumes exactly one value per
/// occurrence. This is the startup counterpart of Commander options such as
/// `--plugin-dir <path>`; unlike [`eager_parse_cli_flag_values`], it must not
/// absorb a following prompt or positional argument.
pub fn eager_parse_cli_flag_repeated<I, S>(flag_name: &str, argv: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let args: Vec<String> = argv.into_iter().map(Into::into).collect();
    let equals_prefix = format!("{flag_name}=");
    let mut values = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if let Some(value) = arg.strip_prefix(&equals_prefix) {
            if !value.is_empty() {
                values.push(value.to_string());
            }
            index += 1;
            continue;
        }
        if arg == flag_name
            && let Some(value) = args.get(index + 1).filter(|value| !value.starts_with('-'))
        {
            values.push(value.clone());
            index += 2;
            continue;
        }
        index += 1;
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eager_parse_cli_flag_supports_space_separated_value() {
        let value =
            eager_parse_cli_flag("--settings", ["nyan", "--settings", "settings.test.json"]);
        assert_eq!(value.as_deref(), Some("settings.test.json"));
    }

    #[test]
    fn eager_parse_cli_flag_supports_equals_value() {
        let value = eager_parse_cli_flag("--settings", ["nyan", "--settings={}"]);
        assert_eq!(value.as_deref(), Some("{}"));
    }

    #[test]
    fn eager_parse_cli_flag_returns_none_when_missing() {
        let value = eager_parse_cli_flag("--settings", ["nyan", "--debug"]);
        assert_eq!(value, None);
    }

    #[test]
    fn eager_parse_cli_flag_values_supports_repeated_variadic_and_equals_forms() {
        let values = eager_parse_cli_flag_values(
            "--add-dir",
            [
                "nyan",
                "--add-dir",
                "/repo/a",
                "/repo/b",
                "--settings",
                "settings.json",
                "--add-dir=/repo/c",
            ],
        );

        assert_eq!(values, vec!["/repo/a", "/repo/b", "/repo/c"]);
    }

    #[test]
    fn eager_parse_cli_flag_repeated_consumes_one_value_per_occurrence() {
        let values = eager_parse_cli_flag_repeated(
            "--plugin-dir",
            [
                "nyan",
                "--plugin-dir",
                "/repo/a",
                "prompt",
                "--plugin-dir=/repo/b",
                "--verbose",
            ],
        );
        assert_eq!(values, vec!["/repo/a", "/repo/b"]);

        let missing = eager_parse_cli_flag_repeated("--plugin-dir", ["nyan", "--verbose"]);
        assert!(missing.is_empty());
    }
}
