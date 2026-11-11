//! Mock-server fixtures for the Anthropic adapter: the exact wire request and
//! the streamed response, driven through the real SDK and HTTP stack.
//!
//! @nyan: these are the oracle for the cache prefix. They assert what reaches
//! the wire (credential source, cache breakpoints, thinking replay, tool
//! blocks) and what comes back (tool-call JSON fragments, thinking with
//! signature, usage, stop reason), so a later request-side migration onto the
//! IR can be checked against them.

use serde_json::{Value, json};

use super::mock::{MockAnthropic, RecordedRequest, Reply, StreamSpec, message_stream};
use crate::services::api::request::{Options, QueryModelStreamItem, query_model_with_streaming};
use crate::types::ids::ToolUseId;
use crate::types::message::{
    AssistantContent, AssistantMessage, Message, ToolResult, ToolUseBlock,
};
use crate::types::tools::Tool;
use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK, TestEnvGuard};
use crate::utils::thinking::ThinkingConfig;

const KEY_VAR: &str = "NYAN_FIXTURE_ANTHROPIC_KEY";
const KEY: &str = "sk-fixture-profile-key";

/// Holds the env lock, a config dir, and a main profile pointing at `base_url`.
struct Env {
    _lock: TestEnvGuard<'static>,
    _guards: Vec<EnvVarGuard>,
    root: std::path::PathBuf,
}

