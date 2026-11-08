//! OpenAI Chat Completions: IR request -> wire body, wire stream -> IR events.
//!
//! @nyan: both halves are pure (no I/O) so contract fixtures can drive them
//! with canned frames. Replay state is not used: Chat Completions has no input
//! channel for reasoning, so thinking in history is dropped, and server-side
//! tool calls (an Anthropic concept) are dropped with it.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::target::Compat;
use super::transport::Frame;
use super::{EMPTY_TOOL_OUTPUT, media_url, reasoning_effort, system_text, tool_result_text};
use crate::services::provider::adapter::ResolvedTarget;
use crate::services::provider::ir::{
    AssistantPart, BlockStart, Delta, MediaSource, Message, ModelRequest, ProviderError,
    ProviderErrorKind, StopReason, StreamEventKind, ToolChoice, ToolResultContent, ToolResultPart,
    Usage, UserPart,
};
use crate::utils::settings::nyan::{CacheRetention, MaxTokensField};

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
                    "tool_call_id": tool_call_id,
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

fn push_assistant(messages: &mut Vec<Value>, parts: &[AssistantPart], compat: &Compat) {
    let mut text = String::new();
    let mut calls = Vec::new();
    let mut thinking = Vec::new();
    let mut reasoning_field = "reasoning_content";
    for part in parts {
        match part {
            AssistantPart::Text { text: t } => text.push_str(t),
            AssistantPart::ToolCall {
                id, name, input, ..
            } => calls.push(json!({
                "id": id,
                "type": "function",
                "function": {"name": name, "arguments": arguments_json(input)},
            })),
            AssistantPart::Thinking {
                text: t, signature, ..
            } => {
                let is_reasoning_field = signature == "reasoning"
                    || signature == "reasoning_text"
                    || signature == "reasoning_content";
                if (is_reasoning_field || compat.requires_reasoning_content) && !t.trim().is_empty()
                {
                    thinking.push(t.as_str());
                    if is_reasoning_field {
                        reasoning_field = signature.as_str();
                    }
                }
            }
            AssistantPart::RedactedThinking { .. }
            | AssistantPart::ServerToolCall { .. }
            | AssistantPart::ServerToolResult { .. } => {}
        }
    }
    if text.is_empty() && calls.is_empty() && thinking.is_empty() {
        return;
    }
    let mut message = json!({
        "role": "assistant",
        "content": if text.is_empty() { Value::Null } else { Value::String(text) },
    });
    if !calls.is_empty() {
        message["tool_calls"] = Value::Array(calls);
    }
    if !thinking.is_empty() {
        message[reasoning_field] = Value::String(thinking.join("\n\n"));
    } else if compat.requires_reasoning_content {
        message["reasoning_content"] = Value::String(String::new());
    }
    messages.push(message);
}

fn add_cache_control_to_text_part(msg: &mut Value, cache_control: &Value) {
    if let Some(content_str) = msg.get("content").and_then(Value::as_str) {
        if !content_str.is_empty() {
            msg["content"] = json!([{
                "type": "text",
                "text": content_str,
                "cache_control": cache_control,
            }]);
        }
    } else if let Some(parts) = msg.get_mut("content").and_then(Value::as_array_mut) {
        for part in parts.iter_mut().rev() {
            if part.get("type").and_then(Value::as_str) == Some("text") {
                part["cache_control"] = cache_control.clone();
                break;
            }
        }
    }
}

