//! Prompt submit coordinator.
//! Maps to official `utils/handlePromptSubmit.ts`: parse/normalize submitted
//! input, call `process_user_input`, and, when appropriate, hand the resulting
//! messages to `query`. In upstream this function also owns queuing, paste
//! expansion, immediate local command UI handling, and abort/query guards. The
//! Rust port returns typed local actions so `screens::repl` can keep UI state
//! ownership while slash command parsing lives in the official pipeline seam.

use super::process_user_input::process_slash_command::SlashCommandAction;
#[cfg(test)]
use super::process_user_input::process_slash_command::SlashCommandInvocation;
use super::process_user_input::{
    ProcessInputMode, ProcessUserInputBaseResult, ProcessUserInputParams, process_user_input,
};
use crate::commands::Command;
use crate::query::deps::QueryDeps;
#[cfg(test)]
use crate::query::query;
use crate::query::{PendingQueryMessage, QueryParams};
use crate::tool::ToolPermissionContext;
use crate::types::message::RenderableMessage;
use crate::utils::token_budget::parse_token_budget;
use std::sync::Arc;
use uuid::Uuid;

/// Maps to CC `handlePromptSubmit`'s `queryGuard` gate after the immediate
/// `local-jsx` branch.  Only prompt and bash submissions are queueable while a
/// turn is active; local commands that are not immediate are intentionally
/// treated as prompt input and therefore follow the same queue path.
pub fn should_queue_submitted_input(
    mode: &str,
    query_guard_active: bool,
    external_loading: bool,
    immediate_local_ui: bool,
) -> bool {
    (query_guard_active || external_loading)
        && !immediate_local_ui
        && matches!(mode, "prompt" | "bash")
}

/// Maps to CC `processSlashCommand`'s command lookup immediately before the
/// `queryGuard.isActive` branch.  Keeping this predicate in the submit owner
/// prevents the REPL from redispatching an active `local-jsx` command merely to
/// discover that it should run now.
pub fn is_immediate_local_ui_submission(
    input: &str,
    commands: &[Command],
    query_guard_active: bool,
    from_keybinding: bool,
) -> bool {
    if !query_guard_active {
        return false;
    }
    let Some(parsed) = crate::utils::slash_command_parsing::parse_slash_command(input) else {
        return false;
    };
    crate::commands::find_command(&parsed.name, commands).is_some_and(|command| {
        command.kind == crate::commands::CommandKind::LocalUi
            && (command.immediate || from_keybinding)
    })
}

/// Maps to CC `handlePromptSubmit.ts`'s `enqueue` payload construction.  Bash
/// mode stores the command without its leading `!`; the queue consumer owns
/// execution mode and therefore must not parse the presentation prefix again.
pub fn queued_command_for_submission(
    input: &str,
    pre_expansion_value: &str,
    pasted_contents: std::collections::BTreeMap<
        usize,
        crate::components::prompt_input::input_paste::PastedContent,
    >,
    mode: &str,
) -> Option<crate::utils::message_queue_manager::QueuedCommand> {
    let value = if mode == "bash" {
        input
            .trim_start()
            .strip_prefix('!')
            .unwrap_or_default()
            .trim()
            .to_string()
    } else {
        input.trim().to_string()
    };
    if value.is_empty() {
        return None;
    }
    let mut command = crate::utils::message_queue_manager::QueuedCommand::new(value, mode);
    command.pre_expansion_value = Some(if mode == "bash" {
        pre_expansion_value
            .trim_start()
            .strip_prefix('!')
            .unwrap_or_default()
            .trim()
            .to_string()
    } else {
        pre_expansion_value.trim().to_string()
    });
    // CC only keeps `pastedContents` on a queued command when at least one
    // valid image is present. Text references were already expanded into
    // `value`; retaining those entries would make the queue look as though it
    // still had live paste chips after the input was cleared.
    command.pasted_contents = pasted_contents
        .into_iter()
        .filter(|(_, content)| {
            matches!(
                content,
                crate::components::prompt_input::input_paste::PastedContent::Image {
                    data: Some(data),
                    ..
                } if !data.is_empty()
            )
        })
        .collect();
    command.uuid = Some(Uuid::new_v4().to_string());
    Some(command)
}

