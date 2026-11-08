//! Hook settings display and grouping helpers.
//! Maps to: CC `utils/hooks/hooksSettings.ts`.
//!
//! The official module is used by hook configuration UI to enumerate hooks by
//! source, compare persisted hooks, and render source labels. Nyan keeps the
//! same responsibility boundary in utils/hooks because hooks settings are loaded
//! from settings/session-hook stores rather than component state.

use crate::services::hooks::{HOOK_EVENTS, HookCommand, HookEvent};
use crate::utils::settings;
use crate::utils::settings::SettingsJson;
use crate::utils::settings::constants::SettingSource;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Maps to: CC `DEFAULT_HOOK_SHELL` imported by `hooksSettings.ts`.
pub const DEFAULT_HOOK_SHELL: &str = "bash";

/// Maps to: CC `HookSource`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HookSource {
    UserSettings,
    ProjectSettings,
    LocalSettings,
    PluginHook,
    SessionHook,
    BuiltinHook,
}

impl HookSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UserSettings => "userSettings",
            Self::ProjectSettings => "projectSettings",
            Self::LocalSettings => "localSettings",
            Self::PluginHook => "pluginHook",
            Self::SessionHook => "sessionHook",
            Self::BuiltinHook => "builtinHook",
        }
    }
}

/// Maps to the four persisted variants of CC `HookCommand` consumed by the
/// read-only hooks browser. Hook execution keeps using the command-only
/// [`HookCommand`] shape; parsing the browser shape separately prevents
/// prompt/agent/http entries from being mistaken for shell commands.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HookConfigKind {
    #[default]
    Command,
    Prompt,
    Agent,
    Http,
}

impl HookConfigKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Command => "command",
            Self::Prompt => "prompt",
            Self::Agent => "agent",
            Self::Http => "http",
        }
    }
}

/// Read-only projection of the official settings hook union.
/// Maps to: CC `utils/settings/types.ts` `HookCommand`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HookDisplayConfig {
    #[serde(rename = "type")]
    pub kind: HookConfigKind,
    pub command: Option<String>,
    pub prompt: Option<String>,
    pub url: Option<String>,
    pub shell: Option<String>,
    #[serde(rename = "if")]
    pub condition: Option<String>,
    pub status_message: Option<String>,
    pub timeout: Option<u64>,
}

impl HookDisplayConfig {
    pub fn from_command(command: HookCommand) -> Self {
        Self {
            kind: HookConfigKind::Command,
            command: Some(command.command),
            shell: command.shell,
            condition: command.condition,
            status_message: command.status,
            timeout: command.timeout,
            prompt: None,
            url: None,
        }
    }

    pub fn content_label(&self) -> &'static str {
        match self.kind {
            HookConfigKind::Command => "Command",
            HookConfigKind::Prompt | HookConfigKind::Agent => "Prompt",
            HookConfigKind::Http => "URL",
        }
    }

    pub fn content_value(&self) -> &str {
        match self.kind {
            HookConfigKind::Command => self.command.as_deref().unwrap_or(""),
            HookConfigKind::Prompt | HookConfigKind::Agent => self.prompt.as_deref().unwrap_or(""),
            HookConfigKind::Http => self.url.as_deref().unwrap_or(""),
        }
    }
}

/// Maps to: CC `IndividualHookConfig`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndividualHookConfig {
    pub event: HookEvent,
    pub config: HookDisplayConfig,
    pub matcher: Option<String>,
    pub source: HookSource,
    pub plugin_name: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct DisplayHookMatcher {
    matcher: Option<String>,
    hooks: Vec<HookDisplayConfig>,
}

type DisplayHooksConfig = HashMap<String, Vec<DisplayHookMatcher>>;

fn editable_hook_sources() -> [(SettingSource, HookSource); 3] {
    [
        (SettingSource::User, HookSource::UserSettings),
        (SettingSource::Project, HookSource::ProjectSettings),
        (SettingSource::Local, HookSource::LocalSettings),
    ]
}

fn event_from_settings_key(key: &str) -> Option<HookEvent> {
    HOOK_EVENTS
        .iter()
        .copied()
        .find(|event| event.as_str() == key)
}

