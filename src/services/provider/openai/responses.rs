//! OpenAI Responses API: IR request -> wire body, wire stream -> IR events.
//!
//! @nyan: pure (no I/O), like `chat`. Two things are specific to this protocol:
//!
//! - **Reasoning items round-trip.** The server returns a reasoning item with
//!   `encrypted_content`; with `store: false` it must be sent back verbatim for
//!   the model to keep its chain of thought across tool calls. It is kept as a
//!   thinking block whose signature is a tagged [`ProviderState`], so only this
//!   protocol replays it and Anthropic/Chat skip it.
//! - **Function calls have two ids.** `call_id` is what the output must echo;
//!   the `fc_...` item id pairs the call with its reasoning item. The transcript
//!   has one id slot, so both are kept, joined (see [`super::join_call_id`]).

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::transport::Frame;
use super::{
    EMPTY_TOOL_OUTPUT, call_part, item_part, join_call_id, media_url, reasoning_effort,
    system_text, tool_result_text,
};
use crate::services::provider::adapter::ResolvedTarget;
use crate::services::provider::ir::{
    AssistantPart, BlockStart, Delta, Message, ModelRequest, Protocol, ProviderError,
    ProviderErrorKind, ProviderState, StopReason, StreamEventKind, ToolChoice, ToolResultContent,
    ToolResultPart, Usage, UserPart,
};

/// The ChatGPT sign-in backend rejects what the public API accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// `api.openai.com` or a Responses-compatible endpoint.
    Api,
    /// `chatgpt.com/backend-api/codex`.
    Codex,
}

const MAX_ID: usize = 64;

fn state_of(data: Value) -> ProviderState {
    ProviderState {
        protocol: Protocol::OpenaiResponses,
        data,
    }
}

fn own_state(state: &Option<ProviderState>) -> Option<&Value> {
    state
        .as_ref()
        .filter(|s| s.protocol == Protocol::OpenaiResponses)
        .map(|s| &s.data)
}

/// An item id the server will accept back: `fc_`-prefixed and at most 64 bytes.
/// Ids from another provider fail that, and sending them makes the server
/// reject the pairing with the reasoning item, so they are not sent at all.
fn replayable_item_id(id: &str) -> Option<&str> {
    (id.starts_with("fc_") && id.len() <= MAX_ID).then_some(id)
}

fn user_content(parts: &[UserPart]) -> Vec<Value> {
    parts
        .iter()
        .filter_map(|part| match part {
            UserPart::Text { text } if !text.is_empty() => {
                Some(json!({"type": "input_text", "text": text}))
            }
            UserPart::Image { source, .. } => Some(json!({
                "type": "input_image", "detail": "auto", "image_url": media_url(source),
            })),
            UserPart::Document { media_type, data } => Some(json!({
                "type": "input_file",
                "filename": "document.pdf",
                "file_data": super::data_url(media_type, data),
            })),
            _ => None,
        })
        .collect()
}

fn tool_output(content: &ToolResultContent) -> Value {
    let text = tool_result_text(content);
    let images: Vec<Value> = match content {
        ToolResultContent::Parts(parts) => parts
            .iter()
            .filter_map(|part| match part {
                ToolResultPart::Image { source, .. } => Some(json!({
                    "type": "input_image", "detail": "auto", "image_url": media_url(source),
                })),
                _ => None,
            })
            .collect(),
        ToolResultContent::Text(_) => Vec::new(),
    };
    if images.is_empty() {
        return Value::String(if text.is_empty() {
            EMPTY_TOOL_OUTPUT.to_string()
        } else {
            text
        });
    }
    let mut parts = vec![json!({
        "type": "input_text",
        "text": if text.is_empty() { "(see attached image)".to_string() } else { text },
    })];
    parts.extend(images);
    Value::Array(parts)
}

fn push_assistant(input: &mut Vec<Value>, parts: &[AssistantPart], backend: Backend) {
    let mut text = String::new();
    let flush = |input: &mut Vec<Value>, text: &mut String| {
        if !text.is_empty() {
            input.push(json!({
                "type": "message",
                "role": "assistant",
                "status": "completed",
                "content": [{"type": "output_text", "text": std::mem::take(text), "annotations": []}],
            }));
        }
    };
    for part in parts {
        match part {
            AssistantPart::Text { text: t } => text.push_str(t),
            AssistantPart::Thinking { provider_state, .. }
            | AssistantPart::RedactedThinking { provider_state, .. } => {
                // Only this protocol's own reasoning item goes back, whole.
                if let Some(item) = own_state(provider_state) {
                    flush(input, &mut text);
                    input.push(item.clone());
                }
            }
            AssistantPart::ToolCall {
                id,
                name,
                input: args,
                ..
            } => {
                flush(input, &mut text);
                let mut item = json!({
                    "type": "function_call",
                    "call_id": call_part(id),
                    "name": name,
                    "arguments": match args {
                        Value::Object(_) => args.to_string(),
                        _ => "{}".to_string(),
                    },
                });
                if backend == Backend::Api
                    && let Some(item_id) = item_part(id).and_then(replayable_item_id)
                {
                    item["id"] = json!(item_id);
                }
                input.push(item);
            }
            AssistantPart::ServerToolCall { .. } | AssistantPart::ServerToolResult { .. } => {}
        }
    }
    flush(input, &mut text);
}

