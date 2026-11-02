//! Permission rule validation.
//!
//! Maps to: CC `utils/settings/permissionValidation.ts`.

use std::sync::OnceLock;

use super::tool_validation_config::{
    get_custom_validation, is_bash_prefix_tool, is_file_pattern_tool,
};
use crate::services::mcp::mcp_string_utils::mcp_info_from_string;
use crate::utils::permissions::permission_rule_parser::permission_rule_value_from_string;
use crate::utils::string_utils::capitalize;
use crate::utils::zod::{self, Schema, SuperRefineIssue};

/// The result shape `validatePermissionRule` (and the custom validations in
/// `toolValidationConfig`) return — CC keeps it anonymous.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct PermissionRuleValidation {
    pub valid: bool,
    pub error: Option<String>,
    pub suggestion: Option<String>,
    pub examples: Vec<String>,
}

impl PermissionRuleValidation {
    pub fn valid() -> Self {
        PermissionRuleValidation {
            valid: true,
            ..Default::default()
        }
    }
}

/// Maps to: CC `isEscaped(str, index)` — a character is escaped when preceded
/// by an odd number of backslashes. Operates on the same char sequence
/// `countUnescapedChar` iterates.
fn is_escaped(chars: &[char], index: usize) -> bool {
    let mut backslash_count = 0;
    let mut j = index;
    while j > 0 && chars[j - 1] == '\\' {
        backslash_count += 1;
        j -= 1;
    }
    backslash_count % 2 != 0
}

/// Maps to: CC `countUnescapedChar(str, char)`.
fn count_unescaped_char(s: &str, needle: char) -> usize {
    let chars: Vec<char> = s.chars().collect();
    chars
        .iter()
        .enumerate()
        .filter(|(i, c)| **c == needle && !is_escaped(&chars, *i))
        .count()
}

/// Maps to: CC `hasUnescapedEmptyParens(str)` — an unescaped `(` immediately
/// followed by `)`.
fn has_unescaped_empty_parens(s: &str) -> bool {
    let chars: Vec<char> = s.chars().collect();
    for i in 0..chars.len().saturating_sub(1) {
        if chars[i] == '(' && chars[i + 1] == ')' && !is_escaped(&chars, i) {
            return true;
        }
    }
    false
}

