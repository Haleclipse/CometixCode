//! Maps to: CC `types/message.ts`.
//!
//! `Message` is CC's `Message` union (:120-146) — the single conversation
//! history type. `RenderableMessage` is the transcript-row bridge: CC's
//! `RenderableMessage` merely widens `Message` with the two grouping wrappers.
//! System rows carry the model union directly since batch D1; Attachment rows
//! carry the typed [`Attachment`] union since batch D2 (declared beside its
//! producers in `utils/attachments.rs`, matching CC's placement in
//! `utils/attachments.ts:440`, and re-exported here for consumers).
//! The REPL owns one `Vec<HistoryEntry>` and both the render rows and the API
//! history are projections of it.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Re-exports: the Attachment union lives with its producers, mirroring CC's
/// `utils/attachments.ts` declaration site (batch D2).
pub use crate::utils::attachments::{Attachment, RelevantMemory};

/// Maps to: CC runtime MessageOrigin objects (`utils/messages.ts:3742-3746`,
/// `:5496-5513`, `utils/attachments.ts:1098`). The generated type stub's
/// string alias is incomplete. Preserve runtime object fields and legacy strings
/// losslessly; consumers inspect `kind` just as JavaScript does.
pub type MessageOrigin = serde_json::Value;

use super::ids::ToolUseId;

///   - 'user' → UserMessage
///   - 'assistant' → AssistantMessage
///   - 'attachment' → AttachmentMessage (file/directory/skill/memory)
///   - 'progress' → ProgressMessage
///   - 'hook_result' → HookResultMessage
///
/// No serde on the union itself: JSONL/session wire shapes are hand-written
/// at the adapters, and the CC `Message` union (types/message.ts:120-125) is
/// likewise a type-level composition, not a serialization contract. The
/// member structs keep their own serde where the adapters use it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    User(UserMessage),
    Assistant(AssistantMessage),
    System(SystemMessage),
    Attachment(AttachmentMessage),
    Progress(ProgressMessage),
    HookResult(HookResultMessage),
}

/// Maps to: CC `types/message.ts:114` `ProgressMessage<P> = MessageBase &
/// { type: 'progress'; data: P; toolUseID: string; parentToolUseID: string }`
/// — a real member of the Message union: progress lives IN the single
/// history, feeds `buildMessageLookups`, and never renders as a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgressMessage {
    pub uuid: String,
    pub timestamp: DateTime<Utc>,
    pub tool_use_id: String,
    pub parent_tool_use_id: String,
    pub data: ToolUseProgressMessage,
}

/// Maps to: CC `types/message.ts` `HookResultMessage` returned by
/// `processSessionStartHooks`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookResultMessage {
    #[serde(rename = "type")]
    pub message_type: String,
    pub uuid: String,
    pub timestamp: DateTime<Utc>,
    pub attachment: serde_json::Value,
}

impl HookResultMessage {
    pub fn attachment(attachment: serde_json::Value) -> Self {
        Self {
            message_type: "hook_result".to_string(),
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            attachment,
        }
    }
}

/// Maps to: CC `types/message.ts` `StreamEvent`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum StreamEvent {
    ApiEvent {
        event: serde_json::Value,
        ttft_ms: Option<u64>,
    },
}

/// Maps to: CC `types/message.ts` `ToolUseSummaryMessage` and
/// `utils/messages.ts` `createToolUseSummaryMessage(...)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolUseSummaryMessage {
    #[serde(rename = "type")]
    pub message_type: String,
    pub uuid: String,
    pub timestamp: DateTime<Utc>,
    pub summary: String,
    pub preceding_tool_use_ids: Vec<String>,
}

impl ToolUseSummaryMessage {
    pub fn new(summary: String, preceding_tool_use_ids: Vec<String>, uuid: String) -> Self {
        Self {
            message_type: "tool_use_summary".to_string(),
            uuid,
            timestamp: Utc::now(),
            summary,
            preceding_tool_use_ids,
        }
    }
}

/// Maps to: CC `types/message.ts#TombstoneMessage` and
/// `utils/messages.ts#handleMessageFromStream`, which carries the orphaned
/// message to the tombstone callback instead of appending it.
///
/// The `message` field is the runtime shape CC yields (`query.ts:717`
/// `{ type: 'tombstone', message: msg }` where `msg` came from
/// `assistantMessages` — whole per-block `Message` values), even though CC's
/// declaration omits it. Carried as `Message` since the streamed-assistant
/// convergence: the tombstoned values are the same whole messages the stream
/// appended to the one history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TombstoneMessage {
    pub message: Message,
}

/// Maps to: CC `types/message.ts` `AssistantMessage` API-error fields
/// (`isApiErrorMessage`, `apiError`, `error`, `errorDetails`) as the typed
/// model-error payload yielded through `query.ts`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemApiErrorMessage {
    pub content: String,
    pub api_error: String,
    pub error: String,
    /// Maps to CC `types/message.ts` `AssistantMessage.errorDetails` and
    /// `utils/hooks.ts` StopFailure `error_details` hook input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_details: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserMessage {
    /// Maps to: CC `MessageBase.uuid` (types/message.ts:30) — every message
    /// carries its own identity; the single history derives row ids from it
    /// via `derive_uuid`. Live constructors mint a v4; JSONL parsing reads
    /// the envelope's real value. Empty string = pre-C3a data.
    #[serde(default)]
    pub uuid: String,
    pub timestamp: DateTime<Utc>,
    pub content: Vec<UserContent>,
    #[serde(default)]
    pub is_compact_summary: bool,
    /// Maps to: CC `UserMessage.planContent` — a fact field missing from CC's
    /// type declaration but read directly (`utils/plans.ts:307`,
    /// `screens/REPL.tsx:4103`) and rendered by `UserTextMessage.tsx:53-54`
    /// before any tag dispatch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_content: Option<String>,
    /// Maps to: CC `UserMessage.imagePasteIds` (`types/message.ts:57`) — the
    /// per-image paste ids the user branch folds into `imageIndices`
    /// (`Message.tsx:154-164`) before the per-block map.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_paste_ids: Option<Vec<u32>>,
    /// Maps to: CC `UserMessage.isVisibleInTranscriptOnly`
    /// (`types/message.ts:53`) — `shouldShowUserMessage`'s second gate hides
    /// the message from the default render list unless transcript mode is
    /// active (`utils/messages.ts:4675`). Producers: compact summaries
    /// (`services/compact/compact.ts:622`, `:1043`) and session-memory compact
    /// (`sessionMemoryCompact.ts:480`, flow not ported).
    #[serde(
        rename = "isVisibleInTranscriptOnly",
        default,
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub is_visible_in_transcript_only: bool,
    /// Maps to: CC `UserMessage.mcpMeta` (`types/message.ts:56`) — `unknown`
    /// at the source too ("MCP protocol metadata to pass through to SDK
    /// consumers (never sent to model)", `utils/messages.ts:482-486`); written
    /// by MCP tool results (`toolExecution.ts:1401/1464/1727`, flow not
    /// ported) and folded into SDK `tool_use_result`
    /// (`queryHelpers.ts:150-151`).
    #[serde(rename = "mcpMeta", default, skip_serializing_if = "Option::is_none")]
    pub mcp_meta: Option<serde_json::Value>,
    /// Maps to: CC `UserMessage.sourceToolAssistantUUID`
    /// (`types/message.ts:58`) — "For tool_result messages: the UUID of the
    /// assistant message containing the matching tool_use"
    /// (`utils/messages.ts:490-491`); session storage re-parents transcript
    /// entries onto it (`sessionStorage.ts:1033-1036`). Written by
    /// `query.ts:145` and every `toolExecution.ts` tool-result yield.
    #[serde(
        rename = "sourceToolAssistantUUID",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub source_tool_assistant_uuid: Option<String>,
    /// Maps to: CC `UserMessage.permissionMode` (`types/message.ts:59`) —
    /// "Permission mode when message was sent (for rewind restoration)"
    /// (`utils/messages.ts:492-493`, restored at `REPL.tsx:4938-4947`).
    /// Wire string form (`permission_mode_internal_name`), matching CC's
    /// string-union `PermissionMode`.
    #[serde(
        rename = "permissionMode",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub permission_mode: Option<String>,
    /// Maps to: CC `UserMessage.origin` (`types/message.ts:60`) — message
    /// provenance ("undefined = human (keyboard)", `utils/messages.ts:499`).
    /// Despite the declared string union, real values are objects with a
    /// `kind` tag (`{kind: 'task-notification' | 'coordinator' | 'channel'
    /// | 'human', server?, ...}` — `utils/messages.ts:3742-3746`,
    /// `:5498-5511`, `:4669-4671`). Carried as `serde_json::Value` for now;
    /// the nested-payload track owns typing it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<serde_json::Value>,
    /// Maps to: CC `UserMessage.summarizeMetadata` (`types/message.ts:61`) —
    /// partial-compact metadata (`services/compact/compact.ts:1037-1041`)
    /// rendered by `CompactSummary.tsx:18`. Reuses the CompactMetadata shape
    /// like CC's type alias.
    #[serde(
        rename = "summarizeMetadata",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub summarize_metadata: Option<CompactMetadata>,
}

impl UserMessage {
    /// The row's single real content block. `normalize_messages` guarantees
    /// one block per user row (the `block_index` stepping stone is gone);
    /// user content has no identity sibling, so this is the first block.
    /// Counterpart of `AssistantMessage::first_content_block`.
    pub fn first_content_block(&self) -> Option<&UserContent> {
        self.content.first()
    }

