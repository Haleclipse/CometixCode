//! Streaming parallel tool executor.
//!
//! Maps to: CC `services/tools/StreamingToolExecutor.ts` — the
//! `StreamingToolExecutor` class (`addTool`, `getCompletedResults`,
//! `getRemainingResults`, `discard`) that starts eligible tools while the
//! assistant message is still streaming and drains remaining results afterwards.
//!
//! This Rust port keeps the official file boundary and data model. It can run
//! permission-preapproved tools off-thread and buffers `MessageUpdate`s for the
//! query loop. Interactive permission requests are still surfaced back to the
//! query actor for the existing pause/resume path instead of being awaited
//! inside the executor like React's `useCanUseTool` promise.

use crate::services::tools::tool_execution::{
    ToolContextModifier, find_tool_call, prepare_permission_prompt_hooks,
    prepare_permission_request_before_prompt, run_tool_use, should_ask_permission_request,
    streamed_check_permissions_and_call_tool_after_pre_tool_hooks_with_response,
};
use crate::services::tools::tool_orchestration::MessageUpdate;
use crate::tool::{AbortController, InterruptBehavior, ToolUseContext};
use crate::types::message::{AssistantMessage, ToolUseBlock, UserMessage};
use crate::types::permissions::{
    PermissionPromptChoice, PermissionPromptResponse, PermissionRequest, ToolUseConfirm,
};
use crate::types::tools::Tool;
use std::sync::mpsc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToolStatus {
    Queued,
    Executing,
    PermissionBlocked,
    Completed,
    Yielded,
}

#[derive(Clone, Debug)]
struct TrackedTool {
    id: String,
    block: ToolUseBlock,
    assistant_message: AssistantMessage,
    status: ToolStatus,
    is_concurrency_safe: bool,
    results: Vec<MessageUpdate>,
    pending_progress: Vec<MessageUpdate>,
    context_modifiers: Vec<ToolContextModifier>,
    pre_tool_prevent_continuation: bool,
    pre_tool_stop_reason: Option<String>,
}

#[derive(Debug)]
struct ToolCompletion {
    id: String,
    results: Vec<MessageUpdate>,
    context_modifiers: Vec<ToolContextModifier>,
    bash_error_description: Option<String>,
    pre_tool_prevent_continuation: bool,
    pre_tool_stop_reason: Option<String>,
}

#[derive(Debug)]
enum ToolWorkerEvent {
    Completed(ToolCompletion),
    Progress {
        id: String,
        progress: crate::types::tools::ToolProgress,
    },
}

/// Maps to: CC `services/tools/StreamingToolExecutor.ts`
/// `class StreamingToolExecutor`.
pub struct StreamingToolExecutor {
    tool_definitions: Vec<Tool>,
    tools: Vec<TrackedTool>,
    tool_use_context: ToolUseContext,
    has_errored: bool,
    errored_tool_description: String,
    sibling_abort_controller: AbortController,
    discarded: bool,
    completion_tx: mpsc::Sender<ToolWorkerEvent>,
    completion_rx: mpsc::Receiver<ToolWorkerEvent>,
    wakeup_tx: async_channel::Sender<()>,
    wakeup_rx: async_channel::Receiver<()>,
}

impl StreamingToolExecutor {
    /// Maps to: CC `StreamingToolExecutor.constructor(...)`.
    pub fn new(tool_definitions: Vec<Tool>, mut tool_use_context: ToolUseContext) -> Self {
        tool_use_context.tools = tool_definitions.clone();
        let (completion_tx, completion_rx) = mpsc::channel();
        let (wakeup_tx, wakeup_rx) = async_channel::unbounded();
        let sibling_abort_controller =
            AbortController::child_of(tool_use_context.abort_controller.clone());
        Self {
            tool_definitions,
            tools: Vec::new(),
            tool_use_context,
            has_errored: false,
            errored_tool_description: String::new(),
            sibling_abort_controller,
            discarded: false,
            completion_tx,
            completion_rx,
            wakeup_tx,
            wakeup_rx,
        }
    }

    /// Async wakeup used by the query actor to mirror CC promise resolution
    /// while the model stream is still open.
    pub fn wakeup_receiver(&self) -> async_channel::Receiver<()> {
        self.wakeup_rx.clone()
    }

    /// Maps to: CC `StreamingToolExecutor.discard()`.
    pub fn discard(&mut self) {
        self.discarded = true;
    }