fn hooks_config_from_settings(settings: &SettingsJson) -> DisplayHooksConfig {
    settings
        .hooks
        .as_ref()
        .and_then(|hooks| serde_json::from_value::<DisplayHooksConfig>(hooks.clone()).ok())
        .unwrap_or_default()
}

fn push_hooks_from_config(
    hooks: &mut Vec<IndividualHookConfig>,
    source: HookSource,
    config: DisplayHooksConfig,
) {
    for (event, matchers) in config {
        let Some(event) = event_from_settings_key(&event) else {
            continue;
        };
        for DisplayHookMatcher {
            matcher,
            hooks: hook_commands,
        } in matchers
        {
            for hook_command in hook_commands {
                hooks.push(IndividualHookConfig {
                    event,
                    config: hook_command,
                    matcher: matcher.clone(),
                    source,
                    plugin_name: None,
                });
            }
        }
    }
}

fn resolved_source_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    }
}

/// Maps to: CC `isHookEqual(...)` for persisted command hooks.
///
/// Rust currently persists command hooks only; prompt/agent/http hook execution
/// parity is tracked separately with `execPromptHook.ts`, `execAgentHook.ts`,
/// and `execHttpHook.ts`.
pub fn is_hook_equal(a: &HookCommand, b: &HookCommand) -> bool {
    a.command == b.command
        && a.shell.as_deref().unwrap_or(DEFAULT_HOOK_SHELL)
            == b.shell.as_deref().unwrap_or(DEFAULT_HOOK_SHELL)
        && a.condition.as_deref().unwrap_or("") == b.condition.as_deref().unwrap_or("")
}

/// Maps to: CC `getHookDisplayText(...)` for command/prompt/agent/http hooks.
pub fn get_hook_display_text(hook: &HookDisplayConfig) -> &str {
    hook.status_message
        .as_deref()
        .unwrap_or_else(|| hook.content_value())
}

/// Maps to: CC `getAllHooks(appState)`.
///
/// Official reads session hooks from `AppState.sessionHooks`; Nyan's current
/// `session_hooks` port is process-local, so callers pass the active session id.
pub fn get_all_hooks(session_id: &str) -> Vec<IndividualHookConfig> {
    let mut hooks = Vec::new();

    let mut seen_files = HashSet::<PathBuf>::new();
    for (setting_source, hook_source) in editable_hook_sources() {
        if let Some(file_path) = settings::get_settings_file_path_for_source(setting_source) {
            let resolved = resolved_source_path(&file_path);
            if !seen_files.insert(resolved) {
                continue;
            }
        }

        let Some(source_settings) = settings::get_settings_for_source(setting_source) else {
            continue;
        };
        push_hooks_from_config(
            &mut hooks,
            hook_source,
            hooks_config_from_settings(&source_settings),
        );
    }

    let session_hooks = super::session_hooks::get_session_hooks(session_id, None)
        .into_iter()
        .map(|(event, matchers)| {
            let matchers = matchers
                .into_iter()
                .map(|matcher| DisplayHookMatcher {
                    matcher: matcher.matcher,
                    hooks: matcher
                        .hooks
                        .into_iter()
                        .map(HookDisplayConfig::from_command)
                        .collect(),
                })
                .collect();
            (event, matchers)
        })
        .collect();
    push_hooks_from_config(&mut hooks, HookSource::SessionHook, session_hooks);

    hooks
}

/// Maps to: CC `getHooksForEvent(...)`.
pub fn get_hooks_for_event(session_id: &str, event: HookEvent) -> Vec<IndividualHookConfig> {
    get_all_hooks(session_id)
        .into_iter()
        .filter(|hook| hook.event == event)
        .collect()
}

/// Maps to: CC `hookSourceDescriptionDisplayString(...)`.
pub fn hook_source_description_display_string(source: HookSource) -> &'static str {
    match source {
        HookSource::UserSettings => "User settings (~/.nyan/settings.json)",
        HookSource::ProjectSettings => "Project settings (.nyan/settings.json)",
        HookSource::LocalSettings => "Local settings (.nyan/settings.local.json)",
        HookSource::PluginHook => "Plugin hooks (~/.nyan/plugins/*/hooks/hooks.json)",
        HookSource::SessionHook => "Session hooks (in-memory, temporary)",
        HookSource::BuiltinHook => "Built-in hooks (registered internally by nyan)",
    }
}

