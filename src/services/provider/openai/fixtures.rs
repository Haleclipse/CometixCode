//! Contract fixtures for the OpenAI adapters, over a real local HTTP server.
//!
//! @nyan: each protocol is driven end to end through the real adapter,
//! transport and SSE parser, and asserts three things: the exact request that
//! reaches the wire (path, credential, body), the IR the stream becomes, and how
//! HTTP failures are classified. The stream machines and body builders have
//! their own unit tests; these prove the pieces are wired together.

use futures::StreamExt;
use serde_json::{Value, json};

use super::adapter::OpenAiAdapter;
use super::mock::{MockOpenAi, Recorded, Reply};
use super::target::{Compat, OpenAiAuth, OpenAiTarget};
use crate::services::provider::adapter::{Cancel, ProviderAdapter, ResolvedTarget};
use crate::services::provider::ir::{
    AssistantPart, BlockStart, CachePolicy, ClientContext, Delta, Effort, Message, ModelRequest,
    Protocol, ProviderError, ProviderErrorKind, StopReason, StreamEventKind, ToolChoice,
    ToolResultContent, ToolSpec, UserPart,
};
use crate::utils::model::profile::ResolvedCapabilities;
use crate::utils::provider_auth::reference::Secret;
use crate::utils::settings::nyan::{ProviderKind, ProviderProtocol};
use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK, TestEnvGuard};

struct Env {
    _lock: TestEnvGuard<'static>,
    _guards: Vec<EnvVarGuard>,
}

fn env() -> Env {
    let lock = TEST_ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let guards = vec![
        EnvVarGuard::set("NO_PROXY", "*"),
        EnvVarGuard::set("no_proxy", "*"),
        EnvVarGuard::unset("HTTPS_PROXY"),
        EnvVarGuard::unset("HTTP_PROXY"),
        EnvVarGuard::unset("https_proxy"),
        EnvVarGuard::unset("http_proxy"),
    ];
    crate::utils::settings::settings_cache::reset_settings_cache();
    Env {
        _lock: lock,
        _guards: guards,
    }
}

fn all() -> ResolvedCapabilities {
    ResolvedCapabilities {
        tools: true,
        images: true,
        thinking: true,
        structured_output: true,
        streaming: true,
    }
}

fn resolved(protocol: Protocol) -> ResolvedTarget {
    ResolvedTarget {
        profile: "main".into(),
        provider: "openai".into(),
        protocol,
        model: "gpt-5".into(),
        context_window: 400_000,
        max_output_tokens: 8_000,
        capabilities: all(),
        cache_retention: Default::default(),
        extra: None,
    }
}

fn target(base_url: &str, protocol: ProviderProtocol, auth: OpenAiAuth) -> OpenAiTarget {
    OpenAiTarget {
        profile: "main".into(),
        provider: "openai".into(),
        provider_kind: ProviderKind::Openai,
        protocol,
        base_url: base_url.into(),
        auth,
        optional_env: None,
        compat: Compat::resolve(ProviderKind::Openai, None),
        cache_retention: Default::default(),
    }
}

fn key() -> OpenAiAuth {
    OpenAiAuth::ApiKey(Secret::new("sk-fixture"))
}

fn chat_adapter(base_url: &str) -> OpenAiAdapter {
    OpenAiAdapter::chat(target(base_url, ProviderProtocol::OpenaiChat, key()))
}

fn responses_adapter(base_url: &str) -> OpenAiAdapter {
    OpenAiAdapter::responses(target(base_url, ProviderProtocol::OpenaiResponses, key()))
}

fn read_tool() -> ToolSpec {
    ToolSpec {
        name: "Read".into(),
        description: "reads a file".into(),
        input_schema: json!({"type": "object", "properties": {"path": {"type": "string"}}}),
        strict: false,
        deferrable: false,
        is_mcp: false,
    }
}

fn request(messages: Vec<Message>) -> ModelRequest {
    ModelRequest {
        model: "gpt-5".into(),
        system: vec!["You are a test.".into()],
        messages,
        tools: vec![read_tool()],
        tool_choice: ToolChoice::Auto,
        effort: Effort::Medium,
        max_output_tokens: 4_000,
        temperature: None,
        structured_output: None,
        cache: CachePolicy::default(),
        query_source: "repl_main_thread".into(),
        agent_id: None,
        disable_parallel_tools: false,
        discovered_tools: vec![],
        tools_pending: false,
        client: ClientContext::default(),
        extensions: vec![],
    }
}

fn hello() -> Vec<Message> {
    vec![Message::User {
        parts: vec![UserPart::Text {
            text: "hello".into(),
        }],
    }]
}

