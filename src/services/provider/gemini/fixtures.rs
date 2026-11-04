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
    AssistantPart, BlockStart, Delta, Effort, JsonSchemaOutput, MediaSource, Message, ModelRequest,
    Protocol, ProviderErrorKind, SYSTEM_BOUNDARY, StopReason, StreamEventKind, ThinkingPreference,
    ToolChoice, ToolResultContent, ToolResultPart, ToolSpec, Usage, UserPart,
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

#[test]
fn test_wire_gemini_3_model_detection() {
    assert!(wire::is_gemini_3_or_newer("gemini-3.8-flash"));
    assert!(wire::is_gemini_3_or_newer("gemini-3-pro-preview"));
    assert!(wire::is_gemini_3_or_newer("gemini-3.1-pro-preview"));
    assert!(wire::is_gemini_3_or_newer("gemini-3.7-flash"));
    assert!(wire::is_gemini_3_or_newer("gemini-4-argon"));
    assert!(wire::is_gemini_3_or_newer("gemini-flash-latest"));
    assert!(wire::is_gemini_3_or_newer("gemini-flash-lite-latest"));
    assert!(wire::is_gemini_3_or_newer("gemma-4-27b"));
    assert!(wire::is_gemini_3_or_newer("gemma4-9b"));
    assert!(wire::is_gemini_3_or_newer("models/gemini-3-flash"));
    assert!(!wire::is_gemini_3_or_newer("gemini-2.5-flash"));
    assert!(!wire::is_gemini_3_or_newer("gemini-2.5-pro"));
    assert!(!wire::is_gemini_3_or_newer("gemini-1.5-pro"));
    assert!(!wire::is_gemini_3_or_newer("claude-3-7-sonnet"));
}

#[test]
fn test_wire_gemini_3_thinking_level_and_effort_mapping() {
    let mut target = test_target();
    target.model = "gemini-3.8-flash".into();

    // 1. Adaptive with no effort hint defaults to HIGH for Gemini 3
    let mut req = minimal_request(vec![Message::User {
        parts: vec![UserPart::Text {
            text: "Solve math".into(),
        }],
    }]);
    req.thinking = ThinkingPreference::Adaptive;
    let wire = wire::build_request(&req, &target);
    let think = wire
        .generation_config
        .as_ref()
        .and_then(|g| g.thinking_config.as_ref())
        .expect("thinking_config present");
    assert_eq!(think.thinking_level.as_deref(), Some("HIGH"));
    assert_eq!(think.include_thoughts, Some(true));
    assert_eq!(think.thinking_budget, None);

    // 2. Named efforts: minimal, low, medium, high, and clamp xhigh/max to HIGH
    for (name, expected) in [
        ("minimal", "MINIMAL"),
        ("low", "LOW"),
        ("medium", "MEDIUM"),
        ("high", "HIGH"),
        ("xhigh", "HIGH"),
        ("max", "HIGH"),
    ] {
        req.effort = Some(Effort::Named(name.into()));
        let wire = wire::build_request(&req, &target);
        let think = wire
            .generation_config
            .as_ref()
            .and_then(|g| g.thinking_config.as_ref())
            .expect("thinking_config present");
        assert_eq!(think.thinking_level.as_deref(), Some(expected));
        assert_eq!(think.include_thoughts, Some(true));
        assert_eq!(think.thinking_budget, None);
    }

    // 3. Budget preference mapped to discrete levels for Gemini 3
    req.effort = None;
    for (tokens, expected) in [(1000, "LOW"), (4096, "MEDIUM"), (32768, "HIGH")] {
        req.thinking = ThinkingPreference::Budget {
            tokens: Some(tokens),
        };
        let wire = wire::build_request(&req, &target);
        let think = wire
            .generation_config
            .as_ref()
            .and_then(|g| g.thinking_config.as_ref())
            .expect("thinking_config present");
        assert_eq!(think.thinking_level.as_deref(), Some(expected));
        assert_eq!(think.include_thoughts, Some(true));
        assert_eq!(think.thinking_budget, None);
    }

    // 4. Disabled thinking produces no thinking_config
    req.thinking = ThinkingPreference::Disabled;
    let wire = wire::build_request(&req, &target);
    assert!(
        wire.generation_config
            .as_ref()
            .and_then(|g| g.thinking_config.as_ref())
            .is_none()
    );
}

