//! OpenAI Chat Completions: IR request -> wire body, wire stream -> IR events.
//!
//! @nyan: both halves are pure (no I/O) so contract fixtures can drive them
//! with canned frames. Replay state is not used: Chat Completions has no input
//! channel for reasoning, so thinking in history is dropped, and server-side
//! tool calls (an Anthropic concept) are dropped with it.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::transport::Frame;
use super::{
    EMPTY_TOOL_OUTPUT, call_part, media_url, reasoning_effort, system_text, tool_result_text,
};
use crate::services::provider::adapter::ResolvedTarget;
use crate::services::provider::ir::{
    AssistantPart, BlockStart, Delta, MediaSource, Message, ModelRequest, ProviderError,
    ProviderErrorKind, StopReason, StreamEventKind, ToolChoice, ToolResultContent, ToolResultPart,
    Usage, UserPart,
};

/// OpenAI rejects tool-call ids longer than 40 characters.
const MAX_CALL_ID: usize = 40;

/// A history id (possibly another protocol's, possibly a Responses composite)
/// as a Chat `tool_call_id`. Pure, so the assistant's call and the matching
/// result always agree.
pub fn chat_call_id(id: &str) -> String {
    let clean: String = call_part(id)
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if clean.len() <= MAX_CALL_ID {
        return clean;
    }
    // FNV-1a over the original keeps two long ids from colliding after the cut.
    let hash = id.bytes().fold(0xcbf29ce484222325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100000001b3)
    });
    format!("{}_{:08x}", &clean[..MAX_CALL_ID - 9], hash as u32)
}

fn image_part(source: &MediaSource) -> Value {
    json!({"type": "image_url", "image_url": {"url": media_url(source)}})
}

fn file_part(media_type: &str, data: &str) -> Value {
    json!({"type": "file", "file": {
        "filename": "document.pdf",
        "file_data": super::data_url(media_type, data),
    }})
}

fn text_part(text: &str) -> Value {
    json!({"type": "text", "text": text})
}

/// One text part collapses to a plain string; anything richer stays an array.
fn collapse(mut parts: Vec<Value>) -> Value {
    if parts.len() == 1 && parts[0]["type"] == "text" {
        return parts.remove(0)["text"].clone();
    }
    Value::Array(parts)
}

fn push_user(messages: &mut Vec<Value>, parts: &[UserPart]) {
    let mut tool_messages = Vec::new();
    let mut tool_attachments = Vec::new();
    let mut rest = Vec::new();
    for part in parts {
        match part {
            UserPart::ToolResult {
                tool_call_id,
                content,
                ..
            } => {
                let text = tool_result_text(content);
                tool_messages.push(json!({
                    "role": "tool",
                    "tool_call_id": chat_call_id(tool_call_id),
                    "content": if text.is_empty() { EMPTY_TOOL_OUTPUT.to_string() } else { text },
                }));
                // A tool message carries text only; its images ride in a user
                // message right after the run of tool messages.
                if let ToolResultContent::Parts(inner) = content {
                    for item in inner {
                        match item {
                            ToolResultPart::Image { source, .. } => {
                                tool_attachments.push(image_part(source))
                            }
                            ToolResultPart::Document { media_type, data } => {
                                tool_attachments.push(file_part(media_type, data))
                            }
                            _ => {}
                        }
                    }
                }
            }
            UserPart::Text { text } if !text.is_empty() => rest.push(text_part(text)),
            UserPart::Text { .. } => {}
            UserPart::Image { source, .. } => rest.push(image_part(source)),
            UserPart::Document { media_type, data } => rest.push(file_part(media_type, data)),
        }
    }
    messages.extend(tool_messages);
    let mut content = Vec::new();
    if !tool_attachments.is_empty() {
        content.push(text_part("Attached image(s) from tool result:"));
        content.extend(tool_attachments);
    }
    content.extend(rest);
    if !content.is_empty() {
        messages.push(json!({"role": "user", "content": collapse(content)}));
    }
}