    /// Mutable counterpart of [`Self::first_content_block`].
    pub fn first_content_block_mut(&mut self) -> Option<&mut UserContent> {
        self.content.first_mut()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum UserContent {
    Text(String),
    /// Rust discriminant carrier for CC `UserMessage.isMeta` text messages.
    /// API/session adapters serialize this as an ordinary text block while
    /// preserving `isMeta: true` on the JSONL envelope.
    MetaText(String),
    Image {
        media_type: String,
        data: String,
    },
    /// Rust discriminant carrier for an image-only CC user message with
    /// envelope-level `isMeta: true` (Read PDF page extraction).
    MetaImage {
        media_type: String,
        data: String,
    },
    /// Lossless carrier for CC ImageBlockParam (`types/message.ts`): URL
    /// sources and block/source metadata cannot fit the legacy base64 fields.
    /// `is_meta` carries the enclosing UserMessage.isMeta, not an API field.
    RawImage {
        block: serde_json::Value,
        is_meta: bool,
    },
    /// Maps to Anthropic `document` content blocks (e.g. PDF from Read).
    Document {
        media_type: String,
        data: String,
    },
    /// Rust discriminant carrier for a document-only CC user message with
    /// envelope-level `isMeta: true` (Read full-PDF supplemental message).
    MetaDocument {
        media_type: String,
        data: String,
    },
    ToolResult(ToolResult),
}

impl UserContent {
    /// Representation-only ImageBlockParam adapter. Preserve the existing
    /// simple base64 variants when they encode every input field exactly;
    /// otherwise retain the full image block without fetching its source.
    pub(crate) fn from_image_block(block: serde_json::Value, is_meta: bool) -> Self {
        let simple = block
            .as_object()
            .filter(|object| object.len() == 2)
            .and_then(|_| block.get("source").and_then(serde_json::Value::as_object))
            .filter(|source| {
                source.len() == 3
                    && source.get("type").and_then(serde_json::Value::as_str) == Some("base64")
            })
            .and_then(|source| {
                Some((
                    source.get("media_type")?.as_str()?.to_string(),
                    source.get("data")?.as_str()?.to_string(),
                ))
            });
        if let Some((media_type, data)) = simple {
            if is_meta {
                Self::MetaImage { media_type, data }
            } else {
                Self::Image { media_type, data }
            }
        } else {
            Self::RawImage { block, is_meta }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssistantMessage {
    /// CC `MessageBase.uuid` — see `UserMessage.uuid`.
    #[serde(default)]
    pub uuid: String,
    pub timestamp: DateTime<Utc>,
    pub content: Vec<AssistantContent>,
    pub model: Option<String>,
    pub stop_reason: Option<StopReason>,
    pub usage: Option<TokenUsage>,
}

/// Rust carrier for CC `AssistantMessage.requestId` and
/// `AssistantMessage.message.id`, whose real owner is
/// `utils/messages.ts:386-408#baseCreateAssistantMessage`.
///
/// Nyan keeps API content and the transcript envelope in one typed message.
/// This metadata block is therefore out-of-band: API/session adapters must
/// project it onto the envelope and must never send it as model content.
///
/// It deliberately carries NO uuid. CC has exactly one uuid per message —
/// `baseCreateAssistantMessage` mints `uuid: randomUUID()` at
/// `utils/messages.ts:388` for the envelope, and the separate
/// `message: { id: randomUUID() }` at `:391` is the BetaMessage id, i.e.
/// [`Self::api_message_id`], not a second envelope uuid. `insertMessageChain`
/// then spreads `...message` (`utils/sessionStorage.ts:1048`) and chains
/// `parentUuid = message.uuid` (`:1067`) off that one value.
///
/// A `uuid` field here used to shadow [`AssistantMessage::uuid`], minted
/// independently by every producer, and the JSONL writer picked the wrong one —
/// see task #120: every join from an in-memory message to a live transcript row
/// silently failed, including `REPL.tsx:3521-3525`'s tombstone removal.
///
/// Do not cite `../rebuild/src/types/message.ts` for this type: it is a
/// `@generated-stub` and therefore a no-source seam, not a citable owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantMessageIdentity {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_message_id: Option<String>,
    /// Maps to: CC `types/message.ts:41` `isApiErrorMessage?: boolean`, set by
    /// `createAssistantAPIErrorMessage` (`utils/messages.ts:453`).
    ///
    /// It belongs on this carrier, not on `AssistantMessage`, for the reason
    /// stated above: it is envelope metadata, never model content. Nyan had
    /// no such field at all and the renderer reconstructed it by sniffing
    /// `text.starts_with("API Error")`
    /// (`components/messages/assistant_text_message.rs`), which misses every
    /// error whose copy does not begin with that literal.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_api_error_message: bool,
    /// Maps to: CC `types/message.ts:42` `apiError?: unknown` — opaque at the
    /// source too.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_error: Option<serde_json::Value>,
    /// Maps to: CC `types/message.ts:44` `errorDetails?: string`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_details: Option<String>,
    /// @nyan: the profile that produced this message. Absent on synthetic
    /// rows and on rows written before nyan recorded it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub producer: Option<ModelProducer>,
}

/// @nyan: what a request was sent to, and so what produced its reply. Two
/// requests target the same model iff `provider`, `protocol` and `model` all
/// match; `profile` is kept for display and diagnostics only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelProducer {
    pub profile: String,
    pub provider: String,
    pub protocol: crate::services::provider::ir::Protocol,
    /// Requested wire model id. The response may name a dated alias, which
    /// would make a model's own history look foreign.
    pub model: String,
}

impl ModelProducer {
    pub fn is_same_model(&self, other: &Self) -> bool {
        self.provider == other.provider
            && self.protocol == other.protocol
            && self.model == other.model
    }
}

impl AssistantMessageIdentity {
    pub fn new(request_id: Option<String>, api_message_id: Option<String>) -> Self {
        Self {
            request_id,
            api_message_id,
            ..Self::default()
        }
    }
}

impl Default for AssistantMessageIdentity {
    /// Mirrors CC `baseCreateAssistantMessage`'s parameter defaults
    /// (`utils/messages.ts:355-361`): `isApiErrorMessage = false`, the rest
    /// absent. The envelope uuid `randomUUID()` at `:388` belongs to
    /// [`AssistantMessage::uuid`], not here.
    fn default() -> Self {
        Self {
            request_id: None,
            api_message_id: None,
            is_api_error_message: false,
            api_error: None,
            error_details: None,
            producer: None,
        }
    }
}

impl AssistantMessage {
    /// Returns the CC assistant envelope identity without exposing the Rust
    /// out-of-band carrier to API-content consumers.
    pub fn identity(&self) -> Option<&AssistantMessageIdentity> {
        self.content.iter().find_map(|content| match content {
            AssistantContent::MessageIdentity(identity) => Some(identity),
            _ => None,
        })
    }

    /// @nyan: record the model that produced this message on its identity
    /// envelope.
    pub fn set_producer(&mut self, producer: ModelProducer) {
        if let Some(identity) = self.content.iter_mut().find_map(|content| match content {
            AssistantContent::MessageIdentity(identity) => Some(identity),
            _ => None,
        }) {
            identity.producer = Some(producer);
        } else {
            self.content.push(AssistantContent::MessageIdentity(
                AssistantMessageIdentity {
                    producer: Some(producer),
                    ..AssistantMessageIdentity::default()
                },
            ));
        }
    }

    pub fn request_id(&self) -> Option<&str> {
        self.identity()
            .and_then(|identity| identity.request_id.as_deref())
    }

    // NOTE: there is deliberately no `AssistantMessage::uuid()` accessor.
    // The envelope uuid is the public `uuid` field, reached through
    // `Message::uuid()` (:1757-1766) like every other variant. An accessor here
    // returning the identity block's own uuid shadowed that field by one
    // character and returned a different value — see the type doc above and
    // task #120.

    pub fn api_message_id(&self) -> Option<&str> {
        self.identity()
            .and_then(|identity| identity.api_message_id.as_deref())
    }

    pub fn has_model_content(&self) -> bool {
        self.content
            .iter()
            .any(|content| !matches!(content, AssistantContent::MessageIdentity(_)))
    }

    /// The row's single real content block: the first non-identity block.
    /// `normalize_messages` guarantees one real block per assistant row (the
    /// `block_index` stepping stone is gone); the `MessageIdentity` sibling
    /// is the out-of-band envelope carrier and never renders.
    pub fn first_content_block(&self) -> Option<&AssistantContent> {
        self.content
            .iter()
            .find(|content| !matches!(content, AssistantContent::MessageIdentity(_)))
    }

    /// Attaches one canonical identity while preserving API content order.
    pub fn attach_identity(&mut self, identity: AssistantMessageIdentity) {
        self.content
            .retain(|content| !matches!(content, AssistantContent::MessageIdentity(_)));
        self.content
            .push(AssistantContent::MessageIdentity(identity));
    }
}

/// Maps to: CC `AdvisorToolResultBlock['content']` (utils/advisor.ts:19-32) —
/// the three shapes an advisor result can take, discriminated by its own
/// `type` tag on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AdvisorResult {
    #[serde(rename = "advisor_result")]
    Result { text: String },
    #[serde(rename = "advisor_redacted_result", rename_all = "snake_case")]
    Redacted { encrypted_content: String },
    #[serde(rename = "advisor_tool_result_error", rename_all = "snake_case")]
    Error { error_code: String },
}

impl AdvisorResult {
    /// The human-readable text, when this variant carries any. Redacted and
    /// error results have none — CC renders those through their own branches
    /// rather than as text.
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Result { text } => Some(text.as_str()),
            Self::Redacted { .. } | Self::Error { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AssistantContent {
    Text(String),
    /// Maps to Anthropic `ThinkingBlock` / `ThinkingBlockParam`.
    ///
    /// `signature` is required when replaying assistant thinking into the next
    /// request (CC `assistantMessageToMessageParam` spreads the whole block).
    /// Stream start may omit it; CC initializes `signature: ''` and fills via
    /// `signature_delta`. `#[serde(default)]` keeps old sessions loadable.
    Thinking {
        text: String,
        #[serde(default)]
        signature: String,
    },
    RedactedThinking {
        data: String,
    },
    ToolUse(ToolUseBlock),
    /// Maps to Anthropic `server_tool_use` content blocks. These are model/API
    /// content, but not local Claude Code tool calls and must not enter
    /// `runTools(...)`.
    ServerToolUse(ToolUseBlock),
    /// Maps to Anthropic `web_search_tool_result` blocks paired with
    /// `server_tool_use`; preserved for future model requests but not routed to
    /// local tool execution.
    WebSearchToolResult {
        tool_use_id: ToolUseId,
        content: serde_json::Value,
    },
    /// Maps to: CC `AdvisorToolResultBlock` (utils/advisor.ts:16-32).
    ///
    /// The block pairs a `tool_use_id` with a three-way content union. CC keeps
    /// both: the id says which `server_tool_use` this answers (so a pending
    /// advisor call can be told from a completed one), and the union's tag
    /// picks the renderer (Message.tsx:467). Flattening it to a single string
    /// dropped the id outright and turned the redacted and error variants into
    /// empty text.
    Advisor {
        tool_use_id: ToolUseId,
        content: AdvisorResult,
    },
    /// Out-of-band Rust carrier for the CC assistant transcript envelope.
    /// It is not an Anthropic API content block.
    MessageIdentity(AssistantMessageIdentity),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolUseBlock {
    pub id: ToolUseId,
    pub name: String,
    pub input: serde_json::Value,
}

/// Maps to Anthropic `ToolResultBlockParam.content` array items.
///
/// Used for:
/// - `tool_reference` from ToolSearch (deferred schema expansion)
/// - `image` / `document` from FileRead multimodal tool_result content
///   (CC `FileReadTool.mapToolResultToToolResultBlockParam` image branch)
/// - `text` / media from Bash `structuredContent` passthrough
///   (CC `BashTool.mapToolResultToToolResultBlockParam` structured branch)
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolResultContentBlock {
    ToolReference {
        tool_name: String,
    },
    Text {
        text: String,
    },
    Image {
        source: ToolResultMediaSource,
    },
    Document {
        source: ToolResultMediaSource,
    },
    /// Lossless image payload after source `smooshIntoToolResult` folds a
    /// top-level image into tool_result content. Serializes as the raw block.
    #[serde(untagged)]
    RawImage(serde_json::Value),
}

impl<'de> Deserialize<'de> for ToolResultContentBlock {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let value = serde_json::Value::deserialize(deserializer)?;
        match value.get("type").and_then(serde_json::Value::as_str) {
            Some("image") => match UserContent::from_image_block(value, false) {
                UserContent::Image { media_type, data } => Ok(Self::image_base64(media_type, data)),
                UserContent::RawImage { block, .. } => Ok(Self::RawImage(block)),
                _ => unreachable!("image representation adapter only constructs image variants"),
            },
            Some("document") => {
                serde_json::from_value(value.get("source").cloned().unwrap_or_default())
                    .map(|source| Self::Document { source })
                    .map_err(D::Error::custom)
            }
            Some("text") => value
                .get("text")
                .and_then(serde_json::Value::as_str)
                .map(|text| Self::Text {
                    text: text.to_string(),
                })
                .ok_or_else(|| D::Error::custom("text block requires text")),
            Some("tool_reference") => value
                .get("tool_name")
                .and_then(serde_json::Value::as_str)
                .map(|tool_name| Self::ToolReference {
                    tool_name: tool_name.to_string(),
                })
                .ok_or_else(|| D::Error::custom("tool_reference block requires tool_name")),
            _ => Err(D::Error::custom("unsupported tool result content block")),
        }
    }
}

/// Maps to Anthropic base64 `source` on image/document content blocks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolResultMediaSource {
    #[serde(rename = "type")]
    pub kind: String,
    pub media_type: String,
    pub data: String,
}

impl ToolResultContentBlock {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }

    pub fn image_base64(media_type: impl Into<String>, data: impl Into<String>) -> Self {
        Self::Image {
            source: ToolResultMediaSource {
                kind: "base64".to_string(),
                media_type: media_type.into(),
                data: data.into(),
            },
        }
    }

    pub fn document_base64(media_type: impl Into<String>, data: impl Into<String>) -> Self {
        Self::Document {
            source: ToolResultMediaSource {
                kind: "base64".to_string(),
                media_type: media_type.into(),
                data: data.into(),
            },
        }
    }

    /// Best-effort conversion for Bash/MCP `structuredContent` arrays.
    /// Known Anthropic block shapes deserialize; unknowns become text JSON.
    pub fn from_structured_value(value: &serde_json::Value) -> Self {
        if let Ok(block) = serde_json::from_value::<Self>(value.clone()) {
            return block;
        }
        Self::text(value.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolResult {
    pub tool_use_id: ToolUseId,
    pub content: String,
    pub is_error: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub content_blocks: Vec<ToolResultContentBlock>,
    /// Maps to: CC `types/message.ts` `UserMessage.toolUseResult` for the
    /// typed tool output retained beside the model-facing tool-result block.
    ///
    /// Rust normalizes the user message into `UserContent::ToolResult`, so the
    /// raw `toolUseResult` lives on that block rather than on the enclosing user wrapper.
    /// It is runtime/session metadata and must never be sent to the API.
    #[serde(default, skip_serializing, skip_deserializing)]
    pub tool_use_result: Option<serde_json::Value>,
}

impl ToolResult {
    /// Derive the render-time status the old row field used to store. Mirrors
    /// CC `UserToolResultMessage.tsx:43-90` dispatch order (cancel sentinel →
    /// reject sentinel → is_error → success). Callers that still need a
    /// `ToolResultStatus` (grouping, collapse, hook seam) die in batch C /
    /// the per-tool display drain.
    pub fn derived_status(&self) -> ToolResultStatus {
        if crate::utils::messages::is_tool_cancel_message(&self.content) {
            ToolResultStatus::Canceled
        } else if crate::utils::messages::is_plain_tool_reject_message(&self.content) {
            ToolResultStatus::Rejected
        } else if self.is_error {
            ToolResultStatus::Error
        } else {
            ToolResultStatus::Success
        }
    }
}

/// Maps to CC types/message.ts#PartialCompactDirection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PartialCompactDirection {
    #[default]
    #[serde(rename = "from")]
    From,
    #[serde(rename = "up_to")]
    UpTo,
}
impl PartialCompactDirection {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::From => "from",
            Self::UpTo => "up_to",
        }
    }
}

/// @nyan: receipt for the deterministic mask/summary admission decision.
/// `Option` on the enclosing field keeps old transcripts wire-compatible.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NyanCompactReceipt {
    pub masked_count: usize,
    pub masked_tokens: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_style: Option<String>,
    pub keep_recent_tokens: u32,
    pub tokens_after_estimate: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<TokenUsage>,
}

/// Maps to CC `types/message.ts` `SystemCompactBoundaryMessage.compactMetadata`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messages_summarized: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_context: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preserved_segment: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pre_compact_discovered_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nyan: Option<NyanCompactReceipt>,
}