#[derive(Clone, Debug, PartialEq)]
pub struct HandlePromptSubmitResult {
    pub messages: Vec<RenderableMessage>,
    pub pending: Vec<PendingQueryMessage>,
    pub should_query: bool,
    /// Maps to CC `utils/handlePromptSubmit.ts` forwarding `result.allowedTools`
    /// into REPL's `onQuery` owner. Missing process output becomes `[]`.
    pub allowed_tools: Vec<String>,
    pub query_params: Option<QueryParams>,
    pub local_action: Option<SlashCommandAction>,
}

/// Rust holder for official `handlePromptSubmit(...)` dependencies.
/// The upstream implementation is a function that closes over React setters and
/// `QueryGuard`. Rust keeps the query deps in this small value so iocraft hooks
/// can store it with `use_const` and clone it cheaply across renders.
#[derive(Clone, Debug)]
pub struct HandlePromptSubmit<D> {
    deps: D,
    /// Maps to CC `handlePromptSubmit` `BaseExecutionParams.commands`.
    commands: Arc<Vec<Command>>,
}

impl<D: QueryDeps> HandlePromptSubmit<D> {
    pub fn new(deps: D, commands: Arc<Vec<Command>>) -> Self {
        Self { deps, commands }
    }

    /// Legacy/mock synchronous prompt path retained for unit tests only.
    /// Maps to the pre-actor compatibility seam; production REPL must use
    /// `submit_prompt_deferred_query(...)` and then `spawn_query(...)` so the
    /// SDK/main path never waits on `Vec<PendingQueryMessage>`.
    #[cfg(test)]
    pub async fn submit_prompt(&self, input: String) -> HandlePromptSubmitResult {
        let turn_id = Uuid::new_v4().to_string();
        let token_budget = parse_token_budget(&input);
        let tool_use_context = crate::tool::ToolUseContext::default();
        let processed = process_user_input(ProcessUserInputParams {
            input: input.clone(),
            uuid: Some(turn_id.clone()),
            mode: ProcessInputMode::Prompt,
            pre_expansion_input: None,
            skip_slash_commands: false,
            is_meta: false,
            commands: self.commands.clone(),
            tool_use_context: tool_use_context.clone(),
            image_content_blocks: Vec::new(),
            image_paste_ids: Vec::new(),
        })
        .await;
        let query_params = processed.should_query.then(|| QueryParams {
            turn_id,
            input,
            messages: processed.messages.clone(),
            model_messages: Vec::new(),
            query_source: processed.query_source.clone(),
            token_budget,
            task_budget: None,
            max_turns: None,
            tool_use_context,
            system_prompt: Vec::new(),
            user_context: std::collections::BTreeMap::new(),
            system_context: std::collections::BTreeMap::new(),
        });
        let pending = query_params
            .as_ref()
            .map(|params| query(&self.deps, params))
            .unwrap_or_default();

        HandlePromptSubmitResult {
            messages: processed.messages,
            pending,
            should_query: processed.should_query,
            allowed_tools: processed.allowed_tools.unwrap_or_default(),
            query_params,
            local_action: processed.local_action,
        }
    }

    /// Converts an already-processed prompt/slash-command result into the
    /// caller-owned deferred query shape.
    ///
    /// Maps to CC `utils/handlePromptSubmit.ts` after
    /// `processUserInput(...)`/`processSlashCommand(...)` has returned. This is
    /// used by async prompt slash commands (notably MCP prompts) whose content
    /// must be fetched from the official service boundary before REPL can build
    /// `QueryParams`.
    pub fn submit_processed_prompt_deferred_query(
        &self,
        input: String,
        turn_id: String,
        processed: ProcessUserInputBaseResult,
        tool_permission_context: ToolPermissionContext,
    ) -> HandlePromptSubmitResult {
        let token_budget = parse_token_budget(&input);
        let query_params = processed.should_query.then(|| QueryParams {
            turn_id,
            input,
            messages: processed.messages.clone(),
            model_messages: Vec::new(),
            query_source: processed.query_source.clone(),
            token_budget,
            task_budget: None,
            max_turns: None,
            tool_use_context: crate::tool::ToolUseContext::with_permission_context(
                tool_permission_context,
            ),
            system_prompt: Vec::new(),
            user_context: std::collections::BTreeMap::new(),
            system_context: std::collections::BTreeMap::new(),
        });

        HandlePromptSubmitResult {
            messages: processed.messages,
            pending: Vec::new(),
            should_query: processed.should_query,
            allowed_tools: processed.allowed_tools.unwrap_or_default(),
            query_params,
            local_action: processed.local_action,
        }
    }

