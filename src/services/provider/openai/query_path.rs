//! The OpenAI profiles through the real query entry point.
//!
//! @nyan: `fixtures.rs` proves the adapters; this proves the wiring above them.
//! `query_model_with_streaming` is the function the REPL, sub-agents and hooks
//! all call, so a profile that works here works everywhere that goes through it.
//! Each test installs an OpenAI main profile aimed at a local mock and reads the
//! transcript items the query layer produces.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::mock::{MockOpenAi, Reply};
use crate::services::api::claude::{Options, QueryModelStreamItem, query_model_with_streaming};
use crate::types::ids::ToolUseId;
use crate::types::message::{
    AssistantContent, AssistantMessage, Message, ToolResult, ToolUseBlock, UserContent,
};
use crate::types::tools::Tool;
use crate::utils::settings::SettingsJson;
use crate::utils::settings::nyan::{
    CredentialReference, ModelCapabilities, ModelProfileSettings, NyanSettings,
    OpenaiCompatSettings, ProviderKind, ProviderProtocol, ProviderSettings, SelectionSettings,
};
use crate::utils::settings::settings_cache::set_session_settings_cache;
use crate::utils::settings::validation::SettingsWithErrors;
use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK, TestEnvGuard};
use crate::utils::thinking::ThinkingConfig;

const KEY_VAR: &str = "NYAN_QUERY_PATH_OPENAI_KEY";

struct Env {
    _lock: TestEnvGuard<'static>,
    _guards: Vec<EnvVarGuard>,
    root: std::path::PathBuf,
}

impl Env {
    fn new(base_url: &str, protocol: ProviderProtocol) -> Self {
        Self::with(Vec::new, || {
            super::install_test_main_profile(
                base_url,
                KEY_VAR,
                protocol,
                ModelCapabilities {
                    context_window: Some(400_000),
                    max_output_tokens: Some(128_000),
                    tools: Some("native".into()),
                    thinking: Some("adaptive".into()),
                    images: Some("input".into()),
                    structured_output: Some(true),
                    streaming: Some(true),
                    ..Default::default()
                },
            )
        })
    }