fn tool_choice(choice: &ToolChoice) -> Value {
    match choice {
        ToolChoice::Auto => json!("auto"),
        ToolChoice::Any => json!("required"),
        ToolChoice::None => json!("none"),
        ToolChoice::Tool { name } => json!({"type": "function", "name": name}),
    }
}

/// The `responses` body for one streaming call.
pub fn build_body(target: &ResolvedTarget, request: &ModelRequest, backend: Backend) -> Value {
    let mut input = Vec::new();
    for message in &request.messages {
        match message {
            Message::User { parts } => {
                // Results first, so each output sits right after its call.
                for part in parts {
                    if let UserPart::ToolResult {
                        tool_call_id,
                        content,
                        ..
                    } = part
                    {
                        input.push(json!({
                            "type": "function_call_output",
                            "call_id": call_part(tool_call_id),
                            "output": tool_output(content),
                        }));
                    }
                }
                let content = user_content(parts);
                if !content.is_empty() {
                    input.push(json!({"type": "message", "role": "user", "content": content}));
                }
            }
            Message::Assistant { parts } => push_assistant(&mut input, parts, backend),
        }
    }

    let mut body = json!({
        "model": target.model,
        "input": input,
        "stream": true,
        "store": false,
        // Without this the server drops the reasoning chain it would need back.
        "include": ["reasoning.encrypted_content"],
    });
    // The ChatGPT backend takes the system prompt as `instructions` and never
    // an empty one.
    match (system_text(request), backend) {
        (Some(system), _) => body["instructions"] = json!(system),
        (None, Backend::Codex) => body["instructions"] = json!("You are a helpful assistant."),
        (None, Backend::Api) => {}
    }
    // `max_output_tokens` and `temperature` are refused by the ChatGPT backend.
    if backend == Backend::Api {
        let ceiling = if target.max_output_tokens > 0 {
            request.max_output_tokens.min(target.max_output_tokens)
        } else {
            request.max_output_tokens
        };
        body["max_output_tokens"] = json!(ceiling.max(16));
        if let Some(temperature) = request.temperature
            && !target.capabilities.thinking
        {
            body["temperature"] = json!(temperature);
        }
    }

    let tools: Vec<_> = super::wire_tools(request).collect();
    if !tools.is_empty() && target.capabilities.tools {
        body["tools"] = tools
            .into_iter()
            .map(|tool| {
                // `strict` is explicit: omitting it means "strict" on this API,
                // and real tool schemas are not strict-compatible.
                json!({
                    "type": "function",
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.input_schema,
                    "strict": false,
                })
            })
            .collect();
        body["tool_choice"] = tool_choice(&request.tool_choice);
        body["parallel_tool_calls"] = json!(!request.disable_parallel_tools);
    }
    if target.capabilities.thinking {
        if let Some(effort) = reasoning_effort(request) {
            body["reasoning"] = json!({"effort": effort, "summary": "auto"});
        }
    }
    if let Some(output) = &request.structured_output {
        body["text"] = json!({"format": {
            "type": "json_schema",
            "name": "response",
            "schema": output.schema,
            "strict": false,
        }});
    }
    if backend == Backend::Codex {
        body["prompt_cache_key"] = json!(crate::bootstrap::state::get_session_id());
    }
    body
}

// ---------------------------------------------------------------------------
// Stream
// ---------------------------------------------------------------------------

fn parse_usage(usage: &Value) -> Usage {
    let input = super::u64_at(usage, "/input_tokens");
    let cached = super::u64_at(usage, "/input_tokens_details/cached_tokens");
    Usage {
        input_tokens: input.saturating_sub(cached),
        output_tokens: super::u64_at(usage, "/output_tokens"),
        cache_read_tokens: cached,
        reasoning_tokens: super::u64_at(usage, "/output_tokens_details/reasoning_tokens"),
        ..Usage::default()
    }
}

/// The call this item belongs to, in the transcript's composite form.
fn composite_id(item: &Value) -> String {
    let call_id = item
        .get("call_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match item.get("id").and_then(Value::as_str) {
        Some(item_id) if !item_id.is_empty() => join_call_id(call_id, item_id),
        _ => call_id.to_string(),
    }
}