    /// Maps to: CC `handlePromptSubmit.ts:476-520` awaiting local-JSX
    /// completion through `processUserInput.ts:174-262`. Native callback
    /// continuation retains the original full context and never redispatches
    /// the slash command or duplicates the canonical hook tail.
    pub(crate) async fn complete_local_command_deferred_query(
        &self,
        input: String,
        processed: ProcessUserInputBaseResult,
        context: crate::tool::ToolUseContext,
        hook_session_id: &str,
    ) -> HandlePromptSubmitResult {
        let mode = crate::utils::permissions::permission_mode::permission_mode_internal_name(
            context.tool_permission_context.mode,
        );
        let processed = super::process_user_input::continue_processed_user_input(
            processed,
            &input,
            mode,
            hook_session_id,
        )
        .await;
        Self::into_deferred_query(input, Uuid::new_v4().to_string(), processed, context)
    }

    /// Submit a prompt but defer query execution to the caller-owned actor.
    ///
    /// Maps to CC `REPL.tsx` threading `ToolPermissionContext` into query
    /// params. Production callers must pass the live REPL/app permission state;
    /// only the legacy `#[cfg(test)]` synchronous path constructs a default
    /// context for isolated unit tests.
    pub async fn submit_prompt_deferred_query(
        &self,
        input: String,
        tool_permission_context: ToolPermissionContext,
    ) -> HandlePromptSubmitResult {
        self.submit_prompt_deferred_query_with_context(
            input,
            crate::tool::ToolUseContext::with_permission_context(tool_permission_context),
            Vec::new(),
            Vec::new(),
        )
        .await
    }

    /// Full-context counterpart used by REPL's direct input path.
    ///
    /// Maps to CC `handlePromptSubmit.ts::getToolUseContext(messages, [],
    /// abortController, mainLoopModel)`: local slash command callbacks must see
    /// the current typed history and live AppState, not a permission-only shell.
    ///
    /// `image_content_blocks` / `image_paste_ids` are CC's pasted-image inputs
    /// (`processTextPrompt.ts:66-88`); they become blocks of the single
    /// submitted user message rather than a message of their own.
    pub async fn submit_prompt_deferred_query_with_context(
        &self,
        input: String,
        tool_use_context: crate::tool::ToolUseContext,
        image_content_blocks: Vec<crate::types::message::UserContent>,
        image_paste_ids: Vec<u32>,
    ) -> HandlePromptSubmitResult {
        let turn_id = Uuid::new_v4().to_string();
        let params = ProcessUserInputParams {
            input: input.clone(),
            uuid: Some(turn_id.clone()),
            mode: ProcessInputMode::Prompt,
            pre_expansion_input: None,
            skip_slash_commands: false,
            is_meta: false,
            commands: self.commands.clone(),
            tool_use_context: tool_use_context.clone(),
            image_content_blocks,
            image_paste_ids,
        };
        // Maps to CC `handlePromptSubmit.ts:476` `await processUserInput({...})`
        // — the outer layer, so a `UserPromptSubmit` hook can erase the
        // submission or stop the turn before any of it reaches `query`.
        let processed = process_user_input(params).await;
        Self::into_deferred_query(input, turn_id, processed, tool_use_context)
    }

