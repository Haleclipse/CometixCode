//! Hook configuration grouping and metadata helpers.
//! Maps to: CC `utils/hooks/hooksConfigManager.ts`.
//!
//! This module keeps hook-config menu metadata and grouping separate from hook
//! execution. Registered plugin/callback hooks are still pending because the
//! official `getRegisteredHooks()` bootstrap store has not been fully ported;
//! settings and session hooks are grouped through `hooks_settings`.

use super::hooks_settings::{IndividualHookConfig, get_all_hooks, sort_matchers_by_priority};
use crate::services::hooks::{HOOK_EVENTS, HookEvent};
use std::collections::HashMap;

/// Maps to: CC `MatcherMetadata`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatcherMetadata {
    pub field_to_match: &'static str,
    pub values: Vec<String>,
}

/// Maps to: CC `HookEventMetadata`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookEventMetadata {
    pub summary: &'static str,
    pub description: &'static str,
    pub matcher_metadata: Option<MatcherMetadata>,
}

/// Stable `Object.entries(getHookEventMetadata(...))` order from the official
/// component. A `HashMap` alone cannot preserve the menu's source order.
pub const HOOK_MENU_EVENTS: &[HookEvent] = &[
    HookEvent::PreToolUse,
    HookEvent::PostToolUse,
    HookEvent::PostToolUseFailure,
    HookEvent::PermissionDenied,
    HookEvent::Notification,
    HookEvent::UserPromptSubmit,
    HookEvent::SessionStart,
    HookEvent::Stop,
    HookEvent::StopFailure,
    HookEvent::SubagentStart,
    HookEvent::SubagentStop,
    HookEvent::PreCompact,
    HookEvent::PostCompact,
    HookEvent::SessionEnd,
    HookEvent::PermissionRequest,
    HookEvent::Setup,
    HookEvent::TeammateIdle,
    HookEvent::TaskCreated,
    HookEvent::TaskCompleted,
    HookEvent::Elicitation,
    HookEvent::ElicitationResult,
    HookEvent::ConfigChange,
    HookEvent::InstructionsLoaded,
    HookEvent::WorktreeCreate,
    HookEvent::WorktreeRemove,
    HookEvent::CwdChanged,
    HookEvent::FileChanged,
];

fn tool_matcher(tool_names: &[String]) -> Option<MatcherMetadata> {
    Some(MatcherMetadata {
        field_to_match: "tool_name",
        values: tool_names.to_vec(),
    })
}

fn literal_matcher(field_to_match: &'static str, values: &[&str]) -> Option<MatcherMetadata> {
    Some(MatcherMetadata {
        field_to_match,
        values: values.iter().map(|value| (*value).to_string()).collect(),
    })
}

fn empty_matcher(field_to_match: &'static str) -> Option<MatcherMetadata> {
    Some(MatcherMetadata {
        field_to_match,
        values: Vec::new(),
    })
}