    /// Environment and settings for one test. `extra` and `install` run under
    /// the env lock, so no variable is touched outside it.
    fn with(extra: impl FnOnce() -> Vec<EnvVarGuard>, install: impl FnOnce()) -> Self {
        let lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = std::env::temp_dir().join(format!("nyan-openai-query-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let mut guards = extra();
        guards.extend([
            EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root),
            EnvVarGuard::set("CLAUDE_CODE_SIMPLE", "1"),
            EnvVarGuard::set(KEY_VAR, "sk-query-path"),
            EnvVarGuard::set("API_TIMEOUT_MS", "5000"),
            EnvVarGuard::set("NO_PROXY", "*"),
            EnvVarGuard::set("no_proxy", "*"),
            // A stray Anthropic key must change nothing for an OpenAI profile.
            EnvVarGuard::set("ANTHROPIC_API_KEY", "sk-ant-must-not-be-used"),
            EnvVarGuard::unset("CLAUDE_CODE_USE_BEDROCK"),
            EnvVarGuard::unset("CLAUDE_CODE_USE_VERTEX"),
            EnvVarGuard::unset("CLAUDE_CODE_USE_FOUNDRY"),
            EnvVarGuard::unset("HTTPS_PROXY"),
            EnvVarGuard::unset("HTTP_PROXY"),
        ]);
        crate::utils::settings::settings_cache::reset_settings_cache();
        install();
        Self {
            _lock: lock,
            _guards: guards,
            root,
        }
    }
}

/// The variable an optional-credential local profile reads.
const LOCAL_KEY_VAR: &str = "NYAN_QUERY_PATH_LOCAL_KEY";

/// A vLLM or Ollama style profile on `openaiChat`. `key` sets the optional
/// credential; `None` leaves it unset, as a keyless local server is run.
fn local_env(
    base_url: &str,
    kind: ProviderKind,
    compat: Option<OpenaiCompatSettings>,
    key: Option<&str>,
) -> Env {
    let base_url = base_url.to_string();
    let key = key.map(str::to_string);
    Env::with(
        move || {
            vec![match key {
                Some(value) => EnvVarGuard::set(LOCAL_KEY_VAR, &value),
                None => EnvVarGuard::unset(LOCAL_KEY_VAR),
            }]
        },
        move || {
            let nyan = NyanSettings {
                providers: Some(BTreeMap::from([(
                    "local".to_string(),
                    ProviderSettings {
                        kind: Some(kind),
                        protocol: Some(ProviderProtocol::OpenaiChat),
                        base_url: Some(base_url),
                        credential: Some(CredentialReference {
                            optional_env: Some(LOCAL_KEY_VAR.into()),
                            ..Default::default()
                        }),
                        compat,
                        ..Default::default()
                    },
                )])),
                models: Some(BTreeMap::from([(
                    "main".to_string(),
                    ModelProfileSettings {
                        provider: Some("local".to_string()),
                        model: Some("qwen-local".to_string()),
                        capabilities: Some(ModelCapabilities {
                            context_window: Some(32_768),
                            max_output_tokens: Some(4_096),
                            tools: Some("native".into()),
                            streaming: Some(true),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                )])),
                selection: Some(SelectionSettings {
                    main: Some("main".to_string()),
                    ..Default::default()
                }),
                ..Default::default()
            };
            set_session_settings_cache(SettingsWithErrors {
                settings: SettingsJson {
                    nyan: Some(Box::new(nyan)),
                    ..Default::default()
                },
                ..Default::default()
            });
        },
    )
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

fn probe_tool() -> Tool {
    Tool {
        name: "Probe".to_string(),
        description: "probe tool".to_string(),
        input_schema: json!({"type": "object", "properties": {"a": {"type": "number"}}}),
        ..Default::default()
    }
}

async fn run(messages: Vec<Message>, tools: Vec<Tool>) -> Vec<QueryModelStreamItem> {
    let options = Options::new("gpt-5".to_string(), "repl_main_thread".to_string());
    let mut rx = query_model_with_streaming(
        &messages,
        &vec!["You are a test.".to_string()],
        &ThinkingConfig::Adaptive,
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
    items
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

fn first(message: &AssistantMessage) -> AssistantContent {
    message.first_content_block().cloned().unwrap()
}

fn chat_chunk(delta: Value, finish: Value) -> Value {
    json!({"id": "chatcmpl-1", "model": "gpt-5", "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
}

#[tokio::test]
async fn a_chat_profile_answers_through_the_query_layer_with_text_a_tool_call_and_usage() {
    let mock = MockOpenAi::start(vec![Reply::sse_data([
        chat_chunk(json!({"content": "Let me look."}), Value::Null),
        chat_chunk(
            json!({"tool_calls": [{"index": 0, "id": "call_a", "type": "function",
                "function": {"name": "Probe", "arguments": "{\"a\":"}}]}),
            Value::Null,
        ),
        chat_chunk(
            json!({"tool_calls": [{"index": 0, "function": {"arguments": "42}"}}]}),
            Value::Null,
        ),
        chat_chunk(json!({}), json!("tool_calls")),
        json!({"id": "chatcmpl-1", "choices": [], "usage": {
            "prompt_tokens": 60, "completion_tokens": 8,
            "prompt_tokens_details": {"cached_tokens": 20}}}),
    ])])
    .await;
    let _env = Env::new(&mock.base_url, ProviderProtocol::OpenaiChat);

    let items = run(vec![user("go")], vec![probe_tool()]).await;
    let messages = assistants(&items);
    assert_eq!(messages.len(), 2, "one assistant per block: {items:?}");
    assert_eq!(
        first(messages[0]),
        AssistantContent::Text("Let me look.".into())
    );
    assert_eq!(
        first(messages[1]),
        AssistantContent::ToolUse(ToolUseBlock {
            id: ToolUseId("call_a".into()),
            name: "Probe".into(),
            input: json!({"a": 42}),
        })
    );
    // The header request id reaches the transcript identity.
    assert_eq!(messages[1].request_id(), Some("req_mock"));

    let usage = items
        .iter()
        .rev()
        .find_map(|item| match item {
            QueryModelStreamItem::AssistantDelta { stop_reason, usage } => {
                Some((stop_reason.clone(), usage.clone()))
            }
            _ => None,
        })
        .expect("final usage item");
    assert_eq!(usage.0, Some(crate::types::message::StopReason::ToolUse));
    let usage = usage.1.unwrap();
    assert_eq!(
        (
            usage.input_tokens,
            usage.cache_read_input_tokens,
            usage.output_tokens
        ),
        (40, 20, 8)
    );

    let requests = mock.requests();
    assert_eq!(
        requests.len(),
        1,
        "exactly one request, and it went to the OpenAI endpoint"
    );
    assert!(
        requests[0]
            .request_line
            .starts_with("POST /chat/completions")
    );
    assert_eq!(
        requests[0].header("authorization"),
        Some("Bearer sk-query-path")
    );
    assert_eq!(
        requests[0].header("x-api-key"),
        None,
        "the Anthropic key is not used"
    );
    assert_eq!(requests[0].body["tools"][0]["function"]["name"], "Probe");
    let body = requests[0].body.to_string();
    for anthropic_only in ["cache_control", "anthropic", "x-anthropic-billing-header"] {
        assert!(
            !body.contains(anthropic_only),
            "{anthropic_only} leaked: {body}"
        );
    }
}

#[tokio::test]
async fn a_responses_profile_keeps_reasoning_across_a_tool_round_trip() {
    let reasoning =
        json!({"type": "reasoning", "id": "rs_1", "summary": [], "encrypted_content": "ENC"});
    let turn_one = vec![
        json!({"type": "response.created", "response": {"id": "resp_1", "model": "gpt-5"}}),
        json!({"type": "response.output_item.added", "output_index": 0, "item": {"type": "reasoning", "id": "rs_1", "summary": []}}),
        json!({"type": "response.reasoning_summary_text.delta", "output_index": 0, "delta": "plan"}),
        json!({"type": "response.output_item.done", "output_index": 0, "item": reasoning}),
        json!({"type": "response.output_item.added", "output_index": 1, "item": {"type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "Probe", "arguments": ""}}),
        json!({"type": "response.function_call_arguments.delta", "output_index": 1, "delta": "{\"a\":1}"}),
        json!({"type": "response.output_item.done", "output_index": 1, "item": {"type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "Probe", "arguments": "{\"a\":1}"}}),
        json!({"type": "response.completed", "response": {"id": "resp_1", "status": "completed",
            "usage": {"input_tokens": 10, "output_tokens": 4}}}),
    ];
    let turn_two = vec![
        json!({"type": "response.created", "response": {"id": "resp_2", "model": "gpt-5"}}),
        json!({"type": "response.output_item.added", "output_index": 0, "item": {"type": "message", "content": []}}),
        json!({"type": "response.output_text.delta", "output_index": 0, "delta": "done"}),
        json!({"type": "response.output_item.done", "output_index": 0, "item": {"type": "message", "content": [{"type": "output_text", "text": "done"}]}}),
        json!({"type": "response.completed", "response": {"id": "resp_2", "status": "completed",
            "usage": {"input_tokens": 20, "output_tokens": 2}}}),
    ];
    let mock = MockOpenAi::start(vec![Reply::sse_data(turn_one), Reply::sse_data(turn_two)]).await;
    let _env = Env::new(&mock.base_url, ProviderProtocol::OpenaiResponses);

    let first_items = run(vec![user("go")], vec![probe_tool()]).await;
    let produced: Vec<Message> = assistants(&first_items)
        .into_iter()
        .cloned()
        .map(Message::Assistant)
        .collect();
    assert_eq!(
        produced.len(),
        2,
        "reasoning block and the call: {first_items:?}"
    );
    let call = produced
        .iter()
        .find_map(|m| match m {
            Message::Assistant(a) => match a.first_content_block() {
                Some(AssistantContent::ToolUse(block)) => Some(block.clone()),
                _ => None,
            },
            _ => None,
        })
        .expect("tool call");
    assert!(call.id.0.starts_with("call_1"), "{}", call.id.0);

    // The transcript is what the next request is built from.
    let mut history = vec![user("go")];
    history.extend(produced);
    let mut result = crate::utils::messages::create_user_message(String::new());
    result.content = vec![UserContent::ToolResult(ToolResult {
        tool_use_id: call.id.clone(),
        content: "probed".into(),
        is_error: false,
        content_blocks: Vec::new(),
        tool_use_result: None,
    })];
    history.push(Message::User(result));
    let second_items = run(history, vec![probe_tool()]).await;
    assert_eq!(
        first(assistants(&second_items)[0]),
        AssistantContent::Text("done".into())
    );

    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[1].request_line.starts_with("POST /responses"));
    let input = requests[1].body["input"].as_array().unwrap();
    let kinds: Vec<_> = input
        .iter()
        .map(|i| i["type"].as_str().unwrap_or("?"))
        .collect();
    assert_eq!(
        kinds,
        [
            "message",
            "reasoning",
            "function_call",
            "function_call_output"
        ],
        "{input:?}"
    );
    assert_eq!(
        input[1], reasoning,
        "the reasoning item is sent back untouched"
    );
    assert_eq!(input[2]["call_id"], "call_1");
    assert_eq!(input[2]["id"], "fc_1");
    assert_eq!(input[3]["call_id"], "call_1");
    assert_eq!(input[3]["output"], "probed");
}

#[tokio::test]
async fn a_billing_429_surfaces_once_with_no_retry_and_no_anthropic_fallback() {
    let mock = MockOpenAi::start(vec![
        Reply::error(
            429,
            json!({"error": {"message": "quota exceeded", "type": "insufficient_quota", "code": "insufficient_quota"}}),
        ),
        // A second reply nobody should ask for.
        Reply::sse_data([chat_chunk(json!({"content": "unexpected"}), json!("stop"))]),
    ])
    .await;
    let _env = Env::new(&mock.base_url, ProviderProtocol::OpenaiChat);

    let options = Options::new("gpt-5".to_string(), "repl_main_thread".to_string());
    let result = query_model_with_streaming(
        &[user("go")],
        &vec![],
        &ThinkingConfig::Disabled,
        &[],
        &options,
    )
    .await;
    let error = match result {
        Err(error) => error.to_string(),
        Ok(mut rx) => {
            let mut text = String::new();
            while let Some(item) = rx.recv().await {
                if let QueryModelStreamItem::SystemError(error) = item {
                    text = error.content;
                }
            }
            text
        }
    };
    assert!(error.contains("quota exceeded"), "{error}");
    assert_eq!(
        mock.requests().len(),
        1,
        "billing 429 is not retried and does not fallback to Anthropic"
    );
}

#[tokio::test]
async fn a_burst_429_retries_under_the_unified_budget_and_recovers() {
    let mock = MockOpenAi::start(vec![
        Reply::error(
            429,
            json!({"error": {"message": "slow down", "type": "rate_limit_error", "code": "rate_limit_exceeded"}}),
        ),
        Reply::sse_data([chat_chunk(json!({"content": "recovered"}), json!("stop"))]),
    ])
    .await;
    let _env = Env::new(&mock.base_url, ProviderProtocol::OpenaiChat);

    let items = run(vec![user("go")], vec![]).await;
    let messages = assistants(&items);
    assert_eq!(messages.len(), 1);
    assert_eq!(
        first(messages[0]),
        AssistantContent::Text("recovered".into())
    );
    assert_eq!(
        mock.requests().len(),
        2,
        "the burst 429 was retried once and recovered"
    );
}

#[tokio::test]
async fn a_503_service_unavailable_retries_under_the_unified_budget_and_recovers() {
    let mock = MockOpenAi::start(vec![
        Reply::error(
            503,
            json!({"error": {"message": "service unavailable", "type": "server_error"}}),
        ),
        Reply::sse_data([chat_chunk(
            json!({"content": "recovered 503"}),
            json!("stop"),
        )]),
    ])
    .await;
    let _env = Env::new(&mock.base_url, ProviderProtocol::OpenaiChat);

    let items = run(vec![user("go")], vec![]).await;
    let messages = assistants(&items);
    assert_eq!(messages.len(), 1);
    assert_eq!(
        first(messages[0]),
        AssistantContent::Text("recovered 503".into())
    );
    assert_eq!(
        mock.requests().len(),
        2,
        "the 503 was retried once and recovered"
    );
}

#[tokio::test]
async fn an_eligible_failure_switches_to_configured_main_fallback() {
    let primary_mock = MockOpenAi::start(vec![Reply::error(
        429,
        json!({"error": {"message": "quota exceeded", "type": "insufficient_quota", "code": "insufficient_quota"}}),
    )])
    .await;
    let fallback_mock = MockOpenAi::start(vec![Reply::sse_data([chat_chunk(
        json!({"content": "served by fallback"}),
        json!("stop"),
    )])])
    .await;

    let _env = Env::with(Vec::new, || {
        use crate::utils::settings::nyan::{
            CredentialReference, ModelCapabilities, ModelProfileSettings, NyanSettings,
            ProviderKind, ProviderSettings, SelectionSettings,
        };
        use crate::utils::settings::settings_cache::set_session_settings_cache;
        use crate::utils::settings::validation::SettingsWithErrors;
        use std::collections::BTreeMap;

        let nyan = NyanSettings {
            providers: Some(BTreeMap::from([
                (
                    "primary-provider".to_string(),
                    ProviderSettings {
                        kind: Some(ProviderKind::Openai),
                        protocols: Some(vec![ProviderProtocol::OpenaiChat]),
                        base_url: Some(primary_mock.base_url.clone()),
                        credential: Some(CredentialReference {
                            env: Some(KEY_VAR.to_string()),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                ),
                (
                    "fallback-provider".to_string(),
                    ProviderSettings {
                        kind: Some(ProviderKind::Openai),
                        protocols: Some(vec![ProviderProtocol::OpenaiChat]),
                        base_url: Some(fallback_mock.base_url.clone()),
                        credential: Some(CredentialReference {
                            env: Some(KEY_VAR.to_string()),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                ),
            ])),
            models: Some(BTreeMap::from([
                (
                    "main".to_string(),
                    ModelProfileSettings {
                        provider: Some("primary-provider".to_string()),
                        model: Some("gpt-5".to_string()),
                        protocol: Some(ProviderProtocol::OpenaiChat),
                        capabilities: Some(ModelCapabilities {
                            context_window: Some(400_000),
                            max_output_tokens: Some(128_000),
                            streaming: Some(true),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                ),
                (
                    "fb".to_string(),
                    ModelProfileSettings {
                        provider: Some("fallback-provider".to_string()),
                        model: Some("gpt-5-fallback".to_string()),
                        protocol: Some(ProviderProtocol::OpenaiChat),
                        capabilities: Some(ModelCapabilities {
                            context_window: Some(400_000),
                            max_output_tokens: Some(128_000),
                            streaming: Some(true),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                ),
            ])),
            selection: Some(SelectionSettings {
                main: Some("main".to_string()),
                main_fallbacks: Some(vec!["fb".to_string()]),
                ..Default::default()
            }),
            ..Default::default()
        };
        set_session_settings_cache(SettingsWithErrors {
            settings: SettingsJson {
                nyan: Some(Box::new(nyan)),
                ..Default::default()
            },
            ..Default::default()
        });
    });

    let options = Options::new("gpt-5".to_string(), "repl_main_thread".to_string());
    let mut rx = query_model_with_streaming(
        &[user("go")],
        &vec![],
        &ThinkingConfig::Disabled,
        &[],
        &options,
    )
    .await
    .expect("stream opens on fallback");
    let mut items = Vec::new();
    while let Some(item) = tokio::time::timeout(std::time::Duration::from_secs(10), rx.recv())
        .await
        .expect("stream item before deadline")
    {
        items.push(item);
    }
    // A system warning notice was emitted for the switch.
    let switched_warning = items.iter().find(|item| {
        matches!(item, QueryModelStreamItem::SystemApiError(msg) if msg.content().is_some_and(|c| c.contains("Switched from profile \"main\" to fallback profile \"fb\"")))
    });
    assert!(
        switched_warning.is_some(),
        "expected switch notice: {items:?}"
    );

    let messages = assistants(&items);
    assert_eq!(messages.len(), 1);
    assert_eq!(
        first(messages[0]),
        AssistantContent::Text("served by fallback".into())
    );
    assert_eq!(primary_mock.requests().len(), 1);
    assert_eq!(fallback_mock.requests().len(), 1);
}

/// Everything the query layer reports for one call: the error from opening the
/// stream, or the terminal error items from reading it.
async fn failure_of(messages: Vec<Message>) -> (String, Vec<QueryModelStreamItem>) {
    let options = Options::new("gpt-5".to_string(), "repl_main_thread".to_string());
    match query_model_with_streaming(&messages, &vec![], &ThinkingConfig::Disabled, &[], &options)
        .await
    {
        Err(error) => (error.to_string(), Vec::new()),
        Ok(mut rx) => {
            let mut items = Vec::new();
            while let Some(item) =
                tokio::time::timeout(std::time::Duration::from_secs(10), rx.recv())
                    .await
                    .expect("stream item before deadline")
            {
                items.push(item);
            }
            let text = items
                .iter()
                .find_map(|item| match item {
                    QueryModelStreamItem::SystemError(error) => Some(error.content.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            (text, items)
        }
    }
}

#[tokio::test]
async fn a_404_at_open_falls_back_to_non_streaming_on_openai_endpoint() {
    let mock = MockOpenAi::start(vec![
        Reply::error(
            404,
            json!({"error": {"message": "Streaming endpoint not found", "type": "invalid_request_error", "code": "endpoint_not_found"}}),
        ),
        Reply::json(json!({
            "id": "chatcmpl-fallback",
            "model": "gpt-5",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "served by non-streaming fallback"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5}
        })),
    ])
    .await;
    let _env = Env::new(&mock.base_url, ProviderProtocol::OpenaiChat);
    crate::cost_tracker::reset_cost_state();

    let items = run(vec![user("go")], vec![]).await;
    assert!(
        items
            .iter()
            .any(|i| matches!(i, QueryModelStreamItem::StreamingFallback)),
        "expected StreamingFallback signal: {items:?}"
    );
    // The failed stream produced no usage; the fallback is accounted once.
    let total = crate::cost_tracker::get_total_usage();
    assert_eq!(
        (total.requests, total.input_tokens, total.output_tokens),
        (1, 10, 5)
    );
    crate::cost_tracker::reset_cost_state();
    let messages = assistants(&items);
    assert_eq!(messages.len(), 1);
    assert_eq!(
        first(messages[0]),
        AssistantContent::Text("served by non-streaming fallback".into())
    );

    let requests = mock.requests();
    assert_eq!(
        requests.len(),
        2,
        "1 streaming attempt + 1 non-streaming fallback attempt"
    );
    assert_eq!(requests[0].body["stream"], true);
    assert_eq!(requests[1].body["stream"], false);
    assert_eq!(
        requests[1].header("authorization"),
        Some("Bearer sk-query-path")
    );
    assert_eq!(
        requests[1].header("x-api-key"),
        None,
        "no Anthropic key used on fallback"
    );
}

#[tokio::test]
async fn a_cut_stream_after_output_is_reported_and_never_replayed() {
    let events: Vec<_> = [
        chat_chunk(
            json!({"role": "assistant", "content": "half "}),
            Value::Null,
        ),
        chat_chunk(
            json!({"tool_calls": [{"index": 0, "id": "call_a", "type": "function",
                "function": {"name": "Probe", "arguments": "{\"a\":"}}]}),
            Value::Null,
        ),
    ]
    .into_iter()
    .map(|v| (None, v.to_string()))
    .collect();
    let mock = MockOpenAi::start(vec![
        Reply::SseThenCut(events),
        Reply::sse_data([chat_chunk(json!({"content": "replayed"}), json!("stop"))]),
    ])
    .await;
    let _env = Env::new(&mock.base_url, ProviderProtocol::OpenaiChat);

    let (message, items) = failure_of(vec![user("go")]).await;
    assert!(!message.is_empty(), "the failure is surfaced: {items:?}");
    assert!(
        !items
            .iter()
            .any(|i| matches!(i, QueryModelStreamItem::StreamingFallback)),
        "no fallback signal: {items:?}"
    );
    assert!(
        !items.iter().any(|i| matches!(
            i,
            QueryModelStreamItem::Assistant(a)
                if matches!(a.first_content_block(), Some(AssistantContent::Text(t)) if t == "replayed")
        )),
        "the request was not replayed: {items:?}"
    );
    assert_eq!(
        mock.requests().len(),
        1,
        "a half-finished request is never sent twice"
    );
}

#[tokio::test]
async fn a_chat_profile_serves_side_query_through_request_ir_and_openai_endpoint() {
    let mock = MockOpenAi::start(vec![Reply::json(json!({
        "id": "chatcmpl-sq",
        "model": "gpt-5",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": "validated"},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 5, "completion_tokens": 1}
    }))])
    .await;
    let _env = Env::new(&mock.base_url, ProviderProtocol::OpenaiChat);

    let response =
        crate::utils::side_query::side_query(crate::utils::side_query::SideQueryOptions {
            model: "gpt-5".to_string(),
            messages: vec![crate::utils::side_query::user_text_message("Hi")],
            max_tokens: Some(1),
            max_retries: Some(0),
            query_source: "model_validation".to_string(),
            ..Default::default()
        })
        .await
        .expect("side_query serves OpenAI Chat profile");

    let requests = mock.requests();
    assert_eq!(requests.len(), 1, "exactly one request sent");
    assert!(
        requests[0]
            .request_line
            .starts_with("POST /chat/completions")
    );
    assert_eq!(
        requests[0].header("authorization"),
        Some("Bearer sk-query-path")
    );
    assert_eq!(requests[0].header("x-api-key"), None, "no Anthropic key");
    assert_eq!(requests[0].body["stream"], false);
    assert_eq!(requests[0].body["model"], "gpt-5");

    assert_eq!(response.id, "chatcmpl-sq");
    let content = crate::utils::messages::extract_text_content(&response.content, None);
    assert_eq!(content, "validated");
}

#[tokio::test]
async fn a_responses_profile_serves_side_query_through_request_ir_and_responses_endpoint() {
    let mock = MockOpenAi::start(vec![Reply::json(json!({
        "id": "resp-sq",
        "model": "gpt-5",
        "status": "completed",
        "output": [{
            "type": "message",
            "content": [{"type": "output_text", "text": "responses side query ok"}]
        }],
        "usage": {"input_tokens": 8, "output_tokens": 4}
    }))])
    .await;
    let _env = Env::new(&mock.base_url, ProviderProtocol::OpenaiResponses);

    let response =
        crate::utils::side_query::side_query(crate::utils::side_query::SideQueryOptions {
            model: "gpt-5".to_string(),
            messages: vec![crate::utils::side_query::user_text_message("Hi")],
            max_tokens: Some(10),
            max_retries: Some(0),
            query_source: "permission_explainer".to_string(),
            ..Default::default()
        })
        .await
        .expect("side_query serves OpenAI Responses profile");

    let requests = mock.requests();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].request_line.starts_with("POST /responses"));
    assert_eq!(requests[0].body["stream"], false);

    assert_eq!(response.id, "resp-sq");
    let content = crate::utils::messages::extract_text_content(&response.content, None);
    assert_eq!(content, "responses side query ok");
}

#[tokio::test]
async fn a_responses_profile_falls_back_to_non_streaming_on_stream_open_404() {
    let mock = MockOpenAi::start(vec![
        Reply::error(
            404,
            json!({"error": {"message": "Streaming not supported", "type": "invalid_request_error"}}),
        ),
        Reply::json(json!({
            "id": "resp-fb",
            "model": "gpt-5",
            "status": "completed",
            "output": [{
                "type": "message",
                "content": [{"type": "output_text", "text": "responses fallback output"}]
            }],
            "usage": {"input_tokens": 12, "output_tokens": 3}
        })),
    ])
    .await;
    let _env = Env::new(&mock.base_url, ProviderProtocol::OpenaiResponses);

    let items = run(vec![user("go")], vec![]).await;
    assert!(
        items
            .iter()
            .any(|i| matches!(i, QueryModelStreamItem::StreamingFallback)),
        "expected StreamingFallback signal: {items:?}"
    );
    let messages = assistants(&items);
    assert_eq!(messages.len(), 1);
    assert_eq!(
        first(messages[0]),
        AssistantContent::Text("responses fallback output".into())
    );

    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].body["stream"], true);
    assert_eq!(requests[1].body["stream"], false);
}

#[tokio::test]
async fn count_tokens_on_openai_profile_falls_back_to_local_estimation() {
    let mock = MockOpenAi::start(vec![]).await;
    let _env = Env::new(&mock.base_url, ProviderProtocol::OpenaiChat);

    let count = crate::services::token_estimation::count_tokens_with_api("hello world").await;
    assert!(count.is_some());
    assert_eq!(
        count.unwrap(),
        crate::services::token_estimation::rough_token_count_estimation("hello world") as usize
    );
    assert!(
        mock.requests().is_empty(),
        "no network request made for OpenAI count_tokens"
    );
}

#[tokio::test]
async fn a_local_vllm_profile_serves_side_query_without_key() {
    let mock = MockOpenAi::start(vec![Reply::json(json!({
        "id": "cmpl-local-sq",
        "model": "qwen-local",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": "local side query ok"},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 6, "completion_tokens": 3}
    }))])
    .await;
    let _env = local_env(&mock.base_url, ProviderKind::Vllm, None, None);

    let response =
        crate::utils::side_query::side_query(crate::utils::side_query::SideQueryOptions {
            model: "qwen-local".to_string(),
            messages: vec![crate::utils::side_query::user_text_message("Hi local")],
            max_tokens: Some(10),
            max_retries: Some(0),
            query_source: "side_query".to_string(),
            ..Default::default()
        })
        .await
        .expect("side_query serves local vLLM profile");

    let requests = mock.requests();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0]
            .request_line
            .starts_with("POST /chat/completions")
    );
    assert_eq!(
        requests[0].header("authorization"),
        None,
        "no key on keyless local server"
    );
    assert_eq!(requests[0].body["stream"], false);
    assert_eq!(response.id, "cmpl-local-sq");
    let content = crate::utils::messages::extract_text_content(&response.content, None);
    assert_eq!(content, "local side query ok");
}

/// A vLLM-style reply: an SSE stream with a usage chunk, `max_tokens` on the
/// request, and no Anthropic headers.
#[tokio::test]
async fn a_vllm_style_local_profile_streams_without_a_key_and_names_its_cap_field() {
    let mock = MockOpenAi::start(vec![Reply::sse_data([
        chat_chunk(json!({"content": "local hi"}), json!("stop")),
        json!({"id": "cmpl-local", "choices": [], "usage": {
            "prompt_tokens": 12, "completion_tokens": 3}}),
    ])])
    .await;
    let _env = local_env(&mock.base_url, ProviderKind::Vllm, None, None);

    let items = run(vec![user("go")], vec![]).await;
    let messages = assistants(&items);
    assert_eq!(messages.len(), 1, "{items:?}");
    assert_eq!(
        first(messages[0]),
        AssistantContent::Text("local hi".into())
    );

    let requests = mock.requests();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0]
            .request_line
            .starts_with("POST /chat/completions")
    );
    assert_eq!(
        requests[0].header("authorization"),
        None,
        "no credential when the optional variable is unset"
    );
    assert_eq!(requests[0].body["model"], "qwen-local");
    assert!(
        requests[0].body.get("max_tokens").is_some(),
        "vLLM default: max_tokens: {}",
        requests[0].body
    );
    assert!(requests[0].body.get("max_completion_tokens").is_none());
    assert_eq!(requests[0].body["stream_options"]["include_usage"], true);
}

/// An Ollama-style endpoint that does not take `stream_options`: the dialect
/// option turns the usage request off and the body must not carry it.
#[tokio::test]
async fn an_ollama_profile_with_usage_disabled_omits_stream_options_and_keeps_the_cap() {
    let mock = MockOpenAi::start(vec![Reply::sse_data([chat_chunk(
        json!({"content": "ok"}),
        json!("stop"),
    )])])
    .await;
    let compat = OpenaiCompatSettings {
        max_tokens_field: Some(crate::utils::settings::nyan::MaxTokensField::MaxCompletionTokens),
        include_usage: Some(false),
        ..Default::default()
    };
    let _env = local_env(&mock.base_url, ProviderKind::Ollama, Some(compat), None);

    let items = run(vec![user("go")], vec![]).await;
    assert_eq!(assistants(&items).len(), 1, "{items:?}");

    let body = &mock.requests()[0].body;
    assert!(body.get("stream_options").is_none(), "{body}");
    assert!(
        body.get("max_completion_tokens").is_some(),
        "explicit compat wins over the local default: {body}"
    );
    assert!(body.get("max_tokens").is_none(), "{body}");
}

/// Ollama answers a missing credential with 401 when the server was started with
/// one. The error must say which variable to set, and nothing is retried.
#[tokio::test]
async fn an_unset_optional_key_that_the_server_rejects_names_the_variable_and_is_not_retried() {
    let mock = MockOpenAi::start(vec![
        Reply::error(
            401,
            json!({"error": {"message": "invalid api key", "type": "authentication_error", "code": "invalid_api_key"}}),
        ),
        Reply::sse_data([chat_chunk(json!({"content": "unexpected"}), json!("stop"))]),
    ])
    .await;
    let _env = local_env(&mock.base_url, ProviderKind::Vllm, None, None);

    let (message, items) = failure_of(vec![user("go")]).await;
    assert!(message.contains(LOCAL_KEY_VAR), "{message}");
    assert!(message.contains("401"), "{message}");
    assert!(
        !items
            .iter()
            .any(|i| matches!(i, QueryModelStreamItem::StreamingFallback)),
        "{items:?}"
    );
    assert_eq!(mock.requests().len(), 1, "one request, no retry");
}

/// The same 401 with a key set reports the server's own words: the variable is
/// not the problem then.
#[tokio::test]
async fn a_401_with_a_key_set_keeps_the_servers_own_message() {
    let mock = MockOpenAi::start(vec![Reply::error(
        401,
        json!({"error": {"message": "wrong key", "type": "authentication_error", "code": "invalid_api_key"}}),
    )])
    .await;
    let _env = local_env(
        &mock.base_url,
        ProviderKind::Vllm,
        None,
        Some("sk-local-set"),
    );

    let (message, _) = failure_of(vec![user("go")]).await;
    assert!(message.contains("wrong key"), "{message}");
    assert!(!message.contains(LOCAL_KEY_VAR), "{message}");
    assert_eq!(
        mock.requests()[0].header("authorization"),
        Some("Bearer sk-local-set")
    );
}

/// A local server that is not running must fail at the connection, quickly and
/// with the endpoint named, and must not reach a commercial endpoint instead.
#[tokio::test]
async fn a_local_profile_with_no_base_url_is_refused_before_any_request() {
    let mock = MockOpenAi::start(vec![]).await;
    let _env = local_env(&mock.base_url, ProviderKind::Vllm, None, None);

    // Replace the session profile with one that has no baseUrl.
    let nyan: NyanSettings = serde_json::from_value(json!({
        "providers": {"local": {"kind": "vllm", "protocol": "openaiChat"}},
        "models": {"main": {"provider": "local", "model": "qwen-local", "capabilities": {
            "contextWindow": 32768, "maxOutputTokens": 4096}}},
        "selection": {"main": "main"}
    }))
    .unwrap();
    set_session_settings_cache(SettingsWithErrors {
        settings: SettingsJson {
            nyan: Some(Box::new(nyan)),
            ..Default::default()
        },
        ..Default::default()
    });

    let (message, _) = failure_of(vec![user("go")]).await;
    assert!(message.contains("baseUrl"), "{message}");
    assert!(mock.requests().is_empty(), "nothing was sent");
}

#[tokio::test]
async fn a_missing_key_is_reported_as_configuration_not_sent() {
    let mock = MockOpenAi::start(vec![]).await;
    let _env = Env::new(&mock.base_url, ProviderProtocol::OpenaiChat);
    // Dropped before `_env`, so the variable is restored in order.
    let _unset = EnvVarGuard::unset(KEY_VAR);
    let options = Options::new("gpt-5".to_string(), "repl_main_thread".to_string());
    let error = query_model_with_streaming(
        &[user("go")],
        &vec![],
        &ThinkingConfig::Disabled,
        &[],
        &options,
    )
    .await;
    let message = match error {
        Err(error) => error.to_string(),
        Ok(mut rx) => {
            let mut text = String::new();
            while let Some(item) = rx.recv().await {
                if let QueryModelStreamItem::SystemError(error) = item {
                    text = error.content;
                }
            }
            text
        }
    };
    assert!(
        message.contains(KEY_VAR),
        "names the variable to set: {message}"
    );
    assert!(mock.requests().is_empty());
}

/// A thinking-capable profile of `kind` on `protocol` whose `effort` may be
/// unset, aimed at the mock.
fn thinking_env(
    base_url: &str,
    kind: ProviderKind,
    protocol: ProviderProtocol,
    model: &str,
    effort: Option<crate::utils::effort::Effort>,
    compat: Option<OpenaiCompatSettings>,
) -> Env {
    let base_url = base_url.to_string();
    let model = model.to_string();
    Env::with(Vec::new, move || {
        let nyan = NyanSettings {
            providers: Some(BTreeMap::from([(
                "p".to_string(),
                ProviderSettings {
                    kind: Some(kind),
                    protocols: Some(vec![protocol]),
                    base_url: Some(base_url),
                    credential: Some(CredentialReference {
                        env: Some(KEY_VAR.into()),
                        ..Default::default()
                    }),
                    compat,
                    ..Default::default()
                },
            )])),
            models: Some(BTreeMap::from([(
                "main".to_string(),
                ModelProfileSettings {
                    provider: Some("p".to_string()),
                    model: Some(model),
                    protocol: Some(protocol),
                    capabilities: Some(ModelCapabilities {
                        context_window: Some(128_000),
                        max_output_tokens: Some(8_000),
                        thinking: Some("adaptive".into()),
                        streaming: Some(true),
                        ..Default::default()
                    }),
                    effort,
                    ..Default::default()
                },
            )])),
            selection: Some(SelectionSettings {
                main: Some("main".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };
        set_session_settings_cache(SettingsWithErrors {
            settings: SettingsJson {
                nyan: Some(Box::new(nyan)),
                ..Default::default()
            },
            ..Default::default()
        });
    })
}

/// The body the main loop sends: the request is built the way `query` builds
/// it (session context, launch thinking default) and goes out through
/// `query_model_with_streaming`.
async fn main_loop_body(
    mock: &MockOpenAi,
    thinking: ThinkingConfig,
    session_effort: Option<crate::utils::effort::Effort>,
) -> Value {
    let context = crate::tool::ToolUseContext::default()
        .with_thinking_config(Some(thinking))
        .with_effort_value(session_effort);
    let request = crate::query::build_call_model_request(
        vec![user("go")],
        crate::tool::ToolPermissionContext::default(),
        &crate::constants::query_source::QuerySource::Prompt,
        &["You are a test.".to_string()],
        &BTreeMap::new(),
        &crate::state::app_state_store::McpState::default(),
        &context,
    );
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
    mock.requests().pop().expect("one request").body
}

fn chat_reply() -> Reply {
    Reply::sse_data([chat_chunk(json!({"content": "ok"}), json!("stop"))])
}

fn responses_reply() -> Reply {
    Reply::sse_data([
        json!({"type": "response.created", "response": {"id": "resp_1", "model": "m"}}),
        json!({"type": "response.completed", "response": {"id": "resp_1", "status": "completed",
            "usage": {"input_tokens": 1, "output_tokens": 1}}}),
    ])
}

/// No effort on the profile or the session leaves the depth to the model: the
/// default adaptive launch must not turn into `medium` on its way to a
/// non-Anthropic provider. Responses still asks for the reasoning summary.
#[tokio::test]
async fn an_unset_effort_sends_no_reasoning_depth_on_chat_or_responses() {
    for (kind, model) in [
        (ProviderKind::Openai, "deepseek-reasoner"),
        (ProviderKind::Openai, "kimi-k2"),
        (ProviderKind::Vllm, "qwen3:32b"),
    ] {
        let mock = MockOpenAi::start(vec![chat_reply()]).await;
        let _env = thinking_env(
            &mock.base_url,
            kind,
            ProviderProtocol::OpenaiChat,
            model,
            None,
            None,
        );
        let body = main_loop_body(&mock, ThinkingConfig::Adaptive, None).await;
        assert!(body.get("reasoning_effort").is_none(), "{model}: {body}");
    }

    let mock = MockOpenAi::start(vec![responses_reply()]).await;
    let _env = thinking_env(
        &mock.base_url,
        ProviderKind::Openai,
        ProviderProtocol::OpenaiResponses,
        "gpt-5.5",
        None,
        None,
    );
    let body = main_loop_body(&mock, ThinkingConfig::Adaptive, None).await;
    assert!(body["reasoning"].get("effort").is_none(), "{body}");
    assert_eq!(body["reasoning"]["summary"], "auto", "{body}");
}

/// A configured depth still maps per family, and an explicit `off` on a
/// family with no known off value (DeepSeek, Kimi, unknown gateways) sends
/// nothing rather than a guess.
#[tokio::test]
async fn a_configured_effort_maps_per_family_and_off_is_omitted_where_unknown() {
    use crate::utils::effort::Effort;

    let mock = MockOpenAi::start(vec![chat_reply()]).await;
    let _env = thinking_env(
        &mock.base_url,
        ProviderKind::Openai,
        ProviderProtocol::OpenaiChat,
        "deepseek-reasoner",
        Some(Effort::High),
        None,
    );
    let body = main_loop_body(&mock, ThinkingConfig::Adaptive, None).await;
    assert_eq!(body["reasoning_effort"], "high", "profile effort: {body}");
    drop(_env);

    for model in ["deepseek-reasoner", "kimi-k2", "mystery-model"] {
        let mock = MockOpenAi::start(vec![chat_reply()]).await;
        let _env = thinking_env(
            &mock.base_url,
            ProviderKind::Openai,
            ProviderProtocol::OpenaiChat,
            model,
            None,
            None,
        );
        let body = main_loop_body(&mock, ThinkingConfig::Adaptive, Some(Effort::Off)).await;
        assert!(body.get("reasoning_effort").is_none(), "{model}: {body}");
    }

    let mock = MockOpenAi::start(vec![responses_reply()]).await;
    let _env = thinking_env(
        &mock.base_url,
        ProviderKind::Openai,
        ProviderProtocol::OpenaiResponses,
        "gpt-5.5",
        None,
        None,
    );
    let body = main_loop_body(&mock, ThinkingConfig::Adaptive, Some(Effort::Max)).await;
    assert_eq!(
        body["reasoning"],
        json!({"effort": "xhigh", "summary": "auto"}),
        "session effort: {body}"
    );
}

/// With the default long retention, only OpenAI's and Kimi's own hosts get a
/// cache key: the mock is neither, so even the OpenAI kind sends none unless
/// the provider's compat asks for the key.
#[tokio::test]
async fn the_default_long_retention_sends_no_cache_key_off_openai_hosts() {
    use crate::utils::settings::nyan::PromptCacheKeyMode;

    for (kind, protocol, reply) in [
        (
            ProviderKind::Vllm,
            ProviderProtocol::OpenaiChat,
            chat_reply(),
        ),
        (
            ProviderKind::Ollama,
            ProviderProtocol::OpenaiChat,
            chat_reply(),
        ),
        (
            ProviderKind::Openai,
            ProviderProtocol::OpenaiChat,
            chat_reply(),
        ),
        (
            ProviderKind::Openai,
            ProviderProtocol::OpenaiResponses,
            responses_reply(),
        ),
    ] {
        let mock = MockOpenAi::start(vec![reply]).await;
        let _env = thinking_env(&mock.base_url, kind, protocol, "gpt-5", None, None);
        let body = main_loop_body(&mock, ThinkingConfig::Adaptive, None).await;
        for field in [
            "prompt_cache_key",
            "prompt_cache_retention",
            "prompt_cache_options",
        ] {
            assert!(body.get(field).is_none(), "{kind:?} {protocol:?}: {body}");
        }
    }

    // `promptCacheKey: enabled` is honoured on both protocols; without
    // `longCacheRetention` no TTL field goes with it.
    for (protocol, reply) in [
        (ProviderProtocol::OpenaiChat, chat_reply()),
        (ProviderProtocol::OpenaiResponses, responses_reply()),
    ] {
        let mock = MockOpenAi::start(vec![reply]).await;
        let compat = OpenaiCompatSettings {
            prompt_cache_key: Some(PromptCacheKeyMode::Enabled),
            ..Default::default()
        };
        let _env = thinking_env(
            &mock.base_url,
            ProviderKind::Openai,
            protocol,
            "gpt-5",
            None,
            Some(compat),
        );
        let body = main_loop_body(&mock, ThinkingConfig::Adaptive, None).await;
        assert!(body["prompt_cache_key"].is_string(), "{protocol:?}: {body}");
        assert!(body.get("prompt_cache_retention").is_none(), "{body}");
        assert!(body.get("prompt_cache_options").is_none(), "{body}");
    }
}

#[tokio::test]
async fn a_streamed_request_is_accounted_exactly_once_with_its_catalog_price() {
    let mock = MockOpenAi::start(vec![Reply::sse_data([
        chat_chunk(json!({"content": "hi"}), Value::Null),
        chat_chunk(json!({}), json!("stop")),
        json!({"id": "chatcmpl-1", "choices": [], "usage": {
            "prompt_tokens": 1_000_000, "completion_tokens": 100_000,
            "prompt_tokens_details": {"cached_tokens": 400_000}}}),
    ])])
    .await;
    let _env = Env::new(&mock.base_url, ProviderProtocol::OpenaiChat);
    crate::cost_tracker::reset_cost_state();

    run(vec![user("go")], vec![]).await;

    let total = crate::cost_tracker::get_total_usage();
    assert_eq!(total.requests, 1, "one record per request: {total:?}");
    assert_eq!(
        (
            total.input_tokens,
            total.cache_read_tokens,
            total.output_tokens
        ),
        (600_000, 400_000, 100_000)
    );
    // gpt-5: $1.25 input, $0.125 cache read, $10 output per Mtok.
    let expected = 0.6 * 1.25 + 0.4 * 0.125 + 0.1 * 10.0;
    assert!((crate::cost_tracker::get_total_cost() - expected).abs() < 1e-9);
    assert_eq!(
        crate::cost_tracker::get_source_usage()[&crate::cost_tracker::CostSource::Main].requests,
        1
    );
    crate::cost_tracker::reset_cost_state();
}

#[tokio::test]
async fn a_side_query_is_accounted_once_under_the_side_query_source() {
    let mock = MockOpenAi::start(vec![Reply::json(json!({
        "id": "chatcmpl-sq",
        "model": "gpt-5",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": "ok"},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 5, "completion_tokens": 1}
    }))])
    .await;
    let _env = Env::new(&mock.base_url, ProviderProtocol::OpenaiChat);
    crate::cost_tracker::reset_cost_state();

    crate::utils::side_query::side_query(crate::utils::side_query::SideQueryOptions {
        model: "gpt-5".to_string(),
        messages: vec![crate::utils::side_query::user_text_message("Hi")],
        max_tokens: Some(1),
        max_retries: Some(0),
        query_source: "model_validation".to_string(),
        ..Default::default()
    })
    .await
    .expect("side query");

    let by_source = crate::cost_tracker::get_source_usage();
    assert_eq!(by_source.len(), 1, "{by_source:?}");
    let side = &by_source[&crate::cost_tracker::CostSource::SideQuery];
    assert_eq!(
        (side.requests, side.input_tokens, side.output_tokens),
        (1, 5, 1)
    );
    assert!(!side.cost_unknown);
    crate::cost_tracker::reset_cost_state();
}
