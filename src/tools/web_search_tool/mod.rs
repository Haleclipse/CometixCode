//! WebSearch tool metadata and UI.
//!
//! Maps to:
//! - CC `tools/WebSearchTool/WebSearchTool.ts`
//! - CC `tools/WebSearchTool/prompt.ts`
//! - CC `tools/WebSearchTool/UI.tsx`
//!
//! Execution lives in this module, dispatched from `services/tools/tool_execution.rs`.

pub mod prompt;
pub mod ui;

/// Maps to CC `tools/WebSearchTool/WebSearchTool.ts` `isEnabled(...)`.
pub fn is_web_search_tool_enabled() -> bool {
    match crate::utils::model::providers::get_api_provider() {
        crate::utils::model::providers::ApiProvider::FirstParty => true,
        crate::utils::model::providers::ApiProvider::Foundry => true,
        crate::utils::model::providers::ApiProvider::Vertex => {
            let model = crate::utils::model::model::get_main_loop_model().to_ascii_lowercase();
            model.contains("claude-opus-4")
                || model.contains("claude-sonnet-4")
                || model.contains("claude-haiku-4")
        }
        crate::utils::model::providers::ApiProvider::Bedrock => false,
    }
}

/// Maps to CC `tools/WebSearchTool/WebSearchTool.ts` `call(...)` local
/// `useHaiku = getFeatureValue_CACHED_MAY_BE_STALE('tengu_plum_vx3', false)`.
fn web_search_use_haiku() -> bool {
    crate::utils::feature_flags::feature_enabled(
        crate::utils::feature_flags::FeatureFlag::WebSearchSmallFastModel,
    )
}

/// Maps to CC `tools/WebSearchTool/WebSearchTool.ts` `call(...)` model,
/// thinkingConfig, and `toolChoice` selection under `tengu_plum_vx3`.
fn web_search_model_profile(
    use_haiku: bool,
    _global_config: &crate::utils::config::GlobalConfig,
) -> (
    String,
    crate::utils::thinking::ThinkingConfig,
    Option<serde_json::Value>,
) {
    if use_haiku {
        (
            crate::utils::model::model::get_small_fast_model(),
            crate::utils::thinking::ThinkingConfig::Disabled,
            Some(serde_json::json!({
                "type": "tool",
                "name": "web_search",
            })),
        )
    } else {
        (
            crate::utils::model::model::get_main_loop_model(),
            // Fallback when no REPL launch config; CC alwaysThinkingEnabled path.
            crate::utils::thinking::production_thinking_config_from_env_and_settings(
                &crate::utils::settings::get_initial_settings(),
            ),
            None,
        )
    }
}

/// Maps to CC `WebSearchTool.inputSchema`.
/// Maps to: CC `WebSearchTool.ts:25-37` `inputSchema`.
pub fn input_schema() -> &'static crate::utils::zod::Schema {
    static SCHEMA: std::sync::OnceLock<crate::utils::zod::Schema> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| {
        use crate::utils::zod;
        zod::strict_object(vec![
            (
                "query",
                zod::string().min(2).describe("The search query to use"),
            ),
            (
                "allowed_domains",
                zod::array(zod::string())
                    .optional()
                    .describe("Only include search results from these domains"),
            ),
            (
                "blocked_domains",
                zod::array(zod::string())
                    .optional()
                    .describe("Never include search results from these domains"),
            ),
        ])
    })
}

pub fn web_search_tool_schema() -> crate::types::tools::Tool {
    crate::types::tools::Tool {
        name: prompt::WEB_SEARCH_TOOL_NAME.to_string(),
        description: prompt::get_web_search_prompt(),
        input_schema: crate::utils::zod_to_json_schema::zod_to_json_schema(input_schema()),
        ..Default::default()
    }
}

/// CC `tools/WebSearchTool/WebSearchTool.ts` `SearchResult` (:38).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WebSearchHit {
    pub(crate) title: String,
    pub(crate) url: String,
}

/// CC `tools/WebSearchTool/WebSearchTool.ts` `SearchResult` (:38).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WebSearchSearchResult {
    pub(crate) tool_use_id: String,
    pub(crate) content: Vec<WebSearchHit>,
}

/// CC `tools/WebSearchTool/WebSearchTool.ts` output `results` item (:61).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WebSearchResultItem {
    SearchResult(WebSearchSearchResult),
    Text(String),
}

