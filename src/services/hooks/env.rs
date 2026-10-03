//! Environment-related hook execution.
//! Maps to: CC `utils/hooks.ts` outside-REPL env hook helpers:
//! `HookOutsideReplResult`, `hasBlockingResult`, `executeEnvHooks`,
//! `executeCwdChangedHooks`, and `executeFileChangedHooks`.
//!
//! These hooks run outside the REPL transcript path and return summaries to the
//! caller instead of injecting model-visible messages. The shared full
//! `executeHooks` orchestrator is still being ported, so this module preserves
//! the official outside-REPL boundary for command hooks and leaves prompt/agent,
//! callback/function, and HTTP-hook dispatch to the broader orchestrator slice.

use super::exec::exec_command_hook;
use super::load_hooks_config;
use super::matching::get_matching_hooks;
use super::parsing::{ParsedHookOutput, parse_hook_output, process_hook_json_output};
use super::{HookContext, HookEvent, RegisteredHooks, create_base_hook_input};
use serde_json::{Map, Value};
use std::path::Path;
use std::time::Duration;

/// Maps to: CC `TOOL_HOOK_EXECUTION_TIMEOUT_MS` for outside-REPL hooks.
pub const TOOL_HOOK_EXECUTION_TIMEOUT_MS: u64 = 10 * 60 * 1_000;

/// Maps to: CC `HookOutsideReplResult`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HookOutsideReplResult {
    pub command: String,
    pub succeeded: bool,
    pub output: String,
    pub blocked: bool,
    pub watch_paths: Option<Vec<String>>,
    pub system_message: Option<String>,
}

/// Maps to the return shape of CC `executeEnvHooks(...)`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EnvHookExecutionResult {
    pub results: Vec<HookOutsideReplResult>,
    pub watch_paths: Vec<String>,
    pub system_messages: Vec<String>,
}

/// Maps to: CC `hasBlockingResult(results)`.
pub fn has_blocking_result(results: &[HookOutsideReplResult]) -> bool {
    results.iter().any(|result| result.blocked)
}

fn hook_event_from_input(hook_input: &Value) -> Option<HookEvent> {
    let event_name = hook_input.get("hook_event_name")?.as_str()?;
    super::HOOK_EVENTS
        .iter()
        .copied()
        .find(|event| event.as_str() == event_name)
}

fn merge_event_fields(mut base_input: Value, event_fields: &[(&str, Value)]) -> Value {
    let mut object = base_input.as_object_mut().cloned().unwrap_or_default();
    for (key, value) in event_fields {
        object.insert((*key).to_string(), value.clone());
    }
    Value::Object(object)
}

fn match_query_from_hook_input(event: HookEvent, hook_input: &Value) -> Option<String> {
    match event {
        HookEvent::FileChanged => hook_input
            .get("file_path")
            .and_then(|value| value.as_str())
            .and_then(|path| Path::new(path).file_name().and_then(|name| name.to_str()))
            .map(ToString::to_string),
        HookEvent::ConfigChange => hook_input
            .get("source")
            .and_then(|value| value.as_str())
            .map(ToString::to_string),
        HookEvent::InstructionsLoaded => hook_input
            .get("load_reason")
            .and_then(|value| value.as_str())
            .map(ToString::to_string),
        _ => None,
    }
}

fn outside_result_from_exec(
    command: &str,
    exec_result: &super::CommandExecResult,
) -> HookOutsideReplResult {
    if exec_result.aborted {
        return HookOutsideReplResult {
            command: command.to_string(),
            succeeded: false,
            output: "Hook cancelled".to_string(),
            blocked: false,
            ..Default::default()
        };
    }

    match parse_hook_output(&exec_result.stdout) {
        ParsedHookOutput::ValidationError { error, .. } => HookOutsideReplResult {
            command: command.to_string(),
            succeeded: false,
            output: error,
            blocked: false,
            ..Default::default()
        },
        ParsedHookOutput::Json(json) => {
            let processed = process_hook_json_output(&json, command);
            let json_blocked = json.decision.as_deref() == Some("block");
            HookOutsideReplResult {
                command: command.to_string(),
                succeeded: exec_result.status == 0,
                output: if exec_result.status == 0 {
                    exec_result.stdout.clone()
                } else {
                    exec_result.stderr.clone()
                },
                blocked: exec_result.status == 2 || json_blocked,
                watch_paths: processed.watch_paths,
                system_message: processed.system_message,
            }
        }
        ParsedHookOutput::Empty | ParsedHookOutput::PlainText(_) => HookOutsideReplResult {
            command: command.to_string(),
            succeeded: exec_result.status == 0,
            output: if exec_result.status == 0 {
                exec_result.stdout.clone()
            } else {
                exec_result.stderr.clone()
            },
            blocked: exec_result.status == 2,
            ..Default::default()
        },
    }
}