async fn collect(
    adapter: &OpenAiAdapter,
    protocol: Protocol,
    request: &ModelRequest,
) -> (Vec<StreamEventKind>, Option<ProviderError>) {
    let cancel = anthropic_sdk::AbortSignal::new();
    let opened = adapter
        .send(&resolved(protocol), request, &cancel)
        .await
        .expect("stream opens");
    let mut kinds = Vec::new();
    let mut events = opened.events;
    while let Some(item) = tokio::time::timeout(std::time::Duration::from_secs(10), events.next())
        .await
        .expect("stream item before deadline")
    {
        match item {
            Ok(event) => kinds.push(event.kind),
            Err(error) => return (kinds, Some(error)),
        }
    }
    (kinds, None)
}

async fn open_error(
    adapter: &OpenAiAdapter,
    protocol: Protocol,
    request: &ModelRequest,
) -> ProviderError {
    let cancel: Cancel = anthropic_sdk::AbortSignal::new();
    adapter
        .send(&resolved(protocol), request, &cancel)
        .await
        .err()
        .expect("an error status must not open a stream")
}

fn only(mock: &MockOpenAi) -> Recorded {
    let requests = mock.requests();
    assert_eq!(requests.len(), 1, "exactly one HTTP request");
    requests.into_iter().next().unwrap()
}

// ---------------------------------------------------------------------------
// Chat Completions
// ---------------------------------------------------------------------------