/// Maps to: CC `tools/WebSearchTool/WebSearchTool.ts:69` `export type Output
/// = z.infer<OutputSchema>` (schema at :56-67) — the single type the tool
/// yields from `call()`, records as the message's `toolUseResult`, and the
/// render path recovers via `outputSchema.safeParse` ([`ui::parse_output`] is
/// the Rust stand-in).
#[derive(Clone, Debug, PartialEq)]
pub struct Output {
    pub query: String,
    pub results: Vec<WebSearchResultItem>,
    pub duration_seconds: f64,
}

fn web_search_error_output(message: impl Into<String>) -> crate::tool::ToolOutput {
    let message = message.into();
    // The `Error: …` raw string rides the row via the `tool_use_result`
    // trait projection, not a display variant.
    crate::tool::ToolOutput::Composed {
        content: format!("<tool_use_error>{message}</tool_use_error>"),
        status: crate::types::message::ToolResultStatus::Error,
    }
}

fn optional_string_array(input: &serde_json::Value, key: &str) -> Option<Vec<String>> {
    input.get(key)?.as_array().map(|values| {
        values
            .iter()
            .filter_map(|value| value.as_str().map(str::to_string))
            .collect()
    })
}

fn non_empty_optional_string_array(input: &serde_json::Value, key: &str) -> Option<Vec<String>> {
    optional_string_array(input, key).filter(|values| !values.is_empty())
}

/// Maps to CC `tools/WebSearchTool/WebSearchTool.ts` `makeToolSchema(...)`.
fn make_tool_schema(input: &serde_json::Value) -> serde_json::Value {
    let mut schema = serde_json::Map::new();
    schema.insert(
        "type".to_string(),
        serde_json::Value::String("web_search_20250305".to_string()),
    );
    schema.insert(
        "name".to_string(),
        serde_json::Value::String("web_search".to_string()),
    );
    schema.insert(
        "max_uses".to_string(),
        serde_json::Value::Number(serde_json::Number::from(8)),
    );
    if let Some(allowed_domains) = optional_string_array(input, "allowed_domains") {
        schema.insert(
            "allowed_domains".to_string(),
            serde_json::Value::Array(
                allowed_domains
                    .into_iter()
                    .map(serde_json::Value::String)
                    .collect(),
            ),
        );
    }
    if let Some(blocked_domains) = optional_string_array(input, "blocked_domains") {
        schema.insert(
            "blocked_domains".to_string(),
            serde_json::Value::Array(
                blocked_domains
                    .into_iter()
                    .map(serde_json::Value::String)
                    .collect(),
            ),
        );
    }
    serde_json::Value::Object(schema)
}

/// Maps to CC `tools/WebSearchTool/WebSearchTool.ts`
/// `makeOutputFromSearchResponse(...)`.
fn make_output_from_search_response(
    result: &[crate::types::message::AssistantContent],
    query: &str,
    duration_seconds: f64,
) -> Output {
    let mut results = Vec::new();
    let mut text_acc = String::new();
    let mut in_text = true;

    for block in result {
        match block {
            crate::types::message::AssistantContent::ServerToolUse(_) => {
                if in_text {
                    in_text = false;
                    let text = text_acc.trim();
                    if !text.is_empty() {
                        results.push(WebSearchResultItem::Text(text.to_string()));
                    }
                    text_acc.clear();
                }
            }
            crate::types::message::AssistantContent::WebSearchToolResult {
                tool_use_id,
                content,
            } => {
                if let Some(items) = content.as_array() {
                    let hits = items
                        .iter()
                        .map(|item| WebSearchHit {
                            title: item
                                .get("title")
                                .and_then(|value| value.as_str())
                                .unwrap_or_default()
                                .to_string(),
                            url: item
                                .get("url")
                                .and_then(|value| value.as_str())
                                .unwrap_or_default()
                                .to_string(),
                        })
                        .collect();
                    results.push(WebSearchResultItem::SearchResult(WebSearchSearchResult {
                        tool_use_id: tool_use_id.0.clone(),
                        content: hits,
                    }));
                } else {
                    let error_code = content
                        .get("error_code")
                        .and_then(|value| value.as_str())
                        .unwrap_or("unknown");
                    let error_message = format!("Web search error: {error_code}");
                    tracing::error!("{error_message}");
                    results.push(WebSearchResultItem::Text(error_message));
                }
            }
            crate::types::message::AssistantContent::Text(text) => {
                if in_text {
                    text_acc.push_str(text);
                } else {
                    in_text = true;
                    text_acc = text.clone();
                }
            }
            crate::types::message::AssistantContent::Thinking { .. }
            | crate::types::message::AssistantContent::RedactedThinking { .. }
            | crate::types::message::AssistantContent::ToolUse(_)
            | crate::types::message::AssistantContent::Advisor { .. }
            | crate::types::message::AssistantContent::MessageIdentity(_) => {}
        }
    }

    if !text_acc.is_empty() {
        results.push(WebSearchResultItem::Text(text_acc.trim().to_string()));
    }

    Output {
        query: query.to_string(),
        results,
        duration_seconds,
    }
}

