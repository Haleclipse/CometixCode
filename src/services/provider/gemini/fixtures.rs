//! Contract fixtures for Gemini adapter wire format and stream events.
//!
//! @nyan: docs/nyan/provider-redesign.md §7.4, PR 6.

use std::time::Duration;

use futures::StreamExt;
use serde_json::json;

use super::adapter::GeminiStream;
use super::errors;
use super::mock::{MockGemini, Reply};
use super::wire;
use crate::services::provider::adapter::{Cancel, ResolvedTarget};
use crate::services::provider::ir::{
    AssistantPart, BlockStart, Delta, JsonSchemaOutput, MediaSource, Message, ModelRequest,
    Protocol, ProviderErrorKind, SYSTEM_BOUNDARY, StopReason, StreamEventKind, ThinkingPreference,
    ToolChoice, ToolResultContent, ToolSpec, Usage, UserPart,
};
use crate::services::provider::openai::adapter::Machine;
use crate::services::provider::openai::transport::Frame;
use crate::utils::model::profile::ResolvedCapabilities;
use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK, TestEnvGuard};

struct Env {
    _lock: TestEnvGuard<'static>,
    _guards: Vec<EnvVarGuard>,
}

fn env(key: &str) -> Env {
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
        EnvVarGuard::set("TEST_GEMINI_KEY", key),
    ];
    crate::utils::settings::settings_cache::reset_settings_cache();
    Env {
        _lock: lock,
        _guards: guards,
    }
}

fn test_target() -> ResolvedTarget {
    ResolvedTarget {
        profile: "gemini-main".into(),
        provider: "google".into(),
        protocol: Protocol::Gemini,
        model: "gemini-2.5-flash".into(),
        context_window: 1_000_000,
        max_output_tokens: 8192,
        capabilities: ResolvedCapabilities {
            tools: true,
            thinking: true,
            images: true,
            structured_output: true,
            streaming: true,
        },
    }
}