    /// Add a tool to the execution queue.
    /// Maps to: CC `StreamingToolExecutor.addTool(block, assistantMessage)`.
    pub fn add_tool(&mut self, block: ToolUseBlock, assistant_message: AssistantMessage) {
        let Some(tool_definition) =
            crate::types::tools::find_tool_by_name(&self.tool_definitions, &block.name)
        else {
            self.tools.push(TrackedTool {
                id: block.id.0.clone(),
                results: vec![synthetic_error_update(
                    &block,
                    &assistant_message,
                    &self.tool_use_context,
                    format!(
                        "<tool_use_error>Error: No such tool available: {}</tool_use_error>",
                        block.name
                    ),
                )],
                block,
                assistant_message,
                status: ToolStatus::Completed,
                is_concurrency_safe: true,
                pending_progress: Vec::new(),
                context_modifiers: Vec::new(),
                pre_tool_prevent_continuation: false,
                pre_tool_stop_reason: None,
            });
            return;
        };

        // Maps to: CC `services/tools/StreamingToolExecutor.ts:104-112`.
        // `safeParse(...).data`, not the raw model JSON, owns the
        // concurrency decision for this streaming queue entry.
        let parsed_input = if tool_definition.is_mcp {
            block.input.clone()
        } else {
            find_tool_call(&tool_definition.name)
                .map(|tool| tool.normalize_input_with_context(&block.input, &self.tool_use_context))
                .unwrap_or_else(|| block.input.clone())
        };
        let is_concurrency_safe =
            crate::services::tools::tool_execution::validate_tool_input_against_schema(
                &tool_definition.name,
                &parsed_input,
                &tool_definition.input_schema,
            )
            .is_ok()
                && std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    if tool_definition.is_mcp {
                        crate::services::mcp::client::mcp_tool_snapshot_for_invocation(
                            &tool_definition.name,
                            &self.tool_use_context.mcp_state,
                        )
                        .is_some_and(|tool| tool.read_only_hint)
                    } else {
                        find_tool_call(&tool_definition.name)
                            .is_some_and(|tool| tool.is_concurrency_safe(&parsed_input))
                    }
                }))
                .unwrap_or(false);
        self.tools.push(TrackedTool {
            id: block.id.0.clone(),
            block,
            assistant_message,
            status: ToolStatus::Queued,
            is_concurrency_safe,
            results: Vec::new(),
            pending_progress: Vec::new(),
            context_modifiers: Vec::new(),
            pre_tool_prevent_continuation: false,
            pre_tool_stop_reason: None,
        });
        self.process_queue();
    }

    /// Maps to: CC `StreamingToolExecutor.getCompletedResults()`.
    pub fn get_completed_results(&mut self) -> Vec<MessageUpdate> {
        if self.discarded {
            return Vec::new();
        }
        self.drain_completed_workers();

        let mut results = Vec::new();
        for tool in &mut self.tools {
            while !tool.pending_progress.is_empty() {
                results.push(tool.pending_progress.remove(0));
            }

            if tool.status == ToolStatus::Yielded {
                continue;
            }

            if tool.status == ToolStatus::PermissionBlocked {
                if !tool.results.is_empty() {
                    // Whole-snapshot adoption is only safe at a serial point
                    // (no concurrent sibling may have merged context changes
                    // in the meantime); concurrent-safe tools contribute via
                    // commutative modifiers applied in `apply_completion`.
                    let adopt_snapshot = !tool.is_concurrency_safe;
                    for mut update in std::mem::take(&mut tool.results) {
                        if adopt_snapshot {
                            let live = self.tool_use_context.clone();
                            self.tool_use_context = update.new_context.clone();
                            self.tool_use_context.retain_file_read_handles_from(&live);
                        }
                        // CC yields one live shared context. Never expose a
                        // concurrent worker's pre-merge clone to query.rs.
                        update.new_context = self.tool_use_context.clone();
                        results.push(update);
                    }
                    // The tool is still logically executing: CC's
                    // `runToolUse(...)` promise is awaiting the interactive
                    // permission decision inside `StreamingToolExecutor`.
                    break;
                }
                continue;
            }

            if tool.status == ToolStatus::Completed {
                tool.status = ToolStatus::Yielded;
                let adopt_snapshot = !tool.is_concurrency_safe;
                for mut update in tool.results.clone() {
                    if adopt_snapshot {
                        let live = self.tool_use_context.clone();
                        self.tool_use_context = update.new_context.clone();
                        self.tool_use_context.retain_file_read_handles_from(&live);
                    }
                    update.new_context = self.tool_use_context.clone();
                    results.push(update);
                }
                mark_tool_use_as_complete(&mut self.tool_use_context, &tool.id);
            } else if tool.status == ToolStatus::Executing && !tool.is_concurrency_safe {
                break;
            }
        }
        results
    }

    /// Drain every remaining buffered result.
    /// Maps to: CC `StreamingToolExecutor.getRemainingResults()`.
    pub fn get_remaining_results(&mut self) -> Vec<MessageUpdate> {
        if self.discarded {
            return Vec::new();
        }

        let mut results = Vec::new();
        while self.has_unfinished_tools() {
            self.process_queue();
            let completed = self.get_completed_results();
            if !completed.is_empty() {
                let blocked_on_permission =
                    completed.iter().any(|update| update.blocked_on_permission);
                results.extend(completed);
                if blocked_on_permission {
                    return results;
                }
                continue;
            }

            if self.has_executing_tools() {
                if !self.recv_one_completed_worker() {
                    break;
                }
                continue;
            }

            // Queued tools with no executing worker should have been started by
            // `process_queue()`. Avoid an infinite loop if a future gating bug
            // leaves a queue item behind — but make that bug loud in debug
            // builds instead of silently dropping the tool.
            debug_assert!(
                !self
                    .tools
                    .iter()
                    .any(|tool| tool.status == ToolStatus::Queued),
                "StreamingToolExecutor: queued tool left behind with no executing worker"
            );
            break;
        }
        results.extend(self.get_completed_results());
        results
    }

    /// Maps to the continuation half of CC `runToolUse(...)` while the
    /// `StreamingToolExecutor` promise remains alive awaiting `useCanUseTool`.
    pub async fn continue_after_permission(
        &mut self,
        permission_request: PermissionRequest,
        response: PermissionPromptResponse,
        mut tool_use_context: ToolUseContext,
    ) -> Vec<MessageUpdate> {
        self.drain_completed_workers();
        let Some(index) = self
            .tools
            .iter()
            .position(|tool| tool.id == permission_request.tool_use_id)
        else {
            return Vec::new();
        };
        tool_use_context.retain_file_read_handles_from(&self.tool_use_context);
        tool_use_context.tools = self.tool_definitions.clone();
        tool_use_context.abort_controller =
            AbortController::child_of(self.sibling_abort_controller.clone());
        let interrupt_behavior = find_tool_call(&self.tools[index].block.name)
            .map(|tool| tool.interrupt_behavior())
            .unwrap_or(InterruptBehavior::Block);
        tool_use_context.mark_in_progress_with_behavior(
            permission_request.tool_use_id.clone(),
            interrupt_behavior,
        );
        let pre_tool_prevent_continuation = self.tools[index].pre_tool_prevent_continuation;
        let pre_tool_stop_reason = self.tools[index].pre_tool_stop_reason.clone();
        self.tools[index].status = ToolStatus::Executing;
        let completion = continue_after_permission_worker(
            self.tools[index].block.clone(),
            self.tools[index].assistant_message.clone(),
            permission_request,
            pre_tool_prevent_continuation,
            pre_tool_stop_reason,
            tool_use_context,
            response,
        )
        .await;
        self.apply_completion(completion);
        self.get_completed_results()
    }

    /// Maps to: CC `StreamingToolExecutor.getUpdatedContext()`.
    pub fn get_updated_context(&self) -> ToolUseContext {
        self.tool_use_context.clone()
    }

    /// Rust actor-resume seam: after an interactive permission continuation
    /// updates the authoritative `ToolUseContext`, sync it back before queued
    /// streaming tools start. CC keeps this object live by reference; Rust's
    /// query actor owns cloned state across pause/resume yields.
    pub fn sync_tool_use_context(&mut self, mut tool_use_context: ToolUseContext) {
        tool_use_context.retain_file_read_handles_from(&self.tool_use_context);
        tool_use_context.tools = self.tool_definitions.clone();
        self.tool_use_context = tool_use_context;
    }

    /// Maps to: CC `services/tools/StreamingToolExecutor.ts#StreamingToolExecutor.canExecuteTool`.
    fn can_execute_tool(&self, is_concurrency_safe: bool) -> bool {
        let executing = self
            .tools
            .iter()
            .filter(|tool| {
                matches!(
                    tool.status,
                    ToolStatus::Executing | ToolStatus::PermissionBlocked
                )
            })
            .collect::<Vec<_>>();
        executing.is_empty()
            || (is_concurrency_safe && executing.iter().all(|tool| tool.is_concurrency_safe))
    }

    /// Maps to: CC `StreamingToolExecutor.processQueue()`.
    fn process_queue(&mut self) {
        self.drain_completed_workers();
        let mut index = 0;
        while index < self.tools.len() {
            if self.tools[index].status != ToolStatus::Queued {
                index += 1;
                continue;
            }
            let is_concurrency_safe = self.tools[index].is_concurrency_safe;
            if self.can_execute_tool(is_concurrency_safe) {
                self.execute_tool(index);
                index += 1;
            } else {
                if !is_concurrency_safe {
                    break;
                }
                index += 1;
            }
        }
    }

    /// Maps to: CC `StreamingToolExecutor.executeTool(...)`.
    fn execute_tool(&mut self, index: usize) {
        if self.discarded {
            return;
        }

        let block = self.tools[index].block.clone();
        let assistant_message = self.tools[index].assistant_message.clone();
        let id = self.tools[index].id.clone();
        let mut context = self.tool_use_context.clone();
        context.abort_controller = AbortController::child_of(self.sibling_abort_controller.clone());
        let interrupt_behavior = find_tool_call(&block.name)
            .map(|tool| tool.interrupt_behavior())
            .unwrap_or(InterruptBehavior::Block);
        context.mark_in_progress_with_behavior(id.clone(), interrupt_behavior);
        self.tool_use_context
            .mark_in_progress_with_behavior(id.clone(), interrupt_behavior);
        self.tools[index].status = ToolStatus::Executing;

        if self.has_errored || context.abort_controller.is_aborted() {
            let reason = if self.has_errored {
                if self.errored_tool_description.is_empty() {
                    "Cancelled: parallel tool call errored".to_string()
                } else {
                    format!(
                        "Cancelled: parallel tool call {} errored",
                        self.errored_tool_description
                    )
                }
            } else {
                "User rejected tool use".to_string()
            };
            self.apply_completion(ToolCompletion {
                id,
                results: vec![synthetic_error_update(
                    &block,
                    &assistant_message,
                    &context,
                    reason,
                )],
                context_modifiers: vec![ToolContextModifier::mark_complete(block.id.0.clone())],
                bash_error_description: None,
                pre_tool_prevent_continuation: false,
                pre_tool_stop_reason: None,
            });
            return;
        }

        let worker_tx = self.completion_tx.clone();
        let worker_wakeup_tx = self.wakeup_tx.clone();
        let progress_tx = self.completion_tx.clone();
        let progress_wakeup_tx = self.wakeup_tx.clone();
        let progress_tool_id = id.clone();
        // Route progress exclusively through the executor's buffer so the
        // query actor emits it interleaved (and ordered) with message
        // updates via `getCompletedResults`, matching CC. Chaining the
        // context-level sink here would double-send every snapshot and let
        // the slower buffered copy overwrite a newer direct one.
        context.tool_progress_sink = crate::tool::ToolProgressSink(Some(std::sync::Arc::new(
            move |progress: crate::types::tools::ToolProgress| {
                let _ = progress_tx.send(ToolWorkerEvent::Progress {
                    id: progress_tool_id.clone(),
                    progress,
                });
                let _ = progress_wakeup_tx.try_send(());
            },
        )));
        // See tool_orchestration.rs: CC's AsyncLocalStorage teammate scope
        // reaches every tool continuation; Rust's task-local must be carried
        // across the worker thread by hand.
        let teammate_context = crate::utils::teammate_context::capture_teammate_context();
        std::thread::spawn(move || {
            let completion = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime.block_on(async move {
                    match teammate_context {
                        Some(teammate_context) => {
                            crate::utils::teammate_context::run_with_teammate_context(
                                teammate_context,
                                execute_tool_worker(block, assistant_message, context),
                            )
                            .await
                        }
                        None => execute_tool_worker(block, assistant_message, context).await,
                    }
                }),
                Err(error) => ToolCompletion {
                    id: block.id.0.clone(),
                    results: vec![synthetic_error_update(
                        &block,
                        &assistant_message,
                        &context,
                        format!("<tool_use_error>runtime_error: {error}</tool_use_error>"),
                    )],
                    context_modifiers: vec![ToolContextModifier::mark_complete(block.id.0.clone())],
                    bash_error_description: None,
                    pre_tool_prevent_continuation: false,
                    pre_tool_stop_reason: None,
                },
            };
            let _ = worker_tx.send(ToolWorkerEvent::Completed(completion));
            let _ = worker_wakeup_tx.try_send(());
        });
    }

    fn drain_completed_workers(&mut self) {
        while let Ok(event) = self.completion_rx.try_recv() {
            self.apply_worker_event(event);
        }
    }

    fn recv_one_completed_worker(&mut self) -> bool {
        match self.completion_rx.recv() {
            Ok(event) => {
                self.apply_worker_event(event);
                true
            }
            Err(_) => false,
        }
    }

    fn apply_worker_event(&mut self, event: ToolWorkerEvent) {
        match event {
            ToolWorkerEvent::Completed(completion) => self.apply_completion(completion),
            ToolWorkerEvent::Progress { id, progress } => {
                let update = progress_update(progress, &self.tool_use_context);
                if let Some(tool) = self.tools.iter_mut().find(|tool| tool.id == id) {
                    tool.pending_progress.push(update);
                }
            }
        }
    }

    fn apply_completion(&mut self, completion: ToolCompletion) {
        let sibling_error_already = self.has_errored;
        if let Some(description) = completion.bash_error_description.clone() {
            self.has_errored = true;
            self.errored_tool_description = description;
            self.sibling_abort_controller.abort();
        }
        let Some(tool) = self.tools.iter_mut().find(|tool| tool.id == completion.id) else {
            return;
        };
        // Note on CC's per-tool `thisToolErrored` guard
        // (StreamingToolExecutor.ts:347-363): in this port a sibling killed
        // by the sibling-abort reports `interrupted` (an error result) too,
        // and — matching CC's abort path — must surface the synthetic
        // "Cancelled: parallel tool call …" message. A tool that genuinely
        // errored *before* the first sibling error is naturally preserved
        // because `sibling_error_already` is still false when its completion
        // is applied (completions are processed in arrival order).
        if sibling_error_already
            && matches!(
                tool.status,
                ToolStatus::Executing | ToolStatus::PermissionBlocked
            )
        {
            let reason = if self.errored_tool_description.is_empty() {
                "Cancelled: parallel tool call errored".to_string()
            } else {
                format!(
                    "Cancelled: parallel tool call {} errored",
                    self.errored_tool_description
                )
            };
            tool.results = vec![synthetic_error_update(
                &tool.block,
                &tool.assistant_message,
                &self.tool_use_context,
                reason,
            )];
            tool.context_modifiers = vec![ToolContextModifier::mark_complete(tool.id.clone())];
            tool.pre_tool_prevent_continuation = false;
            tool.pre_tool_stop_reason = None;
        } else {
            tool.results = completion.results;
            tool.context_modifiers = completion.context_modifiers;
            tool.pre_tool_prevent_continuation = completion.pre_tool_prevent_continuation;
            tool.pre_tool_stop_reason = completion.pre_tool_stop_reason;
        }
        tool.status = if tool
            .results
            .iter()
            .any(|update| update.blocked_on_permission)
        {
            ToolStatus::PermissionBlocked
        } else {
            ToolStatus::Completed
        };
        // Apply modifiers unconditionally: they are commutative
        // (mark-complete is idempotent, permission updates merge), so this is
        // safe for concurrent siblings completing out of order — unlike
        // taking a worker's whole snapshot context, which would erase grants
        // merged from other siblings in the meantime.
        for modifier in &tool.context_modifiers {
            self.tool_use_context = modifier.modify_context(self.tool_use_context.clone());
        }
    }

    /// Maps to: CC `services/tools/StreamingToolExecutor.ts#StreamingToolExecutor.hasExecutingTools`.
    fn has_executing_tools(&self) -> bool {
        self.tools
            .iter()
            .any(|tool| tool.status == ToolStatus::Executing)
    }

    /// Maps to: CC `services/tools/StreamingToolExecutor.ts#StreamingToolExecutor.hasUnfinishedTools`.
    fn has_unfinished_tools(&self) -> bool {
        self.tools
            .iter()
            .any(|tool| tool.status != ToolStatus::Yielded)
    }

    /// Maps to: CC `services/tools/StreamingToolExecutor.ts#StreamingToolExecutor.getToolDescription`.
    fn get_tool_description(block: &ToolUseBlock) -> String {
        let summary = block
            .input
            .get("command")
            .or_else(|| block.input.get("file_path"))
            .or_else(|| block.input.get("pattern"))
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        if summary.is_empty() {
            return block.name.clone();
        }
        let truncated = if summary.encode_utf16().count() > 40 {
            format!(
                "{}…",
                String::from_utf16_lossy(&summary.encode_utf16().take(40).collect::<Vec<_>>())
            )
        } else {
            summary.to_string()
        };
        format!("{}({truncated})", block.name)
    }
}

