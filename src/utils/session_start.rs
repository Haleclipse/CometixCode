//! Session-start / setup hook orchestration.
//!
//! Maps to: CC `utils/sessionStart.ts`.

use std::sync::Mutex;

use crate::services::hooks::HookResult;
use crate::types::message::{HookResultMessage, Message};
use crate::types::message::{RenderableMessage, SystemMessageLevel};

static PENDING_INITIAL_USER_MESSAGE: Mutex<Option<String>> = Mutex::new(None);

/// Maps to: CC `takeInitialUserMessage()`.
pub fn take_initial_user_message() -> Option<String> {
    PENDING_INITIAL_USER_MESSAGE
        .lock()
        .ok()
        .and_then(|mut slot| slot.take())
}

fn set_pending_initial_user_message(message: Option<String>) {
    if let Ok(mut slot) = PENDING_INITIAL_USER_MESSAGE.lock() {
        *slot = message;
    }
}

fn hook_result_messages(results: &[HookResult]) -> Vec<HookResultMessage> {
    let mut messages = Vec::new();
    let mut additional_contexts = Vec::new();
    let mut watch_paths = Vec::new();

    for result in results {
        if let Some(content) = result
            .system_message
            .as_ref()
            .filter(|content| !content.trim().is_empty())
        {
            messages.push(HookResultMessage::attachment(serde_json::json!({
                "type": "hook_system_message",
                "content": content,
                "hookName": "SessionStart",
                "toolUseID": "SessionStart",
                "hookEvent": "SessionStart"
            })));
        }
        if let Some(context) = result
            .additional_context
            .as_ref()
            .filter(|context| !context.trim().is_empty())
        {
            additional_contexts.push(context.clone());
        }
        if let Some(initial_user_message) = result.initial_user_message.clone() {
            set_pending_initial_user_message(Some(initial_user_message));
        }
        watch_paths.extend(result.watch_paths.clone().unwrap_or_default());
    }

    if !watch_paths.is_empty() {
        crate::utils::hooks::file_changed_watcher::update_watch_paths(watch_paths);
    }
    if !additional_contexts.is_empty() {
        messages.push(HookResultMessage::attachment(serde_json::json!({
            "type": "hook_additional_context",
            "content": additional_contexts,
            "hookName": "SessionStart",
            "toolUseID": "SessionStart",
            "hookEvent": "SessionStart"
        })));
    }
    messages
}