    /// Same, minus the `UserPromptSubmit` hooks.
    ///
    /// For prompts the product delivers on its own — the inbox poller and the
    /// scheduled-task runner. Both fire from synchronous iocraft callbacks that
    /// have no `.await`, so they cannot reach the outer layer; they run
    /// `process_user_input_base` directly.
    ///
    /// This is a seam, not a decision: CC routes these through the same
    /// `processUserInput` as typed input, so a configured hook should see them
    /// too. Closing it means giving each caller its own `use_async_handler`, the
    /// way `screens::repl`'s typed path now has one. Tracked as TaskList #45 —
    /// the name is deliberately loud so it cannot be mistaken for the user path.
    pub fn submit_prompt_deferred_query_with_context_skipping_hooks(
        &self,
        input: String,
        tool_use_context: crate::tool::ToolUseContext,
        image_content_blocks: Vec<crate::types::message::UserContent>,
        image_paste_ids: Vec<u32>,
    ) -> HandlePromptSubmitResult {
        let turn_id = Uuid::new_v4().to_string();
        let processed =
            super::process_user_input::process_user_input_base(ProcessUserInputParams {
                input: input.clone(),
                uuid: Some(turn_id.clone()),
                mode: ProcessInputMode::Prompt,
                pre_expansion_input: None,
                skip_slash_commands: false,
                is_meta: false,
                commands: self.commands.clone(),
                tool_use_context: tool_use_context.clone(),
                image_content_blocks,
                image_paste_ids,
            });
        Self::into_deferred_query(input, turn_id, processed, tool_use_context)
    }

    /// Maps to CC `queueProcessor.ts` passing a same-mode batch into
    /// `handlePromptSubmit({ queuedCommands })`. The queue processor isolates
    /// slash and bash commands; this owner still accepts the slash item so its
    /// `skipSlashCommands`, pasted metadata, UUID, and `isMeta` fields follow
    /// the same execution path as every other queued command.
    pub fn submit_queued_commands_deferred_query_with_context(
        &self,
        commands: Vec<crate::utils::message_queue_manager::QueuedCommand>,
        tool_use_context: crate::tool::ToolUseContext,
    ) -> HandlePromptSubmitResult {
        let inputs = commands
            .iter()
            .map(|command| command.value.clone())
            .collect::<Vec<_>>();
        let mut aggregate = HandlePromptSubmitResult {
            messages: Vec::new(),
            pending: Vec::new(),
            should_query: false,
            allowed_tools: Vec::new(),
            query_params: None,
            local_action: None,
        };
        let mut is_first_command = true;
        for command in commands {
            let turn_id = command
                .uuid
                .clone()
                .unwrap_or_else(|| Uuid::new_v4().to_string());
            let image_content_blocks = if is_first_command {
                crate::components::prompt_input::input_paste::image_content_blocks(
                    &command.pasted_contents,
                )
            } else {
                Vec::new()
            };
            let image_paste_ids = if is_first_command {
                command
                    .pasted_contents
                    .values()
                    .filter_map(|content| match content {
                        crate::components::prompt_input::input_paste::PastedContent::Image {
                            id,
                            data: Some(data),
                            ..
                        } if !data.is_empty() => u32::try_from(*id).ok(),
                        _ => None,
                    })
                    .collect()
            } else {
                Vec::new()
            };
            let input = command.value.clone();
            let result =
                super::process_user_input::process_user_input_base(ProcessUserInputParams {
                    input: input.clone(),
                    uuid: Some(turn_id.clone()),
                    mode: ProcessInputMode::Prompt,
                    pre_expansion_input: command.pre_expansion_value.clone(),
                    skip_slash_commands: command.skip_slash_commands,
                    is_meta: command.is_meta,
                    commands: self.commands.clone(),
                    tool_use_context: tool_use_context.clone(),
                    image_content_blocks,
                    image_paste_ids,
                });
            aggregate.messages.extend(result.messages.clone());
            aggregate.should_query |= result.should_query;
            aggregate
                .allowed_tools
                .extend(result.allowed_tools.clone().unwrap_or_default());
            aggregate.local_action = result.local_action.clone();
            if result.should_query {
                let mut current =
                    Self::into_deferred_query(input, turn_id, result, tool_use_context.clone());
                if let Some(params) = current.query_params.as_mut() {
                    params.input = inputs.join("\n");
                }
                aggregate.pending.extend(current.pending);
                aggregate.query_params = current.query_params;
            }
            is_first_command = false;
        }
        aggregate
    }