impl Env {
    fn new(base_url: &str) -> Self {
        let lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root =
            std::env::temp_dir().join(format!("nyan-anthropic-fixture-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let mut guards = vec![
            EnvVarGuard::set("NYAN_CONFIG_DIR", &root),
            EnvVarGuard::set("NYAN_SIMPLE", "1"),
            EnvVarGuard::set(KEY_VAR, KEY),
            EnvVarGuard::set("NYAN_MAX_RETRIES", "0"),
            EnvVarGuard::set("API_TIMEOUT_MS", "5000"),
            EnvVarGuard::set("NO_PROXY", "*"),
            EnvVarGuard::set("no_proxy", "*"),
        ];
        for key in [
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_BASE_URL",
            "ANTHROPIC_CUSTOM_HEADERS",
            "ANTHROPIC_BETAS",
            "NYAN_EXTRA_BODY",
            "DISABLE_PROMPT_CACHING",
            "ANTHROPIC_UNIX_SOCKET",
        ] {
            guards.push(EnvVarGuard::unset(key));
        }
        crate::utils::settings::settings_cache::reset_settings_cache();
        super::target::install_test_main_profile(base_url, KEY_VAR);
        Self {
            _lock: lock,
            _guards: guards,
            root,
        }
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        crate::utils::settings::settings_cache::reset_settings_cache();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn user(text: &str) -> Message {
    Message::User(crate::utils::messages::create_user_message(
        text.to_string(),
    ))
}

fn assistant(content: Vec<AssistantContent>) -> Message {
    let mut message: AssistantMessage =
        crate::utils::messages::create_assistant_message(String::new());
    // History from the model these fixtures request, so it replays as-is.
    message.model = Some("claude-sonnet-4-6".to_string());
    message.content = content;
    Message::Assistant(message)
}

fn tool_result(id: &str, text: &str) -> Message {
    let mut message = crate::utils::messages::create_user_message(String::new());
    message.content = vec![crate::types::message::UserContent::ToolResult(ToolResult {
        tool_use_id: ToolUseId(id.to_string()),
        content: text.to_string(),
        is_error: false,
        content_blocks: Vec::new(),
        tool_use_result: None,
    })];
    Message::User(message)
}

fn probe_tool() -> Tool {
    Tool {
        name: "Probe".to_string(),
        description: "probe tool".to_string(),
        input_schema: json!({"type": "object", "properties": {"a": {"type": "number"}}}),
        ..Default::default()
    }
}

struct Outcome {
    request: RecordedRequest,
    items: Vec<QueryModelStreamItem>,
}

async fn run(
    spec: StreamSpec,
    messages: Vec<Message>,
    tools: Vec<Tool>,
    configure: impl FnOnce(&mut Options),
) -> Outcome {
    let mock = MockAnthropic::start(vec![Reply::Sse(message_stream(spec))]).await;
    let _env = Env::new(&mock.base_url);
    let mut options = Options::new(
        "claude-sonnet-4-6".to_string(),
        "repl_main_thread".to_string(),
    );
    configure(&mut options);
    let mut rx = query_model_with_streaming(
        &messages,
        &vec!["You are a test.".to_string()],
        &ThinkingConfig::Disabled,
        &tools,
        &options,
    )
    .await
    .expect("stream opens");
    let mut items = Vec::new();
    while let Some(item) = tokio::time::timeout(std::time::Duration::from_secs(10), rx.recv())
        .await
        .expect("stream item before deadline")
    {
        items.push(item);
    }
    let requests = mock.requests();
    assert_eq!(requests.len(), 1, "exactly one HTTP request");
    Outcome {
        request: requests.into_iter().next().unwrap(),
        items,
    }
}

fn assistants(items: &[QueryModelStreamItem]) -> Vec<&AssistantMessage> {
    items
        .iter()
        .filter_map(|item| match item {
            QueryModelStreamItem::Assistant(message) => Some(message),
            _ => None,
        })
        .collect()
}

fn blocks(body: &Value, message: usize) -> &Vec<Value> {
    body["messages"][message]["content"]
        .as_array()
        .unwrap_or_else(|| panic!("messages[{message}].content is not an array: {body}"))
}

// ---------------------------------------------------------------------------
// Request on the wire
// ---------------------------------------------------------------------------

#[tokio::test]
async fn request_uses_the_profile_key_and_endpoint_only() {
    let outcome = run(StreamSpec::default(), vec![user("hello")], vec![], |_| {}).await;
    let request = &outcome.request;
    assert!(
        request
            .request_line
            .starts_with("POST /v1/messages?beta=true"),
        "{}",
        request.request_line
    );
    assert_eq!(request.header("x-api-key"), Some(KEY));
    assert_eq!(request.header("authorization"), None);
    assert_eq!(request.body["model"], "claude-sonnet-4-6");
    assert_eq!(request.body["stream"], true);
    assert_eq!(request.body["messages"][0]["role"], "user");
}

#[tokio::test]
async fn betas_travel_in_the_header_not_the_body() {
    let outcome = run(StreamSpec::default(), vec![user("hello")], vec![], |_| {}).await;
    let beta = outcome
        .request
        .header("anthropic-beta")
        .expect("anthropic-beta header");
    assert!(beta.contains("claude-code-20250219"), "{beta}");
    assert!(outcome.request.body.get("betas").is_none());
}

#[tokio::test]
async fn cache_breakpoint_is_on_the_last_message_and_system_blocks_are_cached() {
    let outcome = run(
        StreamSpec::default(),
        vec![
            user("one"),
            assistant(vec![AssistantContent::Text("two".into())]),
            user("three"),
        ],
        vec![],
        |_| {},
    )
    .await;
    let body = &outcome.request.body;
    for (index, expected) in [(0, false), (1, false), (2, true)] {
        let last = blocks(body, index).last().unwrap();
        assert_eq!(
            last.get("cache_control").is_some(),
            expected,
            "message {index}: {body}"
        );
    }
    assert_eq!(
        blocks(body, 2).last().unwrap()["cache_control"]["type"],
        "ephemeral"
    );
    let system = body["system"].as_array().expect("system blocks");
    assert!(!system.is_empty());
    assert!(
        system
            .iter()
            .all(|block| block["cache_control"]["type"] == "ephemeral"),
        "{system:?}"
    );
}

#[tokio::test]
async fn skip_cache_write_moves_the_breakpoint_to_the_second_last_message() {
    let outcome = run(
        StreamSpec::default(),
        vec![
            user("one"),
            assistant(vec![AssistantContent::Text("two".into())]),
            user("three"),
        ],
        vec![],
        |options| options.skip_cache_write = Some(true),
    )
    .await;
    let body = &outcome.request.body;
    assert!(
        blocks(body, 0)
            .last()
            .unwrap()
            .get("cache_control")
            .is_none()
    );
    assert!(
        blocks(body, 1)
            .last()
            .unwrap()
            .get("cache_control")
            .is_some()
    );
    assert!(
        blocks(body, 2)
            .last()
            .unwrap()
            .get("cache_control")
            .is_none()
    );
}

#[tokio::test]
async fn disabling_prompt_caching_removes_every_marker() {
    let outcome = run(
        StreamSpec::default(),
        vec![user("hello")],
        vec![],
        |options| options.enable_prompt_caching = Some(false),
    )
    .await;
    let text = outcome.request.body.to_string();
    assert!(!text.contains("cache_control"), "{text}");
}

#[tokio::test]
async fn thinking_signature_and_tool_blocks_replay_on_the_next_request() {
    let outcome = run(
        StreamSpec::default(),
        vec![
            user("go"),
            assistant(vec![
                AssistantContent::Thinking {
                    text: "reasoning".into(),
                    signature: "sig_abc".into(),
                },
                AssistantContent::ToolUse(ToolUseBlock {
                    id: ToolUseId("toolu_1".into()),
                    name: "Probe".into(),
                    input: json!({"a": 1}),
                }),
            ]),
            tool_result("toolu_1", "done"),
        ],
        vec![probe_tool()],
        |_| {},
    )
    .await;
    let body = &outcome.request.body;
    let assistant_blocks = blocks(body, 1);
    assert_eq!(assistant_blocks[0]["type"], "thinking");
    assert_eq!(assistant_blocks[0]["thinking"], "reasoning");
    assert_eq!(assistant_blocks[0]["signature"], "sig_abc");
    assert_eq!(assistant_blocks[1]["type"], "tool_use");
    assert_eq!(assistant_blocks[1]["id"], "toolu_1");
    assert_eq!(assistant_blocks[1]["input"], json!({"a": 1}));
    let result = &blocks(body, 2)[0];
    assert_eq!(result["type"], "tool_result");
    assert_eq!(result["tool_use_id"], "toolu_1");
    assert_eq!(result["content"], "done");
    assert_eq!(result["is_error"], false);
    // The breakpoint lands on the tool result, the last block of the last message.
    assert_eq!(result["cache_control"]["type"], "ephemeral");
    // The tool definition is sent with its name and schema, and tail cache_control.
    let tool = body["tools"]
        .as_array()
        .and_then(|tools| tools.iter().find(|t| t["name"] == "Probe"))
        .expect("Probe tool on the wire");
    assert_eq!(tool["input_schema"]["properties"]["a"]["type"], "number");
    assert_eq!(tool["cache_control"]["type"], "ephemeral");
}

/// Sends one request under a profile with `retention` and returns the TTL on
/// the tool, system and message breakpoints.
async fn breakpoint_ttls(
    retention: Option<crate::utils::settings::nyan::CacheRetention>,
) -> [serde_json::Value; 3] {
    request_profile_breakpoint_ttls(retention, None).await
}

/// [`breakpoint_ttls`] with the request routed through a second profile
/// `side` carrying `side_retention` (when set) instead of the main profile.
async fn request_profile_breakpoint_ttls(
    main_retention: Option<crate::utils::settings::nyan::CacheRetention>,
    side_retention: Option<crate::utils::settings::nyan::CacheRetention>,
) -> [serde_json::Value; 3] {
    let mock = MockAnthropic::start(vec![Reply::Sse(message_stream(StreamSpec::default()))]).await;
    let _env = Env::new(&mock.base_url);
    super::target::install_test_profiles_with_retention(
        &mock.base_url,
        KEY_VAR,
        main_retention,
        side_retention,
    );

    let mut options = Options::new(
        "claude-sonnet-4-6".to_string(),
        "repl_main_thread".to_string(),
    );
    if side_retention.is_some() {
        options.profile = Some("side".to_string());
    }
    let messages = vec![user("hello")];
    let system_prompt = vec!["System static instructions.".to_string()];
    let tools = vec![probe_tool()];
    let mut rx = query_model_with_streaming(
        &messages,
        &system_prompt,
        &ThinkingConfig::Disabled,
        &tools,
        &options,
    )
    .await
    .expect("stream opens");
    while let Some(_) = rx.recv().await {}

    let requests = mock.requests();
    let body = &requests[0].body;
    let tool = body["tools"]
        .as_array()
        .and_then(|tools| tools.iter().find(|t| t["name"] == "Probe"))
        .expect("Probe tool on the wire");
    assert_eq!(tool["cache_control"]["type"], "ephemeral");
    let system = body["system"].as_array().expect("system blocks");
    let cached_sys = system
        .iter()
        .find(|b| b.get("cache_control").is_some())
        .expect("cached system block");
    let msg = &body["messages"][0]["content"][0];
    [
        tool["cache_control"]["ttl"].clone(),
        cached_sys["cache_control"]["ttl"].clone(),
        msg["cache_control"]["ttl"].clone(),
    ]
}

#[tokio::test]
async fn cache_retention_default_profile_sends_1h_ttl_on_the_wire() {
    assert_eq!(
        breakpoint_ttls(None).await,
        [json!("1h"), json!("1h"), json!("1h")]
    );
}

#[tokio::test]
async fn cache_retention_long_profile_sends_1h_ttl_on_the_wire() {
    use crate::utils::settings::nyan::CacheRetention;
    assert_eq!(
        breakpoint_ttls(Some(CacheRetention::Long)).await,
        [json!("1h"), json!("1h"), json!("1h")]
    );
}

#[tokio::test]
async fn cache_retention_short_profile_sends_explicit_5m_ttl_on_the_wire() {
    use crate::utils::settings::nyan::CacheRetention;
    assert_eq!(
        breakpoint_ttls(Some(CacheRetention::Short)).await,
        [json!("5m"), json!("5m"), json!("5m")]
    );
}

#[tokio::test]
async fn cache_retention_short_request_profile_overrides_long_main_on_the_wire() {
    use crate::utils::settings::nyan::CacheRetention;
    assert_eq!(
        request_profile_breakpoint_ttls(Some(CacheRetention::Long), Some(CacheRetention::Short))
            .await,
        [json!("5m"), json!("5m"), json!("5m")]
    );
}

#[tokio::test]
async fn cache_retention_long_request_profile_overrides_short_main_on_the_wire() {
    use crate::utils::settings::nyan::CacheRetention;
    assert_eq!(
        request_profile_breakpoint_ttls(Some(CacheRetention::Short), Some(CacheRetention::Long))
            .await,
        [json!("1h"), json!("1h"), json!("1h")]
    );
}

#[tokio::test]
async fn tool_search_placeholder_is_on_the_wire_and_tool_cache_anchors_on_a_full_tool() {
    let mock = MockAnthropic::start(vec![Reply::Sse(message_stream(StreamSpec::default()))]).await;
    let _env = Env::new(&mock.base_url);

    let options = Options::new(
        "claude-sonnet-4-6".to_string(),
        "repl_main_thread".to_string(),
    );
    // A discovered WebFetch goes on the wire as a deferred tool after Probe.
    let mut discovery = crate::utils::messages::create_user_message(String::new());
    discovery.content = vec![crate::types::message::UserContent::ToolResult(ToolResult {
        tool_use_id: ToolUseId("toolu_search".to_string()),
        content: String::new(),
        is_error: false,
        content_blocks: vec![
            crate::types::message::ToolResultContentBlock::ToolReference {
                tool_name: "WebFetch".to_string(),
            },
        ],
        tool_use_result: None,
    })];
    let messages = vec![
        user("search"),
        assistant(vec![AssistantContent::ToolUse(ToolUseBlock {
            id: ToolUseId("toolu_search".into()),
            name: "ToolSearch".into(),
            input: json!({"query": "fetch"}),
        })]),
        Message::User(discovery),
    ];
    let system_prompt = vec!["System instructions.".to_string()];
    let tools = vec![
        crate::tools::tool_search_tool::tool_search_tool_schema(),
        probe_tool(),
        crate::tools::web_fetch_tool::web_fetch_tool_schema(),
    ];
    let mut rx = query_model_with_streaming(
        &messages,
        &system_prompt,
        &ThinkingConfig::Disabled,
        &tools,
        &options,
    )
    .await
    .expect("stream opens");
    while let Some(_) = rx.recv().await {}

    let requests = mock.requests();
    let body = &requests[0].body;
    let tools = body["tools"].as_array().expect("tools on the wire");

    let names = tools.iter().map(|t| t["name"].clone()).collect::<Vec<_>>();
    assert_eq!(
        names,
        [
            json!("ToolSearch"),
            json!("Probe"),
            json!("WebFetch"),
            json!(crate::services::api::request::DEFERRED_TOOL_PLACEHOLDER_NAME),
        ]
    );
    let deferred = tools
        .iter()
        .filter(|t| t["defer_loading"] == true)
        .collect::<Vec<_>>();
    assert_eq!(deferred.len(), 2, "WebFetch and the placeholder");
    // Anthropic rejects cache_control on a deferred tool.
    assert!(deferred.iter().all(|t| t.get("cache_control").is_none()));

    // The breakpoint sits on Probe, the last tool sent in full.
    let anchors = tools
        .iter()
        .filter(|t| t.get("cache_control").is_some())
        .collect::<Vec<_>>();
    assert_eq!(anchors.len(), 1);
    assert_eq!(anchors[0]["name"], "Probe");
    assert_eq!(anchors[0]["cache_control"]["type"], "ephemeral");
}

// ---------------------------------------------------------------------------
// Stream back
// ---------------------------------------------------------------------------

#[tokio::test]
async fn streamed_thinking_text_and_tool_call_become_ordered_assistant_blocks() {
    let outcome = run(
        StreamSpec {
            thinking: Some("let me think"),
            text: Some("I will call it"),
            tool_call: Some(("Probe", json!({"a": 42}))),
            stop_reason: "tool_use",
            cache_read_tokens: 40,
            cache_write_tokens: 7,
            output_tokens: 55,
            ..StreamSpec::default()
        },
        vec![user("go")],
        vec![probe_tool()],
        |_| {},
    )
    .await;
    let messages = assistants(&outcome.items);
    assert_eq!(messages.len(), 3, "one assistant message per content block");
    let first = |message: &AssistantMessage| message.first_content_block().cloned().unwrap();
    assert_eq!(
        first(messages[0]),
        AssistantContent::Thinking {
            text: "let me think".into(),
            signature: "sig-mock".into()
        }
    );
    assert_eq!(
        first(messages[1]),
        AssistantContent::Text("I will call it".into())
    );
    assert_eq!(
        first(messages[2]),
        AssistantContent::ToolUse(ToolUseBlock {
            id: ToolUseId("toolu_mock".into()),
            name: "Probe".into(),
            input: json!({"a": 42}),
        })
    );
    // Identity rides on every block's envelope. The API message id comes from
    // `message_start`; the request id comes from the response header, which the
    // adapter copies onto `message_start` (the SDK's streaming `message_start`
    // carries none). Before the query layer read the adapter's stream this was
    // `None` for every streamed assistant.
    assert_eq!(messages[2].request_id(), Some("req_mock"));
    assert_eq!(messages[2].api_message_id(), Some("msg_mock"));

    // Cumulative usage and the stop reason arrive with message_delta.
    let usage = outcome
        .items
        .iter()
        .rev()
        .find_map(|item| match item {
            QueryModelStreamItem::AssistantDelta { stop_reason, usage } => {
                Some((stop_reason.clone(), usage.clone()))
            }
            _ => None,
        })
        .expect("message_delta item");
    assert_eq!(
        usage.0,
        Some(crate::types::message::StopReason::ToolUse),
        "{usage:?}"
    );
    let usage = usage.1.expect("usage");
    assert_eq!(usage.output_tokens, 55);
    assert_eq!(usage.input_tokens, 10, "message_start input survives");
    assert_eq!(usage.cache_read_input_tokens, 40);
    assert_eq!(usage.cache_creation_input_tokens, 7);
}

#[tokio::test]
async fn max_tokens_stop_reports_the_output_limit_error() {
    let outcome = run(
        StreamSpec {
            stop_reason: "max_tokens",
            ..StreamSpec::default()
        },
        vec![user("go")],
        vec![],
        |_| {},
    )
    .await;
    assert!(
        outcome.items.iter().any(|item| matches!(
            item,
            QueryModelStreamItem::SystemError(error) if error.api_error == "max_output_tokens"
        )),
        "{:?}",
        outcome.items
    );
}

#[tokio::test]
async fn an_overloaded_529_retries_under_the_unified_budget_and_recovers() {
    let mock = MockAnthropic::start(vec![
        Reply::Error {
            status: 529,
            body: json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}}),
        },
        Reply::Sse(message_stream(StreamSpec::default())),
    ])
    .await;
    let _env = Env::new(&mock.base_url);
    // Override the fixture's NYAN_MAX_RETRIES="0" pin so the default budget (2) is active.
    let _retries = EnvVarGuard::unset("NYAN_MAX_RETRIES");