fn minimal_request(messages: Vec<Message>) -> ModelRequest {
    ModelRequest {
        model: "gemini-2.5-flash".into(),
        system: Vec::new(),
        messages,
        tools: Vec::new(),
        tool_choice: ToolChoice::Auto,
        thinking: ThinkingPreference::Disabled,
        effort: None,
        max_output_tokens: 4096,
        temperature: None,
        structured_output: None,
        cache: Default::default(),
        query_source: "test".into(),
        agent_id: None,
        disable_parallel_tools: false,
        discovered_tools: Vec::new(),
        tools_pending: false,
        client: Default::default(),
        extensions: Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// 1. Wire Shape Tests
// ---------------------------------------------------------------------------

#[test]
fn test_wire_system_instruction_and_coalescing() {
    let target = test_target();
    let mut req = minimal_request(vec![
        Message::User {
            parts: vec![UserPart::Text {
                text: "Hello".into(),
            }],
        },
        Message::User {
            parts: vec![UserPart::Text {
                text: "World".into(),
            }],
        },
        Message::Assistant {
            parts: vec![
                AssistantPart::Thinking {
                    text: "thinking...".into(),
                    signature: "sig123".into(),
                    provider_state: None,
                },
                AssistantPart::Text {
                    text: "Answer".into(),
                },
            ],
        },
    ]);
    req.system = vec![
        format!("System part 1{SYSTEM_BOUNDARY}"),
        "System part 2".into(),
    ];

    let wire = wire::build_request(&req, &target);

    // System instruction stripped of boundary
    let sys = wire.system_instruction.expect("system instruction present");
    assert_eq!(
        sys.parts[0].text.as_deref(),
        Some("System part 1\n\nSystem part 2")
    );

    // Consecutive user messages should coalesce into one turn
    assert_eq!(wire.contents.len(), 2);
    assert_eq!(wire.contents[0].role.as_deref(), Some("user"));
    assert_eq!(wire.contents[0].parts.len(), 2);
    assert_eq!(wire.contents[0].parts[0].text.as_deref(), Some("Hello"));
    assert_eq!(wire.contents[0].parts[1].text.as_deref(), Some("World"));

    // Assistant message contains thinking with thought: true
    assert_eq!(wire.contents[1].role.as_deref(), Some("model"));
    assert_eq!(wire.contents[1].parts.len(), 2);
    assert_eq!(wire.contents[1].parts[0].thought, Some(true));
    assert_eq!(
        wire.contents[1].parts[0].text.as_deref(),
        Some("thinking...")
    );
    assert_eq!(
        wire.contents[1].parts[0].thought_signature.as_deref(),
        Some("sig123")
    );
    assert_eq!(wire.contents[1].parts[1].text.as_deref(), Some("Answer"));
}

#[test]
fn test_wire_images_and_documents_inline_data() {
    let target = test_target();
    let req = minimal_request(vec![Message::User {
        parts: vec![
            UserPart::Image {
                source: MediaSource::Base64 {
                    media_type: "image/png".into(),
                    data: "iVBORw0KGgoAAAANSUhEUg==".into(),
                },
                provider_state: None,
            },
            UserPart::Document {
                media_type: "application/pdf".into(),
                data: "JVBERi0xLjQK".into(),
            },
        ],
    }]);

    let wire = wire::build_request(&req, &target);
    assert_eq!(wire.contents.len(), 1);
    let parts = &wire.contents[0].parts;
    assert_eq!(parts.len(), 2);

    let img = parts[0].inline_data.as_ref().unwrap();
    assert_eq!(img.mime_type, "image/png");
    assert_eq!(img.data, "iVBORw0KGgoAAAANSUhEUg==");

    let doc = parts[1].inline_data.as_ref().unwrap();
    assert_eq!(doc.mime_type, "application/pdf");
    assert_eq!(doc.data, "JVBERi0xLjQK");
}

#[test]
fn test_wire_tools_function_call_and_response() {
    let target = test_target();
    let mut req = minimal_request(vec![
        Message::User {
            parts: vec![UserPart::Text {
                text: "List files".into(),
            }],
        },
        Message::Assistant {
            parts: vec![AssistantPart::ToolCall {
                id: "call_ls".into(),
                name: "ls".into(),
                input: json!({ "path": "." }),
                provider_state: None,
            }],
        },
        Message::User {
            parts: vec![UserPart::ToolResult {
                tool_call_id: "call_ls".into(),
                content: ToolResultContent::Text("file1.txt\nfile2.txt".into()),
                is_error: false,
            }],
        },
    ]);
    req.tools = vec![ToolSpec {
        name: "ls".into(),
        description: "List directory contents".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" }
            }
        }),
        strict: false,
        deferrable: false,
        is_mcp: false,
    }];
    req.tool_choice = ToolChoice::Tool { name: "ls".into() };

    let wire = wire::build_request(&req, &target);

    // Tools & ToolConfig
    assert_eq!(wire.tools.len(), 1);
    assert_eq!(wire.tools[0].function_declarations.len(), 1);
    assert_eq!(wire.tools[0].function_declarations[0].name, "ls");

    let tool_config = wire.tool_config.expect("tool_config present");
    let fn_config = tool_config.function_calling_config.unwrap();
    assert_eq!(fn_config.mode, "ANY");
    assert_eq!(fn_config.allowed_function_names, Some(vec!["ls".into()]));

    // Contents: FunctionCall and FunctionResponse
    assert_eq!(wire.contents.len(), 3);
    let fc_part = &wire.contents[1].parts[0];
    let fc = fc_part.function_call.as_ref().unwrap();
    assert_eq!(fc.name, "ls");
    assert_eq!(fc.args, json!({ "path": "." }));

    let fr_part = &wire.contents[2].parts[0];
    let fr = fr_part.function_response.as_ref().unwrap();
    assert_eq!(fr.name, "ls"); // Resolved from Assistant ToolCall
    assert_eq!(fr.response, json!({ "output": "file1.txt\nfile2.txt" }));
}