/// Maps to: CC `types/message.ts:65` `SystemMessageBase = MessageBase &
/// { type: 'system'; isMeta: boolean }` — the shared envelope every system
/// subtype carries (`MessageBase` :29-34 contributes `uuid`/`timestamp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemBase {
    /// CC `MessageBase.uuid` — see `UserMessage.uuid`.
    pub uuid: String,
    pub timestamp: DateTime<Utc>,
    /// CC `isMeta: boolean` — required on `SystemMessageBase` (unlike the
    /// optional `MessageBase.isMeta`); every CC factory writes `false`
    /// (`utils/messages.ts:4345` et al.).
    pub is_meta: bool,
}

impl SystemBase {
    /// Fresh envelope the way every CC `create*Message` factory builds one:
    /// `uuid: randomUUID(), timestamp: new Date().toISOString(),
    /// isMeta: false` (`utils/messages.ts:4341-4347`).
    pub fn new() -> Self {
        Self::with_uuid(uuid::Uuid::new_v4().to_string())
    }

    /// Fresh envelope with a caller-owned uuid (row id == message id).
    pub fn with_uuid(uuid: impl Into<String>) -> Self {
        Self {
            uuid: uuid.into(),
            timestamp: Utc::now(),
            is_meta: false,
        }
    }
}

impl Default for SystemBase {
    fn default() -> Self {
        Self::new()
    }
}

/// Maps to: CC `SystemMicrocompactBoundaryMessage.microcompactMetadata`
/// (`types/message.ts:92`, declared `unknown`) with the runtime shape
/// `createMicrocompactBoundaryMessage` writes (`utils/messages.ts:4575-4581`):
/// `{ trigger, preTokens, tokensSaved, compactedToolIds,
/// clearedAttachmentUUIDs }`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MicrocompactMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_tokens: Option<u64>,
    #[serde(default)]
    pub tokens_saved: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub compacted_tool_ids: Vec<String>,
    #[serde(
        rename = "clearedAttachmentUUIDs",
        default,
        skip_serializing_if = "Vec::is_empty"
    )]
    pub cleared_attachment_uuids: Vec<String>,
}