/// Maps to: CC `getHookEventMetadata(toolNames)`.
///
/// CC memoizes this by sorted tool-name key; Nyan computes a fresh map since
/// this service helper is cheap and currently called on demand by command UI.
pub fn get_hook_event_metadata(tool_names: &[String]) -> HashMap<HookEvent, HookEventMetadata> {
    HashMap::from([
        (
            HookEvent::PreToolUse,
            HookEventMetadata {
                summary: "Before tool execution",
                description: "Input to command is JSON of tool call arguments.\nExit code 0 - stdout/stderr not shown\nExit code 2 - show stderr to model and block tool call\nOther exit codes - show stderr to user only but continue with tool call",
                matcher_metadata: tool_matcher(tool_names),
            },
        ),
        (
            HookEvent::PostToolUse,
            HookEventMetadata {
                summary: "After tool execution",
                description: "Input to command is JSON with fields \"inputs\" (tool call arguments) and \"response\" (tool call response).\nExit code 0 - stdout shown in transcript mode (ctrl+o)\nExit code 2 - show stderr to model immediately\nOther exit codes - show stderr to user only",
                matcher_metadata: tool_matcher(tool_names),
            },
        ),
        (
            HookEvent::PostToolUseFailure,
            HookEventMetadata {
                summary: "After tool execution fails",
                description: "Input to command is JSON with tool_name, tool_input, tool_use_id, error, error_type, is_interrupt, and is_timeout.\nExit code 0 - stdout shown in transcript mode (ctrl+o)\nExit code 2 - show stderr to model immediately\nOther exit codes - show stderr to user only",
                matcher_metadata: tool_matcher(tool_names),
            },
        ),
        (
            HookEvent::PermissionDenied,
            HookEventMetadata {
                summary: "After auto mode classifier denies a tool call",
                description: "Input to command is JSON with tool_name, tool_input, tool_use_id, and reason.\nReturn {\"hookSpecificOutput\":{\"hookEventName\":\"PermissionDenied\",\"retry\":true}} to tell the model it may retry.\nExit code 0 - stdout shown in transcript mode (ctrl+o)\nOther exit codes - show stderr to user only",
                matcher_metadata: tool_matcher(tool_names),
            },
        ),
        (
            HookEvent::Notification,
            HookEventMetadata {
                summary: "When notifications are sent",
                description: "Input to command is JSON with notification message and type.\nExit code 0 - stdout/stderr not shown\nOther exit codes - show stderr to user only",
                matcher_metadata: literal_matcher(
                    "notification_type",
                    &[
                        "permission_prompt",
                        "idle_prompt",
                        "auth_success",
                        "elicitation_dialog",
                        "elicitation_complete",
                        "elicitation_response",
                    ],
                ),
            },
        ),
        (
            HookEvent::UserPromptSubmit,
            HookEventMetadata {
                summary: "When the user submits a prompt",
                description: "Input to command is JSON with original user prompt text.\nExit code 0 - stdout shown to Claude\nExit code 2 - block processing, erase original prompt, and show stderr to user only\nOther exit codes - show stderr to user only",
                matcher_metadata: None,
            },
        ),
        (
            HookEvent::SessionStart,
            HookEventMetadata {
                summary: "When a new session is started",
                description: "Input to command is JSON with session start source.\nExit code 0 - stdout shown to Claude\nBlocking errors are ignored\nOther exit codes - show stderr to user only",
                matcher_metadata: literal_matcher(
                    "source",
                    &["startup", "resume", "clear", "compact"],
                ),
            },
        ),
        (
            HookEvent::Stop,
            HookEventMetadata {
                summary: "Right before Claude concludes its response",
                description: "Exit code 0 - stdout/stderr not shown\nExit code 2 - show stderr to model and continue conversation\nOther exit codes - show stderr to user only",
                matcher_metadata: None,
            },
        ),
        (
            HookEvent::StopFailure,
            HookEventMetadata {
                summary: "When the turn ends due to an API error",
                description: "Fires instead of Stop when an API error (rate limit, auth failure, etc.) ended the turn. Fire-and-forget — hook output and exit codes are ignored.",
                matcher_metadata: literal_matcher(
                    "error",
                    &[
                        "rate_limit",
                        "authentication_failed",
                        "billing_error",
                        "invalid_request",
                        "server_error",
                        "max_output_tokens",
                        "unknown",
                    ],
                ),
            },
        ),
        (
            HookEvent::SubagentStart,
            HookEventMetadata {
                summary: "When a subagent (Agent tool call) is started",
                description: "Input to command is JSON with agent_id and agent_type.\nExit code 0 - stdout shown to subagent\nBlocking errors are ignored\nOther exit codes - show stderr to user only",
                matcher_metadata: empty_matcher("agent_type"),
            },
        ),
        (
            HookEvent::SubagentStop,
            HookEventMetadata {
                summary: "Right before a subagent (Agent tool call) concludes its response",
                description: "Input to command is JSON with agent_id, agent_type, and agent_transcript_path.\nExit code 0 - stdout/stderr not shown\nExit code 2 - show stderr to subagent and continue having it run\nOther exit codes - show stderr to user only",
                matcher_metadata: empty_matcher("agent_type"),
            },
        ),
        (
            HookEvent::PreCompact,
            HookEventMetadata {
                summary: "Before conversation compaction",
                description: "Input to command is JSON with compaction details.\nExit code 0 - stdout appended as custom compact instructions\nExit code 2 - block compaction\nOther exit codes - show stderr to user only but continue with compaction",
                matcher_metadata: literal_matcher("trigger", &["manual", "auto"]),
            },
        ),
        (
            HookEvent::PostCompact,
            HookEventMetadata {
                summary: "After conversation compaction",
                description: "Input to command is JSON with compaction details and the summary.\nExit code 0 - stdout shown to user\nOther exit codes - show stderr to user only",
                matcher_metadata: literal_matcher("trigger", &["manual", "auto"]),
            },
        ),
        (
            HookEvent::SessionEnd,
            HookEventMetadata {
                summary: "When a session is ending",
                description: "Input to command is JSON with session end reason.\nExit code 0 - command completes successfully\nOther exit codes - show stderr to user only",
                matcher_metadata: literal_matcher(
                    "reason",
                    &["clear", "logout", "prompt_input_exit", "other"],
                ),
            },
        ),
        (
            HookEvent::PermissionRequest,
            HookEventMetadata {
                summary: "When a permission dialog is displayed",
                description: "Input to command is JSON with tool_name, tool_input, and tool_use_id.\nOutput JSON with hookSpecificOutput containing decision to allow or deny.\nExit code 0 - use hook decision if provided\nOther exit codes - show stderr to user only",
                matcher_metadata: tool_matcher(tool_names),
            },
        ),
        (
            HookEvent::Setup,
            HookEventMetadata {
                summary: "Repo setup hooks for init and maintenance",
                description: "Input to command is JSON with trigger (init or maintenance).\nExit code 0 - stdout shown to Claude\nBlocking errors are ignored\nOther exit codes - show stderr to user only",
                matcher_metadata: literal_matcher("trigger", &["init", "maintenance"]),
            },
        ),
        (
            HookEvent::TeammateIdle,
            HookEventMetadata {
                summary: "When a teammate is about to go idle",
                description: "Input to command is JSON with teammate_name and team_name.\nExit code 0 - stdout/stderr not shown\nExit code 2 - show stderr to teammate and prevent idle (teammate continues working)\nOther exit codes - show stderr to user only",
                matcher_metadata: None,
            },
        ),
        (
            HookEvent::TaskCreated,
            HookEventMetadata {
                summary: "When a task is being created",
                description: "Input to command is JSON with task_id, task_subject, task_description, teammate_name, and team_name.\nExit code 0 - stdout/stderr not shown\nExit code 2 - show stderr to model and prevent task creation\nOther exit codes - show stderr to user only",
                matcher_metadata: None,
            },
        ),
        (
            HookEvent::TaskCompleted,
            HookEventMetadata {
                summary: "When a task is being marked as completed",
                description: "Input to command is JSON with task_id, task_subject, task_description, teammate_name, and team_name.\nExit code 0 - stdout/stderr not shown\nExit code 2 - show stderr to model and prevent task completion\nOther exit codes - show stderr to user only",
                matcher_metadata: None,
            },
        ),
        (
            HookEvent::Elicitation,
            HookEventMetadata {
                summary: "When an MCP server requests user input (elicitation)",
                description: "Input to command is JSON with mcp_server_name, message, and requested_schema.\nOutput JSON with hookSpecificOutput containing action (accept/decline/cancel) and optional content.\nExit code 0 - use hook response if provided\nExit code 2 - deny the elicitation\nOther exit codes - show stderr to user only",
                matcher_metadata: empty_matcher("mcp_server_name"),
            },
        ),
        (
            HookEvent::ElicitationResult,
            HookEventMetadata {
                summary: "After a user responds to an MCP elicitation",
                description: "Input to command is JSON with mcp_server_name, action, content, mode, and elicitation_id.\nOutput JSON with hookSpecificOutput containing optional action and content to override the response.\nExit code 0 - use hook response if provided\nExit code 2 - block the response (action becomes decline)\nOther exit codes - show stderr to user only",
                matcher_metadata: empty_matcher("mcp_server_name"),
            },
        ),
        (
            HookEvent::ConfigChange,
            HookEventMetadata {
                summary: "When configuration files change during a session",
                description: "Input to command is JSON with source (user_settings, project_settings, local_settings, skills) and file_path.\nExit code 0 - allow the change\nExit code 2 - block the change from being applied to the session\nOther exit codes - show stderr to user only",
                matcher_metadata: literal_matcher(
                    "source",
                    &[
                        "user_settings",
                        "project_settings",
                        "local_settings",
                        "skills",
                    ],
                ),
            },
        ),
        (
            HookEvent::InstructionsLoaded,
            HookEventMetadata {
                summary: "When an instruction file (AGENTS.md) is loaded",
                description: "Input to command is JSON with file_path, memory_type (User, Project, Local, Managed), load_reason (session_start, nested_traversal, path_glob_match, include, compact), globs (optional — the paths: frontmatter patterns that matched), trigger_file_path (optional — the file Claude touched that caused the load), and parent_file_path (optional — the file that @-included this one).\nExit code 0 - command completes successfully\nOther exit codes - show stderr to user only\nThis hook is observability-only and does not support blocking.",
                matcher_metadata: literal_matcher(
                    "load_reason",
                    &[
                        "session_start",
                        "nested_traversal",
                        "path_glob_match",
                        "include",
                        "compact",
                    ],
                ),
            },
        ),
        (
            HookEvent::WorktreeCreate,
            HookEventMetadata {
                summary: "Create an isolated worktree for VCS-agnostic isolation",
                description: "Input to command is JSON with name (suggested worktree slug).\nStdout should contain the absolute path to the created worktree directory.\nExit code 0 - worktree created successfully\nOther exit codes - worktree creation failed",
                matcher_metadata: None,
            },
        ),
        (
            HookEvent::WorktreeRemove,
            HookEventMetadata {
                summary: "Remove a previously created worktree",
                description: "Input to command is JSON with worktree_path (absolute path to worktree).\nExit code 0 - worktree removed successfully\nOther exit codes - show stderr to user only",
                matcher_metadata: None,
            },
        ),
        (
            HookEvent::CwdChanged,
            HookEventMetadata {
                summary: "After the working directory changes",
                description: "Input to command is JSON with old_cwd and new_cwd.\nCLAUDE_ENV_FILE is set — write bash exports there to apply env to subsequent BashTool commands.\nHook output can include hookSpecificOutput.watchPaths (array of absolute paths) to register with the FileChanged watcher.\nExit code 0 - command completes successfully\nOther exit codes - show stderr to user only",
                matcher_metadata: None,
            },
        ),
        (
            HookEvent::FileChanged,
            HookEventMetadata {
                summary: "When a watched file changes",
                description: "Input to command is JSON with file_path and event (change, add, unlink).\nCLAUDE_ENV_FILE is set — write bash exports there to apply env to subsequent BashTool commands.\nThe matcher field specifies filenames to watch in the current directory (e.g. \".envrc|.env\").\nHook output can include hookSpecificOutput.watchPaths (array of absolute paths) to dynamically update the watch list.\nExit code 0 - command completes successfully\nOther exit codes - show stderr to user only",
                matcher_metadata: None,
            },
        ),
    ])
}

