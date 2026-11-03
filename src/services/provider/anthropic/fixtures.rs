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
use crate::services::api::claude::{Options, QueryModelStreamItem, query_model_with_streaming};
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
            EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root),
            EnvVarGuard::set("CLAUDE_CODE_SIMPLE", "1"),
            EnvVarGuard::set(KEY_VAR, KEY),
            EnvVarGuard::set("CLAUDE_CODE_MAX_RETRIES", "0"),
            EnvVarGuard::set("API_TIMEOUT_MS", "5000"),
            EnvVarGuard::set("NO_PROXY", "*"),
            EnvVarGuard::set("no_proxy", "*"),
        ];
        for key in [
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
            "CLAUDE_CODE_USE_FOUNDRY",
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_BASE_URL",
            "ANTHROPIC_CUSTOM_HEADERS",
            "ANTHROPIC_BETAS",
            "CLAUDE_CODE_EXTRA_BODY",
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
    // The tool definition is sent with its name and schema.
    let tool = body["tools"]
        .as_array()
        .and_then(|tools| tools.iter().find(|t| t["name"] == "Probe"))
        .expect("Probe tool on the wire");
    assert_eq!(tool["input_schema"]["properties"]["a"]["type"], "number");
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
    // `message_start`. KNOWN GAP, identical on master: the SDK's streaming
    // `message_start` carries no `_request_id`, and `drain_sdk_message_stream`
    // never consults the response header (`ApiResponse.request_id`), so a
    // streamed assistant has no request id. This pins today's behavior; fixing
    // it changes `AssistantMessageIdentity` and is left to its own change, at
    // which point this assertion should become `Some("req_mock")`.
    assert_eq!(messages[2].request_id(), None);
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