fn arguments_json(input: &Value) -> String {
    match input {
        Value::Object(_) => input.to_string(),
        // The stream failed to parse the model's arguments and kept the raw
        // string; sending it back as a JSON string would not be an object.
        _ => "{}".to_string(),
    }
}

fn push_assistant(messages: &mut Vec<Value>, parts: &[AssistantPart]) {
    let mut text = String::new();
    let mut calls = Vec::new();
    for part in parts {
        match part {
            AssistantPart::Text { text: t } => text.push_str(t),
            AssistantPart::ToolCall {
                id, name, input, ..
            } => calls.push(json!({
                "id": chat_call_id(id),
                "type": "function",
                "function": {"name": name, "arguments": arguments_json(input)},
            })),
            // No input channel for reasoning, and server tools are Anthropic's.
            AssistantPart::Thinking { .. }
            | AssistantPart::RedactedThinking { .. }
            | AssistantPart::ServerToolCall { .. }
            | AssistantPart::ServerToolResult { .. } => {}
        }
    }
    if text.is_empty() && calls.is_empty() {
        return;
    }
    let mut message = json!({
        "role": "assistant",
        "content": if text.is_empty() { Value::Null } else { Value::String(text) },
    });
    if !calls.is_empty() {
        message["tool_calls"] = Value::Array(calls);
    }
    messages.push(message);
}

fn tool_choice(choice: &ToolChoice) -> Option<Value> {
    match choice {
        ToolChoice::Auto => None,
        ToolChoice::Any => Some(json!("required")),
        ToolChoice::None => Some(json!("none")),
        ToolChoice::Tool { name } => Some(json!({"type": "function", "function": {"name": name}})),
    }
}

/// The `chat/completions` body for one streaming call.
pub fn build_body(target: &ResolvedTarget, request: &ModelRequest) -> Value {
    let mut messages = Vec::new();
    if let Some(system) = system_text(request) {
        messages.push(json!({"role": "system", "content": system}));
    }
    for message in &request.messages {
        match message {
            Message::User { parts } => push_user(&mut messages, parts),
            Message::Assistant { parts } => push_assistant(&mut messages, parts),
        }
    }

    let max_output = if target.max_output_tokens > 0 {
        request.max_output_tokens.min(target.max_output_tokens)
    } else {
        request.max_output_tokens
    };
    let mut body = json!({
        "model": target.model,
        "messages": messages,
        "stream": true,
        "stream_options": {"include_usage": true},
        "max_completion_tokens": max_output,
    });

    let tools: Vec<_> = super::wire_tools(request).collect();
    if !tools.is_empty() && target.capabilities.tools {
        body["tools"] = tools
            .into_iter()
            .map(|tool| {
                json!({"type": "function", "function": {
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.input_schema,
                }})
            })
            .collect();
        if let Some(choice) = tool_choice(&request.tool_choice) {
            body["tool_choice"] = choice;
        }
        if request.disable_parallel_tools {
            body["parallel_tool_calls"] = json!(false);
        }
    }
    if target.capabilities.thinking
        && let Some(effort) = reasoning_effort(request)
    {
        body["reasoning_effort"] = json!(effort);
    }
    // Reasoning models only accept their default temperature.
    if let Some(temperature) = request.temperature
        && !target.capabilities.thinking
    {
        body["temperature"] = json!(temperature);
    }
    if let Some(output) = &request.structured_output {
        body["response_format"] = json!({"type": "json_schema", "json_schema": {
            "name": "response",
            "schema": output.schema,
            "strict": false,
        }});
    }
    body
}

// ---------------------------------------------------------------------------
// Stream
// ---------------------------------------------------------------------------

fn parse_usage(usage: &Value) -> Usage {
    let prompt = super::u64_at(usage, "/prompt_tokens");
    let cached = super::u64_at(usage, "/prompt_tokens_details/cached_tokens");
    Usage {
        // Anthropic-style: `input` excludes what was served from cache, so the
        // query layer's sum of the four counters is the real context size.
        input_tokens: prompt.saturating_sub(cached),
        output_tokens: super::u64_at(usage, "/completion_tokens"),
        cache_read_tokens: cached,
        reasoning_tokens: super::u64_at(usage, "/completion_tokens_details/reasoning_tokens"),
        ..Usage::default()
    }
}