/// Maps to the command-hook branch of CC `executeHooksOutsideREPL(...)`.
pub async fn execute_hooks_outside_repl_with_config(
    config: &RegisteredHooks,
    hook_input: Value,
    match_query: Option<&str>,
    timeout_ms: u64,
    base_env: Vec<(String, String)>,
) -> Vec<HookOutsideReplResult> {
    let Some(event) = hook_event_from_input(&hook_input) else {
        return Vec::new();
    };
    // Maps to: CC `utils/hooks.ts:3016-3036` — the gate block
    // `executeHooksOutsideREPL` opens with, ahead of `getMatchingHooks`
    // (`:3041`). Shared with `executeHooks`' identical block through
    // [`crate::services::hooks::should_skip_hook_execution`].
    if crate::services::hooks::should_skip_hook_execution(event, match_query.unwrap_or_default()) {
        return Vec::new();
    }
    let derived_match_query = match_query
        .map(ToString::to_string)
        .or_else(|| match_query_from_hook_input(event, &hook_input));
    let matched = get_matching_hooks(
        config,
        event,
        derived_match_query.as_deref().unwrap_or_default(),
        None,
    );
    if matched.is_empty() {
        return Vec::new();
    }

    let json_input = hook_input.to_string();
    let mut results = Vec::with_capacity(matched.len());
    for (hook_index, matched_hook) in matched.iter().enumerate() {
        // CC hooks.ts:2147 — callback hooks resolve via the SDK consumer.
        let result = match &matched_hook.hook {
            crate::schemas::hooks::RegisteredHook::Callback(callback) => {
                let json = super::exec::exec_callback_hook(callback, &json_input, None).await;
                // CC hooks.ts:3122-3133 — callbacks succeed, surface
                // systemMessage as output, and block via decision === 'block';
                // they carry no process exit semantics.
                HookOutsideReplResult {
                    command: "callback".to_string(),
                    succeeded: true,
                    output: json.system_message.clone().unwrap_or_default(),
                    blocked: json.decision.as_deref() == Some("block"),
                    ..Default::default()
                }
            }
            crate::schemas::hooks::RegisteredHook::Command(command) => {
                let command_timeout_ms = command
                    .timeout
                    .map(|seconds| seconds * 1_000)
                    .unwrap_or(timeout_ms);
                let mut env = base_env.clone();
                // Maps to CC `execCommandHook` `CLAUDE_ENV_FILE` injection. PowerShell
                // hooks are routed through their separate shell owner; this path is the
                // Bash `.sh` environment contract.
                if matches!(
                    event,
                    HookEvent::Setup
                        | HookEvent::SessionStart
                        | HookEvent::CwdChanged
                        | HookEvent::FileChanged
                ) {
                    if let Ok(path) = crate::utils::session_environment::get_hook_env_file_path(
                        event.as_str(),
                        hook_index,
                    ) {
                        env.push(("CLAUDE_ENV_FILE".to_string(), path.display().to_string()));
                    }
                }
                let exec_result = exec_command_hook(
                    &command.command,
                    &json_input,
                    Duration::from_millis(command_timeout_ms),
                    env,
                    matched_hook.plugin_root.as_deref(),
                    matched_hook.plugin_id.as_deref(),
                    None,
                )
                .await;
                outside_result_from_exec(&command.command, &exec_result)
            }
        };
        results.push(result);
    }

    results
}

/// Maps to: CC `executeEnvHooks(...)`.
pub async fn execute_env_hooks_with_config(
    config: &RegisteredHooks,
    hook_input: Value,
    timeout_ms: u64,
    base_env: Vec<(String, String)>,
) -> EnvHookExecutionResult {
    let results =
        execute_hooks_outside_repl_with_config(config, hook_input, None, timeout_ms, base_env)
            .await;
    // Maps to CC `executeEnvHooks`: hook-written exports must be visible to the
    // next Bash command.
    if !results.is_empty() {
        crate::utils::session_environment::invalidate_session_env_cache();
    }
    let watch_paths = results
        .iter()
        .flat_map(|result| result.watch_paths.clone().unwrap_or_default())
        .collect::<Vec<_>>();
    let system_messages = results
        .iter()
        .filter_map(|result| result.system_message.clone())
        .collect::<Vec<_>>();

    EnvHookExecutionResult {
        results,
        watch_paths,
        system_messages,
    }
}

