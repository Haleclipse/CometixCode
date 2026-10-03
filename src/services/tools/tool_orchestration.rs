//! Incremental port of official `services/tools/toolOrchestration.ts`.
//! Official `runTools(...)` partitions typed tool-use blocks and delegates each
//! block to `runToolUse(...)`. This Rust subset keeps that typed boundary and
//! mirrors the read-only batching decision conservatively while execution
//! support is expanded tool-by-tool under `tool_execution`.

use super::tool_execution::{
    MessageUpdateLazy, ToolContextModifier, ToolExecutionUpdate, find_tool_call, run_tool_use,
    run_tool_use_permission_gate, validate_tool_input_against_schema,
};
use crate::tool::{InterruptBehavior, ToolUseContext};
use crate::types::message::RenderableMessage;
use crate::types::message::{
    AssistantContent, AssistantMessage, Message, ToolUseBlock, UserMessage,
};
use crate::types::permissions::{PermissionRequest, ToolUseConfirm};
use std::thread;

/// Maps to: CC `getMaxToolUseConcurrency()`.
fn get_max_tool_use_concurrency() -> usize {
    crate::utils::process_env::var("CLAUDE_CODE_MAX_TOOL_USE_CONCURRENCY")
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(10)
}

/// Event-shaped subset of official `runTools(...)` `MessageUpdate` yields.
/// Maps to: CC `services/tools/toolOrchestration.ts` `MessageUpdate` stream.
#[derive(Clone, Debug, PartialEq)]
pub enum ToolOrchestrationEvent {
    PermissionRequest(PermissionRequest),
    Message(RenderableMessage),
    NewContext(ToolUseContext),
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolOrchestrationUpdate {
    pub blocked_on_permission: bool,
    pub request: Option<PermissionRequest>,
    pub forced_choice: Option<crate::types::permissions::PermissionPromptChoice>,
    pub events: Vec<ToolOrchestrationEvent>,
    pub new_context: ToolUseContext,
}

/// Maps to: CC `services/tools/toolOrchestration.ts` `MessageUpdate`.
#[derive(Clone, Debug, PartialEq)]
pub struct MessageUpdate {
    pub message: Option<RenderableMessage>,
    /// Typed model message yielded by CC `ToolResult.newMessages`. The UI
    /// transcript projection remains separate in `message`.
    pub model_message: Option<Message>,
    /// Maps to CC progress `Message` values yielded through
    /// `StreamingToolExecutor.pendingProgress` before final tool results.
    pub progress: Option<crate::types::tools::ToolProgress>,
    pub tool_result: Option<UserMessage>,
    pub new_context: ToolUseContext,
    pub permission_request: Option<PermissionRequest>,
    pub blocked_on_permission: bool,
    pub forced_choice: Option<crate::types::permissions::PermissionPromptChoice>,
    /// Present only while projecting Rust's lazy concurrent updates; maps to
    /// CC `MessageUpdateLazy.contextModifier` before `runTools(...)` applies it.
    pub context_modifier: Option<ToolContextModifier>,
}

#[derive(Clone, Debug)]
pub struct ToolCallBatch {
    pub is_concurrency_safe: bool,
    pub blocks: Vec<ToolUseBlock>,
}

/// Maps to: CC `services/tools/toolOrchestration.ts` `runTools(...)`.
pub fn run_tools(
    tool_use_blocks: &[ToolUseBlock],
    assistant_messages: &[AssistantMessage],
    tool_use_context: &ToolUseContext,
    permission_queue: &mut Vec<ToolUseConfirm>,
) -> Vec<MessageUpdate> {
    let mut current_context = tool_use_context.clone();
    let mut updates = Vec::new();

    for batch in partition_tool_calls(tool_use_blocks, &current_context) {
        if batch.is_concurrency_safe {
            let mut queued_context_modifiers: Vec<ToolContextModifier> = Vec::new();
            for update in run_tools_concurrently(
                &batch.blocks,
                assistant_messages,
                &current_context,
                permission_queue,
            ) {
                if update.blocked_on_permission {
                    for modifier in &queued_context_modifiers {
                        current_context = modifier.modify_context(current_context);
                    }
                    updates.push(MessageUpdate {
                        message: update.message,
                        model_message: None,
                        progress: None,
                        tool_result: update.tool_result,
                        new_context: current_context.clone(),
                        permission_request: update.permission_request,
                        blocked_on_permission: true,
                        forced_choice: update.forced_choice,
                        context_modifier: None,
                    });
                    // Rust's vector-shaped surrogate for CC's async generator
                    // must stop here: official `runToolUse(...)` is awaiting
                    // the interactive permission decision, so later batches
                    // have not yielded yet.
                    return updates;
                }
                if let Some(modifier) = update.context_modifier.clone() {
                    queued_context_modifiers.push(modifier);
                }
                updates.push(MessageUpdate {
                    message: update.message,
                    model_message: None,
                    progress: None,
                    tool_result: update.tool_result,
                    new_context: current_context.clone(),
                    permission_request: update.permission_request,
                    blocked_on_permission: false,
                    forced_choice: update.forced_choice,
                    context_modifier: update.context_modifier,
                });
            }

            for block in &batch.blocks {
                for modifier in queued_context_modifiers
                    .iter()
                    .filter(|modifier| modifier.tool_use_id == block.id.0)
                {
                    current_context = modifier.modify_context(current_context);
                }
            }

            // Maps to official `yield { newContext: currentContext }` after
            // queued context modifiers are applied for a concurrent batch.
            updates.push(MessageUpdate {
                message: None,
                model_message: None,
                progress: None,
                tool_result: None,
                new_context: current_context.clone(),
                permission_request: None,
                blocked_on_permission: false,
                forced_choice: None,
                context_modifier: None,
            });
        } else {
            for update in run_tools_serially(
                &batch.blocks,
                assistant_messages,
                &current_context,
                permission_queue,
            ) {
                let blocked_on_permission = update.blocked_on_permission;
                current_context = update.new_context.clone();
                updates.push(update);
                if blocked_on_permission {
                    // Maps to CC `runTools(...)` yielding from
                    // `runToolsSerially(...)`: once a serial tool awaits
                    // interactive permission, later serial batches have not
                    // started yet. The query actor resumes this function with
                    // the remaining tool_use blocks after permission resolves.
                    return updates;
                }
            }
        }
    }

    updates
}

/// Maps to: CC `services/tools/toolOrchestration.ts:91-115`
/// `partitionToolCalls(...)`, including schema parse and fail-closed
/// `isConcurrencySafe(parsedInput.data)` classification.
pub fn partition_tool_calls(
    tool_use_blocks: &[ToolUseBlock],
    tool_use_context: &ToolUseContext,
) -> Vec<ToolCallBatch> {
    let mut batches: Vec<ToolCallBatch> = Vec::new();

    for block in tool_use_blocks.iter().cloned() {
        let is_concurrency_safe = is_concurrency_safe_tool_use_in_context(&block, tool_use_context);
        if is_concurrency_safe
            && batches
                .last()
                .is_some_and(|batch| batch.is_concurrency_safe)
        {
            batches.last_mut().unwrap().blocks.push(block);
        } else {
            batches.push(ToolCallBatch {
                is_concurrency_safe,
                blocks: vec![block],
            });
        }
    }

    batches
}

fn is_concurrency_safe_tool_use_in_context(
    tool_use: &ToolUseBlock,
    context: &ToolUseContext,
) -> bool {
    let Some(definition) = crate::types::tools::find_tool_by_name(&context.tools, &tool_use.name)
    else {
        return false;
    };
    let parsed_input = if definition.is_mcp {
        tool_use.input.clone()
    } else {
        find_tool_call(&definition.name)
            .map(|tool| tool.normalize_input_with_context(&tool_use.input, context))
            .unwrap_or_else(|| tool_use.input.clone())
    };
    if validate_tool_input_against_schema(&definition.name, &parsed_input, &definition.input_schema)
        .is_err()
    {
        return false;
    }
    if definition.is_mcp {
        return crate::services::mcp::client::mcp_tool_snapshot_for_invocation(
            &definition.name,
            &context.mcp_state,
        )
        .is_some_and(|tool| tool.read_only_hint);
    }
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        find_tool_call(&definition.name).is_some_and(|tool| tool.is_concurrency_safe(&parsed_input))
    }))
    .unwrap_or(false)
}