fn stop_reason(reason: &str, saw_tool_call: bool) -> StopReason {
    match reason {
        "stop" | "end" if saw_tool_call => StopReason::ToolUse,
        "stop" | "end" => StopReason::EndTurn,
        "length" => StopReason::MaxTokens,
        "tool_calls" | "function_call" => StopReason::ToolUse,
        "content_filter" => StopReason::Refusal,
        other => StopReason::Other(other.to_string()),
    }
}

#[derive(Default)]
struct ToolState {
    id: Option<String>,
    name: Option<String>,
    /// Arguments that arrived before the call could be announced.
    pending: String,
    block: Option<usize>,
}

/// Turns Chat Completions chunks into IR events. Feed it frames in order, then
/// call [`ChatStream::end`] once the connection closes.
pub struct ChatStream {
    request_id: Option<String>,
    started: bool,
    finished: bool,
    next_block: usize,
    text_block: Option<usize>,
    tools: BTreeMap<u64, ToolState>,
    stop: Option<StopReason>,
    usage: Usage,
    saw_output: bool,
}

impl ChatStream {
    pub fn new(request_id: Option<String>) -> Self {
        Self {
            request_id,
            started: false,
            finished: false,
            next_block: 0,
            text_block: None,
            tools: BTreeMap::new(),
            stop: None,
            usage: Usage::default(),
            saw_output: false,
        }
    }

    pub fn is_finished(&self) -> bool {
        self.finished
    }

    pub(super) fn partial(&self, mut error: ProviderError) -> ProviderError {
        if self.saw_output {
            error.kind = ProviderErrorKind::PartialStream;
        }
        error
    }

    fn close_text(&mut self, out: &mut Vec<StreamEventKind>) {
        if let Some(index) = self.text_block.take() {
            out.push(StreamEventKind::BlockStop { index });
        }
    }

    fn open_text(&mut self, out: &mut Vec<StreamEventKind>) -> usize {
        if let Some(index) = self.text_block {
            return index;
        }
        let index = self.next_block;
        self.next_block += 1;
        self.text_block = Some(index);
        out.push(StreamEventKind::BlockStart {
            index,
            block: BlockStart::Text,
        });
        index
    }

    /// Announce a tool call once its name is known, then flush buffered args.
    fn start_tool(&mut self, key: u64, out: &mut Vec<StreamEventKind>) {
        self.close_text(out);
        let index = self.next_block;
        let Some(state) = self.tools.get_mut(&key) else {
            return;
        };
        if state.block.is_some() {
            return;
        }
        let Some(name) = state.name.clone() else {
            return;
        };
        self.next_block += 1;
        state.block = Some(index);
        let id = state
            .id
            .clone()
            .unwrap_or_else(|| format!("call_{}", uuid::Uuid::new_v4().simple()));
        out.push(StreamEventKind::BlockStart {
            index,
            block: BlockStart::ToolCall {
                id,
                name,
                server: false,
                input: json!({}),
            },
        });
        if !state.pending.is_empty() {
            out.push(StreamEventKind::BlockDelta {
                index,
                delta: Delta::ToolInput {
                    json: std::mem::take(&mut state.pending),
                },
            });
        }
    }