#[test]
fn test_wire_structured_output_and_thinking_budget() {
    let target = test_target();
    let mut req = minimal_request(vec![Message::User {
        parts: vec![UserPart::Text {
            text: "JSON".into(),
        }],
    }]);
    req.structured_output = Some(JsonSchemaOutput {
        schema: json!({ "type": "object" }),
    });
    req.thinking = ThinkingPreference::Budget { tokens: Some(2048) };
    req.temperature = Some(0.7);

    let wire = wire::build_request(&req, &target);
    let gen_config = wire.generation_config.expect("generation_config present");
    assert_eq!(gen_config.temperature, Some(0.7));
    assert_eq!(
        gen_config.response_mime_type.as_deref(),
        Some("application/json")
    );
    assert_eq!(
        gen_config.response_schema,
        Some(json!({ "type": "object" }))
    );

    let think = gen_config.thinking_config.expect("thinking_config present");
    assert_eq!(think.thinking_budget, Some(2048));
}

// ---------------------------------------------------------------------------
// 2. Stream Machine Tests
// ---------------------------------------------------------------------------

#[test]
fn test_stream_text_and_usage() {
    let target = test_target();
    let mut machine = GeminiStream::new(target, Some("req_123".into()));

    let chunk1 = json!({
        "candidates": [{
            "content": {
                "parts": [{ "text": "Hello, " }]
            }
        }],
        "usageMetadata": {
            "promptTokenCount": 15,
            "candidatesTokenCount": 2,
            "cachedContentTokenCount": 5
        }
    });

    let chunk2 = json!({
        "candidates": [{
            "content": {
                "parts": [{ "text": "world!" }]
            },
            "finishReason": "STOP"
        }],
        "usageMetadata": {
            "promptTokenCount": 15,
            "candidatesTokenCount": 4,
            "cachedContentTokenCount": 5
        }
    });

    let frame1 = Frame {
        event: None,
        data: chunk1.to_string(),
    };
    let frame2 = Frame {
        event: None,
        data: chunk2.to_string(),
    };

    let events1 = machine.feed(&frame1).unwrap();
    assert_eq!(
        events1,
        vec![
            StreamEventKind::MessageStart {
                request_id: Some("req_123".into()),
                message_id: None,
                model: Some("gemini-2.5-flash".into()),
                usage: Usage::default(),
            },
            StreamEventKind::BlockStart {
                index: 0,
                block: BlockStart::Text,
            },
            StreamEventKind::BlockDelta {
                index: 0,
                delta: Delta::Text {
                    text: "Hello, ".into(),
                },
            },
        ]
    );

    let events2 = machine.feed(&frame2).unwrap();
    assert_eq!(
        events2,
        vec![StreamEventKind::BlockDelta {
            index: 0,
            delta: Delta::Text {
                text: "world!".into(),
            },
        }]
    );

    let final_events = machine.end().unwrap();
    assert_eq!(
        final_events,
        vec![
            StreamEventKind::BlockStop { index: 0 },
            StreamEventKind::MessageDelta {
                stop_reason: Some(StopReason::EndTurn),
                usage: Usage {
                    input_tokens: 10, // 15 - 5
                    output_tokens: 4,
                    cache_read_tokens: 5,
                    cache_write_tokens: 0,
                    cache_deleted_tokens: 0,
                    reasoning_tokens: 0,
                },
            },
            StreamEventKind::MessageStop,
        ]
    );
}