#[test]
fn test_wire_tool_result_images_gemini_3() {
    let mut target = test_target();
    target.model = "gemini-3.8-flash".into();

    let req = minimal_request(vec![
        Message::User {
            parts: vec![UserPart::Text {
                text: "Take screenshot".into(),
            }],
        },
        Message::Assistant {
            parts: vec![AssistantPart::ToolCall {
                id: "call_snap".into(),
                name: "screenshot".into(),
                input: json!({}),
                provider_state: None,
            }],
        },
        Message::User {
            parts: vec![UserPart::ToolResult {
                tool_call_id: "call_snap".into(),
                content: ToolResultContent::Parts(vec![
                    ToolResultPart::Text {
                        text: "Screenshot captured".into(),
                    },
                    ToolResultPart::Image {
                        source: MediaSource::Base64 {
                            media_type: "image/png".into(),
                            data: "iVBORw0KGgoAAAANSUhEUg==".into(),
                        },
                        provider_state: None,
                    },
                ]),
                is_error: false,
            }],
        },
    ]);

    let wire = wire::build_request(&req, &target);
    assert_eq!(wire.contents.len(), 3);
    assert_eq!(wire.contents[2].role.as_deref(), Some("user"));
    assert_eq!(wire.contents[2].parts.len(), 1);

    let fr = wire.contents[2].parts[0]
        .function_response
        .as_ref()
        .unwrap();
    assert_eq!(fr.name, "screenshot");
    assert_eq!(fr.id.as_deref(), Some("call_snap"));
    assert_eq!(fr.response, json!({ "output": "Screenshot captured" }));

    // Image is nested directly inside FunctionResponse.parts
    let parts = fr
        .parts
        .as_ref()
        .expect("FunctionResponse parts must be present for Gemini 3");
    assert_eq!(parts.len(), 1);
    let inline = parts[0].inline_data.as_ref().unwrap();
    assert_eq!(inline.mime_type, "image/png");
    assert_eq!(inline.data, "iVBORw0KGgoAAAANSUhEUg==");
}

#[test]
fn test_wire_tool_result_image_only_and_error_handling() {
    let mut target = test_target();
    target.model = "gemini-3-pro-preview".into();

    let req = minimal_request(vec![
        Message::User {
            parts: vec![UserPart::Text {
                text: "Run tool".into(),
            }],
        },
        Message::Assistant {
            parts: vec![AssistantPart::ToolCall {
                id: "call_err".into(),
                name: "failing_visual_tool".into(),
                input: json!({}),
                provider_state: None,
            }],
        },
        Message::User {
            parts: vec![UserPart::ToolResult {
                tool_call_id: "call_err".into(),
                content: ToolResultContent::Parts(vec![
                    // Whitespace-only text should be treated as no text (Pi issue #8720)
                    ToolResultPart::Text {
                        text: "   \n".into(),
                    },
                    ToolResultPart::Image {
                        source: MediaSource::Base64 {
                            media_type: "image/jpeg".into(),
                            data: "/9j/4AAQSkZJRg==".into(),
                        },
                        provider_state: None,
                    },
                ]),
                is_error: true,
            }],
        },
    ]);

    let wire = wire::build_request(&req, &target);
    assert_eq!(wire.contents.len(), 3);
    let fr = wire.contents[2].parts[0]
        .function_response
        .as_ref()
        .unwrap();
    assert_eq!(fr.name, "failing_visual_tool");
    // Since is_error: true and text was whitespace, response uses {"error": "(see attached image)"}
    assert_eq!(fr.response, json!({ "error": "(see attached image)" }));
    let parts = fr.parts.as_ref().unwrap();
    assert_eq!(parts.len(), 1);
    assert_eq!(
        parts[0].inline_data.as_ref().unwrap().mime_type,
        "image/jpeg"
    );
}

