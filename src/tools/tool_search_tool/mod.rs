//! Incremental port of official `ToolSearchTool/*`.
//! Tool-search beta declaration behavior (`defer_loading`) is projected in
//! `utils::api::tool_to_api_schema(...)`. Successful execution maps official
//! `matches` output to `tool_reference`
//! `tool_result` blocks at the API boundary.

pub mod prompt;

/// Maps to: CC `ToolSearchTool` metadata.
/// Maps to: CC `ToolSearchTool.ts:21-34` `inputSchema` — a plain `z.object`,
/// so unknown keys are stripped rather than reported.
///
/// `max_results` is `.optional().default(5)`: the default is the OUTER wrapper,
/// which is why zod keeps it in `required` (oracle: ToolSearchTool). The
/// hand-written literal this replaces had it out of `required`.
pub fn input_schema() -> &'static crate::utils::zod::Schema {
    static SCHEMA: std::sync::OnceLock<crate::utils::zod::Schema> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| {
        use crate::utils::zod as zod;
        zod::object(vec![
            (
                "query",
                zod::string().describe(
                    "Query to find deferred tools. Use \"select:<tool_name>\" for direct selection, or keywords to search.",
                ),
            ),
            (
                "max_results",
                zod::number()
                    .optional()
                    .default(serde_json::json!(5))
                    .describe("Maximum number of results to return (default: 5)"),
            ),
        ])
    })
}

pub fn tool_search_tool_schema() -> crate::types::tools::Tool {
    crate::types::tools::Tool {
        name: prompt::TOOL_SEARCH_TOOL_NAME.to_string(),
        description: prompt::get_prompt(),
        input_schema: crate::utils::zod_to_json_schema::zod_to_json_schema(input_schema()),
        ..Default::default()
    }
}

/// CC `tools/ToolSearchTool/ToolSearchTool.ts` outputSchema (:37).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ToolSearchOutput {
    pub(crate) matches: Vec<String>,
    pub(crate) query: String,
    pub(crate) total_deferred_tools: usize,
    pub(crate) pending_mcp_servers: Vec<String>,
}

fn merge_runtime_mcp_tools(
    tools: &[crate::types::tools::Tool],
    mcp_state: &crate::state::app_state_store::McpState,
) -> Vec<crate::types::tools::Tool> {
    let mut merged = if tools.is_empty() {
        crate::tools::get_all_base_tools()
    } else {
        tools.to_vec()
    };
    let mut names = merged
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<std::collections::BTreeSet<_>>();
    for tool in &mcp_state.tools {
        if names.insert(tool.name.clone()) {
            merged.push(tool.clone());
        }
    }
    merged
}

/// Maps to: CC `ToolSearchTool.call(...)` local `getPendingServerNames()`.
pub(crate) fn pending_mcp_server_names(
    mcp_state: &crate::state::app_state_store::McpState,
) -> Vec<String> {
    mcp_state
        .clients
        .iter()
        .filter(|server| {
            server.client.status == crate::services::mcp::types::McpServerConnectionType::Pending
        })
        .map(|server| server.client.name.clone())
        .collect()
}

/// Search deferred tools by select:/keyword query.
/// Maps to: CC `tools/ToolSearchTool/ToolSearchTool.ts` `call` (:328).
pub(crate) fn tool_search_output(input: &serde_json::Value) -> ToolSearchOutput {
    tool_search_output_from_tools_and_mcp_state(
        input,
        &crate::tools::get_all_base_tools(),
        &crate::state::app_state_store::McpState::default(),
    )
}

/// Maps to: CC `ToolSearchTool.call(input, { options: { tools }, getAppState })`.
pub(crate) fn tool_search_output_from_tools_and_mcp_state(
    input: &serde_json::Value,
    tools: &[crate::types::tools::Tool],
    mcp_state: &crate::state::app_state_store::McpState,
) -> ToolSearchOutput {
    // CC keeps the raw `query` verbatim in the output (`buildSearchResult`);
    // only the keyword path lowercases + trims its working copy (:192).
    let query = input
        .get("query")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    // CC destructure default `max_results = 5` with no floor — 0 yields an
    // empty slice.
    let max_results = input
        .get("max_results")
        .and_then(|value| value.as_u64())
        .unwrap_or(5) as usize;

    let tools = merge_runtime_mcp_tools(tools, mcp_state);
    let deferred_tools = tools
        .iter()
        .filter(|tool| prompt::is_deferred_tool(tool))
        .cloned()
        .collect::<Vec<_>>();
    let matches = tool_search_matches(&query, &deferred_tools, &tools, max_results);
    let pending_mcp_servers = if matches.is_empty() {
        pending_mcp_server_names(mcp_state)
    } else {
        Vec::new()
    };
    ToolSearchOutput {
        matches,
        query,
        total_deferred_tools: deferred_tools.len(),
        pending_mcp_servers,
    }
}