fn json_stringify_hits(hits: &[WebSearchHit]) -> String {
    let value = serde_json::Value::Array(
        hits.iter()
            .map(|hit| {
                serde_json::json!({
                    "title": hit.title,
                    "url": hit.url,
                })
            })
            .collect(),
    );
    serde_json::to_string(&value).unwrap_or_else(|_| "[]".to_string())
}

fn emit_web_search_progress(
    on_progress: Option<crate::tool::ToolCallProgressFn<'_>>,
    progress: crate::types::tools::ToolProgress,
) {
    if let Some(on_progress) = on_progress {
        on_progress(progress);
    }
}

/// Maps to CC `tools/WebSearchTool/WebSearchTool.ts` `call(...)` progress
/// extraction from `server_tool_use` and `web_search_tool_result` stream events.
fn web_search_progress_from_stream_content(
    item: &crate::services::api::claude::ClaudeStreamItem,
    query: &str,
    tool_use_queries: &mut std::collections::HashMap<String, String>,
    outer_tool_use_id: &crate::types::ids::ToolUseId,
) -> Option<crate::types::tools::ToolProgress> {
    match item {
        crate::services::api::claude::ClaudeStreamItem::ToolUse {
            id,
            name,
            input,
            is_server: true,
        } if name == "web_search" => {
            let search_query = input.get("query").and_then(|value| value.as_str())?;
            let changed = tool_use_queries
                .get(id)
                .map_or(true, |previous| previous != search_query);
            if !changed {
                return None;
            }
            tool_use_queries.insert(id.clone(), search_query.to_string());
            Some(crate::types::tools::ToolProgress::WebSearchQueryUpdate {
                tool_use_id: outer_tool_use_id.clone(),
                query: search_query.to_string(),
            })
        }
        crate::services::api::claude::ClaudeStreamItem::WebSearchToolResult {
            tool_use_id,
            content,
        } => {
            let actual_query = tool_use_queries
                .get(tool_use_id)
                .map(String::as_str)
                .unwrap_or(query);
            let result_count = content.as_array().map(Vec::len).unwrap_or(0);
            Some(
                crate::types::tools::ToolProgress::WebSearchResultsReceived {
                    tool_use_id: outer_tool_use_id.clone(),
                    query: actual_query.to_string(),
                    result_count,
                },
            )
        }
        _ => None,
    }
}

