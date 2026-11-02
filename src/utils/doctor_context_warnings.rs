//! Maps to: CC `utils/doctorContextWarnings.ts`.
//!
//! Pure, snapshot-driven Doctor context warning checks. The official module
//! reads AGENTS.md files, AppState agent definitions, MCP tool metadata, and
//! the live permission context. This Rust port keeps the same function/type
//! boundaries while accepting explicit snapshots so the Doctor screen remains a
//! safe local diagnostic surface: no network calls, no settings mutation, and no
//! sandbox runtime initialization occur here.

use crate::tool::ToolPermissionContext;
use crate::types::tools::Tool;
use crate::utils::permissions::permission_rule_parser::permission_rule_value_to_string;
use crate::utils::permissions::shadowed_rule_detection::{
    DetectUnreachableRulesOptions, detect_unreachable_rules,
};
use crate::utils::status_notice_definitions::{
    MAX_MEMORY_CHARACTER_COUNT, MemoryFileInfo, get_large_memory_files,
};
use crate::utils::status_notice_helpers::{
    AGENT_DESCRIPTIONS_THRESHOLD, AgentDefinitionsSnapshot, get_agent_descriptions_total_tokens,
};
use crate::utils::string_utils::plural;
use std::collections::HashMap;

/// Maps to CC `doctorContextWarnings.ts` `MCP_TOOLS_THRESHOLD`.
pub const MCP_TOOLS_THRESHOLD: u64 = 25_000;

/// Maps to CC `ContextWarning.type`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextWarningType {
    ClaudeMdFiles,
    AgentDescriptions,
    McpTools,
    UnreachableRules,
}

/// Maps to CC `ContextWarning.severity`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextWarningSeverity {
    Warning,
    Error,
}

/// Maps to CC `doctorContextWarnings.ts` `ContextWarning`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextWarning {
    pub warning_type: ContextWarningType,
    pub severity: ContextWarningSeverity,
    pub message: String,
    pub details: Vec<String>,
    pub current_value: u64,
    pub threshold: u64,
}

/// Maps to CC `doctorContextWarnings.ts` `ContextWarnings`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContextWarnings {
    pub claude_md_warning: Option<ContextWarning>,
    pub agent_warning: Option<ContextWarning>,
    pub mcp_warning: Option<ContextWarning>,
    pub unreachable_rules_warning: Option<ContextWarning>,
}

/// Snapshot equivalent of the arguments captured by CC `Doctor.tsx` before
/// calling `checkContextWarnings(...)`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DoctorContextWarningsInput {
    /// Maps to CC `checkClaudeMdFiles()`'s `getMemoryFiles()` result.
    pub memory_files: Vec<MemoryFileInfo>,
    /// Maps to CC `checkAgentDescriptions(agentInfo)`.
    pub agent_definitions: Option<AgentDefinitionsSnapshot>,
    /// Maps to CC `checkMcpTools(tools, ...)`.
    pub tools: Vec<Tool>,
    /// Maps to CC `getToolPermissionContext()`.
    pub tool_permission_context: ToolPermissionContext,
    /// Maps to CC `SandboxManager.isSandboxingEnabled() &&
    /// SandboxManager.isAutoAllowBashIfSandboxedEnabled()`.
    pub sandbox_auto_allow_enabled: bool,
}

fn format_locale_number(value: u64) -> String {
    let raw = value.to_string();
    let mut out = String::with_capacity(raw.len() + raw.len() / 3);
    for (index, ch) in raw.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}

