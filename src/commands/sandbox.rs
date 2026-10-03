//! `/sandbox` command seam.
//! Maps to: CC `commands/sandbox-toggle/index.ts` and
//! `commands/sandbox-toggle/sandbox-toggle.tsx`.
//!
//! This module ports the official command metadata/status strings and the
//! non-interactive `exclude` subcommand, dispatching settings updates through
//! `utils/sandbox/sandbox_adapter.rs`. The no-argument slash-command path opens
//! `components/sandbox/SandboxSettings` from the REPL local-jsx seam.

use crate::utils::sandbox::sandbox_adapter::{
    add_to_excluded_commands, are_sandbox_settings_locked_by_policy,
    are_unsandboxed_commands_allowed, check_dependencies_readonly,
    is_auto_allow_bash_if_sandboxed_enabled, is_platform_in_enabled_list, is_sandboxing_enabled,
    is_supported_platform,
};
use crate::utils::settings::constants::SettingSource;
use crate::utils::settings::{get_initial_settings, get_settings_file_path_for_source};
use std::path::{Path, PathBuf};

/// Maps to: CC `commands/sandbox-toggle/index.ts:7-37` `get description()`.
pub fn description() -> String {
    let figures = crate::constants::figures::figures();
    let icon = if !check_dependencies_readonly().errors.is_empty() {
        figures.warning
    } else if is_sandboxing_enabled() {
        figures.tick
    } else {
        figures.circle
    };
    format!("{icon} {} (⏎ to configure)", sandbox_status_text())
}

/// Maps to: CC `commands/sandbox-toggle/index.ts:22-34` dynamic `description`
/// status text.
pub fn sandbox_status_text() -> String {
    let settings = get_initial_settings();
    let currently_enabled = is_sandboxing_enabled();
    let auto_allow = is_auto_allow_bash_if_sandboxed_enabled(&settings);
    let allow_unsandboxed = are_unsandboxed_commands_allowed(&settings);
    let is_locked = are_sandbox_settings_locked_by_policy();

    let mut status = if currently_enabled {
        let mut text = if auto_allow {
            "sandbox enabled (auto-allow)".to_string()
        } else {
            "sandbox enabled".to_string()
        };
        if allow_unsandboxed {
            text.push_str(", fallback allowed");
        }
        text
    } else {
        "sandbox disabled".to_string()
    };

    if is_locked {
        status.push_str(" (managed)");
    }

    status
}

/// Maps to: CC `commands/sandbox-toggle/index.ts:39-44` `get isHidden()`.
pub fn is_hidden() -> bool {
    is_hidden_for(is_supported_platform(), is_platform_in_enabled_list())
}

fn is_hidden_for(supported_platform: bool, platform_in_enabled_list: bool) -> bool {
    !supported_platform || !platform_in_enabled_list
}

fn strip_wrapping_quotes(value: &str) -> &str {
    let trimmed = value.trim();
    if trimmed.len() >= 2 {
        let bytes = trimmed.as_bytes();
        let first = bytes[0];
        let last = bytes[trimmed.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return &trimmed[1..trimmed.len() - 1];
        }
    }
    trimmed
}

fn relative_to_cwd(path: &Path) -> PathBuf {
    let cwd = crate::bootstrap::state::get_original_cwd();
    path.strip_prefix(&cwd).unwrap_or(path).to_path_buf()
}

fn local_settings_relative_path() -> PathBuf {
    get_settings_file_path_for_source(SettingSource::Local)
        .map(|path| relative_to_cwd(&path))
        .unwrap_or_else(|| PathBuf::from(".claude/settings.local.json"))
}

/// Maps to: CC `commands/sandbox-toggle/sandbox-toggle.tsx#call` no-argument
/// path fallback for non-REPL callers. The interactive REPL path opens
/// `SandboxSettings`; this helper preserves a visible status if called as a
/// local-output command by tests or future headless command runners.
pub fn no_args_output() -> String {
    format!("Current sandbox status: {}", sandbox_status_text())
}

/// Maps to: CC `commands/sandbox-toggle/sandbox-toggle.tsx#call` visible
/// `onDone(...)` outputs.
pub fn local_output_for_args(args: &str) -> String {
    let trimmed = args.trim();
    if trimmed.is_empty() {
        return no_args_output();
    }

    let mut parts = trimmed.split_whitespace();
    let subcommand = parts.next().unwrap_or_default();

    if subcommand == "exclude" {
        let command_pattern = trimmed["exclude".len()..].trim();
        if command_pattern.is_empty() {
            return "Error: Please provide a command pattern to exclude (e.g., /sandbox exclude \"npm run test:*\")".to_string();
        }

        let clean_pattern = strip_wrapping_quotes(command_pattern);
        return match add_to_excluded_commands(clean_pattern, None) {
            Ok(pattern) => format!(
                "Added \"{}\" to excluded commands in {}",
                pattern,
                local_settings_relative_path().display()
            ),
            Err(error) => format!("Error writing settings: {error}"),
        };
    }

    format!("Error: Unknown subcommand \"{subcommand}\". Available subcommand: exclude")
}

