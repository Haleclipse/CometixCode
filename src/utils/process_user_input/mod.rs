//! User input normalization.
//! Maps to official `utils/processUserInput/processUserInput.ts`: turn submitted
//! input into transcript messages plus a `shouldQuery` decision. Slash commands
//! are routed through `process_slash_command`, mirroring upstream's split while
//! returning typed Rust actions for REPL-owned local command UI side effects.

pub mod process_bash_command;
pub mod process_slash_command;
pub mod process_text_prompt;

use self::process_slash_command::SlashCommandAction;
use crate::commands::Command;
use crate::constants::query_source::QuerySource;
use crate::types::message::RenderableMessage;
use std::sync::Arc;

/// Subset of official prompt input modes currently needed by CometixCode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProcessInputMode {
    Prompt,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProcessUserInputParams {
    pub input: String,
    pub uuid: Option<String>,
    pub mode: ProcessInputMode,
    /// Maps to CC `processUserInput`'s `preExpansionInput`. The queue keeps
    /// this raw text beside the expanded value so keyword-sensitive owners can
    /// inspect the same representation as the original implementation.
    pub pre_expansion_input: Option<String>,
    /// Maps to CC `QueuedCommand.skipSlashCommands`; bridge/remote input with
    /// this flag is model text even when it begins with `/`.
    pub skip_slash_commands: bool,
    /// Maps to CC `QueuedCommand.isMeta` / `processTextPrompt(..., isMeta)`.
    /// Meta prompts remain model-visible but are hidden from the normal
    /// transcript projection.
    pub is_meta: bool,
    /// Maps to CC `ToolUseContext.options.commands`, resolved once at launch.
    pub commands: Arc<Vec<Command>>,
    /// Maps to the `ToolUseContext` passed into command `call` /
    /// `getPromptForCommand` implementations.
    pub tool_use_context: crate::tool::ToolUseContext,
    /// Maps to CC `processUserInput`'s `imageContentBlocks` / `imagePasteIds`
    /// (`processTextPrompt.ts:66-88`). Both ride into the ONE user message the
    /// prompt path mints, so pasted images are blocks of the submitted prompt
    /// rather than a message of their own.
    pub image_content_blocks: Vec<crate::types::message::UserContent>,
    pub image_paste_ids: Vec<u32>,
}

/// Rust counterpart to official `ProcessUserInputBaseResult`.
#[derive(Clone, Debug, PartialEq)]
pub struct ProcessUserInputBaseResult {
    pub messages: Vec<RenderableMessage>,
    pub should_query: bool,
    /// Maps to CC `ProcessUserInputBaseResult.allowedTools`.
    /// Omission has the submit boundary's default-empty semantics; prompt
    /// commands set `Some`, including an explicitly empty list.
    pub allowed_tools: Option<Vec<String>>,
    pub local_action: Option<SlashCommandAction>,
    pub query_source: QuerySource,
}

/// Maps to: CC `processUserInput.ts:275` `MAX_HOOK_OUTPUT_LENGTH`.
const MAX_HOOK_OUTPUT_LENGTH: usize = 10_000;

/// Maps to: CC `processUserInput.ts:274-280` `applyTruncation`.
fn apply_truncation(content: &str) -> String {
    if content.chars().count() > MAX_HOOK_OUTPUT_LENGTH {
        // CC slices by UTF-16 code units; Rust slices by chars so the cut
        // never lands inside a codepoint. The limit is a guard, not a wire
        // format, so the one-unit difference on astral characters is inert.
        let head: String = content.chars().take(MAX_HOOK_OUTPUT_LENGTH).collect();
        format!("{head}… [output truncated - exceeded {MAX_HOOK_OUTPUT_LENGTH} characters]")
    } else {
        content.to_string()
    }
}