#[test]
fn test_stream_thinking_then_tool_call() {
    let target = test_target();
    let mut machine = GeminiStream::new(target, None);

    let chunk = json!({
        "candidates": [{
            "content": {
                "parts": [
                    { "text": "I should call ls", "thought": true, "thoughtSignature": "tsig1" },
                    { "functionCall": { "name": "ls", "args": { "path": "/" }, "id": "call_1" } }
                ]
            },
            "finishReason": "STOP"
        }]
    });

    let frame = Frame {
        event: None,
        data: chunk.to_string(),
    };
    let events = machine.feed(&frame).unwrap();

    assert_eq!(
        events[0],
        StreamEventKind::MessageStart {
            request_id: None,
            message_id: None,
            model: Some("gemini-2.5-flash".into()),
            usage: Usage::default(),
        }
    );
    // Thinking block
    assert_eq!(
        events[1],
        StreamEventKind::BlockStart {
            index: 0,
            block: BlockStart::Thinking,
        }
    );
    assert_eq!(
        events[2],
        StreamEventKind::BlockDelta {
            index: 0,
            delta: Delta::Thinking {
                text: "I should call ls".into(),
            },
        }
    );
    assert_eq!(
        events[3],
        StreamEventKind::BlockDelta {
            index: 0,
            delta: Delta::Signature {
                signature: "tsig1".into(),
            },
        }
    );
    // Transition to ToolCall block (stops thinking block first)
    assert_eq!(events[4], StreamEventKind::BlockStop { index: 0 });
    assert_eq!(
        events[5],
        StreamEventKind::BlockStart {
            index: 1,
            block: BlockStart::ToolCall {
                id: "call_1".into(),
                name: "ls".into(),
                server: false,
                input: json!({}),
            },
        }
    );
    assert_eq!(
        events[6],
        StreamEventKind::BlockDelta {
            index: 1,
            delta: Delta::ToolInput {
                json: json!({ "path": "/" }).to_string(),
            },
        }
    );
    assert_eq!(events[7], StreamEventKind::BlockStop { index: 1 });

    let final_events = machine.end().unwrap();
    // Finish reason is mapped to ToolUse because a tool call occurred
    assert_eq!(
        final_events[0],
        StreamEventKind::MessageDelta {
            stop_reason: Some(StopReason::ToolUse),
            usage: Usage::default(),
        }
    );
}

// ---------------------------------------------------------------------------
// 3. Error Classification Tests
// ---------------------------------------------------------------------------

#[test]
fn test_classify_resource_exhausted_with_retry_delay() {
    let body = json!({
        "error": {
            "code": 429,
            "message": "Resource has been exhausted (e.g. check quota).",
            "status": "RESOURCE_EXHAUSTED",
            "details": [
                {
                    "@type": "type.googleapis.com/google.rpc.RetryInfo",
                    "retryDelay": "4.5s"
                }
            ]
        }
    });

    let err = errors::from_http(429, Default::default(), &body.to_string(), None);
    assert_eq!(err.kind, ProviderErrorKind::RateLimited);
    assert_eq!(err.retry_after, Some(Duration::from_millis(4500)));
    assert_eq!(err.status, Some(429));
}

#[test]
fn test_classify_google_rpc_statuses() {
    let check = |status_code, rpc_status, expected_kind| {
        let body = json!({
            "error": {
                "code": status_code,
                "message": "Sample error",
                "status": rpc_status
            }
        });
        let err = errors::from_http(status_code, Default::default(), &body.to_string(), None);
        assert_eq!(err.kind, expected_kind, "testing status {rpc_status}");
    };

    check(503, "UNAVAILABLE", ProviderErrorKind::Overloaded);
    check(401, "UNAUTHENTICATED", ProviderErrorKind::Unauthorized);
    check(403, "PERMISSION_DENIED", ProviderErrorKind::Forbidden);
    check(404, "NOT_FOUND", ProviderErrorKind::NotFound);
    check(400, "INVALID_ARGUMENT", ProviderErrorKind::InvalidRequest);
    check(
        500,
        "INTERNAL",
        ProviderErrorKind::Transient { retryable: true },
    );
}

#[test]
fn test_classify_context_overflow_message() {
    let body = json!({
        "error": {
            "code": 400,
            "message": "Invalid argument: token count exceeds maximum context length of 1000000",
            "status": "INVALID_ARGUMENT"
        }
    });
    let err = errors::from_http(400, Default::default(), &body.to_string(), None);
    assert_eq!(err.kind, ProviderErrorKind::ContextOverflow);
}