fn chat_chunk(delta: Value, finish: Value) -> Value {
    json!({"id": "chatcmpl-1", "model": "gpt-5", "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
}

#[tokio::test]
async fn chat_request_reaches_the_wire_with_the_key_path_and_body() {
    let _env = env();
    let mock = MockOpenAi::start(vec![Reply::sse_data([chat_chunk(
        json!({"content": "hi"}),
        json!("stop"),
    )])])
    .await;
    let adapter = chat_adapter(&mock.base_url);
    let (_, error) = collect(&adapter, Protocol::OpenaiChat, &request(hello())).await;
    assert!(error.is_none(), "{error:?}");

    let sent = only(&mock);
    assert!(
        sent.request_line.starts_with("POST /chat/completions"),
        "{}",
        sent.request_line
    );
    assert_eq!(sent.header("authorization"), Some("Bearer sk-fixture"));
    assert_eq!(sent.header("accept"), Some("text/event-stream"));
    assert_eq!(
        sent.header("anthropic-beta"),
        None,
        "no Anthropic header reaches OpenAI"
    );
    assert_eq!(sent.header("x-app"), Some("cli"));
    let body = &sent.body;
    assert_eq!(body["model"], "gpt-5");
    assert_eq!(body["stream"], true);
    assert_eq!(body["stream_options"], json!({"include_usage": true}));
    assert_eq!(body["max_completion_tokens"], 4_000);
    assert_eq!(body["reasoning_effort"], "medium");
    assert_eq!(
        body["messages"][0],
        json!({"role": "system", "content": "You are a test."})
    );
    assert_eq!(
        body["messages"][1],
        json!({"role": "user", "content": "hello"})
    );
    assert_eq!(body["tools"][0]["function"]["name"], "Read");
    assert!(body.get("prompt_cache_key").is_some());
    for forbidden in ["cache_control", "betas", "input", "instructions", "store"] {
        assert!(
            body.get(forbidden).is_none(),
            "{forbidden} is not a Chat field: {body}"
        );
    }
}

#[tokio::test]
async fn chat_stream_with_a_tool_call_becomes_ir_with_usage_and_the_header_request_id() {
    let _env = env();
    let mock = MockOpenAi::start(vec![Reply::sse_data([
        chat_chunk(json!({"role": "assistant", "content": ""}), Value::Null),
        chat_chunk(json!({"content": "Reading."}), Value::Null),
        chat_chunk(
            json!({"tool_calls": [{"index": 0, "id": "call_a", "type": "function",
                "function": {"name": "Read", "arguments": "{\"path\":"}}]}),
            Value::Null,
        ),
        chat_chunk(
            json!({"tool_calls": [{"index": 0, "function": {"arguments": "\"a.rs\"}"}}]}),
            Value::Null,
        ),
        chat_chunk(json!({}), json!("tool_calls")),
        json!({"id": "chatcmpl-1", "choices": [], "usage": {
            "prompt_tokens": 50, "completion_tokens": 9,
            "prompt_tokens_details": {"cached_tokens": 10}}}),
    ])])
    .await;
    let adapter = chat_adapter(&mock.base_url);
    let (kinds, error) = collect(&adapter, Protocol::OpenaiChat, &request(hello())).await;
    assert!(error.is_none(), "{error:?}");

    assert!(matches!(
        &kinds[0],
        StreamEventKind::MessageStart { request_id: Some(r), .. } if r == "req_mock"
    ));
    assert!(kinds.contains(&StreamEventKind::BlockStart {
        index: 1,
        block: BlockStart::ToolCall {
            id: "call_a".into(),
            name: "Read".into(),
            server: false,
            input: json!({}),
        }
    }));
    let args: String = kinds
        .iter()
        .filter_map(|k| match k {
            StreamEventKind::BlockDelta {
                delta: Delta::ToolInput { json },
                ..
            } => Some(json.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        serde_json::from_str::<Value>(&args).unwrap(),
        json!({"path": "a.rs"})
    );
    let Some(StreamEventKind::MessageDelta { stop_reason, usage }) = kinds.iter().rev().nth(1)
    else {
        panic!("{kinds:?}")
    };
    assert_eq!(*stop_reason, Some(StopReason::ToolUse));
    assert_eq!(
        (
            usage.input_tokens,
            usage.cache_read_tokens,
            usage.output_tokens
        ),
        (40, 10, 9)
    );
    assert_eq!(kinds.last(), Some(&StreamEventKind::MessageStop));
}

#[tokio::test]
async fn chat_replays_a_tool_round_trip_without_thinking() {
    let _env = env();
    let mock = MockOpenAi::start(vec![Reply::sse_data([chat_chunk(
        json!({"content": "ok"}),
        json!("stop"),
    )])])
    .await;
    let adapter = chat_adapter(&mock.base_url);
    let history = vec![
        Message::User {
            parts: vec![UserPart::Text { text: "go".into() }],
        },
        Message::Assistant {
            parts: vec![
                AssistantPart::Thinking {
                    text: "private".into(),
                    signature: "sig".into(),
                    provider_state: None,
                },
                AssistantPart::ToolCall {
                    id: "call_1".into(),
                    name: "Read".into(),
                    input: json!({"path": "a"}),
                    provider_state: None,
                },
            ],
        },
        Message::User {
            parts: vec![UserPart::ToolResult {
                tool_call_id: "call_1".into(),
                content: ToolResultContent::Text("data".into()),
                is_error: false,
            }],
        },
    ];
    let (_, error) = collect(&adapter, Protocol::OpenaiChat, &request(history)).await;
    assert!(error.is_none());
    let sent = only(&mock);
    let messages = sent.body["messages"].as_array().unwrap();
    assert_eq!(messages[2]["tool_calls"][0]["id"], "call_1");
    assert_eq!(
        messages[3],
        json!({"role": "tool", "tool_call_id": "call_1", "content": "data"})
    );
    assert!(!sent.body.to_string().contains("private"));
}

#[tokio::test]
async fn chat_http_failures_are_classified_with_status_request_id_and_retry_after() {
    let _env = env();
    let cases = [
        (429, ProviderErrorKind::RateLimited),
        (500, ProviderErrorKind::Transient { retryable: true }),
        (503, ProviderErrorKind::Transient { retryable: true }),
        (401, ProviderErrorKind::Unauthorized),
    ];
    for (status, kind) in cases {
        let mock = MockOpenAi::start(vec![Reply::Error {
            status,
            headers: vec![("retry-after".into(), "3".into())],
            body: json!({"error": {"message": "boom", "type": "x", "code": null}}).to_string(),
        }])
        .await;
        let error = open_error(
            &chat_adapter(&mock.base_url),
            Protocol::OpenaiChat,
            &request(hello()),
        )
        .await;
        assert_eq!(error.kind, kind, "status {status}");
        assert_eq!(error.status, Some(status));
        assert_eq!(error.request_id.as_deref(), Some("req_mock"));
        assert_eq!(error.message, "boom");
        assert_eq!(error.retry_after, Some(std::time::Duration::from_secs(3)));
        assert_eq!(mock.requests().len(), 1, "no automatic retry in this PR");
    }
}

#[tokio::test]
async fn chat_cut_off_mid_stream_is_a_partial_stream_not_a_replayable_failure() {
    let _env = env();
    let mock = MockOpenAi::start(vec![Reply::SseThenCut(vec![(
        None,
        chat_chunk(json!({"content": "half an ans"}), Value::Null).to_string(),
    )])])
    .await;
    let (kinds, error) = collect(
        &chat_adapter(&mock.base_url),
        Protocol::OpenaiChat,
        &request(hello()),
    )
    .await;
    assert!(
        kinds
            .iter()
            .any(|k| matches!(k, StreamEventKind::BlockDelta { .. }))
    );
    assert_eq!(
        error.expect("must fail").kind,
        ProviderErrorKind::PartialStream
    );
}

// ---------------------------------------------------------------------------
// Responses
// ---------------------------------------------------------------------------

fn created() -> Value {
    json!({"type": "response.created", "response": {"id": "resp_1", "model": "gpt-5"}})
}

fn completed(extra: Value) -> Value {
    let mut response = json!({"id": "resp_1", "status": "completed", "usage": {
        "input_tokens": 80, "output_tokens": 25,
        "input_tokens_details": {"cached_tokens": 30},
        "output_tokens_details": {"reasoning_tokens": 11}}});
    if let (Some(target), Some(extra)) = (response.as_object_mut(), extra.as_object()) {
        target.extend(extra.clone());
    }
    json!({"type": "response.completed", "response": response})
}

fn responses_script() -> Vec<Value> {
    vec![
        created(),
        json!({"type": "response.output_item.added", "output_index": 0, "item": {"type": "reasoning", "id": "rs_1", "summary": []}}),
        json!({"type": "response.reasoning_summary_text.delta", "output_index": 0, "delta": "thinking"}),
        json!({"type": "response.output_item.done", "output_index": 0, "item": {"type": "reasoning", "id": "rs_1", "summary": [{"type": "summary_text", "text": "thinking"}], "encrypted_content": "ENC"}}),
        json!({"type": "response.output_item.added", "output_index": 1, "item": {"type": "message", "id": "msg_1", "content": []}}),
        json!({"type": "response.output_text.delta", "output_index": 1, "delta": "Reading."}),
        json!({"type": "response.output_item.done", "output_index": 1, "item": {"type": "message", "content": [{"type": "output_text", "text": "Reading."}]}}),
        json!({"type": "response.output_item.added", "output_index": 2, "item": {"type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "Read", "arguments": ""}}),
        json!({"type": "response.function_call_arguments.delta", "output_index": 2, "delta": "{\"path\":"}),
        json!({"type": "response.function_call_arguments.delta", "output_index": 2, "delta": "\"a.rs\"}"}),
        json!({"type": "response.output_item.done", "output_index": 2, "item": {"type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "Read", "arguments": "{\"path\":\"a.rs\"}"}}),
        completed(json!({})),
    ]
}

#[tokio::test]
async fn responses_request_reaches_the_wire_with_the_key_path_and_body() {
    let _env = env();
    let mock = MockOpenAi::start(vec![Reply::sse_data([created(), completed(json!({}))])]).await;
    let adapter = responses_adapter(&mock.base_url);
    let (_, error) = collect(&adapter, Protocol::OpenaiResponses, &request(hello())).await;
    assert!(error.is_none(), "{error:?}");

    let sent = only(&mock);
    assert!(
        sent.request_line.starts_with("POST /responses"),
        "{}",
        sent.request_line
    );
    assert_eq!(sent.header("authorization"), Some("Bearer sk-fixture"));
    assert_eq!(
        sent.header("chatgpt-account-id"),
        None,
        "the API-key path is not the ChatGPT path"
    );
    let body = &sent.body;
    assert_eq!(body["model"], "gpt-5");
    assert_eq!(body["store"], false);
    assert_eq!(body["stream"], true);
    assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
    assert_eq!(body["instructions"], "You are a test.");
    assert_eq!(body["max_output_tokens"], 4_000);
    assert_eq!(
        body["reasoning"],
        json!({"effort": "medium", "summary": "auto"})
    );
    assert_eq!(
        body["input"][0],
        json!({"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hello"}]})
    );
    assert_eq!(body["tools"][0]["name"], "Read");
    assert_eq!(body["tools"][0]["strict"], false);
    assert!(body.get("prompt_cache_key").is_some());
    for forbidden in [
        "messages",
        "stream_options",
        "max_completion_tokens",
        "reasoning_effort",
    ] {
        assert!(
            body.get(forbidden).is_none(),
            "{forbidden} is not a Responses field: {body}"
        );
    }
}

#[tokio::test]
async fn responses_stream_becomes_ir_with_reasoning_state_a_composite_call_id_and_usage() {
    let _env = env();
    let mock = MockOpenAi::start(vec![Reply::sse_data(responses_script())]).await;
    let (kinds, error) = collect(
        &responses_adapter(&mock.base_url),
        Protocol::OpenaiResponses,
        &request(hello()),
    )
    .await;
    assert!(error.is_none(), "{error:?}");

    assert!(matches!(
        &kinds[0],
        StreamEventKind::MessageStart { request_id: Some(r), message_id: Some(m), .. }
            if r == "req_mock" && m == "resp_1"
    ));
    let state = kinds
        .iter()
        .find_map(|k| match k {
            StreamEventKind::BlockDelta {
                delta: Delta::Signature { signature },
                ..
            } => crate::services::provider::ir::ProviderState::from_signature(signature),
            _ => None,
        })
        .expect("reasoning replay state");
    assert_eq!(state.protocol, Protocol::OpenaiResponses);
    assert_eq!(state.data["encrypted_content"], "ENC");
    assert!(kinds.contains(&StreamEventKind::BlockStart {
        index: 2,
        block: BlockStart::ToolCall {
            id: super::join_call_id("call_1", "fc_1"),
            name: "Read".into(),
            server: false,
            input: json!({}),
        }
    }));
    let Some(StreamEventKind::MessageDelta { stop_reason, usage }) = kinds.iter().rev().nth(1)
    else {
        panic!("{kinds:?}")
    };
    assert_eq!(*stop_reason, Some(StopReason::ToolUse));
    assert_eq!(
        (
            usage.input_tokens,
            usage.cache_read_tokens,
            usage.output_tokens,
            usage.reasoning_tokens
        ),
        (50, 30, 25, 11)
    );
}

#[tokio::test]
async fn responses_next_turn_sends_the_reasoning_item_back_with_the_paired_call() {
    let _env = env();
    let mock = MockOpenAi::start(vec![Reply::sse_data([created(), completed(json!({}))])]).await;
    let reasoning =
        json!({"type": "reasoning", "id": "rs_1", "summary": [], "encrypted_content": "ENC"});
    let history = vec![
        Message::User {
            parts: vec![UserPart::Text { text: "go".into() }],
        },
        Message::Assistant {
            parts: vec![
                AssistantPart::Thinking {
                    text: "thinking".into(),
                    signature: String::new(),
                    provider_state: Some(crate::services::provider::ir::ProviderState {
                        protocol: Protocol::OpenaiResponses,
                        data: reasoning.clone(),
                    }),
                },
                AssistantPart::ToolCall {
                    id: super::join_call_id("call_1", "fc_1"),
                    name: "Read".into(),
                    input: json!({"path": "a.rs"}),
                    provider_state: None,
                },
            ],
        },
        Message::User {
            parts: vec![UserPart::ToolResult {
                tool_call_id: super::join_call_id("call_1", "fc_1"),
                content: ToolResultContent::Text("data".into()),
                is_error: false,
            }],
        },
    ];
    let (_, error) = collect(
        &responses_adapter(&mock.base_url),
        Protocol::OpenaiResponses,
        &request(history),
    )
    .await;
    assert!(error.is_none());
    let input = &only(&mock).body["input"];
    assert_eq!(input[1], reasoning);
    assert_eq!(input[2]["type"], "function_call");
    assert_eq!(input[2]["call_id"], "call_1");
    assert_eq!(input[2]["id"], "fc_1");
    assert_eq!(
        input[3],
        json!({"type": "function_call_output", "call_id": "call_1", "output": "data"})
    );
}

#[tokio::test]
async fn responses_http_failures_are_classified_with_status_request_id_and_retry_after() {
    let _env = env();
    let cases = [
        (429, ProviderErrorKind::RateLimited),
        (500, ProviderErrorKind::Transient { retryable: true }),
        (502, ProviderErrorKind::Transient { retryable: true }),
        (401, ProviderErrorKind::Unauthorized),
    ];
    for (status, kind) in cases {
        let mock = MockOpenAi::start(vec![Reply::Error {
            status,
            headers: vec![("retry-after-ms".into(), "2500".into())],
            body: json!({"error": {"message": "boom", "type": "x", "code": null}}).to_string(),
        }])
        .await;
        let error = open_error(
            &responses_adapter(&mock.base_url),
            Protocol::OpenaiResponses,
            &request(hello()),
        )
        .await;
        assert_eq!(error.kind, kind, "status {status}");
        assert_eq!(error.status, Some(status));
        assert_eq!(error.request_id.as_deref(), Some("req_mock"));
        assert_eq!(
            error.retry_after,
            Some(std::time::Duration::from_millis(2500))
        );
        assert_eq!(mock.requests().len(), 1, "no automatic retry in this PR");
    }
}

#[tokio::test]
async fn responses_failure_inside_a_200_stream_is_classified_by_its_code() {
    let _env = env();
    let mock = MockOpenAi::start(vec![Reply::sse_data([
        created(),
        json!({"type": "response.failed", "response": {"error": {"code": "rate_limit_exceeded", "message": "slow down"}}}),
    ])])
    .await;
    let (_, error) = collect(
        &responses_adapter(&mock.base_url),
        Protocol::OpenaiResponses,
        &request(hello()),
    )
    .await;
    let error = error.expect("must fail");
    assert_eq!(error.kind, ProviderErrorKind::RateLimited);
    assert_eq!(error.message, "slow down");
}

#[tokio::test]
async fn responses_cut_off_after_output_is_a_partial_stream() {
    let _env = env();
    let events: Vec<_> = responses_script()
        .into_iter()
        .take(6)
        .map(|v| (None, v.to_string()))
        .collect();
    let mock = MockOpenAi::start(vec![Reply::SseThenCut(events)]).await;
    let (_, error) = collect(
        &responses_adapter(&mock.base_url),
        Protocol::OpenaiResponses,
        &request(hello()),
    )
    .await;
    assert_eq!(
        error.expect("must fail").kind,
        ProviderErrorKind::PartialStream
    );
}

// ---------------------------------------------------------------------------
// The ChatGPT (Codex) sign-in
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_chatgpt_sign_in_sends_its_headers_and_none_of_the_refused_fields() {
    let _env = env();
    let mock = MockOpenAi::start(vec![Reply::sse_data([created(), completed(json!({}))])]).await;
    let auth = OpenAiAuth::Codex {
        access: Secret::new("tok-codex"),
        account_id: "acct_42".into(),
    };
    let adapter = OpenAiAdapter::responses(target(
        &mock.base_url,
        ProviderProtocol::OpenaiResponses,
        auth,
    ));
    let mut r = request(hello());
    r.temperature = Some(0.5);
    let (_, error) = collect(&adapter, Protocol::OpenaiResponses, &r).await;
    assert!(error.is_none(), "{error:?}");

    let sent = only(&mock);
    assert_eq!(sent.header("authorization"), Some("Bearer tok-codex"));
    assert_eq!(sent.header("chatgpt-account-id"), Some("acct_42"));
    assert_eq!(sent.header("openai-beta"), Some("responses=experimental"));
    assert_eq!(sent.header("originator"), Some("nyan"));
    assert_eq!(sent.body["store"], false);
    assert!(sent.body.get("max_output_tokens").is_none());
    assert!(sent.body.get("temperature").is_none());
    assert!(sent.body["instructions"].is_string());
}

// ---------------------------------------------------------------------------
// Protocol separation
// ---------------------------------------------------------------------------

#[test]
fn an_adapter_refuses_a_profile_of_the_other_protocol_so_chat_and_responses_never_mix() {
    let chat = chat_adapter("http://127.0.0.1:1");
    let responses = responses_adapter("http://127.0.0.1:1");
    let r = request(hello());
    assert!(chat.validate(&resolved(Protocol::OpenaiChat), &r).is_ok());
    assert!(
        responses
            .validate(&resolved(Protocol::OpenaiResponses), &r)
            .is_ok()
    );
    assert!(
        chat.validate(&resolved(Protocol::OpenaiResponses), &r)
            .is_err()
    );
    assert!(
        responses
            .validate(&resolved(Protocol::OpenaiChat), &r)
            .is_err()
    );
    assert!(
        chat.validate(&resolved(Protocol::AnthropicMessages), &r)
            .is_err()
    );
}

#[test]
fn validate_rejects_what_the_profile_does_not_declare() {
    let adapter = chat_adapter("http://127.0.0.1:1");
    let mut no_images = resolved(Protocol::OpenaiChat);
    no_images.capabilities.images = false;
    let with_image = request(vec![Message::User {
        parts: vec![UserPart::Image {
            source: crate::services::provider::ir::MediaSource::Base64 {
                media_type: "image/png".into(),
                data: "AAA".into(),
            },
            provider_state: None,
        }],
    }]);
    let error = adapter.validate(&no_images, &with_image).unwrap_err();
    assert_eq!(
        error.capability,
        crate::services::provider::ir::Capability::Images
    );

    let mut no_schema = resolved(Protocol::OpenaiChat);
    no_schema.capabilities.structured_output = false;
    let mut structured = request(hello());
    structured.structured_output = Some(crate::services::provider::ir::JsonSchemaOutput {
        schema: json!({"type": "object"}),
    });
    assert_eq!(
        adapter
            .validate(&no_schema, &structured)
            .unwrap_err()
            .capability,
        crate::services::provider::ir::Capability::StructuredOutput
    );
    // Tools and thinking are left out, not refused.
    let mut bare = resolved(Protocol::OpenaiChat);
    bare.capabilities = ResolvedCapabilities::default();
    assert!(adapter.validate(&bare, &request(hello())).is_ok());
}

#[tokio::test]
async fn aborting_before_the_request_is_sent_sends_nothing() {
    let _env = env();
    let mock = MockOpenAi::start(vec![Reply::sse_data([created(), completed(json!({}))])]).await;
    let (handle, signal) = anthropic_sdk::AbortSignal::pair();
    handle.abort();
    let adapter = chat_adapter(&mock.base_url);
    let error = adapter
        .send(&resolved(Protocol::OpenaiChat), &request(hello()), &signal)
        .await
        .err()
        .expect("aborted");
    assert_eq!(error.kind, ProviderErrorKind::Aborted);
    assert!(mock.requests().is_empty());
}

#[tokio::test]
async fn chat_stream_ingests_reasoning_content_delta_over_wire() {
    let _env = env();
    let mock = MockOpenAi::start(vec![Reply::sse_data([
        chat_chunk(
            json!({"role": "assistant", "reasoning_content": "calculating"}),
            Value::Null,
        ),
        chat_chunk(json!({"reasoning_content": " result"}), Value::Null),
        chat_chunk(json!({"content": "done"}), Value::Null),
        chat_chunk(json!({}), json!("stop")),
        json!({"id": "chatcmpl-think", "choices": [], "usage": {
            "prompt_tokens": 80, "completion_tokens": 20, "total_tokens": 100,
            "completion_tokens_details": {"reasoning_tokens": 15}}}),
    ])])
    .await;
    let adapter = chat_adapter(&mock.base_url);
    let (kinds, error) = collect(&adapter, Protocol::OpenaiChat, &request(hello())).await;
    assert!(error.is_none(), "{error:?}");

    assert_eq!(
        kinds[1],
        StreamEventKind::BlockStart {
            index: 0,
            block: BlockStart::Thinking,
        }
    );
    assert_eq!(
        kinds[2],
        StreamEventKind::BlockDelta {
            index: 0,
            delta: Delta::Signature {
                signature: "reasoning_content".into(),
            },
        }
    );
    assert_eq!(
        kinds[3],
        StreamEventKind::BlockDelta {
            index: 0,
            delta: Delta::Thinking {
                text: "calculating".into(),
            },
        }
    );
    assert_eq!(
        kinds[4],
        StreamEventKind::BlockDelta {
            index: 0,
            delta: Delta::Thinking {
                text: " result".into(),
            },
        }
    );
    assert_eq!(kinds[5], StreamEventKind::BlockStop { index: 0 });
    assert_eq!(
        kinds[6],
        StreamEventKind::BlockStart {
            index: 1,
            block: BlockStart::Text,
        }
    );
    assert_eq!(
        kinds[7],
        StreamEventKind::BlockDelta {
            index: 1,
            delta: Delta::Text {
                text: "done".into(),
            },
        }
    );
    assert_eq!(kinds[8], StreamEventKind::BlockStop { index: 1 });
}

#[tokio::test]
async fn chat_multi_turn_replays_reasoning_content_over_wire() {
    let _env = env();
    let mock = MockOpenAi::start(vec![Reply::sse_data([chat_chunk(
        json!({"content": "hi again"}),
        json!("stop"),
    )])])
    .await;
    let adapter = chat_adapter(&mock.base_url);
    let messages = vec![
        Message::User {
            parts: vec![UserPart::Text {
                text: "solve this".into(),
            }],
        },
        Message::Assistant {
            parts: vec![
                AssistantPart::Thinking {
                    text: "step by step thoughts".into(),
                    signature: "reasoning_content".into(),
                    provider_state: None,
                },
                AssistantPart::Text {
                    text: "answer 42".into(),
                },
            ],
        },
        Message::User {
            parts: vec![UserPart::Text {
                text: "are you sure?".into(),
            }],
        },
    ];
    let (kinds, error) = collect(&adapter, Protocol::OpenaiChat, &request(messages)).await;
    assert!(error.is_none(), "{error:?}");
    assert!(!kinds.is_empty());

    let sent = only(&mock);
    let sent_msgs = sent.body["messages"].as_array().unwrap();
    assert_eq!(sent_msgs[2]["role"], "assistant");
    assert_eq!(sent_msgs[2]["content"], "answer 42");
    assert_eq!(sent_msgs[2]["reasoning_content"], "step by step thoughts");
}

#[tokio::test]
async fn chat_openrouter_anthropic_cache_control_over_wire() {
    let _env = env();
    let mock = MockOpenAi::start(vec![Reply::sse_data([chat_chunk(
        json!({"content": "cached response"}),
        json!("stop"),
    )])])
    .await;
    let mut target = target(&mock.base_url, ProviderProtocol::OpenaiChat, key());
    target.compat.cache_control_format = Some("anthropic".into());
    target.cache_retention = crate::utils::settings::nyan::CacheRetention::Short;
    let adapter = OpenAiAdapter::chat(target);

    let messages = vec![
        Message::User {
            parts: vec![UserPart::Text {
                text: "first".into(),
            }],
        },
        Message::Assistant {
            parts: vec![AssistantPart::Text {
                text: "second".into(),
            }],
        },
        Message::User {
            parts: vec![UserPart::Text {
                text: "third".into(),
            }],
        },
    ];
    let (_, error) = collect(&adapter, Protocol::OpenaiChat, &request(messages)).await;
    assert!(error.is_none(), "{error:?}");

    let sent = only(&mock);
    let msgs = sent.body["messages"].as_array().unwrap();
    // System prompt wrapped in text parts with cache_control:
    assert_eq!(msgs[0]["role"], "system");
    let sys_parts = msgs[0]["content"].as_array().unwrap();
    assert_eq!(sys_parts[0]["cache_control"]["type"], "ephemeral");

    // Last tool in tools has cache_control:
    let tools = sent.body["tools"].as_array().unwrap();
    let last_tool = tools.last().unwrap();
    assert_eq!(last_tool["cache_control"]["type"], "ephemeral");

    // Last message has cache_control:
    let last_msg = msgs.last().unwrap();
    assert_eq!(last_msg["role"], "user");
    let last_msg_parts = last_msg["content"].as_array().unwrap();
    assert_eq!(last_msg_parts[0]["cache_control"]["type"], "ephemeral");
}

#[tokio::test]
async fn chat_stream_parses_openrouter_cache_write_and_read_usage() {
    let _env = env();
    let mock = MockOpenAi::start(vec![Reply::sse_data([
        chat_chunk(json!({"content": "hi"}), json!("stop")),
        json!({"id": "chatcmpl-or", "choices": [], "usage": {
            "prompt_tokens": 500, "completion_tokens": 50, "total_tokens": 550,
            "prompt_tokens_details": {
                "cached_tokens": 300,
                "cache_write_tokens": 100
            }
        }}),
    ])])
    .await;
    let adapter = chat_adapter(&mock.base_url);
    let (kinds, error) = collect(&adapter, Protocol::OpenaiChat, &request(hello())).await;
    assert!(error.is_none(), "{error:?}");

    let Some(StreamEventKind::MessageDelta { usage, .. }) = kinds.iter().rev().nth(1) else {
        panic!("{kinds:?}");
    };
    assert_eq!(usage.cache_read_tokens, 300);
    assert_eq!(usage.cache_write_tokens, 100);
    assert_eq!(usage.input_tokens, 100, "500 - 300 - 100");
    assert_eq!(usage.output_tokens, 50);
}

#[tokio::test]
async fn responses_stream_parses_cache_read_and_write_usage() {
    let _env = env();
    let mock = MockOpenAi::start(vec![Reply::sse_data([
        created(),
        json!({"type": "response.completed", "response": {
            "id": "resp_cache",
            "status": "completed",
            "usage": {
                "input_tokens": 400,
                "output_tokens": 30,
                "total_tokens": 430,
                "input_tokens_details": {
                    "cached_tokens": 250,
                    "cache_write_tokens": 50
                },
                "output_tokens_details": {
                    "reasoning_tokens": 15
                }
            }
        }}),
    ])])
    .await;
    let adapter = responses_adapter(&mock.base_url);
    let (kinds, error) = collect(&adapter, Protocol::OpenaiResponses, &request(hello())).await;
    assert!(error.is_none(), "{error:?}");

    let Some(StreamEventKind::MessageDelta { usage, .. }) = kinds.iter().rev().nth(1) else {
        panic!("{kinds:?}");
    };
    assert_eq!(usage.cache_read_tokens, 250);
    assert_eq!(usage.cache_write_tokens, 50);
    assert_eq!(usage.input_tokens, 100, "400 - 250 - 50");
    assert_eq!(usage.output_tokens, 30);
    assert_eq!(usage.reasoning_tokens, 15);
}

#[tokio::test]
async fn chat_and_responses_wire_prompt_cache_retention_when_long() {
    let _env = env();
    let mock = MockOpenAi::start(vec![
        Reply::sse_data([chat_chunk(json!({"content": "chat"}), json!("stop"))]),
        Reply::sse_data([created(), completed(json!({}))]),
    ])
    .await;

    // 1. Chat with Long retention
    let mut chat_t = target(&mock.base_url, ProviderProtocol::OpenaiChat, key());
    chat_t.cache_retention = crate::utils::settings::nyan::CacheRetention::Long;
    let chat_ad = OpenAiAdapter::chat(chat_t);
    let mut res_target_long = resolved(Protocol::OpenaiChat);
    res_target_long.cache_retention = crate::utils::settings::nyan::CacheRetention::Long;

    let cancel = anthropic_sdk::AbortSignal::new();
    let opened = chat_ad
        .send(&res_target_long, &request(hello()), &cancel)
        .await
        .unwrap();
    let mut events = opened.events;
    while let Some(item) = events.next().await {
        if item.is_err() {
            break;
        }
    }

    let reqs = mock.requests();
    assert_eq!(reqs[0].body["prompt_cache_retention"], "24h");
    let cache_key = reqs[0].body["prompt_cache_key"].as_str().unwrap();
    assert!(!cache_key.is_empty() && cache_key.len() <= super::OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH);

    // 2. Responses with Long retention
    let mut resp_t = target(&mock.base_url, ProviderProtocol::OpenaiResponses, key());
    resp_t.cache_retention = crate::utils::settings::nyan::CacheRetention::Long;
    let resp_ad = OpenAiAdapter::responses(resp_t);
    let mut resp_target_long = resolved(Protocol::OpenaiResponses);
    resp_target_long.cache_retention = crate::utils::settings::nyan::CacheRetention::Long;

    let cancel2 = anthropic_sdk::AbortSignal::new();
    let opened2 = resp_ad
        .send(&resp_target_long, &request(hello()), &cancel2)
        .await
        .unwrap();
    let mut events2 = opened2.events;
    while let Some(item) = events2.next().await {
        if item.is_err() {
            break;
        }
    }

    let reqs = mock.requests();
    assert_eq!(reqs[1].body["prompt_cache_retention"], "24h");
    assert_eq!(
        reqs[1].body["prompt_cache_key"],
        reqs[0].body["prompt_cache_key"]
    );
}