async fn execute_tool_worker(
    block: ToolUseBlock,
    assistant_message: AssistantMessage,
    mut context: ToolUseContext,
) -> ToolCompletion {
    let id = block.id.0.clone();
    let mut results = Vec::new();
    let mut context_modifiers = Vec::new();
    let mut permission_queue = Vec::<ToolUseConfirm>::new();
    let gate = run_tool_use(&block, &assistant_message, &context, &mut permission_queue);

    let Some(request) = gate.request else {
        let modifier = ToolContextModifier::mark_complete(id.clone());
        context = modifier.modify_context(context);
        context_modifiers.push(modifier.clone());
        let update = MessageUpdate {
            message: gate.message,
            model_message: None,
            progress: None,
            tool_result: gate.tool_result,
            new_context: context,
            permission_request: None,
            blocked_on_permission: false,
            forced_choice: gate.forced_choice,
            context_modifier: Some(modifier),
        };
        let bash_error_description = (block.name
            == crate::tools::bash_tool::tool_name::BASH_TOOL_NAME
            && is_error_tool_result(update.tool_result.as_ref()))
        .then(|| StreamingToolExecutor::get_tool_description(&block));
        results.push(update);
        return ToolCompletion {
            id,
            results,
            context_modifiers,
            bash_error_description,
            pre_tool_prevent_continuation: false,
            pre_tool_stop_reason: None,
        };
    };

    let pre_tool_prepare = prepare_permission_request_before_prompt(&request, &context).await;
    let pre_tool_prevent_continuation = pre_tool_prepare.prevent_continuation;
    let pre_tool_stop_reason = pre_tool_prepare.stop_reason.clone();
    for hook_message in pre_tool_prepare.hook_messages {
        results.push(MessageUpdate {
            message: Some(hook_message),
            model_message: None,
            progress: None,
            tool_result: None,
            new_context: context.clone(),
            permission_request: None,
            blocked_on_permission: false,
            forced_choice: None,
            context_modifier: None,
        });
    }
    for model_message in pre_tool_prepare.hook_model_messages {
        results.push(MessageUpdate {
            message: None,
            model_message: Some(model_message),
            progress: None,
            tool_result: None,
            new_context: context.clone(),
            permission_request: None,
            blocked_on_permission: false,
            forced_choice: None,
            context_modifier: None,
        });
    }
    let mut request = pre_tool_prepare.request;
    if !pre_tool_prepare.permission_updates.is_empty() {
        let next = crate::utils::permissions::permission_update::apply_permission_updates(
            &context.tool_permission_context,
            &pre_tool_prepare.permission_updates,
        );
        context.update_permission_context(next);
        // Also express the grant as a commutative modifier so it survives
        // out-of-order completion of concurrent siblings (the executor no
        // longer takes a concurrent worker's whole snapshot context).
        context_modifiers.push(ToolContextModifier::apply_permission_updates(
            id.clone(),
            pre_tool_prepare.permission_updates.clone(),
        ));
    }
    let hook_choice = crate::services::tools::tool_execution::merge_forced_permission_choices(
        pre_tool_prepare.forced_choice,
        gate.forced_choice,
    );
    let required = crate::services::tools::tool_execution::apply_required_can_use_tool_after_hooks(
        request,
        &context,
        Some(&assistant_message),
        hook_choice,
        pre_tool_prepare.hook_supplied_updated_input,
    )
    .await;
    let required = match required {
        Ok(required) => required,
        Err(error) => {
            let failed = crate::services::tools::tool_execution::permission_check_error_result(
                &id,
                &block.name,
                Some(&assistant_message),
                &error,
            );
            let modifier = ToolContextModifier::mark_complete(id.clone());
            context = modifier.modify_context(context);
            context_modifiers.push(modifier.clone());
            results.push(MessageUpdate {
                message: failed.message,
                model_message: None,
                progress: None,
                tool_result: failed.tool_result,
                new_context: context,
                permission_request: None,
                blocked_on_permission: false,
                forced_choice: None,
                context_modifier: Some(modifier),
            });
            return ToolCompletion {
                id,
                results,
                context_modifiers,
                bash_error_description: (block.name
                    == crate::tools::bash_tool::tool_name::BASH_TOOL_NAME)
                    .then(|| StreamingToolExecutor::get_tool_description(&block)),
                pre_tool_prevent_continuation,
                pre_tool_stop_reason,
            };
        }
    };
    request = required.request;
    if !required.permission_updates.is_empty() {
        let next = crate::utils::permissions::permission_update::apply_permission_updates(
            &context.tool_permission_context,
            &required.permission_updates,
        );
        context.update_permission_context(next);
        context_modifiers.push(ToolContextModifier::apply_permission_updates(
            id.clone(),
            required.permission_updates,
        ));
    }
    let mut forced_choice = required.forced_choice;
    let mut should_ask = required.force_ask
        || (gate.blocked_on_permission
            && forced_choice.is_none()
            && should_ask_permission_request(&request, &context));

    if should_ask {
        let prompt_prepare = prepare_permission_prompt_hooks(&request, &context).await;
        for hook_message in prompt_prepare.hook_messages {
            results.push(MessageUpdate {
                message: Some(hook_message),
                model_message: None,
                progress: None,
                tool_result: None,
                new_context: context.clone(),
                permission_request: None,
                blocked_on_permission: false,
                forced_choice: None,
                context_modifier: None,
            });
        }
        for model_message in prompt_prepare.hook_model_messages {
            results.push(MessageUpdate {
                message: None,
                model_message: Some(model_message),
                progress: None,
                tool_result: None,
                new_context: context.clone(),
                permission_request: None,
                blocked_on_permission: false,
                forced_choice: None,
                context_modifier: None,
            });
        }
        request = prompt_prepare.request;
        if !prompt_prepare.permission_updates.is_empty() {
            let next = crate::utils::permissions::permission_update::apply_permission_updates(
                &context.tool_permission_context,
                &prompt_prepare.permission_updates,
            );
            context.update_permission_context(next);
            context_modifiers.push(ToolContextModifier::apply_permission_updates(
                id.clone(),
                prompt_prepare.permission_updates,
            ));
        }
        // CC PermissionContext.ts:319-335 / interactiveHandler.ts:417-429:
        // PermissionRequest resolves the existing Ask. Its allow is not a
        // PreToolUse approval and must not re-enter that rule-check path.
        // No hook decision leaves the original Ask pending. The producer's
        // existing Glob/Grep reroute guard remains an explicit partial seam.
        forced_choice = prompt_prepare.forced_choice;
        should_ask = forced_choice.is_none() || prompt_prepare.force_ask;
    }

    if should_ask {
        results.push(MessageUpdate {
            message: None,
            model_message: None,
            progress: None,
            tool_result: None,
            new_context: context,
            permission_request: Some(request),
            blocked_on_permission: true,
            forced_choice,
            context_modifier: None,
        });
        return ToolCompletion {
            id,
            results,
            context_modifiers,
            bash_error_description: None,
            pre_tool_prevent_continuation,
            pre_tool_stop_reason,
        };
    }

    let choice = forced_choice.unwrap_or(PermissionPromptChoice::AllowOnce);
    // CC toolExecution.ts:1023-1068 consumes the resolving system decision's
    // message. No dialog ran on this branch; its separate continuation keeps
    // the user's response unchanged (PermissionContext.ts:154-172).
    let response =
        PermissionPromptResponse::new(choice).with_decision_message(request.message.clone());
    let result = streamed_check_permissions_and_call_tool_after_pre_tool_hooks_with_response(
        &request,
        &response,
        pre_tool_prevent_continuation,
        pre_tool_stop_reason.as_deref(),
        &context,
        Some(&assistant_message),
    )
    .await;
    context = result.new_context.clone();
    // Maps to: CC `StreamingToolExecutor.ts:379-381` —
    // `contextModifiers.push(update.contextModifier.modifyContext)` for the
    // tool's own `ToolResult.contextModifier`. The executor replays this list
    // onto `this.toolUseContext` at :391-395 once the tool completes, which is
    // how the effect reaches the tools started after this one. `context` above
    // already carries it: tool execution applied it to `new_context`.
    if let Some(modifier) = result.context_modifier {
        context_modifiers.push(modifier);
    }
    let modifier = ToolContextModifier::mark_complete(id.clone());
    context = modifier.modify_context(context);
    context_modifiers.push(modifier.clone());

    for hook_message in result.pre_tool_messages {
        results.push(MessageUpdate {
            message: Some(hook_message),
            model_message: None,
            progress: None,
            tool_result: None,
            new_context: context.clone(),
            permission_request: None,
            blocked_on_permission: false,
            forced_choice: None,
            context_modifier: None,
        });
    }
    for model_message in result.pre_tool_model_messages {
        results.push(MessageUpdate {
            message: None,
            model_message: Some(model_message),
            progress: None,
            tool_result: None,
            new_context: context.clone(),
            permission_request: None,
            blocked_on_permission: false,
            forced_choice: None,
            context_modifier: None,
        });
    }

    let update = MessageUpdate {
        message: result.message,
        model_message: None,
        progress: None,
        tool_result: result.tool_result,
        new_context: context.clone(),
        permission_request: None,
        blocked_on_permission: false,
        forced_choice: Some(choice),
        // The row keeps this port's mark-complete modifier. The tool's own
        // `ToolResult.contextModifier` (CC `toolExecution.ts:1465-1470`) is
        // already in `context_modifiers` above, which is the list
        // `apply_completion` replays onto the executor's context — CC's
        // `StreamingToolExecutor.ts:391-395`.
        context_modifier: Some(modifier),
    };
    let bash_error_description = (block.name == crate::tools::bash_tool::tool_name::BASH_TOOL_NAME
        && is_error_tool_result(update.tool_result.as_ref()))
    .then(|| StreamingToolExecutor::get_tool_description(&block));
    results.push(update);

    // Official yield order is primary tool_result, PostToolUse feedback, then
    // supplemental ToolResult.newMessages.
    for trailing_message in result.post_tool_messages {
        results.push(MessageUpdate {
            message: Some(trailing_message),
            model_message: None,
            progress: None,
            tool_result: None,
            new_context: context.clone(),
            permission_request: None,
            blocked_on_permission: false,
            forced_choice: None,
            context_modifier: None,
        });
    }
    for model_message in result.post_tool_model_messages {
        results.push(MessageUpdate {
            message: None,
            model_message: Some(model_message),
            progress: None,
            tool_result: None,
            new_context: context.clone(),
            permission_request: None,
            blocked_on_permission: false,
            forced_choice: None,
            context_modifier: None,
        });
    }

    for new_message in result.new_messages {
        results.push(MessageUpdate {
            message: None,
            model_message: Some(new_message),
            progress: None,
            tool_result: None,
            new_context: context.clone(),
            permission_request: None,
            blocked_on_permission: false,
            forced_choice: None,
            context_modifier: None,
        });
    }
    for model_message in result.continuation_messages {
        results.push(MessageUpdate {
            message: None,
            model_message: Some(model_message),
            progress: None,
            tool_result: None,
            new_context: context.clone(),
            permission_request: None,
            blocked_on_permission: false,
            forced_choice: None,
            context_modifier: None,
        });
    }

    ToolCompletion {
        id,
        results,
        context_modifiers,
        bash_error_description,
        pre_tool_prevent_continuation: false,
        pre_tool_stop_reason: None,
    }
}