/// Maps to: CC `runToolsSerially(...)`.
pub fn run_tools_serially(
    tool_use_blocks: &[ToolUseBlock],
    assistant_messages: &[AssistantMessage],
    tool_use_context: &ToolUseContext,
    permission_queue: &mut Vec<ToolUseConfirm>,
) -> Vec<MessageUpdate> {
    let mut current_context = tool_use_context.clone();
    let mut updates = Vec::new();

    for tool_use in tool_use_blocks {
        let Some(assistant_message) =
            assistant_message_for_tool_use(assistant_messages, &tool_use.id.0)
        else {
            continue;
        };
        current_context.mark_in_progress_with_behavior(
            tool_use.id.0.clone(),
            interrupt_behavior_for_tool(&tool_use.name),
        );
        let update = run_tool_use(
            tool_use,
            assistant_message,
            &current_context,
            permission_queue,
        );
        let blocked_on_permission = update.blocked_on_permission;
        if !blocked_on_permission {
            current_context.mark_complete(&tool_use.id.0);
        }
        updates.push(MessageUpdate {
            message: update.message,
            model_message: None,
            progress: None,
            tool_result: update.tool_result,
            new_context: current_context.clone(),
            permission_request: update.request,
            blocked_on_permission,
            forced_choice: update.forced_choice,
            context_modifier: None,
        });
        if blocked_on_permission {
            // Maps to CC `runToolsSerially(...)`: `runToolUse(...)` awaits the
            // interactive permission decision before the next serial tool is
            // started. Rust returns control to the query actor at this yield
            // point; the actor resumes `runTools` with the remaining blocks
            // after `QueryCommand::PermissionDecision` is received.
            break;
        }
    }

    updates
}