/// Folds `UserPromptSubmit` hook results into the submission.
///
/// Maps to: CC `processUserInput.ts:180-262` — the loop that runs after
/// `processUserInputBase` and only when `shouldQuery` is still true (`:171-173`
/// returns local-command results untouched).
///
/// The four outcomes, in CC's order:
/// 1. `blockingError` — REPLACE the messages with one system warning that
///    quotes the original prompt, and stop. The user's own row is erased:
///    the hook rejected the submission, so nothing should reach the model.
/// 2. `preventContinuation` — APPEND a plain user message and stop, keeping
///    the original prompt in context (CC's comment at `:211-212`).
/// 3. `additionalContext` — append one `hook_additional_context` attachment.
/// 4. a hook message — append it; `hook_success` gets truncated content and is
///    dropped when it has none.
///
/// Consumed by [`process_user_input`], which owns *when* this runs.
pub fn apply_user_prompt_submit_hook_results(
    result: &mut ProcessUserInputBaseResult,
    original_prompt: &str,
    hook_results: &[crate::services::hooks::HookResult],
) {
    for hook_result in hook_results {
        if let Some(blocking) = &hook_result.blocking_error {
            let message =
                crate::services::hooks::prompt::get_user_prompt_submit_hook_blocking_message(
                    blocking,
                );
            result.messages = vec![RenderableMessage::system_notice(
                uuid::Uuid::new_v4().to_string(),
                format!("{message}\n\nOriginal prompt: {original_prompt}"),
                crate::types::message::SystemMessageLevel::Warning,
            )];
            result.should_query = false;
            return;
        }

        if hook_result.prevent_continuation {
            let message = match &hook_result.stop_reason {
                Some(reason) => format!("Operation stopped by hook: {reason}"),
                None => "Operation stopped by hook".to_string(),
            };
            result.messages.push(RenderableMessage::user(
                uuid::Uuid::new_v4().to_string(),
                message,
            ));
            result.should_query = false;
            return;
        }

        if let Some(additional_context) = &hook_result.additional_context {
            result.messages.push(RenderableMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                kind: crate::types::message::RenderableMessageKind::Attachment(
                    crate::utils::attachments::Attachment::HookAdditionalContext {
                        // CC maps the whole `additionalContexts` array through
                        // `applyTruncation`; the Rust hook result aggregates to
                        // one string, so this is that array with one entry.
                        content: vec![apply_truncation(additional_context)],
                        hook_name: "UserPromptSubmit".to_string(),
                        tool_use_id: format!("hook-{}", uuid::Uuid::new_v4()),
                        hook_event: "UserPromptSubmit".to_string(),
                    },
                ),
            });
        }

        // CC's `hook_success` case skips an empty payload outright (`:249-252`)
        // and truncates the rest; `system_message` is the Rust carrier for the
        // hook's own textual output.
        if let Some(system_message) = &hook_result.system_message {
            if !system_message.is_empty() {
                result.messages.push(RenderableMessage::system_notice(
                    uuid::Uuid::new_v4().to_string(),
                    apply_truncation(system_message),
                    crate::types::message::SystemMessageLevel::Info,
                ));
            }
        }
    }
}

/// Maps to: CC `processUserInput.ts:141-268` `processUserInput(...)` — the
/// outer layer.
///
/// CC splits the submit pipeline in two. `processUserInputBase` dispatches the
/// input (slash / bash / text); `processUserInput` wraps it to await the
/// `UserPromptSubmit` hooks, which may erase the submission or stop the turn
/// before anything reaches the model. Only the inner half had been ported, and
/// it had taken the outer name — the giveaway being that its result type was
/// already `ProcessUserInputBaseResult`, which belongs to the inner half.
///
/// The hook loop runs only when `should_query` survived the dispatch: CC
/// returns local-command results untouched at `:171-173`.
pub async fn process_user_input(params: ProcessUserInputParams) -> ProcessUserInputBaseResult {
    // CC reads `getContentText(input) || ''` (`:180`) and quotes this same
    // original prompt back in the blocking message (`:198`), so it is the
    // submitted text, not anything the dispatch produced.
    let original_prompt = params.input.clone();
    // CC passes `appState.toolPermissionContext.mode` (`:186`); the Rust live
    // permission state rides the ToolUseContext the submit path constructs.
    let permission_mode = crate::utils::permissions::permission_mode::permission_mode_internal_name(
        params.tool_use_context.tool_permission_context.mode,
    );
    // CC `hooks.ts:3836`: `toolUseContext.agentId ?? getSessionId()`. Read
    // before the dispatch, which consumes `params`.
    let session_id = params
        .tool_use_context
        .agent_id
        .clone()
        .unwrap_or_else(crate::bootstrap::state::get_session_id);

    let result = process_user_input_base(params);
    continue_processed_user_input(result, &original_prompt, permission_mode, &session_id).await
}