/// Maps to: CC `types/message.ts:104-110` — `SystemMessage` is a 16-subtype
/// discriminated union over `SystemMessageBase`. The Rust enum is that same
/// discrimination at the same (type) layer per the discrimination-placement
/// contract (PORTING.md rule 1); the old flat `{content, level, subtype}`
/// struct and its render-layer copy (`SystemMessageKind`) died in batch D1.
///
/// No serde on the union: JSONL/session wire shapes are hand-written at the
/// adapters (`session_storage::system_entry_json`,
/// `conversation_recovery::system_message_from_entry`), like the `Message`
/// union itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SystemMessage {
    /// CC `SystemInformationalMessage` (`types/message.ts:67-73`), built by
    /// `createSystemMessage` (`utils/messages.ts:4335-4352`). Also the parse
    /// fallback for unknown wire subtypes: CC deserializes sessions with a
    /// plain `parseJSONL<Entry>` cast (`utils/sessionStorage.ts:3614`, no
    /// subtype validation) and any unrecognized subtype reaches
    /// `SystemTextMessage`'s terminal branch (`SystemTextMessage.tsx:158-174`)
    /// which reads only `content`/`level` — the informational shape.
    Informational {
        base: SystemBase,
        content: String,
        level: SystemMessageLevel,
        /// CC `toolUseID?` (:71).
        tool_use_id: Option<String>,
        /// CC `preventContinuation?` (:72), spread in only when truthy
        /// (`:4350`).
        prevent_continuation: Option<bool>,
    },
    /// CC `SystemAPIErrorMessage`. The declared type (`types/message.ts:74`)
    /// lists only `content`/`level`, but the sole producer —
    /// `createSystemAPIErrorMessage` (`utils/messages.ts:4585-4603`) — writes
    /// `{ level:'error', error, retryInMs, retryAttempt, maxRetries }` and no
    /// content, and the renderer destructures exactly those
    /// (`SystemAPIErrorMessage.tsx:19-21`). `error` carries the
    /// `formatAPIError(...)` input pre-formatted (the Rust API layer formats
    /// at yield time); the constant `level:'error'` is not stored.
    ApiError {
        base: SystemBase,
        error: String,
        retry_in_ms: u64,
        retry_attempt: u32,
        max_retries: u32,
    },
    /// CC `SystemLocalCommandMessage` (`types/message.ts:75`), built by
    /// `createCommandInputMessage` (`utils/messages.ts:4516-4528`).
    /// Persisted in typed history and converted to User by API normalization
    /// (`utils/messages.ts:2078-2093`); renders through
    /// `UserTextMessage` (`Message.tsx:228-237`). `content` is the full wire
    /// text (command-input tags / tagged stdout); the literal `level:'info'`
    /// is not stored.
    LocalCommand { base: SystemBase, content: String },
    /// CC `SystemPermissionRetryMessage` (`types/message.ts:76`), built by
    /// `createPermissionRetryMessage` (`utils/messages.ts:4354-4367`):
    /// `content = "Allowed {commands.join(', ')}"` with the raw list
    /// alongside. The literal `level:'info'` is not stored.
    PermissionRetry {
        base: SystemBase,
        content: String,
        commands: Vec<String>,
    },
    /// CC `SystemBridgeStatusMessage` (`types/message.ts:77`), built by
    /// `createBridgeStatusMessage` (`utils/messages.ts:4369-4383`).
    BridgeStatus {
        base: SystemBase,
        content: String,
        url: String,
        upgrade_nudge: Option<String>,
    },
    /// CC `SystemScheduledTaskFireMessage` (`types/message.ts:78`), built by
    /// `createScheduledTaskFireMessage` (`utils/messages.ts:4385-4396`).
    ScheduledTaskFire { base: SystemBase, content: String },
    /// CC `SystemStopHookSummaryMessage` (`types/message.ts:79-84`), built by
    /// `createStopHookSummaryMessage` (`utils/messages.ts:4398-4426`). CC has
    /// no pre-baked summary text: `StopHookSummaryMessage` derives
    /// "Ran N ... hooks" at render (`SystemTextMessage.tsx:222,258`), so the
    /// old Rust `text` field is gone.
    StopHookSummary {
        base: SystemBase,
        hook_count: usize,
        hook_infos: Vec<StopHookInfo>,
        hook_errors: Vec<String>,
        prevented_continuation: bool,
        /// CC declares `stopReason: string` but the factory accepts
        /// `string | undefined` (`:4403`) and spreads it through.
        stop_reason: Option<String>,
        has_output: bool,
        level: SystemMessageLevel,
        tool_use_id: Option<String>,
        hook_label: Option<String>,
        total_duration_ms: Option<u64>,
    },
    /// CC `SystemTurnDurationMessage` (`types/message.ts:85-88`), built by
    /// `createTurnDurationMessage` (`utils/messages.ts:4428-4445`). The
    /// display string is derived at render (`SystemTextMessage.tsx:362`
    /// `formatDuration(message.durationMs)`), so the old Rust pre-formatted
    /// `duration` field is gone.
    TurnDuration {
        base: SystemBase,
        duration_ms: u64,
        budget_tokens: Option<u64>,
        budget_limit: Option<u64>,
        budget_nudges: Option<u64>,
        message_count: Option<usize>,
    },
    /// CC `SystemAwaySummaryMessage` (`types/message.ts:89`), built by
    /// `createAwaySummaryMessage` (`utils/messages.ts:4447-4458`).
    AwaySummary { base: SystemBase, content: String },
    /// CC `SystemMemorySavedMessage` (`types/message.ts:90`), built by
    /// `createMemorySavedMessage` (`utils/messages.ts:4460-4471`).
    MemorySaved {
        base: SystemBase,
        written_paths: Vec<String>,
    },
    /// CC `SystemCompactBoundaryMessage` (`types/message.ts:91`) —
    /// `compactMetadata?: CompactMetadata & { preCompactDiscoveredTools?:
    /// string[] }`; built by `createCompactBoundaryMessage`
    /// (`utils/messages.ts:4530-4555`). The factory constants
    /// (`content: 'Conversation compacted'`, `level:'info'`) are not stored:
    /// the renderer never reads them (`Message.tsx:195-202`) and the wire
    /// adapter re-emits them.
    CompactBoundary {
        base: SystemBase,
        compact_metadata: Option<CompactMetadata>,
        logical_parent_uuid: Option<String>,
    },
    /// CC `SystemMicrocompactBoundaryMessage` (`types/message.ts:92`), built
    /// by `createMicrocompactBoundaryMessage` (`utils/messages.ts:4557-4583`).
    /// Renders null (`Message.tsx:204-207`); kept for session/log parity.
    MicrocompactBoundary {
        base: SystemBase,
        microcompact_metadata: Option<MicrocompactMetadata>,
    },
    /// CC `SystemAgentsKilledMessage` (`types/message.ts:93`), built by
    /// `createAgentsKilledMessage` (`utils/messages.ts:4473-4481`). No extra
    /// fields; the render copy is a constant
    /// (`SystemTextMessage.tsx:87-101`).
    AgentsKilled { base: SystemBase },
    /// CC `SystemApiMetricsMessage` (`types/message.ts:94-100`), built by
    /// `createApiMetricsMessage` (`utils/messages.ts:4483-4514`). `ttftMs`
    /// and `otps` stay JSON numbers (may be fractional). Renders null: the
    /// subtype has no `content`, so `SystemTextMessage.tsx:158-163` bails.
    ApiMetrics {
        base: SystemBase,
        ttft_ms: Option<serde_json::Number>,
        otps: Option<serde_json::Number>,
        is_p50: Option<bool>,
        hook_duration_ms: Option<u64>,
        turn_duration_ms: Option<u64>,
        tool_duration_ms: Option<u64>,
        classifier_duration_ms: Option<u64>,
        tool_count: Option<u64>,
        hook_count: Option<u64>,
        classifier_count: Option<u64>,
        config_write_count: Option<u64>,
    },
    /// CC `SystemFileSnapshotMessage` (`types/message.ts:101`), produced at
    /// `utils/plans.ts:383`. No extra fields; renders null (no `content`).
    FileSnapshot { base: SystemBase },
    /// CC `SystemThinkingMessage` (`types/message.ts:102` declares no extra
    /// fields; the ant-only renderer reads a runtime `content` —
    /// `SystemTextMessage.tsx:460-481`, external builds render null).
    Thinking { base: SystemBase, content: String },
}

// Rust-only `SystemMessageKind` variants deleted in batch D1 — each verified
// against CC `types/message.ts:104-110` (no such system subtype) and against
// the Rust tree (no producer beyond parse fallbacks nothing writes):
// - `RateLimit`: CC renders rate-limit copy inside an assistant text row
//   (`AssistantTextMessage.tsx:81` `<RateLimitMessage .../>`).
// - `Shutdown` / `TaskAssignment`: teammate-content renderers
//   (`UserTeammateMessage.tsx:102,108`), not system messages.
// - `HookProgress`: nested tool renderer (`UserToolSuccessMessage.tsx:139`,
//   `AssistantToolUseMessage.tsx:294`), not a system message.
// - `SnipBoundary` / `SnipMarker` flat subtypes: the CC seam is
//   feature('HISTORY_SNIP') (`Message.tsx:208-226`) whose detection modules
//   are generated stubs (`services/compact/snipProjection.ts`,
//   `snipCompact.ts` — both `export {}`); external builds fall through to the
//   informational render, which is exactly what the parse fallback produces.
// - `AgentNotification` flat subtype: a user-message renderer
//   (`UserAgentNotificationMessage.tsx`), not a system subtype.

