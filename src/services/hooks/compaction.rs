//! Pre/Post compact hook execution.
//! Maps to: CC utils/hooks.ts:3961-4095.

use super::exec::exec_command_hook;
use super::matching::{MatchedHook, get_matching_hooks};
use super::{HookContext, HookEvent, RegisteredHooks, create_base_hook_input};
use std::time::Duration;

const TOOL_HOOK_TIMEOUT_MS: u64 = 30_000;

async fn execute_compact_hooks_in_parallel(
    matched: &[MatchedHook],
    input: &str,
    base_env: &[(String, String)],
    abort_controller: &crate::tool::AbortController,
) -> Vec<(String, super::CommandExecResult)> {
    futures::future::join_all(matched.iter().map(|hook| {
        async move {
            // CC hooks.ts:2147 — callback hooks resolve via the SDK consumer.
            match &hook.hook {
                crate::schemas::hooks::RegisteredHook::Callback(callback) => {
                    let json = super::exec::exec_callback_hook(callback, input, None).await;
                    // CC hooks.ts:3122-3133 — a resolved callback succeeds and
                    // surfaces systemMessage as its output; no process exit.
                    (
                        "callback".to_string(),
                        super::CommandExecResult {
                            stdout: json.system_message.clone().unwrap_or_default(),
                            stderr: String::new(),
                            status: 0,
                            aborted: false,
                        },
                    )
                }
                crate::schemas::hooks::RegisteredHook::Command(command) => {
                    let timeout = command.timeout.unwrap_or(TOOL_HOOK_TIMEOUT_MS / 1000) * 1000;
                    let command = command.command.clone();
                    let result = exec_command_hook(
                        &command,
                        input,
                        Duration::from_millis(timeout),
                        base_env.to_vec(),
                        hook.plugin_root.as_deref(),
                        hook.plugin_id.as_deref(),
                        Some(abort_controller),
                    )
                    .await;
                    (command, result)
                }
            }
        }
    }))
    .await
}

/// Maps to: CC `executePreCompactHooks()` (hooks.ts:3961-4033).
/// Returns optional new custom instructions and user display message.
pub async fn execute_pre_compact_hooks(
    config: &RegisteredHooks,
    trigger: &str,
    custom_instructions: Option<&str>,
    hook_context: &HookContext,
    abort_controller: &crate::tool::AbortController,
) -> PreCompactResult {
    // Maps to: CC `hooks.ts:3016-3036` reached through
    // `executeHooksOutsideREPL({hookInput, matchQuery: compactData.trigger})`
    // (`:3979-3983`). This module used to carry a private copy of that whole
    // block; the managed/trust pair now comes from the one entry every executor
    // funnels through, and `--bare` stays a separate call for the reason
    // `bare_mode_disables_hooks` documents.
    if crate::services::hooks::bare_mode_disables_hooks()
        || crate::services::hooks::should_skip_hook_execution(HookEvent::PreCompact, trigger)
    {
        return PreCompactResult::default();
    }
    let matched = get_matching_hooks(config, HookEvent::PreCompact, trigger, None);
    if matched.is_empty() {
        return PreCompactResult::default();
    }

    let mut hook_input = create_base_hook_input(hook_context);
    if let Some(object) = hook_input.as_object_mut() {
        object.insert(
            "hook_event_name".to_string(),
            serde_json::json!("PreCompact"),
        );
        object.insert("trigger".to_string(), serde_json::json!(trigger));
        object.insert(
            "custom_instructions".to_string(),
            serde_json::json!(custom_instructions),
        );
    }
    let input_str = hook_input.to_string();
    let base_env = Vec::new();

    let mut successful_outputs = Vec::new();
    let mut display_messages = Vec::new();
    for (command, result) in
        execute_compact_hooks_in_parallel(&matched, &input_str, &base_env, abort_controller).await
    {
        let succeeded = result.status == 0 && !result.aborted;
        let output = format!("{}{}", result.stdout, result.stderr)
            .trim()
            .to_string();
        if succeeded && !output.is_empty() {
            successful_outputs.push(output.clone());
        }
        let status = if succeeded {
            "completed successfully"
        } else {
            "failed"
        };
        if output.is_empty() {
            display_messages.push(format!("PreCompact [{command}] {status}"));
        } else {
            display_messages.push(format!("PreCompact [{command}] {status}: {output}"));
        }
    }
    PreCompactResult {
        new_custom_instructions: (!successful_outputs.is_empty())
            .then(|| successful_outputs.join("\n\n")),
        user_display_message: (!display_messages.is_empty()).then(|| display_messages.join("\n")),
    }
}

/// Maps to: CC `executePostCompactHooks()` (hooks.ts:4034-4095).
pub async fn execute_post_compact_hooks(
    config: &RegisteredHooks,
    trigger: &str,
    summary: &str,
    hook_context: &HookContext,
    abort_controller: &crate::tool::AbortController,
) -> PostCompactResult {
    // Maps to: CC `hooks.ts:3016-3036` reached through
    // `executeHooksOutsideREPL({hookInput, matchQuery: compactData.trigger})`
    // (`:4052-4056`).
    if crate::services::hooks::bare_mode_disables_hooks()
        || crate::services::hooks::should_skip_hook_execution(HookEvent::PostCompact, trigger)
    {
        return PostCompactResult::default();
    }
    let matched = get_matching_hooks(config, HookEvent::PostCompact, trigger, None);
    if matched.is_empty() {
        return PostCompactResult::default();
    }

    let mut hook_input = create_base_hook_input(hook_context);
    if let Some(object) = hook_input.as_object_mut() {
        object.insert(
            "hook_event_name".to_string(),
            serde_json::json!("PostCompact"),
        );
        object.insert("trigger".to_string(), serde_json::json!(trigger));
        object.insert("compact_summary".to_string(), serde_json::json!(summary));
    }
    let input_str = hook_input.to_string();
    let base_env = Vec::new();
    let mut display_messages = Vec::new();

    for (command, result) in
        execute_compact_hooks_in_parallel(&matched, &input_str, &base_env, abort_controller).await
    {
        let succeeded = result.status == 0 && !result.aborted;
        let output = format!("{}{}", result.stdout, result.stderr)
            .trim()
            .to_string();
        let status = if succeeded {
            "completed successfully"
        } else {
            "failed"
        };
        if output.is_empty() {
            display_messages.push(format!("PostCompact [{command}] {status}"));
        } else {
            display_messages.push(format!("PostCompact [{command}] {status}: {output}"));
        }
    }

    PostCompactResult {
        user_display_message: (!display_messages.is_empty()).then(|| display_messages.join("\n")),
    }
}

#[derive(Debug, Clone, Default)]
pub struct PostCompactResult {
    pub user_display_message: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct PreCompactResult {
    pub new_custom_instructions: Option<String>,
    pub user_display_message: Option<String>,
}