fn apply_anthropic_cache_control(
    messages: &mut [Value],
    tools: Option<&mut Value>,
    cache_control: &Value,
) {
    // 1. System prompt
    for msg in messages.iter_mut() {
        let role = msg.get("role").and_then(Value::as_str);
        if role == Some("system") || role == Some("developer") {
            add_cache_control_to_text_part(msg, cache_control);
            break;
        }
    }

    // 2. Last tool
    if let Some(tool_list) = tools.and_then(Value::as_array_mut) {
        if let Some(last_tool) = tool_list.last_mut() {
            last_tool["cache_control"] = cache_control.clone();
        }
    }

    // 3. Last conversation message
    for msg in messages.iter_mut().rev() {
        let role = msg.get("role").and_then(Value::as_str).unwrap_or_default();
        if role == "user" || role == "assistant" || role == "tool" {
            add_cache_control_to_text_part(msg, cache_control);
            break;
        }
    }
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
pub fn build_body(target: &ResolvedTarget, request: &ModelRequest, compat: &Compat) -> Value {
    let mut messages = Vec::new();
    if let Some(system) = system_text(request) {
        messages.push(json!({"role": "system", "content": system}));
    }
    for message in &request.messages {
        match message {
            Message::User { parts } => push_user(&mut messages, parts),
            Message::Assistant { parts } => push_assistant(&mut messages, parts, compat),
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
    });
    let cap_key = match compat.max_tokens_field {
        MaxTokensField::MaxCompletionTokens => "max_completion_tokens",
        MaxTokensField::MaxTokens => "max_tokens",
    };
    body[cap_key] = json!(max_output);
    if compat.include_usage {
        body["stream_options"] = json!({"include_usage": true});
    }

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
        && let Some(effort) = reasoning_effort(request, target)
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
    if compat.prompt_cache_key && target.cache_retention != CacheRetention::None {
        let session_id = crate::bootstrap::state::get_session_id();
        if !session_id.is_empty() {
            body["prompt_cache_key"] = json!(super::clamp_prompt_cache_key(&session_id));
        }
        // Pi `openai-completions.ts` sends "24h" for long retention on Chat,
        // whatever the model's cache mode.
        if target.cache_retention == CacheRetention::Long && compat.long_cache_retention {
            body["prompt_cache_retention"] = json!("24h");
        }
    }

    if compat.cache_control_format.as_deref() == Some("anthropic")
        && target.cache_retention != CacheRetention::None
    {
        let cache_control = if target.cache_retention == CacheRetention::Long {
            json!({"type": "ephemeral", "ttl": "1h"})
        } else {
            json!({"type": "ephemeral"})
        };
        apply_anthropic_cache_control(&mut messages, body.get_mut("tools"), &cache_control);
        body["messages"] = json!(messages);
    }

    crate::services::provider::adapter::merge_extra(&mut body, target.extra.as_ref());

    body
}

/// The `chat/completions` body for one non-streaming call.
pub fn build_body_non_streaming(
    target: &ResolvedTarget,
    request: &ModelRequest,
    compat: &Compat,
) -> Value {
    let mut body = build_body(target, request, compat);
    body["stream"] = json!(false);
    if let Some(obj) = body.as_object_mut() {
        obj.remove("stream_options");
    }
    body
}

pub fn parse_non_streaming_response(
    response: &Value,
    request_id: Option<String>,
) -> Result<crate::services::provider::non_streaming::NonStreamingResponse, ProviderError> {
    use crate::services::provider::non_streaming::{NonStreamingContent, NonStreamingResponse};

    let choices = response
        .get("choices")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::InvalidRequest,
                format!("OpenAI response missing 'choices': {response}"),
            )
        })?;

    let choice = choices.first().ok_or_else(|| {
        ProviderError::new(
            ProviderErrorKind::InvalidRequest,
            "OpenAI response 'choices' array is empty",
        )
    })?;

    let mut content = Vec::new();
    let message = choice.get("message").unwrap_or(&Value::Null);

    if let Some((field, reasoning)) = ["reasoning_content", "reasoning", "reasoning_text"]
        .iter()
        .find_map(|f| {
            message
                .get(f)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(|text| (*f, text))
        })
    {
        content.push(NonStreamingContent::Thinking {
            text: reasoning.to_string(),
            signature: Some(field.to_string()),
        });
    }

    if let Some(text) = message.get("content").and_then(Value::as_str)
        && !text.is_empty()
    {
        content.push(NonStreamingContent::Text(text.to_string()));
    }

    let mut saw_call = false;
    if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) {
        for call in tool_calls {
            let id = call
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let name = call
                .pointer("/function/name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let args_raw = call
                .pointer("/function/arguments")
                .and_then(Value::as_str)
                .unwrap_or("{}");
            let input = serde_json::from_str(args_raw).unwrap_or_else(|_| json!({}));
            content.push(NonStreamingContent::ToolUse { id, name, input });
            saw_call = true;
        }
    }

    let stop = choice
        .get("finish_reason")
        .and_then(Value::as_str)
        .map(|r| stop_reason(r, saw_call));
    let usage = response.get("usage").map(parse_usage).unwrap_or_default();
    let id = response
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("chatcmpl-1")
        .to_string();
    let model = response
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    Ok(NonStreamingResponse {
        id,
        model,
        request_id,
        content,
        stop_reason: stop,
        usage,
    })
}