pub(crate) fn tool_search_output_json(output: &ToolSearchOutput) -> serde_json::Value {
    let mut object = serde_json::Map::new();
    object.insert("matches".to_string(), serde_json::json!(&output.matches));
    object.insert("query".to_string(), serde_json::json!(&output.query));
    object.insert(
        "total_deferred_tools".to_string(),
        serde_json::json!(output.total_deferred_tools),
    );
    if !output.pending_mcp_servers.is_empty() {
        object.insert(
            "pending_mcp_servers".to_string(),
            serde_json::json!(&output.pending_mcp_servers),
        );
    }
    serde_json::Value::Object(object)
}

/// Maps to: CC `ToolSearchTool.ts:132-161` `parseToolName` — MCP tools split
/// on `__`/`_`, regular tools split CamelCase + underscores.
struct ParsedToolName {
    parts: Vec<String>,
    full: String,
    is_mcp: bool,
}

fn parse_tool_name(name: &str) -> ParsedToolName {
    if let Some(without_prefix) = name.strip_prefix("mcp__") {
        let without_prefix = without_prefix.to_lowercase();
        let parts = without_prefix
            .split("__")
            .flat_map(|p| p.split('_'))
            .filter(|p| !p.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>();
        let full = without_prefix.replace("__", " ").replace('_', " ");
        return ParsedToolName {
            parts,
            full,
            is_mcp: true,
        };
    }

    let mut spaced = String::with_capacity(name.len() + 8);
    let mut prev_lower = false;
    for ch in name.chars() {
        if prev_lower && ch.is_ascii_uppercase() {
            spaced.push(' ');
        }
        prev_lower = ch.is_ascii_lowercase();
        spaced.push(ch);
    }
    let parts = spaced
        .replace('_', " ")
        .to_lowercase()
        .split_whitespace()
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    let full = parts.join(" ");
    ParsedToolName {
        parts,
        full,
        is_mcp: false,
    }
}

/// Maps to: CC `ToolSearchTool.ts:167-175` `compileTermPatterns` — one
/// word-boundary regex per unique term. CC's `new RegExp` carries no `u`
/// flag, so its `\b` is the ASCII word boundary (`[0-9A-Za-z_]`); Rust's
/// default `\b` is Unicode-aware, so pin `(?-u:\b)` — a fully non-ASCII
/// term then never matches its boundary pattern, exactly like JS.
fn compile_term_patterns(terms: &[String]) -> std::collections::HashMap<String, regex::Regex> {
    let mut patterns = std::collections::HashMap::new();
    for term in terms {
        if !patterns.contains_key(term) {
            if let Ok(pattern) =
                regex::Regex::new(&format!(r"(?-u:\b){}(?-u:\b)", regex::escape(term)))
            {
                patterns.insert(term.clone(), pattern);
            }
        }
    }
    patterns
}

fn tool_search_hint(tool_name: &str) -> String {
    crate::services::tools::tool_execution::find_tool_call(tool_name)
        .and_then(|call| call.search_hint())
        .unwrap_or_default()
        .to_lowercase()
}

/// Maps to: CC `ToolSearchTool.ts:63-85` `getToolDescriptionMemoized` — the
/// search-scoring text, read through `Tool.prompt(...)` and NOT off any
/// eagerly rendered field.
///
/// Two CC details this mirrors exactly:
/// - the lookup is by NAME in the FULL tool pool (`findToolByName(tools,
///   toolName)`), not on the deferred entry the caller is iterating, and a
///   name absent from the pool scores as `''` (`:69-71`);
/// - the options bag is NOT this turn's live one. CC hardcodes
///   `getToolPermissionContext: async () => ({ mode: 'default',
///   additionalWorkingDirectories: new Map(), alwaysAllowRules: {},
///   alwaysDenyRules: {}, alwaysAskRules: {}, isBypassPermissionsModeAvailable:
///   false })` — byte-for-byte `Tool.ts:140-148 getEmptyToolPermissionContext`,
///   i.e. [`crate::tool::ToolPermissionContext::default`] — plus `agents: []`
///   (`:73-84`). So a deny rule or an agent list CANNOT move tool-search
///   scoring, by construction: the search path has no permission context to
///   thread and CC deliberately does not give it one.
///
/// Deviation (deliberate): CC wraps this in `memoize(..., toolName)` with an
/// invalidation hook keyed on the deferred-tool set (`:88-105`) because its
/// `prompt()` is async and awaited twice per tool per search. The port's
/// resolution is a synchronous value read, so the memo table and its
/// invalidation would buy nothing and could only go stale.
fn tool_search_description(tool_name: &str, tools: &[crate::types::tools::Tool]) -> String {
    let Some(tool) = crate::types::tools::find_tool_by_name(tools, tool_name) else {
        return String::new();
    };
    let search_permission_context = crate::tool::ToolPermissionContext::default();
    tool.prompt(&crate::tool::ToolPromptOptions {
        tool_permission_context: &search_permission_context,
        tools,
        agents: &[],
        allowed_agent_types: None,
    })
}

/// select:/exact/mcp-prefix/keyword scoring over deferred tools.
/// Maps to: CC `tools/ToolSearchTool/ToolSearchTool.ts` `call` select branch
/// (:363-406) / `searchToolsWithKeywords` (:186-302).
pub(crate) fn tool_search_matches(
    query: &str,
    deferred_tools: &[crate::types::tools::Tool],
    tools: &[crate::types::tools::Tool],
    max_results: usize,
) -> Vec<String> {
    // CC :363 `query.match(/^select:(.+)$/i)` — case-insensitive, anchored on
    // the RAW query; found names dedupe and are NOT capped by max_results.
    // `.` matches no JS line terminator, so \n, \r, U+2028, or U+2029 anywhere
    // after the prefix fails `(.+)$` and falls through to keyword search.
    // `get(..7)` (not `[..7]`) keeps a multi-byte query from panicking on a
    // non-char-boundary slice; a None prefix can never equal "select:".
    let is_select = query.len() > 7
        && query
            .get(..7)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("select:"))
        && !query[7..].contains(['\n', '\r', '\u{2028}', '\u{2029}']);
    if is_select {
        let requested = query[7..]
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>();
        let mut found: Vec<String> = Vec::new();
        for name in requested {
            if let Some(tool) = crate::types::tools::find_tool_by_name(deferred_tools, name)
                .or_else(|| crate::types::tools::find_tool_by_name(tools, name))
            {
                if !found.contains(&tool.name) {
                    found.push(tool.name.clone());
                }
            }
        }
        return found;
    }

    // CC :192 `queryLower = query.toLowerCase().trim()`.
    let query_lower = query.to_lowercase().trim().to_string();
    if let Some(tool) = deferred_tools
        .iter()
        .find(|tool| tool.name.to_lowercase() == query_lower)
        .or_else(|| {
            tools
                .iter()
                .find(|tool| tool.name.to_lowercase() == query_lower)
        })
    {
        return vec![tool.name.clone()];
    }
    if query_lower.starts_with("mcp__") && query_lower.len() > 5 {
        let prefix_matches = deferred_tools
            .iter()
            .filter(|tool| tool.name.to_lowercase().starts_with(&query_lower))
            .take(max_results)
            .map(|tool| tool.name.clone())
            .collect::<Vec<_>>();
        if !prefix_matches.is_empty() {
            return prefix_matches;
        }
    }

    let query_terms = query_lower
        .split_whitespace()
        .filter(|term| !term.is_empty())
        .collect::<Vec<_>>();
    let mut required_terms: Vec<String> = Vec::new();
    let mut optional_terms: Vec<String> = Vec::new();
    for term in query_terms {
        if let Some(required) = term.strip_prefix('+').filter(|term| !term.is_empty()) {
            required_terms.push(required.to_string());
        } else {
            optional_terms.push(term.to_string());
        }
    }
    // CC :231-232: scoring terms are required+optional when any required
    // exists, else the raw term list.
    let scoring_terms = if required_terms.is_empty() {
        optional_terms.clone()
    } else {
        required_terms
            .iter()
            .chain(optional_terms.iter())
            .cloned()
            .collect::<Vec<_>>()
    };
    let term_patterns = compile_term_patterns(&scoring_terms);

    // Pre-filter to tools matching ALL required terms (CC :236-257), whose
    // scoring text is `await getToolDescriptionMemoized(tool.name, tools)`
    // (CC :241) — see [`tool_search_description`].
    let candidates = deferred_tools.iter().filter(|tool| {
        if required_terms.is_empty() {
            return true;
        }
        let parsed = parse_tool_name(&tool.name);
        let desc_normalized = tool_search_description(&tool.name, tools).to_lowercase();
        let hint_normalized = tool_search_hint(&tool.name);
        required_terms.iter().all(|term| {
            let pattern = term_patterns.get(term);
            parsed.parts.iter().any(|part| part == term)
                || parsed.parts.iter().any(|part| part.contains(term.as_str()))
                || pattern.is_some_and(|p| p.is_match(&desc_normalized))
                || (!hint_normalized.is_empty()
                    && pattern.is_some_and(|p| p.is_match(&hint_normalized)))
        })
    });

    // Score (CC :259-295): exact part 12(mcp)/10, part-contains 6/5,
    // full-name fallback +3 only while the accumulated score is 0, word-bound
    // hint +4, word-bound description +2.
    let mut scored = candidates
        .filter_map(|tool| {
            let parsed = parse_tool_name(&tool.name);
            // CC :262 — the same resolved read as the pre-filter above.
            let desc_normalized = tool_search_description(&tool.name, tools).to_lowercase();
            let hint_normalized = tool_search_hint(&tool.name);
            let mut score = 0usize;
            for term in &scoring_terms {
                let pattern = term_patterns.get(term);
                if parsed.parts.iter().any(|part| part == term) {
                    score += if parsed.is_mcp { 12 } else { 10 };
                } else if parsed.parts.iter().any(|part| part.contains(term.as_str())) {
                    score += if parsed.is_mcp { 6 } else { 5 };
                }
                if parsed.full.contains(term.as_str()) && score == 0 {
                    score += 3;
                }
                if !hint_normalized.is_empty()
                    && pattern.is_some_and(|p| p.is_match(&hint_normalized))
                {
                    score += 4;
                }
                if pattern.is_some_and(|p| p.is_match(&desc_normalized)) {
                    score += 2;
                }
            }
            (score > 0).then(|| (tool.name.clone(), score))
        })
        .collect::<Vec<_>>();
    // CC :299 `sort((a, b) => b.score - a.score)` — JS sort is stable, so
    // ties keep the deferred-tools order; Rust `sort_by` is stable too.
    scored.sort_by(|(_, left_score), (_, right_score)| right_score.cmp(left_score));
    scored
        .into_iter()
        .take(max_results)
        .map(|(name, _)| name)
        .collect()
}