fn empty_grouped_hooks() -> HashMap<HookEvent, HashMap<String, Vec<IndividualHookConfig>>> {
    HOOK_EVENTS
        .iter()
        .copied()
        .map(|event| (event, HashMap::new()))
        .collect()
}

/// Maps to the settings/session hook grouping portion of CC
/// `groupHooksByEventAndMatcher(appState, toolNames)`.
pub fn group_individual_hooks_by_event_and_matcher(
    hooks: impl IntoIterator<Item = IndividualHookConfig>,
    tool_names: &[String],
) -> HashMap<HookEvent, HashMap<String, Vec<IndividualHookConfig>>> {
    let mut grouped = empty_grouped_hooks();
    let metadata = get_hook_event_metadata(tool_names);

    for hook in hooks {
        let matcher_key = if metadata
            .get(&hook.event)
            .and_then(|metadata| metadata.matcher_metadata.as_ref())
            .is_some()
        {
            hook.matcher.clone().unwrap_or_default()
        } else {
            String::new()
        };
        grouped
            .entry(hook.event)
            .or_default()
            .entry(matcher_key)
            .or_default()
            .push(hook);
    }

    grouped
}

/// Maps to: CC `groupHooksByEventAndMatcher(appState, toolNames)` for
/// settings/session hooks.
pub fn group_hooks_by_event_and_matcher(
    session_id: &str,
    tool_names: &[String],
) -> HashMap<HookEvent, HashMap<String, Vec<IndividualHookConfig>>> {
    group_individual_hooks_by_event_and_matcher(get_all_hooks(session_id), tool_names)
}