impl SystemMessage {
    pub fn base(&self) -> &SystemBase {
        match self {
            Self::Informational { base, .. }
            | Self::ApiError { base, .. }
            | Self::LocalCommand { base, .. }
            | Self::PermissionRetry { base, .. }
            | Self::BridgeStatus { base, .. }
            | Self::ScheduledTaskFire { base, .. }
            | Self::StopHookSummary { base, .. }
            | Self::TurnDuration { base, .. }
            | Self::AwaySummary { base, .. }
            | Self::MemorySaved { base, .. }
            | Self::CompactBoundary { base, .. }
            | Self::MicrocompactBoundary { base, .. }
            | Self::AgentsKilled { base }
            | Self::ApiMetrics { base, .. }
            | Self::FileSnapshot { base }
            | Self::Thinking { base, .. } => base,
        }
    }

    pub fn base_mut(&mut self) -> &mut SystemBase {
        match self {
            Self::Informational { base, .. }
            | Self::ApiError { base, .. }
            | Self::LocalCommand { base, .. }
            | Self::PermissionRetry { base, .. }
            | Self::BridgeStatus { base, .. }
            | Self::ScheduledTaskFire { base, .. }
            | Self::StopHookSummary { base, .. }
            | Self::TurnDuration { base, .. }
            | Self::AwaySummary { base, .. }
            | Self::MemorySaved { base, .. }
            | Self::CompactBoundary { base, .. }
            | Self::MicrocompactBoundary { base, .. }
            | Self::AgentsKilled { base }
            | Self::ApiMetrics { base, .. }
            | Self::FileSnapshot { base }
            | Self::Thinking { base, .. } => base,
        }
    }

    pub fn uuid(&self) -> &str {
        &self.base().uuid
    }

    pub fn timestamp(&self) -> DateTime<Utc> {
        self.base().timestamp
    }

    /// The CC wire discriminant (`subtype` on the JSONL entry).
    pub fn subtype(&self) -> &'static str {
        match self {
            Self::Informational { .. } => "informational",
            Self::ApiError { .. } => "api_error",
            Self::LocalCommand { .. } => "local_command",
            Self::PermissionRetry { .. } => "permission_retry",
            Self::BridgeStatus { .. } => "bridge_status",
            Self::ScheduledTaskFire { .. } => "scheduled_task_fire",
            Self::StopHookSummary { .. } => "stop_hook_summary",
            Self::TurnDuration { .. } => "turn_duration",
            Self::AwaySummary { .. } => "away_summary",
            Self::MemorySaved { .. } => "memory_saved",
            Self::CompactBoundary { .. } => "compact_boundary",
            Self::MicrocompactBoundary { .. } => "microcompact_boundary",
            Self::AgentsKilled { .. } => "agents_killed",
            Self::ApiMetrics { .. } => "api_metrics",
            Self::FileSnapshot { .. } => "file_snapshot",
            Self::Thinking { .. } => "thinking",
        }
    }

    /// The `content` field of the subtypes that carry one (informational,
    /// local_command, permission_retry, bridge_status, scheduled_task_fire,
    /// away_summary, thinking — `types/message.ts:67-102`). Boundary/metrics
    /// subtypes carry none and return `None`.
    pub fn content(&self) -> Option<&str> {
        match self {
            Self::Informational { content, .. }
            | Self::LocalCommand { content, .. }
            | Self::PermissionRetry { content, .. }
            | Self::BridgeStatus { content, .. }
            | Self::ScheduledTaskFire { content, .. }
            | Self::AwaySummary { content, .. }
            | Self::Thinking { content, .. } => Some(content),
            _ => None,
        }
    }

    /// The compact-boundary metadata when this is a `CompactBoundary` message
    /// (`types/message.ts:91`).
    pub fn compact_metadata(&self) -> Option<&CompactMetadata> {
        match self {
            Self::CompactBoundary {
                compact_metadata, ..
            } => compact_metadata.as_ref(),
            _ => None,
        }
    }

    /// Maps to: CC `createSystemMessage(content, level)` without the optional
    /// params (`utils/messages.ts:4335-4352`).
    pub fn informational(content: impl Into<String>, level: SystemMessageLevel) -> Self {
        Self::informational_with_uuid(uuid::Uuid::new_v4().to_string(), content, level)
    }

    /// Same, with a caller-owned uuid (row id == message id).
    pub fn informational_with_uuid(
        uuid: impl Into<String>,
        content: impl Into<String>,
        level: SystemMessageLevel,
    ) -> Self {
        Self::Informational {
            base: SystemBase::with_uuid(uuid),
            content: content.into(),
            level,
            tool_use_id: None,
            prevent_continuation: None,
        }
    }

    /// Maps to: CC `createCommandInputMessage(content)`
    /// (`utils/messages.ts:4516-4528`).
    pub fn local_command(content: impl Into<String>) -> Self {
        Self::LocalCommand {
            base: SystemBase::new(),
            content: content.into(),
        }
    }

    /// Maps to: CC `createCompactBoundaryMessage(...)`
    /// (`utils/messages.ts:4530-4555`).
    pub fn compact_boundary(compact_metadata: Option<CompactMetadata>) -> Self {
        Self::CompactBoundary {
            base: SystemBase::new(),
            compact_metadata,
            logical_parent_uuid: None,
        }
    }
}

/// Maps to CC `types/message.ts:112-113` `AttachmentMessage`.
///
/// CC declares `attachment` loosely (`{type: string; [key: string]: unknown}`)
/// but the values are always the typed [`Attachment`] union from
/// `utils/attachments.ts:440`. Rust stores the union directly (batch D2);
/// [`Attachment::Unknown`] carries the loose remainder losslessly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentMessage {
    /// CC `MessageBase.uuid` — see `UserMessage.uuid`.
    #[serde(default)]
    pub uuid: String,
    pub timestamp: DateTime<Utc>,
    pub attachment: Attachment,
    /// The verbatim wire payload this attachment was parsed from, when it came
    /// from a transcript.
    ///
    /// CC stores attachments as plain JS objects, so every field an entry
    /// carries survives `JSON.parse` → render → `JSON.stringify`. The typed
    /// enum only declares the fields Nyan reads, so re-serializing it drops
    /// anything else the entry held — a payload written by a newer CC, or by a
    /// producer that spread extra keys, would silently lose those fields the
    /// first time a session round-trips through Nyan. Recording writes this
    /// payload back verbatim when present; attachments built in-process have
    /// no wire form and serialize from the typed value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wire_payload: Option<serde_json::Value>,
}

impl AttachmentMessage {
    /// Accepts a typed [`Attachment`] or a wire `serde_json::Value` (parsed
    /// via [`Attachment::from_wire`]; unmatched payloads stay `Unknown`).
    pub fn new(attachment: impl Into<Attachment>) -> Self {
        Self {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            attachment: attachment.into(),
            wire_payload: None,
        }
    }

    /// Builds a transcript-sourced attachment, keeping the wire payload so a
    /// round-trip is lossless (see [`Self::wire_payload`]).
    pub fn from_wire_payload(
        uuid: String,
        timestamp: DateTime<Utc>,
        payload: serde_json::Value,
    ) -> Self {
        Self {
            uuid,
            timestamp,
            attachment: Attachment::from_wire(payload.clone()),
            wire_payload: Some(payload),
        }
    }

    /// The payload to persist: the verbatim wire form when this attachment
    /// came from a transcript, otherwise the typed projection.
    pub fn payload_for_transcript(&self) -> serde_json::Value {
        match &self.wire_payload {
            Some(payload) => payload.clone(),
            None => serde_json::to_value(&self.attachment).unwrap_or(serde_json::Value::Null),
        }
    }

    /// The CC wire discriminant (`attachment.type`).
    pub fn attachment_type(&self) -> &str {
        self.attachment.wire_type()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SystemMessageLevel {
    Info,
    Warning,
    Error,
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
}

fn is_zero_u64(value: &u64) -> bool {
    *value == 0
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
    /// Maps to CC cached microcompact's dynamic `usage.cache_deleted_input_tokens`.
    /// The Anthropic SDK type does not expose this in all builds, but query.ts
    /// reads it after cache-editing responses to emit the deferred
    /// microcompact boundary with the server-reported token delta.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub cache_deleted_input_tokens: u64,
}

// --- Rendering (`components/Messages.tsx`) ---

/// Maps to: CC `types/message.ts#RenderableMessage`.
///
/// Rust represents the TypeScript structural union with a `uuid` plus the
/// `RenderableMessageKind` discriminant below. The public type keeps CC's
/// name; `RenderableMessageKind` is only the required Rust enum carrier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderableMessage {
    pub uuid: String,
    pub kind: RenderableMessageKind,
}

impl Default for RenderableMessage {
    fn default() -> Self {
        Self::user("", "")
    }
}

impl RenderableMessage {
    /// One-block assistant row constructor: `normalize_messages` splits
    /// messages so every row carries one real block; producers hand the block
    /// to a minimal `AssistantMessage` envelope and the renderer
    /// discriminates.
    pub fn assistant_block(uuid: impl Into<String>, block: AssistantContent) -> Self {
        Self::assistant_blocks(uuid, vec![block])
    }

    /// Same, with sibling blocks present (e.g. a `MessageIdentity` carrying
    /// `isApiErrorMessage` alongside the text block); identity blocks render
    /// nothing themselves.
    pub fn assistant_blocks(uuid: impl Into<String>, content: Vec<AssistantContent>) -> Self {
        let uuid = uuid.into();
        Self {
            uuid: uuid.clone(),
            kind: RenderableMessageKind::Assistant {
                message: AssistantMessage {
                    // Row id == message id for a single-block message — the
                    // normalize projection derives split-row ids from the
                    // message uuid.
                    uuid,
                    timestamp: Utc::now(),
                    content,
                    model: None,
                    stop_reason: None,
                    usage: None,
                },
            },
        }
    }