/// Behavioral half of CC `ToolSearchTool` — dispatched via `crate::tool::ToolCall`.
pub(crate) struct ToolSearchTool;

impl crate::tool::ToolCall for ToolSearchTool {
    fn name(&self) -> &'static str {
        "ToolSearch"
    }

    /// Maps to: CC `ToolSearchTool.ts:319-321` `async prompt() { return
    /// getPrompt() }` (`prompt.ts:119-121`, PROMPT_HEAD + location hint +
    /// PROMPT_TAIL) — same source the wire schema renders eagerly.
    fn prompt(
        &self,
        _tool: &crate::types::tools::Tool,
        _options: &crate::tool::ToolPromptOptions<'_>,
    ) -> String {
        prompt::get_prompt()
    }

    /// Maps to: CC `ToolSearchTool.isEnabled()` → `isToolSearchEnabledOptimistic()`.
    fn is_enabled(&self) -> bool {
        prompt::is_tool_search_enabled_optimistic()
    }

    /// Maps to: CC `ToolSearchTool.ts:308-310` `isConcurrencySafe()`.
    fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
        true
    }

    /// Maps to: CC `ToolSearchTool.ts:311-313` `isReadOnly()`.
    fn is_read_only(&self, _args: &serde_json::Value) -> bool {
        true
    }

    /// Maps to: CC `ToolSearchTool.ts:315` `maxResultSizeChars`.
    fn max_result_size_chars(&self) -> usize {
        100_000
    }

    /// Maps to: CC `ToolSearchTool.ts:316-318` `description()` — same
    /// `getPrompt()` text as `prompt()`.
    fn description(&self, _args: &serde_json::Value) -> String {
        prompt::get_prompt()
    }

    /// Maps to: CC `ToolSearchTool.ts:438` `userFacingName: () => ''`.
    fn user_facing_name(&self, _args: Option<&serde_json::Value>) -> String {
        String::new()
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
                data: crate::tool::ToolOutput::ToolSearch(
                    tool_search_output_from_tools_and_mcp_state(
                        args,
                        &context.tools,
                        &context.mcp_state,
                    ),
                ),
                new_messages: Vec::new(),
            }
        })
    }

    /// Maps to: CC `tools/ToolSearchTool/ToolSearchTool.ts`
    /// `mapToolResultToToolResultBlockParam` (:444-470). The Rust transcript
    /// stores JSON and `transcript_tool_result_to_model_message(...)` expands
    /// successful matches to typed `tool_reference` blocks at the API boundary.
    fn map_tool_result_to_tool_result_block_param(
        &self,
        data: &crate::tool::ToolOutput,
        _tool_use_id: &str,
    ) -> (String, crate::types::message::ToolResultStatus) {
        match data {
            crate::tool::ToolOutput::ToolSearch(output) => (
                tool_search_output_json(output).to_string(),
                crate::types::message::ToolResultStatus::Success,
            ),
            crate::tool::ToolOutput::Composed {
                content, status, ..
            } => (content.clone(), *status),
            _ => (
                "<tool_use_error>ToolSearch returned an unexpected output variant</tool_use_error>"
                    .to_string(),
                crate::types::message::ToolResultStatus::Error,
            ),
        }
    }

    // CC `ToolSearchTool` defines NO `renderToolResultMessage` (and
    // `userFacingName: () => ''`) — the render layer hides success rows by
    // name (`success_tool_result_is_nonvisual`); errors keep the generic
    // `FallbackToolUseErrorMessage` path.
}

