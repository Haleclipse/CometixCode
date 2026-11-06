//! TodoWrite tool metadata.
//!
//! Maps to:
//! - CC `tools/TodoWriteTool/TodoWriteTool.ts`
//! - CC `tools/TodoWriteTool/constants.ts`
//! - CC `tools/TodoWriteTool/prompt.ts`
//!
//! The tool has no visible UI in Claude Code (`renderToolUseMessage() { return
//! null }`).
//! Execution lives in this module, dispatched from `services/tools/tool_execution.rs`.

pub mod constants;
pub mod prompt;

/// Maps to: CC `TodoWriteTool.ts:13-17` `inputSchema` — one field, the shared
/// `TodoListSchema()` from `utils/todo/types.ts` (Rust: `utils::todo::types`).
pub fn input_schema() -> &'static crate::utils::zod::Schema {
    static SCHEMA: std::sync::OnceLock<crate::utils::zod::Schema> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| {
        crate::utils::zod::strict_object(vec![(
            "todos",
            crate::utils::todo::types::todo_list_schema()
                .clone()
                .describe("The updated todo list"),
        )])
    })
}

/// Maps to CC `TodoWriteTool.inputSchema`.
pub fn todo_write_tool_schema() -> crate::types::tools::Tool {
    crate::types::tools::Tool {
        name: constants::TODO_WRITE_TOOL_NAME.to_string(),
        description: prompt::PROMPT.to_string(),
        input_schema: crate::utils::zod_to_json_schema::zod_to_json_schema(input_schema()),
        strict: Some(true),
        ..Default::default()
    }
}

/// Maps to CC `TodoWriteTool.outputSchema` (:20):
/// `{ oldTodos, newTodos, verificationNudgeNeeded? }`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TodoWriteOutput {
    pub(crate) old_todos: Vec<serde_json::Value>,
    pub(crate) new_todos: Vec<serde_json::Value>,
    pub(crate) verification_nudge_needed: bool,
}