/// Maps to CC `checkClaudeMdFiles()`.
pub fn check_claude_md_files(memory_files: &[MemoryFileInfo]) -> Option<ContextWarning> {
    let mut large_files = get_large_memory_files(memory_files);
    if large_files.is_empty() {
        return None;
    }

    large_files.sort_by(|a, b| b.content.len().cmp(&a.content.len()));
    let details = large_files
        .iter()
        .map(|file| {
            format!(
                "{}: {} chars",
                file.path,
                format_locale_number(file.content.chars().count() as u64)
            )
        })
        .collect::<Vec<_>>();

    let message = if large_files.len() == 1 {
        format!(
            "Large AGENTS.md file detected ({} chars > {})",
            format_locale_number(large_files[0].content.chars().count() as u64),
            format_locale_number(MAX_MEMORY_CHARACTER_COUNT as u64)
        )
    } else {
        format!(
            "{} large AGENTS.md files detected (each > {} chars)",
            large_files.len(),
            format_locale_number(MAX_MEMORY_CHARACTER_COUNT as u64)
        )
    };

    Some(ContextWarning {
        warning_type: ContextWarningType::ClaudeMdFiles,
        severity: ContextWarningSeverity::Warning,
        message,
        details,
        current_value: large_files.len() as u64,
        threshold: MAX_MEMORY_CHARACTER_COUNT as u64,
    })
}

/// Maps to CC `checkAgentDescriptions(agentInfo)`.
pub fn check_agent_descriptions(
    agent_definitions: Option<&AgentDefinitionsSnapshot>,
) -> Option<ContextWarning> {
    let agent_definitions = agent_definitions?;
    let total_tokens = get_agent_descriptions_total_tokens(Some(agent_definitions));
    if total_tokens <= AGENT_DESCRIPTIONS_THRESHOLD {
        return None;
    }

    let mut agent_tokens = agent_definitions
        .active_agents
        .iter()
        .filter(|agent| agent.source != "built-in")
        .map(|agent| {
            (
                agent.agent_type.clone(),
                (crate::services::token_estimation::rough_token_count_estimation(&format!(
                    "{}: {}",
                    agent.agent_type, agent.when_to_use
                )) as u64),
            )
        })
        .collect::<Vec<_>>();
    agent_tokens.sort_by(|a, b| b.1.cmp(&a.1));

    let mut details = agent_tokens
        .iter()
        .take(5)
        .map(|(name, tokens)| format!("{name}: ~{} tokens", format_locale_number(*tokens)))
        .collect::<Vec<_>>();
    if agent_tokens.len() > 5 {
        details.push(format!("({} more custom agents)", agent_tokens.len() - 5));
    }

    Some(ContextWarning {
        warning_type: ContextWarningType::AgentDescriptions,
        severity: ContextWarningSeverity::Warning,
        message: format!(
            "Large agent descriptions (~{} tokens > {})",
            format_locale_number(total_tokens),
            format_locale_number(AGENT_DESCRIPTIONS_THRESHOLD)
        ),
        details,
        current_value: total_tokens,
        threshold: AGENT_DESCRIPTIONS_THRESHOLD,
    })
}

/// Maps to CC `checkMcpTools(tools, ...)` grouping after
/// `countMcpToolTokens(...)`.
///
/// Transitional behavior: the official token counter can call API/model-side
/// counting through `analyzeContext.ts`. Doctor is constrained to safe local
/// probes in this port, so token totals are estimated from tool name and
/// description using `roughTokenCountEstimation` while preserving the official
/// threshold, server grouping, detail limit, and message shape.
pub fn check_mcp_tools(tools: &[Tool]) -> Option<ContextWarning> {
    let mcp_tools = tools.iter().filter(|tool| tool.is_mcp).collect::<Vec<_>>();
    if mcp_tools.is_empty() {
        return None;
    }

    let mut total_tokens = 0u64;
    let mut tools_by_server: HashMap<String, (usize, u64)> = HashMap::new();
    for tool in mcp_tools {
        let tokens = (crate::services::token_estimation::rough_token_count_estimation(&format!(
            "{}{}",
            tool.name, tool.description
        )) as u64);
        total_tokens += tokens;
        let server_name = tool
            .name
            .split("__")
            .nth(1)
            .filter(|name| !name.is_empty())
            .unwrap_or("unknown")
            .to_string();
        let entry = tools_by_server.entry(server_name).or_insert((0, 0));
        entry.0 += 1;
        entry.1 += tokens;
    }

    if total_tokens <= MCP_TOOLS_THRESHOLD {
        return None;
    }

    let mut sorted_servers = tools_by_server.into_iter().collect::<Vec<_>>();
    sorted_servers.sort_by(|a, b| b.1.1.cmp(&a.1.1));

    let mut details = sorted_servers
        .iter()
        .take(5)
        .map(|(name, (count, tokens))| {
            format!(
                "{name}: {count} tools (~{} tokens)",
                format_locale_number(*tokens)
            )
        })
        .collect::<Vec<_>>();
    if sorted_servers.len() > 5 {
        details.push(format!("({} more servers)", sorted_servers.len() - 5));
    }

    Some(ContextWarning {
        warning_type: ContextWarningType::McpTools,
        severity: ContextWarningSeverity::Warning,
        message: format!(
            "Large MCP tools context (~{} tokens estimated > {})",
            format_locale_number(total_tokens),
            format_locale_number(MCP_TOOLS_THRESHOLD)
        ),
        details,
        current_value: total_tokens,
        threshold: MCP_TOOLS_THRESHOLD,
    })
}