    /// One-block user row constructor (same shape as `assistant_block`).
    /// Producers hand the block to a minimal `UserMessage` envelope and the
    /// renderer discriminates.
    pub fn user_block(uuid: impl Into<String>, block: UserContent) -> Self {
        Self::user_blocks(uuid, vec![block])
    }

    /// Same, with sibling blocks present.
    pub fn user_blocks(uuid: impl Into<String>, content: Vec<UserContent>) -> Self {
        let uuid = uuid.into();
        Self {
            uuid: uuid.clone(),
            kind: RenderableMessageKind::User {
                message: UserMessage {
                    // Row id == message id for a single-block message; see
                    // `assistant_blocks`.
                    uuid,
                    timestamp: Utc::now(),
                    content,
                    is_compact_summary: false,
                    plan_content: None,
                    image_paste_ids: None,
                    is_visible_in_transcript_only: false,
                    mcp_meta: None,
                    source_tool_assistant_uuid: None,
                    permission_mode: None,
                    origin: None,
                    summarize_metadata: None,
                },
            },
        }
    }

    pub fn user(uuid: impl Into<String>, content: impl Into<String>) -> Self {
        Self::user_block(uuid, UserContent::Text(content.into()))
    }

    /// Maps to: CC compact summary — a plain user message with the
    /// `isCompactSummary` envelope bool (`types/message.ts:54`); CC has no
    /// dedicated message type for it. `Message.tsx:141` checks the bool at
    /// the top of the user branch.
    pub fn user_compact_summary(uuid: impl Into<String>, content: impl Into<String>) -> Self {
        let mut row = Self::user(uuid, content);
        if let RenderableMessageKind::User { message, .. } = &mut row.kind {
            message.is_compact_summary = true;
        }
        row
    }

    /// Seam constructor: a one-block tool_result user row. CC's wire shape
    /// (query.ts:136-147, services/tools/toolExecution.ts:397-1040) sets
    /// `is_error: true` for error, reject, and cancel alike — the sentinel
    /// text in `content` is what distinguishes reject/cancel at render time.
    pub fn user_tool_result(
        uuid: impl Into<String>,
        tool_use_id: impl Into<String>,
        content: impl Into<String>,
        is_error: bool,
    ) -> Self {
        Self::user_block(
            uuid,
            UserContent::ToolResult(ToolResult {
                tool_use_id: crate::types::ids::ToolUseId(tool_use_id.into()),
                content: content.into(),
                is_error,
                content_blocks: Vec::new(),
                tool_use_result: None,
            }),
        )
    }

    /// Attach the raw `toolUseResult` to a message built by
    /// [`Self::user_tool_result`]. Maps to: CC recording the tool's typed
    /// output beside the model-facing block; no-op on other message shapes.
    pub fn with_tool_use_result(mut self, tool_use_result: Option<serde_json::Value>) -> Self {
        if let RenderableMessageKind::User { message } = &mut self.kind
            && let Some(UserContent::ToolResult(result)) = message
                .content
                .iter_mut()
                .find(|content| matches!(content, UserContent::ToolResult(_)))
        {
            result.tool_use_result = tool_use_result;
        }
        self
    }

    pub fn system(uuid: impl Into<String>, content: impl Into<String>) -> Self {
        Self::system_notice(uuid, content, SystemMessageLevel::Info)
    }