fn stop_from(response: &Value, saw_call: bool) -> Result<StopReason, ProviderError> {
    let status = response
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("completed");
    match status {
        "completed" | "in_progress" | "queued" => Ok(if saw_call {
            StopReason::ToolUse
        } else {
            StopReason::EndTurn
        }),
        "incomplete" => match response
            .pointer("/incomplete_details/reason")
            .and_then(Value::as_str)
        {
            Some("max_output_tokens") => Ok(StopReason::MaxTokens),
            Some("content_filter") => Ok(StopReason::Refusal),
            Some(other) => Ok(StopReason::Other(other.to_string())),
            None => Ok(StopReason::Other("incomplete".to_string())),
        },
        // "failed" and "cancelled" carry their own error; the caller reads it.
        other => Ok(StopReason::Other(other.to_string())),
    }
}

struct Slot {
    /// Text and reasoning blocks open on their first content, so an item that
    /// says nothing leaves no empty block in the transcript. A call opens at
    /// once: its name is known and the tool can start early.
    block: Option<usize>,
    kind: SlotKind,
    /// Text or arguments already sent, to tell a `.done` from new content.
    sent: String,
}

#[derive(PartialEq, Eq)]
enum SlotKind {
    Text,
    Reasoning,
    Call,
}

/// Turns Responses SSE events into IR events.
pub struct ResponsesStream {
    request_id: Option<String>,
    started: bool,
    finished: bool,
    next_block: usize,
    slots: BTreeMap<u64, Slot>,
    saw_call: bool,
    saw_output: bool,
}

