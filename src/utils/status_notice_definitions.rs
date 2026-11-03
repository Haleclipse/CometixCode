//! Maps to: CC `utils/statusNoticeDefinitions.tsx`.
//!
//! Safety boundary: official definitions read config, JetBrains plugin state,
//! and current working directory directly. This Rust port accepts
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
    JetbrainsPluginInstall,
}

impl StatusNoticeId {
    pub fn id(self) -> &'static str {
        match self {
            Self::LargeMemoryFiles => "large-memory-files",
            Self::LargeAgentDescriptions => "large-agent-descriptions",
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
        context.is_supported_jetbrains_terminal = true;
        context.is_jetbrains_plugin_installed = false;
        context.ide_display_name = Some("PyCharm".to_string());

        assert_eq!(
            get_active_notices(&context),
            vec![
                StatusNoticeId::LargeMemoryFiles,
                StatusNoticeId::LargeAgentDescriptions,
                StatusNoticeId::JetbrainsPluginInstall,
            ]
        );
    }
}
