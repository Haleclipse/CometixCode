//! Maps to: CC `utils/statusNoticeDefinitions.tsx`.
//!
//! Safety boundary: official definitions read auth sources, config, JetBrains
//! plugin state, and current working directory directly. This Rust port accepts
//! an explicit `StatusNoticeContext` snapshot so startup UI can stay pure and
//! side-effect free.

use super::status_notice_helpers::{
    AGENT_DESCRIPTIONS_THRESHOLD, AgentDefinitionsSnapshot, get_agent_descriptions_total_tokens,
};

pub const MAX_MEMORY_CHARACTER_COUNT: usize = 40_000;
pub const JETBRAINS_MARKETPLACE_URL: &str = "https://docs.claude.com/s/claude-code-jetbrains";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MemoryFileInfo {
    /// Maps to CC `MemoryFileInfo.path`.
    pub path: String,
    /// Maps to CC `MemoryFileInfo.content`.
    pub content: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StatusNoticeType {
    Warning,
    Info,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StatusNoticeId {
    LargeMemoryFiles,
    LargeAgentDescriptions,
    ClaudeAiExternalToken,
    ApiKeyConflict,
    BothAuthMethods,
    JetbrainsPluginInstall,
}

impl StatusNoticeId {
    pub fn id(self) -> &'static str {
        match self {
            Self::LargeMemoryFiles => "large-memory-files",
            Self::LargeAgentDescriptions => "large-agent-descriptions",
            Self::ClaudeAiExternalToken => "claude-ai-external-token",
            Self::ApiKeyConflict => "api-key-conflict",
            Self::BothAuthMethods => "both-auth-methods",
            Self::JetbrainsPluginInstall => "jetbrains-plugin-install",
        }
    }

    pub fn notice_type(self) -> StatusNoticeType {
        match self {
            Self::JetbrainsPluginInstall => StatusNoticeType::Info,
            _ => StatusNoticeType::Warning,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusNoticeContext {
    pub cwd: String,
    pub memory_files: Vec<MemoryFileInfo>,
    pub agent_definitions: Option<AgentDefinitionsSnapshot>,
    /// Maps to `getAuthTokenSource().source`.
    pub auth_token_source: String,
    /// Maps to `getAnthropicApiKeyWithSource(...).source`.
    pub api_key_source: String,
    /// Maps to `!!getApiKeyFromConfigOrMacOSKeychain()`.
    pub has_console_api_key: bool,
    /// Maps to `isClaudeAISubscriber()`.
    pub is_claude_ai_subscriber: bool,
    /// Maps to `config.autoInstallIdeExtension ?? true`.
    pub auto_install_ide_extension: bool,
    /// Maps to `isSupportedJetBrainsTerminal()`.
    pub is_supported_jetbrains_terminal: bool,
    /// Maps to `isJetBrainsPluginInstalledCachedSync(ideType)`.
    pub is_jetbrains_plugin_installed: bool,
    /// Maps to `toIDEDisplayName(getTerminalIdeType())`.
    pub ide_display_name: Option<String>,
}

impl Default for StatusNoticeContext {
    fn default() -> Self {
        Self {
            cwd: String::new(),
            memory_files: Vec::new(),
            agent_definitions: None,
            auth_token_source: "none".to_string(),
            api_key_source: "none".to_string(),
            has_console_api_key: false,
            is_claude_ai_subscriber: false,
            auto_install_ide_extension: true,
            is_supported_jetbrains_terminal: false,
            is_jetbrains_plugin_installed: false,
            ide_display_name: None,
        }
    }
}

/// Maps to CC `getLargeMemoryFiles(files)`.
pub fn get_large_memory_files(files: &[MemoryFileInfo]) -> Vec<MemoryFileInfo> {
    files
        .iter()
        .filter(|file| file.content.chars().count() > MAX_MEMORY_CHARACTER_COUNT)
        .cloned()
        .collect()
}

/// Maps to the `displayPath` branch in `largeMemoryFilesNotice.render`.
pub fn status_notice_memory_display_path(path: &str, cwd: &str) -> String {
    if !cwd.is_empty() && path.starts_with(cwd) {
        path.strip_prefix(cwd)
            .unwrap_or(path)
            .trim_start_matches(['/', '\\'])
            .to_string()
    } else {
        path.to_string()
    }
}

pub fn is_large_agent_descriptions_active(context: &StatusNoticeContext) -> bool {
    get_agent_descriptions_total_tokens(context.agent_definitions.as_ref())
        > AGENT_DESCRIPTIONS_THRESHOLD
}

pub fn is_claude_ai_external_token_active(context: &StatusNoticeContext) -> bool {
    context.is_claude_ai_subscriber
        && (context.auth_token_source == "ANTHROPIC_AUTH_TOKEN"
            || context.auth_token_source == "apiKeyHelper")
}

pub fn is_api_key_conflict_active(context: &StatusNoticeContext) -> bool {
    context.has_console_api_key
        && (context.api_key_source == "ANTHROPIC_API_KEY"
            || context.api_key_source == "apiKeyHelper")
}

pub fn is_both_auth_methods_active(context: &StatusNoticeContext) -> bool {
    context.api_key_source != "none"
        && context.auth_token_source != "none"
        && !(context.api_key_source == "apiKeyHelper"
            && context.auth_token_source == "apiKeyHelper")
}

pub fn is_jetbrains_plugin_notice_active(context: &StatusNoticeContext) -> bool {
    context.is_supported_jetbrains_terminal
        && context.auto_install_ide_extension
        && !context.is_jetbrains_plugin_installed
        && context.ide_display_name.is_some()
}

/// Maps to CC `getActiveNotices(context)` and preserves official order.
pub fn get_active_notices(context: &StatusNoticeContext) -> Vec<StatusNoticeId> {
    let mut notices = Vec::new();
    if !get_large_memory_files(&context.memory_files).is_empty() {
        notices.push(StatusNoticeId::LargeMemoryFiles);
    }
    if is_large_agent_descriptions_active(context) {
        notices.push(StatusNoticeId::LargeAgentDescriptions);
    }
    if is_claude_ai_external_token_active(context) {
        notices.push(StatusNoticeId::ClaudeAiExternalToken);
    }
    if is_api_key_conflict_active(context) {
        notices.push(StatusNoticeId::ApiKeyConflict);
    }
    if is_both_auth_methods_active(context) {
        notices.push(StatusNoticeId::BothAuthMethods);
    }
    if is_jetbrains_plugin_notice_active(context) {
        notices.push(StatusNoticeId::JetbrainsPluginInstall);
    }
    notices
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::status_notice_helpers::{AgentDefinitionSnapshot, AgentDefinitionsSnapshot};

    #[test]
    fn large_memory_files_use_official_threshold_and_relative_path() {
        let files = vec![MemoryFileInfo {
            path: "/repo/AGENTS.md".to_string(),
            content: "x".repeat(MAX_MEMORY_CHARACTER_COUNT + 1),
        }];
        assert_eq!(get_large_memory_files(&files), files);
        assert_eq!(
            status_notice_memory_display_path("/repo/AGENTS.md", "/repo"),
            "AGENTS.md"
        );
    }

    #[test]
    fn active_notices_preserve_official_order() {
        let mut context = StatusNoticeContext::default();
        context.memory_files.push(MemoryFileInfo {
            path: "/repo/AGENTS.md".to_string(),
            content: "x".repeat(MAX_MEMORY_CHARACTER_COUNT + 1),
        });
        context.agent_definitions = Some(AgentDefinitionsSnapshot {
            active_agents: vec![AgentDefinitionSnapshot {
                agent_type: "reviewer".to_string(),
                when_to_use: "x".repeat((AGENT_DESCRIPTIONS_THRESHOLD as usize + 1) * 4),
                source: "project".to_string(),
            }],
        });
        context.is_claude_ai_subscriber = true;
        context.auth_token_source = "ANTHROPIC_AUTH_TOKEN".to_string();
        context.has_console_api_key = true;
        context.api_key_source = "ANTHROPIC_API_KEY".to_string();
        context.is_supported_jetbrains_terminal = true;
        context.is_jetbrains_plugin_installed = false;
        context.ide_display_name = Some("PyCharm".to_string());

        assert_eq!(
            get_active_notices(&context),
            vec![
                StatusNoticeId::LargeMemoryFiles,
                StatusNoticeId::LargeAgentDescriptions,
                StatusNoticeId::ClaudeAiExternalToken,
                StatusNoticeId::ApiKeyConflict,
                StatusNoticeId::BothAuthMethods,
                StatusNoticeId::JetbrainsPluginInstall,
            ]
        );
    }

    #[test]
    fn api_key_helper_for_both_sources_does_not_trigger_both_auth_notice() {
        let context = StatusNoticeContext {
            api_key_source: "apiKeyHelper".to_string(),
            auth_token_source: "apiKeyHelper".to_string(),
            ..StatusNoticeContext::default()
        };
        assert!(!is_both_auth_methods_active(&context));
    }
}