impl ResponsesStream {
    pub fn new(request_id: Option<String>) -> Self {
        Self {
            request_id,
            started: false,
            finished: false,
            next_block: 0,
            slots: BTreeMap::new(),
            saw_call: false,
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

    fn start(&mut self, response: Option<&Value>, out: &mut Vec<StreamEventKind>) {
        if self.started {
            return;
        }
        self.started = true;
        out.push(StreamEventKind::MessageStart {
            request_id: self.request_id.clone(),
            message_id: response
                .and_then(|r| r.get("id"))
                .and_then(Value::as_str)
                .map(str::to_string),
            model: response
                .and_then(|r| r.get("model"))
                .and_then(Value::as_str)
                .map(str::to_string),
            usage: Usage::default(),
        });
    }

    fn take_block_index(&mut self) -> usize {
        let index = self.next_block;
        self.next_block += 1;
        index
    }

    fn add_slot(
        &mut self,
        key: u64,
        kind: SlotKind,
        eager: Option<BlockStart>,
        out: &mut Vec<StreamEventKind>,
    ) {
        let block = eager.map(|block| {
            let index = self.take_block_index();
            out.push(StreamEventKind::BlockStart { index, block });
            index
        });
        self.slots.insert(
            key,
            Slot {
                block,
                kind,
                sent: String::new(),
            },
        );
    }

    /// The block index of a slot, opening its block first if it has none yet.
    fn ensure_block(&mut self, key: u64, out: &mut Vec<StreamEventKind>) -> Option<usize> {
        let slot = self.slots.get(&key)?;
        if let Some(index) = slot.block {
            return Some(index);
        }
        let block = match slot.kind {
            SlotKind::Text => BlockStart::Text,
            SlotKind::Reasoning => BlockStart::Thinking,
            SlotKind::Call => return None,
        };
        let index = self.take_block_index();
        out.push(StreamEventKind::BlockStart { index, block });
        self.slots.get_mut(&key)?.block = Some(index);
        Some(index)
    }

    fn delta(&mut self, key: u64, delta: Delta, out: &mut Vec<StreamEventKind>) {
        let Some(index) = self.ensure_block(key, out) else {
            return;
        };
        if let Some(slot) = self.slots.get_mut(&key) {
            match &delta {
                Delta::Text { text } | Delta::Thinking { text } => slot.sent.push_str(text),
                Delta::ToolInput { json } => slot.sent.push_str(json),
                Delta::Signature { .. } => {}
            }
        }
        self.saw_output = true;
        out.push(StreamEventKind::BlockDelta { index, delta });
    }

    pub fn feed(&mut self, frame: &Frame) -> Result<Vec<StreamEventKind>, ProviderError> {
        let data = frame.data.trim();
        if data == "[DONE]" {
            return Ok(Vec::new());
        }
        let event: Value = serde_json::from_str(data).map_err(|_| {
            self.partial(ProviderError::new(
                ProviderErrorKind::Transport,
                "the server sent a malformed stream event",
            ))
        })?;
        let kind = event
            .get("type")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| frame.event.clone())
            .unwrap_or_default();
        let key = event
            .get("output_index")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let mut out = Vec::new();

        match kind.as_str() {
            "response.created" | "response.in_progress" => {
                self.start(event.get("response"), &mut out);
            }
            "response.output_item.added" => {
                self.start(None, &mut out);
                let item = event.get("item").unwrap_or(&Value::Null);
                match item.get("type").and_then(Value::as_str) {
                    Some("message") => self.add_slot(key, SlotKind::Text, None, &mut out),
                    Some("reasoning") => self.add_slot(key, SlotKind::Reasoning, None, &mut out),
                    Some("function_call") => {
                        self.saw_call = true;
                        self.saw_output = true;
                        self.add_slot(
                            key,
                            SlotKind::Call,
                            Some(BlockStart::ToolCall {
                                id: composite_id(item),
                                name: item
                                    .get("name")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .to_string(),
                                server: false,
                                input: json!({}),
                            }),
                            &mut out,
                        );
                    }
                    _ => {}
                }
            }
            "response.output_text.delta" | "response.refusal.delta" => {
                if let Some(text) = event
                    .get("delta")
                    .and_then(Value::as_str)
                    .filter(|t| !t.is_empty())
                {
                    self.delta(
                        key,
                        Delta::Text {
                            text: text.to_string(),
                        },
                        &mut out,
                    );
                }
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                if let Some(text) = event
                    .get("delta")
                    .and_then(Value::as_str)
                    .filter(|t| !t.is_empty())
                {
                    self.delta(
                        key,
                        Delta::Thinking {
                            text: text.to_string(),
                        },
                        &mut out,
                    );
                }
            }
            // Summary parts are separate paragraphs of one block.
            "response.reasoning_summary_part.added" => {
                if self.slots.get(&key).is_some_and(|s| !s.sent.is_empty()) {
                    self.delta(
                        key,
                        Delta::Thinking {
                            text: "\n\n".into(),
                        },
                        &mut out,
                    );
                }
            }
            "response.function_call_arguments.delta" => {
                if let Some(json) = event
                    .get("delta")
                    .and_then(Value::as_str)
                    .filter(|t| !t.is_empty())
                {
                    self.delta(
                        key,
                        Delta::ToolInput {
                            json: json.to_string(),
                        },
                        &mut out,
                    );
                }
            }
            "response.output_item.done" => {
                let item = event.get("item").unwrap_or(&Value::Null);
                self.finish_slot(key, item, &mut out);
            }
            "response.completed" | "response.incomplete" | "response.done" => {
                let response = event.get("response").unwrap_or(&Value::Null);
                self.start(Some(response), &mut out);
                if !self.slots.is_empty() {
                    return Err(self.partial(ProviderError::new(
                        ProviderErrorKind::Transport,
                        "the response ended with an unfinished output item",
                    )));
                }
                let usage = response.get("usage").map(parse_usage).unwrap_or_default();
                let stop = stop_from(response, self.saw_call)?;
                self.finished = true;
                out.push(StreamEventKind::MessageDelta {
                    stop_reason: Some(stop),
                    usage,
                });
                out.push(StreamEventKind::MessageStop);
            }
            "response.failed" => {
                let response = event.get("response").unwrap_or(&Value::Null);
                let body = json!({"error": response.get("error").cloned().unwrap_or(Value::Null)});
                return Err(
                    self.partial(super::errors::from_stream(&body, self.request_id.clone()))
                );
            }
            "error" => {
                return Err(
                    self.partial(super::errors::from_stream(&event, self.request_id.clone()))
                );
            }
            _ => {}
        }
        Ok(out)
    }

    fn finish_slot(&mut self, key: u64, item: &Value, out: &mut Vec<StreamEventKind>) {
        let Some(slot) = self.slots.get(&key) else {
            return;
        };
        let (kind, sent) = (
            match slot.kind {
                SlotKind::Text => SlotKind::Text,
                SlotKind::Reasoning => SlotKind::Reasoning,
                SlotKind::Call => SlotKind::Call,
            },
            slot.sent.clone(),
        );
        match kind {
            SlotKind::Text => {
                // A part the deltas never carried (a refusal sent whole, or a
                // proxy that streams only the final item).
                let full: String = item
                    .get("content")
                    .and_then(Value::as_array)
                    .map(|parts| {
                        parts
                            .iter()
                            .filter_map(|p| {
                                p.get("text")
                                    .or_else(|| p.get("refusal"))
                                    .and_then(Value::as_str)
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                if let Some(rest) = full.strip_prefix(sent.as_str()).filter(|r| !r.is_empty()) {
                    self.delta(
                        key,
                        Delta::Text {
                            text: rest.to_string(),
                        },
                        out,
                    );
                }
            }
            SlotKind::Reasoning => {
                // The whole item is the replay state; the summary is for display.
                // With `store: false` the server rejects a function call whose
                // reasoning item is missing, so the state is kept even when the
                // model wrote no summary: that reasoning is withheld, not absent.
                let signature = state_of(item.clone()).to_signature();
                self.saw_output = true;
                match self.slots.get(&key).and_then(|s| s.block) {
                    Some(index) => out.push(StreamEventKind::BlockDelta {
                        index,
                        delta: Delta::Signature { signature },
                    }),
                    None => {
                        let index = self.take_block_index();
                        out.push(StreamEventKind::BlockStart {
                            index,
                            block: BlockStart::RedactedThinking { data: signature },
                        });
                        self.slots.get_mut(&key).map(|s| s.block = Some(index));
                    }
                }
            }
            SlotKind::Call => {
                // `.done` carries the complete arguments; trust them over the
                // fragments if the two disagree about what was sent.
                let full = item
                    .get("arguments")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if let (Some(rest), Some(index)) = (
                    full.strip_prefix(sent.as_str()).filter(|r| !r.is_empty()),
                    self.slots.get(&key).and_then(|s| s.block),
                ) {
                    out.push(StreamEventKind::BlockDelta {
                        index,
                        delta: Delta::ToolInput {
                            json: rest.to_string(),
                        },
                    });
                }
            }
        }
        if let Some(index) = self.slots.remove(&key).and_then(|s| s.block) {
            out.push(StreamEventKind::BlockStop { index });
        }
    }

    /// The connection closed. Only `response.completed` ends a message.
    pub fn end(&mut self) -> Result<Vec<StreamEventKind>, ProviderError> {
        if self.finished {
            return Ok(Vec::new());
        }
        Err(self.partial(ProviderError::new(
            ProviderErrorKind::Transport,
            "the stream ended before the response completed",
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::provider::ir::{
        CachePolicy, ClientContext, Effort, JsonSchemaOutput, MediaSource, ThinkingPreference,
        ToolSpec,
    };
    use crate::utils::model::profile::ResolvedCapabilities;

    fn target(capabilities: ResolvedCapabilities) -> ResolvedTarget {
        ResolvedTarget {
            profile: "p".into(),
            provider: "openai".into(),
            protocol: Protocol::OpenaiResponses,
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
    fn api_body_has_the_responses_shape() {
        let body = build_body(
            &target(all()),
            &request(vec![user_text("hi")]),
            Backend::Api,
        );
        assert_eq!(body["model"], "gpt-5");
        assert_eq!(body["stream"], true);
        assert_eq!(body["store"], false);
        assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
        assert_eq!(body["instructions"], "be brief");
        assert_eq!(body["max_output_tokens"], 8_000);
        assert_eq!(
            body["input"][0],
            json!({"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]})
        );
        for absent in [
            "tools",
            "reasoning",
            "text",
            "temperature",
            "prompt_cache_key",
        ] {
            assert!(body.get(absent).is_none(), "{absent}: {body}");
        }
    }

    #[test]
    fn the_chatgpt_backend_gets_instructions_but_not_output_limits_or_temperature() {
        let mut r = request(vec![user_text("hi")]);
        r.system.clear();
        r.temperature = Some(0.3);
        let body = build_body(&target(ResolvedCapabilities::default()), &r, Backend::Codex);
        assert_eq!(body["instructions"], "You are a helpful assistant.");
        assert!(body.get("max_output_tokens").is_none() && body.get("temperature").is_none());
        assert_eq!(body["store"], false);
        assert!(body["prompt_cache_key"].is_string());
    }

    #[test]
    fn tools_are_explicitly_non_strict_and_reasoning_follows_the_capability() {
        let mut r = request(vec![user_text("hi")]);
        r.tools = vec![tool()];
        r.tool_choice = ToolChoice::Any;
        r.disable_parallel_tools = true;
        r.thinking = ThinkingPreference::Adaptive;
        r.effort = Some(Effort::Named("medium".into()));
        let body = build_body(&target(all()), &r, Backend::Api);
        assert_eq!(body["tools"][0]["strict"], false);
        assert_eq!(body["tools"][0]["name"], "Read");
        assert_eq!(body["tool_choice"], "required");
        assert_eq!(body["parallel_tool_calls"], false);
        assert_eq!(
            body["reasoning"],
            json!({"effort": "medium", "summary": "auto"})
        );

        let mut no_tools = all();
        no_tools.tools = false;
        no_tools.thinking = false;
        let body = build_body(&target(no_tools), &r, Backend::Api);
        assert!(body.get("tools").is_none() && body.get("reasoning").is_none());
    }

    #[test]
    fn structured_output_goes_in_text_format() {
        let mut r = request(vec![user_text("hi")]);
        r.structured_output = Some(JsonSchemaOutput {
            schema: json!({"type": "object"}),
        });
        let body = build_body(&target(all()), &r, Backend::Api);
        assert_eq!(body["text"]["format"]["type"], "json_schema");
        assert_eq!(body["text"]["format"]["name"], "response");
        assert_eq!(body["text"]["format"]["schema"], json!({"type": "object"}));
    }

    fn reasoning_item() -> Value {
        json!({"type": "reasoning", "id": "rs_1", "summary": [], "encrypted_content": "ENC"})
    }

    #[test]
    fn history_replays_own_reasoning_whole_pairs_call_ids_and_places_outputs() {
        let composite = join_call_id("call_1", "fc_9");
        let messages = vec![
            user_text("go"),
            Message::Assistant {
                parts: vec![
                    AssistantPart::Thinking {
                        text: "summary".into(),
                        signature: String::new(),
                        provider_state: Some(state_of(reasoning_item())),
                    },
                    // Another protocol's state must not leak in.
                    AssistantPart::Thinking {
                        text: "other".into(),
                        signature: String::new(),
                        provider_state: Some(ProviderState {
                            protocol: Protocol::AnthropicMessages,
                            data: json!({"type": "thinking", "signature": "S"}),
                        }),
                    },
                    AssistantPart::Text {
                        text: "reading".into(),
                    },
                    AssistantPart::ToolCall {
                        id: composite.clone(),
                        name: "Read".into(),
                        input: json!({"p": "a"}),
                        provider_state: None,
                    },
                ],
            },
            Message::User {
                parts: vec![UserPart::ToolResult {
                    tool_call_id: composite,
                    content: ToolResultContent::Text("contents".into()),
                    is_error: false,
                }],
            },
        ];
        let body = build_body(&target(all()), &request(messages), Backend::Api);
        let input = body["input"].as_array().unwrap();
        assert_eq!(
            input[1],
            reasoning_item(),
            "reasoning item replayed verbatim"
        );
        assert_eq!(input[2]["type"], "message");
        assert_eq!(input[2]["content"][0]["text"], "reading");
        assert_eq!(
            input[3],
            json!({"type": "function_call", "call_id": "call_1", "id": "fc_9", "name": "Read", "arguments": "{\"p\":\"a\"}"})
        );
        assert_eq!(
            input[4],
            json!({"type": "function_call_output", "call_id": "call_1", "output": "contents"})
        );
        assert!(!body.to_string().contains("\"S\""), "{body}");
    }

    #[test]
    fn foreign_and_oversized_item_ids_are_not_sent_and_the_chatgpt_backend_never_gets_one() {
        let call = |id: &str| Message::Assistant {
            parts: vec![AssistantPart::ToolCall {
                id: id.into(),
                name: "Read".into(),
                input: json!({}),
                provider_state: None,
            }],
        };
        for id in [
            join_call_id("call_1", "msg_9"),
            join_call_id("call_1", &format!("fc_{}", "x".repeat(70))),
            "toolu_01ABC".to_string(),
        ] {
            let body = build_body(&target(all()), &request(vec![call(&id)]), Backend::Api);
            assert!(body["input"][0].get("id").is_none(), "{id}: {body}");
        }
        let body = build_body(
            &target(all()),
            &request(vec![call(&join_call_id("call_1", "fc_9"))]),
            Backend::Codex,
        );
        assert!(body["input"][0].get("id").is_none());
        assert_eq!(body["input"][0]["call_id"], "call_1");
    }

    #[test]
    fn tool_result_images_stay_inside_the_output_and_empty_output_gets_a_placeholder() {
        let messages = vec![Message::User {
            parts: vec![
                UserPart::ToolResult {
                    tool_call_id: "c1".into(),
                    content: ToolResultContent::Parts(vec![ToolResultPart::Image {
                        source: MediaSource::Base64 {
                            media_type: "image/png".into(),
                            data: "AAA".into(),
                        },
                        provider_state: None,
                    }]),
                    is_error: false,
                },
                UserPart::ToolResult {
                    tool_call_id: "c2".into(),
                    content: ToolResultContent::Text(String::new()),
                    is_error: false,
                },
            ],
        }];
        let body = build_body(&target(all()), &request(messages), Backend::Api);
        let out = &body["input"];
        assert_eq!(out[0]["output"][0]["text"], "(see attached image)");
        assert_eq!(
            out[0]["output"][1]["image_url"],
            "data:image/png;base64,AAA"
        );
        assert_eq!(out[1]["output"], EMPTY_TOOL_OUTPUT);
    }

    fn frame(value: Value) -> Frame {
        Frame {
            event: None,
            data: value.to_string(),
        }
    }

    fn run(events: Vec<Value>) -> (Vec<StreamEventKind>, Option<ProviderError>) {
        let mut stream = ResponsesStream::new(Some("req_9".into()));
        let mut out = Vec::new();
        for event in events {
            match stream.feed(&frame(event)) {
                Ok(more) => out.extend(more),
                Err(error) => return (out, Some(error)),
            }
        }
        match stream.end() {
            Ok(more) => (out.into_iter().chain(more).collect(), None),
            Err(error) => (out, Some(error)),
        }
    }

    fn created() -> Value {
        json!({"type": "response.created", "response": {"id": "resp_1", "model": "gpt-5"}})
    }

    fn completed(status: &str, extra: Value) -> Value {
        let mut response = json!({"id": "resp_1", "status": status, "usage": {
            "input_tokens": 100, "output_tokens": 30, "total_tokens": 130,
            "input_tokens_details": {"cached_tokens": 40},
            "output_tokens_details": {"reasoning_tokens": 12}}});
        if let (Some(target), Some(extra)) = (response.as_object_mut(), extra.as_object()) {
            target.extend(extra.clone());
        }
        json!({"type": "response.completed", "response": response})
    }

    #[test]
    fn reasoning_text_and_a_call_stream_in_order_with_replay_state_and_composite_ids() {
        let reasoning = reasoning_item();
        let (events, error) = run(vec![
            created(),
            json!({"type": "response.output_item.added", "output_index": 0, "item": {"type": "reasoning", "id": "rs_1", "summary": []}}),
            json!({"type": "response.reasoning_summary_part.added", "output_index": 0, "summary_index": 0}),
            json!({"type": "response.reasoning_summary_text.delta", "output_index": 0, "delta": "think"}),
            json!({"type": "response.reasoning_summary_part.added", "output_index": 0, "summary_index": 1}),
            json!({"type": "response.reasoning_summary_text.delta", "output_index": 0, "delta": "more"}),
            json!({"type": "response.output_item.done", "output_index": 0, "item": reasoning}),
            json!({"type": "response.output_item.added", "output_index": 1, "item": {"type": "message", "id": "msg_1", "content": []}}),
            json!({"type": "response.output_text.delta", "output_index": 1, "delta": "I will "}),
            json!({"type": "response.output_text.delta", "output_index": 1, "delta": "read"}),
            json!({"type": "response.output_item.done", "output_index": 1, "item": {"type": "message", "content": [{"type": "output_text", "text": "I will read"}]}}),
            json!({"type": "response.output_item.added", "output_index": 2, "item": {"type": "function_call", "id": "fc_7", "call_id": "call_7", "name": "Read", "arguments": ""}}),
            json!({"type": "response.function_call_arguments.delta", "output_index": 2, "delta": "{\"p\":"}),
            json!({"type": "response.function_call_arguments.delta", "output_index": 2, "delta": "1}"}),
            json!({"type": "response.output_item.done", "output_index": 2, "item": {"type": "function_call", "id": "fc_7", "call_id": "call_7", "name": "Read", "arguments": "{\"p\":1}"}}),
            completed("completed", json!({})),
        ]);
        assert!(error.is_none(), "{error:?}");
        assert!(matches!(
            &events[0],
            StreamEventKind::MessageStart { request_id: Some(r), message_id: Some(m), .. } if r == "req_9" && m == "resp_1"
        ));
        let thinking: String = events
            .iter()
            .filter_map(|e| match e {
                StreamEventKind::BlockDelta {
                    delta: Delta::Thinking { text },
                    ..
                } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(thinking, "think\n\nmore");
        let signature = events
            .iter()
            .find_map(|e| match e {
                StreamEventKind::BlockDelta {
                    delta: Delta::Signature { signature },
                    index: 0,
                } => Some(signature.clone()),
                _ => None,
            })
            .expect("reasoning replay state");
        let restored = ProviderState::from_signature(&signature).unwrap();
        assert_eq!(restored.protocol, Protocol::OpenaiResponses);
        assert_eq!(restored.data, reasoning_item());
        assert!(events.contains(&StreamEventKind::BlockStart {
            index: 2,
            block: BlockStart::ToolCall {
                id: join_call_id("call_7", "fc_7"),
                name: "Read".into(),
                server: false,
                input: json!({}),
            }
        }));
        let args: String = events
            .iter()
            .filter_map(|e| match e {
                StreamEventKind::BlockDelta {
                    index: 2,
                    delta: Delta::ToolInput { json },
                } => Some(json.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(args, "{\"p\":1}");
        let Some(StreamEventKind::MessageDelta { stop_reason, usage }) = events.iter().rev().nth(1)
        else {
            panic!("{events:?}")
        };
        assert_eq!(*stop_reason, Some(StopReason::ToolUse));
        assert_eq!(usage.input_tokens, 60);
        assert_eq!(usage.cache_read_tokens, 40);
        assert_eq!(usage.output_tokens, 30);
        assert_eq!(usage.reasoning_tokens, 12);
    }

    #[test]
    fn reasoning_with_no_summary_is_kept_as_withheld_thinking_and_an_empty_message_leaves_no_block()
    {
        let (events, error) = run(vec![
            created(),
            json!({"type": "response.output_item.added", "output_index": 0, "item": {"type": "reasoning", "id": "rs_1", "summary": []}}),
            json!({"type": "response.output_item.done", "output_index": 0, "item": reasoning_item()}),
            json!({"type": "response.output_item.added", "output_index": 1, "item": {"type": "message", "content": []}}),
            json!({"type": "response.output_item.done", "output_index": 1, "item": {"type": "message", "content": []}}),
            json!({"type": "response.output_item.added", "output_index": 2, "item": {"type": "function_call", "id": "fc_2", "call_id": "call_2", "name": "Read", "arguments": ""}}),
            json!({"type": "response.output_item.done", "output_index": 2, "item": {"type": "function_call", "id": "fc_2", "call_id": "call_2", "name": "Read", "arguments": "{}"}}),
            completed("completed", json!({})),
        ]);
        assert!(error.is_none(), "{error:?}");
        let starts: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                StreamEventKind::BlockStart { index, block } => Some((*index, block.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(
            starts.len(),
            2,
            "reasoning and the call; the silent message has no block: {starts:?}"
        );
        let BlockStart::RedactedThinking { data } = &starts[0].1 else {
            panic!("{starts:?}")
        };
        assert_eq!(
            ProviderState::from_signature(data).unwrap().data,
            reasoning_item(),
            "the whole item survives so the next request can send it back"
        );
        assert!(matches!(starts[1].1, BlockStart::ToolCall { .. }));
        // Every opened block is closed exactly once.
        for (index, _) in &starts {
            let stops = events
                .iter()
                .filter(|e| **e == StreamEventKind::BlockStop { index: *index })
                .count();
            assert_eq!(stops, 1, "block {index}");
        }
    }

    #[test]
    fn arguments_missing_from_the_deltas_are_filled_from_the_done_item() {
        let (events, error) = run(vec![
            created(),
            json!({"type": "response.output_item.added", "output_index": 0, "item": {"type": "function_call", "call_id": "c", "name": "Read"}}),
            json!({"type": "response.function_call_arguments.delta", "output_index": 0, "delta": "{\"a\""}),
            json!({"type": "response.output_item.done", "output_index": 0, "item": {"type": "function_call", "call_id": "c", "name": "Read", "arguments": "{\"a\":1}"}}),
            completed("completed", json!({})),
        ]);
        assert!(error.is_none());
        let args: String = events
            .iter()
            .filter_map(|e| match e {
                StreamEventKind::BlockDelta {
                    delta: Delta::ToolInput { json },
                    ..
                } => Some(json.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(args, "{\"a\":1}");
    }

    #[test]
    fn a_refusal_streams_as_text_and_status_maps_to_the_stop_reason() {
        let (events, _) = run(vec![
            created(),
            json!({"type": "response.output_item.added", "output_index": 0, "item": {"type": "message"}}),
            json!({"type": "response.refusal.delta", "output_index": 0, "delta": "I can't"}),
            json!({"type": "response.output_item.done", "output_index": 0, "item": {"type": "message", "content": [{"type": "refusal", "refusal": "I can't"}]}}),
            completed("completed", json!({})),
        ]);
        assert!(events.contains(&StreamEventKind::BlockDelta {
            index: 0,
            delta: Delta::Text {
                text: "I can't".into()
            }
        }));
        for (extra, expected) in [
            (
                json!({"status": "incomplete", "incomplete_details": {"reason": "max_output_tokens"}}),
                StopReason::MaxTokens,
            ),
            (
                json!({"status": "incomplete", "incomplete_details": {"reason": "content_filter"}}),
                StopReason::Refusal,
            ),
            (
                json!({"status": "incomplete"}),
                StopReason::Other("incomplete".into()),
            ),
        ] {
            let (events, error) = run(vec![created(), completed("completed", extra)]);
            assert!(error.is_none());
            assert!(
                events.iter().any(|e| matches!(
                    e,
                    StreamEventKind::MessageDelta { stop_reason: Some(s), .. } if *s == expected
                )),
                "{expected:?}"
            );
        }
    }

    #[test]
    fn failure_events_are_classified_and_after_output_they_are_partial() {
        let failed = json!({"type": "response.failed", "response": {"error": {"code": "server_error", "message": "boom"}}});
        let (_, early) = run(vec![created(), failed.clone()]);
        let early = early.unwrap();
        assert_eq!(early.kind, ProviderErrorKind::Transient { retryable: true });
        assert_eq!(early.message, "boom");

        let (_, limited) = run(vec![
            json!({"type": "error", "code": "rate_limit_exceeded", "message": "slow"}),
        ]);
        assert_eq!(limited.unwrap().kind, ProviderErrorKind::RateLimited);

        let (_, late) = run(vec![
            created(),
            json!({"type": "response.output_item.added", "output_index": 0, "item": {"type": "message"}}),
            json!({"type": "response.output_text.delta", "output_index": 0, "delta": "x"}),
            failed,
        ]);
        assert_eq!(late.unwrap().kind, ProviderErrorKind::PartialStream);
    }

    #[test]
    fn a_stream_that_never_completes_is_an_error_and_unknown_events_are_ignored() {
        let (_, none) = run(vec![]);
        assert_eq!(none.unwrap().kind, ProviderErrorKind::Transport);
        let (_, cut) = run(vec![
            created(),
            json!({"type": "response.some_future_event", "x": 1}),
            json!({"type": "response.output_item.added", "output_index": 0, "item": {"type": "message"}}),
            json!({"type": "response.output_text.delta", "output_index": 0, "delta": "x"}),
        ]);
        assert_eq!(cut.unwrap().kind, ProviderErrorKind::PartialStream);
    }
}
