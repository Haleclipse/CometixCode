//! Shared shell permission rule matching utilities.
//! Maps to: CC `utils/permissions/shellRuleMatching.ts`.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellPermissionRule {
    Exact { command: String },
    Prefix { prefix: String },
    Wildcard { pattern: String },
}

/// Maps to CC `permissionRuleExtractPrefix(...)`.
pub fn permission_rule_extract_prefix(permission_rule: &str) -> Option<&str> {
    permission_rule
        .strip_suffix(":*")
        .filter(|prefix| !prefix.is_empty())
}

/// Maps to CC `hasWildcards(...)`.
pub fn has_wildcards(pattern: &str) -> bool {
    if pattern.ends_with(":*") {
        return false;
    }
    let bytes = pattern.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if *byte != b'*' {
            continue;
        }
        let mut backslash_count = 0usize;
        let mut cursor = index;
        while cursor > 0 && bytes[cursor - 1] == b'\\' {
            backslash_count += 1;
            cursor -= 1;
        }
        if backslash_count.is_multiple_of(2) {
            return true;
        }
    }
    false
}

/// Maps to CC `matchWildcardPattern(...)`.
pub fn match_wildcard_pattern(pattern: &str, command: &str, case_insensitive: bool) -> bool {
    let trimmed_pattern = pattern.trim();
    let mut regex_pattern = String::new();
    let mut unescaped_star_count = 0usize;
    let mut chars = trimmed_pattern.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.peek().copied() {
                Some('*') => {
                    chars.next();
                    regex_pattern.push_str("\\*");
                    continue;
                }
                Some('\\') => {
                    chars.next();
                    regex_pattern.push_str("\\\\");
                    continue;
                }
                _ => {}
            }
        }
        if ch == '*' {
            unescaped_star_count += 1;
            regex_pattern.push_str(".*");
        } else {
            regex_pattern.push_str(&regex::escape(&ch.to_string()));
        }
    }

    if regex_pattern.ends_with(" .*") && unescaped_star_count == 1 {
        let keep = regex_pattern.len() - " .*".len();
        regex_pattern.truncate(keep);
        regex_pattern.push_str("( .*)?");
    }

    let flags = if case_insensitive { "(?si)" } else { "(?s)" };
    let pattern = format!("{flags}^{regex_pattern}$");
    regex::Regex::new(&pattern)
        .map(|regex| regex.is_match(command))
        .unwrap_or(false)
}

/// Maps to CC `parsePermissionRule(...)`.
pub fn parse_permission_rule(permission_rule: &str) -> ShellPermissionRule {
    if let Some(prefix) = permission_rule_extract_prefix(permission_rule) {
        return ShellPermissionRule::Prefix {
            prefix: prefix.to_string(),
        };
    }
    if has_wildcards(permission_rule) {
        return ShellPermissionRule::Wildcard {
            pattern: permission_rule.to_string(),
        };
    }
    ShellPermissionRule::Exact {
        command: permission_rule.to_string(),
    }
}

/// Maps to CC `suggestionForExactCommand(toolName, command)`.
pub fn suggestion_for_exact_command(
    tool_name: &str,
    command: &str,
) -> Vec<crate::types::permissions::PermissionUpdate> {
    vec![crate::types::permissions::PermissionUpdate::AddRules {
        destination: crate::types::permissions::PermissionUpdateDestination::LocalSettings,
        behavior: crate::types::permissions::PermissionBehavior::Allow,
        rules: vec![crate::types::permissions::PermissionRuleValue::new(
            tool_name,
            Some(command.to_string()),
        )],
    }]
}

/// Maps to CC `suggestionForPrefix(toolName, prefix)`.
pub fn suggestion_for_prefix(
    tool_name: &str,
    prefix: &str,
) -> Vec<crate::types::permissions::PermissionUpdate> {
    vec![crate::types::permissions::PermissionUpdate::AddRules {
        destination: crate::types::permissions::PermissionUpdateDestination::LocalSettings,
        behavior: crate::types::permissions::PermissionBehavior::Allow,
        rules: vec![crate::types::permissions::PermissionRuleValue::new(
            tool_name,
            Some(format!("{prefix}:*")),
        )],
    }]
}

/// Query-level helper used by `permissions.rs` rule buckets.
pub fn permission_rule_content_matches(tool_name: &str, expected: &str, actual: &str) -> bool {
    if expected == actual {
        return true;
    }
    match parse_permission_rule(expected) {
        ShellPermissionRule::Exact { .. } => false,
        ShellPermissionRule::Prefix { prefix } => {
            !is_unsafe_shell_rule_match(tool_name, actual)
                && (actual == prefix || actual.starts_with(&format!("{prefix} ")))
        }
        ShellPermissionRule::Wildcard { pattern } => {
            !is_unsafe_shell_rule_match(tool_name, actual)
                && match_wildcard_pattern(&pattern, actual, false)
        }
    }
}

/// Maps to CC Bash/PowerShell safeguards that avoid applying broad shell rules
/// to unsplit compound commands.
fn is_unsafe_shell_rule_match(tool_name: &str, actual: &str) -> bool {
    if !matches!(tool_name, "Bash" | "PowerShell") {
        return false;
    }
    ["&&", "||", ";", "|", "&", "\n", "\r", "`", "$(", "${"]
        .iter()
        .any(|operator| actual.contains(operator))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard_matching_matches_official_optional_trailing_arg() {
        assert!(match_wildcard_pattern("git *", "git", false));
        assert!(match_wildcard_pattern("git *", "git status", false));
        assert!(match_wildcard_pattern(
            r"echo \* *",
            "echo * literal",
            false
        ));
        assert!(!match_wildcard_pattern(
            r"echo \* *",
            "echo anything literal",
            false
        ));
    }

    #[test]
    fn shell_rule_content_matching_blocks_compound_commands_for_shell_tools() {
        assert!(permission_rule_content_matches(
            "Bash",
            "cargo:*",
            "cargo test"
        ));
        assert!(!permission_rule_content_matches(
            "Bash",
            "cargo:*",
            "cargo test && rm -rf target"
        ));
        assert!(permission_rule_content_matches(
            "Read",
            "src/*.rs",
            "src/main.rs"
        ));
    }

    #[test]
    fn suggestions_match_official_local_settings_allow_updates() {
        assert_eq!(
            suggestion_for_exact_command("Bash", "git status"),
            vec![crate::types::permissions::PermissionUpdate::AddRules {
                destination: crate::types::permissions::PermissionUpdateDestination::LocalSettings,
                behavior: crate::types::permissions::PermissionBehavior::Allow,
                rules: vec![crate::types::permissions::PermissionRuleValue::new(
                    "Bash",
                    Some("git status".to_string())
                )],
            }]
        );
        assert_eq!(
            suggestion_for_prefix("Bash", "git"),
            vec![crate::types::permissions::PermissionUpdate::AddRules {
                destination: crate::types::permissions::PermissionUpdateDestination::LocalSettings,
                behavior: crate::types::permissions::PermissionBehavior::Allow,
                rules: vec![crate::types::permissions::PermissionRuleValue::new(
                    "Bash",
                    Some("git:*".to_string())
                )],
            }]
        );
    }
}