/// Maps to: CC `validatePermissionRule(rule)` — validates permission rule
/// format and content.
pub fn validate_permission_rule(rule: &str) -> PermissionRuleValidation {
    // Empty rule check.
    if rule.trim().is_empty() {
        return PermissionRuleValidation {
            valid: false,
            error: Some("Permission rule cannot be empty".to_string()),
            ..Default::default()
        };
    }

    // Check parentheses matching first (only count unescaped parens).
    let open_count = count_unescaped_char(rule, '(');
    let close_count = count_unescaped_char(rule, ')');
    if open_count != close_count {
        return PermissionRuleValidation {
            valid: false,
            error: Some("Mismatched parentheses".to_string()),
            suggestion: Some(
                "Ensure all opening parentheses have matching closing parentheses".to_string(),
            ),
            ..Default::default()
        };
    }

    // Check for empty parentheses (escape-aware). The tool name is everything
    // before the FIRST `(`, escaped or not — CC uses `indexOf`.
    if has_unescaped_empty_parens(rule) {
        let tool_name = rule.split('(').next().unwrap_or("");
        if tool_name.is_empty() {
            return PermissionRuleValidation {
                valid: false,
                error: Some("Empty parentheses with no tool name".to_string()),
                suggestion: Some("Specify a tool name before the parentheses".to_string()),
                ..Default::default()
            };
        }
        return PermissionRuleValidation {
            valid: false,
            error: Some("Empty parentheses".to_string()),
            suggestion: Some(format!(
                "Either specify a pattern or use just \"{tool_name}\" without parentheses"
            )),
            examples: vec![tool_name.to_string(), format!("{tool_name}(some-pattern)")],
        };
    }

    // Parse the rule.
    let parsed = permission_rule_value_from_string(rule);

    // MCP validation — must be done before general tool validation. MCP rules
    // support server-level (mcp__server), wildcard (mcp__server__*), and
    // tool-level (mcp__server__tool) permissions, but no patterns in
    // parentheses. Check both parsed content and the raw string since the
    // parser normalizes standalone wildcards ("mcp__server(*)") to no content.
    if let Some(mcp_info) = mcp_info_from_string(&parsed.tool_name) {
        if parsed.rule_content.is_some() || count_unescaped_char(rule, '(') > 0 {
            let mut examples = vec![
                format!("mcp__{}", mcp_info.server_name),
                format!("mcp__{}__*", mcp_info.server_name),
            ];
            if let Some(tool_name) = &mcp_info.tool_name
                && tool_name != "*"
            {
                examples.push(format!("mcp__{}__{}", mcp_info.server_name, tool_name));
            }
            return PermissionRuleValidation {
                valid: false,
                error: Some("MCP rules do not support patterns in parentheses".to_string()),
                suggestion: Some(format!(
                    "Use \"{}\" without parentheses, or use \"mcp__{}__*\" for all tools",
                    parsed.tool_name, mcp_info.server_name
                )),
                examples,
            };
        }
        return PermissionRuleValidation::valid(); // Valid MCP rule
    }

    // Tool name validation (for non-MCP tools).
    if parsed.tool_name.is_empty() {
        return PermissionRuleValidation {
            valid: false,
            error: Some("Tool name cannot be empty".to_string()),
            ..Default::default()
        };
    }

    // Check tool name starts with uppercase (standard tools).
    let first = parsed.tool_name.chars().next();
    if first.is_some_and(|c| c.to_uppercase().to_string() != c.to_string()) {
        return PermissionRuleValidation {
            valid: false,
            error: Some("Tool names must start with uppercase".to_string()),
            suggestion: Some(format!("Use \"{}\"", capitalize(&parsed.tool_name))),
            ..Default::default()
        };
    }

    // Check for custom validation rules first.
    if let (Some(custom_validation), Some(content)) = (
        get_custom_validation(&parsed.tool_name),
        parsed.rule_content.as_deref(),
    ) {
        let custom_result = custom_validation(content);
        if !custom_result.valid {
            return custom_result;
        }
    }

    // Bash-specific validation. Wildcards are allowed at any position; only
    // the legacy :* prefix syntax has placement rules. Quote balancing is
    // deliberately not validated (bash quoting rules are complex).
    if is_bash_prefix_tool(&parsed.tool_name)
        && let Some(content) = parsed.rule_content.as_deref()
    {
        if content.contains(":*") && !content.ends_with(":*") {
            return PermissionRuleValidation {
                valid: false,
                error: Some("The :* pattern must be at the end".to_string()),
                suggestion: Some(
                    "Move :* to the end for prefix matching, or use * for wildcard matching"
                        .to_string(),
                ),
                examples: vec![
                    "Bash(npm run:*) - prefix matching (legacy)".to_string(),
                    "Bash(npm run *) - wildcard matching".to_string(),
                ],
            };
        }
        if content == ":*" {
            return PermissionRuleValidation {
                valid: false,
                error: Some("Prefix cannot be empty before :*".to_string()),
                suggestion: Some("Specify a command prefix before :*".to_string()),
                examples: vec!["Bash(npm:*)".to_string(), "Bash(git:*)".to_string()],
            };
        }
    }

    // File tool validation.
    if is_file_pattern_tool(&parsed.tool_name)
        && let Some(content) = parsed.rule_content.as_deref()
    {
        // Check for :* in file patterns (common mistake from Bash patterns).
        if content.contains(":*") {
            return PermissionRuleValidation {
                valid: false,
                error: Some("The \":*\" syntax is only for Bash prefix rules".to_string()),
                suggestion: Some(
                    "Use glob patterns like \"*\" or \"**\" for file matching".to_string(),
                ),
                examples: vec![
                    format!("{}(*.ts) - matches .ts files", parsed.tool_name),
                    format!("{}(src/**) - matches all files in src", parsed.tool_name),
                    format!("{}(**/*.test.ts) - matches test files", parsed.tool_name),
                ],
            };
        }
        // Warn about wildcards not at boundaries — a loose check; middle
        // wildcards might be valid but often indicate confusion. CC's
        // regex /^\*|\*$|\*\*|\/\*|\*\.|\*\)/ expanded to its alternates.
        let at_boundary = content.starts_with('*')
            || content.ends_with('*')
            || content.contains("**")
            || content.contains("/*")
            || content.contains("*.")
            || content.contains("*)");
        if content.contains('*') && !at_boundary {
            return PermissionRuleValidation {
                valid: false,
                error: Some("Wildcard placement might be incorrect".to_string()),
                suggestion: Some("Wildcards are typically used at path boundaries".to_string()),
                examples: vec![
                    format!("{}(*.js) - all .js files", parsed.tool_name),
                    format!("{}(src/*) - all files directly in src", parsed.tool_name),
                    format!(
                        "{}(src/**) - all files recursively in src",
                        parsed.tool_name
                    ),
                ],
            };
        }
    }

    PermissionRuleValidation::valid()
}