/// Maps to: CC `processUserInput.ts:174-262`, after awaited local-JSX onDone.
/// The native UI action returns before its dialog completes, so that caller
/// resumes this SAME hook tail with the input/mode/session captured at dispatch.
/// PORTING.md async callback mapping; no second hook implementation.
pub(crate) async fn continue_processed_user_input(
    mut result: ProcessUserInputBaseResult,
    original_prompt: &str,
    permission_mode: &str,
    session_id: &str,
) -> ProcessUserInputBaseResult {
    if !result.should_query {
        return result;
    }

    // CC's `hasHookForEvent` (`hooks.ts:1582-1593`) considers three sources:
    // the settings snapshot, the registered plugin hooks, and the session's own
    // `appState.sessionHooks`. `load_hooks_config()` covers the first two; the
    // third has to be merged in, the way `tool_execution.rs:3468-3476` and
    // `run_agent.rs:1170` already do — including their `allow_managed_hooks_only`
    // gate.
    let loaded = crate::services::hooks::load_hooks_config();
    let mut hooks_config = loaded.config;
    if !loaded.allow_managed_hooks_only {
        crate::utils::hooks::session_hooks::merge_session_hooks_into_config(
            &mut hooks_config,
            session_id,
        );
    }
    let hook_results = crate::services::hooks::prompt::execute_user_prompt_submit_hooks(
        &hooks_config,
        original_prompt,
        permission_mode,
        Vec::new(),
    )
    .await;
    apply_user_prompt_submit_hook_results(&mut result, original_prompt, &hook_results);
    result
}

