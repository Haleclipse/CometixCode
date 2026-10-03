//! Elicitation hook execution.
//! Maps to: CC utils/hooks.ts:4470-4580 (executeElicitationHooks, executeElicitationResultHooks).

use super::exec::exec_command_hook;
use super::matching::get_matching_hooks;
use super::parsing::{ParsedHookOutput, parse_hook_output, process_hook_json_output};
use super::{HookContext, HookEvent, HookResult, RegisteredHooks, create_base_hook_input};
use serde_json::{Map, Value};
use std::time::Duration;

const TOOL_HOOK_TIMEOUT_MS: u64 = 30_000;

fn base_object(ctx: &HookContext) -> Map<String, Value> {
    create_base_hook_input(ctx)
        .as_object()
        .cloned()
        .unwrap_or_default()
}

fn insert_optional_string(map: &mut Map<String, Value>, key: &str, value: Option<&str>) {
    if let Some(value) = value {
        map.insert(key.to_string(), Value::String(value.to_string()));
    }
}

/// Maps to: CC `executeElicitationHooks()` hookInput construction.
pub fn build_elicitation_hook_input(
    hook_context: &HookContext,
    server_name: &str,
    message: &str,
    requested_schema: Option<Value>,
    mode: Option<&str>,
    url: Option<&str>,
    elicitation_id: Option<&str>,
) -> Value {
    let mut input = base_object(hook_context);
    input.insert(
        "hook_event_name".to_string(),
        Value::String("Elicitation".to_string()),
    );
    input.insert(
        "mcp_server_name".to_string(),
        Value::String(server_name.to_string()),
    );
    input.insert("message".to_string(), Value::String(message.to_string()));
    insert_optional_string(&mut input, "mode", mode);
    insert_optional_string(&mut input, "url", url);
    insert_optional_string(&mut input, "elicitation_id", elicitation_id);
    if let Some(requested_schema) = requested_schema {
        input.insert("requested_schema".to_string(), requested_schema);
    }
    Value::Object(input)
}

/// Maps to: CC `executeElicitationResultHooks()` hookInput construction.
pub fn build_elicitation_result_hook_input(
    hook_context: &HookContext,
    server_name: &str,
    action: &str,
    content: Option<Value>,
    mode: Option<&str>,
    elicitation_id: Option<&str>,
) -> Value {
    let mut input = base_object(hook_context);
    input.insert(
        "hook_event_name".to_string(),
        Value::String("ElicitationResult".to_string()),
    );
    input.insert(
        "mcp_server_name".to_string(),
        Value::String(server_name.to_string()),
    );
    input.insert("action".to_string(), Value::String(action.to_string()));
    insert_optional_string(&mut input, "mode", mode);
    insert_optional_string(&mut input, "elicitation_id", elicitation_id);
    if let Some(content) = content {
        input.insert("content".to_string(), content);
    }
    Value::Object(input)
}

/// Maps to: CC `executeElicitationHooks()` (hooks.ts:4470-4524).
pub async fn execute_elicitation_hooks(
    config: &RegisteredHooks,
    hook_context: &HookContext,
    server_name: &str,
    message: &str,
    requested_schema: Option<Value>,
    mode: Option<&str>,
    url: Option<&str>,
    elicitation_id: Option<&str>,
    base_env: Vec<(String, String)>,
) -> Vec<HookResult> {
    // Maps to: CC `hooks.ts:3016-3036`, reached through
    // `executeHooksOutsideREPL({hookInput, matchQuery: serverName, …})`
    // (`:4502-4507`).
    if crate::services::hooks::should_skip_hook_execution(HookEvent::Elicitation, server_name) {
        return Vec::new();
    }
    let matched = get_matching_hooks(config, HookEvent::Elicitation, server_name, None);
    if matched.is_empty() {
        return Vec::new();
    }

    let input_str = build_elicitation_hook_input(
        hook_context,
        server_name,
        message,
        requested_schema,
        mode,
        url,
        elicitation_id,
    )
    .to_string();

    let mut results = Vec::new();
    for hook in &matched {
        // CC hooks.ts:2147 — callback hooks resolve by calling back into the
        // SDK consumer; command hooks spawn (executeHookCallback vs command).
        let result = match &hook.hook {
            crate::schemas::hooks::RegisteredHook::Callback(callback) => {
                let json = super::exec::exec_callback_hook(callback, &input_str, None).await;
                process_hook_json_output(&json, "callback")
            }
            crate::schemas::hooks::RegisteredHook::Command(command) => {
                let timeout = command.timeout.unwrap_or(TOOL_HOOK_TIMEOUT_MS / 1000) * 1000;
                let exec_result = exec_command_hook(
                    &command.command,
                    &input_str,
                    Duration::from_millis(timeout),
                    base_env.clone(),
                    hook.plugin_root.as_deref(),
                    hook.plugin_id.as_deref(),
                    None,
                )
                .await;
                match parse_hook_output(&exec_result.stdout) {
                    ParsedHookOutput::Json(json) => {
                        process_hook_json_output(&json, &command.command)
                    }
                    _ => HookResult::default(),
                }
            }
        };
        results.push(result);
    }
    results
}