/// Maps to: CC `getSortedMatchersForEvent(...)`.
pub fn get_sorted_matchers_for_event(
    hooks_by_event_and_matcher: &HashMap<HookEvent, HashMap<String, Vec<IndividualHookConfig>>>,
    event: HookEvent,
) -> Vec<String> {
    let matchers = hooks_by_event_and_matcher
        .get(&event)
        .map(|by_matcher| by_matcher.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    sort_matchers_by_priority(&matchers, hooks_by_event_and_matcher, event)
}

/// Maps to: CC `getHooksForMatcher(...)`.
pub fn get_hooks_for_matcher(
    hooks_by_event_and_matcher: &HashMap<HookEvent, HashMap<String, Vec<IndividualHookConfig>>>,
    event: HookEvent,
    matcher: Option<&str>,
) -> Vec<IndividualHookConfig> {
    let matcher_key = matcher.unwrap_or("");
    hooks_by_event_and_matcher
        .get(&event)
        .and_then(|by_matcher| by_matcher.get(matcher_key))
        .cloned()
        .unwrap_or_default()
}

/// Maps to: CC `getMatcherMetadata(...)`.
pub fn get_matcher_metadata(event: HookEvent, tool_names: &[String]) -> Option<MatcherMetadata> {
    get_hook_event_metadata(tool_names)
        .get(&event)
        .and_then(|metadata| metadata.matcher_metadata.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::hooks::HookCommand;
    use crate::utils::hooks::hooks_settings::{
        HookDisplayConfig, HookSource, IndividualHookConfig,
    };

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

    fn hook(event: HookEvent, matcher: Option<&str>, source: HookSource) -> IndividualHookConfig {
        IndividualHookConfig {
            event,
            config: HookDisplayConfig::from_command(command("echo hook")),
            matcher: matcher.map(str::to_string),
            source,
            plugin_name: None,
        }
    }

    #[test]
    fn metadata_contains_official_matcher_fields_and_values() {
        let tool_names = vec!["Bash".to_string(), "Read".to_string()];
        let metadata = get_hook_event_metadata(&tool_names);
        assert_eq!(metadata.len(), HOOK_EVENTS.len());
        let pre = metadata.get(&HookEvent::PreToolUse).expect("pre metadata");
        assert_eq!(pre.summary, "Before tool execution");
        assert_eq!(
            pre.matcher_metadata.as_ref().unwrap().field_to_match,
            "tool_name"
        );
        assert_eq!(pre.matcher_metadata.as_ref().unwrap().values, tool_names);
        let notification = metadata
            .get(&HookEvent::Notification)
            .expect("notification metadata");
        assert!(
            notification
                .matcher_metadata
                .as_ref()
                .unwrap()
                .values
                .contains(&"elicitation_response".to_string())
        );
    }

    #[test]
    fn grouping_uses_matcher_key_only_for_events_with_matcher_metadata() {
        let grouped = group_individual_hooks_by_event_and_matcher(
            vec![
                hook(
                    HookEvent::PreToolUse,
                    Some("Bash"),
                    HookSource::UserSettings,
                ),
                hook(HookEvent::Stop, Some("ignored"), HookSource::UserSettings),
            ],
            &["Bash".to_string()],
        );

        assert_eq!(
            get_hooks_for_matcher(&grouped, HookEvent::PreToolUse, Some("Bash")).len(),
            1
        );
        assert_eq!(
            get_hooks_for_matcher(&grouped, HookEvent::Stop, None).len(),
            1
        );
        assert!(get_hooks_for_matcher(&grouped, HookEvent::Stop, Some("ignored")).is_empty());
    }

    #[test]
    fn sorted_matchers_delegate_to_official_source_priority() {
        let grouped = group_individual_hooks_by_event_and_matcher(
            vec![
                hook(HookEvent::PreToolUse, Some("Read"), HookSource::PluginHook),
                hook(
                    HookEvent::PreToolUse,
                    Some("Bash"),
                    HookSource::UserSettings,
                ),
                hook(
                    HookEvent::PreToolUse,
                    Some("Write"),
                    HookSource::LocalSettings,
                ),
            ],
            &["Bash".to_string(), "Read".to_string(), "Write".to_string()],
        );

        assert_eq!(
            get_sorted_matchers_for_event(&grouped, HookEvent::PreToolUse),
            vec!["Write", "Bash", "Read"]
        );
    }

    #[test]
    fn matcher_metadata_accessor_matches_official_helper() {
        assert_eq!(
            get_matcher_metadata(HookEvent::SessionEnd, &[])
                .expect("session end matcher")
                .field_to_match,
            "reason"
        );
        assert!(get_matcher_metadata(HookEvent::Stop, &[]).is_none());
    }
}
