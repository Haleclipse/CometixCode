//! InstructionsLoaded audit hooks.
//! Maps to CC `utils/hooks.ts#executeInstructionsLoadedHooks`.

use super::{HookContext, RegisteredHooks};

pub const INSTRUCTIONS_LOADED_TIMEOUT_MS: u64 = 60_000;

#[derive(Clone, Debug)]
pub struct InstructionsLoadedInput {
    pub file_path: String,
    pub memory_type: &'static str,
    pub load_reason: &'static str,
    pub globs: Option<Vec<String>>,
    pub trigger_file_path: Option<String>,
    pub parent_file_path: Option<String>,
}

pub async fn execute_instructions_loaded_hooks_with_config(
    config: &RegisteredHooks,
    input: InstructionsLoadedInput,
    context: HookContext,
) {
    let mut hook_input = super::create_base_hook_input(&context);
    let Some(object) = hook_input.as_object_mut() else {
        return;
    };
    object.insert(
        "hook_event_name".to_string(),
        serde_json::Value::String("InstructionsLoaded".to_string()),
    );
    object.insert(
        "file_path".to_string(),
        serde_json::Value::String(input.file_path),
    );
    object.insert(
        "memory_type".to_string(),
        serde_json::Value::String(input.memory_type.to_string()),
    );
    object.insert(
        "load_reason".to_string(),
        serde_json::Value::String(input.load_reason.to_string()),
    );
    if let Some(globs) = input.globs {
        object.insert("globs".to_string(), serde_json::json!(globs));
    }
    if let Some(path) = input.trigger_file_path {
        object.insert(
            "trigger_file_path".to_string(),
            serde_json::Value::String(path),
        );
    }
    if let Some(path) = input.parent_file_path {
        object.insert(
            "parent_file_path".to_string(),
            serde_json::Value::String(path),
        );
    }
    let _ = super::env::execute_hooks_outside_repl_with_config(
        config,
        hook_input,
        Some(input.load_reason),
        INSTRUCTIONS_LOADED_TIMEOUT_MS,
        Vec::new(),
    )
    .await;
}

/// Fire-and-forget production dispatcher. InstructionsLoaded is audit-only and
/// cannot block instruction injection.
///
/// Detach-point note: CC has NO dispatcher — each CALLER detaches itself with
/// `void executeInstructionsLoadedHooks(...)` (`utils/attachments.ts:1760`,
/// commented "fire-and-forget", and `utils/claudemd.ts:1060`); the hooks file
/// only exports the plain async fn (`utils/hooks.ts:4335`). This port
/// converges those caller-side voids into this one shared dispatcher (chosen:
/// one detach point instead of two call sites), so the process-lifetime spawn
/// rule lands HERE rather than in attachments.rs/claudemd.rs.
pub fn dispatch_instructions_loaded_hooks(input: InstructionsLoadedInput) {
    let loaded = super::load_hooks_config();
    if loaded.disable_all_hooks
        || loaded
            .config
            .get("InstructionsLoaded")
            .is_none_or(Vec::is_empty)
    {
        return;
    }
    let cwd = std::env::current_dir()
        .unwrap_or_default()
        .display()
        .to_string();
    let context = HookContext {
        session_id: crate::utils::process_env::var("CLAUDE_SESSION_ID").unwrap_or_default(),
        transcript_path: crate::utils::process_env::var("CLAUDE_TRANSCRIPT_PATH").unwrap_or_default(),
        cwd,
        ..HookContext::default()
    };
    let config = loaded.config;
    let future = async move {
        execute_instructions_loaded_hooks_with_config(&config, input, context).await;
    };
    // CC's caller-side `void` (see the fn doc) detaches onto Node's ONE
    // process loop, so a user-defined hook command always runs to completion.
    // A3 audit (PORTING.md § "Node-async → tokio"): attachments are built
    // INSIDE the query turn, so the old `try_current()` pinned the hook run to
    // the turn's private runtime and a slow hook was silently killed when the
    // turn resolved — the same shape as #150.
    if let Some(handle) = crate::utils::process_runtime::runtime_handle_for_detached_work() {
        handle.spawn(future);
    } else {
        std::thread::spawn(move || super::block_on_hook_future(future));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::TEST_ENV_LOCK;

    #[cfg(unix)]
    #[tokio::test]
    async fn instructions_loaded_input_carries_nested_provenance() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        struct TrustRestore(bool);
        impl Drop for TrustRestore {
            fn drop(&mut self) {
                crate::bootstrap::state::set_session_trust_accepted(self.0);
            }
        }
        let _trust = TrustRestore(crate::bootstrap::state::get_session_trust_accepted());
        crate::bootstrap::state::set_session_trust_accepted(true);
        let output = std::env::temp_dir().join(format!(
            "cometix-instructions-loaded-{}.json",
            uuid::Uuid::new_v4().simple()
        ));
        let command = format!("cat > '{}'", output.display());
        let config: crate::services::hooks::HooksConfig =
            serde_json::from_value(serde_json::json!({
                "InstructionsLoaded": [{
                    "matcher": "path_glob_match",
                    "hooks": [{"command": command, "timeout": 5}]
                }]
            }))
            .unwrap();
        let config = crate::services::hooks::test_support::registered_config(&config);
        execute_instructions_loaded_hooks_with_config(
            &config,
            InstructionsLoadedInput {
                file_path: "/repo/.claude/rules/rust.md".to_string(),
                memory_type: "Project",
                load_reason: "path_glob_match",
                globs: Some(vec!["src/**/*.rs".to_string()]),
                trigger_file_path: Some("/repo/src/main.rs".to_string()),
                parent_file_path: None,
            },
            HookContext::default(),
        )
        .await;
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&output).unwrap()).unwrap();
        assert_eq!(value["hook_event_name"], "InstructionsLoaded");
        assert_eq!(value["load_reason"], "path_glob_match");
        assert_eq!(value["globs"][0], "src/**/*.rs");
        assert_eq!(value["trigger_file_path"], "/repo/src/main.rs");
        let _ = std::fs::remove_file(output);
    }
}