    let options = Options::new(
        "claude-sonnet-4-6".to_string(),
        "repl_main_thread".to_string(),
    );
    let mut rx = query_model_with_streaming(
        &[user("hello")],
        &vec!["You are a test.".to_string()],
        &ThinkingConfig::Disabled,
        &[],
        &options,
    )
    .await
    .expect("stream opens after retry");
    let mut items = Vec::new();
    while let Some(item) = tokio::time::timeout(std::time::Duration::from_secs(10), rx.recv())
        .await
        .expect("stream item before deadline")
    {
        items.push(item);
    }
    let messages = assistants(&items);
    assert!(
        !messages.is_empty(),
        "expected assistant response: {items:?}"
    );
    assert_eq!(
        mock.requests().len(),
        2,
        "overloaded 529 was retried once and recovered"
    );
}

#[tokio::test]
async fn a_streamed_request_is_accounted_once_and_attributed_to_its_source() {
    crate::cost_tracker::reset_cost_state();
    run(
        StreamSpec {
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
            cache_read_tokens: 1_000_000,
            cache_write_tokens: 1_000_000,
            ..StreamSpec::default()
        },
        vec![user("go")],
        vec![],
        |options| options.query_source = "agent:builtin:general-purpose".to_string(),
    )
    .await;

    let total = crate::cost_tracker::get_total_usage();
    assert_eq!(
        total.requests, 1,
        "message_start and message_delta make one record"
    );
    assert_eq!(total.output_tokens, 1_000_000);
    // claude-sonnet-4-6: $3 input, $15 output, $0.30 read, $3.75 5m write.
    assert!((crate::cost_tracker::get_total_cost() - (3.0 + 15.0 + 0.3 + 3.75)).abs() < 1e-9);
    let by_source = crate::cost_tracker::get_source_usage();
    assert_eq!(
        by_source.keys().collect::<Vec<_>>(),
        [&crate::cost_tracker::CostSource::Subagent]
    );
    crate::cost_tracker::reset_cost_state();
}