#[test]
fn test_wire_tool_result_images_gemini_2_5_fallback_turn() {
    let mut target = test_target();
    target.model = "gemini-2.5-flash".into();

    let req = minimal_request(vec![
        Message::User {
            parts: vec![UserPart::Text {
                text: "Run tool".into(),
            }],
        },
        Message::Assistant {
            parts: vec![AssistantPart::ToolCall {
                id: "call_img".into(),
                name: "legacy_tool".into(),
                input: json!({}),
                provider_state: None,
            }],
        },
        Message::User {
            parts: vec![UserPart::ToolResult {
                tool_call_id: "call_img".into(),
                content: ToolResultContent::Parts(vec![ToolResultPart::Image {
                    source: MediaSource::Base64 {
                        media_type: "image/png".into(),
                        data: "abc123".into(),
                    },
                    provider_state: None,
                }]),
                is_error: false,
            }],
        },
    ]);

    let wire = wire::build_request(&req, &target);
    // For Gemini < 3, function response has NO parts, and images go into separate user turn
    assert_eq!(wire.contents.len(), 4);
    assert_eq!(wire.contents[2].role.as_deref(), Some("user"));
    let fr = wire.contents[2].parts[0]
        .function_response
        .as_ref()
        .unwrap();
    assert_eq!(fr.name, "legacy_tool");
    assert_eq!(fr.response, json!({ "output": "(see attached image)" }));
    assert!(fr.parts.is_none());

    // Contents[3] is the synthetic user image turn
    assert_eq!(wire.contents[3].role.as_deref(), Some("user"));
    assert_eq!(wire.contents[3].parts.len(), 2);
    assert_eq!(
        wire.contents[3].parts[0].text.as_deref(),
        Some("Tool result image:")
    );
    assert_eq!(
        wire.contents[3].parts[1]
            .inline_data
            .as_ref()
            .unwrap()
            .mime_type,
        "image/png"
    );
}