/// Maps to: CC `executeCwdChangedHooks(oldCwd, newCwd, timeoutMs)`.
pub async fn execute_cwd_changed_hooks_with_config(
    config: &RegisteredHooks,
    old_cwd: &str,
    new_cwd: &str,
    base_input: Value,
    base_env: Vec<(String, String)>,
    timeout_ms: u64,
) -> EnvHookExecutionResult {
    let hook_input = merge_event_fields(
        base_input,
        &[
            (
                "hook_event_name",
                Value::String(HookEvent::CwdChanged.as_str().to_string()),
            ),
            ("old_cwd", Value::String(old_cwd.to_string())),
            ("new_cwd", Value::String(new_cwd.to_string())),
        ],
    );
    execute_env_hooks_with_config(config, hook_input, timeout_ms, base_env).await
}

/// Maps to: CC `executeFileChangedHooks(filePath, event, timeoutMs)`.
pub async fn execute_file_changed_hooks_with_config(
    config: &RegisteredHooks,
    file_path: &str,
    event: &str,
    base_input: Value,
    base_env: Vec<(String, String)>,
    timeout_ms: u64,
) -> EnvHookExecutionResult {
    let hook_input = merge_event_fields(
        base_input,
        &[
            (
                "hook_event_name",
                Value::String(HookEvent::FileChanged.as_str().to_string()),
            ),
            ("file_path", Value::String(file_path.to_string())),
            ("event", Value::String(event.to_string())),
        ],
    );
    execute_env_hooks_with_config(config, hook_input, timeout_ms, base_env).await
}

/// Maps to: CC `utils/hooks.ts:4194-4199` `ConfigChangeSource` — the five-member
/// string union — and `entrypoints/sdk/coreSchemas.ts:662-668`
/// `CONFIG_CHANGE_SOURCES`, the runtime const `ConfigChangeHookInputSchema`
/// (`:670-678`) validates `source` against with `z.enum(...)`.
///
/// CC has FOUR `SettingSource`s feeding this and five sources here:
/// `settingSourceToConfigChangeSource` (`utils/settings/changeDetector.ts:252-266`)
/// folds `flagSettings` and `policySettings` both onto `policy_settings`, and
/// `skills` arrives from a different watcher entirely
/// (`utils/skills/skillChangeDetector.ts:267`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigChangeSource {
    UserSettings,
    ProjectSettings,
    LocalSettings,
    PolicySettings,
    Skills,
}

impl ConfigChangeSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UserSettings => "user_settings",
            Self::ProjectSettings => "project_settings",
            Self::LocalSettings => "local_settings",
            Self::PolicySettings => "policy_settings",
            Self::Skills => "skills",
        }
    }
}

/// Maps to: CC `utils/hooks.ts:4214-4239#executeConfigChangeHooks`.
///
/// "Fired by file watchers when settings, skills, or commands change on disk.
/// Enables enterprise admins to audit/log configuration changes for security"
/// (`:4202-4204`).
///
/// The `policy_settings` arm (`:4232-4236`) is the whole reason this cannot be
/// a plain `execute_hooks_outside_repl_with_config` call at the trigger site:
/// policy settings are enterprise-managed, so their hooks still FIRE for audit
/// logging but can never come back `blocked`. Dropping that clamp would let a
/// user-authored `ConfigChange` hook veto an admin policy update.
pub async fn execute_config_change_hooks_with_config(
    config: &RegisteredHooks,
    source: ConfigChangeSource,
    file_path: Option<&str>,
    base_input: Value,
    base_env: Vec<(String, String)>,
    timeout_ms: u64,
) -> Vec<HookOutsideReplResult> {
    let mut event_fields = vec![
        (
            "hook_event_name",
            Value::String(HookEvent::ConfigChange.as_str().to_string()),
        ),
        ("source", Value::String(source.as_str().to_string())),
    ];
    // `file_path` is `z.string().optional()` (`coreSchemas.ts:675`) and CC
    // writes `file_path: filePath` straight from an optional parameter, so
    // `JSON.stringify` drops the key entirely when the caller passed none.
    if let Some(file_path) = file_path {
        event_fields.push(("file_path", Value::String(file_path.to_string())));
    }
    let hook_input = merge_event_fields(base_input, &event_fields);

    let mut results = execute_hooks_outside_repl_with_config(
        config,
        hook_input,
        Some(source.as_str()),
        timeout_ms,
        base_env,
    )
    .await;

    // CC `:4232-4236`: `results.map(r => ({ ...r, blocked: false }))`.
    if source == ConfigChangeSource::PolicySettings {
        for result in &mut results {
            result.blocked = false;
        }
    }
    results
}