/// With no effort on the profile or the session, the default adaptive launch
/// still reaches Anthropic as adaptive thinking with no effort parameter.
#[tokio::test]
async fn an_unset_effort_keeps_the_adaptive_default_on_the_wire() {
    let mock = MockAnthropic::start(vec![Reply::Sse(message_stream(StreamSpec::default()))]).await;
    let _env = Env::new(&mock.base_url);

    let context =
        crate::tool::ToolUseContext::default().with_thinking_config(Some(ThinkingConfig::Adaptive));
    let request = crate::query::build_call_model_request(
        vec![user("hello")],
        crate::tool::ToolPermissionContext::default(),
        &crate::constants::query_source::QuerySource::Prompt,
        &["You are a test.".to_string()],
        &std::collections::BTreeMap::new(),
        &crate::state::app_state_store::McpState::default(),
        &context,
    );
    assert_eq!(request.options.effort_value, None);
    let mut rx = query_model_with_streaming(
        &request.messages,
        &request.system_prompt,
        &request.thinking_config,
        &[],
        &request.options,
    )
    .await
    .expect("stream opens");
    while tokio::time::timeout(std::time::Duration::from_secs(10), rx.recv())
        .await
        .expect("stream item before deadline")
        .is_some()
    {}

    let body = &mock.requests()[0].body;
    assert_eq!(body["thinking"], json!({"type": "adaptive"}), "{body}");
    assert!(body.pointer("/output_config/effort").is_none(), "{body}");
}