/// Documented L1 projection of CC's shared `HookResultMessage[]` into the two
/// strong Rust representations used by query history and retained rendering.
pub fn project_hook_result_messages(
    hook_messages: &[HookResultMessage],
) -> (Vec<Message>, Vec<RenderableMessage>) {
    let model_messages = hook_messages
        .iter()
        .cloned()
        .map(Message::HookResult)
        .collect();
    let renderable_messages = hook_messages.iter().filter_map(|message| {
        let attachment_type = message
            .attachment
            .get("type")
            .and_then(serde_json::Value::as_str)?;
        let text = match attachment_type {
            "hook_system_message" => message
                .attachment
                .get("content")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
            "hook_additional_context" => {
                let contexts = message
                    .attachment
                    .get("content")
                    .and_then(serde_json::Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(serde_json::Value::as_str)
                    .collect::<Vec<_>>();
                format!("[SessionStart additional context]\n{}", contexts.join("\n"))
            }
            _ => return None,
        };
        (!text.is_empty()).then(|| {
            RenderableMessage::system_notice(message.uuid.clone(), text, SystemMessageLevel::Info)
        })
    });
    (model_messages, renderable_messages.collect())
}

/// Async owner for CC `processSessionStartHooks(source, …)` used from query
/// and compact tasks. This avoids nesting `block_on` inside Tokio workers.
pub async fn process_session_start_hooks_async(
    source: &str,
    session_id: Option<&str>,
    agent_type: Option<&str>,
    model: Option<&str>,
) -> Vec<HookResultMessage> {
    if crate::utils::env_utils::is_bare_mode() {
        return Vec::new();
    }
    let loaded =
        crate::utils::hooks::hooks_config_snapshot::load_hooks_config_from_settings_sources();
    if !loaded.allow_managed_hooks_only && !loaded.disable_all_hooks {
        if let Err(error) = crate::utils::plugins::load_plugin_hooks::load_plugin_hooks() {
            crate::utils::debug::log_for_debugging(&format!(
                "Warning: Failed to load plugin hooks during {source}: {error}"
            ));
        }
    }
    // Re-read after `load_plugin_hooks`: the settings snapshot is immutable,
    // while registered plugin matchers were atomically installed above.
    let hooks_config = crate::services::hooks::load_hooks_config().config;
    let results = crate::services::hooks::lifecycle::execute_session_start_hooks(
        &hooks_config,
        source,
        session_id,
        agent_type,
        model,
        Vec::new(),
        // CC `processSessionStartHooks` passes `undefined` for the signal
        // (`utils/sessionStart.ts:132-140`).
        None,
    )
    .await;
    hook_result_messages(&results)
}

/// Synchronous startup adapter for call sites that run before the Tokio query
/// runtime. Async consumers must call [`process_session_start_hooks_async`].
pub fn process_session_start_hooks(
    source: &str,
    session_id: Option<&str>,
    agent_type: Option<&str>,
    model: Option<&str>,
) -> Vec<HookResultMessage> {
    crate::services::hooks::block_on_hook_future(process_session_start_hooks_async(
        source, session_id, agent_type, model,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::hooks::{HookOutcome, HookResult};
    use crate::types::message::{RenderableMessageKind, SystemMessage};
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[test]
    fn process_session_start_hook_results_collects_messages_and_context() {
        set_pending_initial_user_message(None);
        let results = vec![
            HookResult {
                outcome: HookOutcome::Success,
                system_message: Some("hello from hook".into()),
                additional_context: Some("ctx-a".into()),
                initial_user_message: Some("auto prompt".into()),
                ..Default::default()
            },
            HookResult {
                outcome: HookOutcome::Success,
                additional_context: Some("ctx-b".into()),
                ..Default::default()
            },
        ];
        let hook_messages = hook_result_messages(&results);
        assert_eq!(hook_messages.len(), 2);
        assert_eq!(
            hook_messages[0]
                .attachment
                .get("type")
                .and_then(serde_json::Value::as_str),
            Some("hook_system_message")
        );
        assert_eq!(
            hook_messages[1]
                .attachment
                .get("type")
                .and_then(serde_json::Value::as_str),
            Some("hook_additional_context")
        );
        let (_, renderable_messages) = project_hook_result_messages(&hook_messages);
        assert!(matches!(
            &renderable_messages[0].kind,
            RenderableMessageKind::System(SystemMessage::Informational { content: text, .. })
                if text == "hello from hook"
        ));
        assert!(matches!(
            &renderable_messages[1].kind,
            RenderableMessageKind::System(SystemMessage::Informational { content: text, .. })
                if text.contains("ctx-a") && text.contains("ctx-b")
        ));
        assert_eq!(take_initial_user_message().as_deref(), Some("auto prompt"));
        assert!(take_initial_user_message().is_none());
    }

    #[test]
    fn session_start_loads_inline_plugin_hooks_before_resume_execution() {
        // Mirror the process runtime published by the production entrypoint.
        crate::utils::process_runtime::initialize_test_process_runtime();
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-plugin-session-start-{}",
            uuid::Uuid::new_v4()
        ));
        let plugin = root.join("formatter");
        std::fs::create_dir_all(plugin.join(".claude-plugin")).unwrap();
        std::fs::create_dir_all(plugin.join("hooks")).unwrap();
        std::fs::write(
            plugin.join(".claude-plugin/plugin.json"),
            serde_json::json!({"name": "formatter"}).to_string(),
        )
        .unwrap();
        std::fs::write(
            plugin.join("hooks/hooks.json"),
            serde_json::json!({
                "description": "test hooks",
                "hooks": {
                    "SessionStart": [{
                        "matcher": "resume",
                        "hooks": [{
                            "type": "command",
                            "command": "echo plugin-session-start",
                            "timeout": 5
                        }]
                    }]
                }
            })
            .to_string(),
        )
        .unwrap();

        let _env = [
            EnvVarGuard::set("CLAUDE_CONFIG_DIR", root.join("config")),
            EnvVarGuard::unset("CLAUDE_CODE_SIMPLE"),
        ];
        let previous_inline = crate::bootstrap::state::get_inline_plugins();
        let previous_registered_hooks = crate::bootstrap::state::get_registered_hooks();
        // Relocating CLAUDE_CONFIG_DIR also relocates `~/.claude.json`
        // (`utils/config.rs:119-120`), so the scratch root above carries no
        // `hasTrustDialogAccepted` for any path — an UNTRUSTED workspace. CC
        // skips every hook in that state (`utils/hooks.ts:1994-1999` over
        // `computeTrustDialogAccepted`'s false default, `utils/config.ts:705`),
        // which this port now honours in `should_skip_hook_execution`. This test
        // is about plugin hooks being LOADED before resume, not about trust, so
        // it states trust instead of inheriting it from the scratch root.
        let _trust = crate::services::hooks::test_support::SessionTrustGuard::accepted();
        crate::bootstrap::state::set_inline_plugins(vec![plugin]);
        crate::utils::plugins::load_plugin_hooks::clear_plugin_hook_cache();
        crate::utils::hooks::hooks_config_snapshot::reset_hooks_config_snapshot();
        crate::utils::hooks::hooks_config_snapshot::capture_hooks_config_snapshot(
            &crate::utils::settings::SettingsJson::default(),
            None,
            false,
        );

        let messages =
            process_session_start_hooks("resume", Some("plugin-resume-session"), None, None);
        assert!(messages.iter().any(|message| {
            message
                .attachment
                .get("content")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|content| content.contains("plugin-session-start"))
        }));

        crate::bootstrap::state::set_inline_plugins(previous_inline);
        crate::utils::plugins::load_plugin_hooks::clear_plugin_hook_cache();
        match previous_registered_hooks {
            Some(config) => crate::bootstrap::state::replace_registered_hooks(config),
            None => crate::bootstrap::state::clear_registered_hooks(),
        }
        crate::utils::hooks::hooks_config_snapshot::reset_hooks_config_snapshot();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn resume_hook_messages_seed_model_and_transcript_projections_identically() {
        let hook_messages = hook_result_messages(&[
            HookResult {
                system_message: Some("resume system".to_string()),
                ..HookResult::default()
            },
            HookResult {
                additional_context: Some("resume context".to_string()),
                ..HookResult::default()
            },
        ]);
        let (model_messages, renderable_messages) = project_hook_result_messages(&hook_messages);

        assert_eq!(model_messages.len(), 2);
        assert_eq!(renderable_messages.len(), 2);
        assert!(matches!(
            &model_messages[0],
            Message::HookResult(message)
                if message.attachment.get("content").and_then(serde_json::Value::as_str)
                    == Some("resume system")
        ));
        assert!(matches!(
            &model_messages[1],
            Message::HookResult(message)
                if message
                    .attachment
                    .get("content")
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|contexts| contexts.iter().any(|context| {
                        context.as_str() == Some("resume context")
                    }))
        ));
    }
}