/// Maps to: CC `hookSourceHeaderDisplayString(...)`.
pub fn hook_source_header_display_string(source: HookSource) -> &'static str {
    match source {
        HookSource::UserSettings => "User Settings",
        HookSource::ProjectSettings => "Project Settings",
        HookSource::LocalSettings => "Local Settings",
        HookSource::PluginHook => "Plugin Hooks",
        HookSource::SessionHook => "Session Hooks",
        HookSource::BuiltinHook => "Built-in Hooks",
    }
}

/// Maps to: CC `hookSourceInlineDisplayString(...)`.
pub fn hook_source_inline_display_string(source: HookSource) -> &'static str {
    match source {
        HookSource::UserSettings => "User",
        HookSource::ProjectSettings => "Project",
        HookSource::LocalSettings => "Local",
        HookSource::PluginHook => "Plugin",
        HookSource::SessionHook => "Session",
        HookSource::BuiltinHook => "Built-in",
    }
}

fn source_priority(source: HookSource) -> usize {
    match source {
        // Maps to CC `SOURCES = ['localSettings', 'projectSettings', 'userSettings']`.
        HookSource::LocalSettings => 0,
        HookSource::ProjectSettings => 1,
        HookSource::UserSettings => 2,
        HookSource::PluginHook | HookSource::BuiltinHook => 999,
        // Non-editable sources are not in CC `SOURCES`; keep them stable after
        // editable settings sources.
        HookSource::SessionHook => 999,
    }
}