/// Maps to: CC `processUserInput.ts:269+` `processUserInputBase(...)` — the
/// dispatch half, with no hook awareness. Callers that submit on a user's
/// behalf want [`process_user_input`] instead.
pub fn process_user_input_base(params: ProcessUserInputParams) -> ProcessUserInputBaseResult {
    match params.mode {
        ProcessInputMode::Prompt
            if !params.skip_slash_commands && params.input.trim_start().starts_with('/') =>
        {
            process_slash_command::process_slash_command_with_context(
                &params.input,
                params.uuid,
                params.commands.as_slice(),
                &params.tool_use_context,
                params.image_content_blocks,
            )
        }
        ProcessInputMode::Prompt => process_text_prompt::process_text_prompt(
            params.input,
            params.uuid,
            // CC `processUserInput` reads `appState.toolPermissionContext.mode`
            // (`processUserInput.ts:152,165`) and threads it into
            // `processTextPrompt`; the Rust live permission state rides the
            // ToolUseContext the submit path constructs.
            Some(params.tool_use_context.tool_permission_context.mode),
            params.image_content_blocks,
            params.image_paste_ids,
            params.is_meta,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::message::{RenderableMessageKind, SystemMessage};
    use crate::utils::test_env::{IsolatedProjectSettings, TEST_ENV_LOCK};

    fn queried_result() -> ProcessUserInputBaseResult {
        ProcessUserInputBaseResult {
            messages: vec![RenderableMessage::user("u1", "the prompt")],
            should_query: true,
            allowed_tools: None,
            local_action: None,
            query_source: QuerySource::Prompt,
        }
    }

    fn hook(
        build: impl FnOnce(&mut crate::services::hooks::HookResult),
    ) -> Vec<crate::services::hooks::HookResult> {
        let mut result = crate::services::hooks::HookResult::default();
        build(&mut result);
        vec![result]
    }

    /// Maps to: CC `processUserInput.ts:194-209` — the blocking branch REPLACES
    /// the messages, so the user's own row never reaches the model, and quotes
    /// the prompt back so they can see what was rejected.
    #[test]
    fn blocking_hook_replaces_the_submission_with_one_warning() {
        let mut result = queried_result();
        apply_user_prompt_submit_hook_results(
            &mut result,
            "the prompt",
            &hook(|hook| {
                hook.blocking_error = Some(crate::services::hooks::HookBlockingError {
                    blocking_error: "no secrets please".to_string(),
                    command: "guard.sh".to_string(),
                });
            }),
        );

        assert!(!result.should_query);
        assert_eq!(result.messages.len(), 1, "the user row is erased");
        assert!(matches!(
            &result.messages[0].kind,
            RenderableMessageKind::System(SystemMessage::Informational { content, .. })
                if content == "UserPromptSubmit operation blocked by hook:\nno secrets please\n\nOriginal prompt: the prompt"
        ));
    }

    /// Maps to: CC `processUserInput.ts:211-223` — unlike blocking, this KEEPS
    /// the original prompt in context and only appends the stop notice.
    #[test]
    fn prevent_continuation_appends_a_notice_and_keeps_the_prompt() {
        let mut result = queried_result();
        apply_user_prompt_submit_hook_results(
            &mut result,
            "the prompt",
            &hook(|hook| {
                hook.prevent_continuation = true;
                hook.stop_reason = Some("busy".to_string());
            }),
        );

        assert!(!result.should_query);
        assert_eq!(result.messages.len(), 2, "the user row survives");
        assert!(matches!(
            &result.messages[1].kind,
            RenderableMessageKind::User { message }
                if matches!(
                    message.first_content_block(),
                    Some(crate::types::message::UserContent::Text(text))
                        if text == "Operation stopped by hook: busy"
                )
        ));
    }

    /// Maps to: CC `processUserInput.ts:226-240` — additional context rides an
    /// attachment and the turn still queries.
    #[test]
    fn additional_context_becomes_an_attachment_without_stopping_the_turn() {
        let mut result = queried_result();
        apply_user_prompt_submit_hook_results(
            &mut result,
            "the prompt",
            &hook(|hook| {
                hook.additional_context = Some("branch is main".to_string());
            }),
        );

        assert!(result.should_query, "additional context is not a blocker");
        assert!(matches!(
            &result.messages[1].kind,
            RenderableMessageKind::Attachment(
                crate::utils::attachments::Attachment::HookAdditionalContext { content, hook_name, .. }
            ) if content == &vec!["branch is main".to_string()] && hook_name == "UserPromptSubmit"
        ));
    }

    /// Maps to: CC `processUserInput.ts:274-280` `applyTruncation`.
    #[test]
    fn hook_output_over_the_limit_is_truncated() {
        let mut result = queried_result();
        let long = "x".repeat(MAX_HOOK_OUTPUT_LENGTH + 50);
        apply_user_prompt_submit_hook_results(
            &mut result,
            "the prompt",
            &hook(|hook| hook.additional_context = Some(long)),
        );

        let RenderableMessageKind::Attachment(
            crate::utils::attachments::Attachment::HookAdditionalContext { content, .. },
        ) = &result.messages[1].kind
        else {
            panic!("expected the hook attachment");
        };
        assert!(content[0].ends_with(&format!(
            "… [output truncated - exceeded {MAX_HOOK_OUTPUT_LENGTH} characters]"
        )));
    }

    #[test]
    fn process_user_input_uses_launch_command_snapshot_instead_of_global_registry() {
        let result = process_user_input_base(ProcessUserInputParams {
            input: "/config".to_string(),
            uuid: Some("command-snapshot".to_string()),
            mode: ProcessInputMode::Prompt,
            pre_expansion_input: None,
            skip_slash_commands: false,
            is_meta: false,
            commands: Arc::new(Vec::new()),
            tool_use_context: crate::tool::ToolUseContext::default(),
            image_content_blocks: Vec::new(),
            image_paste_ids: Vec::new(),
        });

        // [0] is CC's synthetic local-command caveat
        // (processSlashCommand.tsx:675-681); the warning row follows it.
        assert!(matches!(
            &result.messages[1].kind,
            RenderableMessageKind::System(SystemMessage::Informational { content: text, .. })
                if text == "Unknown command: /config"
        ));
    }

    fn params(input: &str) -> ProcessUserInputParams {
        ProcessUserInputParams {
            input: input.to_string(),
            uuid: Some("outer-layer".to_string()),
            mode: ProcessInputMode::Prompt,
            pre_expansion_input: None,
            skip_slash_commands: false,
            is_meta: false,
            commands: Arc::new(Vec::new()),
            tool_use_context: crate::tool::ToolUseContext::default(),
            image_content_blocks: Vec::new(),
            image_paste_ids: Vec::new(),
        }
    }

    #[test]
    fn skip_slash_commands_keeps_bridge_input_as_text() {
        let mut input = params("/plain-text");
        input.skip_slash_commands = true;
        let result = process_user_input_base(input);
        assert!(result.should_query);
        assert!(matches!(
            &result.messages[0].kind,
            RenderableMessageKind::User { message }
                if matches!(message.first_content_block(), Some(crate::types::message::UserContent::Text(text)) if text == "/plain-text")
        ));
    }

    /// CC returns local-command results untouched at `processUserInput.ts:171-173`,
    /// before the hook loop. A `should_query == false` dispatch must therefore
    /// come back byte-identical from the outer layer.
    #[tokio::test(flavor = "current_thread")]
    async fn outer_layer_returns_non_querying_dispatch_untouched_like_official() {
        let base = process_user_input_base(params("/config"));
        assert!(!base.should_query, "unknown command must not query");

        let outer = process_user_input(params("/config")).await;

        assert_eq!(outer.should_query, base.should_query);
        assert_eq!(outer.messages.len(), base.messages.len());
        assert_eq!(outer.local_action, base.local_action);
    }

    /// With no `UserPromptSubmit` hook configured, CC's generator returns before
    /// yielding anything (`hooks.ts:3837`), so the outer layer is a pass-through.
    /// The Rust executor short-circuits on an empty match set the same way.
    #[tokio::test(flavor = "current_thread")]
    async fn outer_layer_is_a_pass_through_when_no_hook_is_configured() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _settings = IsolatedProjectSettings::pin();

        let base = process_user_input_base(params("hello"));
        assert!(base.should_query, "a plain prompt must query");

        let outer = process_user_input(params("hello")).await;

        assert!(outer.should_query);
        assert_eq!(
            outer.messages.len(),
            base.messages.len(),
            "no hook configured means nothing is appended"
        );
        // Compare the payload, not the whole row: the two dispatches mint
        // independent timestamps.
        let RenderableMessageKind::User { message } = &outer.messages[0].kind else {
            panic!("the submitted prompt must still lead the result");
        };
        assert_eq!(
            message.content,
            vec![crate::types::message::UserContent::Text(
                "hello".to_string()
            )]
        );
    }

    /// CC's `hasHookForEvent` (`hooks.ts:1591`) counts
    /// `appState.sessionHooks.get(sessionId)` as a third source alongside the
    /// settings snapshot and the registered plugin hooks, and the session id is
    /// `toolUseContext.agentId ?? getSessionId()` (`:3836`). A hook registered
    /// for this turn's agent must therefore run.
    #[tokio::test(flavor = "current_thread")]
    async fn session_scoped_hooks_reach_the_prompt_submit_loop_like_official() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _settings = IsolatedProjectSettings::pin();

        let session = "process-user-input-session";
        crate::utils::hooks::session_hooks::clear_all_session_hooks();
        crate::utils::hooks::session_hooks::add_session_hook(
            session,
            crate::services::hooks::HookEvent::UserPromptSubmit,
            "",
            crate::services::hooks::HookCommand {
                command: r#"printf '%s' '{"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":"from the session hook"}}'"#
                    .to_string(),
                shell: None,
                timeout: Some(5),
                condition: None,
                status: None,
                once: None,
                is_async: None,
                async_rewake: None,
            },
        );

        let mut with_agent = params("hello");
        with_agent.tool_use_context.agent_id = Some(session.to_string());
        let result = process_user_input(with_agent).await;

        crate::utils::hooks::session_hooks::clear_all_session_hooks();

        let attached = result.messages.iter().any(|message| {
            matches!(
                &message.kind,
                RenderableMessageKind::Attachment(
                    crate::utils::attachments::Attachment::HookAdditionalContext { content, .. },
                ) if content.iter().any(|entry| entry == "from the session hook")
            )
        });
        assert!(
            attached,
            "a session-scoped UserPromptSubmit hook must reach the loop; got {:?}",
            result.messages
        );
    }
}