/// Maps to CC `tools/WebSearchTool/WebSearchTool.ts` `call(...)` using
/// `queryModelWithStreaming(...)` and forwarding `WebSearchProgress` updates.
async fn query_web_search_model_with_streaming(
    messages: &[crate::types::message::Message],
    system_prompt: &crate::services::api::claude::SystemPrompt,
    thinking_config: &crate::utils::thinking::ThinkingConfig,
    options: &crate::services::api::claude::Options,
    query: &str,
    outer_tool_use_id: &str,
    started_at: std::time::Instant,
    on_progress: Option<crate::tool::ToolCallProgressFn<'_>>,
    abort_controller: &crate::tool::AbortController,
) -> anyhow::Result<Output> {
    let mut stream = crate::services::api::claude::query_model_with_streaming(
        messages,
        system_prompt,
        thinking_config,
        &[],
        options,
    )
    .await?;
    let mut final_assistant = None;
    let mut last_error = None;
    let mut tool_use_queries = std::collections::HashMap::<String, String>::new();
    let outer_tool_use_id = crate::types::ids::ToolUseId(outer_tool_use_id.to_string());

    let mut abort_signal = abort_controller.signal();
    loop {
        let item = tokio::select! {
            item = stream.recv() => item,
            _ = abort_signal.aborted() => return Err(anyhow::anyhow!("Operation aborted")),
        };
        let Some(item) = item else {
            break;
        };
        match item {
            crate::services::api::claude::QueryModelStreamItem::Content(content) => {
                if let Some(progress) = web_search_progress_from_stream_content(
                    &content,
                    query,
                    &mut tool_use_queries,
                    &outer_tool_use_id,
                ) {
                    emit_web_search_progress(on_progress, progress);
                }
            }
            crate::services::api::claude::QueryModelStreamItem::Assistant(message) => {
                final_assistant = Some(message);
            }
            crate::services::api::claude::QueryModelStreamItem::AssistantDelta {
                stop_reason,
                usage,
            } => {
                if let Some(assistant) = final_assistant.as_mut() {
                    assistant.stop_reason = stop_reason;
                    assistant.usage = usage;
                }
            }
            crate::services::api::claude::QueryModelStreamItem::SystemError(error) => {
                last_error = Some(error);
            }
            crate::services::api::claude::QueryModelStreamItem::ModelFallback {
                original_model,
                fallback_model,
            } => {
                return Err(anyhow::Error::new(
                    crate::services::api::with_retry::FallbackTriggeredError {
                        original_model,
                        fallback_model,
                    },
                ));
            }
            // Retry heartbeats are non-terminal; keep waiting for the final
            // assistant (CC's consumer ignores SystemAPIErrorMessage yields).
            crate::services::api::claude::QueryModelStreamItem::SystemApiError(_)
            | crate::services::api::claude::QueryModelStreamItem::Stream(_)
            | crate::services::api::claude::QueryModelStreamItem::CompletedContent(_)
            | crate::services::api::claude::QueryModelStreamItem::StreamingFallback => {}
        }
    }

    if let Some(assistant) = final_assistant {
        return Ok(make_output_from_search_response(
            &assistant.content,
            query,
            started_at.elapsed().as_secs_f64(),
        ));
    }
    if let Some(error) = last_error {
        let details = error.error_details.unwrap_or(error.api_error);
        return Err(anyhow::anyhow!(
            "WebSearch query failed: {} ({})",
            error.content,
            details
        ));
    }
    Err(anyhow::anyhow!(
        "WebSearch query ended without an AssistantMessage"
    ))
}

/// Behavioral half of CC `WebSearchTool` — dispatched via `crate::tool::ToolCall`.
pub(crate) struct WebSearchTool;