async fn continue_after_permission_worker(
    block: ToolUseBlock,
    assistant_message: AssistantMessage,
    permission_request: PermissionRequest,
    pre_tool_prevent_continuation: bool,
    pre_tool_stop_reason: Option<String>,
    mut context: ToolUseContext,
    response: PermissionPromptResponse,
) -> ToolCompletion {
    let id = block.id.0.clone();
    let mut results = Vec::new();
    let mut context_modifiers = Vec::new();
    let choice = response.choice;
    let result = streamed_check_permissions_and_call_tool_after_pre_tool_hooks_with_response(
        &permission_request,
        &response,
        pre_tool_prevent_continuation,
        pre_tool_stop_reason.as_deref(),
        &context,
        Some(&assistant_message),
    )
    .await;
    context = result.new_context.clone();
    // Maps to: CC `StreamingToolExecutor.ts:379-381` —
    // `contextModifiers.push(update.contextModifier.modifyContext)` for the
    // tool's own `ToolResult.contextModifier`. The executor replays this list
    // onto `this.toolUseContext` at :391-395 once the tool completes, which is
    // how the effect reaches the tools started after this one. `context` above
    // already carries it: tool execution applied it to `new_context`.
    if let Some(modifier) = result.context_modifier {
        context_modifiers.push(modifier);
    }
    let modifier = ToolContextModifier::mark_complete(id.clone());
    context = modifier.modify_context(context);
    context_modifiers.push(modifier.clone());

    for hook_message in result.pre_tool_messages {
        results.push(MessageUpdate {
            message: Some(hook_message),
            model_message: None,
            progress: None,
            tool_result: None,
            new_context: context.clone(),
            permission_request: None,
            blocked_on_permission: false,
            forced_choice: None,
            context_modifier: None,
        });
    }
    for model_message in result.pre_tool_model_messages {
        results.push(MessageUpdate {
            message: None,
            model_message: Some(model_message),
            progress: None,
            tool_result: None,
            new_context: context.clone(),
            permission_request: None,
            blocked_on_permission: false,
            forced_choice: None,
            context_modifier: None,
        });
    }

    let update = MessageUpdate {
        message: result.message,
        model_message: None,
        progress: None,
        tool_result: result.tool_result,
        new_context: context.clone(),
        permission_request: None,
        blocked_on_permission: false,
        forced_choice: Some(choice),
        // The row keeps this port's mark-complete modifier. The tool's own
        // `ToolResult.contextModifier` (CC `toolExecution.ts:1465-1470`) is
        // already in `context_modifiers` above, which is the list
        // `apply_completion` replays onto the executor's context — CC's
        // `StreamingToolExecutor.ts:391-395`.
        context_modifier: Some(modifier),
    };
    let bash_error_description = (block.name == crate::tools::bash_tool::tool_name::BASH_TOOL_NAME
        && is_error_tool_result(update.tool_result.as_ref()))
    .then(|| StreamingToolExecutor::get_tool_description(&block));
    results.push(update);

    // Official yield order is primary tool_result, PostToolUse feedback, then
    // supplemental ToolResult.newMessages.
    for trailing_message in result.post_tool_messages {
        results.push(MessageUpdate {
            message: Some(trailing_message),
            model_message: None,
            progress: None,
            tool_result: None,
            new_context: context.clone(),
            permission_request: None,
            blocked_on_permission: false,
            forced_choice: None,
            context_modifier: None,
        });
    }
    for model_message in result.post_tool_model_messages {
        results.push(MessageUpdate {
            message: None,
            model_message: Some(model_message),
            progress: None,
            tool_result: None,
            new_context: context.clone(),
            permission_request: None,
            blocked_on_permission: false,
            forced_choice: None,
            context_modifier: None,
        });
    }

    for new_message in result.new_messages {
        results.push(MessageUpdate {
            message: None,
            model_message: Some(new_message),
            progress: None,
            tool_result: None,
            new_context: context.clone(),
            permission_request: None,
            blocked_on_permission: false,
            forced_choice: None,
            context_modifier: None,
        });
    }
    for model_message in result.continuation_messages {
        results.push(MessageUpdate {
            message: None,
            model_message: Some(model_message),
            progress: None,
            tool_result: None,
            new_context: context.clone(),
            permission_request: None,
            blocked_on_permission: false,
            forced_choice: None,
            context_modifier: None,
        });
    }

    ToolCompletion {
        id,
        results,
        context_modifiers,
        bash_error_description,
        pre_tool_prevent_continuation: false,
        pre_tool_stop_reason: None,
    }
}

/// Maps to: CC `services/tools/StreamingToolExecutor.ts#markToolUseAsComplete`.
fn mark_tool_use_as_complete(tool_use_context: &mut ToolUseContext, tool_use_id: &str) {
    tool_use_context.mark_complete(tool_use_id);
}

fn progress_update(
    progress: crate::types::tools::ToolProgress,
    context: &ToolUseContext,
) -> MessageUpdate {
    MessageUpdate {
        message: None,
        model_message: None,
        progress: Some(progress),
        tool_result: None,
        new_context: context.clone(),
        permission_request: None,
        blocked_on_permission: false,
        forced_choice: None,
        context_modifier: None,
    }
}

/// Maps to: CC `services/tools/StreamingToolExecutor.ts:153-209`
/// `StreamingToolExecutor.createSyntheticErrorMessage`.
fn create_synthetic_error_message(
    block: &ToolUseBlock,
    reason: String,
) -> crate::types::message::RenderableMessage {
    let is_user_rejection = reason.starts_with("User rejected");
    let tool_use_result = if is_user_rejection {
        "User rejected tool use".to_string()
    } else {
        crate::utils::messages::extract_tag(&reason, "tool_use_error")
            .unwrap_or_else(|| reason.clone())
    };
    let content = if is_user_rejection {
        "The user doesn't want to proceed with this tool use. The tool use was rejected (eg. if it was a file edit, the new_string was NOT written to the file). STOP what you are doing and wait for the user to tell you how to proceed.".to_string()
    } else if reason.starts_with("<tool_use_error>") {
        reason
    } else {
        format!("<tool_use_error>{reason}</tool_use_error>")
    };
    // CC's synthetic message is `createUserMessage` with a tool_result block,
    // `is_error: true` for rejection and error alike, and every branch records
    // its own string as `toolUseResult` regardless of tool
    // (`StreamingToolExecutor.ts:153-209`); reject renders from the sentinel
    // content, not a stored status.
    crate::types::message::RenderableMessage::user_tool_result(
        uuid::Uuid::new_v4().to_string(),
        block.id.0.clone(),
        content,
        true,
    )
    .with_tool_use_result(Some(serde_json::Value::String(tool_use_result)))
}

/// Rust worker transport for `create_synthetic_error_message`; it adds no
/// synthetic-result policy and only projects the same message into the typed
/// model/context update carried by the streaming executor.
fn synthetic_error_update(
    block: &ToolUseBlock,
    assistant_message: &AssistantMessage,
    context: &ToolUseContext,
    reason: String,
) -> MessageUpdate {
    let message = create_synthetic_error_message(block, reason);
    // The row no longer stores a tool name; the caller has the block in
    // hand, mirroring CC's per-tool model mapping call shape.
    let mut tool_result =
        crate::services::tools::tool_execution::transcript_tool_result_to_model_message(
            &message,
            &block.name,
        );
    // CC `StreamingToolExecutor.ts:171/186/203`: every synthetic tool_result
    // records `sourceToolAssistantUUID: assistantMessage.uuid`.
    if !assistant_message.uuid.is_empty() {
        if let Some(result) = tool_result.as_mut() {
            result.source_tool_assistant_uuid = Some(assistant_message.uuid.clone());
        }
    }
    MessageUpdate {
        message: Some(message),
        model_message: None,
        progress: None,
        tool_result,
        new_context: context.clone(),
        permission_request: None,
        blocked_on_permission: false,
        forced_choice: None,
        context_modifier: Some(ToolContextModifier::mark_complete(block.id.0.clone())),
    }
}