/// `/model deep:off` really switches the session: the next main-loop request
/// goes to the `deep` profile's provider, credential and model, at the effort
/// the switch named, and nothing reaches the previous profile's endpoint.
#[tokio::test]
async fn model_command_routes_the_next_request_through_the_new_profile() {
    let main = MockAnthropic::start(Vec::new()).await;
    let deep = MockAnthropic::start(vec![Reply::Sse(message_stream(StreamSpec::default()))]).await;
    let _env = Env::new(&main.base_url);
    let _deep_key = EnvVarGuard::set("NYAN_FIXTURE_DEEP_KEY", "sk-fixture-deep-key");
    crate::utils::settings::settings_cache::install_test_settings(
        serde_json::from_value(json!({"nyan": {
            "providers": {
                "main-provider": {"kind": "anthropic", "protocol": "anthropicMessages",
                    "baseUrl": main.base_url, "credential": {"env": KEY_VAR}},
                "deep-provider": {"kind": "anthropic", "protocol": "anthropicMessages",
                    "baseUrl": deep.base_url, "credential": {"env": "NYAN_FIXTURE_DEEP_KEY"}}
            },
            "models": {
                "main": {"provider": "main-provider", "model": "claude-sonnet-4-6", "effort": "high",
                    "capabilities": {"contextWindow": 200000, "maxOutputTokens": 64000,
                        "thinking": "adaptive"}},
                "deep": {"provider": "deep-provider", "model": "claude-opus-4-6", "effort": "max",
                    "capabilities": {"contextWindow": 200000, "maxOutputTokens": 64000,
                        "thinking": "adaptive"}}
            },
            "selection": {"main": "main"}
        }}))
        .unwrap(),
    );
    let store = crate::state::store::AppStore::new(
        crate::state::app_state_store::AppState::default(),
        None,
    );

    let Some(crate::commands::model::ModelArgs::Switch { profile, effort }) =
        crate::commands::model::parse_args("deep:off")
    else {
        panic!("deep:off is a profile switch");
    };
    assert_eq!(
        crate::commands::model::apply_switch(Some(&store), profile, effort),
        "Set model to deep (thinking off)"
    );

    let context = crate::tool::ToolUseContext::default().with_app_store(store.clone());
    let request = crate::query::build_call_model_request(
        vec![user("hello")],
        crate::tool::ToolPermissionContext::default(),
        &crate::constants::query_source::QuerySource::Prompt,
        &["You are a test.".to_string()],
        &std::collections::BTreeMap::new(),
        &crate::state::app_state_store::McpState::default(),
        &context,
    );
    assert_eq!(request.options.profile.as_deref(), Some("deep"));
    assert_eq!(request.options.model, "claude-opus-4-6");
    let mut rx = query_model_with_streaming(
        &request.messages,
        &request.system_prompt,
        &request.thinking_config,
        &[],
        &request.options,
    )
    .await
    .expect("stream opens");
    while tokio::time::timeout(std::time::Duration::from_secs(10), rx.recv())
        .await
        .expect("stream item before deadline")
        .is_some()
    {}

    assert!(
        main.requests().is_empty(),
        "nothing reaches the old profile"
    );
    let requests = deep.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].header("x-api-key"), Some("sk-fixture-deep-key"));
    assert_eq!(requests[0].body["model"], "claude-opus-4-6");
    // `:off` is the session effort, not the profile's `max`.
    assert!(
        requests[0]
            .body
            .get("thinking")
            .is_none_or(|thinking| thinking["type"] == "disabled")
    );
}