    pub fn feed(&mut self, frame: &Frame) -> Result<Vec<StreamEventKind>, ProviderError> {
        let data = frame.data.trim();
        if data == "[DONE]" {
            return self.end();
        }
        let chunk: Value = serde_json::from_str(data).map_err(|_| {
            self.partial(ProviderError::new(
                ProviderErrorKind::Transport,
                "the server sent a malformed stream chunk",
            ))
        })?;
        if chunk.get("error").is_some_and(Value::is_object) {
            return Err(self.partial(super::errors::from_stream(&chunk, self.request_id.clone())));
        }

        let mut out = Vec::new();
        if !self.started {
            self.started = true;
            out.push(StreamEventKind::MessageStart {
                request_id: self.request_id.clone(),
                message_id: chunk.get("id").and_then(Value::as_str).map(str::to_string),
                model: chunk
                    .get("model")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                usage: Usage::default(),
            });
        }
        if let Some(usage) = chunk.get("usage").filter(|u| u.is_object()) {
            self.usage = parse_usage(usage);
        }
        let Some(choice) = chunk.pointer("/choices/0") else {
            return Ok(out);
        };
        let delta = choice.get("delta").unwrap_or(&Value::Null);

        // A refusal arrives in its own field and reads as text to the user.
        for field in ["content", "refusal"] {
            if let Some(text) = delta
                .get(field)
                .and_then(Value::as_str)
                .filter(|t| !t.is_empty())
            {
                let index = self.open_text(&mut out);
                self.saw_output = true;
                out.push(StreamEventKind::BlockDelta {
                    index,
                    delta: Delta::Text {
                        text: text.to_string(),
                    },
                });
            }
        }
        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for (position, call) in calls.iter().enumerate() {
                let key = call
                    .get("index")
                    .and_then(Value::as_u64)
                    .unwrap_or(position as u64);
                let state = self.tools.entry(key).or_default();
                if let Some(id) = call
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    state.id = Some(id.to_string());
                }
                if let Some(name) = call
                    .pointer("/function/name")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    state.name = Some(name.to_string());
                }
                let arguments = call
                    .pointer("/function/arguments")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                self.saw_output = true;
                match state.block {
                    Some(index) if !arguments.is_empty() => out.push(StreamEventKind::BlockDelta {
                        index,
                        delta: Delta::ToolInput {
                            json: arguments.to_string(),
                        },
                    }),
                    Some(_) => {}
                    None => {
                        state.pending.push_str(arguments);
                        self.start_tool(key, &mut out);
                    }
                }
            }
        }

        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            self.close_text(&mut out);
            let keys: Vec<u64> = self.tools.keys().copied().collect();
            for key in &keys {
                self.start_tool(*key, &mut out);
            }
            let mut blocks: Vec<usize> = Vec::new();
            for (key, state) in &self.tools {
                match state.block {
                    Some(index) => blocks.push(index),
                    None => {
                        return Err(self.partial(ProviderError::new(
                            ProviderErrorKind::PartialStream,
                            format!("the model streamed tool call #{key} without a name"),
                        )));
                    }
                }
            }
            blocks.sort_unstable();
            out.extend(
                blocks
                    .into_iter()
                    .map(|index| StreamEventKind::BlockStop { index }),
            );
            self.stop = Some(stop_reason(reason, !self.tools.is_empty()));
        }
        Ok(out)
    }

    /// The connection closed (or `[DONE]` arrived). A stream that never said why
    /// it ended was cut off.
    pub fn end(&mut self) -> Result<Vec<StreamEventKind>, ProviderError> {
        if self.finished {
            return Ok(Vec::new());
        }
        if !self.started {
            return Err(ProviderError::new(
                ProviderErrorKind::Transport,
                "the stream ended before the server sent anything",
            ));
        }
        let Some(stop_reason) = self.stop.clone() else {
            return Err(self.partial(ProviderError::new(
                ProviderErrorKind::Transport,
                "the stream ended before the model finished",
            )));
        };
        self.finished = true;
        Ok(vec![
            StreamEventKind::MessageDelta {
                stop_reason: Some(stop_reason),
                usage: self.usage,
            },
            StreamEventKind::MessageStop,
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::provider::ir::{
        CachePolicy, ClientContext, JsonSchemaOutput, ThinkingPreference, ToolSpec,
    };
    use crate::utils::model::profile::ResolvedCapabilities;

    fn target(capabilities: ResolvedCapabilities) -> ResolvedTarget {
        ResolvedTarget {
            profile: "p".into(),
            provider: "openai".into(),
            protocol: crate::services::provider::ir::Protocol::OpenaiChat,
            model: "gpt-5".into(),
            context_window: 400_000,
            max_output_tokens: 8_000,
            capabilities,
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

    fn request(messages: Vec<Message>) -> ModelRequest {
        ModelRequest {
            model: "ignored".into(),
            system: vec!["be brief".into()],
            messages,
            tools: vec![],
            tool_choice: ToolChoice::Auto,
            thinking: ThinkingPreference::Disabled,
            effort: None,
            max_output_tokens: 32_000,
            temperature: None,
            structured_output: None,
            cache: CachePolicy::default(),
            query_source: "t".into(),
            agent_id: None,
            disable_parallel_tools: false,
            discovered_tools: vec![],
            tools_pending: false,
            client: ClientContext::default(),
            extensions: vec![],
        }
    }

    fn user_text(text: &str) -> Message {
        Message::User {
            parts: vec![UserPart::Text { text: text.into() }],
        }
    }

    fn tool() -> ToolSpec {
        ToolSpec {
            name: "Read".into(),
            description: "reads".into(),
            input_schema: json!({"type": "object"}),
            strict: true,
            deferrable: false,
            is_mcp: false,
        }
    }

    #[test]
    fn body_has_the_chat_shape_and_clamps_output_to_the_profile() {
        let body = build_body(&target(all()), &request(vec![user_text("hi")]));
        assert_eq!(body["model"], "gpt-5");
        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"], json!({"include_usage": true}));
        assert_eq!(body["max_completion_tokens"], 8_000);
        assert_eq!(
            body["messages"][0],
            json!({"role": "system", "content": "be brief"})
        );
        assert_eq!(
            body["messages"][1],
            json!({"role": "user", "content": "hi"})
        );
        for absent in [
            "tools",
            "tool_choice",
            "reasoning_effort",
            "temperature",
            "response_format",
        ] {
            assert!(
                body.get(absent).is_none(),
                "{absent} must not be sent: {body}"
            );
        }
    }

    #[test]
    fn tools_choice_and_parallel_flag_are_sent_only_with_tools() {
        let mut r = request(vec![user_text("hi")]);
        r.tools = vec![tool()];
        r.tool_choice = ToolChoice::Tool {
            name: "Read".into(),
        };
        r.disable_parallel_tools = true;
        let body = build_body(&target(all()), &r);
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["function"]["name"], "Read");
        // `strict` is never forced: real tool schemas are not strict-compatible.
        assert!(body["tools"][0]["function"].get("strict").is_none());
        assert_eq!(
            body["tool_choice"],
            json!({"type": "function", "function": {"name": "Read"}})
        );
        assert_eq!(body["parallel_tool_calls"], false);

        r.tools.clear();
        let body = build_body(&target(all()), &r);
        assert!(body.get("tool_choice").is_none() && body.get("parallel_tool_calls").is_none());
    }

    #[test]
    fn thinking_effort_and_temperature_follow_the_declared_capabilities() {
        let mut r = request(vec![user_text("hi")]);
        r.thinking = ThinkingPreference::Adaptive;
        r.effort = Some(crate::services::provider::ir::Effort::Named("high".into()));
        r.temperature = Some(0.2);
        let reasoning = build_body(&target(all()), &r);
        assert_eq!(reasoning["reasoning_effort"], "high");
        assert!(reasoning.get("temperature").is_none());

        let mut plain = all();
        plain.thinking = false;
        let plain = build_body(&target(plain), &r);
        assert!(plain.get("reasoning_effort").is_none());
        assert_eq!(plain["temperature"], 0.2);
    }

    #[test]
    fn structured_output_becomes_a_json_schema_response_format() {
        let mut r = request(vec![user_text("hi")]);
        r.structured_output = Some(JsonSchemaOutput {
            schema: json!({"type": "object", "properties": {"a": {"type": "string"}}}),
        });
        let body = build_body(&target(all()), &r);
        assert_eq!(body["response_format"]["type"], "json_schema");
        assert_eq!(body["response_format"]["json_schema"]["name"], "response");
        assert_eq!(
            body["response_format"]["json_schema"]["schema"]["type"],
            "object"
        );
    }

    #[test]
    fn history_maps_tool_calls_results_and_drops_what_chat_cannot_carry() {
        let call_id = "call_abc-nyanfc-fc_123";
        let messages = vec![
            user_text("go"),
            Message::Assistant {
                parts: vec![
                    AssistantPart::Thinking {
                        text: "hm".into(),
                        signature: "sig".into(),
                        provider_state: None,
                    },
                    AssistantPart::ServerToolCall {
                        id: "srv".into(),
                        name: "web_search".into(),
                        input: json!({}),
                    },
                    AssistantPart::Text {
                        text: "reading".into(),
                    },
                    AssistantPart::ToolCall {
                        id: call_id.into(),
                        name: "Read".into(),
                        input: json!({"p": "a"}),
                        provider_state: None,
                    },
                ],
            },
            Message::User {
                parts: vec![UserPart::ToolResult {
                    tool_call_id: call_id.into(),
                    content: ToolResultContent::Parts(vec![
                        ToolResultPart::Text {
                            text: "contents".into(),
                        },
                        ToolResultPart::Image {
                            source: MediaSource::Base64 {
                                media_type: "image/png".into(),
                                data: "AAA".into(),
                            },
                            provider_state: None,
                        },
                    ]),
                    is_error: false,
                }],
            },
        ];
        let body = build_body(&target(all()), &request(messages));
        let m = body["messages"].as_array().unwrap();
        assert_eq!(m[2]["role"], "assistant");
        assert_eq!(m[2]["content"], "reading");
        assert_eq!(
            m[2]["tool_calls"][0]["id"], "call_abc",
            "composite id reduced to the call id"
        );
        assert_eq!(
            m[2]["tool_calls"][0]["function"]["arguments"],
            "{\"p\":\"a\"}"
        );
        assert!(!m[2].to_string().contains("hm") && !m[2].to_string().contains("web_search"));
        assert_eq!(
            m[3],
            json!({"role": "tool", "tool_call_id": "call_abc", "content": "contents"})
        );
        // The image rides in a user message right after the tool message.
        assert_eq!(m[4]["role"], "user");
        assert_eq!(
            m[4]["content"][1]["image_url"]["url"],
            "data:image/png;base64,AAA"
        );
    }

    #[test]
    fn an_empty_tool_result_and_an_empty_assistant_turn_are_handled() {
        let messages = vec![
            Message::Assistant { parts: vec![] },
            Message::User {
                parts: vec![UserPart::ToolResult {
                    tool_call_id: "c".into(),
                    content: ToolResultContent::Text(String::new()),
                    is_error: true,
                }],
            },
        ];
        let body = build_body(&target(all()), &request(messages));
        let m = body["messages"].as_array().unwrap();
        assert_eq!(
            m.len(),
            2,
            "system + tool; the empty assistant turn is skipped"
        );
        assert_eq!(m[1]["content"], EMPTY_TOOL_OUTPUT);
    }

    #[test]
    fn long_and_foreign_call_ids_stay_within_the_limit_and_stay_distinct() {
        let a = chat_call_id(&format!("{}A", "x".repeat(60)));
        let b = chat_call_id(&format!("{}B", "x".repeat(60)));
        assert!(a.len() <= MAX_CALL_ID && b.len() <= MAX_CALL_ID);
        assert_ne!(a, b);
        assert_eq!(chat_call_id("toolu_01.A:b"), "toolu_01_A_b");
        assert_eq!(chat_call_id("call_1"), chat_call_id("call_1"));
    }

    fn frame(value: Value) -> Frame {
        Frame {
            event: None,
            data: value.to_string(),
        }
    }

    fn run(frames: Vec<Value>) -> (Vec<StreamEventKind>, Option<ProviderError>) {
        let mut stream = ChatStream::new(Some("req_9".into()));
        let mut events = Vec::new();
        for value in frames {
            match stream.feed(&frame(value)) {
                Ok(more) => events.extend(more),
                Err(error) => return (events, Some(error)),
            }
        }
        match stream.end() {
            Ok(more) => (events.into_iter().chain(more).collect(), None),
            Err(error) => (events, Some(error)),
        }
    }

    fn chunk(delta: Value, finish: Value) -> Value {
        json!({"id": "chatcmpl-1", "model": "gpt-5", "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
    }

    #[test]
    fn text_stream_becomes_start_delta_stop_with_usage_after_the_finish_chunk() {
        let (events, error) = run(vec![
            chunk(json!({"role": "assistant", "content": ""}), Value::Null),
            chunk(json!({"content": "hel"}), Value::Null),
            chunk(json!({"content": "lo"}), Value::Null),
            chunk(json!({}), json!("stop")),
            json!({"id": "chatcmpl-1", "choices": [], "usage": {
                "prompt_tokens": 100, "completion_tokens": 20, "total_tokens": 120,
                "prompt_tokens_details": {"cached_tokens": 40},
                "completion_tokens_details": {"reasoning_tokens": 7}}}),
        ]);
        assert!(error.is_none(), "{error:?}");
        assert!(matches!(
            &events[0],
            StreamEventKind::MessageStart { request_id: Some(r), message_id: Some(m), model: Some(model), .. }
                if r == "req_9" && m == "chatcmpl-1" && model == "gpt-5"
        ));
        let text: String = events
            .iter()
            .filter_map(|e| match e {
                StreamEventKind::BlockDelta {
                    delta: Delta::Text { text },
                    ..
                } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "hello");
        assert!(events.contains(&StreamEventKind::BlockStop { index: 0 }));
        let Some(StreamEventKind::MessageDelta { stop_reason, usage }) = events.iter().rev().nth(1)
        else {
            panic!("{events:?}")
        };
        assert_eq!(*stop_reason, Some(StopReason::EndTurn));
        assert_eq!(usage.input_tokens, 60, "cached tokens are not input");
        assert_eq!(usage.cache_read_tokens, 40);
        assert_eq!(usage.output_tokens, 20);
        assert_eq!(usage.reasoning_tokens, 7);
        assert_eq!(events.last(), Some(&StreamEventKind::MessageStop));
    }

    #[test]
    fn interleaved_parallel_tool_calls_assemble_by_index() {
        let (events, error) = run(vec![
            chunk(
                json!({"tool_calls": [
                {"index": 0, "id": "call_a", "type": "function", "function": {"name": "Read", "arguments": ""}},
                {"index": 1, "id": "call_b", "type": "function", "function": {"name": "Grep", "arguments": ""}}]}),
                Value::Null,
            ),
            chunk(
                json!({"tool_calls": [{"index": 0, "function": {"arguments": "{\"p\""}}]}),
                Value::Null,
            ),
            chunk(
                json!({"tool_calls": [{"index": 1, "function": {"arguments": "{\"q\":1}"}}]}),
                Value::Null,
            ),
            chunk(
                json!({"tool_calls": [{"index": 0, "function": {"arguments": ":\"a\"}"}}]}),
                Value::Null,
            ),
            chunk(json!({}), json!("tool_calls")),
        ]);
        assert!(error.is_none(), "{error:?}");
        let mut args: BTreeMap<usize, String> = BTreeMap::new();
        for event in &events {
            if let StreamEventKind::BlockDelta {
                index,
                delta: Delta::ToolInput { json },
            } = event
            {
                args.entry(*index).or_default().push_str(json);
            }
        }
        assert_eq!(args[&0], "{\"p\":\"a\"}");
        assert_eq!(args[&1], "{\"q\":1}");
        let names: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                StreamEventKind::BlockStart {
                    block:
                        BlockStart::ToolCall {
                            id,
                            name,
                            server: false,
                            ..
                        },
                    index,
                } => Some((*index, id.clone(), name.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(
            names,
            vec![
                (0, "call_a".into(), "Read".into()),
                (1, "call_b".into(), "Grep".into())
            ]
        );
        assert!(events.contains(&StreamEventKind::BlockStop { index: 0 }));
        assert!(events.contains(&StreamEventKind::BlockStop { index: 1 }));
        assert!(events.iter().any(|e| matches!(
            e,
            StreamEventKind::MessageDelta {
                stop_reason: Some(StopReason::ToolUse),
                ..
            }
        )));
    }

    #[test]
    fn text_before_a_tool_call_is_closed_first_and_arguments_before_the_name_are_buffered() {
        let (events, error) = run(vec![
            chunk(json!({"content": "let me look"}), Value::Null),
            chunk(
                json!({"tool_calls": [{"index": 0, "function": {"arguments": "{\"p\":1}"}}]}),
                Value::Null,
            ),
            chunk(
                json!({"tool_calls": [{"index": 0, "id": "call_a", "function": {"name": "Read"}}]}),
                Value::Null,
            ),
            chunk(json!({}), json!("stop")),
        ]);
        assert!(error.is_none(), "{error:?}");
        let order: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                StreamEventKind::BlockStart { index, .. } => Some(format!("start{index}")),
                StreamEventKind::BlockStop { index } => Some(format!("stop{index}")),
                _ => None,
            })
            .collect();
        assert_eq!(order, ["start0", "stop0", "start1", "stop1"]);
        assert!(events.contains(&StreamEventKind::BlockDelta {
            index: 1,
            delta: Delta::ToolInput {
                json: "{\"p\":1}".into()
            }
        }));
        // A server that says "stop" after calling tools still means tool use.
        assert!(events.iter().any(|e| matches!(
            e,
            StreamEventKind::MessageDelta {
                stop_reason: Some(StopReason::ToolUse),
                ..
            }
        )));
    }

    #[test]
    fn finish_reasons_map_and_a_tool_call_without_a_name_is_an_error() {
        for (given, expected) in [
            ("length", StopReason::MaxTokens),
            ("content_filter", StopReason::Refusal),
            ("weird", StopReason::Other("weird".into())),
        ] {
            let (events, error) = run(vec![chunk(json!({"content": "x"}), json!(given))]);
            assert!(error.is_none());
            assert!(events.iter().any(|e| matches!(
                e,
                StreamEventKind::MessageDelta { stop_reason: Some(s), .. } if *s == expected
            )));
        }
        let (_, error) = run(vec![
            chunk(
                json!({"tool_calls": [{"index": 0, "function": {"arguments": "{}"}}]}),
                Value::Null,
            ),
            chunk(json!({}), json!("tool_calls")),
        ]);
        assert_eq!(error.unwrap().kind, ProviderErrorKind::PartialStream);
    }

    #[test]
    fn a_cut_off_or_empty_stream_is_an_error_and_output_makes_it_a_partial_stream() {
        let (_, empty) = run(vec![]);
        assert_eq!(empty.unwrap().kind, ProviderErrorKind::Transport);
        let (_, before_output) = run(vec![chunk(json!({"role": "assistant"}), Value::Null)]);
        assert_eq!(before_output.unwrap().kind, ProviderErrorKind::Transport);
        let (_, after_output) = run(vec![chunk(json!({"content": "partial"}), Value::Null)]);
        assert_eq!(after_output.unwrap().kind, ProviderErrorKind::PartialStream);
    }

    #[test]
    fn an_error_chunk_is_classified_and_after_output_it_is_partial() {
        let error_chunk = json!({"error": {"message": "slow down", "type": "rate_limit_error", "code": "rate_limit_exceeded"}});
        let (_, early) = run(vec![error_chunk.clone()]);
        assert_eq!(early.unwrap().kind, ProviderErrorKind::RateLimited);
        let (_, late) = run(vec![
            chunk(json!({"content": "x"}), Value::Null),
            error_chunk,
        ]);
        let late = late.unwrap();
        assert_eq!(late.kind, ProviderErrorKind::PartialStream);
        assert_eq!(late.message, "slow down");
    }

    #[test]
    fn done_ends_the_stream_once_and_malformed_data_is_an_error() {
        let mut stream = ChatStream::new(None);
        stream
            .feed(&frame(chunk(json!({"content": "a"}), json!("stop"))))
            .unwrap();
        let first = stream
            .feed(&Frame {
                event: None,
                data: "[DONE]".into(),
            })
            .unwrap();
        assert_eq!(first.last(), Some(&StreamEventKind::MessageStop));
        assert!(stream.is_finished());
        assert!(
            stream.end().unwrap().is_empty(),
            "a second end adds nothing"
        );
        let mut fresh = ChatStream::new(None);
        assert!(
            fresh
                .feed(&Frame {
                    event: None,
                    data: "{oops".into()
                })
                .is_err()
        );
    }
}