impl crate::tool::ToolCall for WebSearchTool {
    fn name(&self) -> &'static str {
        "WebSearch"
    }

    /// Maps to: CC `WebSearchTool.ts:223-225` `async prompt() { return
    /// getWebSearchPrompt() }` — same source the wire schema renders eagerly.
    fn prompt(
        &self,
        _tool: &crate::types::tools::Tool,
        _options: &crate::tool::ToolPromptOptions<'_>,
    ) -> String {
        prompt::get_web_search_prompt()
    }

    fn is_enabled(&self) -> bool {
        is_web_search_tool_enabled()
    }

    /// Maps to: CC `WebSearchTool.isConcurrencySafe(...)` read-only default.
    fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
        true
    }

    /// Maps to: CC `WebSearchTool.isReadOnly()`.
    fn is_read_only(&self, _args: &serde_json::Value) -> bool {
        true
    }

    /// Maps to: CC `WebSearchTool.searchHint`.
    fn search_hint(&self) -> Option<&'static str> {
        Some("search the web for current information")
    }

    /// Maps to: CC `WebSearchTool.ts:160-162` `userFacingName()`.
    fn user_facing_name(&self, _args: Option<&serde_json::Value>) -> String {
        "Web Search".to_string()
    }

    /// Maps to: CC `WebSearchTool.ts:19,163` mounting `UI.tsx#getToolUseSummary`.
    fn get_tool_use_summary(&self, args: &serde_json::Value) -> Option<String> {
        crate::tools::web_search_tool::ui::get_tool_use_summary(Some(args))
    }

    /// Maps to: CC `WebSearchTool.ts:164-167` `getActivityDescription(input)`.
    fn get_activity_description(&self, args: &serde_json::Value) -> Option<String> {
        Some(
            match crate::tools::web_search_tool::ui::get_tool_use_summary(Some(args)) {
                Some(summary) if !summary.is_empty() => format!("Searching for {summary}"),
                _ => "Searching the web".to_string(),
            },
        )
    }

    /// Maps to: CC `WebSearchTool.ts:229-234` `extractSearchText() → ''` —
    /// implemented-and-empty, distinct from the trait's None ("tool didn't
    /// implement it"): the result render shows only "Did N searches" chrome,
    /// so indexing results[] strings would produce phantom matches.
    fn extract_search_text(&self, _data: &crate::tool::ToolOutput) -> Option<String> {
        Some(String::new())
    }

    /// Maps to: CC `WebSearchTool.description(input)` (:157-159).
    fn description(&self, args: &serde_json::Value) -> String {
        let query = args
            .get("query")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        format!("Claude wants to search the web for: {query}")
    }

    /// Maps to: CC `WebSearchTool.shouldDefer`.
    fn should_defer(&self) -> bool {
        true
    }

    fn max_result_size_chars(&self) -> usize {
        100_000
    }

    /// Maps to: CC `WebSearchTool.toAutoClassifierInput`.
    fn to_auto_classifier_input(&self, args: &serde_json::Value) -> String {
        args.get("query")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string()
    }

    /// Maps to: CC `WebSearchTool.validateInput`.
    fn validate_input(
        &self,
        args: &serde_json::Value,
        _context: &crate::tool::ToolUseContext,
    ) -> crate::tool::ValidationResult {
        let query = args
            .get("query")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        if query.is_empty() {
            return crate::tool::ValidationResult::error("Error: Missing query", 1);
        }
        if non_empty_optional_string_array(args, "allowed_domains").is_some()
            && non_empty_optional_string_array(args, "blocked_domains").is_some()
        {
            return crate::tool::ValidationResult::error(
                "Error: Cannot specify both allowed_domains and blocked_domains in the same request",
                2,
            );
        }
        crate::tool::ValidationResult::Ok
    }

    /// Maps to: CC `WebSearchTool.checkPermissions`.
    fn check_permissions(
        &self,
        _args: &serde_json::Value,
        _context: &crate::tool::ToolUseContext,
    ) -> crate::utils::permissions::permission_result::PermissionResult {
        use crate::types::permissions::{
            PermissionBehavior, PermissionRuleValue, PermissionUpdate, PermissionUpdateDestination,
        };
        crate::utils::permissions::permission_result::PermissionResult::Passthrough {
            message: "WebSearchTool requires permission.".to_string(),
            decision_reason: None,
            suggestions: vec![PermissionUpdate::AddRules {
                destination: PermissionUpdateDestination::LocalSettings,
                behavior: PermissionBehavior::Allow,
                rules: vec![PermissionRuleValue::new(prompt::WEB_SEARCH_TOOL_NAME, None)],
            }],
            blocked_path: None,
            pending_classifier_check: None,
        }
    }

    fn call<'a>(
        &'a self,
        args: &'a serde_json::Value,
        request: &'a crate::types::permissions::PermissionRequest,
        context: &'a crate::tool::ToolUseContext,
        _can_use_tool: Option<crate::tool::CanUseToolFn<'a>>,
        _parent_message: Option<&'a crate::types::message::AssistantMessage>,
        on_progress: Option<crate::tool::ToolCallProgressFn<'a>>,
    ) -> futures::future::BoxFuture<'a, crate::tool::ToolResult> {
        Box::pin(async move {
            let Some(query) = args.get("query").and_then(|value| value.as_str()) else {
                return crate::tool::ToolResult {
                    data: web_search_error_output("Error: Missing query"),
                    new_messages: Vec::new(),
                };
            };

            let start = std::time::Instant::now();
            let user_message =
                crate::types::message::Message::User(crate::types::message::UserMessage {
                    uuid: uuid::Uuid::new_v4().to_string(),
                    timestamp: chrono::Utc::now(),
                    content: vec![crate::types::message::UserContent::Text(format!(
                        "Perform a web search for the query: {query}"
                    ))],
                    is_compact_summary: false,
                    plan_content: None,
                    image_paste_ids: None,
                    is_visible_in_transcript_only: false,
                    mcp_meta: None,
                    source_tool_assistant_uuid: None,
                    permission_mode: None,
                    origin: None,
                    summarize_metadata: None,
                });
            let system_prompt =
                vec!["You are an assistant for performing a web search tool use".to_string()];
            let global_config = crate::utils::config::load_global_config();
            // CC :262 evaluates useHaiku once for the whole call.
            let use_haiku = web_search_use_haiku();
            let (mut model, mut thinking_config, tool_choice) =
                web_search_model_profile(use_haiku, &global_config);
            if !use_haiku {
                // CC :280 `context.options.mainLoopModel` and :273-275
                // `context.options.thinkingConfig` are the LIVE session values;
                // the profile's settings-derived pair only covers a bare
                // context (unit tests / headless call without hydration).
                if let Some(live) = context.main_loop_model.clone() {
                    model = live;
                }
                if let Some(live) = context.thinking_config.clone() {
                    thinking_config = live;
                }
            }
            let mut options =
                crate::services::api::claude::Options::new(model, "web_search_tool".to_string());
            options.is_non_interactive_session = context.is_non_interactive_session;
            // CC :283 `!!context.options.appendSystemPrompt` — JS falsy: the
            // empty string reads false.
            options.has_append_system_prompt = context
                .append_system_prompt
                .as_deref()
                .is_some_and(|prompt| !prompt.is_empty());
            options.tool_choice = tool_choice;
            options.extra_tool_schemas = Some(vec![make_tool_schema(args)]);
            // CC :286-289: agents / agentId ride the options bag. effortValue
            // is CC's live `appState.effortValue` (:267/:289); the context
            // snapshot hydrated at query start is this port's nearest carrier.
            options.agents = context.agent_definitions.active_agents.clone();
            options.agent_id = context.agent_id.clone().map(crate::types::ids::AgentId);
            options.effort_value = context.effort_value.clone();
            options.mcp_tools = Vec::new();
            options.query_tracking = context.query_tracking.clone();
            options.abort_signal = Some(context.abort_controller.signal());

            let data = match query_web_search_model_with_streaming(
                &[user_message],
                &system_prompt,
                &thinking_config,
                &options,
                query,
                &request.tool_use_id,
                start,
                on_progress,
                &context.abort_controller,
            )
            .await
            {
                Ok(output) => crate::tool::ToolOutput::WebSearch(output),
                Err(error) => web_search_error_output(format!("Error: {error}")),
            };

            crate::tool::ToolResult {
                data,
                new_messages: Vec::new(),
            }
        })
    }

    /// Maps to: CC `tools/WebSearchTool/WebSearchTool.ts`
    /// `mapToolResultToToolResultBlockParam` (:374-406).
    fn map_tool_result_to_tool_result_block_param(
        &self,
        data: &crate::tool::ToolOutput,
        _tool_use_id: &str,
    ) -> (String, crate::types::message::ToolResultStatus) {
        match data {
            crate::tool::ToolOutput::WebSearch(output) => {
                let mut formatted_output =
                    format!("Web search results for query: \"{}\"\n\n", output.query);
                for result in &output.results {
                    match result {
                        WebSearchResultItem::Text(text) => {
                            formatted_output.push_str(text);
                            formatted_output.push_str("\n\n");
                        }
                        WebSearchResultItem::SearchResult(result) => {
                            if result.content.is_empty() {
                                formatted_output.push_str("No links found.\n\n");
                            } else {
                                formatted_output.push_str("Links: ");
                                formatted_output.push_str(&json_stringify_hits(&result.content));
                                formatted_output.push_str("\n\n");
                            }
                        }
                    }
                }
                formatted_output.push_str(
                    "\nREMINDER: You MUST include the sources above in your response to the user using markdown hyperlinks.",
                );
                (
                    formatted_output.trim().to_string(),
                    crate::types::message::ToolResultStatus::Success,
                )
            }
            crate::tool::ToolOutput::Composed {
                content, status, ..
            } => (content.clone(), *status),
            _ => (
                "<tool_use_error>WebSearch returned an unexpected output variant</tool_use_error>"
                    .to_string(),
                crate::types::message::ToolResultStatus::Error,
            ),
        }
    }

    /// WebSearch carries no display shape — the raw the trait projects
    /// below is what renders (`renderToolResultMessage` parses it with the
    /// tool's own output schema). A live `Output` is typed, so its projection
    /// always parses; the emit gate never fires here, unlike the cold paths
    /// that must gate on `ui::parse_output`.
    /// Maps to: CC recording WebSearchTool's `Output` as the message's
    /// `toolUseResult`. A failure records the `Error: …` string, matching the
    /// string `toolUseResult` CC keeps for errored searches.
    fn tool_use_result(&self, data: &crate::tool::ToolOutput) -> Option<serde_json::Value> {
        match data {
            crate::tool::ToolOutput::WebSearch(output) => {
                Some(crate::tools::web_search_tool::ui::output_to_value(output))
            }
            crate::tool::ToolOutput::Composed {
                content,
                status: crate::types::message::ToolResultStatus::Error,
                ..
            } => {
                // The Composed content carries the model-facing tag; CC's raw
                // string is the bare `Error: …` message.
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
    use crate::utils::test_env::TEST_ENV_LOCK;

    #[test]
    fn web_search_tool_schema_matches_official_input_shape() {
        let schema = web_search_tool_schema();
        assert_eq!(schema.name, "WebSearch");
        assert_eq!(
            schema.input_schema["required"],
            serde_json::json!(["query"])
        );
        assert_eq!(schema.input_schema["properties"]["query"]["minLength"], 2);
        for key in ["allowed_domains", "blocked_domains"] {
            assert!(
                schema
                    .input_schema
                    .pointer(&format!("/properties/{key}/items/type"))
                    .is_some()
            );
        }
        assert!(schema.description.contains("Sources:"));
        assert!(schema.description.contains("current month"));
    }

    #[test]
    fn web_search_is_enabled_matches_official_provider_and_model_gate() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("CLAUDE_CODE_USE_BEDROCK");
        crate::utils::process_env::remove("CLAUDE_CODE_USE_VERTEX");
        crate::utils::process_env::remove("CLAUDE_CODE_USE_FOUNDRY");
        crate::utils::process_env::remove("ANTHROPIC_MODEL");
        assert!(is_web_search_tool_enabled());

        crate::utils::process_env::set("CLAUDE_CODE_USE_BEDROCK", "1");
        assert!(!is_web_search_tool_enabled());
        crate::utils::process_env::remove("CLAUDE_CODE_USE_BEDROCK");

        crate::utils::process_env::set("CLAUDE_CODE_USE_VERTEX", "1");
        crate::utils::process_env::set("ANTHROPIC_MODEL", "claude-3-5-sonnet-20241022");
        assert!(!is_web_search_tool_enabled());
        crate::utils::process_env::set("ANTHROPIC_MODEL", "claude-sonnet-4-5-20250929");
        assert!(is_web_search_tool_enabled());
        crate::utils::process_env::remove("CLAUDE_CODE_USE_VERTEX");

        crate::utils::process_env::set("CLAUDE_CODE_USE_FOUNDRY", "1");
        crate::utils::process_env::set("ANTHROPIC_MODEL", "custom-foundry-model");
        assert!(is_web_search_tool_enabled());
        crate::utils::process_env::remove("CLAUDE_CODE_USE_FOUNDRY");
        crate::utils::process_env::remove("ANTHROPIC_MODEL");
    }

    #[test]
    fn web_search_model_profile_matches_official_haiku_experiment_shape() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("ANTHROPIC_MODEL");
        crate::utils::process_env::set("ANTHROPIC_SMALL_FAST_MODEL", "claude-haiku-test");
        crate::utils::process_env::set("MAX_THINKING_TOKENS", "2048");
        let config = crate::utils::config::GlobalConfig::default();

        let (main_model, main_thinking, main_tool_choice) =
            web_search_model_profile(false, &config);
        assert_eq!(
            main_model,
            crate::utils::model::model::get_main_loop_model()
        );
        assert!(matches!(
            main_thinking,
            crate::utils::thinking::ThinkingConfig::Enabled {
                budget_tokens: Some(2048),
            }
        ));
        assert!(main_tool_choice.is_none());

        let (haiku_model, haiku_thinking, haiku_tool_choice) =
            web_search_model_profile(true, &config);
        assert_eq!(haiku_model, "claude-haiku-test");
        assert_eq!(
            haiku_thinking,
            crate::utils::thinking::ThinkingConfig::Disabled
        );
        assert_eq!(
            haiku_tool_choice,
            Some(serde_json::json!({"type": "tool", "name": "web_search"}))
        );

        crate::utils::process_env::remove("ANTHROPIC_SMALL_FAST_MODEL");
        crate::utils::process_env::remove("MAX_THINKING_TOKENS");
        crate::utils::process_env::remove("ANTHROPIC_MODEL");
    }

    #[test]
    fn web_search_tool_schema_for_server_tool_matches_official_shape() {
        let schema = make_tool_schema(&serde_json::json!({
            "query": "rust ownership",
            "allowed_domains": ["doc.rust-lang.org"]
        }));
        assert_eq!(schema["type"], "web_search_20250305");
        assert_eq!(schema["name"], "web_search");
        assert_eq!(schema["max_uses"], 8);
        assert_eq!(
            schema["allowed_domains"],
            serde_json::json!(["doc.rust-lang.org"])
        );
        assert!(schema.get("blocked_domains").is_none());
    }

    #[test]
    fn web_search_output_from_response_maps_official_blocks() {
        let blocks = vec![
            crate::types::message::AssistantContent::Text("Intro text".to_string()),
            crate::types::message::AssistantContent::ServerToolUse(
                crate::types::message::ToolUseBlock {
                    id: crate::types::ids::ToolUseId("srvu_1".to_string()),
                    name: "web_search".to_string(),
                    input: serde_json::json!({"query": "rust ownership"}),
                },
            ),
            crate::types::message::AssistantContent::WebSearchToolResult {
                tool_use_id: crate::types::ids::ToolUseId("srvu_1".to_string()),
                content: serde_json::json!([
                    {"title": "Rust Book", "url": "https://doc.rust-lang.org/book/"}
                ]),
            },
            crate::types::message::AssistantContent::Text("Summary text".to_string()),
        ];
        let output = make_output_from_search_response(&blocks, "rust ownership", 1.25);
        assert_eq!(output.query, "rust ownership");
        assert_eq!(output.duration_seconds, 1.25);
        assert_eq!(output.results.len(), 3);
        assert!(matches!(
            &output.results[0],
            WebSearchResultItem::Text(text) if text == "Intro text"
        ));
        assert!(matches!(
            &output.results[1],
            WebSearchResultItem::SearchResult(result)
                if result.tool_use_id == "srvu_1"
                    && result.content[0].title == "Rust Book"
                    && result.content[0].url == "https://doc.rust-lang.org/book/"
        ));
        assert!(matches!(
            &output.results[2],
            WebSearchResultItem::Text(text) if text == "Summary text"
        ));
    }

    #[test]
    fn web_search_progress_from_stream_content_maps_official_events() {
        let outer_tool_use_id = crate::types::ids::ToolUseId("toolu_web_search".to_string());
        let mut tool_use_queries = std::collections::HashMap::new();

        let query_progress = web_search_progress_from_stream_content(
            &crate::services::api::claude::ClaudeStreamItem::ToolUse {
                id: "srvu_1".to_string(),
                name: "web_search".to_string(),
                input: serde_json::json!({"query": "rust ownership"}),
                is_server: true,
            },
            "fallback query",
            &mut tool_use_queries,
            &outer_tool_use_id,
        )
        .expect("server web_search tool use should emit query progress");
        assert!(matches!(
            query_progress,
            crate::types::tools::ToolProgress::WebSearchQueryUpdate { tool_use_id, query }
                if tool_use_id.0 == "toolu_web_search" && query == "rust ownership"
        ));

        let duplicate = web_search_progress_from_stream_content(
            &crate::services::api::claude::ClaudeStreamItem::ToolUse {
                id: "srvu_1".to_string(),
                name: "web_search".to_string(),
                input: serde_json::json!({"query": "rust ownership"}),
                is_server: true,
            },
            "fallback query",
            &mut tool_use_queries,
            &outer_tool_use_id,
        );
        assert!(duplicate.is_none());

        let results_progress = web_search_progress_from_stream_content(
            &crate::services::api::claude::ClaudeStreamItem::WebSearchToolResult {
                tool_use_id: "srvu_1".to_string(),
                content: serde_json::json!([
                    {"title": "Rust Book", "url": "https://doc.rust-lang.org/book/"},
                    {"title": "Rust Reference", "url": "https://doc.rust-lang.org/reference/"}
                ]),
            },
            "fallback query",
            &mut tool_use_queries,
            &outer_tool_use_id,
        )
        .expect("web_search_tool_result should emit result progress");
        assert!(matches!(
            results_progress,
            crate::types::tools::ToolProgress::WebSearchResultsReceived {
                tool_use_id,
                query,
                result_count,
            } if tool_use_id.0 == "toolu_web_search"
                && query == "rust ownership"
                && result_count == 2
        ));
    }

    #[tokio::test]
    async fn web_search_tool_call_missing_query_returns_error_without_network() {
        use crate::tool::ToolCall;

        let args = serde_json::json!({});
        let request = crate::utils::permissions::permissions::mock_permission_request_with_input(
            "perm-web-search".to_string(),
            "toolu_web_search".to_string(),
            "WebSearch".to_string(),
            "".to_string(),
            args.clone(),
            crate::types::permissions::PermissionMode::Default,
        );
        let tool = WebSearchTool;
        let result = tool
            .call(
                &args,
                &request,
                &crate::tool::ToolUseContext::default(),
                None,
                None,
                None,
            )
            .await;

        let (content, status) =
            tool.map_tool_result_to_tool_result_block_param(&result.data, "toolu_web_search");
        assert_eq!(status, crate::types::message::ToolResultStatus::Error);
        assert!(content.contains("Error: Missing query"));
        // The error rides the row as the bare `Error: …` raw string.
        assert!(matches!(
            tool.tool_use_result(&result.data),
            Some(serde_json::Value::String(raw)) if raw.contains("Error: Missing query")
        ));
    }
}