fn default_env_hook_context() -> HookContext {
    let cwd = std::env::current_dir()
        .unwrap_or_default()
        .display()
        .to_string();
    HookContext {
        cwd,
        ..Default::default()
    }
}

/// Production settings-backed wrapper for CC `executeCwdChangedHooks(...)`.
pub async fn execute_cwd_changed_hooks(old_cwd: &str, new_cwd: &str) -> EnvHookExecutionResult {
    let config = load_hooks_config().config;
    let context = default_env_hook_context();
    execute_cwd_changed_hooks_with_config(
        &config,
        old_cwd,
        new_cwd,
        create_base_hook_input(&context),
        Vec::new(),
        TOOL_HOOK_EXECUTION_TIMEOUT_MS,
    )
    .await
}

/// Production settings-backed wrapper for CC `executeFileChangedHooks(...)`.
pub async fn execute_file_changed_hooks(file_path: &str, event: &str) -> EnvHookExecutionResult {
    let config = load_hooks_config().config;
    let context = default_env_hook_context();
    execute_file_changed_hooks_with_config(
        &config,
        file_path,
        event,
        create_base_hook_input(&context),
        Vec::new(),
        TOOL_HOOK_EXECUTION_TIMEOUT_MS,
    )
    .await
}

/// Production settings-backed wrapper for CC `executeConfigChangeHooks(...)`,
/// whose `timeoutMs` defaults to `TOOL_HOOK_EXECUTION_TIMEOUT_MS`
/// (`hooks.ts:4217`, `:166` — 10 minutes).
///
/// Trigger seam: CC fires this from the settings watcher
/// (`utils/settings/changeDetector.ts:292-301` on change, `:344-353` after the
/// deletion grace timer) and from the skills watcher
/// (`utils/skills/skillChangeDetector.ts:267`), and in the settings case the
/// result GATES the change — `hasBlockingResult(results)` skips `fanOut(source)`
/// entirely. `utils/settings/change_detector.rs` (this port's counterpart)
/// documents that gating as a deliberate subset in its own header; wiring it is
/// a change_detector-side edit, not a hooks-side one.
pub async fn execute_config_change_hooks(
    source: ConfigChangeSource,
    file_path: Option<&str>,
) -> Vec<HookOutsideReplResult> {
    let config = load_hooks_config().config;
    let context = default_env_hook_context();
    execute_config_change_hooks_with_config(
        &config,
        source,
        file_path,
        create_base_hook_input(&context),
        Vec::new(),
        TOOL_HOOK_EXECUTION_TIMEOUT_MS,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(value: Value) -> RegisteredHooks {
        let config: crate::services::hooks::HooksConfig = serde_json::from_value(value).unwrap();
        crate::services::hooks::test_support::registered_config(&config)
    }

    use crate::services::hooks::test_support::SessionTrustGuard;

    #[test]
    fn has_blocking_result_matches_official_helper() {
        assert!(!has_blocking_result(&[HookOutsideReplResult {
            blocked: false,
            ..Default::default()
        }]));
        assert!(has_blocking_result(&[HookOutsideReplResult {
            blocked: true,
            ..Default::default()
        }]));
    }

    #[tokio::test]
    async fn cwd_changed_hook_collects_watch_paths_and_system_messages() {
        let _trust_guard = SessionTrustGuard::accepted();
        let config = config(serde_json::json!({
            "CwdChanged": [{
                "hooks": [{
                    "command": "cat >/dev/null; printf '{\"systemMessage\":\"env updated\",\"hookSpecificOutput\":{\"hookEventName\":\"CwdChanged\",\"watchPaths\":[\".envrc\",\"/tmp/global.env\"]}}'",
                    "timeout": 5
                }]
            }]
        }));

        let result = execute_cwd_changed_hooks_with_config(
            &config,
            "/old",
            "/new",
            Value::Object(Map::new()),
            vec![],
            5_000,
        )
        .await;

        assert_eq!(result.results.len(), 1);
        assert!(result.results[0].succeeded);
        assert_eq!(result.watch_paths, vec![".envrc", "/tmp/global.env"]);
        assert_eq!(result.system_messages, vec!["env updated"]);
    }

    #[tokio::test]
    async fn file_changed_hook_uses_file_event_matcher_and_blocks_on_exit_two() {
        let _trust_guard = SessionTrustGuard::accepted();
        let config = config(serde_json::json!({
            "FileChanged": [
                {"matcher": ".envrc|.env", "hooks": [{"command": "sh -c 'echo blocked >&2; exit 2'", "timeout": 5}]},
                {"matcher": "other", "hooks": [{"command": "echo should-not-run", "timeout": 5}]}
            ]
        }));
        let hook_input = serde_json::json!({
            "hook_event_name": "FileChanged",
            "file_path": "/repo/.envrc",
            "event": "change"
        });

        let results = execute_hooks_outside_repl_with_config(
            &config,
            hook_input,
            Some(".envrc"),
            5_000,
            vec![],
        )
        .await;

        assert_eq!(results.len(), 1);
        assert!(!results[0].succeeded);
        assert!(results[0].blocked);
        assert!(results[0].output.contains("blocked"));
    }

    #[tokio::test]
    async fn file_changed_wrapper_returns_empty_when_no_hooks_match() {
        let _trust_guard = SessionTrustGuard::accepted();
        let config = config(serde_json::json!({
            "FileChanged": [{"matcher": ".env", "hooks": [{"command": "echo nope", "timeout": 5}]}]
        }));

        let result = execute_file_changed_hooks_with_config(
            &config,
            "/repo/.envrc",
            "change",
            Value::Object(Map::new()),
            vec![],
            5_000,
        )
        .await;

        // The official wrapper executes FileChanged hooks through
        // executeEnvHooks without a matchQuery; matcher filtering happens in
        // fileChangedWatcher path selection, not here.
        assert_eq!(result.results.len(), 1);
    }

    #[tokio::test]
    async fn validation_error_is_reported_as_non_blocking_failure() {
        let _trust_guard = SessionTrustGuard::accepted();
        // Syntactically VALID JSON that fails the hook schema — `continue` is
        // `Option<bool>`. CC separates the two failure modes: `parseHookOutput`
        // (utils/hooks.ts:447-450) catches a `jsonParse` throw and returns bare
        // `{ plainText }` with no validationError, so malformed JSON is ordinary
        // output and keeps the non-JSON exit-code convention. Only schema
        // failure returns `{ plainText, validationError }` (:446), which is the
        // non-blocking failure this test is about.
        //
        // The old payload was `printf '{bad json'` — a syntax error, so it took
        // the plain-text path and `printf` exited 0, making `succeeded` true.
        // The test was asserting the wrong branch, not catching a defect.
        let config = config(serde_json::json!({
            "CwdChanged": [{"hooks": [
                {"command": "printf '{\"continue\": \"not-a-boolean\"}'", "timeout": 5}
            ]}]
        }));

        let result = execute_cwd_changed_hooks_with_config(
            &config,
            "/old",
            "/new",
            Value::Object(Map::new()),
            vec![],
            5_000,
        )
        .await;

        assert_eq!(result.results.len(), 1);
        assert!(!result.results[0].succeeded);
        assert!(!result.results[0].blocked);
        // CC's validateHookJson copy (the carrier replaced the old serde text).
        assert!(
            result.results[0]
                .output
                .contains("Hook JSON output validation failed")
        );
    }

    fn config_change_config(command: &str) -> RegisteredHooks {
        config(serde_json::json!({
            "ConfigChange": [{"hooks": [{"command": command, "timeout": 5}]}]
        }))
    }

    /// Maps to: CC `utils/hooks.ts:4219-4230#executeConfigChangeHooks` — the
    /// input is `createBaseHookInput(undefined)` plus `hook_event_name`,
    /// `source` and an optional `file_path`, and `matchQuery` is the `source`.
    ///
    /// Old shape: the port had `HookEvent::ConfigChange` in its event table and
    /// a `match_query_from_hook_input` arm reading `source`, but NOTHING built
    /// the input, so the event could never fire and this call did not compile.
    #[tokio::test]
    async fn config_change_hook_receives_the_official_input_and_matches_on_source() {
        let _trust_guard = SessionTrustGuard::accepted();
        let capture = std::env::temp_dir().join(format!(
            "cometix-config-change-hook-{}.json",
            uuid::Uuid::new_v4().simple()
        ));
        let results = execute_config_change_hooks_with_config(
            &config_change_config(r#"cat > "$COMETIX_TEST_CONFIG_CHANGE_CAPTURE""#),
            ConfigChangeSource::ProjectSettings,
            Some("/repo/.claude/settings.json"),
            create_base_hook_input(&HookContext::default()),
            vec![(
                "COMETIX_TEST_CONFIG_CHANGE_CAPTURE".to_string(),
                capture.display().to_string(),
            )],
            5_000,
        )
        .await;

        assert_eq!(results.len(), 1);
        let input: Value =
            serde_json::from_str(&std::fs::read_to_string(&capture).expect("the hook got stdin"))
                .expect("the hook input is JSON");
        let _ = std::fs::remove_file(&capture);
        assert_eq!(input["hook_event_name"], "ConfigChange");
        assert_eq!(input["source"], "project_settings");
        assert_eq!(input["file_path"], "/repo/.claude/settings.json");
    }

    /// `file_path` is `z.string().optional()` (`coreSchemas.ts:675`), and CC
    /// assigns it straight from an optional parameter, so `JSON.stringify` drops
    /// the key when the watcher had no path to report.
    #[tokio::test]
    async fn config_change_omits_file_path_when_the_caller_had_none() {
        let _trust_guard = SessionTrustGuard::accepted();
        let capture = std::env::temp_dir().join(format!(
            "cometix-config-change-nopath-{}.json",
            uuid::Uuid::new_v4().simple()
        ));
        execute_config_change_hooks_with_config(
            &config_change_config(r#"cat > "$COMETIX_TEST_CONFIG_CHANGE_CAPTURE""#),
            ConfigChangeSource::Skills,
            None,
            create_base_hook_input(&HookContext::default()),
            vec![(
                "COMETIX_TEST_CONFIG_CHANGE_CAPTURE".to_string(),
                capture.display().to_string(),
            )],
            5_000,
        )
        .await;

        let input: Value =
            serde_json::from_str(&std::fs::read_to_string(&capture).expect("the hook got stdin"))
                .expect("the hook input is JSON");
        let _ = std::fs::remove_file(&capture);
        assert_eq!(input["source"], "skills");
        assert!(
            input.get("file_path").is_none(),
            "no path means no key, not null: {input}"
        );
    }

    /// Maps to: CC `utils/hooks.ts:4232-4236` — "Policy settings are
    /// enterprise-managed — hooks fire for audit logging but must never block
    /// policy changes from being applied", i.e.
    /// `results.map(r => ({ ...r, blocked: false }))`.
    ///
    /// Without the clamp a user-authored `ConfigChange` hook exiting 2 would
    /// veto an admin policy update, because the settings watcher skips
    /// `fanOut(source)` on `hasBlockingResult(results)`
    /// (`utils/settings/changeDetector.ts:296-301`).
    #[tokio::test]
    async fn policy_settings_config_change_fires_for_audit_but_can_never_block() {
        let _trust_guard = SessionTrustGuard::accepted();
        let blocking = config_change_config("printf 'no you do not' >&2; exit 2");

        let policy = execute_config_change_hooks_with_config(
            &blocking,
            ConfigChangeSource::PolicySettings,
            Some("/etc/claude-code/managed-settings.json"),
            create_base_hook_input(&HookContext::default()),
            vec![],
            5_000,
        )
        .await;
        assert_eq!(policy.len(), 1, "the hook still FIRES, for audit logging");
        assert!(!policy[0].succeeded, "…and it still reports its failure");
        assert!(
            !has_blocking_result(&policy),
            "…but it can never block a policy change"
        );

        // Every other source keeps the ordinary exit-2 blocking convention.
        let user = execute_config_change_hooks_with_config(
            &blocking,
            ConfigChangeSource::UserSettings,
            Some("/home/dev/.claude/settings.json"),
            create_base_hook_input(&HookContext::default()),
            vec![],
            5_000,
        )
        .await;
        assert!(
            has_blocking_result(&user),
            "a non-policy source still blocks on exit 2"
        );
    }
}
