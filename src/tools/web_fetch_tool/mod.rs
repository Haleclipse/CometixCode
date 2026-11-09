//! WebFetch tool metadata and UI.
//!
//! Maps to:
//! - CC `tools/WebFetchTool/WebFetchTool.ts`
//! - CC `tools/WebFetchTool/prompt.ts`
//! - CC `tools/WebFetchTool/UI.tsx`
//!
//! Execution lives in this module, dispatched from `services/tools/tool_execution.rs`.

pub mod preapproved;
pub mod prompt;
pub mod ui;
pub mod utils;

/// Maps to CC `WebFetchTool.inputSchema`.
/// Maps to: CC `WebFetchTool.ts:24-29` `inputSchema`.
pub fn input_schema() -> &'static crate::utils::zod::Schema {
    static SCHEMA: std::sync::OnceLock<crate::utils::zod::Schema> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| {
        use crate::utils::zod;
        zod::strict_object(vec![
            (
                "url",
                zod::string()
                    .url()
                    .describe("The URL to fetch content from"),
            ),
            // @nyan: prompt is an optional literal-excerpt request; range reads a snapshot.
            (
                "prompt",
                zod::string()
                    .optional()
                    .describe("Optional request for verbatim excerpts from the fetched content"),
            ),
            (
                "range",
                zod::string()
                    .optional()
                    .describe("Read a snapshot range such as chars:0-20000 or #section"),
            ),
        ])
    })
}

/// Maps to: CC `WebFetchTool.ts:50-64`
/// `webFetchToolInputToPermissionRuleContent(input)`. The failure arm is
/// CC's `` `input:${input.toString()}` `` — a plain object stringifies to
/// "[object Object]", and stored permission rules match on that exact text.
fn web_fetch_tool_input_to_permission_rule_content(input: &serde_json::Value) -> String {
    input
        .get("url")
        .and_then(|value| value.as_str())
        .and_then(preapproved::parse_web_fetch_url_host_path)
        .map(|(hostname, _)| format!("domain:{hostname}"))
        .unwrap_or_else(|| "input:[object Object]".to_string())
}

pub fn web_fetch_tool_schema() -> crate::types::tools::Tool {
    crate::types::tools::Tool {
        name: prompt::WEB_FETCH_TOOL_NAME.to_string(),
        description: prompt::tool_prompt().to_string(),
        input_schema: crate::utils::zod_to_json_schema::zod_to_json_schema(input_schema()),
        ..Default::default()
    }
}

/// Maps to: CC `tools/WebFetchTool/WebFetchTool.ts:48` `export type Output =
/// z.infer<OutputSchema>` (schema at :32-45) — the single type the tool
/// yields from `call()`, records as the message's `toolUseResult`, and the
/// render path recovers via `outputSchema.safeParse` ([`ui::parse_output`] is
/// the Rust stand-in).
#[derive(Clone, Debug, PartialEq)]
pub struct Output {
    pub bytes: usize,
    pub code: i64,
    pub code_text: String,
    pub result: String,
    pub duration_ms: u64,
    pub url: String,
}

fn web_fetch_error_output(message: impl Into<String>) -> crate::tool::ToolOutput {
    let message = message.into();
    // The `Error: …` raw string rides the row via the `tool_use_result`
    // trait projection, not a display variant.
    crate::tool::ToolOutput::Composed {
        content: format!("<tool_use_error>{message}</tool_use_error>"),
        status: crate::types::message::ToolResultStatus::Error,
    }
}

fn preapproved_markdown_direct_result(
    url: &str,
    content_type: &str,
    content: &str,
) -> Option<String> {
    // Maps to CC `WebFetchTool.call` preapproved text/markdown fast path.
    if utils::is_preapproved_url(url)
        && content_type.contains("text/markdown")
        && content.len() < utils::MAX_MARKDOWN_LENGTH
    {
        Some(content.to_string())
    } else {
        None
    }
}

fn append_binary_content_note(
    result: &mut String,
    content_type: &str,
    persisted_path: Option<&str>,
    persisted_size: Option<usize>,
    bytes: usize,
) {
    // Maps to CC `WebFetchTool.call` persistedPath result suffix.
    if let Some(path) = persisted_path {
        let size = persisted_size.unwrap_or(bytes);
        result.push_str(&format!(
            "\n\n[Binary content ({content_type}, {}) also saved to {path}]",
            crate::utils::format::format_file_size(size as u64)
        ));
    }
}

async fn run_nyan_web_fetch(
    args: &serde_json::Value,
    context: &crate::tool::ToolUseContext,
) -> Result<Output, String> {
    run_nyan_web_fetch_with(args, context, None).await
}

/// One direct-fetch + provider-fallback chain, with the mock seam when a test
/// supplies its own fetcher configuration.
async fn fetch_once(
    url: &str,
    context: &crate::tool::ToolUseContext,
    web: &crate::utils::settings::nyan::WebSettings,
    config: Option<&crate::services::web::fetcher::FetchConfig>,
) -> Result<
    crate::services::web::fallback::FetchChainResult,
    crate::services::web::fallback::FetchChainFailure,
> {
    let config = match config {
        Some(config) => config.clone(),
        None => crate::services::web::fetcher::FetchConfig::from_settings(web),
    };
    // @nyan: `web.fetch_fallback` runs on the same decision seam as the tool's
    // page check, so a scripted provider in tests reaches both.
    let provider = crate::services::web::decision::provider();
    let decisions = provider.as_ref().map(|provider| {
        crate::services::web::decision::WebDecisions::new(provider, &context.abort_controller)
    });
    crate::services::web::fallback::fetch_chain(
        url,
        &context.abort_controller,
        web,
        &config,
        decisions.as_ref(),
    )
    .await
}