/// Maps to: CC `runToolsConcurrently(...)`.
pub fn run_tools_concurrently(
    tool_use_blocks: &[ToolUseBlock],
    assistant_messages: &[AssistantMessage],
    tool_use_context: &ToolUseContext,
    permission_queue: &mut Vec<ToolUseConfirm>,
) -> Vec<MessageUpdateLazy> {
    let max_concurrency = get_max_tool_use_concurrency();
    let mut updates = Vec::new();

    for chunk in tool_use_blocks.chunks(max_concurrency) {
        let mut in_progress_context = tool_use_context.clone();
        for tool_use in chunk {
            in_progress_context.mark_in_progress_with_behavior(
                tool_use.id.0.clone(),
                interrupt_behavior_for_tool(&tool_use.name),
            );
        }

        let mut handles = Vec::new();
        for (index, tool_use) in chunk.iter().cloned().enumerate() {
            let Some(assistant_message) =
                assistant_message_for_tool_use(assistant_messages, &tool_use.id.0).cloned()
            else {
                continue;
            };
            let context = in_progress_context.clone();
            // CC's teammate context is AsyncLocalStorage: a teammate's
            // concurrent tool batch still observes `getTeammateContext()`
            // because every continuation inherits the store. Rust's
            // task-local does not cross `thread::spawn`, so carry it over
            // explicitly (`utils/teammateContext.ts` semantics).
            //
            // Test gap (known): the carry helpers are pinned in
            // `utils/teammate_context.rs`, but no test pins THIS call site —
            // deleting the capture below keeps the suite green. Closing it
            // needs a probe inside a real `Tool::call` on the concurrent
            // path, which has no injection point today.
            let teammate_context = crate::utils::teammate_context::capture_teammate_context();
            handles.push(thread::spawn(move || {
                crate::utils::teammate_context::with_teammate_context_sync(
                    teammate_context,
                    move || {
                        let mut local_queue = Vec::new();
                        let update =
                            run_tool_use(&tool_use, &assistant_message, &context, &mut local_queue);
                        (index, tool_use.id.0, update, local_queue)
                    },
                )
            }));
        }

        let mut results = handles
            .into_iter()
            .map(|handle| handle.join().expect("concurrent tool worker panicked"))
            .collect::<Vec<_>>();
        results.sort_by_key(|(index, _, _, _)| *index);

        for (_, tool_use_id, update, local_queue) in results.iter() {
            permission_queue.extend(local_queue.clone());
            let context_modifier = (!update.blocked_on_permission)
                .then(|| ToolContextModifier::mark_complete(tool_use_id.clone()));
            updates.push(MessageUpdateLazy {
                message: update.message.clone(),
                tool_result: update.tool_result.clone(),
                permission_request: update.request.clone(),
                blocked_on_permission: update.blocked_on_permission,
                forced_choice: update.forced_choice,
                context_modifier,
            });
        }
    }

    updates
}

