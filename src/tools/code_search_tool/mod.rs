//! `CodeSearch` tool adapter for the structured retrieval library.

pub mod prompt;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Output {
    pub content: String,
    pub num_results: usize,
    pub truncated: bool,
}

pub fn input_schema() -> &'static crate::utils::zod::Schema {
    static SCHEMA: std::sync::OnceLock<crate::utils::zod::Schema> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| {
        use crate::utils::semantic_number::semantic_number;
        use crate::utils::zod;
        zod::strict_object(vec![
            (
                "query",
                zod::string()
                    .describe("Natural-language code search query, identifier, or literal"),
            ),
            (
                "path",
                zod::string()
                    .optional()
                    .describe("Optional worktree-relative directory or file"),
            ),
            (
                "glob",
                zod::string()
                    .optional()
                    .describe("Optional file glob filter"),
            ),
            (
                "max_results",
                semantic_number(zod::number().int().positive().optional())
                    .describe("Maximum results to return, from 1 to 30; defaults to 10"),
            ),
        ])
    })
}

pub fn code_search_tool_schema() -> crate::types::tools::Tool {
    crate::types::tools::Tool {
        name: prompt::CODE_SEARCH_TOOL_NAME.to_string(),
        description: prompt::get_description(),
        input_schema: crate::utils::zod_to_json_schema::zod_to_json_schema(input_schema()),
        strict: Some(true),
        ..Default::default()
    }
}

pub(crate) struct CodeSearchTool;

pub(crate) fn parse_output(value: &serde_json::Value) -> Option<Output> {
    let object = value.as_object()?;
    Some(Output {
        content: object.get("content")?.as_str()?.to_string(),
        num_results: object.get("numResults")?.as_u64()? as usize,
        truncated: object.get("truncated")?.as_bool()?,
    })
}

fn request_from_args(
    args: &serde_json::Value,
) -> Result<crate::services::retrieval::SearchRequest, String> {
    let query = args
        .get("query")
        .and_then(serde_json::Value::as_str)
        .filter(|query| !query.trim().is_empty())
        .ok_or_else(|| "missing query".to_string())?;
    let max_results = args
        .get("max_results")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(10)
        .clamp(1, 30) as usize;
    Ok(crate::services::retrieval::SearchRequest {
        query: query.to_string(),
        path: args
            .get("path")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        glob: args
            .get("glob")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        max_results,
    })
}