/// `test_config` is the loopback-mock seam: only `#[cfg(test)]` callers pass it,
/// so a production build cannot loosen the host guard.
async fn run_nyan_web_fetch_with(
    args: &serde_json::Value,
    context: &crate::tool::ToolUseContext,
    test_config: Option<crate::services::web::fetcher::FetchConfig>,
) -> Result<Output, String> {
    let started = std::time::Instant::now();
    let url = args
        .get("url")
        .and_then(|value| value.as_str())
        .ok_or_else(|| "Error: Missing URL".to_string())?;
    let prompt = args
        .get("prompt")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    let range = crate::services::web::snapshot::parse_range(
        args.get("range").and_then(|value| value.as_str()),
    )
    .map_err(|error| error.to_string())?;
    let web = crate::services::web::settings()
        .ok_or_else(|| "Error: nyan.web is not configured".to_string())?;
    let _ = crate::services::web::snapshot::cleanup_retention(web.snapshot_retention_days());
    let fetch_config = match &test_config {
        Some(config) => config.clone(),
        None => crate::services::web::fetcher::FetchConfig::from_settings(&web),
    };
    let normalized_url = crate::services::web::fetcher::validate_target(url, &fetch_config)
        .map_err(|error| error.to_string())?
        .url
        .to_string();

    // @nyan: a `range` continuation reads an existing snapshot for as long as
    // the file exists and counts against neither the call nor the byte budget
    // (external-search 3.5 / 4.3). Only a fresh fetch is budgeted.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Origin {
        Cached,
        Continuation,
        Fresh,
    }
    let fresh = |fetched: crate::services::web::fallback::FetchChainResult| {
        (
            fetched.snapshot,
            fetched.direct_reason,
            Origin::Fresh,
            fetched.challenge_fallback_failed,
        )
    };
    let (snapshot, direct_reason, origin, challenge_fallback_failed) = if range.is_some() {
        match crate::services::web::snapshot::lookup_any(&normalized_url) {
            Some(snapshot) => (snapshot, None, Origin::Continuation, false),
            None => {
                crate::services::web::budget::begin_fetch(&web.fetch())
                    .map_err(|error| error.to_string())?;
                let fetched = fetch_once(url, context, &web, test_config.as_ref())
                    .await
                    .map_err(|error| error.to_string())?;
                crate::services::web::budget::add_fetch_bytes(
                    &web.fetch(),
                    fetched.snapshot.metadata.bytes as u64,
                )
                .map_err(|error| error.to_string())?;
                fresh(fetched)
            }
        }
    } else if let Some(snapshot) = crate::services::web::snapshot::lookup_latest(&normalized_url) {
        (snapshot, None, Origin::Cached, false)
    } else {
        crate::services::web::budget::begin_fetch(&web.fetch())
            .map_err(|error| error.to_string())?;
        let fetched = fetch_once(url, context, &web, test_config.as_ref())
            .await
            .map_err(|error| error.to_string())?;
        crate::services::web::budget::add_fetch_bytes(
            &web.fetch(),
            fetched.snapshot.metadata.bytes as u64,
        )
        .map_err(|error| error.to_string())?;
        fresh(fetched)
    };

    let selected = crate::services::web::snapshot::select_range(
        &snapshot.text,
        range.as_ref(),
        web.max_return_chars() as usize,
    )
    .map_err(|error| error.to_string())?;
    let body = if prompt.trim().is_empty() {
        selected.text.clone()
    } else {
        crate::services::web::extract::extract_prompt(
            prompt,
            &selected.text,
            web.extract_model.as_deref(),
            context.is_non_interactive_session,
            &context.abort_controller,
        )
        .await
        .map_err(|error| error.to_string())?
        .render()
    };
    let body = body
        .chars()
        .take(web.max_return_chars() as usize)
        .collect::<String>();
    // §7 post-hoc probe: a successful fetch is the usage evidence for any
    // retained `web.answers` / `web.page_answers` candidate. Match both the
    // requested URL and the post-redirect final URL.
    crate::services::decision::probe::observe_url(url);
    crate::services::decision::probe::observe_url(&snapshot.metadata.final_url);
    let fetched_at = snapshot.metadata.fetched_at.to_rfc3339();
    let source = &snapshot.metadata.final_url;
    let total = snapshot.text.chars().count();
    let origin_note = match (&direct_reason, origin) {
        (Some(reason), _) => format!(
            "(fetched via {}, {fetched_at}, {total} chars; direct fetch failed: {reason})",
            snapshot.metadata.fetched_by
        ),
        (None, Origin::Fresh) => format!(
            "(fetched {fetched_at}, {total} chars; source: {})",
            snapshot.metadata.fetched_by
        ),
        (None, Origin::Cached | Origin::Continuation) => format!(
            "(snapshot fetched {fetched_at}, {total} chars; source: {}; not refetched)",
            snapshot.metadata.fetched_by
        ),
    };
    // @nyan: web.fetch_fallback asked for a provider, every provider failed, and
    // the direct page was already accepted by the rules — say so, do not hide it.
    let origin_note = if challenge_fallback_failed {
        format!("{origin_note} [provider fallback failed; serving the direct fetch]")
    } else {
        origin_note
    };
    let continuation = if selected.end < selected.total {
        format!(". Continue with range=chars:{}-", selected.end)
    } else {
        String::new()
    };

    // @nyan: web.page_answers only appends a hint; the body is never rewritten.
    // The tool input's prompt is the only query this tool has (decision-provider 4.2).
    let mut hint = String::new();
    if !prompt.trim().is_empty()
        && let Some(provider) = crate::services::web::decision::provider()
    {
        let decisions =
            crate::services::web::decision::WebDecisions::new(&provider, &context.abort_controller);
        if decisions.page_lacks_answer(prompt, &selected.text).await {
            let next = crate::services::web::search::next_candidate(source);
            if let Some(next) = next.as_deref() {
                // §7: the hint names one candidate; probe whether it is used.
                crate::services::web::decision::register_next_candidate_probe(next);
            }
            hint.push_str(&format!(
                "\n[page may not answer the query — consider fetching: {}]",
                next.unwrap_or_else(|| "another search result".to_string())
            ));
        }
    }

    let result = format!(
        "{source}  {origin_note}\n\n<web_content source=\"{source}\">\n{body}\n</web_content>\n\n[Showing chars {}–{} of {}{}]{hint}",
        selected.start, selected.end, selected.total, continuation,
    );
    let code_text = reqwest::StatusCode::from_u16(snapshot.metadata.status)
        .ok()
        .and_then(|status| status.canonical_reason())
        .unwrap_or("Unknown")
        .to_string();
    Ok(Output {
        bytes: snapshot.metadata.bytes,
        code: i64::from(snapshot.metadata.status),
        code_text,
        result,
        duration_ms: started.elapsed().as_millis() as u64,
        url: url.to_string(),
    })
}