    /// One informational system row (CC `createSystemMessage`,
    /// `utils/messages.ts:4335-4352`) with row id == message id.
    pub fn system_notice(
        uuid: impl Into<String>,
        content: impl Into<String>,
        level: SystemMessageLevel,
    ) -> Self {
        let uuid = uuid.into();
        Self {
            uuid: uuid.clone(),
            kind: RenderableMessageKind::System(SystemMessage::informational_with_uuid(
                uuid, content, level,
            )),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RenderableMessageKind {
    /// Maps to: CC `Message.tsx:140-192` `case 'user'` — the row carries the
    /// real `UserMessage` and the renderer discriminates in three layers, all
    /// on the render path: the user branch (`isCompactSummary`, image-index
    /// precompute), the block-type switch (`UserMessage`, Message.tsx:284:
    /// text/image/tool_result), and the content-pattern layer
    /// (`UserTextMessage.tsx:48-189` xml-tag if-chain;
    /// `UserToolResultMessage.tsx:43-90` lookups + content-prefix dispatch).
    /// The type-layer copy of those switches (`UserMessageKind`) is removed.
    ///
    /// The `block_index` stepping stone is complete: `normalize_messages`
    /// guarantees one block per row, and the renderer maps `content.iter()`
    /// like CC `Message.tsx:170`.
    User {
        message: UserMessage,
    },
    /// Maps to: CC `Message.tsx:113-138` `case 'assistant'` — the row carries
    /// the real `AssistantMessage` and the renderer discriminates per content
    /// block (`AssistantMessageBlock`'s switch on `param.type`), instead of a
    /// type-layer copy of that switch (`AssistantMessageKind`, removed).
    ///
    /// The `block_index` stepping stone is complete: `normalize_messages`
    /// guarantees one real block per row (plus the out-of-band
    /// `MessageIdentity` sibling), and the renderer maps `content.iter()`
    /// like CC `Message.tsx:116`.
    Assistant {
        message: AssistantMessage,
    },
    /// Maps to: CC `Message.tsx:194-245` `case 'system'` — the row carries the
    /// real `SystemMessage` union member and the renderer discriminates on the
    /// subtype at render time (compact/microcompact/local_command special
    /// cases, then `SystemTextMessage`'s subtype ladder). The type-layer copy
    /// of that ladder (`SystemMessageKind`) is removed (batch D1) — the model
    /// value IS the render value.
    System(SystemMessage),
    /// Maps to: CC `Message.tsx` `case 'attachment'` — the row carries the
    /// typed model [`Attachment`] union value directly (batch D2); the
    /// type-layer render copy (`AttachmentMessageKind`) is removed, exactly
    /// like the System convergence above.
    Attachment(Attachment),
    /// Maps to: CC `types/message.ts:114`
    /// `ProgressMessage<P> = MessageBase & { type: 'progress'; data: P;
    /// toolUseID: string; parentToolUseID: string }`.
    ///
    /// Progress is an independent message in the array, NOT state on a tool-use
    /// row: `normalizeMessages` passes it through (utils/messages.ts:777),
    /// `buildMessageLookups` groups it by `parentToolUseID` (:1214-1223), and
    /// the render list filters it out (`Messages.tsx:590`) — so it feeds the
    /// lookups but never renders itself.
    Progress {
        tool_use_id: String,
        parent_tool_use_id: String,
        /// CC's `data: P`. The Rust payload carrier predates this variant and
        /// is kept as-is; payload-shape parity with CC's `ToolProgressData` /
        /// `HookProgress` is tracked separately.
        data: ToolUseProgressMessage,
    },
    GroupedToolUse(GroupedToolUseMessage),
    CollapsedReadSearch(CollapsedReadSearchGroup),
}

// `UserMessageKind` is gone. It was a type-layer copy of CC's three
// render-path discrimination layers: the user branch (`Message.tsx:140`), the
// block-type switch (`Message.tsx:284`: text/image/tool_result), and the
// content-pattern layer (`UserTextMessage.tsx` xml-tag if-chain;
// `UserToolResultMessage.tsx` lookups + content-prefix dispatch). Rows carry
// the real `UserMessage`; constructors emit the actual wire text (xml tags
// from `constants/xml`), so the flattened variants — and their re-wrapping
// round-trips like `<user-memory-input>` — have no place to live.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SearchResultMode {
    #[default]
    FilesWithMatches,
    Content,
    Count,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReadResultKind {
    #[default]
    Unknown,
    Text,
    Image,
    Notebook,
    Pdf,
    Parts,
    FileUnchanged,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StructuredDiffHunk {
    pub old_start: usize,
    pub old_lines: usize,
    pub new_start: usize,
    pub new_lines: usize,
    pub lines: Vec<String>,
}

impl StructuredDiffHunk {
    /// CC `hunkSchema` wire projection (`FileEditTool/types.ts:36-44`) — the
    /// shape `structuredPatch` records inside `toolUseResult`.
    pub fn to_official_json(&self) -> serde_json::Value {
        serde_json::json!({
            "oldStart": self.old_start,
            "oldLines": self.old_lines,
            "newStart": self.new_start,
            "newLines": self.new_lines,
            "lines": self.lines,
        })
    }

    /// The Rust stand-in for `hunkSchema` validation: all five keys required.
    pub fn from_official_json(value: &serde_json::Value) -> Option<Self> {
        let map = value.as_object()?;
        let number =
            |key: &str| -> Option<usize> { map.get(key)?.as_u64().map(|number| number as usize) };
        Some(Self {
            old_start: number("oldStart")?,
            old_lines: number("oldLines")?,
            new_start: number("newStart")?,
            new_lines: number("newLines")?,
            lines: map
                .get("lines")?
                .as_array()?
                .iter()
                .map(|line| line.as_str().map(ToOwned::to_owned))
                .collect::<Option<Vec<_>>>()?,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolResultStatus {
    Success,
    Error,
    Rejected,
    Canceled,
}

// `AssistantMessageKind` is gone (P8 A2.3). It was a type-layer copy of CC's
// render-time switch: `Message.tsx` carries the whole `AssistantMessage` and
// `AssistantMessageBlock` discriminates per content block (`param.type`).
// `RenderableMessageKind::Assistant { message }` now carries the real
// message; the per-block match lives in `render_assistant`.
// Removed with it: `Thinking.expanded` (dead — every constructor wrote false),
// `ToolUse.description` (render-time derivation), and
// `TransparentToolUseProgress` (CC branches on `tool.isTransparentWrapper?.()`
// inside AssistantToolUseMessage.tsx:124, not a message type).

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolUseStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
}

/// Maps to: CC `Progress = ToolProgressData | HookProgress` (`Tool.ts:305`) —
/// the `ProgressMessage.data` payload vocabulary. CC never names the tool
/// payloads on the live wire (`types/tools.ts` is a generated stub); each
/// variant here mirrors one real producer's object literal, discriminated by
/// `data.type` (see [`ToolUseProgressMessage::wire_type`]).
///
/// CC 2.1.88 vocabulary, with the producer behind every tag:
/// - `bash_progress`          — `tools/BashTool/BashTool.tsx:900-912`
/// - `powershell_progress`    — `tools/PowerShellTool/PowerShellTool.tsx:635-647`
///   (same fields as `bash_progress`; seam: Nyan has no PowerShell tool)
/// - `mcp_progress`           — `services/mcp/client.ts:1847-1855` (started),
///   `:1885-1894` (completed), `:1926-1935` (failed), `:3104-3112` (progress)
/// - `query_update`           — `tools/WebSearchTool/WebSearchTool.ts:347-353`
/// - `search_results_received`— `tools/WebSearchTool/WebSearchTool.ts:377-384`
/// - `waiting_for_task`       — `tools/TaskOutputTool/TaskOutputTool.tsx:281-287`
/// - `agent_progress`         — `tools/AgentTool/AgentTool.tsx:1084-1092,
///   1495-1505` (`{ message, prompt, agentId }` — carries a whole normalized
///   message)
/// - `skill_progress`         — `tools/SkillTool/SkillTool.ts:250-258`
///   (same shape as `agent_progress`)
/// - `hook_progress`          — `types/hooks.ts:234-241`, produced at
///   `utils/hooks.ts:2096-2115`; the Rust carrier is the actor event
///   `StopHookProgressEvent` (query.rs), not this union — a documented seam
/// - `sleep_progress`         — named only behind PROACTIVE/KAIROS in
///   `utils/sessionStorage.ts:190-192`; no producer in the tree (ant-only/DCE)
///
/// `AgentProgress`/`SkillProgress` carry the whole normalized `data.message`
/// like CC, and every display row is derived at the renderer
/// (`AgentTool/UI.tsx`). The three flattened `Subagent*` variants they replace
/// derived those rows at the PRODUCER (`run_agent.rs`), so each render-side
/// field the producer had not anticipated had to be retrofitted onto the
/// carrier one at a time.
///
/// No serde: CC never serializes progress payloads. Progress is filtered from
/// every JSONL write (`isLoggableMessage`, `utils/sessionStorage.ts:4351-4352`;
/// both `insertMessageChain` callers go through `cleanMessagesForLogging`) and
/// skipped on load (`isTranscriptMessage`, `:139-145`). The SDK `tool_progress`
/// event is a separate projection built field-by-field
/// (`utils/queryHelpers.ts:189-199`), not a serialization of this type.
#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq)]
pub enum ToolUseProgressMessage {
    /// CC `bash_progress` (`BashTool.tsx:900-912`); optionality follows the
    /// `runShellCommand` yield type (`BashTool.tsx:1123-1132`): `totalBytes`,
    /// `taskId`, and `timeoutMs` are the only optional keys.
    BashProgress {
        output: String,
        full_output: String,
        elapsed_time_seconds: u64,
        total_lines: usize,
        total_bytes: Option<u64>,
        task_id: Option<String>,
        timeout_ms: Option<u64>,
    },
    /// CC `query_update` (`WebSearchTool.ts:347-353`).
    QueryUpdate { query: String },
    /// CC `search_results_received` (`WebSearchTool.ts:377-384`).
    SearchResultsReceived { query: String, result_count: usize },
    /// CC `waiting_for_task` (`TaskOutputTool.tsx:281-287`); both fields come
    /// from the required `TaskState` `description`/`type`.
    WaitingForTask {
        task_description: String,
        task_type: String,
    },
    /// CC `mcp_progress` — union of the four producer literals
    /// (`services/mcp/client.ts:1847-1855, 1885-1894, 1926-1935, 3104-3112`):
    /// `status`/`serverName`/`toolName` are always present; `elapsedTimeMs`
    /// only on completed/failed; `progress`/`total`/`progressMessage` only on
    /// SDK `onprogress` forwarding. Seam: Nyan MCP calls do not emit
    /// progress yet — no Rust producer.
    McpProgress {
        /// `'started' | 'progress' | 'completed' | 'failed'`.
        status: String,
        server_name: String,
        tool_name: String,
        elapsed_time_ms: Option<u64>,
        progress: Option<f64>,
        total: Option<f64>,
        progress_message: Option<String>,
    },
    /// CC `agent_progress` (`AgentTool.tsx:1084-1092`, `:1494-1506`) — the
    /// whole normalized message, exactly as CC carries it. Every display row
    /// is derived from it at the renderer (`AgentTool/UI.tsx`), which is where
    /// CC derives them too.
    ///
    /// `Box<RenderableMessage>` because CC's `data.message` is one member of
    /// `normalizeMessages([message])`, and `NormalizedMessage` ≙ Rust
    /// `RenderableMessage`.
    AgentProgress {
        message: Box<RenderableMessage>,
        /// CC `data.prompt` — read only off `progressMessages[0]`
        /// (`UI.tsx:637-639`), which is why the loop literal sends `''`.
        ///
        /// The non-empty value comes from ONE producer, the metadata-first
        /// progress message CC yields before the run loop ("Yield initial
        /// progress message to carry metadata (prompt)",
        /// `AgentTool.tsx:1073-1094`), gated on `promptMessages.length > 0 &&
        /// onProgress`. Ported at
        /// `tools/agent_tool/mod.rs#emit_initial_agent_progress`, called from
        /// the sync spawn branch which now owns `promptMessages` construction
        /// exactly as CC's AgentTool does. Every later message goes through the
        /// loop literal (`run_agent.rs`) and carries `""`.
        ///
        /// Remaining seam: the background/async spawn branches do not emit it —
        /// CC does not either (`:1044` gates the producer to the sync `else`
        /// arm), so a backgrounded agent's transcript has no Prompt block in
        /// both implementations.
        prompt: String,
        /// CC `data.agentId`.
        agent_id: String,
    },
    /// CC `skill_progress` (`SkillTool.ts:250-258`) — the same payload shape.
    /// Seam: no Rust producer (the forked-skill path delegates to `run_agent`,
    /// which emits `agent_progress`); the row derivation is shared with
    /// [`Self::AgentProgress`] and the renderer choice is made by tool name,
    /// exactly like CC's per-tool `renderToolUseProgressMessage` member.
    SkillProgress {
        message: Box<RenderableMessage>,
        prompt: String,
        agent_id: String,
    },
}

impl ToolUseProgressMessage {
    /// The `data.message` of an `agent_progress` / `skill_progress` payload.
    ///
    /// Maps to: CC `AgentTool/UI.tsx:61-67` `hasProgressMessage` — the guard
    /// every UI helper runs before touching `data.message`, which skips
    /// payloads from other progress producers (e.g. a forwarded
    /// `bash_progress`).
    pub fn subagent_progress_message(&self) -> Option<&RenderableMessage> {
        match self {
            Self::AgentProgress { message, .. } | Self::SkillProgress { message, .. } => {
                Some(message)
            }
            _ => None,
        }
    }

    /// CC `data.type` discriminant for this payload — the string every CC
    /// consumer switches on (`Tool.ts:317`, `REPL.tsx:3470`,
    /// `utils/sessionStorage.ts:194-196`, `utils/queryHelpers.ts:122-159`).
    pub fn wire_type(&self) -> &'static str {
        match self {
            Self::BashProgress { .. } => "bash_progress",
            Self::QueryUpdate { .. } => "query_update",
            Self::SearchResultsReceived { .. } => "search_results_received",
            Self::WaitingForTask { .. } => "waiting_for_task",
            Self::McpProgress { .. } => "mcp_progress",
            Self::AgentProgress { .. } => "agent_progress",
            Self::SkillProgress { .. } => "skill_progress",
        }
    }
}

// Progress values are produced by controlled UI-only seams; tests never use
// NaN. Keep Eq available so `RenderableMessage` values retain exact snapshot keys.
impl Eq for ToolUseProgressMessage {}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StopHookInfo {
    pub command: Option<String>,
    pub prompt_text: Option<String>,
    pub duration_ms: Option<u64>,
    pub output: Option<String>,
    pub error: Option<String>,
    pub prevented_continuation: bool,
}

// `SystemMessageKind` is gone (batch D1). It was the render-layer copy of the
// CC `SystemMessage` union; the model enum above IS the union now, and
// `render_system` discriminates on it directly like CC `Message.tsx:194-245`
// plus `SystemTextMessage.tsx`'s subtype ladder.
//
// `AttachmentMessageKind` is gone (batch D2). It was the render-layer copy of
// the CC `Attachment` union; the model union (`utils/attachments.rs`,
// re-exported above) IS the render value now, and the renderer discriminates
// on it directly like CC `AttachmentMessage.tsx:162`. Rust-only variants died
// with it: `NewFile`/`NewDirectory` (CC migrates those legacy wire tags at the
// recovery seam, conversationRecovery.ts:88-110, instead of typing them),
// `Skill`/`Memory`/`Hook` (no such attachment tags exist in CC — quoted
// queries in the batch-D2 report; CC has `invoked_skills`, `nested_memory`,
// and the nine typed `hook_*` members instead).

/// Maps to: CC `types/message.ts:140-144`
///
/// ```ts
/// GroupedToolUseMessage = { type: 'grouped_tool_use'; messages: Message[]; uuid: string
///   toolName: string; results: Array<{ message: Message; toolUseResult?: unknown }>
///   hookInfos?: StopHookInfo[] }
/// ```
///
/// `uuid` lives on the enclosing `RenderableMessage`, `toolName` is
/// `tool_name`. The old `summary`/`count`/per-item `result_*` fields were
/// pre-baked render products; `GroupedToolUseContent` now derives them from
/// the rows at render time, exactly like CC `GroupedToolUseContent.tsx:35-63`.
/// `hookInfos` is absent because the Rust group never carried hook info — a
/// seam, not a deletion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupedToolUseMessage {
    pub tool_name: String,
    /// CC `messages: Message[]` — the grouped single-block tool_use rows.
    /// Stepping stone: the row type until C3 converges rows to `Message`.
    pub messages: Vec<RenderableMessage>,
    /// CC `results` — CC pairs each result message with its `toolUseResult`;
    /// in Rust the raw `toolUseResult` rides the row's tool_result block
    /// (`ToolResult.tool_use_result`), so a plain row vec suffices.
    pub results: Vec<RenderableMessage>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CollapsedReadSearchEntry {
    ToolUse {
        tool_name: String,
        /// Original structured ToolUse input. Collapsed verbose rows must
        /// rerender through the owning tool UI rather than a display summary.
        input: Option<serde_json::Value>,
        /// Original `ToolUseBlock.id` (`param.id` in CC), needed by loaders,
        /// classifier state, and tool-owned row projections.
        tool_use_id: Option<String>,
        description: String,
        status: ToolUseStatus,
    },
    ToolResult {
        tool_name: String,
        status: ToolResultStatus,
        content: String,
        /// CC `message.toolUseResult` — the raw the by-name renderer consumes.
        tool_use_result: Option<serde_json::Value>,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CollapsedReadSearchGroup {
    pub read_count: usize,
    pub search_count: usize,
    pub list_count: usize,
    pub bash_count: usize,
    pub git_op_bash_count: usize,
    pub commits: Vec<crate::tools::shared::git_operation_tracking::GitCommitSummary>,
    pub pushes: Vec<crate::tools::shared::git_operation_tracking::GitPushSummary>,
    pub branches: Vec<crate::tools::shared::git_operation_tracking::GitBranchSummary>,
    pub prs: Vec<crate::tools::shared::git_operation_tracking::GitPrSummary>,
    pub mcp_call_count: usize,
    pub mcp_server_names: Vec<String>,
    pub memory_search_count: usize,
    pub memory_read_count: usize,
    pub memory_write_count: usize,
    pub team_memory_search_count: usize,
    pub team_memory_read_count: usize,
    pub team_memory_write_count: usize,
    pub hook_total_ms: Option<u64>,
    pub hook_count: usize,
    pub hook_infos: Vec<StopHookInfo>,
    pub relevant_memories: Vec<RelevantMemory>,
    pub verbose_entries: Vec<CollapsedReadSearchEntry>,
    pub hint: String,
    pub active: bool,
    pub errored: bool,
}

// `CompactSummaryMessage` is gone. CC has no dedicated message
// type: a compact summary is a user message with the `isCompactSummary`
// envelope bool, checked at the top of the user branch (Message.tsx:141).

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_reason_serializes_official_snake_case_values() {
        assert_eq!(
            serde_json::to_value(StopReason::ToolUse).unwrap(),
            serde_json::json!("tool_use")
        );
        assert_eq!(
            serde_json::from_value::<StopReason>(serde_json::json!("pause_turn")).unwrap(),
            StopReason::PauseTurn
        );
        assert_eq!(
            serde_json::from_value::<StopReason>(serde_json::json!("refusal")).unwrap(),
            StopReason::Refusal
        );
    }

    #[test]
    fn system_api_error_preserves_error_details_for_stop_failure_hooks() {
        let message = SystemApiErrorMessage {
            content: "API Error".to_string(),
            api_error: "formatted".to_string(),
            error: "error".to_string(),
            error_details: Some("raw details".to_string()),
        };

        let json = serde_json::to_value(&message).unwrap();
        assert_eq!(json["error_details"], "raw details");

        let round_trip: SystemApiErrorMessage = serde_json::from_value(json).unwrap();
        assert_eq!(round_trip.error_details.as_deref(), Some("raw details"));
    }

    #[test]
    fn hook_result_message_serializes_like_official_message() {
        let message = HookResultMessage::attachment(serde_json::json!({
            "type": "hook_additional_context",
            "content": ["context"]
        }));

        let json = serde_json::to_value(&message).unwrap();
        assert_eq!(json["type"], "hook_result");
        assert_eq!(json["attachment"]["type"], "hook_additional_context");
        assert!(json["uuid"].as_str().is_some_and(|uuid| !uuid.is_empty()));
    }

    #[test]
    fn tool_use_summary_serializes_like_official_message() {
        let message = ToolUseSummaryMessage::new(
            "Read manifest".to_string(),
            vec!["toolu_1".to_string()],
            "summary-uuid".to_string(),
        );

        let json = serde_json::to_value(&message).unwrap();
        assert_eq!(json["type"], "tool_use_summary");
        assert_eq!(json["summary"], "Read manifest");
        assert_eq!(json["precedingToolUseIds"], serde_json::json!(["toolu_1"]));
    }
}

impl Message {
    pub fn timestamp(&self) -> DateTime<Utc> {
        match self {
            Message::User(m) => m.timestamp,
            Message::Assistant(m) => m.timestamp,
            Message::System(m) => m.timestamp(),
            Message::Attachment(m) => m.timestamp,
            Message::Progress(m) => m.timestamp,
            Message::HookResult(m) => m.timestamp,
        }
    }

    /// The `MessageBase.uuid` envelope (CC `types/message.ts:29-34`), present
    /// on every union member.
    pub fn uuid(&self) -> &str {
        match self {
            Message::User(m) => &m.uuid,
            Message::Assistant(m) => &m.uuid,
            Message::System(m) => m.uuid(),
            Message::Attachment(m) => &m.uuid,
            Message::Progress(m) => &m.uuid,
            Message::HookResult(m) => &m.uuid,
        }
    }
}

/// One entry of the REPL's single conversation history.
///
/// Maps to: CC `screens/REPL.tsx:1650` `const [messages, rawSetMessages] =
/// useState<MessageType[]>(initialMessages ?? [])` — CC keeps ONE array of
/// `Message` values; the render list is a projection over it
/// (`Messages.tsx` via `utils/messages.ts#normalizeMessages`) and the API
/// history is the same array. [`HistoryEntry::Message`] is that CC shape.
///
/// The other two variants are audited dual-carrier residue (batch D3) — CC
/// has no counterpart. The batch-D reason ("rich System/Attachment fields
/// cannot ride the flat model structs") is DEAD: system notices, scheduled
/// fires, progress, attachments, compact boundaries/summaries, and fallback
/// warnings all travel as [`HistoryEntry::Message`] now. What remains is
/// per-flow, each with a named blocker (see `QueryEvent::Row`'s residue list
/// and `seed_history_entries`):
///
/// - [`HistoryEntry::Row`]: a render-only transcript row whose model half is
///   a DIFFERENT value or already present — tool-execution rows carrying the
///   raw `toolUseResult`, the cold-resume render prefix
///   (render-approximation parser), mock pending rows, and grouped/collapse
///   products (`GroupedToolUse` — a `RenderableMessage` widening member, NOT
///   a `Message` member, CC types/message.ts:140-146, so it can never be
///   anything but a row). Skipped by the model projection. Streamed
///   per-block assistants left this list: they arrive as whole
///   `HistoryEntry::Message` values (CC claude.ts:2192-2211, REPL.tsx:3496)
///   and CC's shared-reference `usage` back-fill travels as
///   `QueryEvent::AssistantDelta` applied to the entry in place.
/// - [`HistoryEntry::ModelOnly`]: the history-only half of the same flows —
///   the `Message` whose render half arrived (or was seeded) as `Row`
///   entries. Skipped by the render projection. The post-compact replay and
///   the /compact kept-messages legs left this list (task #9): compact flows
///   now reset the history to the whole-`Message` post-compact block (CC
///   REPL.tsx:3458-3463, query.ts:528-535). The streamed-assistant leg left
///   too; the remaining streamed remnant is the abort/API-error partial
///   built from never-completed block buffers (render half = the REPL
///   streaming preview).
///
/// Both projections read the one vec in order, so a `Row` arriving between
/// two `Message` entries renders between them and the API history keeps
/// arrival order — CC's single-array ordering guarantee.
#[derive(Debug, Clone, PartialEq)]
pub enum HistoryEntry {
    /// CC-shaped entry: model history AND render projection
    /// (`normalize_messages`).
    Message(Message),
    /// Batch-D seam residue: render-only row; never model history.
    Row(RenderableMessage),
    /// Batch-D seam residue: model-history-only message whose render half is
    /// carried by `Row` entries; never rendered.
    ModelOnly(Message),
}

impl HistoryEntry {
    /// The entry's identity for CC `setMessages`-style updates: the message
    /// envelope uuid, or the row uuid for render-only entries.
    pub fn uuid(&self) -> &str {
        match self {
            HistoryEntry::Message(message) | HistoryEntry::ModelOnly(message) => message.uuid(),
            HistoryEntry::Row(row) => &row.uuid,
        }
    }
}