fn is_error_tool_result(user_message: Option<&UserMessage>) -> bool {
    user_message.is_some_and(|message| {
        message.content.iter().any(|content| match content {
            crate::types::message::UserContent::ToolResult(result) => result.is_error,
            _ => false,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::ToolPermissionContext;
    use crate::types::message::{AssistantContent, StopReason};
    use crate::utils::test_env::TEST_ENV_LOCK;

    fn assistant_with_block(block: ToolUseBlock) -> AssistantMessage {
        AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![AssistantContent::ToolUse(block)],
            model: None,
            stop_reason: Some(StopReason::ToolUse),
            usage: None,
        }
    }

    fn bash_block(id: &str, command: &str) -> ToolUseBlock {
        ToolUseBlock {
            id: crate::types::ids::ToolUseId(id.to_string()),
            name: "Bash".to_string(),
            input: serde_json::json!({ "command": command }),
        }
    }

    #[test]
    fn streaming_add_tool_classifies_semantically_parsed_input_like_official() {
        let context = ToolUseContext::with_permission_context(ToolPermissionContext::default());
        let mut executor = StreamingToolExecutor::new(context.tools.clone(), context);
        let valid = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu_stream_semantic_read".to_string()),
            name: "Read".to_string(),
            input: serde_json::json!({
                "file_path": "Cargo.toml",
                "offset": "0",
                "limit": "1"
            }),
        };
        executor.add_tool(valid.clone(), assistant_with_block(valid));
        assert!(executor.tools[0].is_concurrency_safe);

        let invalid = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu_stream_invalid_semantic_read".to_string()),
            name: "Read".to_string(),
            input: serde_json::json!({
                "file_path": "Cargo.toml",
                "offset": "1e3"
            }),
        };
        executor.add_tool(invalid.clone(), assistant_with_block(invalid));
        assert!(!executor.tools[1].is_concurrency_safe);
    }

    #[test]
    fn streaming_executor_add_tool_yields_permission_request_from_typed_block() {
        let block = bash_block("toolu_stream_bash", "printf streaming");
        let assistant = assistant_with_block(block.clone());
        let context = ToolUseContext::with_permission_context(ToolPermissionContext::default());
        let mut executor = StreamingToolExecutor::new(context.tools.clone(), context);

        executor.add_tool(block, assistant);
        let updates = executor.get_remaining_results();

        assert_eq!(updates.len(), 1);
        assert!(updates[0].blocked_on_permission);
        assert_eq!(
            updates[0]
                .permission_request
                .as_ref()
                .expect("permission request")
                .tool_use_id,
            "toolu_stream_bash"
        );
    }

    #[tokio::test]
    async fn streaming_executor_continues_permission_inside_executor() {
        let block = bash_block("toolu_stream_resume", "printf streaming-resume");
        let assistant = assistant_with_block(block.clone());
        let context = ToolUseContext::with_permission_context(ToolPermissionContext::default());
        let mut executor = StreamingToolExecutor::new(context.tools.clone(), context);

        executor.add_tool(block, assistant);
        let updates = executor.get_remaining_results();
        let permission_request = updates[0]
            .permission_request
            .clone()
            .expect("permission request");

        let updated_context = executor.get_updated_context();
        let continuation = executor
            .continue_after_permission(
                permission_request,
                PermissionPromptResponse::new(PermissionPromptChoice::AllowOnce),
                updated_context,
            )
            .await;

        assert!(
            continuation
                .iter()
                .any(|update| update.tool_result.is_some())
        );
        assert!(executor.get_remaining_results().is_empty());
        assert!(
            executor
                .get_updated_context()
                .in_progress_tool_use_ids
                .is_empty()
        );
    }

    #[test]
    fn streaming_executor_discard_drops_buffered_results() {
        let block = bash_block("toolu_stream_discard", "echo discarded");
        let assistant = assistant_with_block(block.clone());
        let context = ToolUseContext::with_permission_context(ToolPermissionContext::default());
        let mut executor = StreamingToolExecutor::new(context.tools.clone(), context);

        executor.add_tool(block, assistant);
        executor.discard();

        assert!(executor.get_completed_results().is_empty());
        assert!(executor.get_remaining_results().is_empty());
    }

    #[test]
    fn streaming_executor_unknown_tool_yields_error_tool_result() {
        let block = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu_stream_unknown".to_string()),
            name: "DefinitelyMissingTool".to_string(),
            input: serde_json::json!({"value": 1}),
        };
        let assistant = assistant_with_block(block.clone());
        let context = ToolUseContext::with_permission_context(ToolPermissionContext::default());
        let mut executor = StreamingToolExecutor::new(Vec::new(), context);

        executor.add_tool(block, assistant);
        let updates = executor.get_remaining_results();

        assert_eq!(updates.len(), 1);
        assert!(!updates[0].blocked_on_permission);
        let request: Option<&PermissionRequest> = updates[0].permission_request.as_ref();
        assert!(request.is_none());
        let tool_result = updates[0].tool_result.as_ref().expect("tool_result");
        match &tool_result.content[0] {
            crate::types::message::UserContent::ToolResult(result) => {
                assert_eq!(result.tool_use_id.0, "toolu_stream_unknown");
                assert!(result.is_error);
                assert!(
                    result
                        .content
                        .contains("No such tool available: DefinitelyMissingTool")
                );
            }
            other => panic!("unexpected content: {other:?}"),
        }
    }

    #[test]
    fn streaming_executor_runs_preallowed_read_tool_to_tool_result() {
        let block = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu_stream_read".to_string()),
            name: "Read".to_string(),
            input: serde_json::json!({"file_path": "Cargo.toml"}),
        };
        let assistant = assistant_with_block(block.clone());
        let context = ToolUseContext::with_permission_context(ToolPermissionContext::default());
        let mut executor = StreamingToolExecutor::new(context.tools.clone(), context);

        executor.add_tool(block, assistant);
        let updates = executor.get_remaining_results();

        assert!(updates.iter().any(|update| update.tool_result.is_some()));
        assert!(updates.iter().all(|update| !update.blocked_on_permission));
        assert!(
            executor
                .get_updated_context()
                .in_progress_tool_use_ids
                .is_empty()
        );
    }

    #[test]
    fn sync_tool_use_context_preserves_live_read_handle_identities() {
        let context = ToolUseContext::default();
        let live_cache = context.read_file_state.clone();
        let live_nested = context
            .nested_memory_attachment_triggers
            .as_ref()
            .unwrap()
            .clone();
        let live_dynamic = context.dynamic_skill_dir_triggers.as_ref().unwrap().clone();
        let mut executor = StreamingToolExecutor::new(context.tools.clone(), context);

        live_nested.add("nested-live".to_string());
        live_dynamic.add("dynamic-live".to_string());
        let stale = ToolUseContext::default();
        executor.sync_tool_use_context(stale);
        let synced = executor.get_updated_context();

        assert!(synced.read_file_state.same_identity(&live_cache));
        assert!(
            synced
                .nested_memory_attachment_triggers
                .as_ref()
                .unwrap()
                .same_identity(&live_nested)
        );
        assert!(
            synced
                .dynamic_skill_dir_triggers
                .as_ref()
                .unwrap()
                .same_identity(&live_dynamic)
        );
        assert!(
            synced
                .nested_memory_attachment_triggers
                .as_ref()
                .unwrap()
                .contains("nested-live")
        );
        assert!(
            synced
                .dynamic_skill_dir_triggers
                .as_ref()
                .unwrap()
                .contains("dynamic-live")
        );
    }

    #[test]
    fn concurrent_reads_merge_state_nested_and_dynamic_skill_effects() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        crate::skills::load_skills_dir::clear_dynamic_skills();
        struct DynamicSkillsRestore;
        impl Drop for DynamicSkillsRestore {
            fn drop(&mut self) {
                crate::skills::load_skills_dir::clear_dynamic_skills();
            }
        }
        let _dynamic_skills_restore = DynamicSkillsRestore;
        let root = std::env::temp_dir().join(format!(
            "cometix-streaming-read-effects-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let mut blocks = Vec::new();
        for (index, branch) in ["a", "b"].into_iter().enumerate() {
            let branch_root = root.join(branch);
            let skill_dir = branch_root.join(".claude/skills");
            let source = branch_root.join("src/value.txt");
            std::fs::create_dir_all(skill_dir.join(format!("skill-{branch}"))).unwrap();
            std::fs::create_dir_all(source.parent().unwrap()).unwrap();
            std::fs::write(
                skill_dir.join(format!("skill-{branch}/SKILL.md")),
                format!("# Skill {branch}"),
            )
            .unwrap();
            std::fs::write(&source, format!("value-{branch}\n")).unwrap();
            blocks.push(ToolUseBlock {
                id: crate::types::ids::ToolUseId(format!("toolu-read-{index}")),
                name: "Read".to_string(),
                input: serde_json::json!({"file_path": source.display().to_string()}),
            });
        }
        let mut context = ToolUseContext {
            cwd_override: Some(root.clone()),
            ..ToolUseContext::default()
        };
        // CC filesystem.ts allWorkingDirectories: cwd override alone grants no access.
        // This concurrency fixture explicitly authorizes the temporary workspace.
        context.tool_permission_context =
            crate::utils::permissions::permission_update::apply_permission_update(
                &context.tool_permission_context,
                &crate::types::permissions::PermissionUpdate::AddDirectories {
                    directories: vec![root.to_string_lossy().into_owned()],
                    destination: crate::types::permissions::PermissionUpdateDestination::Session,
                },
            );
        context.tools = crate::tools::get_tools(&context.tool_permission_context);
        let mut executor = StreamingToolExecutor::new(context.tools.clone(), context);
        for block in blocks {
            executor.add_tool(block.clone(), assistant_with_block(block));
        }
        let updates = executor.get_remaining_results();
        assert_eq!(
            updates
                .iter()
                .filter(|update| update.tool_result.is_some())
                .count(),
            2
        );
        let merged = executor.get_updated_context();
        assert_eq!(merged.read_file_state.len(), 2);
        assert_eq!(
            merged
                .nested_memory_attachment_triggers
                .as_ref()
                .map(crate::tool::SharedOrderedTriggerSet::len),
            Some(2)
        );
        assert_eq!(
            merged
                .dynamic_skill_dir_triggers
                .as_ref()
                .map(crate::tool::SharedOrderedTriggerSet::len),
            Some(2)
        );
        assert!(
            updates
                .iter()
                .any(|update| update.new_context.read_file_state.len() == 2)
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn streaming_executor_aborted_context_yields_synthetic_tool_result() {
        let block = bash_block("toolu_stream_abort", "echo interrupted");
        let assistant = assistant_with_block(block.clone());
        let context = ToolUseContext::with_permission_context(ToolPermissionContext::default());
        context.abort_controller.abort();
        let mut executor = StreamingToolExecutor::new(context.tools.clone(), context);

        executor.add_tool(block, assistant);
        let updates = executor.get_remaining_results();

        assert_eq!(updates.len(), 1);
        assert!(!updates[0].blocked_on_permission);
        assert!(updates[0].permission_request.is_none());
        let tool_result = updates[0].tool_result.as_ref().expect("tool_result");
        match &tool_result.content[0] {
            crate::types::message::UserContent::ToolResult(result) => {
                assert_eq!(result.tool_use_id.0, "toolu_stream_abort");
                assert!(result.is_error);
                assert!(result.content.contains("doesn't want to proceed"));
            }
            other => panic!("unexpected content: {other:?}"),
        }
    }

    #[test]
    fn streaming_read_synthetic_raw_tool_use_result_matches_official() {
        let block = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu_stream_read_abort".to_string()),
            name: "Read".to_string(),
            input: serde_json::json!({"file_path": "src/main.rs"}),
        };
        let assistant = assistant_with_block(block.clone());
        let context = ToolUseContext::with_permission_context(ToolPermissionContext::default());
        context.abort_controller.abort();
        let mut executor = StreamingToolExecutor::new(context.tools.clone(), context);

        executor.add_tool(block, assistant);
        let updates = executor.get_remaining_results();

        assert_eq!(updates.len(), 1);
        let tool_result = updates[0].tool_result.as_ref().expect("tool_result");
        assert!(matches!(
            tool_result.content.first(),
            Some(crate::types::message::UserContent::ToolResult(result))
                if result.tool_use_result == Some(serde_json::json!("User rejected tool use"))
        ));
        // The synthetic Read error records its raw string on the row.
        assert!(matches!(
            updates[0].message.as_ref().map(|message| &message.kind),
            Some(crate::types::message::RenderableMessageKind::User { message }) if matches!(
                message.first_content_block(),
                Some(crate::types::message::UserContent::ToolResult(result))
                    if result.tool_use_result
                        == Some(serde_json::json!("User rejected tool use"))
            )
        ));
    }

    #[test]
    fn streaming_executor_applies_always_allow_rule_without_prompting() {
        let block = bash_block("toolu_stream_bash_allowed", "printf streaming-allowed");
        let assistant = assistant_with_block(block.clone());
        let mut permission_context = ToolPermissionContext::default();
        permission_context.always_allow_rules.insert(
            crate::types::permissions::PermissionRuleSource::Session,
            vec![crate::types::permissions::PermissionRuleValue::new(
                "Bash",
                Some("printf streaming-allowed".to_string()),
            )],
        );
        let context = ToolUseContext::with_permission_context(permission_context);
        let mut executor = StreamingToolExecutor::new(context.tools.clone(), context);

        executor.add_tool(block, assistant);
        let updates = executor.get_remaining_results();

        assert!(updates.iter().all(|update| !update.blocked_on_permission));
        assert!(updates.iter().any(|update| update.tool_result.is_some()));
    }

    #[test]
    fn streaming_executor_replays_skill_context_modifier_onto_the_shared_context() {
        // Maps to: CC `StreamingToolExecutor.ts:379-395` — the tool's own
        // `contextModifier` is collected while its updates drain and then
        // applied to `this.toolUseContext`, so `getUpdatedContext()` (which
        // `query.rs` adopts) carries it into every tool started afterwards.
        //
        // Old shape: `SkillTool` produced no modifier, so the executor's
        // context came out of a skill call byte-identical and the Bash
        // assertion below stayed `true`.
        use std::io::Write;

        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-streaming-skill-modifier-{}",
            uuid::Uuid::new_v4()
        ));
        let skill_file = root
            .join(".claude")
            .join("skills")
            .join("ship")
            .join("SKILL.md");
        std::fs::create_dir_all(skill_file.parent().unwrap()).unwrap();
        let mut file = std::fs::File::create(&skill_file).unwrap();
        file.write_all(
            b"---\ndescription: Ship\nallowed-tools: Bash(printf *)\nmodel: opus\n---\nShip body",
        )
        .unwrap();
        let old_cwd = crate::bootstrap::state::get_original_cwd();
        crate::bootstrap::state::set_original_cwd(&root);

        let block = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu_stream_skill".to_string()),
            name: "Skill".to_string(),
            input: serde_json::json!({"skill": "ship"}),
        };
        let assistant = assistant_with_block(block.clone());
        let mut permission_context = ToolPermissionContext::default();
        permission_context.always_allow_rules.insert(
            crate::types::permissions::PermissionRuleSource::Session,
            vec![crate::types::permissions::PermissionRuleValue::new(
                "Skill", None,
            )],
        );
        let mut context = ToolUseContext::with_permission_context(permission_context);
        context.main_loop_model = Some("claude-sonnet-4-5".to_string());

        let later_bash = crate::utils::permissions::permissions::mock_permission_request_with_input(
            "perm-later-bash".to_string(),
            "toolu_later_bash".to_string(),
            crate::tools::bash_tool::tool_name::BASH_TOOL_NAME.to_string(),
            "printf hi".to_string(),
            serde_json::json!({"command": "printf hi"}),
            crate::types::permissions::PermissionMode::Default,
        );
        assert!(
            crate::services::tools::tool_execution::should_ask_permission_request(
                &later_bash,
                &context
            ),
            "pre-skill executor context must still ask for Bash(printf ...)"
        );

        let mut executor = StreamingToolExecutor::new(context.tools.clone(), context);
        executor.add_tool(block, assistant);
        let updates = executor.get_remaining_results();
        let updated = executor.get_updated_context();

        crate::bootstrap::state::set_original_cwd(old_cwd);
        let _ = std::fs::remove_dir_all(&root);

        assert!(updates.iter().all(|update| !update.blocked_on_permission));
        assert!(
            !crate::services::tools::tool_execution::should_ask_permission_request(
                &later_bash,
                &updated
            ),
            "the skill's allowed-tools must widen tools started after it in the turn"
        );
        assert_eq!(updated.main_loop_model.as_deref(), Some("opus"));
    }

    #[test]
    fn streaming_executor_buffers_running_progress_updates() {
        let command = "for i in 1 2 3; do echo progress-$i; sleep 1.2; done";
        let block = bash_block("toolu_stream_bash_progress", command);
        let assistant = assistant_with_block(block.clone());
        let mut permission_context = ToolPermissionContext::default();
        permission_context.always_allow_rules.insert(
            crate::types::permissions::PermissionRuleSource::Session,
            vec![crate::types::permissions::PermissionRuleValue::new(
                "Bash", None,
            )],
        );
        let context = ToolUseContext::with_permission_context(permission_context);
        let mut executor = StreamingToolExecutor::new(context.tools.clone(), context);

        executor.add_tool(block, assistant);
        let updates = executor.get_remaining_results();

        assert!(updates.iter().any(|update| matches!(
            update.progress,
            Some(crate::types::tools::ToolProgress::BashProgress { ref tool_use_id, .. })
                if tool_use_id.0 == "toolu_stream_bash_progress"
        )));
        assert!(updates.iter().any(|update| update.tool_result.is_some()));
    }

    #[test]
    fn streaming_executor_aborts_running_sibling_after_bash_error() {
        let first_command = "sleep 0.08; exit 2";
        let second_command = "sleep 10";
        let first_block = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu_stream_bash_timeout".to_string()),
            name: "Bash".to_string(),
            input: serde_json::json!({ "command": first_command }),
        };
        let second_block = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu_stream_bash_sibling".to_string()),
            name: "Bash".to_string(),
            input: serde_json::json!({ "command": second_command }),
        };
        let mut permission_context = ToolPermissionContext::default();
        permission_context.always_allow_rules.insert(
            crate::types::permissions::PermissionRuleSource::Session,
            vec![crate::types::permissions::PermissionRuleValue::new(
                "Bash", None,
            )],
        );
        let context = ToolUseContext::with_permission_context(permission_context);
        let mut executor = StreamingToolExecutor::new(context.tools.clone(), context);

        let started = std::time::Instant::now();
        executor.add_tool(first_block.clone(), assistant_with_block(first_block));
        executor.add_tool(second_block.clone(), assistant_with_block(second_block));
        let updates = executor.get_remaining_results();

        assert!(
            started.elapsed() < std::time::Duration::from_millis(1_200),
            "sibling command should be aborted before its own timeout"
        );
        let tool_result_contents = updates
            .iter()
            .filter_map(|update| update.tool_result.as_ref())
            .flat_map(|message| message.content.iter())
            .filter_map(|content| match content {
                crate::types::message::UserContent::ToolResult(result) => {
                    Some(result.content.clone())
                }
                _ => None,
            })
            .collect::<Vec<_>>();

        assert!(
            tool_result_contents
                .iter()
                .any(|content| content.contains("Exit code 2")),
            "tool results: {tool_result_contents:#?}"
        );
        assert!(
            tool_result_contents
                .iter()
                .any(|content| content.contains("Cancelled: parallel tool call Bash(")),
            "tool results: {tool_result_contents:#?}"
        );
    }
}