    /// Shared tail of both: fold the dispatch result into the deferred query
    /// shape REPL owns.
    fn into_deferred_query(
        input: String,
        turn_id: String,
        processed: ProcessUserInputBaseResult,
        mut tool_use_context: crate::tool::ToolUseContext,
    ) -> HandlePromptSubmitResult {
        let token_budget = parse_token_budget(&input);
        if let Some(SlashCommandAction::ApplyPlanMode { next_context }) =
            processed.local_action.as_ref()
        {
            tool_use_context.update_permission_context(next_context.clone());
        }
        let query_params = processed.should_query.then(|| QueryParams {
            turn_id,
            input,
            messages: processed.messages.clone(),
            model_messages: Vec::new(),
            query_source: processed.query_source.clone(),
            token_budget,
            task_budget: None,
            max_turns: None,
            tool_use_context,
            system_prompt: Vec::new(),
            user_context: std::collections::BTreeMap::new(),
            system_context: std::collections::BTreeMap::new(),
        });

        HandlePromptSubmitResult {
            messages: processed.messages,
            pending: Vec::new(),
            should_query: processed.should_query,
            allowed_tools: processed.allowed_tools.unwrap_or_default(),
            query_params,
            local_action: processed.local_action,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::message::{RenderableMessageKind, UserContent};
    use crate::utils::test_env::{IsolatedProjectSettings, TEST_ENV_LOCK};

    #[test]
    fn active_prompt_and_bash_inputs_queue_after_immediate_ui_is_checked() {
        assert!(should_queue_submitted_input("prompt", true, false, false));
        assert!(should_queue_submitted_input("bash", false, true, false));
        assert!(!should_queue_submitted_input("prompt", true, false, true));
        assert!(!should_queue_submitted_input("local", true, false, false));
        assert!(!should_queue_submitted_input("prompt", false, false, false));
        assert_eq!(
            queued_command_for_submission(
                "  ! echo hi  ",
                "  ! echo hi  ",
                Default::default(),
                "bash"
            )
            .unwrap()
            .value,
            "echo hi"
        );
    }

    #[derive(Clone, Debug)]
    struct TestQueryDeps;

    impl QueryDeps for TestQueryDeps {
        fn call_model(
            &self,
            request: crate::query::deps::CallModelRequest,
        ) -> crate::query::deps::CallModelStreamFuture {
            let messages = request.messages;
            Box::pin(async move {
                let (tx, rx) = tokio::sync::mpsc::channel(1);
                let text = messages
                    .iter()
                    .rev()
                    .find_map(|message| match message {
                        crate::types::message::Message::User(user) => {
                            user.content.iter().find_map(|content| match content {
                                crate::types::message::UserContent::Text(text) => {
                                    Some(text.clone())
                                }
                                _ => None,
                            })
                        }
                        _ => None,
                    })
                    .unwrap_or_default();
                tx.send(
                    crate::services::api::claude::QueryModelStreamItem::Assistant(
                        crate::types::message::AssistantMessage {
                            uuid: uuid::Uuid::new_v4().to_string(),
                            timestamp: chrono::Utc::now(),
                            content: vec![crate::types::message::AssistantContent::Text(format!(
                                "echo: {text}"
                            ))],
                            model: None,
                            stop_reason: Some(crate::types::message::StopReason::EndTurn),
                            usage: None,
                        },
                    ),
                )
                .await
                .ok();
                Ok(rx)
            })
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn local_command_continuation_matches_official_hooks_and_full_context() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _settings = IsolatedProjectSettings::pin();
        let _trust = crate::services::hooks::test_support::SessionTrustGuard::accepted();
        let input_path =
            std::env::temp_dir().join(format!("permission-retry-hook-{}.json", Uuid::new_v4()));
        let session = "permissions-completion-hook";
        crate::utils::hooks::session_hooks::clear_all_session_hooks();
        crate::utils::hooks::session_hooks::add_session_hook(
            session,
            crate::services::hooks::HookEvent::UserPromptSubmit,
            "",
            crate::services::hooks::HookCommand {
                command: format!(
                    "cat > '{}'; printf '%s' '{{\"decision\":\"block\",\"reason\":\"retry denied by fixture\"}}'",
                    input_path.display()
                ),
                shell: None,
                timeout: Some(5),
                condition: None,
                status: None,
                once: None,
                is_async: None,
                async_rewake: None,
            },
        );
        let processed =
            super::super::process_user_input::process_slash_command::local_jsx_command_result(
                "allowed-tools",
                "",
                None,
                None,
                true,
                &["Permission granted for: echo test".into()],
                Vec::new(),
            );
        let mut context = crate::tool::ToolUseContext::default();
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        let mut opening_permission = store.tool_permission_context();
        opening_permission.mode = crate::types::permissions::PermissionMode::Auto;
        store.set_tool_permission_context(opening_permission);
        context = context.with_app_store(store.clone());
        // Live state changes while the dialog is mounted; the hook must still
        // receive the source's captured appState permission mode.
        let mut live_permission = store.tool_permission_context();
        live_permission.mode = crate::types::permissions::PermissionMode::Default;
        store.set_tool_permission_context(live_permission);
        context.debug = true;
        context.custom_system_prompt = Some("retained full context".into());
        context.agent_id = Some(session.into());
        let handler = HandlePromptSubmit::new(TestQueryDeps, Arc::new(Vec::new()));
        let blocked = handler
            .complete_local_command_deferred_query(
                "/allowed-tools".into(),
                processed.clone(),
                context.clone(),
                session,
            )
            .await;
        let hook_input: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&input_path).unwrap()).unwrap();
        assert_eq!(hook_input["permission_mode"], "auto");
        assert_eq!(hook_input["prompt"], "/allowed-tools");
        std::fs::remove_file(&input_path).unwrap();
        assert!(!blocked.should_query);
        assert!(blocked.query_params.is_none());
        assert_eq!(blocked.messages.len(), 1);
        assert!(
            matches!(&blocked.messages[0].kind, RenderableMessageKind::System(crate::types::message::SystemMessage::Informational { content, .. }) if content.contains("retry denied by fixture") && content.ends_with("Original prompt: /allowed-tools"))
        );
        crate::utils::hooks::session_hooks::clear_all_session_hooks();
        let resumed = handler
            .complete_local_command_deferred_query(
                "/allowed-tools".into(),
                processed,
                context,
                session,
            )
            .await;
        let params = resumed
            .query_params
            .expect("completed permission retry must request query");
        assert!(params.tool_use_context.debug);
        assert_eq!(
            params.tool_use_context.custom_system_prompt.as_deref(),
            Some("retained full context")
        );
        assert_eq!(params.tool_use_context.agent_id.as_deref(), Some(session));
        assert_eq!(params.input, "/allowed-tools");
        assert_eq!(params.messages.len(), 3);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn handle_prompt_submit_prompt_matches_process_then_query_shape() {
        // Submitting a prompt runs the `UserPromptSubmit` hook path, which
        // captures the hooks-config snapshot lazily from `projectSettings`.
        // Unpinned, this reads THIS repository's `.claude/settings.json` and
        // leaves its hooks in the process-global snapshot for the rest of the
        // run — and would spawn them.
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _settings = IsolatedProjectSettings::pin();
        let handler = HandlePromptSubmit::new(
            TestQueryDeps,
            std::sync::Arc::new(crate::commands::declared_commands_for_tests()),
        );
        let result = handler.submit_prompt("hello".to_string()).await;

        assert!(result.should_query);
        assert!(matches!(
            &result.messages[0].kind,
            RenderableMessageKind::User { message } if matches!(
                message.first_content_block(),
                Some(UserContent::Text(text)) if text == "hello"
            )
        ));
        assert!(result.pending.iter().any(|pending| matches!(
            &pending.message.kind,
            RenderableMessageKind::Assistant { message }
                if matches!(
                    message.first_content_block(),
                    Some(crate::types::message::AssistantContent::Text(text))
                        if text.contains("hello")
                )
        )));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn handle_prompt_submit_deferred_query_carries_parsed_token_budget() {
        let handler = HandlePromptSubmit::new(
            TestQueryDeps,
            std::sync::Arc::new(crate::commands::declared_commands_for_tests()),
        );
        let mut permission_context = ToolPermissionContext::default();
        permission_context.mode = crate::types::permissions::PermissionMode::Plan;
        let result = handler
            .submit_prompt_deferred_query(
                "+500k keep working".to_string(),
                permission_context.clone(),
            )
            .await;

        assert_eq!(
            result
                .query_params
                .as_ref()
                .and_then(|params| params.token_budget),
            Some(500_000)
        );
        assert_eq!(
            result
                .query_params
                .as_ref()
                .map(|params| params.tool_use_context.tool_permission_context.clone()),
            Some(permission_context)
        );
        assert!(result.pending.is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn processed_prompt_allowed_tools_match_official_on_query_transport() {
        let handler = HandlePromptSubmit::new(
            TestQueryDeps,
            std::sync::Arc::new(crate::commands::declared_commands_for_tests()),
        );
        let processed = ProcessUserInputBaseResult {
            messages: vec![RenderableMessage::user_block(
                "cmd-1",
                UserContent::MetaText("MCP prompt body".to_string()),
            )],
            should_query: true,
            allowed_tools: Some(vec!["Read(/repo/**)".to_string()]),
            local_action: None,
            query_source: crate::constants::query_source::QuerySource::Prompt,
        };

        let result = handler.submit_processed_prompt_deferred_query(
            "/review".to_string(),
            "turn-1".to_string(),
            processed,
            ToolPermissionContext::default(),
        );

        assert!(result.should_query);
        assert!(result.pending.is_empty());
        let params = result.query_params.as_ref().unwrap();
        assert_eq!(params.turn_id, "turn-1");
        assert_eq!(params.input, "/review");
        assert_eq!(params.messages.len(), 1);
        assert_eq!(result.allowed_tools, ["Read(/repo/**)"]);
        assert!(
            params
                .tool_use_context
                .tool_permission_context
                .always_allow_rules
                .get(&crate::types::permissions::PermissionRuleSource::Command)
                .is_none()
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn handle_prompt_submit_slash_command_does_not_query() {
        let handler = HandlePromptSubmit::new(
            TestQueryDeps,
            std::sync::Arc::new(crate::commands::declared_commands_for_tests()),
        );
        let result = handler.submit_prompt("/config".to_string()).await;

        assert!(!result.should_query);
        assert!(result.pending.is_empty());
        assert_eq!(
            result.local_action,
            Some(SlashCommandAction::OpenLocalCommandUi {
                command: super::super::process_user_input::process_slash_command::LocalCommandUi::Settings {
                    default_tab: super::super::process_user_input::process_slash_command::LocalSettingsTab::Config,
                },
                invocation: SlashCommandInvocation::new("config", ""),
            })
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn handle_prompt_submit_full_context_reaches_local_command_callbacks() {
        let handler = HandlePromptSubmit::new(
            TestQueryDeps,
            std::sync::Arc::new(crate::commands::declared_commands_for_tests()),
        );
        let context = crate::tool::ToolUseContext::default().with_messages(vec![
            crate::types::message::Message::Assistant(crate::types::message::AssistantMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![crate::types::message::AssistantContent::Text(
                    "copy me".to_string(),
                )],
                model: None,
                stop_reason: None,
                usage: None,
            }),
        ]);
        let result = handler
            .submit_prompt_deferred_query_with_context(
                "/copy".to_string(),
                context,
                Vec::new(),
                Vec::new(),
            )
            .await;

        assert!(!result.should_query);
        assert!(matches!(
            result.local_action,
            Some(SlashCommandAction::CopyToClipboard {
                content: crate::commands::copy::CopyContent { ref text, .. },
                ..
            }) if text == "copy me"
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn handle_prompt_submit_plan_description_copies_prepared_mode_into_query_context() {
        let handler = HandlePromptSubmit::new(
            TestQueryDeps,
            std::sync::Arc::new(crate::commands::declared_commands_for_tests()),
        );
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        let context = crate::tool::ToolUseContext::default().with_app_store(store);
        let result = handler
            .submit_prompt_deferred_query_with_context(
                "/plan implement carefully".to_string(),
                context,
                Vec::new(),
                Vec::new(),
            )
            .await;

        assert!(result.should_query);
        assert_eq!(
            result
                .query_params
                .as_ref()
                .expect("plan query")
                .tool_use_context
                .tool_permission_context
                .mode,
            crate::types::permissions::PermissionMode::Plan
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn handle_prompt_submit_unknown_slash_command_does_not_query() {
        let handler = HandlePromptSubmit::new(
            TestQueryDeps,
            std::sync::Arc::new(crate::commands::declared_commands_for_tests()),
        );
        let result = handler.submit_prompt("/unknown-command".to_string()).await;

        assert!(!result.should_query);
        assert!(result.pending.is_empty());
        assert!(result.local_action.is_none());
        // [0] is CC's synthetic local-command caveat
        // (processSlashCommand.tsx:675-681); the warning row follows it.
        assert!(matches!(
            &result.messages[1].kind,
            RenderableMessageKind::System(_)
        ));
    }
}