#[cfg(test)]
mod tests {
    use crate::utils::test_env::TEST_ENV_LOCK;

    #[test]
    fn tool_search_schema_matches_official_input_shape() {
        let schema = super::tool_search_tool_schema();
        assert_eq!(schema.name, "ToolSearch");
        // `max_results` is `.optional().default(5)` — the default is the outer
        // wrapper, so zod keeps it required (oracle: ToolSearchTool). This
        // assertion used to pin `["query"]` alone, which was the hand-written
        // literal's shape, not CC's.
        assert_eq!(
            schema.input_schema.get("required"),
            Some(&serde_json::json!(["query", "max_results"]))
        );
        assert_eq!(
            schema
                .input_schema
                .pointer("/properties/max_results/default")
                .and_then(|value| value.as_i64()),
            Some(5)
        );
        assert!(schema.description.contains("select:Read,Edit,Grep"));
    }

    #[test]
    fn tool_search_request_gate_requires_model_support_and_tool_availability() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("ENABLE_TOOL_SEARCH");
        crate::utils::process_env::remove("CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS");
        let tools = vec![
            super::tool_search_tool_schema(),
            crate::tools::web_fetch_tool::web_fetch_tool_schema(),
        ];

        assert!(super::prompt::is_tool_search_enabled_for_request(
            "claude-sonnet-4-20250514",
            &tools,
        ));
        assert!(!super::prompt::is_tool_search_enabled_for_request(
            "claude-3-5-haiku-latest",
            &tools,
        ));
        assert!(!super::prompt::is_tool_search_enabled_for_request(
            "claude-sonnet-4-20250514",
            &[crate::tools::web_fetch_tool::web_fetch_tool_schema()],
        ));
    }

    #[test]
    fn tool_search_optimistic_gate_respects_beta_kill_switch() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("ENABLE_TOOL_SEARCH");
        crate::utils::process_env::remove("CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS");
        assert!(super::prompt::is_tool_search_enabled_optimistic());

        crate::utils::process_env::set("ENABLE_TOOL_SEARCH", "false");
        assert!(!super::prompt::is_tool_search_enabled_optimistic());
        crate::utils::process_env::remove("ENABLE_TOOL_SEARCH");

        crate::utils::process_env::set("CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS", "1");
        assert!(!super::prompt::is_tool_search_enabled_optimistic());
        crate::utils::process_env::remove("CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS");
    }

    #[test]
    fn tool_search_consumes_task_behavior_defer_and_search_hint_metadata() {
        let tools = vec![
            crate::tools::task_output_tool::task_output_tool_schema(),
            crate::tools::task_stop_tool::task_stop_tool_schema(),
        ];
        assert!(tools.iter().all(super::prompt::is_deferred_tool));
        assert_eq!(
            super::tool_search_matches("logs", &tools, &tools, 5),
            vec!["TaskOutput"]
        );
        assert_eq!(
            super::tool_search_matches("kill", &tools, &tools, 5),
            vec!["TaskStop"]
        );
    }

    #[test]
    fn tool_search_select_branch_matches_official_semantics() {
        let tools = vec![
            crate::tools::task_output_tool::task_output_tool_schema(),
            crate::tools::task_stop_tool::task_stop_tool_schema(),
            crate::tools::task_list_tool::task_list_tool_schema(),
        ];
        // CC `/^select:(.+)$/i` — case-insensitive prefix, dedup, and no
        // max_results cap (three names survive max_results=1).
        assert_eq!(
            super::tool_search_matches(
                "Select:TaskOutput,TaskStop,TaskOutput,TaskList",
                &tools,
                &tools,
                1
            ),
            vec!["TaskOutput", "TaskStop", "TaskList"]
        );
        // The regex anchors on the raw query — a leading space fails ^ and
        // falls through to keyword search, where the joined term
        // "select:taskoutput" matches nothing.
        assert_eq!(
            super::tool_search_matches(" select:TaskOutput", &tools, &tools, 5),
            Vec::<String>::new()
        );
        // `.` matches no JS line terminator — even a trailing newline fails
        // `(.+)$` and the query falls through to keyword search (a select
        // branch that trimmed the name would wrongly return TaskOutput).
        assert_eq!(
            super::tool_search_matches("select:TaskOutput\n", &tools, &tools, 5),
            Vec::<String>::new()
        );
        // A multi-byte query whose 7th byte is not a char boundary must fall
        // through to keyword search without panicking on the prefix slice.
        assert_eq!(
            super::tool_search_matches("日本語のツール検索", &tools, &tools, 5),
            Vec::<String>::new()
        );
    }

    #[test]
    fn tool_search_scoring_uses_name_parts_and_word_boundaries() {
        // CamelCase names split into parts (CC parseToolName) — the exact
        // part "output" scores the +10 tier for TaskOutput.
        let tools = vec![
            crate::tools::task_output_tool::task_output_tool_schema(),
            crate::tools::task_stop_tool::task_stop_tool_schema(),
        ];
        assert_eq!(
            super::tool_search_matches("output", &tools, &tools, 5),
            vec!["TaskOutput"]
        );
        // Word-boundary description matching: "stop" must not match inside
        // an unrelated longer word; TaskStop wins via its name part.
        let matches = super::tool_search_matches("stop", &tools, &tools, 5);
        assert_eq!(matches.first().map(String::as_str), Some("TaskStop"));
    }

    #[test]
    fn tool_search_uses_runtime_mcp_tools_and_reports_pending_servers_like_official() {
        let mut mcp_state = crate::state::app_state_store::McpState {
            clients: vec![
                crate::services::mcp::types::McpServerSnapshot {
                    connection_id: None,
                    client: crate::services::mcp::types::McpClientSnapshot {
                        name: "GitHub Server".to_string(),
                        status: crate::services::mcp::types::McpServerConnectionType::Connected,
                        reconnect_attempt: None,
                        max_reconnect_attempts: None,
                        ide_name: None,
                        server_version: None,
                        error: None,
                    },
                    config: None,
                    supports_resources: false,
                    tools: vec![crate::services::mcp::types::McpToolSnapshot {
                        name: "Create Issue".to_string(),
                        display_name: Some("Create Issue".to_string()),
                        description: Some("Create a GitHub issue".to_string()),
                        input_schema: serde_json::json!({"type":"object"}),
                        read_only_hint: false,
                        destructive_hint: false,
                        open_world_hint: true,
                    }],
                    prompts: Vec::new(),
                    resources: Vec::new(),
                },
                crate::services::mcp::types::McpServerSnapshot {
                    connection_id: None,
                    client: crate::services::mcp::types::McpClientSnapshot {
                        name: "slack".to_string(),
                        status: crate::services::mcp::types::McpServerConnectionType::Pending,
                        reconnect_attempt: None,
                        max_reconnect_attempts: None,
                        ide_name: None,
                        server_version: None,
                        error: None,
                    },
                    config: None,
                    supports_resources: false,
                    tools: Vec::new(),
                    prompts: Vec::new(),
                    resources: Vec::new(),
                },
            ],
            ..crate::state::app_state_store::McpState::default()
        };
        crate::services::mcp::client::refresh_flat_mcp_capabilities(&mut mcp_state);
        let tools = vec![super::tool_search_tool_schema()];

        let matched = super::tool_search_output_from_tools_and_mcp_state(
            &serde_json::json!({"query":"mcp__github", "max_results": 5}),
            &tools,
            &mcp_state,
        );
        assert_eq!(matched.matches, vec!["mcp__GitHub_Server__Create_Issue"]);
        assert!(matched.pending_mcp_servers.is_empty());
        assert_eq!(matched.total_deferred_tools, 1);

        let no_match = super::tool_search_output_from_tools_and_mcp_state(
            &serde_json::json!({"query":"no-such-tool", "max_results": 5}),
            &tools,
            &mcp_state,
        );
        assert!(no_match.matches.is_empty());
        assert_eq!(no_match.pending_mcp_servers, vec!["slack"]);
    }

    /// CC `ToolSearchTool.ts:241`/`:262` score against `await
    /// getToolDescriptionMemoized(tool.name, tools)` (`:63-85`), i.e.
    /// `tool.prompt(...)` — NOT any description the caller happens to carry on
    /// the tool object. Pinned with a deferred built-in whose wire field is
    /// deliberately wrong: `+markdown` is a REQUIRED term, so it must come out
    /// of WebFetch's real prompt ("converts HTML to markdown",
    /// `WebFetchTool/prompt.ts`) — the name parts are only "web"/"fetch" and
    /// the searchHint is "fetch and extract content from a URL".
    ///
    /// Old shape: `tool.description.to_lowercase()` at both sites, so the
    /// pre-filter saw "stale-carried-text" and returned no matches.
    #[test]
    fn tool_search_scoring_reads_the_lazy_prompt_not_the_carried_description() {
        let deferred = vec![crate::types::tools::Tool {
            name: "WebFetch".to_string(),
            description: "stale-carried-text".to_string(),
            ..Default::default()
        }];
        assert!(crate::tools::tool_search_tool::prompt::is_deferred_tool(
            &deferred[0]
        ));

        assert_eq!(
            super::tool_search_matches("+markdown", &deferred, &deferred, 5),
            vec!["WebFetch"]
        );
        // The stale field is not a search source in either direction.
        assert_eq!(
            super::tool_search_matches("+stale-carried-text", &deferred, &deferred, 5),
            Vec::<String>::new()
        );
    }

    /// CC `ToolSearchTool.ts:69-71`: the description lookup is
    /// `findToolByName(tools, toolName)` against the FULL pool, and a name the
    /// pool does not carry resolves to `''` — the deferred entry the caller is
    /// iterating is NOT used as the fallback instance.
    #[test]
    fn tool_search_description_resolves_through_the_full_pool_like_official() {
        let deferred = vec![crate::types::tools::Tool {
            name: "WebFetch".to_string(),
            description: "stale-carried-text".to_string(),
            ..Default::default()
        }];

        // Absent from the pool: no description text, so a description-only
        // required term cannot match even though the deferred entry carries it.
        assert_eq!(
            super::tool_search_matches("+markdown", &deferred, &[], 5),
            Vec::<String>::new()
        );
        assert_eq!(super::tool_search_description("WebFetch", &[]), "");
        // Present in the pool: the registered prompt.
        assert!(super::tool_search_description("WebFetch", &deferred).contains("markdown"));
    }
}