/// Maps to: CC `executeElicitationResultHooks()` (hooks.ts:4525-4580).
pub async fn execute_elicitation_result_hooks(
    config: &RegisteredHooks,
    hook_context: &HookContext,
    server_name: &str,
    action: &str,
    content: Option<Value>,
    mode: Option<&str>,
    elicitation_id: Option<&str>,
    base_env: Vec<(String, String)>,
) -> Vec<HookResult> {
    // Maps to: CC `hooks.ts:3016-3036`, reached through
    // `executeHooksOutsideREPL({hookInput, matchQuery: serverName, …})`
    // (`:4554-4559`).
    if crate::services::hooks::should_skip_hook_execution(HookEvent::ElicitationResult, server_name)
    {
        return Vec::new();
    }
    let matched = get_matching_hooks(config, HookEvent::ElicitationResult, server_name, None);
    if matched.is_empty() {
        return Vec::new();
    }

    let input_str = build_elicitation_result_hook_input(
        hook_context,
        server_name,
        action,
        content,
        mode,
        elicitation_id,
    )
    .to_string();

    let mut results = Vec::new();
    for hook in &matched {
        // CC hooks.ts:2147 — callback hooks resolve by calling back into the
        // SDK consumer; command hooks spawn (executeHookCallback vs command).
        let result = match &hook.hook {
            crate::schemas::hooks::RegisteredHook::Callback(callback) => {
                let json = super::exec::exec_callback_hook(callback, &input_str, None).await;
                process_hook_json_output(&json, "callback")
            }
            crate::schemas::hooks::RegisteredHook::Command(command) => {
                let timeout = command.timeout.unwrap_or(TOOL_HOOK_TIMEOUT_MS / 1000) * 1000;
                let exec_result = exec_command_hook(
                    &command.command,
                    &input_str,
                    Duration::from_millis(timeout),
                    base_env.clone(),
                    hook.plugin_root.as_deref(),
                    hook.plugin_id.as_deref(),
                    None,
                )
                .await;
                match parse_hook_output(&exec_result.stdout) {
                    ParsedHookOutput::Json(json) => {
                        process_hook_json_output(&json, &command.command)
                    }
                    _ => HookResult::default(),
                }
            }
        };
        results.push(result);
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::hooks::{HookCommand, HookConfigEntry, HooksConfig};

    fn hook_context() -> HookContext {
        HookContext {
            session_id: "session-1".to_string(),
            transcript_path: "/tmp/transcript.jsonl".to_string(),
            cwd: "/repo".to_string(),
            permission_mode: Some("default".to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn build_elicitation_hook_input_matches_official_shape() {
        let input = build_elicitation_hook_input(
            &hook_context(),
            "docs",
            "Authorize docs",
            Some(serde_json::json!({"type":"object"})),
            Some("url"),
            Some("https://example.com/auth"),
            Some("elicit-1"),
        );
        assert_eq!(input["hook_event_name"], "Elicitation");
        assert_eq!(input["mcp_server_name"], "docs");
        assert_eq!(input["message"], "Authorize docs");
        assert_eq!(input["mode"], "url");
        assert_eq!(input["url"], "https://example.com/auth");
        assert_eq!(input["elicitation_id"], "elicit-1");
        assert_eq!(
            input["requested_schema"],
            serde_json::json!({"type":"object"})
        );
        assert_eq!(input["cwd"], "/repo");
    }

    #[test]
    fn build_elicitation_result_hook_input_matches_official_shape() {
        let input = build_elicitation_result_hook_input(
            &hook_context(),
            "docs",
            "accept",
            Some(serde_json::json!({"email":"user@example.com"})),
            Some("form"),
            None,
        );
        assert_eq!(input["hook_event_name"], "ElicitationResult");
        assert_eq!(input["mcp_server_name"], "docs");
        assert_eq!(input["action"], "accept");
        assert_eq!(input["mode"], "form");
        assert_eq!(
            input["content"],
            serde_json::json!({"email":"user@example.com"})
        );
        assert!(input.get("elicitation_id").is_none());
    }

    #[test]
    fn execute_elicitation_hooks_matches_server_name_like_official() {
        // The executor now opens with CC's trust gate, which would also return
        // an empty list — state trust so the emptiness below still means
        // "the `other` matcher did not match `docs`".
        let _trust = crate::services::hooks::test_support::SessionTrustGuard::accepted();
        let config: HooksConfig = std::collections::HashMap::from([(
            "Elicitation".to_string(),
            vec![HookConfigEntry {
                matcher: Some("other".to_string()),
                hooks: vec![HookCommand {
                    command: "sh -c 'exit 2'".to_string(),
                    shell: None,
                    timeout: Some(1),
                    condition: None,
                    status: None,
                    once: None,
                    is_async: None,
                    async_rewake: None,
                }],
                plugin_root: None,
                plugin_name: None,
                plugin_id: None,
            }],
        )]);
        let config = crate::services::hooks::test_support::registered_config(&config);
        let results = futures::executor::block_on(execute_elicitation_hooks(
            &config,
            &hook_context(),
            "docs",
            "Authorize docs",
            None,
            Some("url"),
            Some("https://example.com/auth"),
            Some("elicit-1"),
            Vec::new(),
        ));
        assert!(results.is_empty());
    }
}