#[cfg(test)]
pub(crate) mod streaming_hook_decision_tests {
    //! Real streaming-worker boundary for CC's resolving PermissionRequest deny.
    use super::*;
    use crate::services::hooks::{
        HookCallback, RegisteredHook, RegisteredHookMatcher, RegisteredHooks,
    };
    use crate::types::message::{AssistantContent, UserContent};
    use crate::types::permissions::{PermissionRuleSource, PermissionRuleValue};
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};
    use serde_json::json;
    use std::sync::Arc;

    struct RegisteredHooksGuard(Option<RegisteredHooks>);

    impl Drop for RegisteredHooksGuard {
        fn drop(&mut self) {
            crate::bootstrap::state::replace_registered_hooks(self.0.take().unwrap_or_default());
        }
    }

    /// Test-only fixture shared with the serial query actor regression. Both
    /// exercise the production settings writer in an isolated localSettings root.
    pub(crate) struct PermissionRequestFixture {
        pub(crate) root: std::path::PathBuf,
        pub(crate) target: std::path::PathBuf,
        original_cwd: std::path::PathBuf,
        _config: EnvVarGuard,
        _writes: EnvVarGuard,
        _simple: EnvVarGuard,
        _managed: crate::services::hooks::test_support::ManagedSettingsGuard,
        _registered: RegisteredHooksGuard,
    }

    impl PermissionRequestFixture {
        pub(crate) fn new(writes: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("cc-pr-resolution-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(root.join("config")).unwrap();
            let root = root.canonicalize().unwrap();
            std::fs::create_dir_all(root.join("workspace")).unwrap();
            let original_cwd = crate::bootstrap::state::get_original_cwd();
            crate::bootstrap::state::set_original_cwd(root.join("workspace"));
            let target = root.join("workspace/authorized.txt");
            std::fs::write(&target, "PermissionRequest authorized exact content\n").unwrap();
            std::fs::write(
                root.join("workspace/original.txt"),
                "must not read original\n",
            )
            .unwrap();
            Self {
                _config: EnvVarGuard::set("CLAUDE_CONFIG_DIR", root.join("config")),
                _writes: EnvVarGuard::set("COMETIX_WRITE_ENABLED", writes),
                _simple: EnvVarGuard::unset("CLAUDE_CODE_SIMPLE"),
                _managed: crate::services::hooks::test_support::ManagedSettingsGuard::install(
                    Some(r#"{"allowManagedHooksOnly":true}"#),
                ),
                _registered: RegisteredHooksGuard(crate::bootstrap::state::get_registered_hooks()),
                root,
                target,
                original_cwd,
            }
        }

        pub(crate) fn context(&self) -> ToolUseContext {
            let mut permissions = crate::tool::ToolPermissionContext::default();
            permissions.always_ask_rules.insert(
                PermissionRuleSource::Session,
                vec![PermissionRuleValue::new("Read", None)],
            );
            let mut context = ToolUseContext::with_permission_context(permissions);
            context.cwd_override = Some(self.root.join("workspace"));
            context.tools = vec![crate::tools::file_read_tool::file_read_tool_schema()];
            context
        }

        pub(crate) fn block(&self) -> ToolUseBlock {
            ToolUseBlock {
                id: crate::types::ids::ToolUseId("toolu_pr_resolved".into()),
                name: "Read".into(),
                input: json!({"file_path": self.root.join("workspace/original.txt")}),
            }
        }

        pub(crate) fn local_settings(&self) -> std::path::PathBuf {
            self.root.join("workspace/.claude/settings.local.json")
        }

        pub(crate) fn install_hooks(&self, decide: bool) {
            let (sender, receiver) = async_channel::bounded::<()>(1);
            let target = self.target.clone();
            let winner = RegisteredHook::Callback(HookCallback {
                callback: Arc::new(move |_, _| {
                    let sender = sender.clone();
                    let target = target.clone();
                    Box::pin(async move {
                        sender.send(()).await.unwrap();
                        if !decide {
                            return json!({});
                        }
                        json!({"hookSpecificOutput": {"hookEventName": "PermissionRequest", "decision": {
                            "behavior": "allow", "updatedInput": {"file_path": target},
                            "updatedPermissions": [{"type":"addRules", "destination":"localSettings",
                                "behavior":"allow", "rules":[{"toolName":"Read"}]}]
                        }}})
                    })
                }),
                timeout: Some(3),
            });
            let loser = RegisteredHook::Callback(HookCallback {
                callback: Arc::new(move |_, _| {
                    let receiver = receiver.clone();
                    Box::pin(async move {
                        tokio::time::timeout(std::time::Duration::from_secs(2), receiver.recv())
                            .await
                            .unwrap()
                            .unwrap();
                        if !decide {
                            return json!({});
                        }
                        json!({"hookSpecificOutput": {"hookEventName":"PermissionRequest", "decision": {
                            "behavior":"deny", "message":"losing deny", "interrupt":true
                        }}})
                    })
                }),
                timeout: Some(3),
            });
            let pre_ask = RegisteredHook::Callback(HookCallback {
                callback: Arc::new(|_, _| {
                    Box::pin(async {
                        json!({"hookSpecificOutput": {
                            "hookEventName":"PreToolUse", "permissionDecision":"ask",
                            "permissionDecisionReason":"pre-tool ask must be resolved"
                        }})
                    })
                }),
                timeout: Some(3),
            });
            crate::bootstrap::state::replace_registered_hooks(std::collections::HashMap::from([
                (
                    "PermissionRequest".into(),
                    vec![RegisteredHookMatcher {
                        matcher: Some("Read".into()),
                        hooks: vec![loser, winner],
                        ..Default::default()
                    }],
                ),
                (
                    "PreToolUse".into(),
                    vec![RegisteredHookMatcher {
                        matcher: Some("Read".into()),
                        hooks: vec![pre_ask],
                        ..Default::default()
                    }],
                ),
            ]));
        }
    }

    impl Drop for PermissionRequestFixture {
        fn drop(&mut self) {
            crate::bootstrap::state::set_original_cwd(&self.original_cwd);
            crate::utils::settings::settings_cache::reset_settings_cache();
            crate::utils::hooks::hooks_config_snapshot::reset_hooks_config_snapshot();
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// CC PermissionContext.handleHookAllow resolves whole-tool Ask rules;
    /// persistPermissions ignores ordinary disk errors but publishes the grant.
    #[tokio::test]
    async fn streaming_permission_request_hook_resolution_and_persistence_matches_official() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _trust = crate::services::hooks::test_support::SessionTrustGuard::accepted();
        for (decide, writes, ordinary_io_failure) in [
            (true, "1", false),
            (true, "1", true),
            (true, "0", false),
            (false, "1", false),
        ] {
            let fixture = PermissionRequestFixture::new(writes);
            fixture.install_hooks(decide);
            if ordinary_io_failure {
                // A directory where the canonical writer expects a file is a
                // real ordinary I/O failure, unrelated to the explicit no-write gate.
                std::fs::create_dir_all(fixture.local_settings()).unwrap();
            }
            let context = fixture.context();
            let block = fixture.block();
            let assistant = AssistantMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![AssistantContent::ToolUse(block.clone())],
                model: None,
                stop_reason: None,
                usage: None,
            };
            let completion = execute_tool_worker(block, assistant, context.clone()).await;
            assert!(
                !context.abort_controller.is_aborted(),
                "the losing deny cannot interrupt"
            );
            let final_context = &completion
                .results
                .last()
                .expect("worker completion")
                .new_context;
            assert!(!final_context.abort_controller.is_aborted());
            assert!(
                final_context
                    .tool_permission_context
                    .always_ask_rules
                    .get(&PermissionRuleSource::Session)
                    .is_some_and(|rules| rules.contains(&PermissionRuleValue::new("Read", None))),
                "the whole-tool Ask remains; resolving this request does not remove it"
            );
            if !decide {
                assert!(
                    completion
                        .results
                        .iter()
                        .any(|update| update.blocked_on_permission
                            && update.permission_request.is_some())
                );
                assert!(
                    completion
                        .results
                        .iter()
                        .all(|update| update.tool_result.is_none())
                );
                assert!(!fixture.local_settings().exists());
                continue;
            }
            assert!(
                completion
                    .results
                    .iter()
                    .all(|update| !update.blocked_on_permission),
                "a PermissionRequest decision resolves both the rule Ask and earlier PreToolUse Ask"
            );
            let result = completion
                .results
                .iter()
                .filter_map(|update| update.tool_result.as_ref())
                .flat_map(|message| &message.content)
                .find_map(|content| match content {
                    UserContent::ToolResult(block) => Some(block),
                    _ => None,
                })
                .expect("actual model tool_result");
            if writes == "0" {
                assert!(result.is_error);
                assert_eq!(
                    result.content,
                    crate::tools::shared::write_gate::PERMISSION_PERSISTENCE_DISABLED_ERROR
                );
                assert!(!fixture.local_settings().exists());
                assert!(
                    final_context
                        .tool_permission_context
                        .always_allow_rules
                        .is_empty()
                );
                assert!(final_context.read_file_state.get(&fixture.target).is_none());
                continue;
            }
            assert!(
                !result.is_error,
                "actual Read should run: {}",
                result.content
            );
            assert!(
                result
                    .content
                    .contains("PermissionRequest authorized exact content")
            );
            assert!(!result.content.contains("must not read original"));
            let raw = result.tool_use_result.as_ref().expect("Read raw result");
            assert_eq!(raw["file"]["filePath"], json!(fixture.target));
            assert!(completion.results.iter().any(|update| matches!(
                update.model_message.as_ref(), Some(crate::types::message::Message::Attachment(attachment))
                if attachment.attachment == crate::types::message::Attachment::HookPermissionDecision {
                    decision: "allow".into(), tool_use_id: "toolu_pr_resolved".into(), hook_event: "PermissionRequest".into(),
                }
            )), "the final decision retains its PermissionRequest Hook reason");
            assert_eq!(
                final_context
                    .tool_permission_context
                    .always_allow_rules
                    .get(&PermissionRuleSource::LocalSettings),
                Some(&vec![PermissionRuleValue::new("Read", None)])
            );
            if ordinary_io_failure {
                assert!(fixture.local_settings().is_dir());
            } else {
                let persisted: serde_json::Value = serde_json::from_str(
                    &std::fs::read_to_string(fixture.local_settings()).unwrap(),
                )
                .unwrap();
                assert_eq!(persisted["permissions"]["allow"], json!(["Read"]));
            }
        }
    }

    #[tokio::test]
    async fn streaming_permission_request_hook_deny_model_result_matches_official() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _trust = crate::services::hooks::test_support::SessionTrustGuard::accepted();
        let _simple = EnvVarGuard::unset("CLAUDE_CODE_SIMPLE");
        let _managed = crate::services::hooks::test_support::ManagedSettingsGuard::install(Some(
            r#"{"allowManagedHooksOnly":true}"#,
        ));
        let _registered = RegisteredHooksGuard(crate::bootstrap::state::get_registered_hooks());
        for reason in [Some("streaming policy denied"), Some(""), None] {
            let mut decision = json!({"behavior": "deny"});
            if let Some(reason) = reason {
                decision["message"] = json!(reason);
            }
            // SDK callbacks enter the production registered-hook loader. No shell
            // subprocess or test-only substitute for execute_tool_worker is used.
            crate::bootstrap::state::replace_registered_hooks(std::collections::HashMap::from([(
                "PermissionRequest".into(),
                vec![RegisteredHookMatcher {
                    matcher: Some("Read".into()),
                    hooks: vec![RegisteredHook::Callback(HookCallback {
                        callback: Arc::new(move |_, _| {
                            let decision = decision.clone();
                            Box::pin(async move {
                                json!({"hookSpecificOutput": {
                                    "hookEventName": "PermissionRequest", "decision": decision
                                }})
                            })
                        }),
                        timeout: Some(3),
                    })],
                    ..Default::default()
                }],
            )]));
            let mut permission_context = crate::tool::ToolPermissionContext::default();
            permission_context.always_ask_rules.insert(
                PermissionRuleSource::Session,
                vec![PermissionRuleValue::new("Read", None)],
            );
            let mut context = ToolUseContext::with_permission_context(permission_context);
            context.tools = vec![crate::tools::file_read_tool::file_read_tool_schema()];
            let block = ToolUseBlock {
                id: crate::types::ids::ToolUseId("toolu_stream_hook_deny".into()),
                name: "Read".into(),
                input: json!({"file_path": "/tmp/stream-hook-denied"}),
            };
            let assistant = AssistantMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![AssistantContent::ToolUse(block.clone())],
                model: None,
                stop_reason: None,
                usage: None,
            };
            let completion = execute_tool_worker(block, assistant, context.clone()).await;
            assert!(
                completion
                    .results
                    .iter()
                    .all(|update| !update.blocked_on_permission),
                "the hook resolved the ask without opening a dialog"
            );
            let result = completion
                .results
                .iter()
                .filter_map(|update| update.tool_result.as_ref())
                .flat_map(|message| &message.content)
                .find_map(|content| match content {
                    UserContent::ToolResult(block) => Some(block),
                    _ => None,
                })
                .expect("the real worker must emit a model tool_result");
            let expected = reason
                .filter(|reason| !reason.is_empty())
                .unwrap_or("Permission denied by hook");
            assert_eq!(result.content, expected);
            assert!(result.is_error);
            assert!(
                !context.abort_controller.is_aborted(),
                "system deny is not dialog cancelAndAbort"
            );
        }
    }

    /// CC hooks.ts:2862-2866 supplies each completion's reason with the sticky
    /// behavior; toolHooks.ts:535-556 replaces the whole decision on each yield.
    #[tokio::test]
    async fn streaming_pre_tool_hook_aggregated_deny_model_result_matches_official() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _trust = crate::services::hooks::test_support::SessionTrustGuard::accepted();
        let _simple = EnvVarGuard::unset("CLAUDE_CODE_SIMPLE");
        let _managed = crate::services::hooks::test_support::ManagedSettingsGuard::install(Some(
            r#"{"allowManagedHooksOnly":true}"#,
        ));
        let _registered = RegisteredHooksGuard(crate::bootstrap::state::get_registered_hooks());
        for later_reason in [None, Some("late allow explanation")] {
            let (sender, receiver) = async_channel::bounded::<()>(1);
            let deny = RegisteredHook::Callback(HookCallback {
                callback: Arc::new(move |_, _| {
                    let sender = sender.clone();
                    Box::pin(async move {
                        sender.send(()).await.unwrap();
                        json!({"hookSpecificOutput": {"hookEventName": "PreToolUse",
                            "permissionDecision": "deny", "permissionDecisionReason": "first deny"}})
                    })
                }),
                timeout: Some(3),
            });
            let allow = RegisteredHook::Callback(HookCallback {
                callback: Arc::new(move |_, _| {
                    let receiver = receiver.clone();
                    Box::pin(async move {
                        tokio::time::timeout(std::time::Duration::from_secs(2), receiver.recv())
                            .await
                            .unwrap()
                            .unwrap();
                        let mut output = json!({"hookSpecificOutput": {
                            "hookEventName": "PreToolUse", "permissionDecision": "allow"
                        }});
                        if let Some(reason) = later_reason {
                            output["hookSpecificOutput"]["permissionDecisionReason"] =
                                json!(reason);
                        }
                        output
                    })
                }),
                timeout: Some(3),
            });
            crate::bootstrap::state::replace_registered_hooks(std::collections::HashMap::from([(
                "PreToolUse".into(),
                vec![RegisteredHookMatcher {
                    matcher: Some("Read".into()),
                    hooks: vec![allow, deny],
                    ..Default::default()
                }],
            )]));
            let mut permission_context = crate::tool::ToolPermissionContext::default();
            permission_context.always_allow_rules.insert(
                PermissionRuleSource::Session,
                vec![PermissionRuleValue::new("Read", None)],
            );
            let mut context = ToolUseContext::with_permission_context(permission_context);
            context.tools = vec![crate::tools::file_read_tool::file_read_tool_schema()];
            let block = ToolUseBlock {
                id: crate::types::ids::ToolUseId("toolu_stream_pre_deny".into()),
                name: "Read".into(),
                input: json!({"file_path": "/tmp/stream-pre-hook-denied"}),
            };
            let assistant = AssistantMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![AssistantContent::ToolUse(block.clone())],
                model: None,
                stop_reason: None,
                usage: None,
            };
            let gate = run_tool_use(&block, &assistant, &context, &mut Vec::new());
            // Typed runToolUse defers permission resolution until after hooks
            // (toolExecution.ts:921; this port's gate passes evaluate=false).
            // Its blocked flag is pending resolution, not an Ask decision.
            assert!(
                gate.blocked_on_permission,
                "typed gate defers the final permission decision"
            );
            let request = gate.request.expect("parsed deferred request");
            assert_eq!(request.message, "");
            // CC toolHooks.ts:416-436: without a hook decision the canonical
            // canUseTool path resolves this same input. Prove the fixture's
            // initial Allow instead of inferring it from the deferred flag.
            let baseline =
                crate::services::tools::tool_execution::apply_required_can_use_tool_after_hooks(
                    request,
                    &context,
                    Some(&assistant),
                    None,
                    false,
                )
                .await
                .expect("permission baseline must not reject");
            assert_eq!(
                baseline.forced_choice,
                Some(PermissionPromptChoice::AllowOnce)
            );
            assert!(!baseline.force_ask);
            assert_eq!(baseline.request.message, "");
            let completion = execute_tool_worker(block, assistant, context.clone()).await;
            assert!(
                completion
                    .results
                    .iter()
                    .all(|update| !update.blocked_on_permission)
            );
            let result = completion
                .results
                .iter()
                .filter_map(|update| update.tool_result.as_ref())
                .flat_map(|message| &message.content)
                .find_map(|content| match content {
                    UserContent::ToolResult(block) => Some(block),
                    _ => None,
                })
                .expect("real worker denied model result");
            assert_eq!(
                result.content,
                later_reason.unwrap_or("Hook PreToolUse:Read denied this tool")
            );
            assert!(result.is_error);
            assert!(
                !context.abort_controller.is_aborted(),
                "denied hook must not abort as a user rejection"
            );
            assert!(
                completion
                    .results
                    .iter()
                    .all(|update| !update.new_context.abort_controller.is_aborted())
            );
        }
    }
    #[tokio::test]
    async fn streaming_hook_ask_final_callback_deny_matches_official_no_second_prompt() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _trust = crate::services::hooks::test_support::SessionTrustGuard::accepted();
        let _simple = EnvVarGuard::unset("CLAUDE_CODE_SIMPLE");
        let _managed = crate::services::hooks::test_support::ManagedSettingsGuard::install(Some(
            r#"{"allowManagedHooksOnly":true}"#,
        ));
        let _registered = RegisteredHooksGuard(crate::bootstrap::state::get_registered_hooks());
        let prompt_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let prompt_counter = prompt_calls.clone();
        crate::bootstrap::state::replace_registered_hooks(std::collections::HashMap::from([
            (
                "PreToolUse".into(),
                vec![RegisteredHookMatcher {
                    matcher: Some("Read".into()),
                    hooks: vec![RegisteredHook::Callback(HookCallback {
                        callback: Arc::new(|_, _| {
                            Box::pin(async {
                                json!({"hookSpecificOutput":{
                        "hookEventName":"PreToolUse", "permissionDecision":"ask", "permissionDecisionReason":"review"}})
                            })
                        }),
                        timeout: Some(3),
                    })],
                    ..Default::default()
                }],
            ),
            (
                "PermissionRequest".into(),
                vec![RegisteredHookMatcher {
                    matcher: Some("Read".into()),
                    hooks: vec![RegisteredHook::Callback(HookCallback {
                        callback: Arc::new(move |_, _| {
                            let counter = prompt_counter.clone();
                            Box::pin(async move {
                                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                                json!({})
                            })
                        }),
                        timeout: Some(3),
                    })],
                    ..Default::default()
                }],
            ),
        ]));
        let mut context = ToolUseContext::default();
        context.tools = vec![crate::tools::file_read_tool::file_read_tool_schema()];
        context.can_use_tool = crate::tool::CanUseToolCallback::new(|_, _, _, _, _, forced| {
            assert!(matches!(
                forced,
                Some(crate::types::permissions::PermissionDecision::Ask { .. })
            ));
            crate::types::permissions::PermissionDecision::Deny {
                message: "final callback deny".into(),
                decision_reason: crate::types::permissions::PermissionDecisionReason::Other {
                    reason: "final policy".into(),
                },
                tool_use_id: None,
            }
        });
        let block = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu-hook-ask-final-deny".into()),
            name: "Read".into(),
            input: json!({"file_path":"/tmp/never-read-permission-test"}),
        };
        let assistant = AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![AssistantContent::ToolUse(block.clone())],
            model: None,
            stop_reason: None,
            usage: None,
        };
        let completion = execute_tool_worker(block, assistant, context).await;
        assert_eq!(prompt_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(
            completion
                .results
                .iter()
                .all(|update| !update.blocked_on_permission)
        );
        assert!(completion.results.iter().filter_map(|update|update.tool_result.as_ref())
            .flat_map(|message|&message.content).any(|content|matches!(content,
                UserContent::ToolResult(result) if result.is_error && result.content=="final callback deny")));
    }
}