impl crate::tool::ToolCall for CodeSearchTool {
    fn name(&self) -> &'static str {
        prompt::CODE_SEARCH_TOOL_NAME
    }

    fn prompt(
        &self,
        _tool: &crate::types::tools::Tool,
        _options: &crate::tool::ToolPromptOptions<'_>,
    ) -> String {
        prompt::get_description()
    }

    fn description(&self, _args: &serde_json::Value) -> String {
        prompt::get_description()
    }

    fn user_facing_name(&self, _args: Option<&serde_json::Value>) -> String {
        "Search".to_string()
    }

    fn search_hint(&self) -> Option<&'static str> {
        Some("search code with exact, symbol, text, and semantic retrieval")
    }

    fn get_tool_use_summary(&self, args: &serde_json::Value) -> Option<String> {
        args.get("query")
            .and_then(serde_json::Value::as_str)
            .filter(|query| !query.is_empty())
            .map(|query| {
                crate::utils::truncate::truncate_to_width(
                    query,
                    crate::constants::tool_limits::TOOL_SUMMARY_MAX_LENGTH,
                )
            })
    }

    fn get_activity_description(&self, args: &serde_json::Value) -> Option<String> {
        Some(self.get_tool_use_summary(args).map_or_else(
            || "Searching code".to_string(),
            |query| format!("Searching code for {query}"),
        ))
    }

    fn max_result_size_chars(&self) -> usize {
        100_000
    }

    fn normalize_input(&self, args: &serde_json::Value) -> serde_json::Value {
        let mut normalized = args.clone();
        crate::utils::semantic_number::preprocess_object_field(&mut normalized, "max_results");
        normalized
    }

    fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
        true
    }

    fn is_read_only(&self, _args: &serde_json::Value) -> bool {
        true
    }

    fn is_search_or_read_command(
        &self,
        _args: &serde_json::Value,
    ) -> Option<crate::tool::SearchOrReadCommand> {
        Some(crate::tool::SearchOrReadCommand {
            is_search: true,
            is_read: false,
        })
    }

    fn get_path(&self, args: &serde_json::Value) -> Option<String> {
        Some(
            args.get("path")
                .and_then(serde_json::Value::as_str)
                .filter(|path| !path.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| {
                    crate::bootstrap::state::get_original_cwd()
                        .display()
                        .to_string()
                }),
        )
    }

    fn to_auto_classifier_input(&self, args: &serde_json::Value) -> String {
        args.get("query")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    fn prepare_permission_matcher(
        &self,
        args: &serde_json::Value,
    ) -> Option<crate::tool::PermissionPatternMatcher> {
        let query = args.get("query")?.as_str()?.to_string();
        Some(Box::new(move |rule_pattern| {
            crate::utils::permissions::shell_rule_matching::match_wildcard_pattern(
                rule_pattern,
                &query,
                false,
            )
        }))
    }

    fn validate_input(
        &self,
        args: &serde_json::Value,
        _context: &crate::tool::ToolUseContext,
    ) -> crate::tool::ValidationResult {
        if args
            .get("query")
            .and_then(serde_json::Value::as_str)
            .is_none_or(|query| query.trim().is_empty())
        {
            return crate::tool::ValidationResult::Error {
                message: "query must not be empty".to_string(),
                error_code: 1,
            };
        }
        if let Some(value) = args.get("max_results")
            && value
                .as_u64()
                .is_none_or(|value| !(1..=30).contains(&value))
        {
            return crate::tool::ValidationResult::Error {
                message: "max_results must be between 1 and 30".to_string(),
                error_code: 1,
            };
        }
        crate::tool::ValidationResult::Ok
    }

    fn check_permissions(
        &self,
        args: &serde_json::Value,
        context: &crate::tool::ToolUseContext,
    ) -> crate::utils::permissions::permission_result::PermissionResult {
        let cwd = context.effective_cwd();
        let path = args
            .get("path")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_else(|| cwd.to_str().unwrap_or("."));
        crate::utils::permissions::filesystem::check_read_permission_for_tool(
            path,
            args,
            &context.tool_permission_context,
            &cwd,
        )
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
        let args = args.clone();
        let cwd = context.effective_cwd();
        let abort = context.abort_controller.clone();
        let permission_context = context.tool_permission_context.clone();
        Box::pin(async move {
            let result = request_from_args(&args).map(|request| async move {
                // @nyan: retrieval 4.11 — the Decision Provider is optional;
                // without it every decision falls back to 4.3's rules.
                let provider = crate::utils::settings::get_settings_with_errors()
                    .settings
                    .nyan
                    .as_ref()
                    .map(crate::services::decision::DecisionProvider::from_nyan);
                let outcome = crate::services::retrieval::search_with_provider(
                    request.clone(),
                    &cwd,
                    &abort,
                    &permission_context,
                    provider.as_ref(),
                )
                .await;
                let content = crate::services::retrieval::render::render_outcome(
                    &outcome,
                    request.bounded_max_results(),
                );
                Output {
                    content,
                    num_results: outcome.candidates.len(),
                    truncated: outcome.truncated,
                }
            });
            match result {
                Ok(future) => crate::tool::ToolResult {
                    data: crate::tool::ToolOutput::CodeSearch(future.await),
                    new_messages: Vec::new(),
                },
                Err(error) => crate::tool::ToolResult {
                    data: crate::tool::ToolOutput::Composed {
                        content: error,
                        status: crate::types::message::ToolResultStatus::Error,
                    },
                    new_messages: Vec::new(),
                },
            }
        })
    }

    fn map_tool_result_to_tool_result_block_param(
        &self,
        data: &crate::tool::ToolOutput,
        _tool_use_id: &str,
    ) -> (String, crate::types::message::ToolResultStatus) {
        match data {
            crate::tool::ToolOutput::CodeSearch(output) => (
                output.content.clone(),
                crate::types::message::ToolResultStatus::Success,
            ),
            crate::tool::ToolOutput::Composed { content, status } => (content.clone(), *status),
            _ => (
                "CodeSearch returned an invalid output variant".to_string(),
                crate::types::message::ToolResultStatus::Error,
            ),
        }
    }

    fn tool_use_result(&self, data: &crate::tool::ToolOutput) -> Option<serde_json::Value> {
        match data {
            crate::tool::ToolOutput::CodeSearch(output) => Some(serde_json::json!({
                "content": output.content,
                "numResults": output.num_results,
                "truncated": output.truncated,
            })),
            crate::tool::ToolOutput::Composed {
                content,
                status: crate::types::message::ToolResultStatus::Error,
            } => Some(serde_json::Value::String(format!("Error: {content}"))),
            _ => None,
        }
    }

    fn extract_search_text(&self, data: &crate::tool::ToolOutput) -> Option<String> {
        match data {
            crate::tool::ToolOutput::CodeSearch(output) => Some(output.content.clone()),
            _ => None,
        }
    }

    fn is_result_truncated(&self, data: &crate::tool::ToolOutput) -> bool {
        matches!(
            data,
            crate::tool::ToolOutput::CodeSearch(Output {
                truncated: true,
                ..
            })
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_has_small_strict_surface_and_caps_results() {
        let schema = code_search_tool_schema();
        assert_eq!(schema.name, "CodeSearch");
        assert_eq!(schema.input_schema["additionalProperties"], false);
        assert_eq!(
            schema.input_schema["required"],
            serde_json::json!(["query"])
        );
        assert!(
            schema.input_schema["properties"]
                .get("max_results")
                .is_some()
        );
        assert_eq!(
            request_from_args(&serde_json::json!({"query":"q","max_results":99}))
                .unwrap()
                .max_results,
            30
        );
    }
}