/// Behavioral half of CC `WebFetchTool` — dispatched via `crate::tool::ToolCall`.
pub(crate) struct WebFetchTool;

impl crate::tool::ToolCall for WebFetchTool {
    fn name(&self) -> &'static str {
        "WebFetch"
    }

    /// Maps to: CC `WebFetchTool.ts:181-191` `async prompt(_options)` — a
    /// constant template (auth warning + DESCRIPTION) that deliberately
    /// IGNORES the options bag: CC's in-source comment explains that toggling
    /// the warning on ToolSearch availability flickered the description
    /// between SDK query() calls and busted the prompt cache twice per
    /// flicker. Same source the wire schema renders eagerly.
    fn prompt(
        &self,
        _tool: &crate::types::tools::Tool,
        _options: &crate::tool::ToolPromptOptions<'_>,
    ) -> String {
        prompt::tool_prompt().to_string()
    }

    /// Maps to: CC `WebFetchTool.isConcurrencySafe(...)` read-only default.
    fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
        true
    }

    /// Maps to: CC `WebFetchTool.isReadOnly()` (:98-100).
    fn is_read_only(&self, _args: &serde_json::Value) -> bool {
        true
    }

    /// Maps to: CC `WebFetchTool.searchHint` (:68).
    fn search_hint(&self) -> Option<&'static str> {
        Some("fetch and extract content from a URL")
    }

    /// Maps to: CC `WebFetchTool.ts:11,84` mounting `UI.tsx#getToolUseSummary`.
    fn get_tool_use_summary(&self, args: &serde_json::Value) -> Option<String> {
        crate::tools::web_fetch_tool::ui::get_tool_use_summary(Some(args))
    }

    /// Maps to: CC `WebFetchTool.ts:85-88` `getActivityDescription(input)`.
    fn get_activity_description(&self, args: &serde_json::Value) -> Option<String> {
        Some(
            match crate::tools::web_fetch_tool::ui::get_tool_use_summary(Some(args)) {
                Some(summary) if !summary.is_empty() => format!("Fetching {summary}"),
                _ => "Fetching web page".to_string(),
            },
        )
    }

    /// Maps to: CC `WebFetchTool.description(input)` (:72-80).
    fn description(&self, args: &serde_json::Value) -> String {
        args.get("url")
            .and_then(|value| value.as_str())
            .and_then(preapproved::parse_web_fetch_url_host_path)
            .map_or_else(
                || "Claude wants to fetch content from this URL".to_string(),
                |(hostname, _)| format!("Claude wants to fetch content from {hostname}"),
            )
    }

    /// Maps to: CC `WebFetchTool.shouldDefer` (:71).
    fn should_defer(&self) -> bool {
        true
    }

    /// Maps to: CC `WebFetchTool.maxResultSizeChars` (:70).
    fn max_result_size_chars(&self) -> usize {
        100_000
    }

    /// Maps to: CC `WebFetchTool.userFacingName()` (:81-83).
    fn user_facing_name(&self, _args: Option<&serde_json::Value>) -> String {
        "Fetch".to_string()
    }

    /// Maps to: CC `WebFetchTool.toAutoClassifierInput(input)` (:101-103).
    fn to_auto_classifier_input(&self, args: &serde_json::Value) -> String {
        let url = args
            .get("url")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        match args.get("prompt").and_then(|value| value.as_str()) {
            Some(prompt) if !prompt.is_empty() => format!("{url}: {prompt}"),
            _ => url.to_string(),
        }
    }

    /// Maps to: CC `WebFetchTool.validateInput(input)` (:191-204).
    fn validate_input(
        &self,
        args: &serde_json::Value,
        _context: &crate::tool::ToolUseContext,
    ) -> crate::tool::ValidationResult {
        let url = args
            .get("url")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        if utils::is_parseable_url(url) {
            crate::tool::ValidationResult::Ok
        } else {
            crate::tool::ValidationResult::error(
                format!("Error: Invalid URL \"{url}\". The URL provided could not be parsed."),
                1,
            )
        }
    }

    /// Maps to: CC `WebFetchTool.checkPermissions(input, context)` (:104-180).
    fn check_permissions(
        &self,
        args: &serde_json::Value,
        context: &crate::tool::ToolUseContext,
    ) -> crate::utils::permissions::permission_result::PermissionResult {
        use crate::types::permissions::{
            PermissionBehavior, PermissionRuleValue, PermissionUpdate, PermissionUpdateDestination,
        };
        use crate::utils::permissions::permission_result::{
            PermissionDecisionReason, PermissionResult,
        };

        let permission_context = &context.tool_permission_context;

        if let Some((hostname, pathname)) = args
            .get("url")
            .and_then(|value| value.as_str())
            .and_then(preapproved::parse_web_fetch_url_host_path)
            && preapproved::is_preapproved_host(&hostname, &pathname)
        {
            return PermissionResult::Allow {
                updated_input: Some(args.clone()),
                user_modified: None,
                decision_reason: Some(PermissionDecisionReason::Other {
                    reason: "Preapproved host".to_string(),
                }),
                tool_use_id: None,
                accept_feedback: None,
                content_blocks: Vec::new(),
            };
        }

        let rule_content = web_fetch_tool_input_to_permission_rule_content(args);
        let suggestions = || {
            vec![PermissionUpdate::AddRules {
                destination: PermissionUpdateDestination::LocalSettings,
                behavior: PermissionBehavior::Allow,
                rules: vec![PermissionRuleValue::new(
                    prompt::WEB_FETCH_TOOL_NAME,
                    Some(rule_content.clone()),
                )],
            }]
        };
        let rule_for = |behavior: PermissionBehavior| {
            crate::utils::permissions::permissions::get_rule_by_contents_for_tool_name(
                permission_context,
                prompt::WEB_FETCH_TOOL_NAME,
                behavior,
            )
            .get(&rule_content)
            .cloned()
        };

        if let Some(rule) = rule_for(PermissionBehavior::Deny) {
            return PermissionResult::Deny {
                message: format!(
                    "{} denied access to {rule_content}.",
                    prompt::WEB_FETCH_TOOL_NAME
                ),
                decision_reason: PermissionDecisionReason::Rule { rule },
                tool_use_id: None,
            };
        }
        if let Some(rule) = rule_for(PermissionBehavior::Ask) {
            return PermissionResult::Ask {
                message: format!(
                    "Claude requested permissions to use {}, but you haven't granted it yet.",
                    prompt::WEB_FETCH_TOOL_NAME
                ),
                updated_input: None,
                decision_reason: Some(PermissionDecisionReason::Rule { rule }),
                suggestions: suggestions(),
                blocked_path: None,
                metadata: None,
                is_bash_security_check_for_misparsing: false,
                pending_classifier_check: None,
                content_blocks: Vec::new(),
            };
        }
        if let Some(rule) = rule_for(PermissionBehavior::Allow) {
            return PermissionResult::Allow {
                updated_input: Some(args.clone()),
                user_modified: None,
                decision_reason: Some(PermissionDecisionReason::Rule { rule }),
                tool_use_id: None,
                accept_feedback: None,
                content_blocks: Vec::new(),
            };
        }

        PermissionResult::Ask {
            message: format!(
                "Claude requested permissions to use {}, but you haven't granted it yet.",
                prompt::WEB_FETCH_TOOL_NAME
            ),
            updated_input: None,
            decision_reason: None,
            suggestions: suggestions(),
            blocked_path: None,
            metadata: None,
            is_bash_security_check_for_misparsing: false,
            pending_classifier_check: None,
            content_blocks: Vec::new(),
        }
    }

    fn call<'a>(
        &'a self,
        args: &'a serde_json::Value,
        request: &'a crate::types::permissions::PermissionRequest,
        context: &'a crate::tool::ToolUseContext,
        _can_use_tool: Option<crate::tool::CanUseToolFn<'a>>,
        _parent_message: Option<&'a crate::types::message::AssistantMessage>,
        _on_progress: Option<crate::tool::ToolCallProgressFn<'a>>,
    ) -> futures::future::BoxFuture<'a, crate::tool::ToolResult> {
        Box::pin(async move {
            let _ = request;
            let url = args
                .get("url")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            let prompt = args
                .get("prompt")
                .and_then(|value| value.as_str())
                .unwrap_or_default();

            // @nyan: configured web mode returns the verified snapshot directly;
            // the CC small-model rewrite path remains the explicit fallback.
            if crate::services::web::is_configured() {
                let data = match run_nyan_web_fetch(args, context).await {
                    Ok(output) => crate::tool::ToolOutput::WebFetch(output),
                    Err(error) => web_fetch_error_output(error),
                };
                return crate::tool::ToolResult {
                    data,
                    new_messages: Vec::new(),
                };
            }

            let start = std::time::Instant::now();
            let data = match utils::get_url_markdown_content(url, &context.abort_controller).await {
                Ok(utils::UrlMarkdownContent::Fetched(content)) => {
                    let result = if let Some(result) = preapproved_markdown_direct_result(
                        url,
                        &content.content_type,
                        &content.content,
                    ) {
                        Ok(result)
                    } else {
                        utils::apply_prompt_to_markdown(
                            prompt,
                            &content.content,
                            &context.abort_controller,
                            context.is_non_interactive_session,
                            utils::is_preapproved_url(url),
                        )
                        .await
                    };

                    match result {
                        Ok(mut result) => {
                            append_binary_content_note(
                                &mut result,
                                &content.content_type,
                                content.persisted_path.as_deref(),
                                content.persisted_size,
                                content.bytes,
                            );
                            crate::tool::ToolOutput::WebFetch(Output {
                                bytes: content.bytes,
                                code: i64::from(content.code),
                                code_text: content.code_text,
                                result,
                                duration_ms: start.elapsed().as_millis() as u64,
                                url: url.to_string(),
                            })
                        }
                        Err(error) => web_fetch_error_output(format!("Error: {error}")),
                    }
                }
                Err(error) => web_fetch_error_output(format!("Error: {error}")),
            };

            crate::tool::ToolResult {
                data,
                new_messages: Vec::new(),
            }
        })
    }

    /// Maps to: CC `tools/WebFetchTool/WebFetchTool.ts`
    /// `mapToolResultToToolResultBlockParam` (:303-309).
    fn map_tool_result_to_tool_result_block_param(
        &self,
        data: &crate::tool::ToolOutput,
        _tool_use_id: &str,
    ) -> (String, crate::types::message::ToolResultStatus) {
        match data {
            crate::tool::ToolOutput::WebFetch(output) => (
                output.result.clone(),
                crate::types::message::ToolResultStatus::Success,
            ),
            crate::tool::ToolOutput::Composed {
                content, status, ..
            } => (content.clone(), *status),
            _ => (
                "<tool_use_error>WebFetch returned an unexpected output variant</tool_use_error>"
                    .to_string(),
                crate::types::message::ToolResultStatus::Error,
            ),
        }
    }

    /// WebFetch carries no display shape — the raw the trait projects
    /// below is what renders (`renderToolResultMessage` parses it with the
    /// tool's own output schema). A live `Output` is typed, so its projection
    /// always parses; the emit gate never fires here, unlike the cold paths
    /// that must gate on `ui::parse_output`.
    /// Maps to: CC recording WebFetchTool's `Output` as the message's
    /// `toolUseResult`. A failure records the `Error: …` string, matching the
    /// string `toolUseResult` CC keeps for errored fetches.
    fn tool_use_result(&self, data: &crate::tool::ToolOutput) -> Option<serde_json::Value> {
        match data {
            crate::tool::ToolOutput::WebFetch(output) => {
                Some(crate::tools::web_fetch_tool::ui::output_to_value(output))
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

    #[test]
    fn web_fetch_tool_schema_matches_official_input_shape() {
        // The prompt switches to the nyan wording once `nyan.web` exists.
        let _guard = crate::services::web::test_support::WebSettingsGuard::none();
        let schema = web_fetch_tool_schema();
        assert_eq!(schema.name, "WebFetch");
        assert_eq!(schema.input_schema["required"], serde_json::json!(["url"]));
        assert_eq!(schema.input_schema["properties"]["url"]["format"], "uri");
        assert!(schema.description.contains("Fetches content"));
        assert!(
            schema
                .description
                .starts_with("IMPORTANT: WebFetch WILL FAIL for authenticated or private URLs.")
        );

        let secondary = prompt::make_secondary_model_prompt("# Doc", "Summarize", false);
        assert!(secondary.contains("Web page content"));
        assert!(secondary.contains("125-character maximum"));
    }

    #[test]
    fn web_fetch_preapproved_markdown_fast_path_matches_official_branch() {
        assert_eq!(
            preapproved_markdown_direct_result(
                "https://doc.rust-lang.org/book/",
                "text/markdown; charset=utf-8",
                "# Rust Book"
            ),
            Some("# Rust Book".to_string())
        );
        assert!(
            preapproved_markdown_direct_result(
                "https://example.com/docs",
                "text/markdown",
                "# Example"
            )
            .is_none()
        );
        assert!(
            preapproved_markdown_direct_result(
                "https://doc.rust-lang.org/book/",
                "text/html",
                "# Rust Book"
            )
            .is_none()
        );
        assert!(
            preapproved_markdown_direct_result(
                "https://doc.rust-lang.org/book/",
                "text/markdown",
                &"a".repeat(utils::MAX_MARKDOWN_LENGTH)
            )
            .is_none()
        );
    }

    #[test]
    fn web_fetch_binary_persistence_suffix_matches_official_copy() {
        let mut result = "summary".to_string();
        append_binary_content_note(
            &mut result,
            "application/pdf",
            Some("/tmp/webfetch.pdf"),
            Some(2048),
            4096,
        );
        assert_eq!(
            result,
            "summary\n\n[Binary content (application/pdf, 2KB) also saved to /tmp/webfetch.pdf]"
        );

        let mut no_path = "summary".to_string();
        append_binary_content_note(&mut no_path, "application/pdf", None, Some(2048), 4096);
        assert_eq!(no_path, "summary");
    }

    #[test]
    fn web_fetch_auto_classifier_input_matches_official_projection() {
        use crate::tool::ToolCall;

        let tool = WebFetchTool;
        assert_eq!(
            tool.to_auto_classifier_input(&serde_json::json!({
                "url": "https://example.com/docs",
                "prompt": "summarize"
            })),
            "https://example.com/docs: summarize"
        );
        assert_eq!(
            tool.to_auto_classifier_input(&serde_json::json!({
                "url": "https://example.com/docs"
            })),
            "https://example.com/docs"
        );
    }

    #[test]
    fn web_fetch_validate_input_matches_official_invalid_url_copy() {
        use crate::tool::ToolCall;

        let tool = WebFetchTool;
        let context = crate::tool::ToolUseContext::default();
        assert!(
            tool.validate_input(
                &serde_json::json!({"url": "https://example.com", "prompt": "p"}),
                &context
            )
            .is_ok()
        );
        assert_eq!(
            tool.validate_input(
                &serde_json::json!({"url": "not a url", "prompt": "p"}),
                &context
            ),
            crate::tool::ValidationResult::error(
                "Error: Invalid URL \"not a url\". The URL provided could not be parsed.",
                1
            )
        );
    }

    #[test]
    fn web_fetch_check_permissions_deny_uses_official_copy() {
        use crate::tool::ToolCall;
        use crate::types::permissions::{PermissionRuleSource, PermissionRuleValue};

        let mut context = crate::tool::ToolUseContext::default();
        context.tool_permission_context.always_deny_rules.insert(
            PermissionRuleSource::Session,
            vec![PermissionRuleValue::new(
                "WebFetch",
                Some("domain:example.com".to_string()),
            )],
        );
        let decision = WebFetchTool.check_permissions(
            &serde_json::json!({"url": "https://example.com/docs", "prompt": "p"}),
            &context,
        );
        assert!(matches!(
            decision,
            crate::utils::permissions::permission_result::PermissionResult::Deny { message, .. }
            if message == "WebFetch denied access to domain:example.com."
        ));
    }

    // ════════════════════════════════════════════════════════
    // @nyan: the configured web-fetch path, against a local mock.
    // ════════════════════════════════════════════════════════

    mod nyan_fetch {
        use std::collections::BTreeMap;
        use std::time::Duration;

        use serde_json::{Value, json};

        use super::super::{run_nyan_web_fetch_with, web_fetch_tool_schema};
        use crate::services::decision::Answer;
        use crate::services::web::test_support::{
            DecisionOverrideGuard, Route, ScriptedBackend, WebSettingsGuard, key_file, serve,
        };
        use crate::services::web::{budget, extract, fetcher::FetchConfig, search};
        use crate::tool::ToolUseContext;
        use crate::utils::settings::nyan::WebSettings;
        use crate::utils::test_env::EnvVarGuard;

        fn settings(json: Value) -> WebSettings {
            serde_json::from_value(json).unwrap()
        }

        fn config_dir(name: &str) -> (std::path::PathBuf, EnvVarGuard) {
            let dir = std::env::temp_dir().join(format!(
                "nyan-webfetch-{name}-{}",
                uuid::Uuid::new_v4().simple()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            let guard = EnvVarGuard::set("NYAN_CONFIG_DIR", &dir);
            (dir, guard)
        }

        fn mock_config() -> FetchConfig {
            FetchConfig::for_local_mock()
        }

        /// The `web` argument is unused by the implementation (settings are the
        /// process settings installed by [`WebSettingsGuard`]); it stays in the
        /// signature so a test cannot forget that it must install one.
        async fn fetch(
            args: Value,
            _web: WebSettings,
            config: Option<FetchConfig>,
        ) -> Result<super::super::Output, String> {
            let context = ToolUseContext::default();
            run_nyan_web_fetch_with(&args, &context, config).await
        }

        fn long_html() -> String {
            format!(
                "<html><body><h1>Doc</h1><p>{}</p></body></html>",
                "body text ".repeat(40)
            )
        }

        /// The fallback chain treats < 200 chars as a JS shell, so fixture
        /// pages that must be served directly carry filler.
        fn padded(text: &str) -> String {
            format!("{text}\n{}", "filler line. ".repeat(30))
        }

        #[tokio::test]
        async fn a_fresh_fetch_returns_the_original_text_and_a_second_read_uses_the_snapshot() {
            budget::reset();
            let _settings_guard =
                WebSettingsGuard::install(settings(json!({"maxReturnChars": 20000})));
            let (home, _home_guard) = config_dir("fresh");
            let server = serve(vec![("/doc", Route::html(&long_html()))]).await;
            let url = server.url("/doc");

            let first = fetch(
                json!({"url": url}),
                settings(json!({})),
                Some(mock_config()),
            )
            .await
            .unwrap();
            assert!(first.result.starts_with(&url), "{}", first.result);
            assert!(
                first.result.contains("<web_content source=\""),
                "{}",
                first.result
            );
            assert!(first.result.contains("fetched "), "{}", first.result);

            let second = fetch(
                json!({"url": url}),
                settings(json!({})),
                Some(mock_config()),
            )
            .await
            .unwrap();
            assert!(second.result.contains("not refetched"), "{}", second.result);
            assert_eq!(server.hits("/doc"), 1, "the cached read must not refetch");
            let _ = std::fs::remove_dir_all(home);
        }

        #[tokio::test]
        async fn range_continues_read_the_snapshot_without_refetching_or_counting() {
            budget::reset();
            let _settings_guard = WebSettingsGuard::install(settings(json!({})));
            let (home, _home_guard) = config_dir("range");
            let body = padded(&"0123456789".repeat(10));
            let server = serve(vec![("/page", Route::text(&body))]).await;
            let url = server.url("/page");
            let _ = fetch(
                json!({"url": url}),
                settings(json!({})),
                Some(mock_config()),
            )
            .await
            .unwrap();
            let after_first = budget::snapshot();

            let page = fetch(
                json!({"url": url, "range": "chars:10-25"}),
                settings(json!({})),
                Some(mock_config()),
            )
            .await
            .unwrap();
            let total = body.chars().count();
            assert!(page.result.contains("0123456789"), "{}", page.result);
            assert!(
                page.result
                    .contains(&format!("[Showing chars 10–25 of {total}")),
                "{}",
                page.result
            );
            assert!(page.result.contains("not refetched"));
            assert_eq!(server.hits("/page"), 1);
            assert_eq!(
                budget::snapshot(),
                after_first,
                "continuations are not counted"
            );

            let tail = fetch(
                json!({"url": url, "range": &format!("chars:{}-", total - 5)}),
                settings(json!({})),
                Some(mock_config()),
            )
            .await
            .unwrap();
            assert!(
                tail.result
                    .contains(&format!("[Showing chars {}–{total} of {total}]", total - 5))
            );

            let over = fetch(
                json!({"url": url, "range": &format!("chars:{}-", total + 1)}),
                settings(json!({})),
                Some(mock_config()),
            )
            .await
            .unwrap_err();
            assert!(
                over.contains(&format!("range_out_of_bounds: page has {total} chars")),
                "{over}"
            );
            let _ = std::fs::remove_dir_all(home);
        }

        #[tokio::test]
        async fn a_section_range_returns_that_heading_and_lists_headings_when_missing() {
            budget::reset();
            let _settings_guard = WebSettingsGuard::install(settings(json!({})));
            let (home, _home_guard) = config_dir("section");
            let markdown =
                padded("# Intro\nintro body\n## Usage\nuse it like this\n## Other\nother body");
            let server = serve(vec![("/md", Route::new(200, "text/markdown", markdown))]).await;
            let url = server.url("/md");
            let page = fetch(
                json!({"url": url, "range": "#usage"}),
                settings(json!({})),
                Some(mock_config()),
            )
            .await
            .unwrap();
            assert!(
                page.result.contains("## Usage\nuse it like this\n"),
                "{}",
                page.result
            );
            assert!(!page.result.contains("other body"), "{}", page.result);
            let missing = fetch(
                json!({"url": url, "range": "#missing"}),
                settings(json!({})),
                Some(mock_config()),
            )
            .await
            .unwrap_err();
            assert!(missing.starts_with("section_not_found"), "{missing}");
            assert!(missing.contains("Intro"), "{missing}");
            let _ = std::fs::remove_dir_all(home);
        }

        #[tokio::test]
        async fn prompt_extraction_returns_only_verbatim_excerpts_and_counts_the_rest() {
            budget::reset();
            let _settings_guard = WebSettingsGuard::install(settings(json!({})));
            let (home, _home_guard) = config_dir("extract");
            let page_text = padded("The exact answer is here. Unrelated sentence.");
            let server = serve(vec![("/p", Route::text(&page_text))]).await;
            let url = server.url("/p");
            // The extraction model replies with one real excerpt and one invented.
            extract::test_reply::set(r#"["The exact answer is here.", "An invented sentence."]"#);
            let page = fetch(
                json!({"url": url, "prompt": "what is the answer?"}),
                settings(json!({})),
                Some(mock_config()),
            )
            .await
            .unwrap();
            assert!(
                page.result.contains("The exact answer is here."),
                "{}",
                page.result
            );
            assert!(
                !page.result.contains("An invented sentence."),
                "{}",
                page.result
            );
            assert!(
                page.result
                    .contains("1 excerpts discarded (not found in page)"),
                "{}",
                page.result
            );
            let _ = std::fs::remove_dir_all(home);
        }

        #[tokio::test]
        async fn page_answers_adds_a_hint_and_keeps_the_body_verbatim() {
            budget::reset();
            let _settings_guard = WebSettingsGuard::install(settings(json!({})));
            let (home, _home_guard) = config_dir("hint");
            let page_text = padded("A page about something else entirely.");
            let server = serve(vec![("/p", Route::text(&page_text))]).await;
            let url = server.url("/p");
            search::remember_candidates(vec!["https://example.com/next".to_string()]);
            // The extraction model returns one real excerpt; the hint is added
            // around the excerpt body, never by rewriting it.
            extract::test_reply::set(r#"["A page about something else entirely."]"#);
            let (provider, backend) = ScriptedBackend::provider(|request| match request.point {
                // Not a challenge page; keep the direct fetch.
                crate::services::decision::DecisionPoint::WebFetchFallback => {
                    [("challenge".to_string(), Answer::Noul { probability: 0.02 })]
                        .into_iter()
                        .collect::<BTreeMap<_, _>>()
                }
                crate::services::decision::DecisionPoint::WebPageAnswers => {
                    [("answers".to_string(), Answer::Noul { probability: 0.02 })]
                        .into_iter()
                        .collect::<BTreeMap<_, _>>()
                }
                other => panic!("unexpected decision point {other:?}"),
            });
            let _decision = DecisionOverrideGuard::install(provider);
            let page = fetch(
                json!({"url": url, "prompt": "what is the answer?"}),
                settings(json!({})),
                Some(mock_config()),
            )
            .await
            .unwrap();
            assert!(
                page.result
                    .contains("A page about something else entirely."),
                "the verbatim excerpt is in the body: {}",
                page.result
            );
            assert!(
                page.result
                    .contains("[page may not answer the query — consider fetching:"),
                "{}",
                page.result
            );
            // The exact candidate can be whatever the most recent search
            // remembered (tests run concurrently); the hint must name one.
            assert!(
                page.result.contains("consider fetching: https://"),
                "the hint names a candidate URL: {}",
                page.result
            );
            let sent = backend.requests();
            let page_answers = sent
                .iter()
                .filter(|request| {
                    request.point == crate::services::decision::DecisionPoint::WebPageAnswers
                })
                .collect::<Vec<_>>();
            assert_eq!(page_answers.len(), 1);
            assert_eq!(page_answers[0].state["query"], "what is the answer?");
            assert!(
                sent.iter().any(|request| request.point
                    == crate::services::decision::DecisionPoint::WebFetchFallback),
                "the fallback point is also consulted"
            );
            let _ = std::fs::remove_dir_all(home);
        }

        #[tokio::test]
        async fn page_answers_cannot_alter_the_body_when_it_says_yes() {
            budget::reset();
            let _settings_guard = WebSettingsGuard::install(settings(json!({})));
            let (home, _home_guard) = config_dir("hint-no");
            let server = serve(vec![("/p", Route::text(&padded("The answer is 42.")))]).await;
            extract::test_reply::set("[]");
            let (provider, _) = ScriptedBackend::provider(|request| {
                let (question, probability) = match request.point {
                    crate::services::decision::DecisionPoint::WebFetchFallback => {
                        ("challenge", 0.02)
                    }
                    _ => ("answers", 0.99),
                };
                [(question.to_string(), Answer::Noul { probability })]
                    .into_iter()
                    .collect::<BTreeMap<_, _>>()
            });
            let _decision = DecisionOverrideGuard::install(provider);
            let page = fetch(
                json!({"url": server.url("/p"), "prompt": "answer?"}),
                settings(json!({})),
                Some(mock_config()),
            )
            .await
            .unwrap();
            assert!(!page.result.contains("may not answer"));
            let _ = std::fs::remove_dir_all(home);
        }

        #[tokio::test]
        async fn a_403_is_served_by_the_configured_fetch_provider() {
            budget::reset();
            let provider = serve(vec![("/scrape", Route::new(200, "application/json",
                json!({"data":{"markdown": format!("# Provider page\n{}", "text ".repeat(40))}}).to_string()))]).await;
            let _settings_guard = WebSettingsGuard::install(settings(json!({
                "providers": crate::services::web::test_support::only_mocked_providers(json!({
                    "firecrawl": {"enabled": true, "baseUrl": provider.origin, "apiKeyFile": key_file()},
                })),
                "fetchFallback": ["firecrawl"],
            })));
            let (home, _home_guard) = config_dir("fallback");
            let site = serve(vec![("/blocked", Route::new(403, "text/html", "denied"))]).await;
            let page = fetch(
                json!({"url": site.url("/blocked")}),
                settings(json!({})),
                Some(mock_config()),
            )
            .await
            .unwrap();
            assert!(
                page.result.contains("(fetched via firecrawl"),
                "{}",
                page.result
            );
            assert!(
                page.result.contains("direct fetch failed: http_403"),
                "{}",
                page.result
            );
            assert!(page.result.contains("# Provider page"));
            assert_eq!(provider.hits("/scrape"), 1);
            let _ = std::fs::remove_dir_all(home);
        }

        #[tokio::test]
        async fn ssrf_and_config_denials_surface_by_name_and_send_nothing() {
            budget::reset();
            let provider =
                serve(vec![("/scrape", Route::new(200, "application/json", "{}"))]).await;
            let web = settings(json!({
                "providers": crate::services::web::test_support::only_mocked_providers(json!({
                    "firecrawl": {"enabled": true, "baseUrl": provider.origin, "apiKeyFile": key_file()},
                })),
                "fetchFallback": ["firecrawl"],
                "denyHosts": ["blocked.example"],
            }));
            let _settings_guard = WebSettingsGuard::install(web.clone());
            let (home, _home_guard) = config_dir("ssrf");
            // Production configuration: no mock seam.
            let production = FetchConfig::from_settings(&web);
            for (target, expected) in [
                ("https://127.0.0.1/x", "denied_private_network"),
                ("https://[::1]/", "denied_private_network"),
                ("https://localhost./", "denied_private_network"),
                ("https://169.254.169.254/latest", "denied_private_network"),
                ("https://100.80.19.12/", "denied_private_network"),
                ("https://intranet/", "denied_private_network"),
                ("https://example.com:8443/", "denied_port"),
                ("ftp://example.com/", "denied_scheme"),
                ("https://blocked.example/", "denied_host"),
            ] {
                let error = fetch(
                    json!({"url": target}),
                    web.clone(),
                    Some(production.clone()),
                )
                .await
                .unwrap_err();
                assert!(error.starts_with(expected), "{target}: {error}");
            }
            assert!(
                provider.seen().is_empty(),
                "no denied URL may reach a provider"
            );
            let _ = std::fs::remove_dir_all(home);
        }

        #[tokio::test]
        async fn fetch_calls_and_bytes_are_budgeted_with_named_errors() {
            budget::reset();
            let server = serve(vec![
                ("/a", Route::text(&"a".repeat(400))),
                ("/b", Route::text(&"b".repeat(400))),
            ])
            .await;
            let home;
            {
                let _settings_guard = WebSettingsGuard::install(settings(json!({
                    "fetch": {"maxPerSession": 1, "maxBytesPerSession": 1048576},
                })));
                let (dir, _home_guard) = config_dir("budget-calls");
                home = dir;
                let _ = fetch(
                    json!({"url": server.url("/a")}),
                    settings(json!({})),
                    Some(mock_config()),
                )
                .await
                .unwrap();
                let error = fetch(
                    json!({"url": server.url("/b")}),
                    settings(json!({})),
                    Some(mock_config()),
                )
                .await
                .unwrap_err();
                assert_eq!(
                    error,
                    "budget_exhausted: web fetch limit (1 per session) reached"
                );
                assert_eq!(server.hits("/b"), 0);
            }
            let _ = std::fs::remove_dir_all(&home);

            budget::reset();
            {
                let _settings_guard = WebSettingsGuard::install(settings(json!({
                    "fetch": {"maxBytesPerSession": 300},
                })));
                let (dir, _home_guard) = config_dir("budget-bytes");
                let error = fetch(
                    json!({"url": server.url("/a")}),
                    settings(json!({})),
                    Some(mock_config()),
                )
                .await
                .unwrap_err();
                assert!(
                    error.starts_with("budget_exhausted: web fetch byte limit"),
                    "{error}"
                );
                let _ = std::fs::remove_dir_all(dir);
            }
            let _ = std::fs::remove_dir_all(home);
        }

        #[tokio::test]
        async fn the_tool_schema_keeps_prompt_optional_and_range_available() {
            let _settings_guard = WebSettingsGuard::install(settings(json!({})));
            let schema = web_fetch_tool_schema();
            assert_eq!(schema.input_schema["required"], json!(["url"]));
            assert!(schema.input_schema["properties"]["range"].is_object());
            assert!(schema.description.contains("<web_content>"));
        }
    }

    #[tokio::test]
    async fn web_fetch_tool_call_invalid_url_returns_error_without_network() {
        use crate::tool::ToolCall;

        let args = serde_json::json!({
            "url": "not a url",
            "prompt": "summarize"
        });
        let request = crate::utils::permissions::permissions::mock_permission_request_with_input(
            "perm-web-fetch".to_string(),
            "toolu_web_fetch".to_string(),
            "WebFetch".to_string(),
            "input:not a url".to_string(),
            args.clone(),
            crate::types::permissions::PermissionMode::Default,
        );
        let tool = WebFetchTool;
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
            tool.map_tool_result_to_tool_result_block_param(&result.data, "toolu_web_fetch");
        assert_eq!(status, crate::types::message::ToolResultStatus::Error);
        assert!(content.contains("Invalid URL"));
        // The error rides the row as the bare `Error: …` raw string.
        assert!(matches!(
            tool.tool_use_result(&result.data),
            Some(serde_json::Value::String(raw)) if raw.contains("Invalid URL")
        ));
    }
}