#[test]
fn test_wire_tools_deterministic_sorting() {
    let target = test_target();
    let mut req = minimal_request(vec![Message::User {
        parts: vec![UserPart::Text {
            text: "Hello".into(),
        }],
    }]);
    // Tools in non-alphabetical order
    req.tools = vec![
        ToolSpec {
            name: "zeta".into(),
            description: "Zeta tool".into(),
            input_schema: json!({ "type": "object" }),
            strict: false,
            deferrable: false,
            is_mcp: false,
        },
        ToolSpec {
            name: "alpha".into(),
            description: "Alpha tool".into(),
            input_schema: json!({ "type": "object" }),
            strict: false,
            deferrable: false,
            is_mcp: false,
        },
        ToolSpec {
            name: "beta".into(),
            description: "Beta tool".into(),
            input_schema: json!({ "type": "object" }),
            strict: false,
            deferrable: false,
            is_mcp: false,
        },
    ];

    let wire = wire::build_request(&req, &target);
    assert_eq!(wire.tools.len(), 1);
    let decls = &wire.tools[0].function_declarations;
    assert_eq!(decls.len(), 3);
    assert_eq!(decls[0].name, "alpha");
    assert_eq!(decls[1].name, "beta");
    assert_eq!(decls[2].name, "zeta");
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

// ---------------------------------------------------------------------------
// 5. Query Layer Retry & RatePolicy Fixture Tests
// ---------------------------------------------------------------------------

struct QueryEnv {
    _lock: TestEnvGuard<'static>,
    _guards: Vec<EnvVarGuard>,
    root: std::path::PathBuf,
}

impl Drop for QueryEnv {
    fn drop(&mut self) {
        crate::utils::settings::settings_cache::reset_settings_cache();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl QueryEnv {
    fn new(base_url: &str) -> Self {
        let lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = std::env::temp_dir().join(format!("nyan-gemini-query-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let guards = vec![
            EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root),
            EnvVarGuard::set("CLAUDE_CODE_SIMPLE", "1"),
            EnvVarGuard::set("TEST_GEMINI_KEY", "fake-gemini-key"),
            EnvVarGuard::set("API_TIMEOUT_MS", "5000"),
            EnvVarGuard::set("NO_PROXY", "*"),
            EnvVarGuard::set("no_proxy", "*"),
            EnvVarGuard::unset("HTTPS_PROXY"),
            EnvVarGuard::unset("HTTP_PROXY"),
            EnvVarGuard::unset("ANTHROPIC_API_KEY"),
        ];
        crate::utils::settings::settings_cache::reset_settings_cache();
        super::install_test_main_profile(
            base_url,
            "TEST_GEMINI_KEY",
            crate::utils::settings::nyan::ModelCapabilities {
                context_window: Some(1_000_000),
                max_output_tokens: Some(8192),
                tools: Some("native".into()),
                thinking: Some("adaptive".into()),
                streaming: Some(true),
                ..Default::default()
            },
        );
        Self {
            _lock: lock,
            _guards: guards,
            root,
        }
    }
}

fn user_msg(text: &str) -> crate::types::message::Message {
    crate::types::message::Message::User(crate::types::message::UserMessage {
        uuid: uuid::Uuid::new_v4().to_string(),
        timestamp: chrono::Utc::now(),
        content: vec![crate::types::message::UserContent::Text(text.to_string())],
        is_compact_summary: false,
        plan_content: None,
        image_paste_ids: None,
        is_visible_in_transcript_only: false,
        mcp_meta: None,
        source_tool_assistant_uuid: None,
        permission_mode: None,
        origin: None,
        summarize_metadata: None,
    })
}

fn gemini_chunk(text: &str) -> serde_json::Value {
    json!({
        "candidates": [{
            "content": {
                "parts": [{ "text": text }]
            },
            "finishReason": "STOP"
        }],
        "usageMetadata": {
            "promptTokenCount": 10,
            "candidatesTokenCount": 5
        }
    })
}

#[tokio::test]
async fn test_gemini_burst_429_retries_and_recovers() {
    let chunk = gemini_chunk("recovered");
    let err_body = json!({
        "error": {
            "code": 429,
            "message": "Resource has been exhausted",
            "status": "RESOURCE_EXHAUSTED",
            "details": [
                {
                    "@type": "type.googleapis.com/google.rpc.RetryInfo",
                    "retryDelay": "0.01s"
                }
            ]
        }
    });

    let server = MockGemini::start(vec![
        Reply::error(429, err_body),
        Reply::sse_data(vec![chunk]),
    ])
    .await;
    let _env = QueryEnv::new(&server.base_url);

    let options = crate::services::api::claude::Options::new(
        "gemini-2.5-flash".to_string(),
        "repl_main_thread".to_string(),
    );
    let mut rx = crate::services::api::claude::query_model_with_streaming(
        &[user_msg("go")],
        &vec![],
        &crate::utils::thinking::ThinkingConfig::Disabled,
        &[],
        &options,
    )
    .await
    .expect("stream should open after retry");

    let mut found_text = false;
    while let Some(item) = rx.recv().await {
        if let crate::services::api::claude::QueryModelStreamItem::Assistant(msg) = item {
            if let Some(crate::types::message::AssistantContent::Text(text)) =
                msg.first_content_block()
            {
                if text == "recovered" {
                    found_text = true;
                }
            }
        }
    }
    assert!(found_text, "expected recovered text from second attempt");
    assert_eq!(
        server.requests().len(),
        2,
        "burst 429 retried once and recovered"
    );
}

#[tokio::test]
async fn test_gemini_billing_quota_429_does_not_retry() {
    let err_body = json!({
        "error": {
            "code": 429,
            "message": "Quota exceeded for quota metric 'Queries'",
            "status": "RESOURCE_EXHAUSTED"
        }
    });

    let server = MockGemini::start(vec![
        Reply::error(429, err_body),
        Reply::sse_data(vec![gemini_chunk("unexpected")]),
    ])
    .await;
    let _env = QueryEnv::new(&server.base_url);

    let options = crate::services::api::claude::Options::new(
        "gemini-2.5-flash".to_string(),
        "repl_main_thread".to_string(),
    );
    let result = crate::services::api::claude::query_model_with_streaming(
        &[user_msg("go")],
        &vec![],
        &crate::utils::thinking::ThinkingConfig::Disabled,
        &[],
        &options,
    )
    .await;

    let error_text = match result {
        Err(e) => e.to_string(),
        Ok(mut rx) => {
            let mut text = String::new();
            while let Some(item) = rx.recv().await {
                if let crate::services::api::claude::QueryModelStreamItem::SystemError(err) = item {
                    text = err.content;
                }
            }
            text
        }
    };
    assert!(
        error_text.contains("Quota exceeded") || error_text.contains("RESOURCE_EXHAUSTED"),
        "error_text: {error_text}"
    );
    assert_eq!(
        server.requests().len(),
        1,
        "quota 429 must not be retried on same endpoint"
    );
}

#[tokio::test]
async fn test_gemini_503_unavailable_retries_and_recovers() {
    let chunk = gemini_chunk("recovered from 503");
    let err_body = json!({
        "error": {
            "code": 503,
            "message": "Service unavailable",
            "status": "UNAVAILABLE"
        }
    });

    let server = MockGemini::start(vec![
        Reply::error(503, err_body),
        Reply::sse_data(vec![chunk]),
    ])
    .await;
    let _env = QueryEnv::new(&server.base_url);

    let options = crate::services::api::claude::Options::new(
        "gemini-2.5-flash".to_string(),
        "repl_main_thread".to_string(),
    );
    let mut rx = crate::services::api::claude::query_model_with_streaming(
        &[user_msg("go")],
        &vec![],
        &crate::utils::thinking::ThinkingConfig::Disabled,
        &[],
        &options,
    )
    .await
    .expect("stream should open after retry");

    let mut found_text = false;
    while let Some(item) = rx.recv().await {
        if let crate::services::api::claude::QueryModelStreamItem::Assistant(msg) = item {
            if let Some(crate::types::message::AssistantContent::Text(text)) =
                msg.first_content_block()
            {
                if text == "recovered from 503" {
                    found_text = true;
                }
            }
        }
    }
    assert!(found_text);
    assert_eq!(server.requests().len(), 2, "503 retried once and recovered");
}

#[tokio::test]
async fn test_gemini_401_unauthenticated_fails_fast_without_retry() {
    let err_body = json!({
        "error": {
            "code": 401,
            "message": "API key not valid",
            "status": "UNAUTHENTICATED"
        }
    });

    let server = MockGemini::start(vec![
        Reply::error(401, err_body),
        Reply::sse_data(vec![gemini_chunk("unexpected")]),
    ])
    .await;
    let _env = QueryEnv::new(&server.base_url);

    let options = crate::services::api::claude::Options::new(
        "gemini-2.5-flash".to_string(),
        "repl_main_thread".to_string(),
    );
    let result = crate::services::api::claude::query_model_with_streaming(
        &[user_msg("go")],
        &vec![],
        &crate::utils::thinking::ThinkingConfig::Disabled,
        &[],
        &options,
    )
    .await;

    assert!(
        result.is_err() || {
            let mut rx = result.unwrap();
            let mut has_err = false;
            while let Some(item) = rx.recv().await {
                if matches!(
                    item,
                    crate::services::api::claude::QueryModelStreamItem::SystemError(_)
                ) {
                    has_err = true;
                }
            }
            has_err
        }
    );
    assert_eq!(
        server.requests().len(),
        1,
        "401 must fail immediately without retry"
    );
}

#[tokio::test]
async fn test_gemini_partial_stream_after_output_does_not_replay() {
    let chunk = json!({
        "candidates": [{
            "content": {
                "parts": [{ "text": "partial output" }]
            }
        }]
    });

    let server = MockGemini::start(vec![
        Reply::SseThenCut(vec![(None, chunk.to_string())]),
        Reply::sse_data(vec![gemini_chunk("unexpected replay")]),
    ])
    .await;
    let _env = QueryEnv::new(&server.base_url);

    let options = crate::services::api::claude::Options::new(
        "gemini-2.5-flash".to_string(),
        "repl_main_thread".to_string(),
    );
    let mut rx = crate::services::api::claude::query_model_with_streaming(
        &[user_msg("go")],
        &vec![],
        &crate::utils::thinking::ThinkingConfig::Disabled,
        &[],
        &options,
    )
    .await
    .expect("stream should open with first chunk committed");

    let mut got_error = false;
    while let Some(item) = rx.recv().await {
        if matches!(
            item,
            crate::services::api::claude::QueryModelStreamItem::SystemError(_)
        ) {
            got_error = true;
        }
    }
    assert!(got_error, "expected system error on mid-stream cut");
    assert_eq!(
        server.requests().len(),
        1,
        "partial stream must not trigger a replay request"
    );
}

#[tokio::test]
async fn test_gemini_side_query_success() {
    let reply = json!({
        "candidates": [{
            "content": {
                "parts": [{ "text": "gemini side query response" }]
            },
            "finishReason": "STOP"
        }],
        "usageMetadata": {
            "promptTokenCount": 8,
            "candidatesTokenCount": 4
        }
    });

    let server = MockGemini::start(vec![Reply::json(reply)]).await;
    let _env = QueryEnv::new(&server.base_url);

    let response =
        crate::utils::side_query::side_query(crate::utils::side_query::SideQueryOptions {
            model: "gemini-2.5-flash".to_string(),
            messages: vec![crate::utils::side_query::user_text_message("Hi Gemini")],
            max_tokens: Some(10),
            max_retries: Some(0),
            query_source: "model_validation".to_string(),
            ..Default::default()
        })
        .await
        .expect("side_query succeeds on Gemini profile");

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0]
            .request_line
            .contains("/models/gemini-2.5-flash:generateContent")
    );
    assert_eq!(
        requests[0].header("x-goog-api-key"),
        Some("fake-gemini-key")
    );

    let content = crate::utils::messages::extract_text_content(&response.content, None);
    assert_eq!(content, "gemini side query response");
}

#[tokio::test]
async fn test_gemini_count_tokens_api() {
    let reply = json!({
        "totalTokens": 42
    });

    let server = MockGemini::start(vec![Reply::json(reply)]).await;
    let _env = QueryEnv::new(&server.base_url);

    let count =
        crate::services::token_estimation::count_tokens_with_api("hello gemini world").await;
    assert_eq!(count, Some(42));

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0]
            .request_line
            .contains("/models/gemini-2.5-flash:countTokens")
    );
    assert_eq!(
        requests[0].header("x-goog-api-key"),
        Some("fake-gemini-key")
    );
}