// ---------------------------------------------------------------------------
// Stream
// ---------------------------------------------------------------------------

fn parse_usage(usage: &Value) -> Usage {
    Usage::from_openai_chat(usage)
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
    thinking_block: Option<usize>,
    thinking_signature_emitted: bool,
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
            thinking_block: None,
            thinking_signature_emitted: false,
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

    fn close_thinking(&mut self, out: &mut Vec<StreamEventKind>) {
        if let Some(index) = self.thinking_block.take() {
            out.push(StreamEventKind::BlockStop { index });
        }
    }

    fn open_thinking(&mut self, out: &mut Vec<StreamEventKind>) -> usize {
        if let Some(index) = self.thinking_block {
            return index;
        }
        let index = self.next_block;
        self.next_block += 1;
        self.thinking_block = Some(index);
        out.push(StreamEventKind::BlockStart {
            index,
            block: BlockStart::Thinking,
        });
        index
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
        self.close_thinking(out);
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

        // Reasoning content arrived (DeepSeek, Kimi, llama.cpp, etc.):
        let reasoning_match = ["reasoning_content", "reasoning", "reasoning_text"]
            .iter()
            .find_map(|field| {
                delta
                    .get(field)
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(|text| (*field, text))
            });

        if let Some((field, reasoning)) = reasoning_match {
            self.close_text(&mut out);
            let index = self.open_thinking(&mut out);
            self.saw_output = true;
            if !self.thinking_signature_emitted {
                self.thinking_signature_emitted = true;
                out.push(StreamEventKind::BlockDelta {
                    index,
                    delta: Delta::Signature {
                        signature: field.to_string(),
                    },
                });
            }
            out.push(StreamEventKind::BlockDelta {
                index,
                delta: Delta::Thinking {
                    text: reasoning.to_string(),
                },
            });
        }

        // A refusal arrives in its own field and reads as text to the user.
        for field in ["content", "refusal"] {
            if let Some(text) = delta
                .get(field)
                .and_then(Value::as_str)
                .filter(|t| !t.is_empty())
            {
                self.close_thinking(&mut out);
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
            self.close_thinking(&mut out);
            self.close_text(&mut out);
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
            self.close_thinking(&mut out);
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
        CachePolicy, ClientContext, Effort, JsonSchemaOutput, ToolSpec,
    };
    use crate::utils::model::profile::ResolvedCapabilities;
    use crate::utils::settings::nyan::ProviderKind;

    /// The OpenAI kind's defaults, which every case here except the local-server
    /// tests uses.
    fn build_body(target: &ResolvedTarget, request: &ModelRequest) -> Value {
        super::build_body(
            target,
            request,
            &Compat::resolve(ProviderKind::Openai, None),
        )
    }

    fn target(capabilities: ResolvedCapabilities) -> ResolvedTarget {
        ResolvedTarget {
            profile: "p".into(),
            provider: "openai".into(),
            protocol: crate::services::provider::ir::Protocol::OpenaiChat,
            model: "gpt-5".into(),
            context_window: 400_000,
            max_output_tokens: 8_000,
            capabilities,
            cache_retention: Default::default(),
            extra: None,
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
            effort: Some(Effort::Off),
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
        // `off` on gpt-5 clamps to its lowest tier rather than being left out.
        assert_eq!(body["reasoning_effort"], "minimal");
        for absent in ["tools", "tool_choice", "temperature", "response_format"] {
            assert!(
                body.get(absent).is_none(),
                "{absent} must not be sent: {body}"
            );
        }
    }

    #[test]
    fn profile_extra_is_merged_last_into_the_chat_body() {
        let mut with_extra = target(all());
        with_extra.extra = Some(json!({
            "max_completion_tokens": 123,
            "chat_template_kwargs": {"enable_thinking": false}
        }));
        let body = build_body(&with_extra, &request(vec![user_text("hi")]));
        assert_eq!(body["max_completion_tokens"], 123);
        assert_eq!(
            body["chat_template_kwargs"],
            json!({"enable_thinking": false})
        );
        assert_eq!(body["model"], "gpt-5");
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
        r.effort = Some(Effort::High);
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
        let call_id = "call_abc";
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
        assert_eq!(m[2]["tool_calls"][0]["id"], "call_abc");
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
    fn text_stream_parses_deepseek_and_kimi_cache_usage() {
        let (events, error) = run(vec![
            chunk(json!({"role": "assistant", "content": "ok"}), Value::Null),
            chunk(json!({}), json!("stop")),
            json!({"id": "chatcmpl-ds", "choices": [], "usage": {
                "prompt_tokens": 1000, "completion_tokens": 50, "total_tokens": 1050,
                "prompt_cache_hit_tokens": 800,
                "prompt_cache_miss_tokens": 200}}),
        ]);
        assert!(error.is_none(), "{error:?}");
        let Some(StreamEventKind::MessageDelta { usage, .. }) = events.iter().rev().nth(1) else {
            panic!("{events:?}");
        };
        assert_eq!(usage.input_tokens, 200);
        assert_eq!(usage.cache_read_tokens, 800);
        assert_eq!(usage.output_tokens, 50);

        let (events_kimi, error_kimi) = run(vec![
            chunk(json!({"role": "assistant", "content": "ok"}), Value::Null),
            chunk(json!({}), json!("stop")),
            json!({"id": "chatcmpl-kimi", "choices": [], "usage": {
                "prompt_tokens": 500, "completion_tokens": 40, "total_tokens": 540,
                "cached_tokens": 350}}),
        ]);
        assert!(error_kimi.is_none(), "{error_kimi:?}");
        let Some(StreamEventKind::MessageDelta {
            usage: usage_kimi, ..
        }) = events_kimi.iter().rev().nth(1)
        else {
            panic!("{events_kimi:?}");
        };
        assert_eq!(usage_kimi.input_tokens, 150);
        assert_eq!(usage_kimi.cache_read_tokens, 350);
        assert_eq!(usage_kimi.output_tokens, 40);
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

    #[test]
    fn stream_parses_reasoning_content_delta_before_text() {
        let (events, error) = run(vec![
            chunk(
                json!({"role": "assistant", "reasoning_content": "let me think"}),
                Value::Null,
            ),
            chunk(json!({"reasoning_content": " a bit more"}), Value::Null),
            chunk(json!({"content": "the answer"}), Value::Null),
            chunk(json!({}), json!("stop")),
        ]);
        assert!(error.is_none(), "{error:?}");
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
                delta: Delta::Signature {
                    signature: "reasoning_content".into(),
                },
            }
        );
        assert_eq!(
            events[3],
            StreamEventKind::BlockDelta {
                index: 0,
                delta: Delta::Thinking {
                    text: "let me think".into(),
                },
            }
        );
        assert_eq!(
            events[4],
            StreamEventKind::BlockDelta {
                index: 0,
                delta: Delta::Thinking {
                    text: " a bit more".into(),
                },
            }
        );
        assert_eq!(events[5], StreamEventKind::BlockStop { index: 0 });
        assert_eq!(
            events[6],
            StreamEventKind::BlockStart {
                index: 1,
                block: BlockStart::Text,
            }
        );
        assert_eq!(
            events[7],
            StreamEventKind::BlockDelta {
                index: 1,
                delta: Delta::Text {
                    text: "the answer".into(),
                },
            }
        );
        assert_eq!(events[8], StreamEventKind::BlockStop { index: 1 });
    }

    #[test]
    fn build_body_emits_prompt_cache_key_and_retention() {
        let req = request(vec![user_text("hi")]);
        let mut t = target(all());

        // Default / Short retention: prompt_cache_key present, retention absent
        t.cache_retention = CacheRetention::Short;
        let body = super::build_body(&t, &req, &Compat::resolve(ProviderKind::Openai, None));
        let key = body["prompt_cache_key"].as_str().unwrap();
        assert!(
            !key.is_empty()
                && key.len()
                    <= crate::services::provider::openai::OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH
        );
        assert!(body.get("prompt_cache_retention").is_none());

        // Long retention: prompt_cache_key present, retention is "24h"
        t.cache_retention = CacheRetention::Long;
        let body_long = super::build_body(&t, &req, &Compat::resolve(ProviderKind::Openai, None));
        assert_eq!(body_long["prompt_cache_retention"], "24h");
        assert_eq!(body_long["prompt_cache_key"], body["prompt_cache_key"]);

        // None retention: prompt_cache_key and retention omitted
        t.cache_retention = CacheRetention::None;
        let body_none = super::build_body(&t, &req, &Compat::resolve(ProviderKind::Openai, None));
        assert!(body_none.get("prompt_cache_key").is_none());
        assert!(body_none.get("prompt_cache_retention").is_none());
    }

    #[test]
    fn build_body_sends_no_openai_cache_fields_off_openai_hosts() {
        let req = request(vec![user_text("hi")]);
        let mut t = target(all());
        t.model = "deepseek-v4-flash".into();
        t.cache_retention = CacheRetention::Long;
        for (kind, base_url) in [
            (ProviderKind::Openai, "https://api.deepseek.com"),
            (ProviderKind::Openai, "https://openrouter.ai/api/v1"),
            (ProviderKind::Openai, "https://gw.example/v1"),
            (ProviderKind::Gateway, "https://api.openai.com/v1"),
            (ProviderKind::Gateway, "https://api.moonshot.cn/v1"),
            (ProviderKind::Openrouter, "https://openrouter.ai/api/v1"),
            (ProviderKind::Vllm, "http://127.0.0.1:8000/v1"),
            (ProviderKind::Ollama, "http://127.0.0.1:11434/v1"),
        ] {
            let compat = Compat::resolve_full(kind, base_url, Some(&t.model), None);
            let body = super::build_body(&t, &req, &compat);
            // DeepSeek caches automatically; only OpenAI and Kimi take these.
            assert!(
                body.get("prompt_cache_key").is_none(),
                "{kind:?} {base_url}"
            );
            assert!(
                body.get("prompt_cache_retention").is_none(),
                "{kind:?} {base_url}"
            );
            assert!(
                body.get("prompt_cache_options").is_none(),
                "{kind:?} {base_url}"
            );
            assert!(
                !body.to_string().contains("cache_control"),
                "{kind:?} {base_url}"
            );
        }
    }

    #[test]
    fn build_body_follows_explicit_prompt_cache_options() {
        use crate::utils::settings::nyan::{OpenaiCompatSettings, PromptCacheKeyMode};

        let req = request(vec![user_text("hi")]);
        let mut t = target(all());
        t.cache_retention = CacheRetention::Long;
        let body = |base_url: &str, settings: OpenaiCompatSettings| {
            let compat = Compat::resolve_full(
                ProviderKind::Openai,
                base_url,
                Some(&t.model),
                Some(&settings),
            );
            super::build_body(&t, &req, &compat)
        };
        let enabled = |long: Option<bool>| OpenaiCompatSettings {
            prompt_cache_key: Some(PromptCacheKeyMode::Enabled),
            long_cache_retention: long,
            ..Default::default()
        };

        // Enabled off OpenAI: the key, and 24h only when asked for.
        let key_only = body("https://api.deepseek.com", enabled(None));
        assert!(key_only["prompt_cache_key"].is_string(), "{key_only}");
        assert!(key_only.get("prompt_cache_retention").is_none());
        let long = body("https://api.deepseek.com", enabled(Some(true)));
        assert!(long["prompt_cache_key"].is_string(), "{long}");
        assert_eq!(long["prompt_cache_retention"], "24h");

        // Disabled on OpenAI itself: nothing.
        let disabled = OpenaiCompatSettings {
            prompt_cache_key: Some(PromptCacheKeyMode::Disabled),
            long_cache_retention: Some(true),
            ..Default::default()
        };
        let none = body("https://api.openai.com/v1", disabled);
        assert!(none.get("prompt_cache_key").is_none(), "{none}");
        assert!(none.get("prompt_cache_retention").is_none(), "{none}");

        // Auto is the default; the retention switch alone never adds a key.
        let auto = |long| OpenaiCompatSettings {
            prompt_cache_key: Some(PromptCacheKeyMode::Auto),
            long_cache_retention: long,
            ..Default::default()
        };
        let official = body("https://api.openai.com/v1", auto(Some(false)));
        assert!(official["prompt_cache_key"].is_string(), "{official}");
        assert!(official.get("prompt_cache_retention").is_none());
        let elsewhere = body("https://api.deepseek.com", auto(Some(true)));
        assert!(elsewhere.get("prompt_cache_key").is_none(), "{elsewhere}");
        assert!(elsewhere.get("prompt_cache_retention").is_none());
    }

    #[test]
    fn build_body_sends_kimi_the_key_but_no_ttl_field_by_default() {
        use crate::utils::settings::nyan::{OpenaiCompatSettings, PromptCacheKeyMode};

        let req = request(vec![user_text("hi")]);
        let mut t = target(all());
        t.model = "kimi-k3".into();
        let body = |t: &ResolvedTarget, base_url: &str, settings: Option<OpenaiCompatSettings>| {
            let compat = Compat::resolve_full(
                ProviderKind::Openai,
                base_url,
                Some(&t.model),
                settings.as_ref(),
            );
            super::build_body(t, &req, &compat)
        };
        let hosts = [
            "https://api.moonshot.cn/v1",
            "https://api.moonshot.ai/v1",
            "https://api.kimi.com/coding/v1",
            "https://eu.api.moonshot.ai/v1",
        ];

        // Long (the default) and short retention: the key, never a TTL field.
        for retention in [CacheRetention::Long, CacheRetention::Short] {
            t.cache_retention = retention;
            for url in hosts {
                let sent = body(&t, url, None);
                assert!(sent["prompt_cache_key"].is_string(), "{url}: {sent}");
                assert!(
                    sent.get("prompt_cache_retention").is_none(),
                    "{url}: {sent}"
                );
                assert!(sent.get("prompt_cache_options").is_none(), "{url}: {sent}");
            }
        }

        // No caching, or the key disabled: nothing.
        t.cache_retention = CacheRetention::None;
        let off = body(&t, hosts[0], None);
        assert!(off.get("prompt_cache_key").is_none(), "{off}");
        t.cache_retention = CacheRetention::Long;
        let disabled = body(
            &t,
            hosts[0],
            Some(OpenaiCompatSettings {
                prompt_cache_key: Some(PromptCacheKeyMode::Disabled),
                ..Default::default()
            }),
        );
        assert!(disabled.get("prompt_cache_key").is_none(), "{disabled}");
        assert!(disabled.get("prompt_cache_retention").is_none());

        // An explicit longCacheRetention still asks for OpenAI's 24h.
        let long = body(
            &t,
            hosts[0],
            Some(OpenaiCompatSettings {
                long_cache_retention: Some(true),
                ..Default::default()
            }),
        );
        assert!(long["prompt_cache_key"].is_string(), "{long}");
        assert_eq!(long["prompt_cache_retention"], "24h");
    }

    #[test]
    fn build_body_replays_reasoning_content_for_assistant_messages() {
        let mut messages = vec![
            Message::User {
                parts: vec![UserPart::Text { text: "q".into() }],
            },
            Message::Assistant {
                parts: vec![
                    AssistantPart::Thinking {
                        text: "step 1".into(),
                        signature: "reasoning_content".into(),
                        provider_state: None,
                    },
                    AssistantPart::Text { text: "ans".into() },
                ],
            },
        ];
        let req = request(messages.clone());
        let body = super::build_body(
            &target(all()),
            &req,
            &Compat::resolve(ProviderKind::Openai, None),
        );
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs[2]["role"], "assistant");
        assert_eq!(msgs[2]["content"], "ans");
        assert_eq!(msgs[2]["reasoning_content"], "step 1");

        // When requires_reasoning_content is set on compat and assistant has no thinking:
        messages[1] = Message::Assistant {
            parts: vec![AssistantPart::Text {
                text: "ans only".into(),
            }],
        };
        let req_no_think = request(messages);
        let mut compat = Compat::resolve(ProviderKind::Openai, None);
        compat.requires_reasoning_content = true;
        let body_compat = super::build_body(&target(all()), &req_no_think, &compat);
        let msgs_compat = body_compat["messages"].as_array().unwrap();
        assert_eq!(msgs_compat[2]["reasoning_content"], "");
    }

    #[test]
    fn build_body_applies_anthropic_cache_control_for_openrouter() {
        let mut req = request(vec![
            Message::User {
                parts: vec![UserPart::Text {
                    text: "first question".into(),
                }],
            },
            Message::Assistant {
                parts: vec![AssistantPart::Text {
                    text: "first answer".into(),
                }],
            },
            Message::User {
                parts: vec![UserPart::Text {
                    text: "second question".into(),
                }],
            },
        ]);
        req.tools = vec![tool()];
        let mut compat = Compat::resolve(ProviderKind::Openai, None);
        compat.cache_control_format = Some("anthropic".into());

        let mut t = target(all());
        t.cache_retention = CacheRetention::Short;
        let body = super::build_body(&t, &req, &compat);

        let msgs = body["messages"].as_array().unwrap();
        // 1. System prompt has cache_control
        assert_eq!(msgs[0]["role"], "system");
        let sys_parts = msgs[0]["content"].as_array().unwrap();
        assert_eq!(sys_parts[0]["cache_control"]["type"], "ephemeral");
        assert!(sys_parts[0]["cache_control"].get("ttl").is_none());

        // 2. Last tool has cache_control
        let tools = body["tools"].as_array().unwrap();
        let last_tool = tools.last().unwrap();
        assert_eq!(last_tool["cache_control"]["type"], "ephemeral");

        // 3. Last conversation message has cache_control
        let last_msg = msgs.last().unwrap();
        assert_eq!(last_msg["role"], "user");
        let last_msg_parts = last_msg["content"].as_array().unwrap();
        assert_eq!(last_msg_parts[0]["cache_control"]["type"], "ephemeral");

        // With Long retention: ttl is "1h"
        t.cache_retention = CacheRetention::Long;
        let body_long = super::build_body(&t, &req, &compat);
        let msgs_long = body_long["messages"].as_array().unwrap();
        let sys_parts_long = msgs_long[0]["content"].as_array().unwrap();
        assert_eq!(sys_parts_long[0]["cache_control"]["ttl"], "1h");
    }

    #[test]
    fn non_streaming_parses_reasoning_content() {
        let response_json = json!({
            "id": "chatcmpl-test",
            "model": "gpt-5",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "reasoning_content": "detailed thinking",
                    "content": "final answer"
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 100,
                "completion_tokens": 50,
                "total_tokens": 150
            }
        });
        let parsed = super::parse_non_streaming_response(&response_json, Some("req_1".into()))
            .expect("parses");
        assert_eq!(parsed.content.len(), 2);
        assert_eq!(
            parsed.content[0],
            crate::services::provider::non_streaming::NonStreamingContent::Thinking {
                text: "detailed thinking".into(),
                signature: Some("reasoning_content".into()),
            }
        );
        assert_eq!(
            parsed.content[1],
            crate::services::provider::non_streaming::NonStreamingContent::Text(
                "final answer".into()
            )
        );
    }
}