pub fn local_output_is_error(output: &str) -> bool {
    output.starts_with("Error:")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    struct CwdGuard {
        old: PathBuf,
        old_original: PathBuf,
    }

    impl CwdGuard {
        fn set(path: &Path) -> Self {
            let old = std::env::current_dir().unwrap();
            let old_original = crate::bootstrap::state::get_original_cwd();
            std::env::set_current_dir(path).unwrap();
            crate::bootstrap::state::set_original_cwd(path);
            Self { old, old_original }
        }
    }

    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.old);
            crate::bootstrap::state::set_original_cwd(&self.old_original);
        }
    }

    #[test]
    fn sandbox_local_output_matches_official_errors_and_status() {
        let output = local_output_for_args("");
        assert!(output.contains("Current sandbox status:"));
        assert!(output.contains("sandbox"));

        let missing = local_output_for_args("exclude");
        assert_eq!(
            missing,
            "Error: Please provide a command pattern to exclude (e.g., /sandbox exclude \"npm run test:*\")"
        );
        assert!(local_output_is_error(&missing));

        let unknown = local_output_for_args("wat now");
        assert_eq!(
            unknown,
            "Error: Unknown subcommand \"wat\". Available subcommand: exclude"
        );
    }

    #[test]
    fn sandbox_hidden_condition_matches_official_predicate_pair() {
        assert!(!is_hidden_for(true, true));
        assert!(is_hidden_for(false, true));
        assert!(is_hidden_for(true, false));
        assert!(is_hidden_for(false, false));
    }

    #[test]
    fn sandbox_command_leaves_the_command_list_on_unsupported_platforms() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-sandbox-hidden-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let config_home = root.join("user-config");
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&config_home).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();

        {
            let _cwd_guard = CwdGuard::set(&workspace);
            let _config_guard = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config_home);
            let _managed_guard = EnvVarGuard::set(
                "CLAUDE_CODE_MANAGED_SETTINGS_PATH",
                root.join("missing-managed-settings.json"),
            );

            // An empty `enabledPlatforms` excludes every platform, which is the
            // only runtime-reachable arm of the official pair on a host that
            // `isSupportedPlatform()` already accepts.
            std::fs::write(
                config_home.join("settings.json"),
                br#"{"sandbox":{"enabledPlatforms":[]}}"#,
            )
            .unwrap();
            crate::utils::settings::settings_cache::reset_settings_cache();

            let catalog = crate::commands::commands();
            let sandbox = catalog
                .iter()
                .find(|command| command.name == "sandbox")
                .expect("/sandbox is declared");
            assert!(crate::commands::is_command_hidden(sandbox));
            assert!(
                !crate::commands::filter_commands(&catalog, "sandbox")
                    .iter()
                    .any(|command| command.name == "sandbox")
            );

            std::fs::write(config_home.join("settings.json"), br#"{"sandbox":{}}"#).unwrap();
            crate::utils::settings::settings_cache::reset_settings_cache();
            assert_eq!(
                crate::commands::filter_commands(&catalog, "sandbox")
                    .iter()
                    .any(|command| command.name == "sandbox"),
                is_supported_platform()
            );
        }

        crate::utils::settings::settings_cache::reset_settings_cache();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn sandbox_exclude_writes_local_settings_like_official() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-sandbox-command-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let managed = root.join("managed");
        let config_home = root.join("user-config");
        std::fs::create_dir_all(root.join(".claude")).unwrap();
        std::fs::create_dir_all(&managed).unwrap();
        std::fs::create_dir_all(&config_home).unwrap();
        let _cwd_guard = CwdGuard::set(&root);
        let _managed_guard = EnvVarGuard::set("CLAUDE_CODE_MANAGED_SETTINGS_PATH", &managed);
        let _config_guard = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config_home);
        let _write_guard = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");

        let output = local_output_for_args("exclude \"npm run test:*\"");
        assert_eq!(
            output,
            "Added \"npm run test:*\" to excluded commands in .claude/settings.local.json"
        );
        let settings = std::fs::read_to_string(root.join(".claude/settings.local.json")).unwrap();
        assert!(
            settings.contains("npm run test:*"),
            "settings.local.json=\n{settings}"
        );

        let output_again = local_output_for_args("exclude 'npm run test:*'");
        assert_eq!(
            output_again,
            "Added \"npm run test:*\" to excluded commands in .claude/settings.local.json"
        );
        let value: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(root.join(".claude/settings.local.json")).unwrap(),
        )
        .unwrap();
        let excluded = value
            .pointer("/sandbox/excludedCommands")
            .and_then(serde_json::Value::as_array)
            .unwrap();
        assert_eq!(excluded.len(), 1);

        let _ = std::fs::remove_dir_all(&root);
    }
}