#[tokio::test]
async fn test_gemini_non_streaming_fallback_on_404() {
    let err_body = json!({
        "error": {
            "code": 404,
            "message": "Stream endpoint not found",
            "status": "NOT_FOUND"
        }
    });
    let non_stream_reply = json!({
        "candidates": [{
            "content": {
                "parts": [{ "text": "gemini fallback response" }]
            },
            "finishReason": "STOP"
        }],
        "usageMetadata": {
            "promptTokenCount": 12,
            "candidatesTokenCount": 4
        }
    });

    let server = MockGemini::start(vec![
        Reply::error(404, err_body),
        Reply::json(non_stream_reply),
    ])
    .await;
    let _env = QueryEnv::new(&server.base_url);

    let options = crate::services::api::claude::Options::new(
        "gemini-2.5-flash".to_string(),
        "repl_main_thread".to_string(),
    );
    let mut rx = crate::services::api::claude::query_model_with_streaming(
        &[user_msg("go")],
        &vec![],
        &crate::utils::thinking::ThinkingConfig::Disabled,
        &[],
        &options,
    )
    .await
    .expect("stream opens on fallback");

    let mut items = Vec::new();
    while let Some(item) = rx.recv().await {
        items.push(item);
    }
    assert!(
        items.iter().any(|i| matches!(
            i,
            crate::services::api::claude::QueryModelStreamItem::StreamingFallback
        )),
        "expected StreamingFallback item: {items:?}"
    );

    let found_text = items.iter().any(|item| {
        if let crate::services::api::claude::QueryModelStreamItem::Assistant(msg) = item {
            if let Some(crate::types::message::AssistantContent::Text(text)) =
                msg.first_content_block()
            {
                return text == "gemini fallback response";
            }
        }
        false
    });
    assert!(found_text, "expected fallback text: {items:?}");

    let requests = server.requests();
    assert_eq!(
        requests.len(),
        2,
        "1 streaming + 1 non-streaming fallback request"
    );
    assert!(
        requests[0]
            .request_line
            .contains(":streamGenerateContent?alt=sse")
    );
    assert!(requests[1].request_line.contains(":generateContent"));
}
