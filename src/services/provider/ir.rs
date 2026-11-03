//! Provider-neutral request / stream / error IR.
//!
//! @nyan: docs/nyan/provider-redesign.md §3.5. Nothing in this module may name
//! an Anthropic, OpenAI or Gemini wire type. Adapters translate this IR to
//! their wire format and normalize the wire stream back into [`StreamEvent`].
//!
//! The shapes are chosen so that all four planned protocols fit without
//! changing the IR:
//! - Anthropic Messages: content blocks, `thinking.signature`, cache markers.
//! - OpenAI Chat Completions: `messages[].content` parts, `tool_calls`.
//! - OpenAI Responses: `input` items; reasoning items round-trip through
//!   [`ProviderState`].
//! - Gemini: `contents[].parts`; `thoughtSignature` round-trips through
//!   [`Thinking`](AssistantPart::Thinking) / [`ToolCall`](AssistantPart::ToolCall).

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Marker that splits `ModelRequest::system` into a stable prefix and a
/// per-session suffix. Adapters that support prefix caching place their cache
/// boundary on it; every adapter must strip it from the wire text.
pub use crate::constants::prompts::SYSTEM_PROMPT_DYNAMIC_BOUNDARY as SYSTEM_BOUNDARY;

/// Wire protocol an adapter speaks. Mirrors `ProviderProtocol` in settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Protocol {
    AnthropicMessages,
    OpenaiChat,
    OpenaiResponses,
    Gemini,
}

/// Opaque per-protocol replay state attached to assistant output.
///
/// An adapter only reads state whose `protocol` equals its own and must ignore
/// the rest, so history produced by one provider never leaks into another's
/// request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderState {
    pub protocol: Protocol,
    pub data: Value,
}

// ---------------------------------------------------------------------------
// Request
// ---------------------------------------------------------------------------

/// One model call. Carries no provider header, beta flag or credential.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelRequest {
    /// Provider wire model id (`ModelRef::model`), not a profile name.
    pub model: String,
    /// System prompt blocks in order; may contain [`SYSTEM_BOUNDARY`].
    pub system: Vec<String>,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    pub tool_choice: ToolChoice,
    pub thinking: ThinkingPreference,
    /// Reasoning-effort hint; adapters map it or drop it per capability.
    pub effort: Option<Effort>,
    pub max_output_tokens: u32,
    pub temperature: Option<f64>,
    pub structured_output: Option<JsonSchemaOutput>,
    pub cache: CachePolicy,
    /// Who is asking (`repl_main_thread`, `compact`, ...). Adapters may use it
    /// for routing hints; it never changes message content.
    pub query_source: String,
    pub agent_id: Option<String>,
    /// The model must not call several tools in one turn.
    pub disable_parallel_tools: bool,
    /// Deferrable tools the conversation has already loaded through tool
    /// search; an adapter that defers tools must send these in full.
    pub discovered_tools: Vec<String>,
    /// More tools may still arrive (MCP servers connecting), so an adapter
    /// must not conclude that tool search has nothing to defer.
    pub tools_pending: bool,
    pub client: ClientContext,
    /// Per-protocol extras that have no neutral meaning (Anthropic cached
    /// microcompact edits, provider-executed tool schemas, ...). An adapter
    /// reads only entries tagged with its own protocol.
    pub extensions: Vec<ProviderState>,
}