/// Maps to CC `checkUnreachableRules(getToolPermissionContext)`.
pub fn check_unreachable_rules(
    context: &ToolPermissionContext,
    sandbox_auto_allow_enabled: bool,
) -> Option<ContextWarning> {
    let unreachable = detect_unreachable_rules(
        context,
        DetectUnreachableRulesOptions {
            sandbox_auto_allow_enabled,
        },
    );
    if unreachable.is_empty() {
        return None;
    }

    let details = unreachable
        .iter()
        .flat_map(|rule| {
            [
                format!(
                    "{}: {}",
                    permission_rule_value_to_string(&rule.rule.rule_value),
                    rule.reason
                ),
                format!("  Fix: {}", rule.fix),
            ]
        })
        .collect::<Vec<_>>();

    Some(ContextWarning {
        warning_type: ContextWarningType::UnreachableRules,
        severity: ContextWarningSeverity::Warning,
        message: format!(
            "{} {} detected",
            unreachable.len(),
            plural(unreachable.len(), "unreachable permission rule", None)
        ),
        details,
        current_value: unreachable.len() as u64,
        threshold: 0,
    })
}

/// Maps to CC `checkContextWarnings(...)`.
pub fn check_context_warnings(input: &DoctorContextWarningsInput) -> ContextWarnings {
    ContextWarnings {
        claude_md_warning: check_claude_md_files(&input.memory_files),
        agent_warning: check_agent_descriptions(input.agent_definitions.as_ref()),
        mcp_warning: check_mcp_tools(&input.tools),
        unreachable_rules_warning: check_unreachable_rules(
            &input.tool_permission_context,
            input.sandbox_auto_allow_enabled,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::ToolPermissionContext;
    use crate::types::permissions::{PermissionRuleSource, PermissionRuleValue};
    use crate::utils::status_notice_helpers::{AgentDefinitionSnapshot, AgentDefinitionsSnapshot};

    fn mcp_tool(name: &str, description_len: usize) -> Tool {
        Tool {
            name: name.to_string(),
            description: "x".repeat(description_len),
            input_schema: serde_json::json!({"type": "object"}),
            is_mcp: true,
            ..Default::default()
        }
    }

    #[test]
    fn claude_md_warning_matches_official_message_and_sorted_details() {
        let warning = check_claude_md_files(&[
            MemoryFileInfo {
                path: "small/AGENTS.md".to_string(),
                content: "x".repeat(MAX_MEMORY_CHARACTER_COUNT),
            },
            MemoryFileInfo {
                path: "large-a/AGENTS.md".to_string(),
                content: "x".repeat(MAX_MEMORY_CHARACTER_COUNT + 1),
            },
            MemoryFileInfo {
                path: "large-b/AGENTS.md".to_string(),
                content: "x".repeat(MAX_MEMORY_CHARACTER_COUNT + 20),
            },
        ])
        .expect("large memory files warning");

        assert_eq!(warning.warning_type, ContextWarningType::ClaudeMdFiles);
        assert_eq!(
            warning.message,
            "2 large AGENTS.md files detected (each > 40,000 chars)"
        );
        assert!(warning.details[0].starts_with("large-b/AGENTS.md"));
        assert!(warning.details[0].contains("40,020 chars"));
    }

    #[test]
    fn agent_descriptions_warning_lists_top_custom_agents() {
        let definitions = AgentDefinitionsSnapshot {
            active_agents: vec![
                AgentDefinitionSnapshot {
                    agent_type: "built-in".to_string(),
                    when_to_use: "x".repeat(100_000),
                    source: "built-in".to_string(),
                },
                AgentDefinitionSnapshot {
                    agent_type: "reviewer".to_string(),
                    when_to_use: "x".repeat((AGENT_DESCRIPTIONS_THRESHOLD as usize + 1) * 4),
                    source: "project".to_string(),
                },
            ],
        };

        let warning = check_agent_descriptions(Some(&definitions)).expect("agent warning");
        assert_eq!(warning.warning_type, ContextWarningType::AgentDescriptions);
        assert!(warning.message.contains("Large agent descriptions (~15,0"));
        assert!(
            warning
                .details
                .iter()
                .any(|line| line.starts_with("reviewer: ~"))
        );
        assert!(!warning.details.iter().any(|line| line.contains("built-in")));
    }

    #[test]
    fn mcp_tools_warning_groups_by_server_and_limits_details() {
        let tools = vec![
            mcp_tool("mcp__alpha__one", 60_000),
            mcp_tool("mcp__alpha__two", 60_000),
            mcp_tool("mcp__beta__one", 60_000),
        ];
        let warning = check_mcp_tools(&tools).expect("mcp warning");

        assert_eq!(warning.warning_type, ContextWarningType::McpTools);
        assert!(warning.message.contains("Large MCP tools context"));
        assert!(
            warning
                .details
                .iter()
                .any(|line| line.starts_with("alpha: 2 tools"))
        );
        assert!(
            warning
                .details
                .iter()
                .any(|line| line.starts_with("beta: 1 tools"))
        );
    }

    #[test]
    fn unreachable_permission_rules_warning_uses_official_shadow_detection() {
        let mut context = ToolPermissionContext::default();
        context.always_ask_rules.insert(
            PermissionRuleSource::ProjectSettings,
            vec![PermissionRuleValue::new("Bash", None)],
        );
        context.always_allow_rules.insert(
            PermissionRuleSource::UserSettings,
            vec![PermissionRuleValue::new(
                "Bash",
                Some("cargo test:*".to_string()),
            )],
        );

        let warning = check_unreachable_rules(&context, false).expect("unreachable warning");
        assert_eq!(warning.warning_type, ContextWarningType::UnreachableRules);
        assert_eq!(warning.message, "1 unreachable permission rule detected");
        assert!(
            warning
                .details
                .iter()
                .any(|line| line.contains("Bash(cargo test:*)"))
        );
        assert!(warning.details.iter().any(|line| line.contains("Fix:")));
    }

    #[test]
    fn check_context_warnings_preserves_official_fields() {
        let input = DoctorContextWarningsInput {
            memory_files: vec![MemoryFileInfo {
                path: "AGENTS.md".to_string(),
                content: "x".repeat(MAX_MEMORY_CHARACTER_COUNT + 1),
            }],
            agent_definitions: None,
            tools: Vec::new(),
            tool_permission_context: ToolPermissionContext::default(),
            sandbox_auto_allow_enabled: false,
        };
        let warnings = check_context_warnings(&input);
        assert!(warnings.claude_md_warning.is_some());
        assert!(warnings.agent_warning.is_none());
        assert!(warnings.mcp_warning.is_none());
        assert!(warnings.unreachable_rules_warning.is_none());
    }
}