/// Maps to: CC `sortMatchersByPriority(...)`.
pub fn sort_matchers_by_priority(
    matchers: &[String],
    hooks_by_event_and_matcher: &std::collections::HashMap<
        HookEvent,
        std::collections::HashMap<String, Vec<IndividualHookConfig>>,
    >,
    selected_event: HookEvent,
) -> Vec<String> {
    let mut sorted = matchers.to_vec();
    sorted.sort_by(|a, b| {
        let a_hooks = hooks_by_event_and_matcher
            .get(&selected_event)
            .and_then(|by_matcher| by_matcher.get(a))
            .cloned()
            .unwrap_or_default();
        let b_hooks = hooks_by_event_and_matcher
            .get(&selected_event)
            .and_then(|by_matcher| by_matcher.get(b))
            .cloned()
            .unwrap_or_default();

        let a_priority = a_hooks
            .iter()
            .map(|hook| source_priority(hook.source))
            .min()
            .unwrap_or(usize::MAX);
        let b_priority = b_hooks
            .iter()
            .map(|hook| source_priority(hook.source))
            .min()
            .unwrap_or(usize::MAX);

        a_priority.cmp(&b_priority).then_with(|| a.cmp(b))
    });
    sorted
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn command(command: &str) -> HookCommand {
        HookCommand {
            command: command.to_string(),
            shell: None,
            timeout: None,
            condition: None,
            status: None,
            once: None,
            is_async: None,
            async_rewake: None,
        }
    }

    fn settings_with_hooks() -> SettingsJson {
        SettingsJson {
            hooks: Some(serde_json::json!({
                "PreToolUse": [{
                    "matcher": "Bash",
                    "hooks": [{
                        "type": "command",
                        "command": "echo pre",
                        "statusMessage": "Checking"
                    }]
                }],
                "Stop": [{
                    "hooks": [{"type": "command", "command": "echo stop"}]
                }]
            })),
            ..SettingsJson::default()
        }
    }

    #[test]
    fn hook_identity_matches_official_command_shell_and_if_semantics() {
        let mut a = command("echo hi");
        let mut b = command("echo hi");
        assert!(is_hook_equal(&a, &b));

        b.shell = Some("bash".to_string());
        assert!(is_hook_equal(&a, &b));

        b.shell = Some("powershell".to_string());
        assert!(!is_hook_equal(&a, &b));

        b.shell = None;
        b.timeout = Some(30);
        assert!(is_hook_equal(&a, &b));

        a.condition = Some("Bash(git *)".to_string());
        assert!(!is_hook_equal(&a, &b));
    }

    #[test]
    fn display_text_prefers_official_status_message_alias() {
        let config = hooks_config_from_settings(&settings_with_hooks());
        let hook = &config["PreToolUse"][0].hooks[0];
        assert_eq!(get_hook_display_text(hook), "Checking");
        assert_eq!(hook.status_message.as_deref(), Some("Checking"));
    }

    #[test]
    fn display_parser_preserves_all_four_official_hook_types() {
        let settings = SettingsJson {
            hooks: Some(serde_json::json!({
                "Stop": [{"hooks": [
                    {"type": "command", "command": "echo stop"},
                    {"type": "prompt", "prompt": "Review the response"},
                    {"type": "agent", "prompt": "Verify the result"},
                    {"type": "http", "url": "https://example.test/hook"}
                ]}]
            })),
            ..SettingsJson::default()
        };
        let config = hooks_config_from_settings(&settings);
        let hooks = &config["Stop"][0].hooks;
        assert_eq!(hooks.len(), 4);
        assert_eq!(hooks[0].kind, HookConfigKind::Command);
        assert_eq!(hooks[1].content_value(), "Review the response");
        assert_eq!(hooks[2].kind, HookConfigKind::Agent);
        assert_eq!(hooks[3].content_label(), "URL");
    }

    #[test]
    fn settings_hooks_expand_to_individual_hook_configs() {
        let mut hooks = Vec::new();
        push_hooks_from_config(
            &mut hooks,
            HookSource::ProjectSettings,
            hooks_config_from_settings(&settings_with_hooks()),
        );

        assert_eq!(hooks.len(), 2);
        let pre = hooks
            .iter()
            .find(|hook| hook.event == HookEvent::PreToolUse)
            .expect("pre hook");
        assert_eq!(pre.matcher.as_deref(), Some("Bash"));
        assert_eq!(pre.source, HookSource::ProjectSettings);
        assert_eq!(pre.config.command.as_deref(), Some("echo pre"));
        let stop = hooks
            .iter()
            .find(|hook| hook.event == HookEvent::Stop)
            .expect("stop hook");
        assert_eq!(stop.matcher, None);
    }

    #[test]
    fn source_display_strings_match_official_copy() {
        assert_eq!(
            hook_source_description_display_string(HookSource::UserSettings),
            "User settings (~/.nyan/settings.json)"
        );
        assert_eq!(
            hook_source_header_display_string(HookSource::PluginHook),
            "Plugin Hooks"
        );
        assert_eq!(
            hook_source_inline_display_string(HookSource::BuiltinHook),
            "Built-in"
        );
    }

    #[test]
    fn matchers_sort_by_editable_source_priority_then_name() {
        let local = IndividualHookConfig {
            event: HookEvent::PreToolUse,
            config: HookDisplayConfig::from_command(command("echo local")),
            matcher: Some("Write".to_string()),
            source: HookSource::LocalSettings,
            plugin_name: None,
        };
        let user = IndividualHookConfig {
            event: HookEvent::PreToolUse,
            config: HookDisplayConfig::from_command(command("echo user")),
            matcher: Some("Bash".to_string()),
            source: HookSource::UserSettings,
            plugin_name: None,
        };
        let plugin = IndividualHookConfig {
            event: HookEvent::PreToolUse,
            config: HookDisplayConfig::from_command(command("echo plugin")),
            matcher: Some("Read".to_string()),
            source: HookSource::PluginHook,
            plugin_name: Some("plugin".to_string()),
        };
        let mut by_matcher = HashMap::new();
        by_matcher.insert("Read".to_string(), vec![plugin]);
        by_matcher.insert("Bash".to_string(), vec![user]);
        by_matcher.insert("Write".to_string(), vec![local]);
        let mut grouped = HashMap::new();
        grouped.insert(HookEvent::PreToolUse, by_matcher);

        let sorted = sort_matchers_by_priority(
            &["Read".to_string(), "Bash".to_string(), "Write".to_string()],
            &grouped,
            HookEvent::PreToolUse,
        );
        assert_eq!(sorted, vec!["Write", "Bash", "Read"]);
    }
}