impl ModelRequest {
    /// The extension tagged for `protocol`, if any.
    pub fn extension(&self, protocol: Protocol) -> Option<&Value> {
        self.extensions
            .iter()
            .find(|state| state.protocol == protocol)
            .map(|state| &state.data)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum Message {
    User { parts: Vec<UserPart> },
    Assistant { parts: Vec<AssistantPart> },
}

impl Message {
    pub fn is_user(&self) -> bool {
        matches!(self, Self::User { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UserPart {
    Text {
        text: String,
    },
    Image {
        source: MediaSource,
        /// Lossless wire block for a protocol that needs more than `source`
        /// (e.g. an Anthropic image block with extra metadata). Other
        /// adapters ignore it.
        provider_state: Option<ProviderState>,
    },
    Document {
        media_type: String,
        data: String,
    },
    ToolResult {
        tool_call_id: String,
        content: ToolResultContent,
        is_error: bool,
    },
}

/// A tool result is either one plain string or a list of typed parts. The
/// distinction is observable on some wires, so the IR keeps it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolResultContent {
    Text(String),
    Parts(Vec<ToolResultPart>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolResultPart {
    Text {
        text: String,
    },
    Image {
        source: MediaSource,
        provider_state: Option<ProviderState>,
    },
    Document {
        media_type: String,
        data: String,
    },
    /// Deferred-tool expansion marker. Adapters without native support render
    /// it as text naming the tool.
    ToolReference {
        tool_name: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MediaSource {
    Base64 { media_type: String, data: String },
    Url { url: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AssistantPart {
    Text {
        text: String,
    },
    /// Visible reasoning text. `signature` is the provider's replay signature
    /// (Anthropic `signature`, Gemini `thoughtSignature`); an empty string is
    /// a real value and must be replayed as-is by adapters that need it.
    Thinking {
        text: String,
        signature: String,
        provider_state: Option<ProviderState>,
    },
    /// Reasoning the provider returned encrypted or withheld.
    RedactedThinking {
        data: String,
        provider_state: Option<ProviderState>,
    },
    /// A function call the client must execute.
    ToolCall {
        id: String,
        name: String,
        input: Value,
        provider_state: Option<ProviderState>,
    },
    /// A call the provider executes itself (web search, advisor, ...).
    ServerToolCall {
        id: String,
        name: String,
        input: Value,
    },
    /// Result of a provider-executed call. `name` is the server tool that
    /// produced it (`web_search`, `advisor`).
    ServerToolResult {
        tool_call_id: String,
        name: String,
        content: Value,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema for the arguments.
    pub input_schema: Value,
    /// The tool asks for provider-side schema enforcement. Whether that is
    /// honoured is the adapter's decision.
    pub strict: bool,
    /// The tool may be loaded lazily through tool search. Whether it is
    /// actually deferred is the adapter's decision.
    pub deferrable: bool,
    /// The tool comes from an MCP server.
    pub is_mcp: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolChoice {
    #[default]
    Auto,
    Any,
    None,
    Tool {
        name: String,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ThinkingPreference {
    #[default]
    Disabled,
    /// Let the model decide how much to think.
    Adaptive,
    /// Think with a token budget; `None` means "adapter default for the model".
    Budget { tokens: Option<u32> },
}

/// Provider-neutral effort hint (`low`/`medium`/`high`/... or a number).
/// Adapters map or drop it according to their capabilities.
pub type Effort = crate::utils::effort::EffortValue;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonSchemaOutput {
    pub schema: Value,
}

/// How the client is running; adapters that must identify the client to the
/// provider read it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ClientContext {
    pub non_interactive: bool,
    pub has_append_system_prompt: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CachePolicy {
    /// Allow the adapter to place cache markers at all.
    pub enabled: bool,
    /// Do not extend the cache with this request's tail (fork / side query).
    pub skip_write: bool,
}

// ---------------------------------------------------------------------------
// Stream
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_write_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_deleted_tokens: u64,
    pub reasoning_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    StopSequence,
    PauseTurn,
    Refusal,
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BlockStart {
    Text,
    Thinking,
    RedactedThinking {
        data: String,
    },
    ToolCall {
        id: String,
        name: String,
        server: bool,
        /// Arguments already known at block start (usually empty).
        input: Value,
    },
    ServerToolResult {
        tool_call_id: String,
        name: String,
        content: Value,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Delta {
    Text {
        text: String,
    },
    Thinking {
        text: String,
    },
    Signature {
        signature: String,
    },
    /// A fragment of the tool call's JSON arguments.
    ToolInput {
        json: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEventKind {
    MessageStart {
        request_id: Option<String>,
        message_id: Option<String>,
        model: Option<String>,
        usage: Usage,
    },
    BlockStart {
        index: usize,
        block: BlockStart,
    },
    BlockDelta {
        index: usize,
        delta: Delta,
    },
    BlockStop {
        index: usize,
    },
    /// Cumulative usage and, once known, the stop reason.
    MessageDelta {
        stop_reason: Option<StopReason>,
        usage: Usage,
    },
    MessageStop,
}

/// One normalized stream event plus, optionally, the wire event it came from.
///
/// `wire` exists for diagnostics and for SDK-format passthrough. No consumer
/// may branch on its provider-specific shape; when it is `None` the consumer
/// derives what it needs from `kind`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StreamEvent {
    pub kind: StreamEventKind,
    pub wire: Option<Value>,
}

impl StreamEvent {
    pub fn new(kind: StreamEventKind) -> Self {
        Self { kind, wire: None }
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderErrorKind {
    RateLimited,
    Overloaded,
    Transient {
        retryable: bool,
    },
    ContextOverflow,
    InvalidRequest,
    Unauthorized,
    Forbidden,
    NotFound,
    Transport,
    /// The stream produced output and then failed; the request must not be
    /// replayed automatically.
    PartialStream,
    Aborted,
}

/// Normalized provider failure. `headers` and `body` are kept only so retry
/// policy can inspect them; they must never be logged or shown verbatim.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderError {
    pub kind: ProviderErrorKind,
    pub message: String,
    pub status: Option<u16>,
    pub retry_after: Option<Duration>,
    pub request_id: Option<String>,
    pub headers: BTreeMap<String, String>,
    pub body: Option<Value>,
}

impl ProviderError {
    pub fn new(kind: ProviderErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            status: None,
            retry_after: None,
            request_id: None,
            headers: BTreeMap::new(),
            body: None,
        }
    }

    /// Case-insensitive header lookup.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.status {
            Some(status) => write!(f, "[{status}] {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for ProviderError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Tools,
    Thinking,
    Images,
    Documents,
    StructuredOutput,
    Streaming,
    PromptCache,
}

/// A request asks for something the resolved model does not declare. Raised by
/// `validate` before any I/O; never retried and never answered by switching
/// model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityError {
    pub capability: Capability,
    pub detail: String,
}

impl std::fmt::Display for CapabilityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "model does not support {:?}: {}",
            self.capability, self.detail
        )
    }
}

impl std::error::Error for CapabilityError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_request() -> ModelRequest {
        ModelRequest {
            model: "m".into(),
            system: vec!["a".into(), SYSTEM_BOUNDARY.into(), "b".into()],
            messages: vec![
                Message::User {
                    parts: vec![UserPart::Text { text: "hi".into() }],
                },
                Message::Assistant {
                    parts: vec![
                        AssistantPart::Thinking {
                            text: "t".into(),
                            signature: String::new(),
                            provider_state: None,
                        },
                        AssistantPart::ToolCall {
                            id: "c1".into(),
                            name: "Read".into(),
                            input: serde_json::json!({"p": 1}),
                            provider_state: Some(ProviderState {
                                protocol: Protocol::Gemini,
                                data: serde_json::json!({"thoughtSignature": "s"}),
                            }),
                        },
                    ],
                },
                Message::User {
                    parts: vec![UserPart::ToolResult {
                        tool_call_id: "c1".into(),
                        content: ToolResultContent::Parts(vec![ToolResultPart::Text {
                            text: "ok".into(),
                        }]),
                        is_error: false,
                    }],
                },
            ],
            tools: vec![ToolSpec {
                name: "Read".into(),
                description: "d".into(),
                input_schema: serde_json::json!({"type": "object"}),
                strict: false,
                deferrable: false,
                is_mcp: false,
            }],
            tool_choice: ToolChoice::Tool {
                name: "Read".into(),
            },
            thinking: ThinkingPreference::Budget { tokens: Some(100) },
            effort: Some(Effort::Named("high".into())),
            max_output_tokens: 1000,
            temperature: None,
            structured_output: None,
            cache: CachePolicy {
                enabled: true,
                ..CachePolicy::default()
            },
            query_source: "repl_main_thread".into(),
            agent_id: None,
            disable_parallel_tools: false,
            discovered_tools: Vec::new(),
            tools_pending: false,
            client: ClientContext::default(),
            extensions: vec![ProviderState {
                protocol: Protocol::AnthropicMessages,
                data: serde_json::json!({"k": 1}),
            }],
        }
    }

    #[test]
    fn extension_is_selected_by_protocol() {
        let request = sample_request();
        assert!(request.extension(Protocol::AnthropicMessages).is_some());
        assert!(request.extension(Protocol::OpenaiChat).is_none());
    }

    #[test]
    fn request_round_trips_through_json() {
        let request = sample_request();
        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["messages"][1]["parts"][1]["type"], "tool_call");
        assert_eq!(json["tool_choice"]["type"], "tool");
        let back: ModelRequest = serde_json::from_value(json).unwrap();
        assert_eq!(back, request);
    }

    #[test]
    fn provider_error_header_lookup_is_case_insensitive() {
        let mut error = ProviderError::new(ProviderErrorKind::RateLimited, "slow down");
        error.status = Some(429);
        error
            .headers
            .insert("Retry-After".to_string(), "7".to_string());
        assert_eq!(error.header("retry-after"), Some("7"));
        assert_eq!(error.to_string(), "[429] slow down");
    }

    #[test]
    fn ir_has_no_anthropic_shaped_keys() {
        // Extensions are protocol-tagged by design; the IR body must be clean.
        let mut request = sample_request();
        request.extensions.clear();
        let json = serde_json::to_string(&request).unwrap();
        for forbidden in ["cache_control", "tool_use", "anthropic", "betas"] {
            assert!(!json.contains(forbidden), "{forbidden} leaked into the IR");
        }
    }
}