/// Maps to: CC `PermissionRuleSchema` — `z.string().superRefine(...)` folding
/// error + suggestion + examples into one custom-issue message with
/// `params: {received}`.
pub fn permission_rule_schema() -> &'static Schema {
    static SCHEMA: OnceLock<Schema> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        zod::string().super_refine(|value| {
            let val = value.as_str()?;
            let result = validate_permission_rule(val);
            if result.valid {
                return None;
            }
            let mut message = result.error.unwrap_or_default();
            if let Some(suggestion) = result.suggestion {
                message.push_str(&format!(". {suggestion}"));
            }
            if !result.examples.is_empty() {
                message.push_str(&format!(". Examples: {}", result.examples.join(", ")));
            }
            Some(SuperRefineIssue {
                message,
                params: Some(serde_json::json!({"received": val})),
            })
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::zod::safe_parse;
    use serde_json::json;

    #[test]
    fn validate_permission_rule_matches_official_branches() {
        // Empty and structural failures.
        assert_eq!(
            validate_permission_rule("").error.as_deref(),
            Some("Permission rule cannot be empty")
        );
        assert_eq!(
            validate_permission_rule("Bash(npm").error.as_deref(),
            Some("Mismatched parentheses")
        );
        assert_eq!(
            validate_permission_rule("()").error.as_deref(),
            Some("Empty parentheses with no tool name")
        );
        let empty = validate_permission_rule("Bash()");
        assert_eq!(empty.error.as_deref(), Some("Empty parentheses"));
        assert_eq!(empty.examples, vec!["Bash", "Bash(some-pattern)"]);

        // MCP rules: bare and wildcard forms pass, patterns are rejected.
        assert!(validate_permission_rule("mcp__server").valid);
        assert!(validate_permission_rule("mcp__server__tool").valid);
        let mcp = validate_permission_rule("mcp__server__tool(x)");
        assert_eq!(
            mcp.error.as_deref(),
            Some("MCP rules do not support patterns in parentheses")
        );
        assert_eq!(
            mcp.examples,
            vec!["mcp__server", "mcp__server__*", "mcp__server__tool"]
        );

        // Casing.
        let lower = validate_permission_rule("bash(ls)");
        assert_eq!(
            lower.error.as_deref(),
            Some("Tool names must start with uppercase")
        );
        assert_eq!(lower.suggestion.as_deref(), Some("Use \"Bash\""));

        // Custom validations.
        assert_eq!(
            validate_permission_rule("WebFetch(https://x.com)")
                .error
                .as_deref(),
            Some("WebFetch permissions use domain format, not URLs")
        );
        assert_eq!(
            validate_permission_rule("WebFetch(x.com)").error.as_deref(),
            Some("WebFetch permissions must use \"domain:\" prefix")
        );
        assert!(validate_permission_rule("WebFetch(domain:*.google.com)").valid);
        assert_eq!(
            validate_permission_rule("WebSearch(claude*)")
                .error
                .as_deref(),
            Some("WebSearch does not support wildcards")
        );

        // Bash legacy :* placement.
        assert_eq!(
            validate_permission_rule("Bash(npm:* install)")
                .error
                .as_deref(),
            Some("The :* pattern must be at the end")
        );
        assert_eq!(
            validate_permission_rule("Bash(:*)").error.as_deref(),
            Some("Prefix cannot be empty before :*")
        );
        assert!(validate_permission_rule("Bash(npm run:*)").valid);
        assert!(validate_permission_rule("Bash(git * main)").valid);

        // File patterns.
        assert_eq!(
            validate_permission_rule("Read(src:*)").error.as_deref(),
            Some("The \":*\" syntax is only for Bash prefix rules")
        );
        assert_eq!(
            validate_permission_rule("Read(sr*c/x)").error.as_deref(),
            Some("Wildcard placement might be incorrect")
        );
        assert!(validate_permission_rule("Read(src/**)").valid);
        assert!(validate_permission_rule("Read").valid);
    }

    #[test]
    fn permission_rule_schema_folds_copy_and_attaches_received() {
        let err = safe_parse(permission_rule_schema(), &json!("Bash()"))
            .expect_err("empty parens should fail");
        assert_eq!(err.issues.len(), 1);
        assert_eq!(
            err.issues[0].message,
            "Empty parentheses. Either specify a pattern or use just \"Bash\" without parentheses. Examples: Bash, Bash(some-pattern)"
        );
        assert_eq!(err.issues[0].params, Some(json!({"received": "Bash()"})));
        assert!(safe_parse(permission_rule_schema(), &json!("Bash(npm run:*)")).is_ok());
    }
}