fn assistant_message_for_tool_use<'a>(
    assistant_messages: &'a [AssistantMessage],
    tool_use_id: &str,
) -> Option<&'a AssistantMessage> {
    assistant_messages.iter().find(|message| {
        message.content.iter().any(|content| match content {
            AssistantContent::ToolUse(block) => block.id.0 == tool_use_id,
            _ => false,
        })
    })
}

/// Maps to CC `StreamingToolExecutor.getToolInterruptBehavior`: unknown and
/// MCP tools use the fail-closed `block` default from `Tool.ts`.
fn interrupt_behavior_for_tool(tool_name: &str) -> InterruptBehavior {
    find_tool_call(tool_name)
        .map(|tool| tool.interrupt_behavior())
        .unwrap_or(InterruptBehavior::Block)
}

pub fn run_tools_for_message(
    message: &RenderableMessage,
    context: &ToolUseContext,
    permission_queue: &mut Vec<ToolUseConfirm>,
) -> ToolExecutionUpdate {
    run_tool_use_permission_gate(message, context, permission_queue)
}

pub fn run_tools_for_message_events(
    message: &RenderableMessage,
    context: &ToolUseContext,
    permission_queue: &mut Vec<ToolUseConfirm>,
) -> ToolOrchestrationUpdate {
    let update = run_tool_use_permission_gate(message, context, permission_queue);
    let mut events = Vec::new();
    if update.blocked_on_permission {
        if let Some(request) = update.request.clone() {
            events.push(ToolOrchestrationEvent::PermissionRequest(request));
        }
    }
    events.push(ToolOrchestrationEvent::NewContext(context.clone()));

    ToolOrchestrationUpdate {
        blocked_on_permission: update.blocked_on_permission,
        request: update.request,
        forced_choice: update.forced_choice,
        events,
        new_context: context.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::tools::tool_execution::ToolContextModifierOperation;
    use crate::tool::ToolPermissionContext;
    use crate::types::message::{AssistantContent, StopReason};
    use crate::types::message::{RenderableMessage, RenderableMessageKind, ToolUseStatus};
    use crate::types::permissions::PermissionRuleValue;
    use crate::utils::test_env::PinnedProjectDir;
    use std::collections::HashMap;

    fn typed_bash_block() -> ToolUseBlock {
        ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu_typed".to_string()),
            name: "Bash".to_string(),
            input: serde_json::json!({ "command": "printf permission-gated" }),
        }
    }

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

    fn queued_bash() -> RenderableMessage {
        RenderableMessage::assistant_block(
            "toolu_mock",
            crate::types::message::AssistantContent::ToolUse(crate::types::message::ToolUseBlock {
                id: crate::types::ids::ToolUseId(String::new()),
                name: "Bash".to_string(),
                input: serde_json::json!({"command": "printf permission-gated"}),
            }),
        )
    }

    #[test]
    fn run_tools_accepts_typed_tool_use_blocks_and_requests_permission() {
        let block = typed_bash_block();
        let assistant = assistant_with_block(block.clone());
        let context = ToolUseContext::with_permission_context(ToolPermissionContext::default());
        let mut queue = Vec::new();

        let updates = run_tools(&[block], &[assistant], &context, &mut queue);

        assert_eq!(updates.len(), 1);
        assert!(updates[0].blocked_on_permission);
        assert_eq!(
            updates[0].permission_request.as_ref().unwrap().tool_use_id,
            "toolu_typed"
        );
        assert!(queue.is_empty());
    }

    #[test]
    fn run_tools_serially_pauses_at_first_interactive_permission_request() {
        let first = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu_first_serial".to_string()),
            name: "Bash".to_string(),
            input: serde_json::json!({ "command": "printf first" }),
        };
        let second = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu_second_serial".to_string()),
            name: "Bash".to_string(),
            input: serde_json::json!({ "command": "printf second" }),
        };
        let assistant = AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![
                AssistantContent::ToolUse(first.clone()),
                AssistantContent::ToolUse(second.clone()),
            ],
            model: None,
            stop_reason: Some(StopReason::ToolUse),
            usage: None,
        };
        let context = ToolUseContext::with_permission_context(ToolPermissionContext::default());
        let mut queue = Vec::new();

        let updates = run_tools(&[first, second], &[assistant], &context, &mut queue);

        assert_eq!(updates.len(), 1);
        assert!(updates[0].blocked_on_permission);
        assert_eq!(
            updates[0].permission_request.as_ref().unwrap().tool_use_id,
            "toolu_first_serial"
        );
        assert!(queue.is_empty());
    }

    #[test]
    fn partition_tool_calls_batches_consecutive_read_only_tools() {
        let read = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu_read".to_string()),
            name: "Read".to_string(),
            input: serde_json::json!({ "file_path": "src/main.rs" }),
        };
        let grep = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu_grep".to_string()),
            name: "Grep".to_string(),
            input: serde_json::json!({ "pattern": "fn main" }),
        };
        let mcp_list = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu_mcp_list".to_string()),
            name: "ListMcpResourcesTool".to_string(),
            input: serde_json::json!({}),
        };
        let bash_write = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu_bash_write".to_string()),
            name: "Bash".to_string(),
            input: serde_json::json!({ "command": "rm -rf target" }),
        };
        let bash_read = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu_bash_read".to_string()),
            name: "Bash".to_string(),
            input: serde_json::json!({ "command": "git status --short" }),
        };
        // MCP resource tools are CC `specialTools`: they are absent from the
        // default model-facing set and injected only when needed. Include the
        // list tool explicitly so this test exercises its read-only batching.
        let mut active_tools = crate::tools::get_tools(&ToolPermissionContext::default());
        active_tools.push(crate::tools::list_mcp_resources_tool::list_mcp_resources_tool_schema());
        let context = ToolUseContext::with_permission_context(ToolPermissionContext::default())
            .with_tools(active_tools);

        let batches =
            partition_tool_calls(&[read, grep, mcp_list, bash_write, bash_read], &context);

        assert_eq!(batches.len(), 3);
        assert!(batches[0].is_concurrency_safe);
        assert_eq!(batches[0].blocks.len(), 3);
        assert!(!batches[1].is_concurrency_safe);
        assert_eq!(batches[1].blocks[0].id.0, "toolu_bash_write");
        assert!(batches[2].is_concurrency_safe);
        assert_eq!(batches[2].blocks[0].id.0, "toolu_bash_read");
    }

    #[test]
    fn dynamic_mcp_read_only_annotation_controls_concurrency() {
        let mut server =
            crate::services::mcp::client::McpConnectionDiscovery::pending("docs").server;
        server.client.status = crate::services::mcp::types::McpServerConnectionType::Connected;
        server
            .tools
            .push(crate::services::mcp::types::McpToolSnapshot {
                name: "lookup".to_string(),
                display_name: None,
                description: Some("lookup docs".to_string()),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {"query": {"type": "string"}},
                    "required": ["query"],
                    "additionalProperties": false
                }),
                read_only_hint: true,
                destructive_hint: false,
                open_world_hint: false,
            });
        let mut state = crate::state::app_state_store::McpState {
            clients: vec![server],
            ..crate::state::app_state_store::McpState::default()
        };
        crate::services::mcp::client::refresh_flat_mcp_capabilities(&mut state);
        let tool_name = "mcp__docs__lookup";
        let context = ToolUseContext::with_permission_context(ToolPermissionContext::default())
            .with_tools(state.tools.clone())
            .with_mcp_state(state);
        let block = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu_mcp_read".to_string()),
            name: tool_name.to_string(),
            input: serde_json::json!({"query": "rust"}),
        };

        let batches = partition_tool_calls(&[block], &context);
        assert_eq!(batches.len(), 1);
        assert!(batches[0].is_concurrency_safe);
    }

    #[test]
    fn partition_tool_calls_treats_schema_invalid_tools_as_serial_like_official_safe_parse() {
        let invalid_read = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu_invalid_read_partition".to_string()),
            name: "Read".to_string(),
            input: serde_json::json!({ "limit": 20 }),
        };
        let grep = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu_grep_after_invalid".to_string()),
            name: "Grep".to_string(),
            input: serde_json::json!({ "pattern": "fn main" }),
        };
        let context = ToolUseContext::with_permission_context(ToolPermissionContext::default());

        let batches = partition_tool_calls(&[invalid_read, grep], &context);

        assert_eq!(batches.len(), 2);
        assert!(!batches[0].is_concurrency_safe);
        assert_eq!(batches[0].blocks[0].id.0, "toolu_invalid_read_partition");
        assert!(batches[1].is_concurrency_safe);
        assert_eq!(batches[1].blocks[0].id.0, "toolu_grep_after_invalid");
    }

    #[test]
    fn typed_run_tools_defers_permission_and_completion_until_after_hooks() {
        // The relative `src/main.rs` inputs resolve against the project dir.
        let _project_dir = PinnedProjectDir::at_manifest_root();
        let read = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu_read_concurrent".to_string()),
            name: "Read".to_string(),
            input: serde_json::json!({ "file_path": "src/main.rs" }),
        };
        let grep = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu_grep_concurrent".to_string()),
            name: "Grep".to_string(),
            input: serde_json::json!({ "pattern": "fn main", "path": "src" }),
        };
        let assistants = vec![
            assistant_with_block(read.clone()),
            assistant_with_block(grep.clone()),
        ];
        let context = ToolUseContext::with_permission_context(ToolPermissionContext::default());
        let mut queue = Vec::new();

        let lazy_updates = run_tools_concurrently(
            &[read.clone(), grep.clone()],
            &assistants,
            &context,
            &mut queue,
        );

        // Typed production orchestration only parses here. PreToolUse and the
        // final canUseTool decision own allow/ask/deny and completion later.
        assert!(queue.is_empty());
        assert_eq!(lazy_updates.len(), 2);
        assert!(
            lazy_updates.iter().all(|update| {
                update.blocked_on_permission && update.context_modifier.is_none()
            })
        );

        let mut queue = Vec::new();
        let updates = run_tools(&[read, grep], &assistants, &context, &mut queue);

        assert_eq!(updates.len(), 1);
        assert!(updates[0].blocked_on_permission);
        assert!(updates[0].context_modifier.is_none());
        assert!(queue.is_empty());
    }

    #[test]
    fn run_tools_concurrent_permission_request_pauses_before_later_batches() {
        let web_fetch = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu_web_fetch_permission".to_string()),
            name: "WebFetch".to_string(),
            input: serde_json::json!({
                "url": "https://example.com/private",
                "prompt": "summarize"
            }),
        };
        let bash = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu_bash_after_web_fetch".to_string()),
            name: "Bash".to_string(),
            input: serde_json::json!({ "command": "echo should-not-prequeue" }),
        };
        let assistants = vec![
            assistant_with_block(web_fetch.clone()),
            assistant_with_block(bash.clone()),
        ];
        let context = ToolUseContext::with_permission_context(ToolPermissionContext::default());
        let mut queue = Vec::new();

        let updates = run_tools(&[web_fetch, bash], &assistants, &context, &mut queue);

        assert_eq!(updates.len(), 1);
        assert!(updates[0].blocked_on_permission);
        assert_eq!(
            updates[0].permission_request.as_ref().unwrap().tool_use_id,
            "toolu_web_fetch_permission"
        );
        assert!(updates[0].context_modifier.is_none());
        assert!(queue.is_empty());
    }

    #[test]
    fn run_tools_concurrently_does_not_mark_permission_blocked_tool_complete() {
        let web_fetch = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu_web_fetch_blocked".to_string()),
            name: "WebFetch".to_string(),
            input: serde_json::json!({
                "url": "https://example.com/private",
                "prompt": "summarize"
            }),
        };
        let assistant = assistant_with_block(web_fetch.clone());
        let context = ToolUseContext::with_permission_context(ToolPermissionContext::default());
        let mut queue = Vec::new();

        let updates = run_tools_concurrently(&[web_fetch], &[assistant], &context, &mut queue);

        assert_eq!(updates.len(), 1);
        assert!(updates[0].blocked_on_permission);
        assert!(updates[0].context_modifier.is_none());
        assert!(queue.is_empty());
    }

    #[test]
    fn orchestration_delegates_queued_tool_use_to_execution_gate() {
        let message = queued_bash();
        let mut queue = Vec::new();

        let context = ToolUseContext::with_permission_context(ToolPermissionContext::default());
        let update = run_tools_for_message(&message, &context, &mut queue);

        assert!(update.blocked_on_permission);
        assert_eq!(queue.len(), 1);
    }

    #[test]
    fn orchestration_events_include_permission_request_and_new_context() {
        let message = queued_bash();
        let mut queue = Vec::new();
        let context = ToolUseContext::with_permission_context(ToolPermissionContext::default());

        let update = run_tools_for_message_events(&message, &context, &mut queue);

        assert!(update.blocked_on_permission);
        assert_eq!(update.request.as_ref().unwrap().tool_use_id, "toolu_mock");
        assert_eq!(queue.len(), 1);
        assert!(matches!(
            &update.events[0],
            ToolOrchestrationEvent::PermissionRequest(request)
                if request.tool_use_id == "toolu_mock"
        ));
        assert!(matches!(
            update.events.last(),
            Some(ToolOrchestrationEvent::NewContext(_))
        ));
    }

    #[test]
    fn orchestration_events_keep_request_when_permission_is_preallowed() {
        let message = queued_bash();
        let mut ctx = ToolPermissionContext::default();
        let mut rules = HashMap::new();
        rules.insert(
            crate::types::permissions::PermissionRuleSource::Session,
            vec![PermissionRuleValue::new("Bash", None)],
        );
        ctx.always_allow_rules = rules;
        let context = ToolUseContext::with_permission_context(ctx);
        let mut queue = Vec::new();

        let update = run_tools_for_message_events(&message, &context, &mut queue);

        assert!(!update.blocked_on_permission);
        assert!(queue.is_empty());
        assert_eq!(update.request.as_ref().unwrap().tool_use_id, "toolu_mock");
        assert!(
            !update
                .events
                .iter()
                .any(|event| matches!(event, ToolOrchestrationEvent::PermissionRequest(_)))
        );
        assert!(matches!(
            update.events.last(),
            Some(ToolOrchestrationEvent::NewContext(_))
        ));
    }
}