// ---------------------------------------------------------------------------
// 4. Mock Server End-to-End Integration Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_mock_gemini_success_stream() {
    let _env = env("fake-api-key");
    let chunk = json!({
        "candidates": [{
            "content": {
                "parts": [{ "text": "Mock response" }]
            },
            "finishReason": "STOP"
        }],
        "usageMetadata": {
            "promptTokenCount": 10,
            "candidatesTokenCount": 3
        }
    });

    let server = MockGemini::start(vec![Reply::sse_data(vec![chunk])]).await;
    super::install_test_main_profile(&server.base_url, "TEST_GEMINI_KEY", Default::default());
    let profile = crate::utils::model::profile::resolve_main_profile(
        &crate::utils::settings::get_initial_settings(),
    )
    .unwrap();

    let req = minimal_request(vec![Message::User {
        parts: vec![UserPart::Text { text: "Hi".into() }],
    }]);

    let cancel = Cancel::new();
    let opened = super::open_stream(&profile, &req, &cancel).await.unwrap();
    let mut events = Vec::new();
    let mut stream = opened.events;
    while let Some(ev) = stream.next().await {
        events.push(ev.unwrap().kind);
    }

    assert_eq!(events.len(), 6);
    assert!(matches!(events[0], StreamEventKind::MessageStart { .. }));
    assert!(matches!(events[1], StreamEventKind::BlockStart { .. }));
    assert!(matches!(events[2], StreamEventKind::BlockDelta { .. }));
    assert!(matches!(events[3], StreamEventKind::BlockStop { .. }));
    assert!(matches!(events[4], StreamEventKind::MessageDelta { .. }));
    assert!(matches!(events[5], StreamEventKind::MessageStop));

    // Verify request line and header
    let reqs = server.requests();
    assert_eq!(reqs.len(), 1);
    assert!(
        reqs[0]
            .request_line
            .contains("/models/gemini-2.5-flash:streamGenerateContent?alt=sse")
    );
    assert_eq!(reqs[0].header("x-goog-api-key"), Some("fake-api-key"));
}

#[tokio::test]
async fn test_mock_gemini_rate_limited() {
    let _env = env("fake-key");
    let err_body = json!({
        "error": {
            "code": 429,
            "message": "Resource has been exhausted",
            "status": "RESOURCE_EXHAUSTED"
        }
    });

    let server = MockGemini::start(vec![Reply::error(429, err_body)]).await;
    super::install_test_main_profile(&server.base_url, "TEST_GEMINI_KEY", Default::default());
    let profile = crate::utils::model::profile::resolve_main_profile(
        &crate::utils::settings::get_initial_settings(),
    )
    .unwrap();

    let req = minimal_request(vec![Message::User {
        parts: vec![UserPart::Text { text: "Hi".into() }],
    }]);

    let cancel = Cancel::new();
    let err = super::open_stream(&profile, &req, &cancel)
        .await
        .err()
        .expect("expected error status");
    assert_eq!(err.kind, ProviderErrorKind::RateLimited);
    assert_eq!(err.status, Some(429));
}

#[tokio::test]
async fn test_mock_gemini_partial_stream_on_cut() {
    let _env = env("fake-key");
    let chunk = json!({
        "candidates": [{
            "content": {
                "parts": [{ "text": "First part of reply" }]
            }
        }]
    });

    let server = MockGemini::start(vec![Reply::SseThenCut(vec![(None, chunk.to_string())])]).await;
    super::install_test_main_profile(&server.base_url, "TEST_GEMINI_KEY", Default::default());
    let profile = crate::utils::model::profile::resolve_main_profile(
        &crate::utils::settings::get_initial_settings(),
    )
    .unwrap();

    let req = minimal_request(vec![Message::User {
        parts: vec![UserPart::Text { text: "Hi".into() }],
    }]);

    let cancel = Cancel::new();
    let opened = super::open_stream(&profile, &req, &cancel).await.unwrap();
    let mut stream = opened.events;
    let first = stream.next().await.unwrap().unwrap();
    assert!(matches!(first.kind, StreamEventKind::MessageStart { .. }));
    let second = stream.next().await.unwrap().unwrap();
    assert!(matches!(second.kind, StreamEventKind::BlockStart { .. }));
    let third = stream.next().await.unwrap().unwrap();
    assert!(matches!(third.kind, StreamEventKind::BlockDelta { .. }));

    // Now stream cuts mid-way after output was already yielded
    let err = stream.next().await.unwrap().unwrap_err();
    assert_eq!(err.kind, ProviderErrorKind::PartialStream);
}
