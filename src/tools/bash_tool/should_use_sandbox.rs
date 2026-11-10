//! Maps to: CC `tools/BashTool/shouldUseSandbox.ts`.
//!
//! This preserves the official decision boundary: BashTool asks sandbox policy
//! whether a command should run in sandbox, rather than inlining sandbox rules in
//! command execution.

use crate::utils::permissions::shell_rule_matching::ShellPermissionRule;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SandboxInput {
    pub command: Option<String>,
    pub dangerously_disable_sandbox: bool,
}

/// Maps to CC `shouldUseSandbox(input)`.
pub fn should_use_sandbox(input: &SandboxInput) -> bool {
    if !crate::utils::sandbox::sandbox_adapter::is_sandboxing_enabled() {
        return false;
    }

    let settings = crate::utils::settings::get_initial_settings();
    if input.dangerously_disable_sandbox
        && crate::utils::sandbox::sandbox_adapter::are_unsandboxed_commands_allowed(&settings)
    {
        return false;
    }

    let Some(command) = input.command.as_ref().filter(|command| !command.is_empty()) else {
        return false;
    };

    !contains_excluded_command(command, &settings)
}

/// Maps to CC local `containsExcludedCommand(command)` settings branch.
pub fn contains_excluded_command(
    command: &str,
    settings: &crate::utils::settings::types::SettingsJson,
) -> bool {
    let user_excluded_commands =
        crate::utils::sandbox::sandbox_adapter::get_excluded_commands(settings);
    if user_excluded_commands.is_empty() {
        return false;
    }

    let commands = match crate::utils::bash::ast::parse_for_security(command) {
        crate::utils::bash::ast::ParseForSecurityResult::Simple { commands } => commands,
        _ => return true,
    };
    for subcommand in commands {
        let trimmed = subcommand.text.trim();
        let normalized =
            super::path_validation::strip_wrappers_from_argv(&subcommand.argv).join(" ");
        let mut candidates = vec![trimmed.to_string(), normalized];
        let mut seen = candidates
            .iter()
            .cloned()
            .collect::<std::collections::HashSet<_>>();
        let mut start = 0usize;
        while start < candidates.len() {
            let end = candidates.len();
            for index in start..end {
                let candidate = candidates[index].clone();
                let env_stripped = super::bash_permissions::strip_all_leading_env_vars(
                    &candidate,
                    Some(&super::bash_permissions::BINARY_HIJACK_VARS),
                );
                if seen.insert(env_stripped.clone()) {
                    candidates.push(env_stripped);
                }
                let wrapper_stripped = super::bash_permissions::strip_safe_wrappers(&candidate);
                if seen.insert(wrapper_stripped.clone()) {
                    candidates.push(wrapper_stripped);
                }
            }
            start = end;
        }

        for pattern in &user_excluded_commands {
            let rule = super::bash_permissions::bash_permission_rule(pattern);
            for candidate in &candidates {
                match &rule {
                    ShellPermissionRule::Prefix { prefix } => {
                        if candidate == prefix || candidate.starts_with(&format!("{prefix} ")) {
                            return true;
                        }
                    }
                    ShellPermissionRule::Exact { command } => {
                        if candidate == command {
                            return true;
                        }
                    }
                    ShellPermissionRule::Wildcard { pattern } => {
                        if super::bash_permissions::match_wildcard_pattern(pattern, candidate) {
                            return true;
                        }
                    }
                }
            }
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings_with_excluded(
        excluded_commands: Vec<&str>,
    ) -> crate::utils::settings::types::SettingsJson {
        crate::utils::settings::types::SettingsJson {
            sandbox: Some(serde_json::json!({
                "excludedCommands": excluded_commands,
            })),
            ..Default::default()
        }
    }

    #[test]
    fn excluded_command_matching_checks_compounds_wrappers_envs_and_patterns() {
        let settings = settings_with_excluded(vec!["bazel:*", "git status", "python *"]);
        assert!(contains_excluded_command(
            "echo hi && bazel build //...",
            &settings
        ));
        assert!(contains_excluded_command(
            "timeout 30 FOO=bar bazel test //...",
            &settings
        ));
        assert!(contains_excluded_command(
            "NO_COLOR=1 git status",
            &settings
        ));
        assert!(contains_excluded_command("python script.py", &settings));
        assert!(!contains_excluded_command("cargo test", &settings));
    }

    #[test]
    fn empty_excluded_command_settings_do_not_exclude() {
        assert!(!contains_excluded_command(
            "bazel build //...",
            &settings_with_excluded(Vec::new())
        ));
    }

    #[test]
    fn ast_exclusions_cover_control_flow_quotes_and_fail_closed() {
        let settings = settings_with_excluded(vec!["bazel:*"]);
        for command in [
            "bazel build //...",
            "if true; then bazel build //...; fi",
            "(bazel build //...)",
            "b\"\"azel build //...",
            "echo \"prefix: $(bazel build //...)\"",
            "echo 'unterminated",
        ] {
            assert!(contains_excluded_command(command, &settings), "{command}");
        }
        assert!(!contains_excluded_command("", &settings));
    }
}