impl TodoWriteOutput {
    fn from_input(args: &serde_json::Value, context: &crate::tool::ToolUseContext) -> Self {
        let todos = args
            .get("todos")
            .and_then(|value| value.as_array())
            .cloned()
            .unwrap_or_default();
        let todo_key = context
            .agent_id
            .clone()
            .unwrap_or_else(crate::bootstrap::state::get_session_id);
        let old_todos = context
            .app_store
            .store
            .as_ref()
            .map(|store| {
                store
                    .get()
                    .todos
                    .get(&todo_key)
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|item| {
                        serde_json::json!({
                            "content": item.content,
                            "status": match item.status {
                                crate::utils::todo::types::TodoStatus::Pending => "pending",
                                crate::utils::todo::types::TodoStatus::InProgress => "in_progress",
                                crate::utils::todo::types::TodoStatus::Completed => "completed",
                            },
                            "activeForm": item.active_form,
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let parsed_new = parse_todo_items(&todos);
        // Maps to: CC `todos.every(status === completed)` — empty array is allDone.
        let all_done = todos
            .iter()
            .all(|item| item.get("status").and_then(|v| v.as_str()) == Some("completed"));
        if let Some(store) = context.app_store.store.as_ref() {
            // Maps to: CC TodoWriteTool `setAppState` writing `todos[todoKey]`.
            // All-done → store `[]` (CC keeps the key with empty array).
            // P4 identity: make_mut = CC's `{...prev.todos}` map spread.
            store.replace_with(|state| {
                let stored = if all_done {
                    Vec::new()
                } else {
                    parsed_new.clone()
                };
                std::sync::Arc::make_mut(&mut state.todos).insert(todo_key.clone(), stored);
            });
        }
        // Maps to: CC TodoWrite.call verificationNudgeNeeded gate.
        let verification_nudge_needed = crate::utils::feature_flags::feature_enabled(
            crate::utils::feature_flags::FeatureFlag::HiveEvidence,
        ) && context.agent_id.is_none()
            && all_done
            && todos.len() >= 3
            && !todos.iter().any(|item| {
                item.get("content")
                    .and_then(|v| v.as_str())
                    .is_some_and(|content| content.to_ascii_lowercase().contains("verif"))
            });
        Self {
            old_todos,
            // CC returns the model-provided `todos` here even if all items are
            // completed and the stored app-state list is cleared.
            new_todos: todos,
            verification_nudge_needed,
        }
    }
}

fn parse_todo_items(values: &[serde_json::Value]) -> Vec<crate::utils::todo::types::TodoItem> {
    values
        .iter()
        .filter_map(|value| {
            let content = value.get("content")?.as_str()?.to_string();
            let active_form = value
                .get("activeForm")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let status = match value
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("pending")
            {
                "in_progress" => crate::utils::todo::types::TodoStatus::InProgress,
                "completed" => crate::utils::todo::types::TodoStatus::Completed,
                _ => crate::utils::todo::types::TodoStatus::Pending,
            };
            Some(crate::utils::todo::types::TodoItem {
                content,
                status,
                active_form,
            })
        })
        .collect()
}

const TODO_WRITE_BASE_RESULT: &str = "Todos have been modified successfully. Ensure that you continue to use the todo list to track your progress. Please proceed with the current tasks if applicable";

/// Maps to: CC `TodoWriteTool.ts:104-114` `mapToolResultToToolResultBlockParam`
/// — the nudge interpolates the verifier's agent type from
/// `AgentTool/constants.ts`, which nyan renamed to `REVIEW_AGENT_TYPE`.
fn todo_write_tool_result_content(output: &TodoWriteOutput) -> String {
    if output.verification_nudge_needed {
        format!(
            "{TODO_WRITE_BASE_RESULT}\n\nNOTE: You just closed out 3+ tasks and none of them was a verification step. Before writing your final summary, spawn the review agent (subagent_type=\"{}\"). You cannot self-assign PARTIAL by listing caveats in your summary — only the verifier issues a verdict.",
            crate::tools::agent_tool::constants::REVIEW_AGENT_TYPE
        )
    } else {
        TODO_WRITE_BASE_RESULT.to_string()
    }
}

/// Behavioral half of CC `TodoWriteTool` — dispatched via `crate::tool::ToolCall`.
pub(crate) struct TodoWriteTool;

impl crate::tool::ToolCall for TodoWriteTool {
    fn name(&self) -> &'static str {
        "TodoWrite"
    }

    /// Maps to: CC `TodoWriteTool.ts:39-41` `async prompt() { return PROMPT }`
    /// — same source the wire schema renders eagerly.
    fn prompt(
        &self,
        _tool: &crate::types::tools::Tool,
        _options: &crate::tool::ToolPromptOptions<'_>,
    ) -> String {
        prompt::PROMPT.to_string()
    }

    /// Maps to: CC `TodoWriteTool.isEnabled()` — mutually exclusive with TodoV2.
    fn is_enabled(&self) -> bool {
        !crate::utils::tasks::is_todo_v2_enabled()
    }

    /// Maps to: CC `TodoWriteTool.ts:33` `searchHint`.
    fn search_hint(&self) -> Option<&'static str> {
        Some("manage the session task checklist")
    }

    /// Maps to: CC `TodoWriteTool.ts:36-38` `description()`.
    fn description(&self, _args: &serde_json::Value) -> String {
        prompt::DESCRIPTION.to_string()
    }

    /// Maps to: CC `TodoWriteTool.ts:34` `maxResultSizeChars`.
    fn max_result_size_chars(&self) -> usize {
        100_000
    }

    /// Maps to: CC `TodoWriteTool.ts:48-50` `userFacingName()` — the empty
    /// string; the visible todo UI arrives through the todo attachment.
    fn user_facing_name(&self, _args: Option<&serde_json::Value>) -> String {
        String::new()
    }

    fn should_defer(&self) -> bool {
        true
    }

    fn to_auto_classifier_input(&self, args: &serde_json::Value) -> String {
        let count = args
            .get("todos")
            .and_then(|value| value.as_array())
            .map(|items| items.len())
            .unwrap_or(0);
        format!("{count} items")
    }

    fn check_permissions(
        &self,
        args: &serde_json::Value,
        _context: &crate::tool::ToolUseContext,
    ) -> crate::utils::permissions::permission_result::PermissionResult {
        crate::utils::permissions::permission_result::PermissionResult::Allow {
            updated_input: Some(args.clone()),
            user_modified: None,
            decision_reason: None,
            tool_use_id: None,
            accept_feedback: None,
            content_blocks: Vec::new(),
        }
    }

    fn call<'a>(
        &'a self,
        args: &'a serde_json::Value,
        _request: &'a crate::types::permissions::PermissionRequest,
        context: &'a crate::tool::ToolUseContext,
        _can_use_tool: Option<crate::tool::CanUseToolFn<'a>>,
        _parent_message: Option<&'a crate::types::message::AssistantMessage>,
        _on_progress: Option<crate::tool::ToolCallProgressFn<'a>>,
    ) -> futures::future::BoxFuture<'a, crate::tool::ToolResult> {
        Box::pin(async move {
            crate::tool::ToolResult {
                data: crate::tool::ToolOutput::TodoWrite(TodoWriteOutput::from_input(
                    args, context,
                )),
                new_messages: Vec::new(),
            }
        })
    }

    fn map_tool_result_to_tool_result_block_param(
        &self,
        data: &crate::tool::ToolOutput,
        _tool_use_id: &str,
    ) -> (String, crate::types::message::ToolResultStatus) {
        match data {
            crate::tool::ToolOutput::TodoWrite(output) => (
                todo_write_tool_result_content(output),
                crate::types::message::ToolResultStatus::Success,
            ),
            crate::tool::ToolOutput::Composed {
                content, status, ..
            } => (content.clone(), *status),
            _ => (
                "<tool_use_error>tool output variant not handled by TodoWrite mapper</tool_use_error>"
                    .to_string(),
                crate::types::message::ToolResultStatus::Error,
            ),
        }
    }

    // CC `TodoWriteTool` defines no `renderToolResultMessage` (the visible
    // todo UI arrives through the todo attachment) — the render layer hides
    // success rows by name (`success_tool_result_is_nonvisual`).

    /// Maps to: CC recording TodoWriteTool's call data
    /// (`TodoWriteTool.ts:96-102` `{ oldTodos, newTodos,
    /// verificationNudgeNeeded }`) as the message's `toolUseResult`. Key order
    /// follows the CC object literal; `verificationNudgeNeeded` is optional in
    /// the outputSchema but `call` always returns a boolean.
    fn tool_use_result(&self, data: &crate::tool::ToolOutput) -> Option<serde_json::Value> {
        match data {
            crate::tool::ToolOutput::TodoWrite(output) => {
                let mut map = serde_json::Map::new();
                map.insert(
                    "oldTodos".to_string(),
                    serde_json::Value::Array(output.old_todos.clone()),
                );
                map.insert(
                    "newTodos".to_string(),
                    serde_json::Value::Array(output.new_todos.clone()),
                );
                map.insert(
                    "verificationNudgeNeeded".to_string(),
                    serde_json::Value::Bool(output.verification_nudge_needed),
                );
                Some(serde_json::Value::Object(map))
            }
            crate::tool::ToolOutput::Composed {
                content,
                status: crate::types::message::ToolResultStatus::Error,
                ..
            } => {
                let message = crate::utils::messages::extract_tag(content, "tool_use_error")
                    .unwrap_or_else(|| content.clone());
                Some(serde_json::Value::String(message))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn todo_write_tool_call_returns_official_output_schema_and_model_copy() {
        use crate::tool::ToolCall;

        let args = serde_json::json!({
            "todos": [{
                "content": "Review query parity",
                "status": "in_progress",
                "activeForm": "Reviewing query parity"
            }]
        });
        let request = crate::types::permissions::PermissionRequest {
            permission_result: None,
            id: "perm-todo".to_string(),
            tool_use_id: "toolu_todo".to_string(),
            tool_name: "TodoWrite".to_string(),
            mcp_info: None,
            decision_reason: None,
            description: String::new(),
            message: String::new(),
            input_summary: String::new(),
            input: args.clone(),
            call_input: None,
            rule: crate::types::permissions::PermissionRuleValue::new("TodoWrite", None),
            suggestions: Vec::new(),
            blocked_path: None,
            metadata: None,
            is_compound_command: false,
            mode: crate::types::permissions::PermissionMode::Default,
        };
        let context = crate::tool::ToolUseContext::with_permission_context(
            crate::tool::ToolPermissionContext::default(),
        );

        let result = futures::executor::block_on(
            TodoWriteTool.call(&args, &request, &context, None, None, None),
        );
        let crate::tool::ToolOutput::TodoWrite(output) = &result.data else {
            panic!("expected TodoWrite output");
        };
        assert!(output.old_todos.is_empty());
        assert_eq!(output.new_todos, args["todos"].as_array().unwrap().clone());
        assert!(!output.verification_nudge_needed);

        let (content, status) =
            TodoWriteTool.map_tool_result_to_tool_result_block_param(&result.data, "toolu_todo");
        assert_eq!(status, crate::types::message::ToolResultStatus::Success);
        assert_eq!(content, TODO_WRITE_BASE_RESULT);

        // CC records `{ oldTodos, newTodos, verificationNudgeNeeded }` as the
        // message's toolUseResult (TodoWriteTool.ts:96-102).
        let raw = TodoWriteTool
            .tool_use_result(&result.data)
            .expect("call data should ride the row");
        assert_eq!(raw.get("oldTodos"), Some(&serde_json::json!([])));
        assert_eq!(
            raw.get("newTodos").and_then(|v| v.as_array()).map(Vec::len),
            Some(1)
        );
        assert_eq!(
            raw.get("verificationNudgeNeeded"),
            Some(&serde_json::json!(false))
        );

        // Explicit CC member values (TodoWriteTool.ts:33-34, :48-50).
        assert_eq!(
            TodoWriteTool.search_hint(),
            Some("manage the session task checklist")
        );
        assert_eq!(TodoWriteTool.max_result_size_chars(), 100_000);
        assert_eq!(TodoWriteTool.user_facing_name(None), "");
    }

    #[test]
    fn todo_write_nudge_interpolates_the_verification_agent_type() {
        let output = TodoWriteOutput {
            old_todos: Vec::new(),
            new_todos: Vec::new(),
            verification_nudge_needed: true,
        };
        let content = todo_write_tool_result_content(&output);
        assert!(content.starts_with(TODO_WRITE_BASE_RESULT));
        assert!(content.contains(&format!(
            "subagent_type=\"{}\"",
            crate::tools::agent_tool::constants::REVIEW_AGENT_TYPE
        )));
        assert!(content.ends_with("only the verifier issues a verdict."));
    }

    #[test]
    fn todo_write_tool_schema_matches_official_todo_list_shape() {
        let schema = todo_write_tool_schema();
        assert_eq!(schema.name, "TodoWrite");
        assert!(schema.description.contains("Task States"));
        assert_eq!(
            schema.input_schema["required"],
            serde_json::json!(["todos"])
        );
        let item = &schema.input_schema["properties"]["todos"]["items"];
        assert_eq!(
            item["required"],
            serde_json::json!(["content", "status", "activeForm"])
        );
        assert!(item["properties"].get("priority").is_none());
        assert!(item["properties"].get("id").is_none());
        assert!(item["properties"].get("activeForm").is_some());
    }
}
