//! 1:1 Rust port of CC `services/api/claude.ts`.
//!
//! Maps to: CC services/api/claude.ts:1-3420
//!
//! This module contains the Claude API interaction layer for CometixCode:
//! - Extra body parameter assembly from env vars and beta headers
//! - Prompt caching configuration and cache control
//! - Message conversion (UserMessage/AssistantMessage -> API MessageParam)
//! - Usage tracking and accumulation across turns
//! - API key verification
//! - Streaming and non-streaming query wrappers
//! - System prompt block construction
//! - Convenience query wrappers (queryHaiku, queryWithModel)
//!
//! API connectivity follows the same config/env path as Claude Code: direct
//! Anthropic settings are read from the environment or explicit `--settings`.

use crate::constants::api_limits::API_MAX_MEDIA_PER_REQUEST;
use crate::types::message::{StreamEvent, SystemApiErrorMessage};
use crate::types::tools::Tool;
use crate::utils::betas::{
    ADVISOR_BETA_HEADER, CONTEXT_MANAGEMENT_BETA_HEADER, EFFORT_BETA_HEADER,
    PROMPT_CACHING_SCOPE_BETA_HEADER, REDACT_THINKING_BETA_HEADER, STRUCTURED_OUTPUTS_BETA_HEADER,
    TASK_BUDGETS_BETA_HEADER, TOOL_SEARCH_BETA_HEADER_3P, get_bedrock_extra_body_params_betas,
    get_merged_betas, get_model_betas, get_tool_search_beta_header,
    model_supports_structured_outputs, should_include_first_party_only_betas,
    should_use_global_cache_scope,
};
use crate::utils::context::{CAPPED_DEFAULT_MAX_TOKENS, get_model_max_output_tokens};
use crate::utils::effort::{EffortValue, model_supports_effort, resolve_applied_effort};
use crate::utils::content_array::insert_block_after_tool_results;
use crate::utils::model::model::{
    get_default_opus_model, get_default_sonnet_model, get_main_loop_model, get_small_fast_model,
    normalize_model_string_for_api,
};
use crate::utils::process_env::JsTruthy;
use crate::utils::thinking::{
    ThinkingConfig, model_supports_adaptive_thinking, model_supports_thinking,
    production_thinking_config_from_env_and_settings,
};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Stub types for modules not yet ported
// ---------------------------------------------------------------------------

/// Serialized query source string from `constants/query_source.rs`.
type ApiQuerySource = crate::constants::query_source::QuerySourceString;

/// Maps to: CC `services/api/claude.ts` `import type { CacheScope } from '../../utils/api.js'`.
pub use crate::utils::api::CacheScope;

/// Stub for CC `SystemPrompt` (utils/systemPromptType.ts).
/// In CC this is `string[]`.
/// TODO: Port from CC utils/systemPromptType.ts
pub type SystemPrompt = Vec<String>;

/// Maps to: CC `AgentDefinition` (tools/AgentTool/loadAgentsDir.ts), imported
/// by `services/api/claude.ts` for `Options.agents`. The real definition owner
/// is `tools::agent_tool::load_agents_dir`; this alias replaces the old
/// name-only stub now that `toolToAPISchema` forwards the agents into
/// `Tool.prompt(options)` (claude.ts:1240 → api.ts:174).
pub type AgentDefinition = crate::tools::agent_tool::load_agents_dir::AgentDefinition;

/// Stub for CC `Notification` (context/notifications.ts).
/// TODO: Port from CC context/notifications.ts
#[derive(Debug, Clone)]
pub struct Notification {
    pub message: String,
}

/// Stub for CC `ToolPermissionContext` (Tool.ts).
/// TODO: Port from CC Tool.ts
#[derive(Debug, Clone, Default)]
pub struct ToolPermissionContext {
    pub mode: String,
}

/// Maps to CC `Tool.ts` `QueryChainTracking`.
pub type QueryChainTracking = crate::tool::QueryChainTracking;

/// Stub for CC `BetaToolUnion` (SDK type).
/// TODO: Use anthropic_sdk::ToolUnion when wired
pub type BetaToolUnion = serde_json::Value;

/// Stub for CC `BetaToolChoiceAuto | BetaToolChoiceTool` (SDK types).
/// TODO: Use anthropic_sdk::ToolChoice when wired
pub type ToolChoice = serde_json::Value;

/// Stub for CC `BetaJSONOutputFormat` (SDK type).
/// TODO: Use anthropic_sdk output format type when wired
pub type JsonOutputFormat = serde_json::Value;

/// Stub for CC `BetaOutputConfig` (SDK type).
/// TODO: Use anthropic_sdk output config type when wired
pub type OutputConfig = serde_json::Map<String, JsonValue>;

// Re-use the project's existing message types
use crate::types::ids::AgentId;
use crate::types::message::{AssistantMessage, Message, UserMessage};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maps to: CC services/api/claude.ts:3354
/// Non-streaming requests have a 10min max per the docs.
/// The SDK's 21333-token cap is derived from 10min x 128k tokens/hour, but we
/// bypass it by setting a client-level timeout, so we can cap higher.
pub const MAX_NON_STREAMING_TOKENS: u32 = 64_000;

// Beta header constants are sourced from `utils/betas.rs`, which maps to
// CC `constants/betas.ts` / `utils/betas.ts`.

// ---------------------------------------------------------------------------
// Usage types mirroring CC services/api/logging.ts
// ---------------------------------------------------------------------------

/// Maps to: CC services/api/logging.ts:NonNullableUsage
/// Cumulative token usage statistics with no nullable fields.
/// All fields default to 0 rather than being Option.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct NonNullableUsage {
    pub input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub output_tokens: u64,
    pub server_tool_use: ServerToolUse,
    pub service_tier: Option<String>,
    pub cache_creation: CacheCreation,
    pub inference_geo: Option<String>,
    pub iterations: Option<u64>,
    pub speed: Option<String>,
}

/// Maps to: CC services/api/logging.ts:NonNullableUsage.server_tool_use
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServerToolUse {
    pub web_search_requests: u64,
    pub web_fetch_requests: u64,
}

/// Maps to: CC services/api/logging.ts:NonNullableUsage.cache_creation
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CacheCreation {
    pub ephemeral_1h_input_tokens: u64,
    pub ephemeral_5m_input_tokens: u64,
}

/// Maps to: CC services/api/logging.ts:EMPTY_USAGE
pub const EMPTY_USAGE: NonNullableUsage = NonNullableUsage {
    input_tokens: 0,
    cache_creation_input_tokens: 0,
    cache_read_input_tokens: 0,
    output_tokens: 0,
    server_tool_use: ServerToolUse {
        web_search_requests: 0,
        web_fetch_requests: 0,
    },
    service_tier: None,
    cache_creation: CacheCreation {
        ephemeral_1h_input_tokens: 0,
        ephemeral_5m_input_tokens: 0,
    },
    inference_geo: None,
    iterations: None,
    speed: None,
};

/// Maps to: CC services/api/logging.ts:GlobalCacheStrategy
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GlobalCacheStrategy {
    None,
    SystemPrompt,
}

// ---------------------------------------------------------------------------
// Stream event types
// ---------------------------------------------------------------------------

/// Renderable items accumulated from Anthropic streaming content blocks.
///
/// This is intentionally still below the tool-execution layer: it records what
/// the model requested, but does not run tools or write sessions.
#[derive(Debug, Clone, PartialEq)]
pub enum ClaudeStreamItem {
    Text(String),
    /// Thinking text (+ optional signature).
    ///
    /// Delta preview items use `signature: ""`. Completed blocks from
    /// `content_block_stop` carry the signature accumulated from
    /// `signature_delta` (CC `services/api/claude.ts`).
    Thinking {
        text: String,
        signature: String,
    },
    RedactedThinking(String),
    ToolUse {
        id: String,
        name: String,
        input: JsonValue,
        is_server: bool,
    },
    WebSearchToolResult {
        tool_use_id: String,
        content: JsonValue,
    },
}

// ---------------------------------------------------------------------------
// Cache control
// ---------------------------------------------------------------------------

/// Maps to: CC services/api/claude.ts:358-374 cache-control object shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheControl {
    #[serde(rename = "type")]
    pub cache_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttl: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<CacheScope>,
}

/// Maps to: CC services/api/claude.ts:358-374 `getCacheControl({scope, querySource})`.
///
/// `scope` is only emitted when it is `global` (`...(scope === 'global' && { scope })`);
/// `org` is the API default and stays off the wire.
///
/// @cometix offset: first-party `ttl: "1h"` is gated by `PromptCache1hConfig`.
/// The query-source allowlist is settings.json `promptCache1h.allowlist`.
/// Vertex / Foundry / Bedrock keep `should1hCacheTTL`.
pub fn get_cache_control(scope: Option<CacheScope>, query_source: Option<&str>) -> CacheControl {
    CacheControl {
        cache_type: "ephemeral".to_string(),
        ttl: should_1h_cache_ttl(query_source).then(|| "1h".to_string()),
        scope: (scope == Some(CacheScope::Global)).then_some(CacheScope::Global),
    }
}

fn should_1h_cache_ttl(query_source: Option<&str>) -> bool {
    // Maps to CC `services/api/claude.ts:393-434` `should1hCacheTTL(...)`.
    let provider = crate::utils::model::providers::get_api_provider();
    let bedrock_opt_in = crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("ENABLE_PROMPT_CACHING_1H_BEDROCK")
            .as_deref(),
    );
    should_1h_cache_ttl_with_inputs(
        provider,
        bedrock_opt_in,
        prompt_cache_1h_user_eligible(),
        &prompt_cache_1h_allowlist(),
        query_source,
        crate::utils::feature_flags::feature_enabled(
            crate::utils::feature_flags::FeatureFlag::PromptCache1hConfig,
        ),
    )
}

/// Maps to CC `should1hCacheTTL` eligibility:
/// `USER_TYPE === 'ant' || (isClaudeAISubscriber() && !currentLimits.isUsingOverage)`.
fn prompt_cache_1h_user_eligible() -> bool {
    if crate::utils::build_profile::has_internal_capability(
        crate::utils::build_profile::InternalCapability::Api,
    ) {
        return true;
    }
    crate::utils::auth::is_claude_ai_subscriber()
        && !crate::services::claude_ai_limits::current_limits().is_using_overage
}

/// Maps to CC `getFeatureValue_CACHED_MAY_BE_STALE('tengu_prompt_cache_1h_config', {})`
/// then `config.allowlist ?? []`.
///
/// @cometix offset: GrowthBook is not ported; the same `{ allowlist }` payload
/// is read from settings.json via `get_prompt_cache_1h_allowlist`.
fn prompt_cache_1h_allowlist() -> Vec<String> {
    // @cometix offset: official reads GrowthBook `tengu_prompt_cache_1h_config`.
    // Users set the same `{ allowlist }` payload in settings.json.
    crate::utils::settings::get_prompt_cache_1h_allowlist()
}

fn prompt_cache_1h_allowlist_from_feature_value(value: Option<&serde_json::Value>) -> Vec<String> {
    value
        .and_then(|value| value.get("allowlist"))
        .and_then(|value| value.as_array())
        .map(|allowlist| {
            allowlist
                .iter()
                .filter_map(|entry| entry.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn prompt_cache_1h_allowlist_matches(allowlist: &[String], query_source: &str) -> bool {
    allowlist.iter().any(|pattern| {
        if let Some(prefix) = pattern.strip_suffix('*') {
            query_source.starts_with(prefix)
        } else {
            query_source == pattern
        }
    })
}

fn should_1h_cache_ttl_with_inputs(
    provider: crate::utils::model::providers::ApiProvider,
    bedrock_opt_in: bool,
    user_eligible: bool,
    allowlist: &[String],
    query_source: Option<&str>,
    first_party_config_enabled: bool,
) -> bool {
    use crate::utils::model::providers::ApiProvider;

    // Official: Bedrock users opt in via env and skip GrowthBook.
    if provider == ApiProvider::Bedrock && bedrock_opt_in {
        return true;
    }

    if provider != ApiProvider::FirstParty {
        // Official remaining path for Vertex / Foundry / Bedrock-without-opt-in:
        // eligible user + `tengu_prompt_cache_1h_config.allowlist` match.
        if !user_eligible {
            return false;
        }
        let Some(query_source) = query_source else {
            return false;
        };
        return prompt_cache_1h_allowlist_matches(allowlist, query_source);
    }

    // @cometix offset: first-party uses `PromptCache1hConfig` as the final
    // control. An empty settings allowlist means "any querySource"; a
    // non-empty list uses the official prefix-match rules.
    if !first_party_config_enabled {
        return false;
    }
    let Some(query_source) = query_source else {
        return false;
    };
    if allowlist.is_empty() {
        return true;
    }
    prompt_cache_1h_allowlist_matches(allowlist, query_source)
}

// ---------------------------------------------------------------------------
// Task budget param
// ---------------------------------------------------------------------------

/// Maps to: CC services/api/claude.ts:473-477
/// API-side token budget awareness for the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskBudgetParam {
    #[serde(rename = "type")]
    pub budget_type: String,
    pub total: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remaining: Option<u64>,
}

// ---------------------------------------------------------------------------
// Options type
// ---------------------------------------------------------------------------

/// Maps to: CC services/api/claude.ts:676-707
/// Options struct for query model calls. Callbacks are represented as trait
/// objects or closures where CC uses function references.
#[derive(Debug, Clone)]
pub struct Options {
    pub model: String,
    pub is_non_interactive_session: bool,
    pub query_source: ApiQuerySource,
    pub agents: Vec<AgentDefinition>,
    pub has_append_system_prompt: bool,

    // Optional fields
    pub tool_choice: Option<ToolChoice>,
    pub extra_tool_schemas: Option<Vec<BetaToolUnion>>,
    pub max_output_tokens_override: Option<u32>,
    pub fallback_model: Option<String>,
    pub enable_prompt_caching: Option<bool>,
    pub skip_cache_write: Option<bool>,
    pub temperature_override: Option<f64>,
    pub effort_value: Option<EffortValue>,
    /// Maps to: CC `services/api/claude.ts:694` `mcpTools: Tools`.
    ///
    /// **Write-only, exactly as in CC.** The only producer is `query.ts:689`
    /// (`mcpTools: appState.mcp.tools`) plus the `mcpTools: []` literals at the
    /// text-only call sites; CC has no reader anywhere:
    ///
    /// ```text
    /// ast-grep --lang ts  -p 'options.mcpTools' src/  -> 0
    /// ast-grep --lang tsx -p 'options.mcpTools' src/  -> 0
    /// ast-grep --lang ts  -p 'options.hasPendingMcpServers' \
    ///     src/services/api/claude.ts -> claude.ts:1142   (control: form works)
    /// ```
    ///
    /// The value is `appState.mcp.tools` RAW — it has NOT been through
    /// `assembleToolPool`'s deny filter (`tools.ts:352`). Reading it back and
    /// chaining it onto the `tools` parameter re-opens the `mcp__server` deny
    /// bypass *and* puts a second copy of every MCP tool on the wire. Every
    /// request-side tool list must come from the `tools` parameter, which is
    /// already the assembled, deny-filtered pool.
    pub mcp_tools: Vec<Tool>,
    pub has_pending_mcp_servers: Option<bool>,
    pub query_tracking: Option<QueryChainTracking>,
    pub agent_id: Option<AgentId>,
    pub output_format: Option<JsonOutputFormat>,
    pub fast_mode: Option<bool>,
    pub advisor_model: Option<String>,
    pub allowed_agent_types: Option<Vec<String>>,
    /// Maps to: CC `services/api/claude.ts:677`
    /// `getToolPermissionContext: () => Promise<ToolPermissionContext>` — a
    /// REQUIRED member of CC's Options, consumed only by `toolToAPISchema`
    /// (`claude.ts:1238` → `api.ts:172`) to feed `Tool.prompt(options)`.
    ///
    /// Carried as a resolved snapshot, not a factory: CC's member is async
    /// only because its one producer is an async `getAppState()` read
    /// (`query.ts:666-669`); every Rust producer resolves synchronously at
    /// request build, and on the actor path there is no await between the
    /// snapshot and its use inside `build_sdk_message_create_plan` (a sync
    /// `fn`), so the evaluation position is preserved. `None` (the
    /// `Options::new` default kept for the text-only/no-tools call sites)
    /// projects to the empty permission context at the serialization site.
    pub tool_permission_context: Option<crate::tool::ToolPermissionContext>,
    pub task_budget: Option<TaskBudget>,
    /// Maps to CC cached microcompact `useCachedMC` request assembly gate.
    pub cached_mc_enabled: bool,
    /// Maps to CC `newCacheEdits` consumed from microCompact before SDK params.
    pub cached_mc_new_cache_edits: Option<CachedMcEditsBlock>,
    /// Maps to CC `pinnedEdits` re-sent at their original user message index.
    pub cached_mc_pinned_edits: Vec<CachedMcPinnedEdit>,
    /// Maps to CC `toolUseContext.abortController.signal` passed into
    /// `queryModelWithStreaming` / `withRetry` / fetch `RequestOptions.signal`.
    /// When aborted (Esc / user-cancel), in-flight HTTP and retry sleeps stop.
    pub abort_signal: Option<anthropic_sdk::AbortSignal>,
}

/// Maps to: CC services/api/claude.ts:701-706
/// API-side task budget configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskBudget {
    pub total: u64,
    pub remaining: Option<u64>,
}

// ---------------------------------------------------------------------------
// MessageParam for API
// ---------------------------------------------------------------------------

/// Maps to: CC `BetaMessageParam` (SDK type alias for API message shape).
/// Serializable message parameter sent to the Anthropic API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageParam {
    pub role: String,
    pub content: MessageContent,
}

/// Content of a MessageParam: either a plain string or array of content blocks.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Blocks(Vec<serde_json::Value>),
}

/// Maps to: CC `services/api/claude.ts` `CachedMCEditsBlock`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedMcEditsBlock {
    #[serde(rename = "type")]
    pub block_type: String,
    pub edits: Vec<CachedMcEdit>,
}

impl CachedMcEditsBlock {
    pub fn delete_refs(refs: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            block_type: "cache_edits".to_string(),
            edits: refs
                .into_iter()
                .map(|reference| CachedMcEdit {
                    edit_type: "delete".to_string(),
                    cache_reference: reference.into(),
                })
                .collect(),
        }
    }
}

/// Maps to: CC `CachedMCEditsBlock.edits[number]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedMcEdit {
    #[serde(rename = "type")]
    pub edit_type: String,
    pub cache_reference: String,
}

/// Maps to: CC `services/api/claude.ts` `CachedMCPinnedEdits`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CachedMcPinnedEdit {
    pub user_message_index: usize,
    pub block: CachedMcEditsBlock,
}

/// Maps to: CC `TextBlockParam` (SDK type).
/// A text block in the system prompt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextBlockParam {
    #[serde(rename = "type")]
    pub block_type: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

// ---------------------------------------------------------------------------
// API metadata
// ---------------------------------------------------------------------------

/// Maps to: CC services/api/claude.ts:503-528 request metadata object shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiMetadata {
    pub user_id: String,
}

// ---------------------------------------------------------------------------
// getExtraBodyParams
// ---------------------------------------------------------------------------

/// Maps to: CC services/api/claude.ts:272-331
///
/// Assemble the extra body parameters for the API request, based on the
/// CLAUDE_CODE_EXTRA_BODY environment variable if present and on any beta
/// headers (primarily for Bedrock requests).
pub fn get_extra_body_params(
    beta_headers: Option<&[String]>,
) -> serde_json::Map<String, JsonValue> {
    let mut result = serde_json::Map::new();

    // Parse user's extra body parameters first
    if let Some(extra_body_str) = crate::utils::process_env::var("CLAUDE_CODE_EXTRA_BODY").truthy()
    {
        match crate::utils::json::safe_parse_json(Some(&extra_body_str), true).as_ref() {
            JsonValue::Object(obj) => {
                // Clone before mutation to avoid poisoning the shared parse cache.
                result = obj.clone();
            }
            _ => {
                crate::utils::debug::log_for_debugging_with_level(
                    &format!(
                        "CLAUDE_CODE_EXTRA_BODY env var must be a JSON object, but was given {extra_body_str}"
                    ),
                    crate::utils::debug::DebugLogLevel::Error,
                );
            }
        }
    }

    // NOTE: Anti-distillation feature gated on ANTI_DISTILLATION_CC is skipped
    // (first-party internal feature). See CC claude.ts:301-313.

    // Handle beta headers if provided
    if let Some(headers) = beta_headers {
        if !headers.is_empty() {
            if let Some(JsonValue::Array(existing)) = result.get("anthropic_beta") {
                // Add to existing array, avoiding duplicates
                let existing_strs: Vec<String> = existing
                    .iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect();
                let mut merged = existing.clone();
                for h in headers {
                    if !existing_strs.contains(h) {
                        merged.push(JsonValue::String(h.clone()));
                    }
                }
                result.insert("anthropic_beta".to_string(), JsonValue::Array(merged));
            } else {
                // Create new array with the beta headers
                let arr: Vec<JsonValue> = headers
                    .iter()
                    .map(|h| JsonValue::String(h.clone()))
                    .collect();
                result.insert("anthropic_beta".to_string(), JsonValue::Array(arr));
            }
        }
    }

    result
}

// ---------------------------------------------------------------------------
// getPromptCachingEnabled
// ---------------------------------------------------------------------------

/// Maps to: CC services/api/claude.ts:333-356
///
/// Returns whether prompt caching is enabled for the given model.
/// Respects DISABLE_PROMPT_CACHING, DISABLE_PROMPT_CACHING_HAIKU,
/// DISABLE_PROMPT_CACHING_SONNET, and DISABLE_PROMPT_CACHING_OPUS env vars.
pub fn get_prompt_caching_enabled(model: &str) -> bool {
    // Global disable takes precedence
    if crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("DISABLE_PROMPT_CACHING").as_deref(),
    ) {
        return false;
    }

    // Check if we should disable for small/fast model
    if crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("DISABLE_PROMPT_CACHING_HAIKU")
            .as_deref(),
    ) {
        let small_fast_model = get_small_fast_model();
        if model == small_fast_model {
            return false;
        }
    }

    // Check if we should disable for default Sonnet
    if crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("DISABLE_PROMPT_CACHING_SONNET")
            .as_deref(),
    ) {
        let default_sonnet = get_default_sonnet_model();
        if model == default_sonnet {
            return false;
        }
    }

    // Check if we should disable for default Opus
    if crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("DISABLE_PROMPT_CACHING_OPUS").as_deref(),
    ) {
        let default_opus = get_default_opus_model();
        if model == default_opus {
            return false;
        }
    }

    true
}

// ---------------------------------------------------------------------------
// getCacheControl (already defined above)
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// configureEffortParams
// ---------------------------------------------------------------------------

/// Maps to: CC services/api/claude.ts:440-466
///
/// Configure effort parameters for API request.
/// Mutates the output config and betas in-place.
pub fn configure_effort_params(
    effort_value: Option<&EffortValue>,
    output_config: &mut OutputConfig,
    extra_body_params: &mut serde_json::Map<String, JsonValue>,
    betas: &mut Vec<String>,
    model: &str,
) {
    if !model_supports_effort(model) || output_config.contains_key("effort") {
        return;
    }

    match effort_value {
        None => {
            betas.push(EFFORT_BETA_HEADER.to_string());
        }
        Some(EffortValue::Named(level)) => {
            output_config.insert("effort".to_string(), JsonValue::String(level.clone()));
            betas.push(EFFORT_BETA_HEADER.to_string());
        }
        Some(EffortValue::Numeric(value)) => {
            // Numeric effort override - ant-only (uses anthropic_internal)
            if crate::utils::build_profile::has_internal_capability(
                crate::utils::build_profile::InternalCapability::Api,
            ) {
                let existing = extra_body_params
                    .get("anthropic_internal")
                    .cloned()
                    .unwrap_or(JsonValue::Object(serde_json::Map::new()));
                if let JsonValue::Object(mut internal) = existing {
                    internal.insert(
                        "effort_override".to_string(),
                        JsonValue::Number(serde_json::Number::from(*value)),
                    );
                    extra_body_params.insert(
                        "anthropic_internal".to_string(),
                        JsonValue::Object(internal),
                    );
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// configureTaskBudgetParams
// ---------------------------------------------------------------------------

/// Maps to: CC services/api/claude.ts:479-501
///
/// Configure task budget parameters for API request.
/// Mutates the output config and betas in-place.
pub fn configure_task_budget_params(
    task_budget: Option<&TaskBudget>,
    output_config: &mut serde_json::Map<String, JsonValue>,
    betas: &mut Vec<String>,
) {
    let budget = match task_budget {
        Some(b) => b,
        None => return,
    };

    if output_config.contains_key("task_budget") || !should_include_first_party_only_betas() {
        return;
    }

    let mut task_budget_obj = serde_json::Map::new();
    task_budget_obj.insert("type".to_string(), JsonValue::String("tokens".to_string()));
    task_budget_obj.insert(
        "total".to_string(),
        JsonValue::Number(serde_json::Number::from(budget.total)),
    );
    if let Some(remaining) = budget.remaining {
        task_budget_obj.insert(
            "remaining".to_string(),
            JsonValue::Number(serde_json::Number::from(remaining)),
        );
    }

    output_config.insert(
        "task_budget".to_string(),
        JsonValue::Object(task_budget_obj),
    );

    if !betas.contains(&TASK_BUDGETS_BETA_HEADER.to_string()) {
        betas.push(TASK_BUDGETS_BETA_HEADER.to_string());
    }
}

// ---------------------------------------------------------------------------
// getAPIMetadata
// ---------------------------------------------------------------------------

/// Maps to: CC services/api/claude.ts:503-528 `getAPIMetadata()`.
///
/// Assembles metadata JSON for API request. Returns an ApiMetadata struct
/// containing the user_id field (itself a JSON-encoded string of device_id,
/// account_uuid, and session_id). `device_id` is `getOrCreateUserID()`
/// (`utils/config.rs`) and `account_uuid` is `getOauthAccountInfo()?.accountUuid ?? ''`
/// (`utils/auth.rs`), exactly the two sources CC reads.
pub fn get_api_metadata() -> ApiMetadata {
    let mut extra = serde_json::Map::new();

    // Parse CLAUDE_CODE_EXTRA_METADATA env var
    if let Some(extra_str) = crate::utils::process_env::var("CLAUDE_CODE_EXTRA_METADATA").truthy() {
        match crate::utils::json::safe_parse_json(Some(&extra_str), false).as_ref() {
            JsonValue::Object(obj) => {
                extra = obj.clone();
            }
            _ => {
                crate::utils::debug::log_for_debugging_with_level(
                    &format!(
                        "CLAUDE_CODE_EXTRA_METADATA env var must be a JSON object, but was given {extra_str}"
                    ),
                    crate::utils::debug::DebugLogLevel::Error,
                );
            }
        }
    }

    // Maps to CC `claude.ts:519-526`: the persisted `getOrCreateUserID()`
    // (`~/.claude.json` `userID`) is the stable per-install device id — never a
    // per-request random value, which gateways keyed on `user_id` would treat
    // as a new user on every call.
    extra.insert(
        "device_id".to_string(),
        JsonValue::String(crate::utils::config::get_or_create_user_id()),
    );
    // Only include OAuth account UUID when actively using OAuth authentication
    extra.insert(
        "account_uuid".to_string(),
        JsonValue::String(
            crate::utils::auth::get_oauth_account_info()
                .and_then(|account| account.account_uuid)
                .unwrap_or_default(),
        ),
    );
    extra.insert(
        "session_id".to_string(),
        JsonValue::String(crate::bootstrap::state::get_session_id()),
    );

    let user_id =
        serde_json::to_string(&JsonValue::Object(extra)).unwrap_or_else(|_| "{}".to_string());

    ApiMetadata { user_id }
}

// ---------------------------------------------------------------------------
// verifyApiKey
// ---------------------------------------------------------------------------

/// Maps to: CC services/api/claude.ts:530-586
///
/// Verifies an API key by making a minimal messages.create call.
/// Returns true if the key is valid, false if authentication fails.
/// Errors on other failures.
///
pub async fn verify_api_key(
    api_key: &str,
    is_non_interactive_session: bool,
) -> anyhow::Result<bool> {
    // Skip API verification if running in print mode
    if is_non_interactive_session {
        return Ok(true);
    }

    // TODO: Implement real API key verification using anthropic_sdk::Anthropic.
    // The CC implementation creates a minimal messages.create call with
    // model=getSmallFastModel(), max_tokens=1, messages=[{role:'user', content:'test'}].
    // See CC services/api/claude.ts:530-586 for full retry and error handling.
    let _ = api_key;
    let _model = get_small_fast_model();
    let _betas = get_model_betas(&_model);

    Err(anyhow::anyhow!(
        "verifyApiKey stubbed: real implementation pending anthropic-sdk-rs wiring"
    ))
}

// ---------------------------------------------------------------------------
// userMessageToMessageParam
// ---------------------------------------------------------------------------

/// Maps to: CC services/api/claude.ts:588-631
///
/// Converts an internal UserMessage to an API MessageParam with optional
/// cache_control on the last content block.
fn tool_result_api_content(result: &crate::types::message::ToolResult) -> serde_json::Value {
    if result.content_blocks.is_empty() {
        serde_json::Value::String(result.content.clone())
    } else {
        serde_json::to_value(&result.content_blocks)
            .unwrap_or_else(|_| serde_json::Value::String(result.content.clone()))
    }
}

pub fn user_message_to_message_param(
    message: &UserMessage,
    add_cache: bool,
    enable_prompt_caching: bool,
    query_source: Option<&str>,
) -> MessageParam {
    use crate::types::message::UserContent;

    if add_cache {
        let blocks: Vec<serde_json::Value> = message
            .content
            .iter()
            .enumerate()
            .map(|(i, content)| {
                let mut block = match content {
                    UserContent::Text(text) | UserContent::MetaText(text) => {
                        serde_json::json!({
                            "type": "text",
                            "text": text,
                        })
                    }
                    UserContent::RawImage { block, .. } => block.clone(),
                    UserContent::Image { media_type, data }
                    | UserContent::MetaImage { media_type, data } => {
                        serde_json::json!({
                            "type": "image",
                            "source": {
                                "type": "base64",
                                "media_type": media_type,
                                "data": data,
                            }
                        })
                    }
                    UserContent::Document { media_type, data }
                    | UserContent::MetaDocument { media_type, data } => {
                        serde_json::json!({
                            "type": "document",
                            "source": {
                                "type": "base64",
                                "media_type": media_type,
                                "data": data,
                            }
                        })
                    }
                    UserContent::ToolResult(result) => {
                        let content = tool_result_api_content(result);
                        serde_json::json!({
                            "type": "tool_result",
                            "tool_use_id": result.tool_use_id.0,
                            "content": content,
                            "is_error": result.is_error,
                        })
                    }
                };

                // Add cache_control to the last block
                if i == message.content.len() - 1 && enable_prompt_caching {
                    if let Some(obj) = block.as_object_mut() {
                        let cc = get_cache_control(None, query_source);
                        obj.insert(
                            "cache_control".to_string(),
                            serde_json::to_value(&cc).unwrap_or_default(),
                        );
                    }
                }

                block
            })
            .collect();

        return MessageParam {
            role: "user".to_string(),
            content: MessageContent::Blocks(blocks),
        };
    }

    // Non-cached path: clone content blocks to prevent in-place mutation
    let blocks: Vec<serde_json::Value> = message
        .content
        .iter()
        .map(|content| match content {
            UserContent::Text(text) | UserContent::MetaText(text) => {
                serde_json::json!({
                    "type": "text",
                    "text": text,
                })
            }
            UserContent::RawImage { block, .. } => block.clone(),
            UserContent::Image { media_type, data }
            | UserContent::MetaImage { media_type, data } => {
                serde_json::json!({
                    "type": "image",
                    "source": {
                        "type": "base64",
                        "media_type": media_type,
                        "data": data,
                    }
                })
            }
            UserContent::Document { media_type, data }
            | UserContent::MetaDocument { media_type, data } => {
                serde_json::json!({
                    "type": "document",
                    "source": {
                        "type": "base64",
                        "media_type": media_type,
                        "data": data,
                    }
                })
            }
            UserContent::ToolResult(result) => {
                let content = tool_result_api_content(result);
                serde_json::json!({
                    "type": "tool_result",
                    "tool_use_id": result.tool_use_id.0,
                    "content": content,
                    "is_error": result.is_error,
                })
            }
        })
        .collect();

    MessageParam {
        role: "user".to_string(),
        content: MessageContent::Blocks(blocks),
    }
}

// ---------------------------------------------------------------------------
// assistantMessageToMessageParam
// ---------------------------------------------------------------------------

/// Maps to: CC services/api/claude.ts:633-674
///
/// Converts an internal AssistantMessage to an API MessageParam with optional
/// cache_control on the last eligible content block (excluding thinking and
/// redacted_thinking blocks).
pub fn assistant_message_to_message_param(
    message: &AssistantMessage,
    add_cache: bool,
    enable_prompt_caching: bool,
    query_source: Option<&str>,
) -> MessageParam {
    use crate::types::message::AssistantContent;

    // `MessageIdentity` is the Rust carrier for CC's assistant envelope, not
    // an Anthropic content block. Filter it before cache-breakpoint indexing
    // so adding identity cannot change request bytes or cache placement.
    let api_content = message
        .content
        .iter()
        .filter(|content| !matches!(content, AssistantContent::MessageIdentity(_)))
        .collect::<Vec<_>>();
    let blocks: Vec<serde_json::Value> = api_content
        .iter()
        .enumerate()
        .map(|(i, content)| {
            let mut block = match *content {
                AssistantContent::Text(text) => {
                    serde_json::json!({
                        "type": "text",
                        "text": text,
                    })
                }
                AssistantContent::Thinking { text, signature } => {
                    // Maps to CC `assistantMessageToMessageParam`: spread the
                    // full thinking block so `signature` is replayed on the
                    // next request (even when empty after stream start init).
                    serde_json::json!({
                        "type": "thinking",
                        "thinking": text,
                        "signature": signature,
                    })
                }
                AssistantContent::RedactedThinking { data } => {
                    serde_json::json!({
                        "type": "redacted_thinking",
                        "data": data,
                    })
                }
                AssistantContent::ToolUse(tool_use) => {
                    serde_json::json!({
                        "type": "tool_use",
                        "id": tool_use.id.0,
                        "name": tool_use.name,
                        "input": tool_use.input,
                    })
                }
                AssistantContent::ServerToolUse(tool_use) => {
                    serde_json::json!({
                        "type": "server_tool_use",
                        "id": tool_use.id.0,
                        "name": tool_use.name,
                        "input": tool_use.input,
                    })
                }
                AssistantContent::WebSearchToolResult {
                    tool_use_id,
                    content,
                } => {
                    serde_json::json!({
                        "type": "web_search_tool_result",
                        "tool_use_id": tool_use_id.0,
                        "content": content,
                    })
                }
                AssistantContent::Advisor {
                    content: adv_content,
                    ..
                } => {
                    // KNOWN DEVIATION (#25). CC has exactly two paths
                    // (claude.ts:1303-1306): without the advisor beta header
                    // `stripAdvisorBlocks` removes these outright, and with it
                    // they reach the API unchanged. Degrading to text is a
                    // third path CC does not have — it drops the
                    // `advisor_result` / `advisor_redacted_result` /
                    // `advisor_tool_result_error` discriminant, and redacted
                    // and error results carry no text at all, so they ship as
                    // EMPTY text blocks.
                    //
                    // What blocks the faithful version is not the wire format
                    // but a layer CC does not have: `build_sdk_message_create_params`
                    // deserializes into `anthropic_sdk::ContentBlockParam`,
                    // whose variant list has no `advisor_tool_result` and no
                    // passthrough arm, so the whole request fails to build. CC
                    // says its own SDK lacks the type too (utils/advisor.ts:7-8)
                    // and casts past it, which TypeScript makes free.
                    //
                    // ACCEPTED, not pending (ruling 2026-08-09). Advisor is an
                    // unreleased internal tool: it exists in the CC sources we
                    // port from but is not a working feature of the shipped
                    // build, so nothing exercises this path. Closing the gap
                    // would mean editing the `../anthropic-sdk-rs` fork — a
                    // reviewed batch of its own — to serve a code path that
                    // cannot currently run. Not worth it.
                    //
                    // Revisit ONLY if advisor becomes live upstream. The route
                    // is known: give `ContentBlockParam` a passthrough variant
                    // (preferred over an advisor-specific one — CC's own SDK
                    // keeps lagging beta blocks, so the next one would need the
                    // same edit again), then send these blocks unchanged.
                    //
                    // The three sibling gaps are NOT affected and stay fixed:
                    // pairing (`server_tool_result_id` exhaustive match),
                    // assistant-side lookup, and whole-block token estimation
                    // are general rules CC applies to every server tool result;
                    // advisor merely exposed them.
                    serde_json::json!({
                        "type": "text",
                        "text": adv_content.text().unwrap_or_default(),
                    })
                }
                AssistantContent::MessageIdentity(_) => {
                    unreachable!("assistant envelope identity was filtered before API projection")
                }
            };

            // Add cache_control to the last eligible block
            // (not thinking or redacted_thinking)
            if add_cache
                && i == api_content.len() - 1
                && enable_prompt_caching
                && !matches!(
                    content,
                    AssistantContent::Thinking { .. } | AssistantContent::RedactedThinking { .. }
                )
            {
                if let Some(obj) = block.as_object_mut() {
                    let cc = get_cache_control(None, query_source);
                    obj.insert(
                        "cache_control".to_string(),
                        serde_json::to_value(&cc).unwrap_or_default(),
                    );
                }
            }

            block
        })
        .collect();

    MessageParam {
        role: "assistant".to_string(),
        content: MessageContent::Blocks(blocks),
    }
}

// ---------------------------------------------------------------------------
// stripExcessMediaItems
// ---------------------------------------------------------------------------

/// Maps to: CC `services/api/claude.ts` `stripExcessMediaItems(...)` for
/// Rust's current typed-message subset.
///
/// Ensures messages contain at most `limit` media items and strips the oldest
/// first to preserve recent context. Cometix typed messages currently expose
/// user images directly; document/media blocks nested inside raw SDK params are
/// handled by `strip_excess_media_items_typed(...)` below.
pub fn strip_excess_media_items(messages: &[(Message,)], limit: usize) -> Vec<Message> {
    let media_count = messages
        .iter()
        .map(|(message,)| count_typed_message_media(message))
        .sum::<usize>();
    if media_count <= limit {
        return messages.iter().map(|(message,)| message.clone()).collect();
    }

    let mut to_remove = media_count - limit;
    messages
        .iter()
        .map(|(message,)| strip_typed_message_media(message.clone(), &mut to_remove))
        .collect()
}

fn count_typed_message_media(message: &Message) -> usize {
    match message {
        Message::User(user) => user
            .content
            .iter()
            .filter(|content| {
                matches!(
                    content,
                    crate::types::message::UserContent::Image { .. }
                        | crate::types::message::UserContent::MetaImage { .. }
                        | crate::types::message::UserContent::RawImage { .. }
                )
            })
            .count(),
        Message::Assistant(_)
        | Message::System(_)
        | Message::Attachment(_)
        | Message::Progress(_)
        | Message::HookResult(_) => 0,
    }
}

fn strip_typed_message_media(mut message: Message, to_remove: &mut usize) -> Message {
    if *to_remove == 0 {
        return message;
    }
    if let Message::User(user) = &mut message {
        user.content.retain(|content| {
            if *to_remove == 0 {
                return true;
            }
            if matches!(
                content,
                crate::types::message::UserContent::Image { .. }
                    | crate::types::message::UserContent::MetaImage { .. }
                    | crate::types::message::UserContent::RawImage { .. }
            ) {
                *to_remove -= 1;
                return false;
            }
            true
        });
    }
    message
}

/// Maps to: CC services/api/claude.ts:956-1015 (typed for UserMessage/AssistantMessage pairs)
///
/// Ensures messages contain at most `limit` media items (images + documents).
/// Strips oldest media first to preserve the most recent.
/// This variant works on the raw user/assistant message slices.
pub fn strip_excess_media_items_typed(
    messages: Vec<MessageParam>,
    limit: usize,
) -> Vec<MessageParam> {
    // Count total media items
    let mut media_count: usize = 0;
    for msg in &messages {
        if let MessageContent::Blocks(blocks) = &msg.content {
            for block in blocks {
                if let Some(block_type) = block.get("type").and_then(|t| t.as_str()) {
                    if block_type == "image" || block_type == "document" {
                        media_count += 1;
                    }
                    if block_type == "tool_result" {
                        if let Some(JsonValue::Array(nested)) = block.get("content") {
                            for n in nested {
                                if let Some(nt) = n.get("type").and_then(|t| t.as_str()) {
                                    if nt == "image" || nt == "document" {
                                        media_count += 1;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    if media_count <= limit {
        return messages;
    }

    let mut to_remove = media_count - limit;
    messages
        .into_iter()
        .map(|msg| {
            if to_remove == 0 {
                return msg;
            }
            match msg.content {
                MessageContent::Blocks(blocks) => {
                    let stripped: Vec<serde_json::Value> = blocks
                        .into_iter()
                        .filter_map(|mut block| {
                            if to_remove == 0 {
                                return Some(block);
                            }
                            let block_type = block
                                .get("type")
                                .and_then(|t| t.as_str())
                                .unwrap_or("")
                                .to_string();

                            // Handle nested media in tool_result
                            if block_type == "tool_result" {
                                if let Some(JsonValue::Array(nested)) =
                                    block.get("content").cloned()
                                {
                                    let filtered: Vec<JsonValue> = nested
                                        .into_iter()
                                        .filter(|n| {
                                            if to_remove == 0 {
                                                return true;
                                            }
                                            let nt = n
                                                .get("type")
                                                .and_then(|t| t.as_str())
                                                .unwrap_or("");
                                            if nt == "image" || nt == "document" {
                                                to_remove -= 1;
                                                false
                                            } else {
                                                true
                                            }
                                        })
                                        .collect();
                                    if let Some(obj) = block.as_object_mut() {
                                        obj.insert(
                                            "content".to_string(),
                                            JsonValue::Array(filtered),
                                        );
                                    }
                                }
                                return Some(block);
                            }

                            // Handle top-level media
                            if (block_type == "image" || block_type == "document") && to_remove > 0
                            {
                                to_remove -= 1;
                                return None;
                            }
                            Some(block)
                        })
                        .collect();

                    MessageParam {
                        role: msg.role,
                        content: MessageContent::Blocks(stripped),
                    }
                }
                other => MessageParam {
                    role: msg.role,
                    content: other,
                },
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// cleanupStream
// ---------------------------------------------------------------------------

/// Maps to: CC services/api/claude.ts:2898-2912
///
/// Cleans up stream resources to prevent memory leaks.
/// In Rust, this is a no-op if the stream has already been dropped. The
/// equivalent of aborting the stream controller is dropping the stream
/// or cancelling the associated CancellationToken.
pub fn cleanup_stream<T>(_stream: Option<T>) {
    // In Rust, dropping the stream is sufficient to release resources.
    // The SDK stream type implements Drop which handles cleanup.
    // This function exists to maintain 1:1 call boundary alignment with CC.
}

// ---------------------------------------------------------------------------
// updateUsage
// ---------------------------------------------------------------------------

/// Maps to: CC services/api/claude.ts:2924-2987
///
/// Updates usage statistics with new values from streaming API events.
/// Note: Anthropic's streaming API provides cumulative usage totals, not
/// incremental deltas. Each event contains the complete usage up to that
/// point in the stream.
///
/// Input-related tokens (input_tokens, cache_creation_input_tokens,
/// cache_read_input_tokens) are typically set in message_start and remain
/// constant. message_delta events may send explicit 0 values for these
/// fields, which should not overwrite the values from message_start.
/// We only update these fields if they have a non-null, non-zero value.
pub fn update_usage(
    usage: &NonNullableUsage,
    part_usage: Option<&serde_json::Map<String, JsonValue>>,
) -> NonNullableUsage {
    let part = match part_usage {
        Some(p) => p,
        None => return usage.clone(),
    };

    let get_u64 = |key: &str| -> Option<u64> { part.get(key).and_then(|v| v.as_u64()) };

    let get_nested_u64 = |parent: &str, key: &str| -> Option<u64> {
        part.get(parent)
            .and_then(|v| v.as_object())
            .and_then(|obj| obj.get(key))
            .and_then(|v| v.as_u64())
    };

    NonNullableUsage {
        input_tokens: match get_u64("input_tokens") {
            Some(v) if v > 0 => v,
            _ => usage.input_tokens,
        },
        cache_creation_input_tokens: match get_u64("cache_creation_input_tokens") {
            Some(v) if v > 0 => v,
            _ => usage.cache_creation_input_tokens,
        },
        cache_read_input_tokens: match get_u64("cache_read_input_tokens") {
            Some(v) if v > 0 => v,
            _ => usage.cache_read_input_tokens,
        },
        output_tokens: get_u64("output_tokens").unwrap_or(usage.output_tokens),
        server_tool_use: ServerToolUse {
            web_search_requests: get_nested_u64("server_tool_use", "web_search_requests")
                .unwrap_or(usage.server_tool_use.web_search_requests),
            web_fetch_requests: get_nested_u64("server_tool_use", "web_fetch_requests")
                .unwrap_or(usage.server_tool_use.web_fetch_requests),
        },
        service_tier: usage.service_tier.clone(),
        cache_creation: CacheCreation {
            ephemeral_1h_input_tokens: get_nested_u64(
                "cache_creation",
                "ephemeral_1h_input_tokens",
            )
            .unwrap_or(usage.cache_creation.ephemeral_1h_input_tokens),
            ephemeral_5m_input_tokens: get_nested_u64(
                "cache_creation",
                "ephemeral_5m_input_tokens",
            )
            .unwrap_or(usage.cache_creation.ephemeral_5m_input_tokens),
        },
        inference_geo: usage.inference_geo.clone(),
        iterations: get_u64("iterations").or(usage.iterations),
        speed: part
            .get("speed")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| usage.speed.clone()),
    }
}

// ---------------------------------------------------------------------------
// accumulateUsage
// ---------------------------------------------------------------------------

/// Maps to: CC services/api/claude.ts:2993-3038
///
/// Accumulates usage from one message into a total usage object.
/// Used to track cumulative usage across multiple assistant turns.
pub fn accumulate_usage(
    total_usage: &NonNullableUsage,
    message_usage: &NonNullableUsage,
) -> NonNullableUsage {
    NonNullableUsage {
        input_tokens: total_usage.input_tokens + message_usage.input_tokens,
        cache_creation_input_tokens: total_usage.cache_creation_input_tokens
            + message_usage.cache_creation_input_tokens,
        cache_read_input_tokens: total_usage.cache_read_input_tokens
            + message_usage.cache_read_input_tokens,
        output_tokens: total_usage.output_tokens + message_usage.output_tokens,
        server_tool_use: ServerToolUse {
            web_search_requests: total_usage.server_tool_use.web_search_requests
                + message_usage.server_tool_use.web_search_requests,
            web_fetch_requests: total_usage.server_tool_use.web_fetch_requests
                + message_usage.server_tool_use.web_fetch_requests,
        },
        // Use the most recent service tier
        service_tier: message_usage.service_tier.clone(),
        cache_creation: CacheCreation {
            ephemeral_1h_input_tokens: total_usage.cache_creation.ephemeral_1h_input_tokens
                + message_usage.cache_creation.ephemeral_1h_input_tokens,
            ephemeral_5m_input_tokens: total_usage.cache_creation.ephemeral_5m_input_tokens
                + message_usage.cache_creation.ephemeral_5m_input_tokens,
        },
        // Use the most recent values
        inference_geo: message_usage.inference_geo.clone(),
        iterations: message_usage.iterations,
        speed: message_usage.speed.clone(),
    }
}

// ---------------------------------------------------------------------------
// addCacheBreakpoints
// ---------------------------------------------------------------------------

/// Maps to: CC services/api/claude.ts:3063-3211
///
/// Adds cache control markers to messages for the API. Places exactly one
/// message-level cache_control marker per request. The marker position depends
/// on `skip_cache_write`: if true, it goes on the second-to-last message
/// (shared prefix point); otherwise on the last message.
///
pub fn add_cache_breakpoints(
    messages: &[MessageRef<'_>],
    enable_prompt_caching: bool,
    query_source: Option<&str>,
    use_cached_mc: bool,
    new_cache_edits: Option<&CachedMcEditsBlock>,
    pinned_edits: &[CachedMcPinnedEdit],
    skip_cache_write: bool,
) -> Vec<MessageParam> {
    let marker_index = if skip_cache_write && messages.len() >= 2 {
        messages.len() - 2
    } else if !messages.is_empty() {
        messages.len() - 1
    } else {
        0
    };

    let mut result: Vec<MessageParam> = messages
        .iter()
        .enumerate()
        .map(|(index, msg_ref)| {
            let add_cache = index == marker_index;
            match msg_ref {
                MessageRef::User(msg) => user_message_to_message_param(
                    msg,
                    add_cache,
                    enable_prompt_caching,
                    query_source,
                ),
                MessageRef::Assistant(msg) => assistant_message_to_message_param(
                    msg,
                    add_cache,
                    enable_prompt_caching,
                    query_source,
                ),
            }
        })
        .collect();

    if !use_cached_mc {
        return result;
    }

    let mut seen_delete_refs = std::collections::HashSet::new();
    for pinned in pinned_edits {
        let Some(message) = result.get_mut(pinned.user_message_index) else {
            continue;
        };
        if message.role != "user" {
            continue;
        }
        let deduped = deduplicate_cached_mc_edits(&pinned.block, &mut seen_delete_refs);
        if deduped.edits.is_empty() {
            continue;
        }
        let content = ensure_message_param_content_blocks(message);
        insert_block_after_tool_results(
            content,
            serde_json::to_value(&deduped).unwrap_or_default(),
        );
    }

    if let Some(new_cache_edits) = new_cache_edits {
        let deduped = deduplicate_cached_mc_edits(new_cache_edits, &mut seen_delete_refs);
        if !deduped.edits.is_empty() {
            if let Some(message) = result
                .iter_mut()
                .rev()
                .find(|message| message.role == "user")
            {
                let content = ensure_message_param_content_blocks(message);
                insert_block_after_tool_results(
                    content,
                    serde_json::to_value(&deduped).unwrap_or_default(),
                );
                crate::utils::debug::log_for_debugging(&format!(
                    "Added cache_edits block with {} edits",
                    deduped.edits.len()
                ));
                // TODO: Wire official `pinCacheEdits(...)` state once cached
                // microcompact production state is fully ported. The typed
                // `pinned_edits` parameter above preserves the re-insertion
                // behavior without mutating hidden API adapter state.
            }
        }
    }

    if enable_prompt_caching {
        add_cache_references_before_last_cache_control(&mut result);
    }

    result
}

fn deduplicate_cached_mc_edits(
    block: &CachedMcEditsBlock,
    seen_delete_refs: &mut std::collections::HashSet<String>,
) -> CachedMcEditsBlock {
    CachedMcEditsBlock {
        block_type: block.block_type.clone(),
        edits: block
            .edits
            .iter()
            .filter(|edit| seen_delete_refs.insert(edit.cache_reference.clone()))
            .cloned()
            .collect(),
    }
}

fn ensure_message_param_content_blocks(message: &mut MessageParam) -> &mut Vec<serde_json::Value> {
    if let MessageContent::Text(text) = &message.content {
        message.content = MessageContent::Blocks(vec![serde_json::json!({
            "type": "text",
            "text": text,
        })]);
    }
    match &mut message.content {
        MessageContent::Blocks(blocks) => blocks,
        MessageContent::Text(_) => unreachable!("text content converted to blocks above"),
    }
}

fn add_cache_references_before_last_cache_control(messages: &mut [MessageParam]) {
    let last_cache_control_message =
        messages
            .iter()
            .enumerate()
            .fold(None, |last, (index, msg)| {
                let has_cache_control = match &msg.content {
                    MessageContent::Blocks(blocks) => blocks.iter().any(|block| {
                        block
                            .as_object()
                            .is_some_and(|obj| obj.contains_key("cache_control"))
                    }),
                    MessageContent::Text(_) => false,
                };
                if has_cache_control { Some(index) } else { last }
            });

    let Some(last_cache_control_message) = last_cache_control_message else {
        return;
    };

    for message in messages.iter_mut().take(last_cache_control_message) {
        if message.role != "user" {
            continue;
        }
        let MessageContent::Blocks(blocks) = &mut message.content else {
            continue;
        };
        for block in blocks.iter_mut() {
            let tool_use_id = block
                .as_object()
                .filter(|obj| {
                    obj.get("type").and_then(|value| value.as_str()) == Some("tool_result")
                })
                .and_then(|obj| obj.get("tool_use_id"))
                .and_then(|value| value.as_str())
                .map(str::to_string);
            if let Some(tool_use_id) = tool_use_id {
                if let Some(obj) = block.as_object_mut() {
                    obj.insert(
                        "cache_reference".to_string(),
                        serde_json::Value::String(tool_use_id),
                    );
                }
            }
        }
    }
}

/// Reference to either a UserMessage or AssistantMessage for cache breakpoint
/// processing. Avoids cloning messages during the conversion pipeline.
pub enum MessageRef<'a> {
    User(&'a UserMessage),
    Assistant(&'a AssistantMessage),
}

// ---------------------------------------------------------------------------
// buildSystemPromptBlocks
// ---------------------------------------------------------------------------

/// Maps to: CC `services/api/claude.ts:3213-3237` `buildSystemPromptBlocks(...)`.
///
/// `splitSysPromptPrefix` (`utils/api.rs`) does the content-based split —
/// attribution header, CLI sysprompt prefix, static/dynamic boundary — and
/// every block whose `cacheScope` is not null gets `getCacheControl(...)`
/// when prompt caching is enabled.
///
/// IMPORTANT (CC): Do not add any more blocks for caching or you will get a 400.
pub fn build_system_prompt_blocks(
    system_prompt: &SystemPrompt,
    enable_prompt_caching: bool,
    skip_global_cache_for_system_prompt: bool,
    query_source: Option<&str>,
) -> Vec<TextBlockParam> {
    crate::utils::api::split_sys_prompt_prefix(system_prompt, skip_global_cache_for_system_prompt)
        .into_iter()
        .map(|block| {
            let cache_control = if enable_prompt_caching {
                block
                    .cache_scope
                    .map(|scope| get_cache_control(Some(scope), query_source))
            } else {
                None
            };
            TextBlockParam {
                block_type: "text".to_string(),
                text: block.text,
                cache_control,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// adjustParamsForNonStreaming
// ---------------------------------------------------------------------------

/// Maps to: CC services/api/claude.ts:3364-3392 non-streaming params shape.
///
/// Adjusts thinking budget when max_tokens is capped for non-streaming fallback.
/// Ensures the API constraint: max_tokens > thinking.budget_tokens.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NonStreamingParams {
    pub max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ThinkingConfig>,
    /// All other params pass through.
    #[serde(flatten)]
    pub extra: HashMap<String, JsonValue>,
}

/// Maps to: CC services/api/claude.ts:3364-3392 `adjustParamsForNonStreaming(...)`.
///
/// Caps max_tokens and adjusts thinking budget for non-streaming fallback.
pub fn adjust_params_for_non_streaming(
    params: &NonStreamingParams,
    max_tokens_cap: u32,
) -> NonStreamingParams {
    let capped_max_tokens = params.max_tokens.min(max_tokens_cap);

    let adjusted_thinking = params.thinking.as_ref().map(|thinking| match thinking {
        ThinkingConfig::Enabled {
            budget_tokens: Some(budget),
        } => ThinkingConfig::Enabled {
            budget_tokens: Some((*budget).min((capped_max_tokens as i64) - 1)),
        },
        other => other.clone(),
    });

    NonStreamingParams {
        max_tokens: capped_max_tokens,
        thinking: adjusted_thinking,
        extra: params.extra.clone(),
    }
}

// ---------------------------------------------------------------------------
// getMaxOutputTokensForModel
// ---------------------------------------------------------------------------

/// Maps to: CC services/api/claude.ts:3399-3419
///
/// Returns effective max output tokens for a model considering env overrides
/// and slot-reservation cap. The slot-reservation cap (CAPPED_DEFAULT_MAX_TOKENS)
/// is controlled by a GrowthBook feature flag in CC; here it is controlled by
/// the COMETIX_MAX_TOKENS_CAP env var.
pub fn get_max_output_tokens_for_model(model: &str) -> u32 {
    let max_output = get_model_max_output_tokens(model);

    // Slot-reservation cap: drop default to 8k for all models
    let is_cap_enabled = crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("COMETIX_MAX_TOKENS_CAP").as_deref(),
    );
    let default_tokens = if is_cap_enabled {
        max_output.default.min(CAPPED_DEFAULT_MAX_TOKENS)
    } else {
        max_output.default
    };

    // Env var override: CLAUDE_CODE_MAX_OUTPUT_TOKENS
    crate::utils::env_validation::validate_bounded_int_env_var(
        "CLAUDE_CODE_MAX_OUTPUT_TOKENS",
        crate::utils::process_env::var("CLAUDE_CODE_MAX_OUTPUT_TOKENS")
            .as_deref(),
        default_tokens as usize,
        max_output.upper_limit as usize,
    )
    .effective as u32
}

// ---------------------------------------------------------------------------
// getNonstreamingFallbackTimeoutMs
// ---------------------------------------------------------------------------

/// Maps to: CC services/api/claude.ts:807-811
///
/// Per-attempt timeout for non-streaming fallback requests, in milliseconds.
/// Reads API_TIMEOUT_MS when set. Remote sessions default to 120s.
/// Otherwise defaults to 300s.
pub fn get_nonstreaming_fallback_timeout_ms() -> u64 {
    if let Some(override_str) = crate::utils::process_env::var("API_TIMEOUT_MS") {
        if let Ok(ms) = override_str.parse::<u64>() {
            if ms > 0 {
                return ms;
            }
        }
    }
    if crate::utils::env_utils::is_env_truthy(crate::utils::process_env::var("CLAUDE_CODE_REMOTE").as_deref()) {
        120_000
    } else {
        300_000
    }
}

// ---------------------------------------------------------------------------
// getPreviousRequestIdFromMessages
// ---------------------------------------------------------------------------

/// Maps to: CC services/api/claude.ts:928-938
///
/// Extracts the request ID from the most recent assistant message in the
/// conversation. Used to link consecutive API requests in analytics.
pub fn get_previous_request_id_from_messages(messages: &[Message]) -> Option<String> {
    messages.iter().rev().find_map(|message| match message {
        Message::Assistant(assistant) => assistant.request_id().map(ToString::to_string),
        _ => None,
    })
}

// ---------------------------------------------------------------------------
// anthropic-sdk-rs Phase 1 adapter helpers
// ---------------------------------------------------------------------------

impl Options {
    /// Construct CC `Options` with the common REPL/query defaults.
    ///
    /// Maps to: CC `services/api/claude.ts` `Options` object literals passed into
    /// `queryModel` / `queryModelWithStreaming` (there is no TS factory — call
    /// sites inline `{ model, querySource, ... }`). `abort_signal` is a Rust
    /// seam for the separate TS `signal` parameter on those functions.
    pub fn new(model: String, query_source: ApiQuerySource) -> Self {
        Self {
            model,
            is_non_interactive_session: false,
            query_source,
            agents: Vec::new(),
            has_append_system_prompt: false,
            tool_choice: None,
            extra_tool_schemas: None,
            max_output_tokens_override: None,
            fallback_model: None,
            enable_prompt_caching: None,
            skip_cache_write: Some(false),
            temperature_override: None,
            effort_value: None,
            mcp_tools: Vec::new(),
            has_pending_mcp_servers: None,
            query_tracking: None,
            agent_id: None,
            output_format: None,
            fast_mode: None,
            advisor_model: None,
            allowed_agent_types: None,
            tool_permission_context: None,
            task_budget: None,
            cached_mc_enabled: false,
            cached_mc_new_cache_edits: None,
            cached_mc_pinned_edits: Vec::new(),
            abort_signal: None,
        }
    }
}

fn local_message_param_to_sdk(
    param: &MessageParam,
) -> anyhow::Result<anthropic_sdk::resources::messages::MessageParam> {
    local_message_param_to_sdk_at(param, None)
}

fn local_message_param_to_sdk_at(
    param: &MessageParam,
    index: Option<usize>,
) -> anyhow::Result<anthropic_sdk::resources::messages::MessageParam> {
    let value = serde_json::to_value(param)?;
    serde_json::from_value(value).map_err(|error| {
        anyhow::anyhow!(
            "{}",
            format_message_param_convert_error(param, index, &error)
        )
    })
}

fn format_message_param_convert_error(
    param: &MessageParam,
    index: Option<usize>,
    error: &serde_json::Error,
) -> String {
    let index_label = index
        .map(|i| format!("message[{i}]"))
        .unwrap_or_else(|| "message".to_string());
    let block_types = super::api_trace::content_block_types(param);
    let mut failed_block_detail = None;
    let mut failed_block_snippet = None;
    if let MessageContent::Blocks(blocks) = &param.content {
        for (block_index, block) in blocks.iter().enumerate() {
            if let Some(detail) = super::api_trace::diagnose_block_convert_error(block) {
                failed_block_detail = Some(format!("block[{block_index}] {detail}"));
                failed_block_snippet = Some(super::api_trace::truncate_json_snippet(block, 2048));
                break;
            }
        }
    }
    let detail = failed_block_detail.unwrap_or_else(|| error.to_string());
    let mut message = format!(
        "failed to convert Cometix message to SDK MessageParam: {index_label} role={} block_types={:?}: {detail}",
        param.role, block_types
    );
    if let Some(snippet) = failed_block_snippet {
        message.push_str(&format!(" snippet={snippet}"));
    }
    message
}

fn convert_local_messages_to_sdk(
    local_messages: &[MessageParam],
    trace: &super::api_trace::ApiTrace,
) -> anyhow::Result<Vec<anthropic_sdk::resources::messages::MessageParam>> {
    let mut sdk_messages = Vec::with_capacity(local_messages.len());
    for (index, param) in local_messages.iter().enumerate() {
        match local_message_param_to_sdk_at(param, Some(index)) {
            Ok(sdk) => {
                super::api_trace::emit(
                    trace,
                    "convert_ok",
                    serde_json::json!({
                        "message_index": index,
                        "role": param.role,
                        "block_types": super::api_trace::content_block_types(param),
                    }),
                );
                sdk_messages.push(sdk);
            }
            Err(error) => {
                let mut failed_block = None;
                let mut failed_block_index = None;
                if let MessageContent::Blocks(blocks) = &param.content {
                    for (block_index, block) in blocks.iter().enumerate() {
                        if super::api_trace::diagnose_block_convert_error(block).is_some() {
                            failed_block_index = Some(block_index);
                            failed_block = Some(block.clone());
                            break;
                        }
                    }
                }
                super::api_trace::emit_always_when_enabled(
                    trace,
                    "convert_error",
                    serde_json::json!({
                        "message_index": index,
                        "role": param.role,
                        "block_types": super::api_trace::content_block_types(param),
                        "error": error.to_string(),
                        "failed_block_index": failed_block_index,
                        "failed_block": failed_block.as_ref().map(|block| {
                            serde_json::Value::String(
                                super::api_trace::truncate_json_snippet(block, 2048),
                            )
                        }),
                    }),
                );
                crate::utils::debug::log_for_debugging_with_level(
                    &format!("api: [API:trace] {error}"),
                    crate::utils::debug::DebugLogLevel::Error,
                );
                if let Ok(values) = local_messages
                    .iter()
                    .map(serde_json::to_value)
                    .collect::<Result<Vec<_>, _>>()
                {
                    super::dump_prompts::dump_local_messages_on_error(&values, &error.to_string());
                }
                return Err(error);
            }
        }
    }
    Ok(sdk_messages)
}

fn sdk_system_prompt_from_blocks(
    blocks: &[TextBlockParam],
) -> anyhow::Result<Option<anthropic_sdk::resources::messages::SystemPrompt>> {
    if blocks.is_empty() {
        return Ok(None);
    }
    let value = serde_json::to_value(blocks)?;
    let sdk_blocks = serde_json::from_value(value).map_err(|error| {
        anyhow::anyhow!("failed to convert Cometix system prompt blocks to SDK params: {error}")
    })?;
    Ok(Some(
        anthropic_sdk::resources::messages::SystemPrompt::Blocks(sdk_blocks),
    ))
}

fn local_thinking_config_to_sdk(
    thinking: &ThinkingConfig,
    model: &str,
    max_output_tokens: u32,
) -> anthropic_sdk::resources::messages::ThinkingConfig {
    if matches!(thinking, ThinkingConfig::Disabled) || !model_supports_thinking(model) {
        return anthropic_sdk::resources::messages::ThinkingConfig::Disabled;
    }

    match thinking {
        ThinkingConfig::Disabled => anthropic_sdk::resources::messages::ThinkingConfig::Disabled,
        ThinkingConfig::Adaptive if model_supports_adaptive_thinking(model) => {
            anthropic_sdk::resources::messages::ThinkingConfig::Adaptive
        }
        ThinkingConfig::Adaptive => anthropic_sdk::resources::messages::ThinkingConfig::Enabled {
            budget_tokens: max_thinking_tokens_for_model(model, max_output_tokens) as i64,
        },
        ThinkingConfig::Enabled { budget_tokens } => {
            let requested = budget_tokens
                .unwrap_or_else(|| max_thinking_tokens_for_model(model, max_output_tokens) as i64);
            anthropic_sdk::resources::messages::ThinkingConfig::Enabled {
                budget_tokens: requested
                    .min((max_output_tokens as i64).saturating_sub(1))
                    .max(1),
            }
        }
    }
}

fn max_thinking_tokens_for_model(model: &str, max_output_tokens: u32) -> u32 {
    get_max_output_tokens_for_model(model)
        .min(max_output_tokens)
        .saturating_sub(1)
        .max(1)
}

fn should_use_cached_mc_for_request(options: &Options) -> bool {
    // Maps to CC `services/api/claude.ts` `useCachedMC`: cached microcompact
    // cache_edits body blocks are first-party REPL-main-thread only. The beta
    // header can be latched separately, but provider/source-incompatible calls
    // must not receive cache_edits/cache_reference blocks.
    let requested = options.cached_mc_enabled
        || options.cached_mc_new_cache_edits.is_some()
        || !options.cached_mc_pinned_edits.is_empty();
    requested
        && crate::utils::model::providers::get_api_provider()
            == crate::utils::model::providers::ApiProvider::FirstParty
        && options.query_source == "repl_main_thread"
}

#[derive(Debug, Clone, Default)]
struct SdkPrivateBodyOverrides {
    messages: Option<JsonValue>,
    system: Option<JsonValue>,
}

#[derive(Debug)]
struct SdkMessageCreatePlan {
    // Official `paramsFromContext` returns the object passed to
    // `anthropic.beta.messages.create`.
    params: anthropic_sdk::resources::beta::messages::BetaMessageCreateParams,
    private_body_overrides: SdkPrivateBodyOverrides,
}

/// Maps to CC `services/api/claude.ts` `paramsFromContext(...)`: CC adds
/// first-party cache-editing fields to structurally typed SDK objects at
/// runtime. They are not public `anthropic-sdk-typescript` message models, so
/// Cometix keeps those extras in its request adapter and overlays them through
/// `RequestOptions`. The create object itself is `BetaMessageCreateParams`
/// because official passes it to `anthropic.beta.messages.create`.
fn build_sdk_message_create_plan(
    messages: &[Message],
    system_prompt: &SystemPrompt,
    thinking_config: &ThinkingConfig,
    tools: &[Tool],
    options: &Options,
) -> anyhow::Result<SdkMessageCreatePlan> {
    let trace =
        super::api_trace::current_trace().unwrap_or_else(|| super::api_trace::ApiTrace::new(1));
    super::api_trace::set_current_trace(Some(trace.clone()));
    let enable_prompt_caching = options
        .enable_prompt_caching
        .unwrap_or_else(|| get_prompt_caching_enabled(&options.model));
    let normalized_api_messages = crate::utils::messages::ensure_tool_result_pairing(
        crate::utils::messages::normalize_messages_for_api_with_model(
            messages.to_vec(),
            tools,
            Some(&options.model),
        ),
    );
    super::api_trace::emit(
        &trace,
        "normalize",
        serde_json::json!({
            "input_count": messages.len(),
            "normalized_count": normalized_api_messages.len(),
            "query_source": options.query_source,
            "model": options.model,
        }),
    );
    let api_messages = if should_strip_advisor_blocks_for_request(options) {
        crate::utils::messages::strip_advisor_blocks(normalized_api_messages)
    } else {
        normalized_api_messages
    };
    // Maps to CC `claude.ts:1322-1325`: compute fingerprint from first user
    // message for attribution. Must run BEFORE injecting synthetic messages
    // (e.g. deferred tool names) so the fingerprint reflects the actual user
    // input.
    let fingerprint = crate::utils::fingerprint::compute_fingerprint_from_messages(&api_messages);
    let message_refs = api_messages
        .iter()
        .filter_map(|message| match message {
            Message::User(user) => Some(MessageRef::User(user)),
            Message::Assistant(assistant) => Some(MessageRef::Assistant(assistant)),
            // Progress never reaches the API payload (CC filters it out of
            // messagesForAPI alongside the other non-model members).
            Message::System(_)
            | Message::Attachment(_)
            | Message::Progress(_)
            | Message::HookResult(_) => None,
        })
        .collect::<Vec<_>>();
    let local_messages = add_cache_breakpoints(
        &message_refs,
        enable_prompt_caching,
        Some(&options.query_source),
        should_use_cached_mc_for_request(options),
        options.cached_mc_new_cache_edits.as_ref(),
        &options.cached_mc_pinned_edits,
        options.skip_cache_write.unwrap_or(false),
    );
    super::api_trace::emit(
        &trace,
        "cache_breakpoints",
        serde_json::json!({
            "message_count": local_messages.len(),
            "enable_prompt_caching": enable_prompt_caching,
            "cached_mc": should_use_cached_mc_for_request(options),
        }),
    );

    let local_messages = strip_excess_media_items_typed(local_messages, API_MAX_MEDIA_PER_REQUEST);
    let (sdk_compatible_messages, has_private_message_extensions) =
        strip_private_cache_extensions_for_sdk(&local_messages);
    let private_messages = has_private_message_extensions
        .then(|| serde_json::to_value(&local_messages))
        .transpose()?;
    let sdk_messages = match convert_local_messages_to_sdk(&sdk_compatible_messages, &trace) {
        Ok(messages) => messages,
        Err(error) => {
            super::api_trace::emit_always_when_enabled(
                &trace,
                "request_failed",
                serde_json::json!({
                    "reason": "convert_error",
                    "error": error.to_string(),
                }),
            );
            return Err(error);
        }
    };

    if sdk_messages.is_empty() {
        anyhow::bail!("cannot query model without at least one user or assistant message");
    }

    // Maps to CC `services/api/claude.ts:1120-1236`: `isToolSearchEnabled(...,
    // tools, ...)`, `deferredToolNames`, and `filteredTools` are all derived
    // from the `tools` PARAMETER. `options.mcpTools` exists on CC's Options
    // (`:694`) but has no reader anywhere in CC; chaining it in here appended
    // the raw `appState.mcp.tools` on top of an already-assembled pool, which
    // (a) bypassed `assembleToolPool`'s MCP deny filter (`tools.ts:352`) and
    // (b) put a second copy of every MCP tool definition on the wire.
    let request_tools = tools;
    let mut tool_search_enabled =
        crate::tools::tool_search_tool::prompt::is_tool_search_enabled_for_request(
            &options.model,
            request_tools,
        );
    let deferred_tool_names = if tool_search_enabled {
        request_tools
            .iter()
            .filter(|tool| crate::tools::tool_search_tool::prompt::is_deferred_tool(tool))
            .map(|tool| tool.name.clone())
            .collect::<std::collections::BTreeSet<_>>()
    } else {
        std::collections::BTreeSet::new()
    };
    if tool_search_enabled
        && deferred_tool_names.is_empty()
        && !options.has_pending_mcp_servers.unwrap_or(false)
    {
        // Maps to CC: `Tool search disabled: no deferred tools available to search`
        crate::utils::debug::log_for_debugging(
            "Tool search disabled: no deferred tools available to search",
        );
        tool_search_enabled = false;
    }
    let discovered_tool_names = if tool_search_enabled {
        crate::utils::tool_search::extract_discovered_tool_names(messages)
    } else {
        std::collections::BTreeSet::new()
    };
    let filtered_tools = request_tools
        .iter()
        .filter(|tool| {
            if !tool_search_enabled {
                return !crate::types::tools::tool_matches_name(
                    tool,
                    crate::tools::tool_search_tool::prompt::TOOL_SEARCH_TOOL_NAME,
                );
            }
            if crate::types::tools::tool_matches_name(
                tool,
                crate::tools::tool_search_tool::prompt::TOOL_SEARCH_TOOL_NAME,
            ) {
                return true;
            }
            !deferred_tool_names.contains(&tool.name) || discovered_tool_names.contains(&tool.name)
        })
        .collect::<Vec<_>>();
    let skip_global_cache_for_system_prompt = should_use_global_cache_scope()
        && filtered_tools.iter().any(|tool| {
            tool.is_mcp && !(tool_search_enabled && deferred_tool_names.contains(&tool.name))
        });
    // Maps to CC `claude.ts:1358-1369`: `systemPrompt = asSystemPrompt([
    //   getAttributionHeader(fingerprint),
    //   getCLISyspromptPrefix({ isNonInteractive, hasAppendSystemPrompt }),
    //   ...systemPrompt,
    // ].filter(Boolean))` — the two leading blocks are what the server's
    // Claude Code cache policy prefix-matches on; `filter(Boolean)` drops the
    // header when attribution is disabled (empty string).
    let system_prompt = std::iter::once(crate::constants::system::get_attribution_header(
        &fingerprint,
    ))
    .chain(std::iter::once(
        crate::constants::system::get_cli_sysprompt_prefix(
            options.is_non_interactive_session,
            options.has_append_system_prompt,
        )
        .to_string(),
    ))
    .chain(system_prompt.iter().cloned())
    .filter(|block| !block.is_empty())
    .collect::<SystemPrompt>();
    // CC: logAPIPrefix(systemPrompt) — joins with analytics.
    let local_system_blocks = build_system_prompt_blocks(
        &system_prompt,
        enable_prompt_caching,
        skip_global_cache_for_system_prompt,
        Some(&options.query_source),
    );
    let private_system = local_system_blocks
        .iter()
        .any(|block| {
            block
                .cache_control
                .as_ref()
                .and_then(|cache_control| cache_control.scope.as_ref())
                .is_some()
        })
        .then(|| serde_json::to_value(&local_system_blocks))
        .transpose()?;
    let system = sdk_system_prompt_from_blocks(&local_system_blocks)?;
    // Maps to CC `claude.ts:1231-1246`: `filteredTools.map(tool =>
    // toolToAPISchema(tool, { getToolPermissionContext, tools, agents,
    // allowedAgentTypes, model, deferLoading }))`. Note CC passes the FULL
    // `tools` parameter (not filteredTools) into the options bag "so that
    // ToolSearchTool's prompt can list ALL available MCP tools"
    // (claude.ts:1232-1234); only the mapped list is filtered.
    let prompt_permission_context = options.tool_permission_context.clone().unwrap_or_default();
    let mut sdk_tools = filtered_tools
        .iter()
        .map(|tool| {
            crate::utils::api::tool_to_api_schema(
                tool,
                crate::utils::api::ToolToApiSchemaOptions {
                    model: Some(&options.model),
                    defer_loading: tool_search_enabled && deferred_tool_names.contains(&tool.name),
                    tool_permission_context: &prompt_permission_context,
                    tools: request_tools,
                    agents: &options.agents,
                    allowed_agent_types: options.allowed_agent_types.as_deref(),
                },
            )
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    if tool_search_enabled {
        let included_deferred = filtered_tools
            .iter()
            .filter(|tool| deferred_tool_names.contains(&tool.name))
            .count();
        // Maps to CC: `Dynamic tool loading: N/M deferred tools included`
        crate::utils::debug::log_for_debugging(&format!(
            "Dynamic tool loading: {included_deferred}/{} deferred tools included",
            deferred_tool_names.len()
        ));
    }
    if let Some(extra_tool_schemas) = options.extra_tool_schemas.as_ref() {
        for schema in extra_tool_schemas {
            sdk_tools.push(sdk_tool_union_from_json(schema.clone())?);
        }
    }
    if let Some(advisor_model) = options.advisor_model.as_ref() {
        // Maps to CC: `[AdvisorTool] Server-side tool enabled with …`
        crate::utils::debug::log_for_debugging(&format!(
            "[AdvisorTool] Server-side tool enabled with {advisor_model} as the advisor model"
        ));
        sdk_tools.push(sdk_tool_union_from_json(serde_json::json!({
            "type": "advisor_20260301",
            "name": "advisor",
            "model": advisor_model,
        }))?);
    }

    let max_tokens = options
        .max_output_tokens_override
        .unwrap_or_else(|| get_max_output_tokens_for_model(&options.model));
    let sdk_thinking = local_thinking_config_to_sdk(thinking_config, &options.model, max_tokens);
    let temperature = if matches!(
        &sdk_thinking,
        anthropic_sdk::resources::messages::ThinkingConfig::Disabled
    ) {
        Some(options.temperature_override.unwrap_or(1.0))
    } else {
        None
    };

    let params = anthropic_sdk::resources::beta::messages::BetaMessageCreateParams {
        max_tokens: max_tokens as i64,
        messages: serde_json::from_value(serde_json::to_value(&sdk_messages)?)?,
        model: normalize_model_string_for_api(&options.model),
        system,
        temperature,
        thinking: Some(sdk_thinking),
        metadata: Some(anthropic_sdk::resources::messages::Metadata {
            user_id: Some(get_api_metadata().user_id),
        }),
        tools: if sdk_tools.is_empty() {
            None
        } else {
            Some(
                sdk_tools
                    .into_iter()
                    .map(anthropic_sdk::resources::beta::messages::BetaToolUnion::Base)
                    .collect(),
            )
        },
        tool_choice: sdk_tool_choice_from_options(options)?,
        output_config: sdk_output_config_from_options(options)?,
        stream: Some(true),
        ..Default::default()
    };

    let private_body_overrides = SdkPrivateBodyOverrides {
        messages: private_messages,
        system: private_system,
    };
    let effective_body = sdk_message_body_with_private_overrides(&params, &private_body_overrides)
        .unwrap_or(JsonValue::Null);
    let tool_count = params.tools.as_ref().map(|t| t.len()).unwrap_or(0);
    super::api_trace::emit(
        &trace,
        "params_ready",
        serde_json::json!({
            "model": params.model,
            "message_count": params.messages.len(),
            "tool_count": tool_count,
            "max_tokens": params.max_tokens,
            "params": effective_body.clone(),
        }),
    );
    if !effective_body.is_null() {
        // Maps to CC `query.ts:588-590`
        // `createDumpPromptsFetch(toolUseContext.agentId ?? config.sessionId)`
        // — the dump file and its `dumpState` entry are keyed per SUBAGENT, and
        // `dump_prompts.rs` falls back to the session id when the id is absent.
        super::dump_prompts::dump_request_value(
            &effective_body,
            options.agent_id.as_ref().map(|id| id.0.as_str()),
        );
    }

    Ok(SdkMessageCreatePlan {
        params,
        private_body_overrides,
    })
}

#[cfg(test)]
fn build_sdk_message_create_params(
    messages: &[Message],
    system_prompt: &SystemPrompt,
    thinking_config: &ThinkingConfig,
    tools: &[Tool],
    options: &Options,
) -> anyhow::Result<anthropic_sdk::resources::beta::messages::BetaMessageCreateParams> {
    Ok(
        build_sdk_message_create_plan(messages, system_prompt, thinking_config, tools, options)?
            .params,
    )
}

fn strip_private_cache_extensions_for_sdk(messages: &[MessageParam]) -> (Vec<MessageParam>, bool) {
    let mut sanitized = messages.to_vec();
    let mut changed = false;
    for message in &mut sanitized {
        let MessageContent::Blocks(blocks) = &mut message.content else {
            continue;
        };
        blocks.retain_mut(|block| {
            if block.get("type").and_then(JsonValue::as_str) == Some("cache_edits") {
                changed = true;
                return false;
            }
            let Some(object) = block.as_object_mut() else {
                return true;
            };
            changed |= object.remove("cache_reference").is_some();
            if let Some(cache_control) = object
                .get_mut("cache_control")
                .and_then(JsonValue::as_object_mut)
            {
                changed |= cache_control.remove("scope").is_some();
            }
            true
        });
    }
    (sanitized, changed)
}

fn sdk_message_body_with_private_overrides(
    params: &anthropic_sdk::resources::beta::messages::BetaMessageCreateParams,
    overrides: &SdkPrivateBodyOverrides,
) -> anyhow::Result<JsonValue> {
    let mut body = serde_json::to_value(params)?;
    let object = body.as_object_mut().ok_or_else(|| {
        anyhow::anyhow!("SDK BetaMessageCreateParams did not serialize to an object")
    })?;
    if let Some(messages) = overrides.messages.as_ref() {
        object.insert("messages".to_string(), messages.clone());
    }
    if let Some(system) = overrides.system.as_ref() {
        object.insert("system".to_string(), system.clone());
    }
    Ok(body)
}

fn apply_sdk_private_body_overrides(
    request_options: &mut anthropic_sdk::RequestOptions,
    overrides: &SdkPrivateBodyOverrides,
) {
    if let Some(messages) = overrides.messages.as_ref() {
        request_options
            .json_body_patches
            .push(anthropic_sdk::JsonBodyPatch::set(
                "messages",
                messages.clone(),
            ));
    }
    if let Some(system) = overrides.system.as_ref() {
        request_options
            .json_body_patches
            .push(anthropic_sdk::JsonBodyPatch::set("system", system.clone()));
    }
}

/// Rust carrier for CC `queryModelWithStreaming(...)`'s closure-captured
/// `betas`, `useBetas`, Tool Search header, and original `options.model`.
#[derive(Debug, Clone)]
struct SdkRequestBetaCapture {
    betas: Vec<String>,
    emit_betas: bool,
    tool_search_header: Option<String>,
    original_model: String,
}

#[derive(Debug, Clone, Default)]
struct SdkRequestOptionsPlan {
    betas: Vec<String>,
    emit_betas: bool,
    extra_body_params: serde_json::Map<String, JsonValue>,
    output_config: serde_json::Map<String, JsonValue>,
    context_management: Option<JsonValue>,
}

/// Maps to CC `services/api/claude.ts` `paramsFromContext(...)` request
/// option side effects: beta headers and extra body fields are attached outside
/// the stable SDK `MessageCreateParams` shape while the generated body remains
/// owned by the SDK resource helper.
/// Applies one retry-local CC `paramsFromContext(...)` evaluation to an
/// immutable request-level beta capture.
fn build_sdk_message_request_options(
    messages: &[Message],
    thinking_config: &ThinkingConfig,
    options: &Options,
    capture: &SdkRequestBetaCapture,
    params: Option<&mut anthropic_sdk::resources::beta::messages::BetaMessageCreateParams>,
    mut base: anthropic_sdk::RequestOptions,
) -> anyhow::Result<anthropic_sdk::RequestOptions> {
    let plan = params_from_context(messages, thinking_config, options, capture)?;

    // Maps to CC `queryModelWithStreaming` / fetch `signal: abortController.signal`.
    if base.signal.is_none() {
        base.signal = options.abort_signal.clone();
    }

    if plan.emit_betas {
        // Official `paramsFromContext` also spreads `...(useBetas && { betas })`
        // onto the object passed to `anthropic.beta.messages.create`.
        if let Some(params) = params {
            params.betas = Some(plan.betas.clone());
        }
        let mut headers = base.headers.take().unwrap_or_default();
        headers.insert("anthropic-beta".to_string(), Some(plan.betas.join(",")));
        base.headers = Some(headers);
    }

    for (key, value) in plan.extra_body_params {
        base.json_body_patches
            .push(anthropic_sdk::JsonBodyPatch::set(key, value));
    }
    if !plan.output_config.is_empty() {
        base.json_body_patches
            .push(anthropic_sdk::JsonBodyPatch::set(
                "output_config",
                JsonValue::Object(plan.output_config),
            ));
    }
    if let Some(context_management) = plan.context_management {
        base.json_body_patches
            .push(anthropic_sdk::JsonBodyPatch::set(
                "context_management",
                context_management,
            ));
    }

    Ok(base)
}

#[cfg(test)]
fn sdk_request_beta_capture_for_testing(
    tools: &[Tool],
    options: &Options,
) -> SdkRequestBetaCapture {
    // Same `tools`-parameter-only derivation as the production capture; see the
    // note in `build_sdk_message_create_plan` (CC `claude.ts:1120-1236`).
    let request_tools = tools;
    let mut tool_search_enabled =
        crate::tools::tool_search_tool::prompt::is_tool_search_enabled_for_request(
            &options.model,
            request_tools,
        );
    let deferred_tool_names = if tool_search_enabled {
        request_tools
            .iter()
            .filter(|tool| crate::tools::tool_search_tool::prompt::is_deferred_tool(tool))
            .map(|tool| tool.name.clone())
            .collect::<std::collections::BTreeSet<_>>()
    } else {
        std::collections::BTreeSet::new()
    };
    if tool_search_enabled
        && deferred_tool_names.is_empty()
        && !options.has_pending_mcp_servers.unwrap_or(false)
    {
        tool_search_enabled = false;
    }

    let is_agentic_query = is_agentic_query_source(&options.query_source);
    let provider = crate::utils::model::providers::get_api_provider();
    let mut betas = get_merged_betas(&options.model, is_agentic_query);
    // Maps to CC `services/api/claude.ts:1028-1031`: Advisor appends at its
    // source position without a local includes guard.
    if options.advisor_model.is_some() {
        betas.push(ADVISOR_BETA_HEADER.to_string());
    }
    let tool_search_header = if tool_search_enabled {
        Some(get_tool_search_beta_header().to_string())
    } else {
        None
    };
    if let Some(tool_search_header) = tool_search_header.as_ref() {
        if provider != crate::utils::model::providers::ApiProvider::Bedrock
            && !betas.contains(tool_search_header)
        {
            betas.push(tool_search_header.clone());
        }
    }
    if should_use_global_cache_scope()
        && !betas.contains(&PROMPT_CACHING_SCOPE_BETA_HEADER.to_string())
    {
        betas.push(PROMPT_CACHING_SCOPE_BETA_HEADER.to_string());
    }

    // Maps to CC `services/api/claude.ts:1326,1515-1728`: `useBetas` and
    // base betas are captured once before Tool projection and retry-local
    // effort/task-budget/outputFormat additions.
    let emit_betas = !betas.is_empty();
    SdkRequestBetaCapture {
        betas,
        emit_betas,
        tool_search_header,
        original_model: options.model.clone(),
    }
}

#[cfg(test)]
fn build_sdk_request_options_plan(
    messages: &[Message],
    thinking_config: &ThinkingConfig,
    tools: &[Tool],
    options: &Options,
) -> anyhow::Result<SdkRequestOptionsPlan> {
    let capture = sdk_request_beta_capture_for_testing(tools, options);
    params_from_context(messages, thinking_config, options, &capture)
}

/// Maps to: CC `services/api/claude.ts:1538-1728` local
/// `paramsFromContext`; pure projection of one retry-local invocation.
fn params_from_context(
    messages: &[Message],
    thinking_config: &ThinkingConfig,
    options: &Options,
    capture: &SdkRequestBetaCapture,
) -> anyhow::Result<SdkRequestOptionsPlan> {
    let mut betas = capture.betas.clone();
    let emit_betas = capture.emit_betas;
    let provider = crate::utils::model::providers::get_api_provider();
    let bedrock_betas = if provider == crate::utils::model::providers::ApiProvider::Bedrock {
        let mut headers = get_bedrock_extra_body_params_betas(&options.model);
        // Maps to CC `services/api/claude.ts:1548-1554`: this is array
        // concatenation, not an includes-guarded merge.
        if let Some(tool_search_header) = capture.tool_search_header.as_ref() {
            headers.push(tool_search_header.clone());
        }
        headers
    } else {
        Vec::new()
    };
    let mut extra_body_params = if bedrock_betas.is_empty() {
        get_extra_body_params(None)
    } else {
        get_extra_body_params(Some(&bedrock_betas))
    };
    let mut output_config = take_extra_body_output_config(&mut extra_body_params);

    let applied_effort =
        resolve_applied_effort(&capture.original_model, options.effort_value.as_ref());
    configure_effort_params(
        applied_effort.as_ref(),
        &mut output_config,
        &mut extra_body_params,
        &mut betas,
        &capture.original_model,
    );
    configure_task_budget_params(options.task_budget.as_ref(), &mut output_config, &mut betas);

    if let Some(output_format) = options.output_format.as_ref() {
        if !output_config.contains_key("format") {
            output_config.insert("format".to_string(), output_format.clone());
            if model_supports_structured_outputs(&capture.original_model)
                && !betas.contains(&STRUCTURED_OUTPUTS_BETA_HEADER.to_string())
            {
                betas.push(STRUCTURED_OUTPUTS_BETA_HEADER.to_string());
            }
        }
    }

    let max_tokens = options
        .max_output_tokens_override
        .unwrap_or_else(|| get_max_output_tokens_for_model(&options.model));
    let sdk_thinking = local_thinking_config_to_sdk(thinking_config, &options.model, max_tokens);
    let has_thinking = !matches!(
        sdk_thinking,
        anthropic_sdk::resources::messages::ThinkingConfig::Disabled
    ) && !crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_DISABLE_THINKING")
            .as_deref(),
    );
    let context_management = if betas.contains(&CONTEXT_MANAGEMENT_BETA_HEADER.to_string()) {
        crate::services::compact::api_microcompact::get_api_context_management(
            has_thinking,
            betas.contains(&REDACT_THINKING_BETA_HEADER.to_string()),
            false,
        )
    } else {
        None
    };

    let _ = messages;
    Ok(SdkRequestOptionsPlan {
        betas,
        emit_betas,
        extra_body_params,
        output_config,
        context_management,
    })
}

fn is_agentic_query_source(query_source: &str) -> bool {
    query_source.starts_with("repl_main_thread")
        || query_source.starts_with("agent:")
        || query_source == "sdk"
        || query_source == "hook_agent"
        || query_source == "verification_agent"
}

fn should_strip_advisor_blocks_for_request(options: &Options) -> bool {
    // Maps to: CC `services/api/claude.ts` advisor request setup and
    // `stripAdvisorBlocks(...)` gate. The API rejects advisor server-tool
    // history without the advisor beta; when the beta is present (because the
    // advisor tool is enabled or explicitly supplied via ANTHROPIC_BETAS), the
    // history must be preserved for continuation parity.
    if options.advisor_model.is_some() {
        return false;
    }
    !get_merged_betas(
        &options.model,
        is_agentic_query_source(&options.query_source),
    )
    .iter()
    .any(|beta| beta == ADVISOR_BETA_HEADER)
}

fn take_extra_body_output_config(
    extra_body_params: &mut serde_json::Map<String, JsonValue>,
) -> serde_json::Map<String, JsonValue> {
    match extra_body_params.remove("output_config") {
        Some(JsonValue::Object(output_config)) => output_config,
        Some(value) => {
            extra_body_params.insert("output_config".to_string(), value);
            serde_json::Map::new()
        }
        None => serde_json::Map::new(),
    }
}

fn sdk_tool_union_from_json(
    value: serde_json::Value,
) -> anyhow::Result<anthropic_sdk::resources::messages::ToolUnion> {
    Ok(anthropic_sdk::resources::messages::ToolUnion::Raw(value))
}

fn sdk_tool_choice_from_options(
    options: &Options,
) -> anyhow::Result<Option<anthropic_sdk::resources::messages::ToolChoice>> {
    let Some(tool_choice) = options.tool_choice.as_ref() else {
        return Ok(None);
    };
    serde_json::from_value(tool_choice.clone())
        .map(Some)
        .map_err(|error| anyhow::anyhow!("failed to convert tool_choice to SDK params: {error}"))
}

fn sdk_output_config_from_options(
    options: &Options,
) -> anyhow::Result<Option<anthropic_sdk::resources::messages::OutputConfig>> {
    let Some(output_format) = options.output_format.as_ref() else {
        return Ok(None);
    };

    // Maps to: CC `services/api/claude.ts` merging
    // `options.outputFormat` into `output_config.format` before request
    // creation. Beta-header/provider gating remains in the direct SDK adapter
    // TODO surface; keeping the body field here lets queryHaiku/queryWithModel
    // and the main query seam preserve structured-output requests.
    let format = match serde_json::from_value::<anthropic_sdk::resources::messages::JsonOutputFormat>(
        output_format.clone(),
    ) {
        Ok(format) => format,
        Err(_) => anthropic_sdk::resources::messages::JsonOutputFormat {
            schema: output_format.clone(),
            type_name: "json_schema".to_string(),
        },
    };

    Ok(Some(anthropic_sdk::resources::messages::OutputConfig {
        effort: None,
        format: Some(format),
    }))
}

fn sdk_error_message_without_secrets(error: impl std::fmt::Display) -> String {
    let raw = error.to_string();
    let mut sanitized = raw.replace('\n', " ");
    for key in [
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "CLAUDE_CODE_OAUTH_TOKEN",
    ] {
        sanitized = sanitized.replace(key, "<redacted>");
        if let Some(secret) = crate::utils::process_env::var(key) {
            let trimmed = secret.trim();
            if !trimmed.is_empty() {
                sanitized = sanitized.replace(trimmed, "<redacted>");
            }
        }
    }
    sanitized
}

pub async fn query_model_streaming_text(
    messages: &[Message],
    system_prompt: &SystemPrompt,
    thinking_config: &ThinkingConfig,
    options: &Options,
) -> anyhow::Result<String> {
    let items =
        query_model_streaming_items(messages, system_prompt, thinking_config, options).await?;
    Ok(items
        .into_iter()
        .filter_map(|item| match item {
            ClaudeStreamItem::Text(text) => Some(text),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(""))
}

pub async fn query_model_streaming_items(
    messages: &[Message],
    system_prompt: &SystemPrompt,
    thinking_config: &ThinkingConfig,
    options: &Options,
) -> anyhow::Result<Vec<ClaudeStreamItem>> {
    let client = super::client::get_anthropic_client(super::client::GetAnthropicClientOptions {
        api_key: None,
        max_retries: 2,
        model: Some(options.model.clone()),
        source: Some(options.query_source.clone()),
    })
    .await?
    .build()?;
    let beta_capture = {
        // This text-only helper builds its plan with an EMPTY tool list (see the
        // `&[]` argument below), so the beta capture must gate on the same list.
        // It previously gated on `options.mcp_tools`, which no caller ever fills
        // and which CC never reads (`claude.ts:694` declared, zero readers).
        let request_tools: &[Tool] = &[];
        let mut tool_search_enabled =
            crate::tools::tool_search_tool::prompt::is_tool_search_enabled_for_request(
                &options.model,
                request_tools,
            );
        let deferred_tool_names = if tool_search_enabled {
            request_tools
                .iter()
                .filter(|tool| crate::tools::tool_search_tool::prompt::is_deferred_tool(tool))
                .map(|tool| tool.name.clone())
                .collect::<std::collections::BTreeSet<_>>()
        } else {
            std::collections::BTreeSet::new()
        };
        if tool_search_enabled
            && deferred_tool_names.is_empty()
            && !options.has_pending_mcp_servers.unwrap_or(false)
        {
            tool_search_enabled = false;
        }
        let mut betas = get_merged_betas(
            &options.model,
            is_agentic_query_source(&options.query_source),
        );
        if options.advisor_model.is_some() {
            betas.push(ADVISOR_BETA_HEADER.to_string());
        }
        let tool_search_header =
            tool_search_enabled.then(|| get_tool_search_beta_header().to_string());
        if let Some(header) = tool_search_header.as_ref() {
            if crate::utils::model::providers::get_api_provider()
                != crate::utils::model::providers::ApiProvider::Bedrock
                && !betas.contains(header)
            {
                betas.push(header.clone());
            }
        }
        if should_use_global_cache_scope()
            && !betas.contains(&PROMPT_CACHING_SCOPE_BETA_HEADER.to_string())
        {
            betas.push(PROMPT_CACHING_SCOPE_BETA_HEADER.to_string());
        }
        SdkRequestBetaCapture {
            emit_betas: !betas.is_empty(),
            betas,
            tool_search_header,
            original_model: options.model.clone(),
        }
    };
    let SdkMessageCreatePlan {
        mut params,
        private_body_overrides,
    } = build_sdk_message_create_plan(messages, system_prompt, thinking_config, &[], options)?;
    let mut request_options = build_sdk_message_request_options(
        messages,
        thinking_config,
        options,
        &beta_capture,
        Some(&mut params),
        anthropic_sdk::RequestOptions::default(),
    )?;
    apply_sdk_private_body_overrides(&mut request_options, &private_body_overrides);
    // Maps to CC `anthropic.beta.messages.create({ ...params, stream: true })`.
    let response = client
        .beta()
        .messages()
        .create_stream_with_response_and_options(&params, Some(&request_options))
        .await?;
    crate::services::claude_ai_limits::extract_quota_status_from_headers(
        &response.response.headers,
    );
    let mut stream = response.data;
    let mut blocks: HashMap<usize, ActiveClaudeStreamBlock> = HashMap::new();
    let mut items = Vec::new();

    use futures::StreamExt;
    while let Some(event) = stream.next().await {
        match serde_json::from_value(serde_json::to_value(&event?)?)? {
            anthropic_sdk::resources::messages::MessageStreamEvent::ContentBlockStart {
                content_block,
                index,
            } => {
                blocks.insert(
                    index,
                    ActiveClaudeStreamBlock::from_content_block(content_block),
                );
            }
            anthropic_sdk::resources::messages::MessageStreamEvent::ContentBlockDelta {
                delta,
                index,
            } => {
                if let Some(block) = blocks.get_mut(&index) {
                    block.push_delta(delta);
                }
            }
            anthropic_sdk::resources::messages::MessageStreamEvent::ContentBlockStop { index } => {
                if let Some(mut block) = blocks.remove(&index) {
                    block.normalize_api_content(
                        &[],
                        options.agent_id.as_ref().map(|id| id.0.as_str()),
                    )?;
                    if let Some(item) = block.into_item() {
                        items.push(item);
                    }
                }
            }
            _ => {}
        }
    }

    Ok(items)
}

fn stream_item_for_delta(
    delta: &anthropic_sdk::resources::messages::ContentBlockDelta,
) -> Option<ClaudeStreamItem> {
    match delta {
        anthropic_sdk::resources::messages::ContentBlockDelta::TextDelta { text }
            if !text.is_empty() =>
        {
            Some(ClaudeStreamItem::Text(text.clone()))
        }
        anthropic_sdk::resources::messages::ContentBlockDelta::ThinkingDelta { thinking }
            if !thinking.is_empty() =>
        {
            // Preview-only; signature arrives via SignatureDelta on the active
            // block and is attached at content_block_stop.
            Some(ClaudeStreamItem::Thinking {
                text: thinking.clone(),
                signature: String::new(),
            })
        }
        _ => None,
    }
}

#[derive(Debug)]
enum ActiveClaudeStreamBlock {
    Text(String),
    Thinking {
        thinking: String,
        signature: String,
    },
    RedactedThinking(String),
    ToolUse {
        id: String,
        name: String,
        input_json: String,
        is_server: bool,
    },
    WebSearchToolResult {
        tool_use_id: String,
        content: JsonValue,
    },
    Ignored,
}

impl ActiveClaudeStreamBlock {
    fn from_content_block(block: anthropic_sdk::resources::messages::ContentBlock) -> Self {
        match block {
            anthropic_sdk::resources::messages::ContentBlock::Text { text, .. } => Self::Text(text),
            anthropic_sdk::resources::messages::ContentBlock::Thinking { .. } => {
                // Maps to CC `services/api/claude.ts` content_block_start:
                // reset thinking text and initialize signature so the field
                // exists even if signature_delta never arrives.
                Self::Thinking {
                    thinking: String::new(),
                    signature: String::new(),
                }
            }
            anthropic_sdk::resources::messages::ContentBlock::RedactedThinking { data } => {
                Self::RedactedThinking(data)
            }
            anthropic_sdk::resources::messages::ContentBlock::ToolUse { id, input, name } => {
                Self::ToolUse {
                    id,
                    name,
                    input_json: initial_tool_input_json(input),
                    is_server: false,
                }
            }
            anthropic_sdk::resources::messages::ContentBlock::ServerToolUse { id, input, name } => {
                if name == "advisor" {
                    // Maps to CC: `[AdvisorTool] Advisor tool called`
                    crate::utils::debug::log_for_debugging("[AdvisorTool] Advisor tool called");
                }
                Self::ToolUse {
                    id,
                    name,
                    input_json: initial_tool_input_json(input),
                    is_server: true,
                }
            }
            anthropic_sdk::resources::messages::ContentBlock::WebSearchToolResult {
                content,
                tool_use_id,
            } => Self::WebSearchToolResult {
                tool_use_id,
                content: serde_json::to_value(content).unwrap_or(JsonValue::Null),
            },
        }
    }

    fn push_delta(&mut self, delta: anthropic_sdk::resources::messages::ContentBlockDelta) {
        match (self, delta) {
            (
                Self::Text(text),
                anthropic_sdk::resources::messages::ContentBlockDelta::TextDelta { text: delta },
            ) => {
                text.push_str(&delta);
            }
            (
                Self::Thinking { thinking, .. },
                anthropic_sdk::resources::messages::ContentBlockDelta::ThinkingDelta {
                    thinking: delta,
                },
            ) => {
                thinking.push_str(&delta);
            }
            (
                Self::Thinking { signature, .. },
                anthropic_sdk::resources::messages::ContentBlockDelta::SignatureDelta {
                    signature: delta,
                },
            ) => {
                // Maps to CC: `contentBlock.signature = delta.signature`
                *signature = delta;
            }
            (
                Self::ToolUse { input_json, .. },
                anthropic_sdk::resources::messages::ContentBlockDelta::InputJsonDelta {
                    partial_json,
                },
            ) => {
                input_json.push_str(&partial_json);
            }
            _ => {}
        }
    }

    /// L1 carrier for CC normalizeContentFromAPI([contentBlock], tools, agentId).
    /// Preserve the raw streamed JSON string until the canonical source owner parses it.
    fn normalize_api_content(
        &mut self,
        tools: &[Tool],
        agent_id: Option<&str>,
    ) -> anyhow::Result<()> {
        if let Self::ToolUse {
            id,
            name,
            input_json,
            is_server,
        } = self
        {
            let raw = serde_json::json!({
                "type": if *is_server { "server_tool_use" } else { "tool_use" },
                "id": id, "name": name, "input": input_json,
            });
            let normalized =
                crate::utils::messages::normalize_content_from_api(&[raw], tools, agent_id)
                    .map_err(anyhow::Error::msg)?;
            *input_json = serde_json::to_string(&normalized[0]["input"])?;
        }
        Ok(())
    }

    fn into_item(self) -> Option<ClaudeStreamItem> {
        match self {
            Self::Text(text) => Some(ClaudeStreamItem::Text(text)),
            Self::Thinking {
                thinking,
                signature,
            } if !thinking.is_empty() => Some(ClaudeStreamItem::Thinking {
                text: thinking,
                signature,
            }),
            Self::RedactedThinking(data) => Some(ClaudeStreamItem::RedactedThinking(data)),
            Self::ToolUse {
                id,
                name,
                input_json,
                is_server,
            } => Some(ClaudeStreamItem::ToolUse {
                id,
                name,
                input: parse_tool_input_json(&input_json),
                is_server,
            }),
            Self::WebSearchToolResult {
                tool_use_id,
                content,
            } => Some(ClaudeStreamItem::WebSearchToolResult {
                tool_use_id,
                content,
            }),
            _ => None,
        }
    }
}

fn initial_tool_input_json(input: JsonValue) -> String {
    if input.as_object().is_some_and(|object| object.is_empty()) || input.is_null() {
        String::new()
    } else {
        serde_json::to_string(&input).unwrap_or_default()
    }
}

fn parse_tool_input_json(input_json: &str) -> JsonValue {
    let trimmed = input_json.trim();
    if trimmed.is_empty() {
        return JsonValue::Object(serde_json::Map::new());
    }
    serde_json::from_str(trimmed).unwrap_or_else(|_| JsonValue::String(trimmed.to_string()))
}

fn claude_stream_item_to_assistant_content(
    item: ClaudeStreamItem,
) -> Option<crate::types::message::AssistantContent> {
    match item {
        ClaudeStreamItem::Text(text) => Some(crate::types::message::AssistantContent::Text(text)),
        ClaudeStreamItem::Thinking { text, signature } => {
            Some(crate::types::message::AssistantContent::Thinking { text, signature })
        }
        ClaudeStreamItem::RedactedThinking(data) => {
            Some(crate::types::message::AssistantContent::RedactedThinking { data })
        }
        ClaudeStreamItem::ToolUse {
            id,
            name,
            input,
            is_server,
        } => {
            let block = crate::types::message::ToolUseBlock {
                id: crate::types::ids::ToolUseId(id),
                name,
                input,
            };
            if is_server {
                Some(crate::types::message::AssistantContent::ServerToolUse(
                    block,
                ))
            } else {
                Some(crate::types::message::AssistantContent::ToolUse(block))
            }
        }
        ClaudeStreamItem::WebSearchToolResult {
            tool_use_id,
            content,
        } => Some(
            crate::types::message::AssistantContent::WebSearchToolResult {
                tool_use_id: crate::types::ids::ToolUseId(tool_use_id),
                content,
            },
        ),
    }
}

// ---------------------------------------------------------------------------
// queryModelWithoutStreaming
// ---------------------------------------------------------------------------

/// Maps to: CC services/api/claude.ts:709-750
///
/// Wraps queryModel async generator, collecting into a single AssistantMessage.
/// Consumes the stream and returns the final message.
///
pub async fn query_model_without_streaming(
    messages: &[Message],
    system_prompt: &SystemPrompt,
    thinking_config: &ThinkingConfig,
    tools: &[Tool],
    options: &Options,
) -> anyhow::Result<AssistantMessage> {
    // CC `queryModelWithoutStreaming(...)` consumes the streaming queryModel
    // generator and returns the final typed AssistantMessage. Keep SDK request
    // assembly in `query_model_with_streaming(...)` and only collect here.
    let mut stream =
        query_model_with_streaming(messages, system_prompt, thinking_config, tools, options)
            .await?;
    collect_query_model_stream_to_final_assistant(&mut stream).await
}

async fn collect_query_model_stream_to_final_assistant(
    stream: &mut tokio::sync::mpsc::Receiver<QueryModelStreamItem>,
) -> anyhow::Result<AssistantMessage> {
    let mut final_assistant = None;
    let mut last_error = None;
    while let Some(item) = stream.recv().await {
        match item {
            QueryModelStreamItem::Assistant(message) => {
                final_assistant = Some(message);
            }
            QueryModelStreamItem::SystemError(error) => {
                last_error = Some(error);
            }
            QueryModelStreamItem::ModelFallback {
                original_model,
                fallback_model,
            } => {
                return Err(anyhow::Error::new(
                    crate::services::api::with_retry::FallbackTriggeredError {
                        original_model,
                        fallback_model,
                    },
                ));
            }
            QueryModelStreamItem::AssistantDelta { stop_reason, usage } => {
                if let Some(assistant) = final_assistant.as_mut() {
                    assistant.stop_reason = stop_reason;
                    assistant.usage = usage;
                }
            }
            // Retry heartbeats are non-terminal notifications; a non-streaming
            // collector waits for the final assistant (CC `returnValue(...)`
            // drains the generator, discarding yields).
            QueryModelStreamItem::SystemApiError(_)
            | QueryModelStreamItem::Stream(_)
            | QueryModelStreamItem::Content(_)
            | QueryModelStreamItem::CompletedContent(_)
            | QueryModelStreamItem::StreamingFallback => {}
        }
    }

    if let Some(message) = final_assistant {
        return Ok(message);
    }
    if let Some(error) = last_error {
        let details = error.error_details.unwrap_or(error.api_error);
        return Err(anyhow::anyhow!(
            "queryModelWithoutStreaming failed: {} ({})",
            error.content,
            details
        ));
    }
    Err(anyhow::anyhow!(
        "queryModelWithoutStreaming ended without an AssistantMessage"
    ))
}

// ---------------------------------------------------------------------------
// queryModelWithStreaming
// ---------------------------------------------------------------------------

/// Convenience production stream opener for the REPL query actor.
/// Maps to the `query.ts -> deps.callModel(...) -> claude.ts#queryModelWithStreaming(...)`
/// API boundary while keeping SDK/request assembly in `services/api/claude.rs`.
///
/// Official `query.ts` supplies `fallbackModel` from `toolUseContext.options`;
/// this helper has no context, so it does not re-scan argv.
pub async fn open_model_stream_with_context(
    model_messages: &[Message],
    permission_context: &crate::tool::ToolPermissionContext,
    query_source: &str,
) -> anyhow::Result<tokio::sync::mpsc::Receiver<QueryModelStreamItem>> {
    let model = get_main_loop_model();
    let mut options = Options::new(model, query_source.to_string());
    options.query_source = query_source.to_string();
    // CC `Options.getToolPermissionContext` (claude.ts:677) — resolved snapshot.
    options.tool_permission_context = Some(permission_context.clone());
    let system_context = std::collections::BTreeMap::new();
    let system_prompt: SystemPrompt =
        crate::utils::api::append_system_context(&[], &system_context);
    let tools = crate::tools::get_tools(permission_context);
    let settings = crate::utils::settings::get_initial_settings();
    let thinking_config = production_thinking_config_from_env_and_settings(&settings);
    query_model_with_streaming(
        model_messages,
        &system_prompt,
        &thinking_config,
        &tools,
        &options,
    )
    .await
}

/// Maps to: CC services/api/claude.ts:752-780
///
/// Async generator yielding stream events. Returns a receiver that yields
/// StreamEvent, AssistantMessage, or SystemApiErrorMessage values.
///
/// In CC this is an async generator (`async function*`). In Rust we return
/// a tokio mpsc Receiver. The caller consumes items from the receiver.
///
pub async fn query_model_with_streaming(
    messages: &[Message],
    system_prompt: &SystemPrompt,
    thinking_config: &ThinkingConfig,
    tools: &[Tool],
    options: &Options,
) -> anyhow::Result<tokio::sync::mpsc::Receiver<QueryModelStreamItem>> {
    let (tx, rx) = tokio::sync::mpsc::channel(32);
    // Maps to CC `services/api/claude.ts:1057-1230,1380`
    // `queryModelWithStreaming`: capture base betas/useBetas before Tool
    // projection and retry-local `paramsFromContext` invocations.
    let beta_capture = {
        // Maps to CC `claude.ts:1120-1236`: the tool-search gate reads the
        // `tools` PARAMETER. `options.mcpTools` has no reader in CC, and the
        // chain that used to be here re-added the raw (non-deny-filtered) MCP
        // set on top of the assembled pool.
        let request_tools = tools;
        let mut tool_search_enabled =
            crate::tools::tool_search_tool::prompt::is_tool_search_enabled_for_request(
                &options.model,
                request_tools,
            );
        let deferred_tool_names = if tool_search_enabled {
            request_tools
                .iter()
                .filter(|tool| crate::tools::tool_search_tool::prompt::is_deferred_tool(tool))
                .map(|tool| tool.name.clone())
                .collect::<std::collections::BTreeSet<_>>()
        } else {
            std::collections::BTreeSet::new()
        };
        if tool_search_enabled
            && deferred_tool_names.is_empty()
            && !options.has_pending_mcp_servers.unwrap_or(false)
        {
            tool_search_enabled = false;
        }

        let mut betas = get_merged_betas(
            &options.model,
            is_agentic_query_source(&options.query_source),
        );
        // CC has no includes guard for this source-position Advisor append.
        if options.advisor_model.is_some() {
            betas.push(ADVISOR_BETA_HEADER.to_string());
        }
        let tool_search_header =
            tool_search_enabled.then(|| get_tool_search_beta_header().to_string());
        if let Some(header) = tool_search_header.as_ref() {
            if crate::utils::model::providers::get_api_provider()
                != crate::utils::model::providers::ApiProvider::Bedrock
                && !betas.contains(header)
            {
                betas.push(header.clone());
            }
        }
        if should_use_global_cache_scope()
            && !betas.contains(&PROMPT_CACHING_SCOPE_BETA_HEADER.to_string())
        {
            betas.push(PROMPT_CACHING_SCOPE_BETA_HEADER.to_string());
        }
        SdkRequestBetaCapture {
            emit_betas: !betas.is_empty(),
            betas,
            tool_search_header,
            original_model: options.model.clone(),
        }
    };
    let stream = match open_sdk_message_stream_with_retry(
        messages,
        system_prompt,
        thinking_config,
        tools,
        options,
        &beta_capture,
        tx.clone(),
    )
    .await
    {
        Ok(stream) => stream,
        // Maps to CC `queryModel` / `withRetry`: `APIUserAbortError` or
        // `signal.aborted` before the stream is opened — propagate, no fallback.
        Err(error)
            if crate::utils::errors::is_abort_error(&error)
                || options
                    .abort_signal
                    .as_ref()
                    .is_some_and(anthropic_sdk::AbortSignal::is_aborted) =>
        {
            crate::utils::debug::log_for_debugging(&format!(
                "Streaming aborted by user before stream open: {error}"
            ));
            return Err(error);
        }
        Err(error)
            if should_fallback_stream_creation_error_to_non_streaming(&error)
                && !is_non_streaming_fallback_disabled() =>
        {
            let messages = messages.to_vec();
            let system_prompt = system_prompt.clone();
            let thinking_config = thinking_config.clone();
            let tools = tools.to_vec();
            let options = options.clone();
            let beta_capture = beta_capture.clone();
            crate::utils::debug::log_for_debugging(
                "Streaming endpoint error — falling back to non-streaming mode",
            );
            tokio::spawn(async move {
                let _ = tx.send(QueryModelStreamItem::StreamingFallback).await;
                match execute_non_streaming_fallback_to_assistant(
                    &messages,
                    &system_prompt,
                    &thinking_config,
                    &tools,
                    &options,
                    &beta_capture,
                    None,
                )
                .await
                {
                    Ok(assistant) => {
                        let _ = tx.send(QueryModelStreamItem::Assistant(assistant)).await;
                    }
                    Err(error) => {
                        crate::utils::debug::log_for_debugging(&format!(
                            "Non-streaming fallback also failed: {error}"
                        ));
                        if let Some(item) = model_fallback_stream_item_from_error(&error) {
                            let _ = tx.send(item).await;
                        } else {
                            let _ = tx
                                .send(QueryModelStreamItem::SystemError(
                                    system_api_error_message_from_error(error),
                                ))
                                .await;
                        }
                    }
                }
            });
            return Ok(rx);
        }
        Err(error) => {
            // Maps to CC: `Error in API request: …` (non-404 / non-fallback path).
            crate::utils::debug::log_for_debugging(&format!("Error in API request: {error}"));
            return Err(error);
        }
    };
    let messages = messages.to_vec();
    let system_prompt = system_prompt.clone();
    let thinking_config = thinking_config.clone();
    let tools = tools.to_vec();
    let options = options.clone();
    let beta_capture = beta_capture.clone();

    tokio::spawn(async move {
        if let Err(error) =
            drain_sdk_message_stream_to_channel(stream, &tools, &options, tx.clone()).await
        {
            // Maps to CC `queryModel` (~2434-2441, ~2794-2798):
            // `streamingError instanceof APIUserAbortError` && `signal.aborted`
            // → log and return without yielding an assistant/system error.
            // Interruption UI is owned by query.ts / REPL.
            let signal_aborted = options
                .abort_signal
                .as_ref()
                .is_some_and(anthropic_sdk::AbortSignal::is_aborted);
            if crate::utils::errors::is_abort_error(&error) && signal_aborted {
                crate::utils::debug::log_for_debugging(&format!(
                    "Streaming aborted by user: {error}"
                ));
                return;
            }

            crate::utils::debug::log_for_debugging(&format!(
                "Error streaming (will fall back if allowed): {error}"
            ));
            if is_non_streaming_fallback_disabled() {
                let _ = tx
                    .send(QueryModelStreamItem::SystemError(
                        system_api_error_message_from_error(error),
                    ))
                    .await;
                return;
            }

            // Maps to CC `queryModel(...)` catch around the raw stream drain:
            // notify `query.ts`/`query_loop(...)` that partial streamed rows
            // must be discarded, then recover by issuing an equivalent
            // non-streaming request through `executeNonStreamingRequest(...)`.
            let initial_consecutive_529_errors = if stream_error_is_529(&error) {
                Some(1)
            } else {
                None
            };
            let _ = tx.send(QueryModelStreamItem::StreamingFallback).await;
            match execute_non_streaming_fallback_to_assistant(
                &messages,
                &system_prompt,
                &thinking_config,
                &tools,
                &options,
                &beta_capture,
                initial_consecutive_529_errors,
            )
            .await
            {
                Ok(assistant) => {
                    let _ = tx.send(QueryModelStreamItem::Assistant(assistant)).await;
                }
                Err(error) => {
                    crate::utils::debug::log_for_debugging(&format!(
                        "Non-streaming fallback also failed: {error}"
                    ));
                    if let Some(item) = model_fallback_stream_item_from_error(&error) {
                        let _ = tx.send(item).await;
                    } else {
                        let _ = tx
                            .send(QueryModelStreamItem::SystemError(
                                system_api_error_message_from_error(error),
                            ))
                            .await;
                    }
                }
            }
        }
    });

    Ok(rx)
}

type SdkMessageStream = anthropic_sdk::core::streaming::SseStream<
    anthropic_sdk::resources::beta::messages::BetaMessageStreamEvent,
>;

async fn open_sdk_message_stream_with_retry(
    messages: &[Message],
    system_prompt: &SystemPrompt,
    thinking_config: &ThinkingConfig,
    tools: &[Tool],
    options: &Options,
    beta_capture: &SdkRequestBetaCapture,
    tx: tokio::sync::mpsc::Sender<QueryModelStreamItem>,
) -> anyhow::Result<SdkMessageStream> {
    // Maps to CC `queryModelWithStreaming(...)` wrapping raw stream creation in
    // `withRetry(...)` with SDK auto-retry disabled. A
    // `FallbackTriggeredError` remains an `Err` from `callModel(...)`, so
    // `query.ts`/`query_loop(...)` owns the model switch and signature stripping.
    // Maps to CC `queryModel` (`claude.ts:1848-1856`): drive the withRetry
    // generator and re-yield every non-stream value — the yielded
    // `SystemAPIErrorMessage` itself — into the
    // `StreamEvent | AssistantMessage | SystemAPIErrorMessage` union
    // (`claude.ts:1025`). No conversion: the message `withRetry` built via
    // `createSystemAPIErrorMessage` (utils/messages.ts:4585-4603) is the value.
    let (heartbeat_tx, mut heartbeat_rx) = tokio::sync::mpsc::channel(32);
    let heartbeat_forward_tx = tx.clone();
    tokio::spawn(async move {
        while let Some(heartbeat) = heartbeat_rx.recv().await {
            if heartbeat_forward_tx
                .send(QueryModelStreamItem::SystemApiError(heartbeat))
                .await
                .is_err()
            {
                break;
            }
        }
    });

    // CC captures base betas/useBetas before Tool projection and reuses that
    // immutable base for every paramsFromContext retry.
    let beta_capture_for_operation = beta_capture.clone();
    let messages_for_operation = messages.to_vec();
    let system_prompt_for_operation = system_prompt.clone();
    let tools_for_operation = tools.to_vec();
    let options_for_operation = options.clone();
    let options_for_retry = options.clone();
    let thinking_config_for_retry = thinking_config.clone();

    let request_trace = super::api_trace::ApiTrace::new(1);
    super::api_trace::set_current_trace(Some(request_trace.clone()));

    let result = crate::services::api::with_retry::with_retry(
        || async { Ok(()) },
        move |attempt, retry_context| {
            let messages = messages_for_operation.clone();
            let system_prompt = system_prompt_for_operation.clone();
            let tools = tools_for_operation.clone();
            let base_options = options_for_operation.clone();
            let beta_capture = beta_capture_for_operation.clone();
            let attempt_trace = request_trace.with_attempt(attempt);
            async move {
                super::api_trace::set_current_trace(Some(attempt_trace.clone()));
                let attempt_options =
                    streaming_attempt_options_from_retry_context(&base_options, &retry_context);
                let client =
                    super::client::get_anthropic_client(super::client::GetAnthropicClientOptions {
                        api_key: None,
                        max_retries: 0,
                        model: Some(attempt_options.model.clone()),
                        source: Some(attempt_options.query_source.clone()),
                    })
                    .await
                    .map_err(crate::services::api::with_retry::RetryableError::Other)?
                    .build()
                    .map_err(|error| {
                        crate::services::api::with_retry::RetryableError::Other(anyhow::Error::new(
                            error,
                        ))
                    })?;
                let SdkMessageCreatePlan {
                    mut params,
                    private_body_overrides,
                } = build_sdk_message_create_plan(
                    &messages,
                    &system_prompt,
                    &retry_context.thinking_config,
                    &tools,
                    &attempt_options,
                )
                .map_err(crate::services::api::with_retry::RetryableError::Other)?;
                let mut request_options = build_sdk_message_request_options(
                    &messages,
                    &retry_context.thinking_config,
                    &attempt_options,
                    &beta_capture,
                    Some(&mut params),
                    anthropic_sdk::RequestOptions {
                        max_retries: Some(0),
                        ..Default::default()
                    },
                )
                .map_err(crate::services::api::with_retry::RetryableError::Other)?;
                apply_sdk_private_body_overrides(&mut request_options, &private_body_overrides);

                let response = client
                    .beta()
                    .messages()
                    .create_stream_with_response_and_options(&params, Some(&request_options))
                    .await
                    .map_err(sdk_api_error_to_retryable)?;
                crate::services::claude_ai_limits::extract_quota_status_from_headers(
                    &response.response.headers,
                );
                let stream = response.data;
                super::api_trace::emit(
                    &attempt_trace,
                    "http_sent",
                    serde_json::json!({
                        "model": attempt_options.model,
                        "stream": true,
                        "message_count": params.messages.len(),
                    }),
                );
                Ok(stream)
            }
        },
        crate::services::api::with_retry::RetryOptions {
            max_retries: None,
            model: options_for_retry.model.clone(),
            fallback_model: options_for_retry.fallback_model.clone(),
            thinking_config: thinking_config_for_retry,
            fast_mode: options_for_retry.fast_mode,
            // Maps to CC `withRetry({ signal })` — same AbortSignal as fetch.
            abort_rx: options_for_retry
                .abort_signal
                .as_ref()
                .map(anthropic_sdk::AbortSignal::watch_receiver),
            query_source: Some(retry_query_source_from_api_source(
                &options_for_retry.query_source,
            )),
            initial_consecutive_529_errors: None,
        },
        heartbeat_tx,
    )
    .await;

    result.map_err(with_retry_error_to_anyhow)
}

fn streaming_attempt_options_from_retry_context(
    options: &Options,
    retry_context: &crate::services::api::with_retry::RetryContext,
) -> Options {
    // Maps to CC `paramsFromContext(context)` inside
    // `queryModelWithStreaming(...)`: retry attempts use the retry context's
    // current model and max-token override before creating the raw stream.
    let mut attempt_options = options.clone();
    attempt_options.model = retry_context.model.clone();
    if let Some(max_tokens_override) = retry_context.max_tokens_override {
        attempt_options.max_output_tokens_override =
            Some(max_tokens_override.min(u32::MAX as u64) as u32);
    }
    attempt_options
}

/// Maps to CC `CLAUDE_ENABLE_STREAM_WATCHDOG` gate around the idle timers.
fn stream_watchdog_enabled() -> bool {
    crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_ENABLE_STREAM_WATCHDOG")
            .as_deref(),
    )
}

/// Maps to CC `STREAM_IDLE_TIMEOUT_MS` (`CLAUDE_STREAM_IDLE_TIMEOUT_MS` or 90s).
fn stream_idle_timeout_ms() -> u64 {
    crate::utils::process_env::var("CLAUDE_STREAM_IDLE_TIMEOUT_MS")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|ms| *ms > 0)
        .unwrap_or(90_000)
}

/// Maps to CC `STALL_THRESHOLD_MS` (30s gap between stream events).
const STREAM_STALL_THRESHOLD_MS: u64 = 30_000;

fn stream_event_type_label(
    event: &anthropic_sdk::resources::messages::MessageStreamEvent,
) -> &'static str {
    match event {
        anthropic_sdk::resources::messages::MessageStreamEvent::MessageStart { .. } => {
            "message_start"
        }
        anthropic_sdk::resources::messages::MessageStreamEvent::MessageDelta { .. } => {
            "message_delta"
        }
        anthropic_sdk::resources::messages::MessageStreamEvent::MessageStop => "message_stop",
        anthropic_sdk::resources::messages::MessageStreamEvent::ContentBlockStart { .. } => {
            "content_block_start"
        }
        anthropic_sdk::resources::messages::MessageStreamEvent::ContentBlockDelta { .. } => {
            "content_block_delta"
        }
        anthropic_sdk::resources::messages::MessageStreamEvent::ContentBlockStop { .. } => {
            "content_block_stop"
        }
        anthropic_sdk::resources::messages::MessageStreamEvent::Ping => "ping",
    }
}

/// Maps to CC `services/api/claude.ts:2171-2208` `content_block_stop`.
fn assistant_message_for_completed_block(
    assistant_content: crate::types::message::AssistantContent,
    model: Option<String>,
    request_id: Option<String>,
    api_message_id: Option<String>,
    stop_reason: Option<crate::types::message::StopReason>,
    usage: Option<crate::types::message::TokenUsage>,
) -> AssistantMessage {
    let identity = crate::types::message::AssistantMessageIdentity::new(request_id, api_message_id);
    AssistantMessage {
        uuid: uuid::Uuid::new_v4().to_string(),
        timestamp: chrono::Utc::now(),
        content: vec![
            assistant_content,
            crate::types::message::AssistantContent::MessageIdentity(identity),
        ],
        model,
        stop_reason,
        usage,
    }
}

async fn drain_sdk_message_stream_to_channel(
    mut stream: SdkMessageStream,
    tools: &[Tool],
    options: &Options,
    tx: tokio::sync::mpsc::Sender<QueryModelStreamItem>,
) -> anyhow::Result<()> {
    let mut blocks: HashMap<usize, ActiveClaudeStreamBlock> = HashMap::new();
    let mut content_block_count = 0usize;
    let mut has_tool_use = false;
    let mut response_model = Some(options.model.clone());
    let mut response_request_id = None;
    let mut response_api_message_id = None;
    let mut stop_reason = None;
    let mut usage = None;
    let mut deferred_api_error: Option<SystemApiErrorMessage> = None;
    let stream_started_at = std::time::Instant::now();
    let mut saw_first_stream_event = false;
    let mut stream_event_count: u64 = 0;
    let mut stream_event_types: Vec<&'static str> = Vec::new();
    let mut first_ttft_ms: Option<u64> = None;

    // Maps to CC streaming idle timeout watchdog + stall detection
    // (`services/api/claude.ts` around STREAM_IDLE_TIMEOUT_MS / STALL_THRESHOLD_MS).
    let watchdog_enabled = stream_watchdog_enabled();
    let idle_timeout = std::time::Duration::from_millis(stream_idle_timeout_ms());
    let idle_warning = idle_timeout / 2;
    let mut last_event_at: Option<std::time::Instant> = None;
    let mut stall_count: u64 = 0;
    let mut total_stall_time = std::time::Duration::ZERO;
    let mut idle_period_started = std::time::Instant::now();
    let mut idle_warning_emitted = false;
    // Maps to CC `signal` on `beta.messages.create(..., { signal })`: Esc must
    // stop mid-SSE drain after the HTTP handshake (request-level abort alone
    // is insufficient once the body stream is open).
    let abort_signal = options.abort_signal.clone();

    use futures::StreamExt;
    loop {
        // Maps to CC `signal.aborted` checks around the stream consumer.
        if abort_signal
            .as_ref()
            .is_some_and(anthropic_sdk::AbortSignal::is_aborted)
        {
            drop(stream);
            return Err(anyhow::Error::new(anthropic_sdk::ApiError::UserAbort {
                message: "Request was aborted.".to_owned(),
            }));
        }

        let next_event = stream.next();
        tokio::pin!(next_event);
        let abort_wait = async {
            match abort_signal.clone() {
                Some(mut signal) => signal.aborted().await,
                None => futures::future::pending().await,
            }
        };
        tokio::pin!(abort_wait);

        let event = if watchdog_enabled {
            let until_warning = idle_warning.saturating_sub(idle_period_started.elapsed());
            let until_timeout = idle_timeout.saturating_sub(idle_period_started.elapsed());
            tokio::select! {
                biased;
                event = &mut next_event => event,
                _ = &mut abort_wait => {
                    drop(next_event);
                    drop(stream);
                    return Err(anyhow::Error::new(anthropic_sdk::ApiError::UserAbort {
                        message: "Request was aborted.".to_owned(),
                    }));
                }
                _ = tokio::time::sleep(until_warning), if !idle_warning_emitted => {
                    let warn_ms = idle_warning.as_millis();
                    crate::utils::debug::log_for_debugging_with_level(
                        &format!(
                            "Streaming idle warning: no chunks received for {}s",
                            warn_ms as f64 / 1000.0
                        ),
                        crate::utils::debug::DebugLogLevel::Warn,
                    );
                    idle_warning_emitted = true;
                    continue;
                }
                _ = tokio::time::sleep(until_timeout) => {
                    crate::utils::debug::log_for_debugging_with_level(
                        &format!(
                            "Streaming idle timeout: no chunks received for {}s, aborting stream",
                            idle_timeout.as_secs()
                        ),
                        crate::utils::debug::DebugLogLevel::Error,
                    );
                    // Drop the stream to abort the underlying SSE body (CC
                    // `releaseStreamResources()` / abort controller).
                    drop(next_event);
                    drop(stream);
                    super::api_trace::emit_current(
                        "request_failed",
                        serde_json::json!({
                            "reason": "stream_idle_timeout",
                            "event_count": stream_event_count,
                        }),
                    );
                    anyhow::bail!("Stream idle timeout - no chunks received");
                }
            }
        } else {
            tokio::select! {
                biased;
                event = &mut next_event => event,
                _ = &mut abort_wait => {
                    drop(next_event);
                    drop(stream);
                    return Err(anyhow::Error::new(anthropic_sdk::ApiError::UserAbort {
                        message: "Request was aborted.".to_owned(),
                    }));
                }
            }
        };

        let Some(event) = event else {
            break;
        };
        let event = match event {
            Ok(event) => serde_json::from_value(serde_json::to_value(&event)?)?,
            Err(error) => {
                super::api_trace::emit_current(
                    "request_failed",
                    serde_json::json!({
                        "reason": "stream_error",
                        "error": error.to_string(),
                        "event_count": stream_event_count,
                    }),
                );
                return Err(error.into());
            }
        };
        stream_event_count += 1;
        let event_label = stream_event_type_label(&event);
        if stream_event_types.len() < 64 {
            stream_event_types.push(event_label);
        }

        // Reset idle timers on every chunk (CC `resetStreamIdleTimer()`).
        idle_period_started = std::time::Instant::now();
        idle_warning_emitted = false;

        let now = std::time::Instant::now();
        // Stall detection: only after first event so TTFB is not counted.
        if let Some(last) = last_event_at {
            let gap = now.saturating_duration_since(last);
            if gap.as_millis() as u64 > STREAM_STALL_THRESHOLD_MS {
                stall_count += 1;
                total_stall_time += gap;
                crate::utils::debug::log_for_debugging_with_level(
                    &format!(
                        "Streaming stall detected: {:.1}s gap between events (stall #{stall_count}, event={})",
                        gap.as_secs_f64(),
                        event_label
                    ),
                    crate::utils::debug::DebugLogLevel::Warn,
                );
            }
        }
        last_event_at = Some(now);

        let ttft_ms = if saw_first_stream_event {
            None
        } else {
            saw_first_stream_event = true;
            // Maps to: CC `Stream started - received first chunk`
            crate::utils::debug::log_for_debugging("Stream started - received first chunk");
            let ms = stream_started_at.elapsed().as_millis() as u64;
            first_ttft_ms = Some(ms);
            Some(ms)
        };
        let event_request_id = match &event {
            anthropic_sdk::resources::messages::MessageStreamEvent::MessageStart { message } => {
                message.request_id.clone()
            }
            _ => None,
        };
        let mut raw_event =
            serde_json::to_value(&event).unwrap_or_else(|_| serde_json::Value::Null);
        // The SDK deliberately skips `_request_id` when serializing Message;
        // preserve the response-header metadata on the typed stream envelope
        // so forked suggestion consumers can join generation outcomes.
        if let Some(object) = raw_event.as_object_mut() {
            if let Some(request_id) = event_request_id {
                object.insert(
                    "request_id".to_string(),
                    serde_json::Value::String(request_id),
                );
            }
        }
        if tx
            .send(QueryModelStreamItem::Stream(StreamEvent::ApiEvent {
                event: raw_event,
                ttft_ms,
            }))
            .await
            .is_err()
        {
            return Ok(());
        }
        match event {
            anthropic_sdk::resources::messages::MessageStreamEvent::MessageStart { message } => {
                response_request_id = message.request_id.clone();
                response_api_message_id = Some(message.id.clone());
                response_model = Some(message.model);
                usage = Some(sdk_usage_to_token_usage(&message.usage));
            }
            anthropic_sdk::resources::messages::MessageStreamEvent::MessageDelta {
                delta,
                usage: delta_usage,
            } => {
                let raw_stop_reason = delta.stop_reason.clone();
                if let Some(reason) = sdk_stop_reason_to_message_stop_reason(delta.stop_reason) {
                    stop_reason = Some(reason);
                }
                if matches!(
                    raw_stop_reason,
                    Some(anthropic_sdk::resources::messages::StopReason::MaxTokens)
                ) {
                    let max_output_tokens = options
                        .max_output_tokens_override
                        .unwrap_or_else(|| get_max_output_tokens_for_model(&options.model));
                    deferred_api_error = Some(max_output_tokens_system_api_error_message(
                        max_output_tokens,
                    ));
                }
                usage = Some(merge_sdk_delta_usage(usage, &delta_usage));
                if tx
                    .send(QueryModelStreamItem::AssistantDelta {
                        stop_reason: stop_reason.clone(),
                        usage: usage.clone(),
                    })
                    .await
                    .is_err()
                {
                    return Ok(());
                }
            }
            anthropic_sdk::resources::messages::MessageStreamEvent::ContentBlockStart {
                content_block,
                index,
            } => {
                blocks.insert(
                    index,
                    ActiveClaudeStreamBlock::from_content_block(content_block),
                );
            }
            anthropic_sdk::resources::messages::MessageStreamEvent::ContentBlockDelta {
                delta,
                index,
            } => {
                let delta_item = stream_item_for_delta(&delta);
                if let Some(block) = blocks.get_mut(&index) {
                    block.push_delta(delta);
                }
                if let Some(item) = delta_item {
                    if tx.send(QueryModelStreamItem::Content(item)).await.is_err() {
                        return Ok(());
                    }
                }
            }
            anthropic_sdk::resources::messages::MessageStreamEvent::ContentBlockStop { index } => {
                if let Some(mut block) = blocks.remove(&index) {
                    block.normalize_api_content(
                        tools,
                        options.agent_id.as_ref().map(|id| id.0.as_str()),
                    )?;
                    if let Some(item) = block.into_item() {
                        has_tool_use |= matches!(
                            item,
                            ClaudeStreamItem::ToolUse {
                                is_server: false,
                                ..
                            }
                        );
                        let Some(assistant_content) = claude_stream_item_to_assistant_content(item)
                        else {
                            continue;
                        };
                        content_block_count += 1;
                        let assistant = assistant_message_for_completed_block(
                            assistant_content,
                            response_model.clone(),
                            response_request_id.clone(),
                            response_api_message_id.clone(),
                            stop_reason.clone(),
                            usage.clone(),
                        );
                        // Maps to CC `services/api/claude.ts:2171-2208`: every
                        // completed API content block is a distinct typed
                        // AssistantMessage with its own transcript UUID.
                        if tx
                            .send(QueryModelStreamItem::Assistant(assistant))
                            .await
                            .is_err()
                        {
                            return Ok(());
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // Maps to CC stall summary after a clean stream exit.
    if stall_count > 0 {
        crate::utils::debug::log_for_debugging_with_level(
            &format!(
                "Streaming completed with {stall_count} stall(s), total stall time: {:.1}s",
                total_stall_time.as_secs_f64()
            ),
            crate::utils::debug::DebugLogLevel::Warn,
        );
    }

    let assistant_stop_reason = stop_reason;
    let assistant_model = response_model.or_else(|| Some(options.model.clone()));
    let duration_ms = stream_started_at.elapsed().as_millis() as u64;
    super::api_trace::emit_current(
        "stream_meta",
        serde_json::json!({
            "event_count": stream_event_count,
            "event_types": stream_event_types,
            "ttft_ms": first_ttft_ms,
            "duration_ms": duration_ms,
            "stall_count": stall_count,
            "stop_reason": assistant_stop_reason.as_ref().map(|r| format!("{r:?}")),
            "content_blocks": content_block_count,
            "has_tool_use": has_tool_use,
        }),
    );
    super::dump_prompts::dump_response_value(
        &serde_json::json!({
            "stream": true,
            "chunks": stream_event_types.iter().map(|t| serde_json::json!({ "type": t })).collect::<Vec<_>>(),
            "ttft_ms": first_ttft_ms,
            "duration_ms": duration_ms,
            "stop_reason": assistant_stop_reason.as_ref().map(|r| format!("{r:?}")),
        }),
        // CC's `createDumpPromptsFetch` closure (`dumpPrompts.ts:146-190`) owns
        // request AND response with the ONE `agentIdOrSessionId` it was built
        // with, so both halves land in the same per-agent file.
        options.agent_id.as_ref().map(|id| id.0.as_str()),
    );
    super::api_trace::emit_current(
        "request_done",
        serde_json::json!({
            "duration_ms": duration_ms,
            "event_count": stream_event_count,
            "model": assistant_model,
        }),
    );
    if assistant_stop_reason == Some(crate::types::message::StopReason::Refusal) {
        let model = assistant_model.as_deref().unwrap_or(&options.model);
        if let Some(error) = refusal_system_api_error_message(model) {
            if tx
                .send(QueryModelStreamItem::SystemError(error))
                .await
                .is_err()
            {
                return Ok(());
            }
        }
    }
    if let Some(error) = deferred_api_error {
        let _ = tx.send(QueryModelStreamItem::SystemError(error)).await;
    }

    Ok(())
}

fn max_output_tokens_system_api_error_message(max_output_tokens: u32) -> SystemApiErrorMessage {
    let content = format!(
        "{}: Claude's response exceeded the {} output token maximum. To configure this behavior, set the CLAUDE_CODE_MAX_OUTPUT_TOKENS environment variable.",
        crate::services::api::errors::API_ERROR_MESSAGE_PREFIX,
        max_output_tokens,
    );
    SystemApiErrorMessage {
        content: content.clone(),
        api_error: "max_output_tokens".to_string(),
        error: "max_output_tokens".to_string(),
        error_details: Some(content),
    }
}

fn refusal_system_api_error_message(model: &str) -> Option<SystemApiErrorMessage> {
    let api_message = crate::services::api::errors::get_error_message_if_refusal(
        Some(crate::services::api::errors::ApiStopReason::Refusal),
        model,
    )?;
    let content = api_message
        .content
        .iter()
        .map(|block| block.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let error = api_message
        .error
        .map(|error| error.as_str().to_string())
        .unwrap_or_else(|| "invalid_request".to_string());
    Some(SystemApiErrorMessage {
        content: content.clone(),
        api_error: content,
        error,
        error_details: None,
    })
}

fn sdk_stop_reason_to_message_stop_reason(
    stop_reason: Option<anthropic_sdk::resources::messages::StopReason>,
) -> Option<crate::types::message::StopReason> {
    stop_reason.map(|reason| match reason {
        anthropic_sdk::resources::messages::StopReason::EndTurn => {
            crate::types::message::StopReason::EndTurn
        }
        anthropic_sdk::resources::messages::StopReason::ToolUse => {
            crate::types::message::StopReason::ToolUse
        }
        anthropic_sdk::resources::messages::StopReason::MaxTokens => {
            crate::types::message::StopReason::MaxTokens
        }
        anthropic_sdk::resources::messages::StopReason::StopSequence => {
            crate::types::message::StopReason::StopSequence
        }
        anthropic_sdk::resources::messages::StopReason::PauseTurn => {
            crate::types::message::StopReason::PauseTurn
        }
        anthropic_sdk::resources::messages::StopReason::Refusal => {
            crate::types::message::StopReason::Refusal
        }
    })
}

fn sdk_usage_to_token_usage(
    usage: &anthropic_sdk::resources::messages::Usage,
) -> crate::types::message::TokenUsage {
    crate::types::message::TokenUsage {
        input_tokens: nonnegative_i64_to_u64(usage.input_tokens),
        output_tokens: nonnegative_i64_to_u64(usage.output_tokens),
        cache_creation_input_tokens: usage
            .cache_creation_input_tokens
            .map(nonnegative_i64_to_u64)
            .unwrap_or(0),
        cache_read_input_tokens: usage
            .cache_read_input_tokens
            .map(nonnegative_i64_to_u64)
            .unwrap_or(0),
        // CC reads this undocumented cache-editing field via an `unknown`
        // cast because it is absent from the v0.74 SDK Usage interface.
        cache_deleted_input_tokens: sdk_cache_deleted_input_tokens(usage).unwrap_or(0),
    }
}

fn merge_sdk_delta_usage(
    current: Option<crate::types::message::TokenUsage>,
    delta: &anthropic_sdk::resources::messages::MessageDeltaUsage,
) -> crate::types::message::TokenUsage {
    let current = current.unwrap_or_default();
    crate::types::message::TokenUsage {
        input_tokens: delta
            .input_tokens
            .map(nonnegative_i64_to_u64)
            .unwrap_or(current.input_tokens),
        output_tokens: nonnegative_i64_to_u64(delta.output_tokens),
        cache_creation_input_tokens: delta
            .cache_creation_input_tokens
            .map(nonnegative_i64_to_u64)
            .unwrap_or(current.cache_creation_input_tokens),
        cache_read_input_tokens: delta
            .cache_read_input_tokens
            .map(nonnegative_i64_to_u64)
            .unwrap_or(current.cache_read_input_tokens),
        cache_deleted_input_tokens: sdk_cache_deleted_input_tokens(delta)
            .filter(|tokens| *tokens > 0)
            .unwrap_or(current.cache_deleted_input_tokens),
    }
}

/// Maps to: CC `services/api/claude.ts:2965-2980` dynamic access to the
/// undocumented `cache_deleted_input_tokens` usage extension.
///
/// Keeping the lookup at the serialized boundary avoids adding a non-v0.74
/// field back to the SDK's public `Usage` structs. Raw JSON responses retain
/// the value; typed v0.74 values correctly fall back to `None`.
fn sdk_cache_deleted_input_tokens<T: Serialize>(usage: &T) -> Option<u64> {
    serde_json::to_value(usage)
        .ok()?
        .get("cache_deleted_input_tokens")?
        .as_i64()
        .map(nonnegative_i64_to_u64)
}

fn nonnegative_i64_to_u64(value: i64) -> u64 {
    value.max(0) as u64
}

/// Items yielded by the queryModelWithStreaming channel.
/// The public event boundaries map to CC's
/// `StreamEvent | AssistantMessage | SystemAPIErrorMessage` generator, while
/// `Content`/legacy `CompletedContent` project previews, while
/// `AssistantDelta` models CC's post-yield reference mutation of the last
/// per-block assistant without mutating transcript history on every token.
#[derive(Debug, Clone)]
pub enum QueryModelStreamItem {
    Stream(StreamEvent),
    /// Streaming content-block delta/progress item. Text/thinking variants are
    /// previews only; the completed formal block is sent as `CompletedContent`.
    Content(ClaudeStreamItem),
    /// Legacy compatibility seam for injected deps that still separate a
    /// completed block from its assistant envelope. Production streaming emits
    /// `Assistant` directly at every `content_block_stop`.
    CompletedContent(ClaudeStreamItem),
    /// Rust transport for CC's post-yield reference mutation in
    /// `message_delta`: the final usage/stop reason is written back to the last
    /// assistant envelope yielded at `content_block_stop`.
    AssistantDelta {
        stop_reason: Option<crate::types::message::StopReason>,
        usage: Option<crate::types::message::TokenUsage>,
    },
    /// Maps to CC `options.onStreamingFallback()`: query.ts tombstones
    /// orphaned partial stream rows before accepting the non-streaming
    /// fallback assistant. Rust uses this typed signal to reset partial
    /// assistant buffers at the same generator boundary.
    StreamingFallback,
    /// Maps to CC `FallbackTriggeredError` thrown from
    /// `executeNonStreamingRequest(...)` and rethrown to `query.ts` so the
    /// query loop, not the API adapter, owns the model switch retry.
    ModelFallback {
        original_model: String,
        fallback_model: String,
    },
    Assistant(AssistantMessage),
    /// The `SystemAPIErrorMessage` component of CC's generator union
    /// (`claude.ts:1025`): the retry heartbeat `withRetry` yields
    /// (`withRetry.ts:493,509`), re-yielded whole by `queryModel`
    /// (`claude.ts:1848-1856`) and passed through `query.ts` untouched
    /// (`query.ts:659,824`) into REPL messages (`REPL.tsx:3496`).
    /// Non-terminal: the request is still retrying. Always the
    /// `SystemMessage::ApiError` member.
    ///
    /// Distinct from [`Self::SystemError`], the Rust-side terminal error row
    /// (CC yields assistant API-error messages for those paths — recorded
    /// seam).
    SystemApiError(crate::types::message::SystemMessage),
    SystemError(SystemApiErrorMessage),
}

async fn execute_non_streaming_fallback_to_assistant(
    messages: &[Message],
    system_prompt: &SystemPrompt,
    thinking_config: &ThinkingConfig,
    tools: &[Tool],
    options: &Options,
    beta_capture: &SdkRequestBetaCapture,
    initial_consecutive_529_errors: Option<u32>,
) -> anyhow::Result<AssistantMessage> {
    // Maps to CC `queryModel(...)` streaming-error fallback: reuse the same
    // paramsFromContext shape as the streaming attempt, run
    // `executeNonStreamingRequest(...)`, then normalize the returned message
    // into the typed assistant item yielded by the query generator.
    let result = execute_non_streaming_request_with_initial_consecutive_529_errors(
        &options.model,
        &options.query_source,
        options.fallback_model.as_deref(),
        thinking_config,
        || {
            let plan = build_sdk_message_create_plan(
                messages,
                system_prompt,
                thinking_config,
                tools,
                options,
            )?;
            sdk_message_body_with_private_overrides(&plan.params, &plan.private_body_overrides)
        },
        None,
        initial_consecutive_529_errors,
        || {
            build_sdk_message_request_options(
                messages,
                thinking_config,
                options,
                beta_capture,
                None,
                anthropic_sdk::RequestOptions::default(),
            )
        },
        options
            .abort_signal
            .as_ref()
            .map(anthropic_sdk::AbortSignal::watch_receiver),
    )
    .await?;
    sdk_message_response_to_assistant_message(
        result,
        tools,
        options.agent_id.as_ref().map(|id| id.0.as_str()),
    )
}

fn sdk_message_response_to_assistant_message(
    mut value: serde_json::Value,
    tools: &[Tool],
    agent_id: Option<&str>,
) -> anyhow::Result<AssistantMessage> {
    // CC :2574/:2671 normalizes before the yielded assistant enters persistence.
    if let Some(content) = value.get("content").and_then(serde_json::Value::as_array) {
        value["content"] =
            crate::utils::messages::normalize_content_from_api(content, tools, agent_id)
                .map_err(anyhow::Error::msg)?
                .into();
    }
    let cache_deleted_input_tokens = value.get("usage").and_then(sdk_cache_deleted_input_tokens);
    let message: anthropic_sdk::resources::messages::Message = serde_json::from_value(value)
        .map_err(|error| {
            anyhow::anyhow!("failed to parse non-streaming fallback response: {error}")
        })?;
    let identity = crate::types::message::AssistantMessageIdentity::new(
        message.request_id.clone(),
        Some(message.id.clone()),
    );
    let mut content = message
        .content
        .into_iter()
        .filter_map(|block| {
            ActiveClaudeStreamBlock::from_content_block(block)
                .into_item()
                .and_then(claude_stream_item_to_assistant_content)
        })
        .collect::<Vec<_>>();
    content.push(crate::types::message::AssistantContent::MessageIdentity(
        identity,
    ));
    let mut usage = sdk_usage_to_token_usage(&message.usage);
    if let Some(cache_deleted_input_tokens) = cache_deleted_input_tokens {
        usage.cache_deleted_input_tokens = cache_deleted_input_tokens;
    }
    Ok(AssistantMessage {
        uuid: uuid::Uuid::new_v4().to_string(),
        timestamp: chrono::Utc::now(),
        content,
        model: Some(message.model),
        stop_reason: sdk_stop_reason_to_message_stop_reason(message.stop_reason),
        usage: Some(usage),
    })
}

fn model_fallback_stream_item_from_error(error: &anyhow::Error) -> Option<QueryModelStreamItem> {
    let fallback =
        error.downcast_ref::<crate::services::api::with_retry::FallbackTriggeredError>()?;
    Some(QueryModelStreamItem::ModelFallback {
        original_model: fallback.original_model.clone(),
        fallback_model: fallback.fallback_model.clone(),
    })
}

fn system_api_error_message_from_error(error: anyhow::Error) -> SystemApiErrorMessage {
    let message = sdk_error_message_without_secrets(&error);
    SystemApiErrorMessage {
        content: message.clone(),
        api_error: message.clone(),
        error: message.clone(),
        error_details: Some(message),
    }
}

fn is_non_streaming_fallback_disabled() -> bool {
    // Maps to CC `CLAUDE_CODE_DISABLE_NONSTREAMING_FALLBACK`. GrowthBook gates
    // are not available in Cometix's direct API adapter, so only the explicit
    // env kill switch is honored here.
    crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_DISABLE_NONSTREAMING_FALLBACK")
            .as_deref(),
    )
}

fn should_fallback_stream_creation_error_to_non_streaming(error: &anyhow::Error) -> bool {
    // Maps to CC stream-creation catch: only a 404 from the streaming endpoint
    // falls back to non-streaming; model-fallback signals must continue to
    // propagate to query_loop for the official model switch retry.
    let Some(cannot_retry) =
        error.downcast_ref::<crate::services::api::with_retry::CannotRetryError>()
    else {
        return false;
    };
    matches!(
        &cannot_retry.original_error,
        crate::services::api::with_retry::RetryableError::Api(api) if api.status == Some(404)
    )
}

fn stream_error_is_529(error: &anyhow::Error) -> bool {
    if let Some(api_error) = error.downcast_ref::<anthropic_sdk::ApiError>() {
        return api_error.status() == Some(529);
    }
    if let Some(cannot_retry) =
        error.downcast_ref::<crate::services::api::with_retry::CannotRetryError>()
    {
        return matches!(
            &cannot_retry.original_error,
            crate::services::api::with_retry::RetryableError::Api(api) if api.status == Some(529)
        );
    }
    false
}

// ---------------------------------------------------------------------------
// executeNonStreamingRequest
// ---------------------------------------------------------------------------

/// Maps to: CC services/api/claude.ts:818-917
///
/// Helper for non-streaming API requests with retry logic.
/// Encapsulates the common pattern of creating a withRetry generator,
/// iterating to yield system messages, and returning the final message.
///
/// In CC this is an async generator. In Rust this helper currently returns a
/// `Result` with the successful message; retry heartbeat yielding remains a
/// caller-facing TODO because no Rust non-streaming fallback caller consumes it
/// yet.
///
pub async fn execute_non_streaming_request(
    model: &str,
    source: &str,
    fallback_model: Option<&str>,
    thinking_config: &ThinkingConfig,
    params_builder: impl Fn() -> anyhow::Result<serde_json::Value> + Send + Sync,
) -> anyhow::Result<serde_json::Value> {
    execute_non_streaming_request_with_initial_consecutive_529_errors(
        model,
        source,
        fallback_model,
        thinking_config,
        params_builder,
        None,
        None,
        || Ok(anthropic_sdk::RequestOptions::default()),
        None,
    )
    .await
}

async fn execute_non_streaming_request_with_initial_consecutive_529_errors(
    model: &str,
    source: &str,
    fallback_model: Option<&str>,
    thinking_config: &ThinkingConfig,
    params_builder: impl Fn() -> anyhow::Result<serde_json::Value> + Send + Sync,
    max_output_tokens_override: Option<u32>,
    initial_consecutive_529_errors: Option<u32>,
    request_options_builder: impl Fn() -> anyhow::Result<anthropic_sdk::RequestOptions> + Send + Sync,
    abort_rx: Option<tokio::sync::watch::Receiver<bool>>,
) -> anyhow::Result<serde_json::Value> {
    // Maps to: CC `executeNonStreamingRequest(...)`: create a `withRetry(...)`
    // generator, adjust params for the non-streaming fallback request on each
    // attempt, disable SDK auto-retry (`maxRetries: 0`), and return the final
    // message body. This helper still exposes a `Result` rather than yielding
    // retry heartbeat messages because no Rust caller consumes non-streaming
    // fallback progress yet; the retry/fallback semantics and error types are
    // now owned by `services/api/with_retry.rs`, matching the official API
    // boundary without moving SDK adapter logic into query deps.
    let fallback_timeout_ms = get_nonstreaming_fallback_timeout_ms();
    let (heartbeat_tx, _heartbeat_rx) = tokio::sync::mpsc::channel(8);
    let source_for_retry = source.to_string();
    let original_model = model.to_string();
    let params_builder = std::sync::Arc::new(params_builder);
    let request_options_builder = std::sync::Arc::new(request_options_builder);
    let client_slot = std::sync::Arc::new(std::sync::Mutex::new(None));
    let get_client_slot = std::sync::Arc::clone(&client_slot);
    let get_client_model = model.to_string();
    let get_client_source = source.to_string();

    let result = crate::services::api::with_retry::with_retry(
        move || {
            let client_slot = std::sync::Arc::clone(&get_client_slot);
            let model = get_client_model.clone();
            let source = get_client_source.clone();
            async move {
                let client =
                    super::client::get_anthropic_client(super::client::GetAnthropicClientOptions {
                        api_key: None,
                        max_retries: 0,
                        model: Some(model),
                        source: Some(source),
                    })
                    .await
                    .map_err(crate::services::api::with_retry::RetryableError::Other)?
                    .build()
                    .map_err(|error| {
                        crate::services::api::with_retry::RetryableError::Other(anyhow::Error::new(
                            error,
                        ))
                    })?;
                *client_slot.lock().map_err(|_| {
                    crate::services::api::with_retry::RetryableError::Other(anyhow::anyhow!(
                        "non-streaming API client slot lock poisoned"
                    ))
                })? = Some(client);
                Ok(())
            }
        },
        move |_attempt, retry_context| {
            let original_model = original_model.clone();
            let params_builder = std::sync::Arc::clone(&params_builder);
            let request_options_builder = std::sync::Arc::clone(&request_options_builder);
            let client_slot = std::sync::Arc::clone(&client_slot);
            async move {
                let client = client_slot
                    .lock()
                    .map_err(|_| {
                        crate::services::api::with_retry::RetryableError::Other(anyhow::anyhow!(
                            "non-streaming API client slot lock poisoned"
                        ))
                    })?
                    .clone()
                    .ok_or_else(|| {
                        crate::services::api::with_retry::RetryableError::Other(anyhow::anyhow!(
                            "non-streaming API client was not initialized"
                        ))
                    })?;
                let mut body = build_non_streaming_request_body(
                    &original_model,
                    || params_builder(),
                    max_output_tokens_override,
                )
                .map_err(crate::services::api::with_retry::RetryableError::Other)?;
                apply_retry_context_to_non_streaming_body(&mut body, &retry_context);
                let mut request_options = request_options_builder()
                    .map_err(crate::services::api::with_retry::RetryableError::Other)?;
                request_options.timeout =
                    Some(std::time::Duration::from_millis(fallback_timeout_ms));
                request_options.max_retries = Some(0);
                let mut params: anthropic_sdk::resources::beta::messages::BetaMessageCreateParams =
                    serde_json::from_value(body).map_err(|error| {
                        crate::services::api::with_retry::RetryableError::Other(anyhow::anyhow!(
                            "non-streaming BetaMessageCreateParams: {error}"
                        ))
                    })?;
                if let Some(header) = request_options
                    .headers
                    .as_ref()
                    .and_then(|headers| headers.get("anthropic-beta"))
                    .and_then(|value| value.as_deref())
                {
                    let betas: Vec<String> = header
                        .split(',')
                        .map(str::trim)
                        .filter(|part| !part.is_empty())
                        .map(str::to_string)
                        .collect();
                    if !betas.is_empty() {
                        params.betas = Some(betas);
                    }
                }

                let response = client
                    .beta()
                    .messages()
                    .create_with_response_and_options(&params, Some(&request_options))
                    .await
                    .map_err(sdk_api_error_to_retryable)?;
                crate::services::claude_ai_limits::extract_quota_status_from_headers(
                    &response.response.headers,
                );
                serde_json::to_value(response.data).map_err(|error| {
                    crate::services::api::with_retry::RetryableError::Other(anyhow::anyhow!(
                        "serialize BetaMessage: {error}"
                    ))
                })
            }
        },
        crate::services::api::with_retry::RetryOptions {
            max_retries: None,
            model: model.to_string(),
            fallback_model: fallback_model.map(str::to_string),
            thinking_config: thinking_config.clone(),
            fast_mode: None,
            abort_rx,
            query_source: Some(retry_query_source_from_api_source(&source_for_retry)),
            initial_consecutive_529_errors,
        },
        heartbeat_tx,
    )
    .await;

    result.map_err(with_retry_error_to_anyhow)
}

fn apply_retry_context_to_non_streaming_body(
    body: &mut serde_json::Value,
    retry_context: &crate::services::api::with_retry::RetryContext,
) {
    // Maps to CC `executeNonStreamingRequest(...)` `paramsFromContext(context)`:
    // retry attempts can swap the model and shrink `max_tokens` after a context
    // overflow before `adjustParamsForNonStreaming(...)` is applied.
    if let Some(object) = body.as_object_mut() {
        object.insert(
            "model".to_string(),
            serde_json::Value::String(retry_context.model.clone()),
        );
        if let Some(max_tokens_override) = retry_context.max_tokens_override {
            let capped = max_tokens_override.min(MAX_NON_STREAMING_TOKENS as u64);
            object.insert(
                "max_tokens".to_string(),
                serde_json::Value::Number(serde_json::Number::from(capped)),
            );
            if let ThinkingConfig::Enabled { budget_tokens } = &retry_context.thinking_config {
                let adjusted_budget = budget_tokens
                    .unwrap_or(capped.saturating_sub(1) as i64)
                    .min(capped.saturating_sub(1) as i64)
                    .max(1);
                object.insert(
                    "thinking".to_string(),
                    serde_json::json!({ "type": "enabled", "budget_tokens": adjusted_budget }),
                );
            }
        }
    }
}

fn sdk_api_error_to_retryable(
    error: anthropic_sdk::ApiError,
) -> crate::services::api::with_retry::RetryableError {
    let quota_status = error.status();
    let quota_headers = error.headers().cloned();
    crate::services::claude_ai_limits::extract_quota_status_from_error(
        quota_status,
        quota_headers.as_ref(),
    );

    // Maps to CC `withRetry(...)` inspecting SDK `APIError` / connection / abort
    // classes. The local SDK models those classes as enum variants, so preserve
    // status, headers, and body for retry predicates such as 529 fallback and
    // context-overflow max-token reduction.
    match error {
        anthropic_sdk::ApiError::Connection { message, .. }
        | anthropic_sdk::ApiError::ConnectionTimeout { message } => {
            crate::services::api::with_retry::RetryableError::Connection(
                crate::services::api::with_retry::ConnectionError {
                    message,
                    code: None,
                },
            )
        }
        anthropic_sdk::ApiError::UserAbort { .. } => {
            crate::services::api::with_retry::RetryableError::Aborted
        }
        anthropic_sdk::ApiError::Sdk(message) => {
            crate::services::api::with_retry::RetryableError::Other(anyhow::anyhow!(message))
        }
        error => crate::services::api::with_retry::RetryableError::Api(
            crate::services::api::with_retry::ApiError {
                status: error.status(),
                message: error.to_string(),
                headers: error.headers().cloned().unwrap_or_default(),
                body: error.body().cloned(),
            },
        ),
    }
}

fn with_retry_error_to_anyhow(
    error: crate::services::api::with_retry::WithRetryError,
) -> anyhow::Error {
    match error {
        crate::services::api::with_retry::WithRetryError::CannotRetry(error) => {
            anyhow::Error::new(error)
        }
        crate::services::api::with_retry::WithRetryError::FallbackTriggered(error) => {
            anyhow::Error::new(error)
        }
    }
}

fn retry_query_source_from_api_source(
    source: &str,
) -> crate::constants::query_source::RetryQuerySource {
    match source {
        "repl_main_thread" => crate::constants::query_source::RetryQuerySource::ReplMainThread,
        "sdk" => crate::constants::query_source::RetryQuerySource::Sdk,
        // Maps to CC `withRetry.ts:68-70` — the set holds `'agent:custom'`,
        // `'agent:default'` and `'agent:builtin'` verbatim, and membership is
        // `FOREGROUND_529_RETRY_SOURCES.has(querySource)`, an EXACT lookup.
        // `'agent:builtin'` therefore never matches a real
        // `agent:builtin:<agentType>` source in CC either; that string reaching
        // `Other(..)` below is the faithful outcome, not a mapping gap. These
        // used to be spelled with underscores, which matched nothing at all
        // once `getQuerySourceForAgent`'s colon strings became reachable.
        "agent:custom" => crate::constants::query_source::RetryQuerySource::AgentCustom,
        "agent:default" => crate::constants::query_source::RetryQuerySource::AgentDefault,
        "agent:builtin" => crate::constants::query_source::RetryQuerySource::AgentBuiltin,
        "compact" => crate::constants::query_source::RetryQuerySource::Compact,
        "hook_agent" => crate::constants::query_source::RetryQuerySource::HookAgent,
        "hook_prompt" => crate::constants::query_source::RetryQuerySource::HookPrompt,
        "verification_agent" => crate::constants::query_source::RetryQuerySource::VerificationAgent,
        "side_question" => crate::constants::query_source::RetryQuerySource::SideQuestion,
        "auto_mode" => crate::constants::query_source::RetryQuerySource::AutoMode,
        "bash_classifier" => crate::constants::query_source::RetryQuerySource::BashClassifier,
        other => crate::constants::query_source::RetryQuerySource::Other(other.to_string()),
    }
}

fn build_non_streaming_request_body(
    model: &str,
    params_builder: impl Fn() -> anyhow::Result<serde_json::Value>,
    max_output_tokens_override: Option<u32>,
) -> anyhow::Result<serde_json::Value> {
    let params = params_builder()?;
    let params: NonStreamingParams = serde_json::from_value(params.clone()).map_err(|error| {
        anyhow::anyhow!("failed to parse non-streaming request params: {error}")
    })?;
    let max_tokens = max_output_tokens_override
        .unwrap_or(MAX_NON_STREAMING_TOKENS)
        .min(MAX_NON_STREAMING_TOKENS);
    let adjusted = adjust_params_for_non_streaming(&params, max_tokens);
    let mut body = serde_json::to_value(adjusted)?;
    let object = body
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("non-streaming request params must serialize to object"))?;
    object.insert(
        "model".to_string(),
        serde_json::Value::String(normalize_model_string_for_api(model)),
    );
    object.insert("stream".to_string(), serde_json::Value::Bool(false));
    Ok(body)
}

// ---------------------------------------------------------------------------
// queryHaiku
// ---------------------------------------------------------------------------

/// Maps to: CC services/api/claude.ts:3241-3291
///
/// Convenience wrapper for small/fast model queries (historically Haiku, now
/// may be Sonnet depending on configuration). Uses queryModelWithoutStreaming
/// internally with thinking disabled and no tools.
///
pub async fn query_haiku(
    system_prompt: &SystemPrompt,
    user_prompt: &str,
    output_format: Option<&JsonOutputFormat>,
    options: &Options,
) -> anyhow::Result<AssistantMessage> {
    let messages = single_user_prompt_messages(user_prompt);
    let single_shot_options =
        single_shot_query_options(options, Some(get_small_fast_model()), output_format);

    query_model_without_streaming(
        &messages,
        system_prompt,
        &ThinkingConfig::Disabled,
        &[],
        &single_shot_options,
    )
    .await
}

// ---------------------------------------------------------------------------
// queryWithModel
// ---------------------------------------------------------------------------

/// Maps to: CC services/api/claude.ts:3300-3348
///
/// Generic single-shot model query wrapper. Goes through the full query
/// pipeline including proper authentication, betas, and headers.
///
pub async fn query_with_model(
    system_prompt: &SystemPrompt,
    user_prompt: &str,
    output_format: Option<&JsonOutputFormat>,
    options: &Options,
) -> anyhow::Result<AssistantMessage> {
    let messages = single_user_prompt_messages(user_prompt);
    let single_shot_options = single_shot_query_options(options, None, output_format);

    query_model_without_streaming(
        &messages,
        system_prompt,
        &ThinkingConfig::Disabled,
        &[],
        &single_shot_options,
    )
    .await
}

fn single_user_prompt_messages(user_prompt: &str) -> Vec<Message> {
    // Maps to: CC `queryHaiku(...)` / `queryWithModel(...)` local
    // `messages = [createUserMessage({ content: userPrompt })]`.
    vec![Message::User(crate::types::message::UserMessage {
        uuid: uuid::Uuid::new_v4().to_string(),
        timestamp: chrono::Utc::now(),
        content: vec![crate::types::message::UserContent::Text(
            user_prompt.to_string(),
        )],
        is_compact_summary: false,
        plan_content: None,
        image_paste_ids: None,
        is_visible_in_transcript_only: false,
        mcp_meta: None,
        source_tool_assistant_uuid: None,
        permission_mode: None,
        origin: None,
        summarize_metadata: None,
    })]
}

fn single_shot_query_options(
    options: &Options,
    model_override: Option<String>,
    output_format: Option<&JsonOutputFormat>,
) -> Options {
    // Maps to: CC wrappers spreading `options`, defaulting
    // `enablePromptCaching` to false, installing `outputFormat`, and using an
    // empty permission context via no local tools. `tool_permission_context`
    // (CC `getToolPermissionContext`) rides the spread unchanged; these
    // single-shot calls pass no local tools, so the serialization-time
    // `Tool.prompt` consumer never fires here.
    let mut next = options.clone();
    if let Some(model) = model_override {
        next.model = model;
    }
    next.enable_prompt_caching = Some(options.enable_prompt_caching.unwrap_or(false));
    next.output_format = output_format.cloned();
    next
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    #[test]
    fn api_stream_and_fallback_normalize_plan_before_typed_messages_like_official() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _plan_lock = crate::utils::plans::test_plan_state_lock();
        let dir =
            std::env::temp_dir().join(format!("cometix-normalize-plan-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &dir);
        let _project = PinnedProjectDir::at(&dir);
        let _local = EnvVarGuard::set("CLAUDE_CODE_ENVIRONMENT_KIND", "");
        let tools = vec![crate::tools::exit_plan_mode_tool::exit_plan_mode_tool_schema()];
        let plan_path = crate::utils::plans::get_plan_file_path(Some("agent-fixture"));
        std::fs::write(&plan_path, "").unwrap();
        let expected = serde_json::json!({"keep":true,"plan":"","planFilePath":plan_path});
        let array = crate::utils::messages::normalize_content_from_api(&[
            serde_json::json!({"type":"tool_use","id":"array","name":"ExitPlanMode","input":["first", {"nested":true}]})
        ], &tools, Some("agent-fixture")).unwrap();
        assert_eq!(
            array[0]["input"],
            serde_json::json!({"0":"first","1":{"nested":true},"plan":"","planFilePath":plan_path})
        );
        let object = crate::utils::messages::normalize_content_from_api(&[
            serde_json::json!({"type":"tool_use","id":"object","name":"ExitPlanMode","input":{"keep":true,"plan":"stale","planFilePath":"old"}})
        ], &tools, Some("agent-fixture")).unwrap();
        assert_eq!(object[0]["input"], expected);

        let mut streamed = ActiveClaudeStreamBlock::ToolUse {
            id: "toolu-stream".into(),
            name: "ExitPlanMode".into(),
            input_json: "{\"keep\":true}".into(),
            is_server: false,
        };
        streamed
            .normalize_api_content(&tools, Some("agent-fixture"))
            .unwrap();
        let Some(ClaudeStreamItem::ToolUse { input, .. }) = streamed.into_item() else {
            panic!("missing tool input")
        };
        assert_eq!(input, expected);
        let response = serde_json::json!({
            "id":"msg-normalized", "type":"message", "role":"assistant", "model":"fixture",
            "content":[{"type":"text","text":""},{"type":"tool_use","id":"toolu-fallback","name":"ExitPlanMode","input":{"keep":true}}],
            "stop_reason":"tool_use", "stop_sequence":null,
            "usage":{"input_tokens":1,"output_tokens":1}
        });
        let assistant =
            sdk_message_response_to_assistant_message(response, &tools, Some("agent-fixture"))
                .unwrap();
        assert_eq!(
            assistant.content[0],
            crate::types::message::AssistantContent::Text(String::new())
        );
        let crate::types::message::AssistantContent::ToolUse(block) = &assistant.content[1] else {
            panic!("missing normalized fallback tool")
        };
        assert_eq!(block.input, expected);
        // CC messages.ts:2213–2220 → api.ts:685–703: the API projection
        // strips injected fields, while the execution/history and transcript
        // projection retain the plan required by later remote recovery.
        let history = vec![Message::Assistant(assistant.clone())];
        let transcript_before =
            crate::utils::session_storage::typed_messages_as_transcript_values(&history);
        let transcript_path = dir.join("normalized-plan-transcript.jsonl");
        std::fs::write(
            &transcript_path,
            format!(
                "{}\n",
                serde_json::to_string(&transcript_before[0]).unwrap()
            ),
        )
        .unwrap();
        let persisted = crate::utils::json::read_jsonl_file(&transcript_path).unwrap();
        assert_eq!(persisted[0]["message"]["content"][1]["input"], expected);
        let mut options =
            Options::new("claude-sonnet-4-20250514".into(), "repl_main_thread".into());
        options.enable_prompt_caching = Some(false);
        let api_tool = |messages: &[Message], tools: &[Tool]| {
            let params = build_sdk_message_create_params(
                messages,
                &Vec::new(),
                &ThinkingConfig::Disabled,
                tools,
                &options,
            )
            .unwrap();
            let wire = serde_json::to_value(params).unwrap();
            wire["messages"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|message| message["content"].as_array())
                .flatten()
                .find(|block| block["type"] == "tool_use")
                .expect("outbound tool use")
                .clone()
        };
        assert_eq!(
            api_tool(&history, &tools)["input"],
            serde_json::json!({"keep":true})
        );
        assert_eq!(api_tool(&history, &[])["input"], expected);
        assert_eq!(
            crate::utils::session_storage::typed_messages_as_transcript_values(&history),
            transcript_before,
        );
        assert_eq!(
            crate::utils::json::read_jsonl_file(&transcript_path).unwrap(),
            persisted
        );
        let mut named_history = history.clone();
        let Message::Assistant(named) = &mut named_history[0] else {
            unreachable!()
        };
        let crate::types::message::AssistantContent::ToolUse(named) = &mut named.content[1] else {
            unreachable!()
        };
        named.name = "Unknown".into();
        assert_eq!(api_tool(&named_history, &tools)["input"], expected);
        let Message::Assistant(named) = &mut named_history[0] else {
            unreachable!()
        };
        let crate::types::message::AssistantContent::ToolUse(named) = &mut named.content[1] else {
            unreachable!()
        };
        named.name = "ExitAlias".into();
        let mut aliased_tools = tools.clone();
        aliased_tools[0].aliases.push("ExitAlias".into());
        let alias_wire = api_tool(&named_history, &aliased_tools);
        assert_eq!(alias_wire["name"], "ExitPlanMode");
        assert_eq!(alias_wire["input"], serde_json::json!({"keep":true}));
        // Unknown / unavailable tools must not receive the plan corrections.
        let mut unknown = ActiveClaudeStreamBlock::ToolUse {
            id: "unknown".into(),
            name: "ExitPlanMode".into(),
            input_json: "{}".into(),
            is_server: false,
        };
        unknown
            .normalize_api_content(&[], Some("agent-fixture"))
            .unwrap();
        let Some(ClaudeStreamItem::ToolUse { input, .. }) = unknown.into_item() else {
            panic!("missing input")
        };
        assert_eq!(input, serde_json::json!({}));
        crate::utils::plans::clear_plans_directory_cache();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    use super::*;
    use crate::utils::test_env::{EnvVarGuard, PinnedProjectDir, TEST_ENV_LOCK};

    fn wire_test_user_message(text: &str) -> Message {
        Message::User(UserMessage {
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

    fn wire_test_mcp_tool(server: &str, name: &str) -> Tool {
        Tool {
            name: format!("mcp__{server}__{name}"),
            description: format!("{name} on {server}"),
            input_schema: serde_json::json!({"type": "object", "properties": {}}),
            is_mcp: true,
            mcp_info: Some(crate::types::tools::McpToolInfo {
                server_name: server.to_string(),
                tool_name: name.to_string(),
            }),
            ..Default::default()
        }
    }

    /// CC `services/api/claude.ts:1120-1236` builds `filteredTools` — and thus
    /// the `tools` array that goes on the wire — from the `tools` PARAMETER
    /// alone. `options.mcpTools` is declared at `:694` and has no reader
    /// anywhere in CC:
    ///
    /// ```text
    /// ast-grep --lang ts  -p 'options.mcpTools' src/ -> 0
    /// ast-grep --lang tsx -p 'options.mcpTools' src/ -> 0
    /// ast-grep --lang ts  -p 'options.hasPendingMcpServers' \
    ///     src/services/api/claude.ts -> :1142   (control: the form finds reads)
    /// ```
    ///
    /// The port used to chain `options.mcp_tools` onto `tools` here. That value
    /// is the RAW `appState.mcp.tools` (`query.ts:689`) — it never passed
    /// through `assembleToolPool`'s deny filter (`tools.ts:352`) — so the chain
    /// (a) put deny-ruled MCP tools back in front of the model after the pool
    /// had removed them and (b) sent a second copy of every MCP tool the pool
    /// had already included. Nothing downstream de-duplicates: `filtered_tools`
    /// maps 1:1 into `params.tools`, which is what gets serialized.
    #[test]
    fn request_tools_come_only_from_the_tools_parameter_never_from_options_mcp_tools() {
        // What `assemble_tool_pool` hands the query loop: `mcp__untrusted__*`
        // has already been deny-filtered out.
        let assembled_pool = vec![
            Tool {
                name: "Bash".to_string(),
                description: "run a command".to_string(),
                input_schema: serde_json::json!({"type": "object", "properties": {}}),
                ..Default::default()
            },
            wire_test_mcp_tool("trusted", "read_doc"),
        ];
        let mut options = Options::new(
            "claude-sonnet-4-20250514".to_string(),
            "repl_main_thread".to_string(),
        );
        // What `build_call_model_request` writes (CC `query.ts:689`
        // `mcpTools: appState.mcp.tools`): the raw, unfiltered MCP set.
        options.mcp_tools = vec![
            wire_test_mcp_tool("trusted", "read_doc"),
            wire_test_mcp_tool("untrusted", "read_secret"),
        ];

        let params = build_sdk_message_create_params(
            &[wire_test_user_message("hello")],
            &Vec::new(),
            &ThinkingConfig::Disabled,
            &assembled_pool,
            &options,
        )
        .expect("SDK request params");

        let body = serde_json::to_value(&params).expect("params serialize");
        let names = body["tools"]
            .as_array()
            .map(|tools| {
                tools
                    .iter()
                    .map(|tool| tool["name"].as_str().unwrap_or_default().to_string())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        assert_eq!(
            names,
            vec!["Bash".to_string(), "mcp__trusted__read_doc".to_string()],
            "the request tool list must be exactly the assembled pool: no \
             deny-filtered MCP tool may re-enter through options.mcp_tools, and \
             no tool may be sent twice"
        );
    }

    #[test]
    fn meta_text_maps_to_anthropic_text_block_without_leaking_local_discriminant() {
        let message = UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::UserContent::MetaText(
                "internal init prompt".to_string(),
            )],
            is_compact_summary: false,
            plan_content: None,
            image_paste_ids: None,
            is_visible_in_transcript_only: false,
            mcp_meta: None,
            source_tool_assistant_uuid: None,
            permission_mode: None,
            origin: None,
            summarize_metadata: None,
        };
        let param = user_message_to_message_param(&message, false, false, None);
        let MessageContent::Blocks(blocks) = param.content else {
            panic!("expected content blocks");
        };
        assert_eq!(
            blocks,
            vec![serde_json::json!({"type":"text","text":"internal init prompt"})]
        );
    }

    #[test]
    fn assistant_identity_is_out_of_band_and_previous_request_id_uses_latest_envelope() {
        let timestamp = chrono::Utc::now();
        let identity = crate::types::message::AssistantMessageIdentity {
            request_id: Some("req_latest".to_string()),
            api_message_id: Some("msg_latest".to_string()),
            ..Default::default()
        };
        let assistant = AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp,
            content: vec![
                crate::types::message::AssistantContent::Text("answer".to_string()),
                crate::types::message::AssistantContent::MessageIdentity(identity),
            ],
            model: Some("claude-test".to_string()),
            stop_reason: Some(crate::types::message::StopReason::EndTurn),
            usage: None,
        };

        let param = assistant_message_to_message_param(&assistant, true, true, Some("prompt"));
        let MessageContent::Blocks(blocks) = param.content else {
            panic!("assistant should use content blocks");
        };
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], "text");
        assert_eq!(blocks[0]["text"], "answer");
        assert_eq!(blocks[0]["cache_control"]["type"], "ephemeral");

        let messages = vec![
            Message::Assistant(AssistantMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp,
                content: vec![crate::types::message::AssistantContent::MessageIdentity(
                    crate::types::message::AssistantMessageIdentity {
                        request_id: Some("req_older".to_string()),
                        api_message_id: None,
                        ..Default::default()
                    },
                )],
                model: None,
                stop_reason: None,
                usage: None,
            }),
            Message::Assistant(assistant),
        ];
        assert_eq!(
            get_previous_request_id_from_messages(&messages).as_deref(),
            Some("req_latest")
        );
    }

    #[test]
    fn content_block_stop_creates_distinct_typed_envelopes_with_shared_api_identity() {
        let first = assistant_message_for_completed_block(
            crate::types::message::AssistantContent::Thinking {
                text: "reasoning".to_string(),
                signature: "sig".to_string(),
            },
            Some("claude-test".to_string()),
            Some("req-shared".to_string()),
            Some("msg-shared".to_string()),
            None,
            Some(crate::types::message::TokenUsage::default()),
        );
        let second = assistant_message_for_completed_block(
            crate::types::message::AssistantContent::Text("answer".to_string()),
            Some("claude-test".to_string()),
            Some("req-shared".to_string()),
            Some("msg-shared".to_string()),
            None,
            Some(crate::types::message::TokenUsage::default()),
        );

        assert_ne!(first.uuid, second.uuid);
        assert_eq!(first.request_id(), Some("req-shared"));
        assert_eq!(second.request_id(), Some("req-shared"));
        assert_eq!(first.api_message_id(), Some("msg-shared"));
        assert_eq!(second.api_message_id(), Some("msg-shared"));
        assert_eq!(
            first
                .content
                .iter()
                .filter(|content| !matches!(
                    content,
                    crate::types::message::AssistantContent::MessageIdentity(_)
                ))
                .count(),
            1
        );
        assert_eq!(
            second
                .content
                .iter()
                .filter(|content| !matches!(
                    content,
                    crate::types::message::AssistantContent::MessageIdentity(_)
                ))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn query_model_without_streaming_collector_returns_final_assistant() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        tx.send(QueryModelStreamItem::Content(ClaudeStreamItem::Text(
            "partial".to_string(),
        )))
        .await
        .unwrap();
        tx.send(QueryModelStreamItem::Assistant(AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::AssistantContent::Text(
                "final".to_string(),
            )],
            model: Some("claude-test".to_string()),
            stop_reason: None,
            usage: None,
        }))
        .await
        .unwrap();
        tx.send(QueryModelStreamItem::AssistantDelta {
            stop_reason: Some(crate::types::message::StopReason::EndTurn),
            usage: Some(crate::types::message::TokenUsage {
                output_tokens: 3,
                ..Default::default()
            }),
        })
        .await
        .unwrap();
        drop(tx);

        let message = collect_query_model_stream_to_final_assistant(&mut rx)
            .await
            .expect("collector should return final assistant");

        assert_eq!(message.model.as_deref(), Some("claude-test"));
        assert_eq!(
            message.stop_reason,
            Some(crate::types::message::StopReason::EndTurn)
        );
        assert_eq!(
            message.usage.as_ref().map(|usage| usage.output_tokens),
            Some(3)
        );
        assert!(matches!(
            message.content.first(),
            Some(crate::types::message::AssistantContent::Text(text)) if text == "final"
        ));
    }

    #[tokio::test]
    async fn query_model_without_streaming_collector_surfaces_system_error_without_final() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        tx.send(QueryModelStreamItem::SystemError(SystemApiErrorMessage {
            content: "API Error".to_string(),
            api_error: "invalid_request".to_string(),
            error: "invalid_request".to_string(),
            error_details: Some("raw details".to_string()),
        }))
        .await
        .unwrap();
        drop(tx);

        let error = collect_query_model_stream_to_final_assistant(&mut rx)
            .await
            .expect_err("collector should return system error");

        assert!(error.to_string().contains("API Error"));
        assert!(error.to_string().contains("raw details"));
    }

    #[test]
    fn add_cache_breakpoints_cached_mc_inserts_cache_edits_and_references_like_official() {
        let user = crate::types::message::UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::UserContent::ToolResult(
                crate::types::message::ToolResult {
                    tool_use_id: crate::types::ids::ToolUseId("tool-1".to_string()),
                    content: "tool output".to_string(),
                    is_error: false,
                    content_blocks: Vec::new(),
                    tool_use_result: None,
                },
            )],
            is_compact_summary: false,
            plan_content: None,
            image_paste_ids: None,
            is_visible_in_transcript_only: false,
            mcp_meta: None,
            source_tool_assistant_uuid: None,
            permission_mode: None,
            origin: None,
            summarize_metadata: None,
        };
        let assistant = AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::AssistantContent::Text(
                "final prompt marker".to_string(),
            )],
            model: None,
            stop_reason: None,
            usage: None,
        };
        let cache_edits = CachedMcEditsBlock::delete_refs(["tool-1"]);
        let refs = vec![MessageRef::User(&user), MessageRef::Assistant(&assistant)];

        let result = add_cache_breakpoints(
            &refs,
            true,
            Some("prompt"),
            true,
            Some(&cache_edits),
            &[],
            false,
        );

        let MessageContent::Blocks(user_blocks) = &result[0].content else {
            panic!("user message should use array content");
        };
        assert_eq!(user_blocks[0]["type"], "tool_result");
        assert_eq!(user_blocks[0]["cache_reference"], "tool-1");
        assert_eq!(user_blocks[1]["type"], "cache_edits");
        assert_eq!(user_blocks[1]["edits"][0]["cache_reference"], "tool-1");
        assert_eq!(
            user_blocks[2],
            serde_json::json!({ "type": "text", "text": "." })
        );
        let (sdk_compatible, changed) = strip_private_cache_extensions_for_sdk(&result);
        assert!(changed);
        local_message_param_to_sdk(&sdk_compatible[0])
            .expect("adapter must strip private fields before stable SDK conversion");
        let MessageContent::Blocks(sdk_blocks) = &sdk_compatible[0].content else {
            panic!("SDK-compatible message should use array content");
        };
        assert!(sdk_blocks[0].get("cache_reference").is_none());
        assert!(
            !sdk_blocks
                .iter()
                .any(|block| block.get("type").and_then(JsonValue::as_str) == Some("cache_edits"))
        );

        let MessageContent::Blocks(assistant_blocks) = &result[1].content else {
            panic!("assistant message should use array content");
        };
        assert!(assistant_blocks[0].get("cache_control").is_some());
    }

    #[test]
    fn sdk_request_adapter_overlays_private_cached_mc_blocks_without_widening_sdk_types() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("CLAUDE_CODE_USE_BEDROCK");
        crate::utils::process_env::remove("CLAUDE_CODE_USE_VERTEX");
        crate::utils::process_env::remove("CLAUDE_CODE_USE_FOUNDRY");
        let messages = vec![
            Message::User(UserMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![crate::types::message::UserContent::Text(
                    "use the tool".to_string(),
                )],
                is_compact_summary: false,
                plan_content: None,
                image_paste_ids: None,
                is_visible_in_transcript_only: false,
                mcp_meta: None,
                source_tool_assistant_uuid: None,
                permission_mode: None,
                origin: None,
                summarize_metadata: None,
            }),
            Message::Assistant(AssistantMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![crate::types::message::AssistantContent::ToolUse(
                    crate::types::message::ToolUseBlock {
                        id: crate::types::ids::ToolUseId("tool-1".to_string()),
                        name: "Example".to_string(),
                        input: serde_json::json!({}),
                    },
                )],
                model: None,
                stop_reason: Some(crate::types::message::StopReason::ToolUse),
                usage: None,
            }),
            Message::User(UserMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![crate::types::message::UserContent::ToolResult(
                    crate::types::message::ToolResult {
                        tool_use_id: crate::types::ids::ToolUseId("tool-1".to_string()),
                        content: "tool output".to_string(),
                        is_error: false,
                        content_blocks: Vec::new(),
                        tool_use_result: None,
                    },
                )],
                is_compact_summary: false,
                plan_content: None,
                image_paste_ids: None,
                is_visible_in_transcript_only: false,
                mcp_meta: None,
                source_tool_assistant_uuid: None,
                permission_mode: None,
                origin: None,
                summarize_metadata: None,
            }),
            Message::Assistant(AssistantMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![crate::types::message::AssistantContent::Text(
                    "continue".to_string(),
                )],
                model: None,
                stop_reason: None,
                usage: None,
            }),
        ];
        let mut options = Options::new(
            "claude-sonnet-4-20250514".to_string(),
            "repl_main_thread".to_string(),
        );
        options.enable_prompt_caching = Some(true);
        options.cached_mc_new_cache_edits = Some(CachedMcEditsBlock::delete_refs(["tool-1"]));

        let plan = build_sdk_message_create_plan(
            &messages,
            &Vec::new(),
            &ThinkingConfig::Disabled,
            &[],
            &options,
        )
        .expect("SDK request plan");
        let typed_body = serde_json::to_value(&plan.params).expect("typed body serialize");
        let typed_blocks = typed_body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|message| message["content"].as_array().into_iter().flatten())
            .collect::<Vec<_>>();
        assert!(
            typed_blocks
                .iter()
                .all(|block| block.get("cache_reference").is_none())
        );
        assert!(
            !typed_blocks
                .iter()
                .any(|block| block["type"] == "cache_edits")
        );

        let effective_body =
            sdk_message_body_with_private_overrides(&plan.params, &plan.private_body_overrides)
                .expect("effective body serialize");
        let effective_blocks = effective_body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|message| message["content"].as_array().into_iter().flatten())
            .collect::<Vec<_>>();
        assert!(effective_blocks.iter().any(|block| {
            block.get("cache_reference").and_then(JsonValue::as_str) == Some("tool-1")
        }));
        assert!(
            effective_blocks
                .iter()
                .any(|block| block["type"] == "cache_edits")
        );

        let mut request_options = anthropic_sdk::RequestOptions::default();
        apply_sdk_private_body_overrides(&mut request_options, &plan.private_body_overrides);
        assert!(
            request_options
                .json_body_patches
                .iter()
                .any(|patch| matches!(
                    patch,
                    anthropic_sdk::JsonBodyPatch::Set { path, .. } if path == "messages"
                ))
        );
    }

    #[test]
    fn add_cache_breakpoints_cached_mc_deduplicates_pinned_and_new_edits() {
        let user = crate::types::message::UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::UserContent::Text(
                "hello".to_string(),
            )],
            is_compact_summary: false,
            plan_content: None,
            image_paste_ids: None,
            is_visible_in_transcript_only: false,
            mcp_meta: None,
            source_tool_assistant_uuid: None,
            permission_mode: None,
            origin: None,
            summarize_metadata: None,
        };
        let pinned = CachedMcPinnedEdit {
            user_message_index: 0,
            block: CachedMcEditsBlock::delete_refs(["tool-1"]),
        };
        let new_edits = CachedMcEditsBlock::delete_refs(["tool-1", "tool-2"]);
        let refs = vec![MessageRef::User(&user)];

        let result =
            add_cache_breakpoints(&refs, false, None, true, Some(&new_edits), &[pinned], false);

        let MessageContent::Blocks(user_blocks) = &result[0].content else {
            panic!("user message should use array content");
        };
        assert_eq!(user_blocks[0]["type"], "cache_edits");
        assert_eq!(user_blocks[0]["edits"].as_array().unwrap().len(), 1);
        assert_eq!(user_blocks[0]["edits"][0]["cache_reference"], "tool-1");
        assert_eq!(user_blocks[1]["type"], "cache_edits");
        assert_eq!(user_blocks[1]["edits"].as_array().unwrap().len(), 1);
        assert_eq!(user_blocks[1]["edits"][0]["cache_reference"], "tool-2");
        assert_eq!(user_blocks[2]["type"], "text");
    }

    #[test]
    fn cached_mc_body_blocks_are_first_party_repl_only_like_official() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("CLAUDE_CODE_USE_BEDROCK");
        crate::utils::process_env::remove("CLAUDE_CODE_USE_VERTEX");
        crate::utils::process_env::remove("CLAUDE_CODE_USE_FOUNDRY");

        let mut options = Options::new(
            "claude-sonnet-4-20250514".to_string(),
            "repl_main_thread".to_string(),
        );
        options.cached_mc_new_cache_edits = Some(CachedMcEditsBlock::delete_refs(["tool-1"]));
        assert!(should_use_cached_mc_for_request(&options));

        options.query_source = "sdk".to_string();
        assert!(!should_use_cached_mc_for_request(&options));

        options.query_source = "repl_main_thread".to_string();
        crate::utils::process_env::set("CLAUDE_CODE_USE_BEDROCK", "1");
        assert!(!should_use_cached_mc_for_request(&options));
        crate::utils::process_env::remove("CLAUDE_CODE_USE_BEDROCK");
    }

    // -- Direct API tool schema adapter --

    #[test]
    fn production_thinking_config_matches_official_env_and_settings_precedence() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("MAX_THINKING_TOKENS");
        crate::utils::process_env::remove("CLAUDE_CODE_THINKING");
        crate::utils::process_env::remove("COMETIX_THINKING");

        let mut settings = crate::utils::settings::types::SettingsJson::default();
        assert!(matches!(
            production_thinking_config_from_env_and_settings(&settings),
            ThinkingConfig::Adaptive
        ));

        // CC uses settings.alwaysThinkingEnabled, not GlobalConfig.thinking_enabled.
        settings.always_thinking_enabled = Some(false);
        assert!(matches!(
            production_thinking_config_from_env_and_settings(&settings),
            ThinkingConfig::Disabled
        ));

        crate::utils::process_env::set("MAX_THINKING_TOKENS", "2048");
        assert!(matches!(
            production_thinking_config_from_env_and_settings(&settings),
            ThinkingConfig::Enabled {
                budget_tokens: Some(2048)
            }
        ));
        crate::utils::process_env::set("MAX_THINKING_TOKENS", "0");
        assert!(matches!(
            production_thinking_config_from_env_and_settings(
                &crate::utils::settings::types::SettingsJson::default()
            ),
            ThinkingConfig::Disabled
        ));
        crate::utils::process_env::remove("MAX_THINKING_TOKENS");
    }

    #[test]
    fn adaptive_thinking_downgrades_to_budget_for_non_adaptive_sonnet4() {
        let messages = vec![Message::User(UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::UserContent::Text(
                "think".to_string(),
            )],
            is_compact_summary: false,
            plan_content: None,
            image_paste_ids: None,
            is_visible_in_transcript_only: false,
            mcp_meta: None,
            source_tool_assistant_uuid: None,
            permission_mode: None,
            origin: None,
            summarize_metadata: None,
        })];
        let mut options = Options::new(
            "claude-sonnet-4-20250514".to_string(),
            "repl_main_thread".to_string(),
        );
        options.max_output_tokens_override = Some(4096);

        let params = build_sdk_message_create_params(
            &messages,
            &Vec::new(),
            &ThinkingConfig::Adaptive,
            &[],
            &options,
        )
        .expect("SDK params build");
        let value = serde_json::to_value(params).expect("params serialize");

        assert_eq!(value["thinking"]["type"], serde_json::json!("enabled"));
        assert_eq!(value["thinking"]["budget_tokens"], serde_json::json!(4095));
    }

    #[test]
    fn sdk_params_send_default_temperature_only_when_thinking_disabled_like_official() {
        let messages = vec![Message::User(UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::UserContent::Text(
                "hello".to_string(),
            )],
            is_compact_summary: false,
            plan_content: None,
            image_paste_ids: None,
            is_visible_in_transcript_only: false,
            mcp_meta: None,
            source_tool_assistant_uuid: None,
            permission_mode: None,
            origin: None,
            summarize_metadata: None,
        })];
        let options = Options::new(
            "claude-sonnet-4-20250514".to_string(),
            "repl_main_thread".to_string(),
        );

        let disabled = build_sdk_message_create_params(
            &messages,
            &Vec::new(),
            &ThinkingConfig::Disabled,
            &[],
            &options,
        )
        .expect("SDK params build");
        let disabled_value = serde_json::to_value(disabled).expect("params serialize");
        assert_eq!(disabled_value["temperature"], serde_json::json!(1.0));

        let enabled = build_sdk_message_create_params(
            &messages,
            &Vec::new(),
            &ThinkingConfig::Enabled {
                budget_tokens: Some(1024),
            },
            &[],
            &options,
        )
        .expect("SDK params build");
        let enabled_value = serde_json::to_value(enabled).expect("params serialize");
        assert!(enabled_value.get("temperature").is_none());
    }

    #[test]
    fn sdk_params_use_system_prompt_blocks_with_cache_control_like_official() {
        let messages = vec![Message::User(UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::UserContent::Text(
                "hello".to_string(),
            )],
            is_compact_summary: false,
            plan_content: None,
            image_paste_ids: None,
            is_visible_in_transcript_only: false,
            mcp_meta: None,
            source_tool_assistant_uuid: None,
            permission_mode: None,
            origin: None,
            summarize_metadata: None,
        })];
        let mut options = Options::new(
            "claude-sonnet-4-20250514".to_string(),
            "repl_main_thread".to_string(),
        );
        options.enable_prompt_caching = Some(true);
        let system_prompt = vec!["System A".to_string(), "System B".to_string()];

        let params = build_sdk_message_create_params(
            &messages,
            &system_prompt,
            &ThinkingConfig::Disabled,
            &[],
            &options,
        )
        .expect("SDK params build");
        let value = serde_json::to_value(params).expect("params serialize");

        assert!(value["system"].is_array());
        assert_eq!(value["system"][0]["type"], serde_json::json!("text"));
        assert_eq!(
            value["system"][0]["text"],
            serde_json::json!("System A\n\nSystem B")
        );
        assert_eq!(
            value["system"][0]["cache_control"]["type"],
            serde_json::json!("ephemeral")
        );
    }

    #[test]
    fn sdk_request_adapter_preserves_global_system_prompt_cache_scope() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("CLAUDE_CODE_USE_GLOBAL_CACHE_SCOPE", "1");
        crate::utils::process_env::remove("ANTHROPIC_USE_GLOBAL_CACHE_SCOPE");
        let messages = vec![Message::User(UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::UserContent::Text(
                "hello".to_string(),
            )],
            is_compact_summary: false,
            plan_content: None,
            image_paste_ids: None,
            is_visible_in_transcript_only: false,
            mcp_meta: None,
            source_tool_assistant_uuid: None,
            permission_mode: None,
            origin: None,
            summarize_metadata: None,
        })];
        let mut options = Options::new(
            "claude-sonnet-4-20250514".to_string(),
            "repl_main_thread".to_string(),
        );
        options.enable_prompt_caching = Some(true);
        let system_prompt = vec![
            "static".to_string(),
            crate::constants::prompts::SYSTEM_PROMPT_DYNAMIC_BOUNDARY.to_string(),
            "dynamic".to_string(),
        ];

        let plan = build_sdk_message_create_plan(
            &messages,
            &system_prompt,
            &ThinkingConfig::Disabled,
            &[],
            &options,
        )
        .expect("SDK params build");
        let value =
            sdk_message_body_with_private_overrides(&plan.params, &plan.private_body_overrides)
                .expect("effective body serialize");

        crate::utils::process_env::remove("CLAUDE_CODE_USE_GLOBAL_CACHE_SCOPE");
        assert_eq!(value["system"][0]["text"], serde_json::json!("static"));
        assert_eq!(
            value["system"][0]["cache_control"]["scope"],
            serde_json::json!("global")
        );
        assert_eq!(value["system"][1]["text"], serde_json::json!("dynamic"));
        assert!(value["system"][1].get("cache_control").is_none());
    }

    #[test]
    fn sdk_params_skip_global_system_prompt_cache_when_mcp_tool_cache_marker_is_needed() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("CLAUDE_CODE_USE_GLOBAL_CACHE_SCOPE", "1");
        crate::utils::process_env::remove("ANTHROPIC_USE_GLOBAL_CACHE_SCOPE");
        let messages = vec![Message::User(UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::UserContent::Text(
                "hello".to_string(),
            )],
            is_compact_summary: false,
            plan_content: None,
            image_paste_ids: None,
            is_visible_in_transcript_only: false,
            mcp_meta: None,
            source_tool_assistant_uuid: None,
            permission_mode: None,
            origin: None,
            summarize_metadata: None,
        })];
        let mut options = Options::new(
            "claude-sonnet-4-20250514".to_string(),
            "repl_main_thread".to_string(),
        );
        options.enable_prompt_caching = Some(true);
        let tools = vec![crate::types::tools::Tool {
            name: "mcp__server__tool".to_string(),
            description: "MCP tool".to_string(),
            input_schema: serde_json::json!({"type":"object","properties":{}}),
            is_mcp: true,
            strict: Some(true),
            ..Default::default()
        }];

        let system_prompt = vec![
            "static".to_string(),
            crate::constants::prompts::SYSTEM_PROMPT_DYNAMIC_BOUNDARY.to_string(),
            "dynamic".to_string(),
        ];

        let params = build_sdk_message_create_params(
            &messages,
            &system_prompt,
            &ThinkingConfig::Disabled,
            &tools,
            &options,
        )
        .expect("SDK params build");
        let value = serde_json::to_value(params).expect("params serialize");

        crate::utils::process_env::remove("CLAUDE_CODE_USE_GLOBAL_CACHE_SCOPE");
        assert_eq!(
            value["system"][0]["text"],
            serde_json::json!("static\n\ndynamic")
        );
        assert_eq!(
            value["system"][0]["cache_control"]["type"],
            serde_json::json!("ephemeral")
        );
        assert!(value["system"][0]["cache_control"].get("scope").is_none());
        assert!(value["system"].as_array().unwrap().len() == 1);
    }

    #[test]
    fn assistant_thinking_replays_signature_in_sdk_params() {
        // Maps to CC `assistantMessageToMessageParam` spreading thinking blocks
        // (including signature, possibly "") into the next request.
        let messages = vec![Message::Assistant(AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::AssistantContent::Thinking {
                text: "internal reasoning".to_string(),
                signature: "sig_abc".to_string(),
            }],
            model: Some("claude-sonnet-4-20250514".to_string()),
            stop_reason: Some(crate::types::message::StopReason::EndTurn),
            usage: None,
        })];
        let options = Options::new(
            "claude-sonnet-4-20250514".to_string(),
            "repl_main_thread".to_string(),
        );

        let params = build_sdk_message_create_params(
            &messages,
            &Vec::new(),
            &ThinkingConfig::Disabled,
            &[],
            &options,
        )
        .expect("thinking with signature must convert");
        let value = serde_json::to_value(params).expect("params serialize");
        assert_eq!(
            value["messages"][0]["content"][0]["type"],
            serde_json::json!("thinking")
        );
        assert_eq!(
            value["messages"][0]["content"][0]["thinking"],
            serde_json::json!("internal reasoning")
        );
        assert_eq!(
            value["messages"][0]["content"][0]["signature"],
            serde_json::json!("sig_abc")
        );
    }

    #[test]
    fn assistant_thinking_empty_signature_still_converts() {
        // CC initializes signature to "" when signature_delta never arrives.
        let messages = vec![Message::Assistant(AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::AssistantContent::Thinking {
                text: "internal reasoning".to_string(),
                signature: String::new(),
            }],
            model: Some("claude-sonnet-4-20250514".to_string()),
            stop_reason: Some(crate::types::message::StopReason::EndTurn),
            usage: None,
        })];
        let options = Options::new(
            "claude-sonnet-4-20250514".to_string(),
            "repl_main_thread".to_string(),
        );

        let params = build_sdk_message_create_params(
            &messages,
            &Vec::new(),
            &ThinkingConfig::Disabled,
            &[],
            &options,
        )
        .expect("empty signature must still convert like CC");
        let value = serde_json::to_value(params).expect("params serialize");
        assert_eq!(
            value["messages"][0]["content"][0]["signature"],
            serde_json::json!("")
        );
    }

    #[test]
    fn assistant_redacted_thinking_preserves_data_in_sdk_params() {
        let messages = vec![Message::Assistant(AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::AssistantContent::RedactedThinking {
                data: "encrypted-thinking-payload".to_string(),
            }],
            model: Some("claude-sonnet-4-20250514".to_string()),
            stop_reason: Some(crate::types::message::StopReason::EndTurn),
            usage: None,
        })];
        let options = Options::new(
            "claude-sonnet-4-20250514".to_string(),
            "repl_main_thread".to_string(),
        );

        let params = build_sdk_message_create_params(
            &messages,
            &Vec::new(),
            &ThinkingConfig::Disabled,
            &[],
            &options,
        )
        .expect("SDK params build");
        let value = serde_json::to_value(params).expect("params serialize");
        assert_eq!(
            value["messages"][0]["content"][0]["type"],
            serde_json::json!("redacted_thinking")
        );
        assert_eq!(
            value["messages"][0]["content"][0]["data"],
            serde_json::json!("encrypted-thinking-payload")
        );
    }

    #[test]
    fn assistant_server_tool_use_preserves_content_in_sdk_params() {
        let messages = vec![Message::Assistant(AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![
                crate::types::message::AssistantContent::ServerToolUse(
                    crate::types::message::ToolUseBlock {
                        id: crate::types::ids::ToolUseId("srvu_123".to_string()),
                        name: "web_search".to_string(),
                        input: serde_json::json!({"query": "rust"}),
                    },
                ),
                crate::types::message::AssistantContent::WebSearchToolResult {
                    tool_use_id: crate::types::ids::ToolUseId("srvu_123".to_string()),
                    content: serde_json::json!([
                        {
                            "type": "web_search_result",
                            "encrypted_content": "encrypted",
                            "title": "Rust",
                            "url": "https://www.rust-lang.org"
                        }
                    ]),
                },
            ],
            model: Some("claude-sonnet-4-20250514".to_string()),
            stop_reason: Some(crate::types::message::StopReason::EndTurn),
            usage: None,
        })];
        let options = Options::new(
            "claude-sonnet-4-20250514".to_string(),
            "repl_main_thread".to_string(),
        );

        let params = build_sdk_message_create_params(
            &messages,
            &Vec::new(),
            &ThinkingConfig::Disabled,
            &[],
            &options,
        )
        .expect("SDK params build");
        let value = serde_json::to_value(params).expect("params serialize");
        assert_eq!(
            value["messages"][0]["content"][0]["type"],
            serde_json::json!("server_tool_use")
        );
        assert_eq!(
            value["messages"][0]["content"][0]["id"],
            serde_json::json!("srvu_123")
        );
        assert_eq!(
            value["messages"][0]["content"][0]["input"]["query"],
            serde_json::json!("rust")
        );
        assert_eq!(
            value["messages"][0]["content"][1]["type"],
            serde_json::json!("web_search_tool_result")
        );
        assert_eq!(
            value["messages"][0]["content"][1]["tool_use_id"],
            serde_json::json!("srvu_123")
        );
    }

    #[test]
    fn sdk_params_normalize_model_string_for_api_like_official() {
        let messages = vec![Message::User(UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::UserContent::Text(
                "hello".to_string(),
            )],
            is_compact_summary: false,
            plan_content: None,
            image_paste_ids: None,
            is_visible_in_transcript_only: false,
            mcp_meta: None,
            source_tool_assistant_uuid: None,
            permission_mode: None,
            origin: None,
            summarize_metadata: None,
        })];
        let options = Options::new(
            "claude-sonnet-4-20250514[1m]".to_string(),
            "repl_main_thread".to_string(),
        );

        let params = build_sdk_message_create_params(
            &messages,
            &Vec::new(),
            &ThinkingConfig::Disabled,
            &[],
            &options,
        )
        .expect("SDK params build");
        let value = serde_json::to_value(params).expect("params serialize");

        assert_eq!(
            value["model"],
            serde_json::json!("claude-sonnet-4-20250514")
        );
    }

    #[test]
    fn sdk_params_append_extra_and_advisor_tool_schemas_like_official() {
        let messages = vec![Message::User(UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::UserContent::Text(
                "use server tools".to_string(),
            )],
            is_compact_summary: false,
            plan_content: None,
            image_paste_ids: None,
            is_visible_in_transcript_only: false,
            mcp_meta: None,
            source_tool_assistant_uuid: None,
            permission_mode: None,
            origin: None,
            summarize_metadata: None,
        })];
        let mut options = Options::new(
            "claude-sonnet-4-20250514".to_string(),
            "repl_main_thread".to_string(),
        );
        options.extra_tool_schemas = Some(vec![serde_json::json!({
            "type": "experimental_server_tool_20260701",
            "name": "experimental",
            "model": "claude-sonnet-4-20250514"
        })]);
        options.advisor_model = Some("claude-opus-4-20250514".to_string());

        let params = build_sdk_message_create_params(
            &messages,
            &Vec::new(),
            &ThinkingConfig::Disabled,
            &[],
            &options,
        )
        .expect("SDK params build");
        let value = serde_json::to_value(params).expect("params serialize");
        let api_tools = value["tools"].as_array().expect("tools should serialize");

        assert!(api_tools.iter().any(|tool| {
            tool.get("name").and_then(|value| value.as_str()) == Some("experimental")
                && tool.get("type").and_then(|value| value.as_str())
                    == Some("experimental_server_tool_20260701")
        }));
        assert!(api_tools.iter().any(|tool| {
            tool.get("name").and_then(|value| value.as_str()) == Some("advisor")
                && tool.get("type").and_then(|value| value.as_str()) == Some("advisor_20260301")
                && tool.get("model").and_then(|value| value.as_str())
                    == Some("claude-opus-4-20250514")
        }));
    }

    #[test]
    fn sdk_params_preserve_tool_choice_like_official() {
        let messages = vec![Message::User(UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::UserContent::Text(
                "use bash".to_string(),
            )],
            is_compact_summary: false,
            plan_content: None,
            image_paste_ids: None,
            is_visible_in_transcript_only: false,
            mcp_meta: None,
            source_tool_assistant_uuid: None,
            permission_mode: None,
            origin: None,
            summarize_metadata: None,
        })];
        let mut options = Options::new(
            "claude-sonnet-4-20250514".to_string(),
            "repl_main_thread".to_string(),
        );
        options.tool_choice = Some(serde_json::json!({
            "type": "tool",
            "name": "Bash",
            "disable_parallel_tool_use": true
        }));

        let params = build_sdk_message_create_params(
            &messages,
            &Vec::new(),
            &ThinkingConfig::Disabled,
            &[],
            &options,
        )
        .expect("SDK params build");
        let value = serde_json::to_value(params).expect("params serialize");

        assert_eq!(value["tool_choice"]["type"], serde_json::json!("tool"));
        assert_eq!(value["tool_choice"]["name"], serde_json::json!("Bash"));
        assert_eq!(
            value["tool_choice"]["disable_parallel_tool_use"],
            serde_json::json!(true)
        );
    }

    #[test]
    fn sdk_params_add_cache_breakpoint_to_last_message_like_official() {
        let timestamp = chrono::Utc::now();
        let messages = vec![
            Message::User(UserMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp,
                content: vec![crate::types::message::UserContent::Text("one".to_string())],
                is_compact_summary: false,
                plan_content: None,
                image_paste_ids: None,
                is_visible_in_transcript_only: false,
                mcp_meta: None,
                source_tool_assistant_uuid: None,
                permission_mode: None,
                origin: None,
                summarize_metadata: None,
            }),
            Message::Assistant(AssistantMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp,
                content: vec![crate::types::message::AssistantContent::Text(
                    "two".to_string(),
                )],
                model: Some("claude-sonnet-4-20250514".to_string()),
                stop_reason: Some(crate::types::message::StopReason::EndTurn),
                usage: None,
            }),
            Message::User(UserMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp,
                content: vec![crate::types::message::UserContent::Text(
                    "three".to_string(),
                )],
                is_compact_summary: false,
                plan_content: None,
                image_paste_ids: None,
                is_visible_in_transcript_only: false,
                mcp_meta: None,
                source_tool_assistant_uuid: None,
                permission_mode: None,
                origin: None,
                summarize_metadata: None,
            }),
        ];
        let mut options = Options::new(
            "claude-sonnet-4-20250514".to_string(),
            "repl_main_thread".to_string(),
        );
        options.enable_prompt_caching = Some(true);
        options.skip_cache_write = Some(false);

        let params = build_sdk_message_create_params(
            &messages,
            &Vec::new(),
            &ThinkingConfig::Disabled,
            &[],
            &options,
        )
        .expect("SDK params build");
        let value = serde_json::to_value(params).expect("params serialize");

        assert!(
            value["messages"][0]["content"][0]
                .get("cache_control")
                .is_none()
        );
        assert!(
            value["messages"][1]["content"][0]
                .get("cache_control")
                .is_none()
        );
        assert_eq!(
            value["messages"][2]["content"][0]["cache_control"]["type"],
            serde_json::json!("ephemeral")
        );
    }

    #[test]
    fn sdk_params_skip_cache_write_moves_breakpoint_to_second_last_message_like_official() {
        let timestamp = chrono::Utc::now();
        let messages = vec![
            Message::User(UserMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp,
                content: vec![crate::types::message::UserContent::Text("one".to_string())],
                is_compact_summary: false,
                plan_content: None,
                image_paste_ids: None,
                is_visible_in_transcript_only: false,
                mcp_meta: None,
                source_tool_assistant_uuid: None,
                permission_mode: None,
                origin: None,
                summarize_metadata: None,
            }),
            Message::Assistant(AssistantMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp,
                content: vec![crate::types::message::AssistantContent::Text(
                    "two".to_string(),
                )],
                model: Some("claude-sonnet-4-20250514".to_string()),
                stop_reason: Some(crate::types::message::StopReason::EndTurn),
                usage: None,
            }),
            Message::User(UserMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp,
                content: vec![crate::types::message::UserContent::Text(
                    "three".to_string(),
                )],
                is_compact_summary: false,
                plan_content: None,
                image_paste_ids: None,
                is_visible_in_transcript_only: false,
                mcp_meta: None,
                source_tool_assistant_uuid: None,
                permission_mode: None,
                origin: None,
                summarize_metadata: None,
            }),
        ];
        let mut options = Options::new(
            "claude-sonnet-4-20250514".to_string(),
            "repl_main_thread".to_string(),
        );
        options.enable_prompt_caching = Some(true);
        options.skip_cache_write = Some(true);

        let params = build_sdk_message_create_params(
            &messages,
            &Vec::new(),
            &ThinkingConfig::Disabled,
            &[],
            &options,
        )
        .expect("SDK params build");
        let value = serde_json::to_value(params).expect("params serialize");

        assert!(
            value["messages"][0]["content"][0]
                .get("cache_control")
                .is_none()
        );
        assert_eq!(
            value["messages"][1]["content"][0]["cache_control"]["type"],
            serde_json::json!("ephemeral")
        );
        assert!(
            value["messages"][2]["content"][0]
                .get("cache_control")
                .is_none()
        );
    }

    #[test]
    fn sdk_params_normalize_adjacent_roles_before_request_like_official() {
        let messages = vec![
            Message::User(crate::types::message::UserMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![crate::types::message::UserContent::Text("one".to_string())],
                is_compact_summary: false,
                plan_content: None,
                image_paste_ids: None,
                is_visible_in_transcript_only: false,
                mcp_meta: None,
                source_tool_assistant_uuid: None,
                permission_mode: None,
                origin: None,
                summarize_metadata: None,
            }),
            Message::User(crate::types::message::UserMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![crate::types::message::UserContent::Text("two".to_string())],
                is_compact_summary: false,
                plan_content: None,
                image_paste_ids: None,
                is_visible_in_transcript_only: false,
                mcp_meta: None,
                source_tool_assistant_uuid: None,
                permission_mode: None,
                origin: None,
                summarize_metadata: None,
            }),
        ];
        let options = Options::new(
            "claude-sonnet-4-20250514".to_string(),
            "repl_main_thread".to_string(),
        );

        let params = build_sdk_message_create_params(
            &messages,
            &Vec::new(),
            &ThinkingConfig::Disabled,
            &[],
            &options,
        )
        .expect("SDK params build");
        let value = serde_json::to_value(params).expect("params serialize");

        // CC messages.ts:2505-2518 appends a newline to the A text block.
        // The old assertion encoded the missing joinTextAtSeam call.
        assert_eq!(value["messages"].as_array().unwrap().len(), 1);
        assert_eq!(
            value["messages"][0]["content"][0]["text"],
            serde_json::json!("one\n")
        );
        assert_eq!(
            value["messages"][0]["content"][1]["text"],
            serde_json::json!("two")
        );
    }

    #[test]
    fn sdk_params_strip_tool_search_caller_field_like_official() {
        let messages = vec![Message::Assistant(AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::AssistantContent::ToolUse(
                crate::types::message::ToolUseBlock {
                    id: crate::types::ids::ToolUseId("toolu_caller".to_string()),
                    name: "Bash".to_string(),
                    input: serde_json::json!({
                        "command": "echo hi",
                        "caller": "ToolSearch"
                    }),
                },
            )],
            model: Some("claude-sonnet-4-20250514".to_string()),
            stop_reason: Some(crate::types::message::StopReason::ToolUse),
            usage: None,
        })];
        let options = Options::new(
            "claude-sonnet-4-20250514".to_string(),
            "repl_main_thread".to_string(),
        );

        let params = build_sdk_message_create_params(
            &messages,
            &Vec::new(),
            &ThinkingConfig::Disabled,
            &[],
            &options,
        )
        .expect("SDK params build");
        let value = serde_json::to_value(params).expect("params serialize");

        assert_eq!(
            value["messages"][0]["content"][0]["input"]["command"],
            serde_json::json!("echo hi")
        );
        assert!(
            value["messages"][0]["content"][0]["input"]
                .get("caller")
                .is_none()
        );
    }

    #[test]
    fn sdk_params_strip_advisor_blocks_like_official_without_beta() {
        let messages = vec![Message::Assistant(AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::AssistantContent::Advisor {
                tool_use_id: crate::types::ids::ToolUseId("toolu_advisor".to_string()),
                content: crate::types::message::AdvisorResult::Result {
                    text: "private advisor output".to_string(),
                },
            }],
            model: Some("claude-sonnet-4-20250514".to_string()),
            stop_reason: Some(crate::types::message::StopReason::EndTurn),
            usage: None,
        })];
        let options = Options::new(
            "claude-sonnet-4-20250514".to_string(),
            "repl_main_thread".to_string(),
        );

        let params = build_sdk_message_create_params(
            &messages,
            &Vec::new(),
            &ThinkingConfig::Disabled,
            &[],
            &options,
        )
        .expect("SDK params build");
        let value = serde_json::to_value(params).expect("params serialize");

        assert_eq!(
            value["messages"][0]["content"][0]["text"],
            serde_json::json!("[Advisor response]")
        );
        assert_ne!(
            value["messages"][0]["content"][0]["text"],
            serde_json::json!("private advisor output")
        );
    }

    #[test]
    fn sdk_params_preserve_advisor_blocks_when_advisor_beta_is_sent() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("ANTHROPIC_BETAS");
        let messages = vec![Message::Assistant(AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::AssistantContent::Advisor {
                tool_use_id: crate::types::ids::ToolUseId("toolu_advisor".to_string()),
                content: crate::types::message::AdvisorResult::Result {
                    text: "private advisor output".to_string(),
                },
            }],
            model: Some("claude-sonnet-4-20250514".to_string()),
            stop_reason: Some(crate::types::message::StopReason::EndTurn),
            usage: None,
        })];
        let mut options = Options::new(
            "claude-sonnet-4-20250514".to_string(),
            "repl_main_thread".to_string(),
        );
        options.advisor_model = Some("claude-opus-4-20250514".to_string());

        let params = build_sdk_message_create_params(
            &messages,
            &Vec::new(),
            &ThinkingConfig::Disabled,
            &[],
            &options,
        )
        .expect("SDK params build");
        let value = serde_json::to_value(params).expect("params serialize");

        // Pins the CURRENT deviation, not CC's behaviour. CC ships the block
        // whole under the beta header (claude.ts:1303-1306); Cometix flattens
        // it because `anthropic_sdk::ContentBlockParam` has no
        // `advisor_tool_result` variant. The test previously carried a name
        // promising the block was preserved while asserting `["text"]` — an
        // earlier attempt at the real fix read that failure as evidence the
        // fix was wrong. See #25.
        assert_eq!(
            value["messages"][0]["content"][0]["text"],
            serde_json::json!("private advisor output")
        );
    }

    /// The cost of that flattening, pinned so it is visible: a redacted result
    /// has no text, so it reaches the API as an EMPTY block — the model loses
    /// both the payload and the fact that one was withheld. CC never produces
    /// this shape. Flip this assertion when #25 lands.
    #[test]
    fn sdk_params_flatten_redacted_advisor_to_an_empty_block_pending_25() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("ANTHROPIC_BETAS");
        let messages = vec![Message::Assistant(AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::AssistantContent::Advisor {
                tool_use_id: crate::types::ids::ToolUseId("toolu_advisor".to_string()),
                content: crate::types::message::AdvisorResult::Redacted {
                    encrypted_content: "AAAA".to_string(),
                },
            }],
            model: Some("claude-sonnet-4-20250514".to_string()),
            stop_reason: Some(crate::types::message::StopReason::EndTurn),
            usage: None,
        })];
        let mut options = Options::new(
            "claude-sonnet-4-20250514".to_string(),
            "repl_main_thread".to_string(),
        );
        options.advisor_model = Some("claude-opus-4-20250514".to_string());

        let params = build_sdk_message_create_params(
            &messages,
            &Vec::new(),
            &ThinkingConfig::Disabled,
            &[],
            &options,
        )
        .expect("SDK params build");
        let value = serde_json::to_value(params).expect("params serialize");

        assert_eq!(
            value["messages"][0]["content"][0]["text"],
            serde_json::json!(""),
            "the encrypted payload is dropped entirely"
        );
    }

    #[test]
    fn sdk_params_repair_missing_tool_result_pairing_like_official() {
        let messages = vec![
            Message::User(crate::types::message::UserMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![crate::types::message::UserContent::Text(
                    "run it".to_string(),
                )],
                is_compact_summary: false,
                plan_content: None,
                image_paste_ids: None,
                is_visible_in_transcript_only: false,
                mcp_meta: None,
                source_tool_assistant_uuid: None,
                permission_mode: None,
                origin: None,
                summarize_metadata: None,
            }),
            Message::Assistant(AssistantMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![crate::types::message::AssistantContent::ToolUse(
                    crate::types::message::ToolUseBlock {
                        id: crate::types::ids::ToolUseId("toolu_missing".to_string()),
                        name: "Bash".to_string(),
                        input: serde_json::json!({"command": "echo hi"}),
                    },
                )],
                model: Some("claude-sonnet-4-20250514".to_string()),
                stop_reason: Some(crate::types::message::StopReason::ToolUse),
                usage: None,
            }),
        ];
        let options = Options::new(
            "claude-sonnet-4-20250514".to_string(),
            "repl_main_thread".to_string(),
        );

        let params = build_sdk_message_create_params(
            &messages,
            &Vec::new(),
            &ThinkingConfig::Disabled,
            &[],
            &options,
        )
        .expect("SDK params build");
        let value = serde_json::to_value(params).expect("params serialize");

        assert_eq!(value["messages"].as_array().unwrap().len(), 3);
        assert_eq!(
            value["messages"][2]["content"][0]["type"],
            serde_json::json!("tool_result")
        );
        assert_eq!(
            value["messages"][2]["content"][0]["tool_use_id"],
            serde_json::json!("toolu_missing")
        );
        assert_eq!(
            value["messages"][2]["content"][0]["content"],
            serde_json::json!(crate::utils::messages::SYNTHETIC_TOOL_RESULT_PLACEHOLDER)
        );
        assert_eq!(
            value["messages"][2]["content"][0]["is_error"],
            serde_json::json!(true)
        );
    }

    #[test]
    fn sdk_params_preserve_tool_search_tool_reference_result_blocks_like_official() {
        let messages = vec![
            Message::Assistant(AssistantMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![crate::types::message::AssistantContent::ToolUse(
                    crate::types::message::ToolUseBlock {
                        id: crate::types::ids::ToolUseId("toolu_tool_search".to_string()),
                        name: "ToolSearch".to_string(),
                        input: serde_json::json!({"query": "select:Read"}),
                    },
                )],
                model: Some("claude-sonnet-4-20250514".to_string()),
                stop_reason: Some(crate::types::message::StopReason::ToolUse),
                usage: None,
            }),
            Message::User(crate::types::message::UserMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![crate::types::message::UserContent::ToolResult(
                    crate::types::message::ToolResult {
                        tool_use_id: crate::types::ids::ToolUseId("toolu_tool_search".to_string()),
                        content: serde_json::json!({"matches": ["Read"]}).to_string(),
                        is_error: false,
                        content_blocks: vec![
                            crate::types::message::ToolResultContentBlock::ToolReference {
                                tool_name: "Read".to_string(),
                            },
                        ],
                        tool_use_result: None,
                    },
                )],
                is_compact_summary: false,
                plan_content: None,
                image_paste_ids: None,
                is_visible_in_transcript_only: false,
                mcp_meta: None,
                source_tool_assistant_uuid: None,
                permission_mode: None,
                origin: None,
                summarize_metadata: None,
            }),
        ];
        let options = Options::new(
            "claude-sonnet-4-20250514".to_string(),
            "repl_main_thread".to_string(),
        );

        let params = build_sdk_message_create_params(
            &messages,
            &Vec::new(),
            &ThinkingConfig::Disabled,
            &[],
            &options,
        )
        .expect("SDK params build");
        let value = serde_json::to_value(params).expect("params serialize");

        assert_eq!(
            value["messages"][1]["content"][0]["content"],
            serde_json::json!([{ "type": "tool_reference", "tool_name": "Read" }])
        );
    }

    #[test]
    fn sdk_params_strip_excess_media_items_before_request_like_official() {
        let images = (0..(API_MAX_MEDIA_PER_REQUEST + 2))
            .map(|_| crate::types::message::UserContent::Image {
                media_type: "image/png".to_string(),
                data: "AA==".to_string(),
            })
            .collect::<Vec<_>>();
        let messages = vec![Message::User(crate::types::message::UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: images,
            is_compact_summary: false,
            plan_content: None,
            image_paste_ids: None,
            is_visible_in_transcript_only: false,
            mcp_meta: None,
            source_tool_assistant_uuid: None,
            permission_mode: None,
            origin: None,
            summarize_metadata: None,
        })];
        let options = Options::new(
            "claude-sonnet-4-20250514".to_string(),
            "repl_main_thread".to_string(),
        );

        let params = build_sdk_message_create_params(
            &messages,
            &Vec::new(),
            &ThinkingConfig::Disabled,
            &[],
            &options,
        )
        .expect("SDK params build");
        let value = serde_json::to_value(params).expect("params serialize");
        let media_count = value["messages"][0]["content"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|block| block.get("type").and_then(|value| value.as_str()) == Some("image"))
            .count();

        assert_eq!(media_count, API_MAX_MEDIA_PER_REQUEST);
    }

    #[test]
    fn sdk_params_include_structured_output_format_like_official() {
        let messages = vec![Message::User(UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::UserContent::Text(
                "return JSON".to_string(),
            )],
            is_compact_summary: false,
            plan_content: None,
            image_paste_ids: None,
            is_visible_in_transcript_only: false,
            mcp_meta: None,
            source_tool_assistant_uuid: None,
            permission_mode: None,
            origin: None,
            summarize_metadata: None,
        })];
        let mut options = Options::new(
            "claude-sonnet-4-20250514".to_string(),
            "repl_main_thread".to_string(),
        );
        options.output_format = Some(serde_json::json!({
            "type": "json_schema",
            "schema": {
                "type": "object",
                "properties": {
                    "answer": { "type": "string" }
                },
                "required": ["answer"]
            }
        }));

        let params = build_sdk_message_create_params(
            &messages,
            &Vec::new(),
            &ThinkingConfig::Disabled,
            &[],
            &options,
        )
        .expect("SDK params build");
        let value = serde_json::to_value(params).expect("params serialize");

        assert_eq!(
            value["output_config"]["format"]["type"],
            serde_json::json!("json_schema")
        );
        assert_eq!(
            value["output_config"]["format"]["schema"]["properties"]["answer"]["type"],
            serde_json::json!("string")
        );
    }

    #[test]
    fn sdk_request_options_attach_beta_header_extra_body_output_config_and_context_management_like_official()
     {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("CLAUDE_CODE_USE_BEDROCK");
        crate::utils::process_env::remove("CLAUDE_CODE_USE_VERTEX");
        crate::utils::process_env::remove("CLAUDE_CODE_USE_FOUNDRY");
        crate::utils::process_env::remove("CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS");
        crate::utils::betas::clear_betas_caches();
        crate::services::analytics::growthbook::reset_growth_book();
        // CC `betas.ts:270-277`: SDK / print-mode sessions keep thinking
        // summaries (no redact-thinking beta), which is what lets
        // `clear_thinking_20251015` lead the context_management edits below.
        let _interactive = crate::bootstrap::state::IsInteractiveGuard::capture();
        crate::bootstrap::state::set_is_interactive(false);
        crate::utils::process_env::set("CLAUDE_CODE_EXTRA_BODY", r#"{"service_tier":"priority"}"#);

        let messages = vec![Message::User(UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::UserContent::Text(
                "return JSON".to_string(),
            )],
            is_compact_summary: false,
            plan_content: None,
            image_paste_ids: None,
            is_visible_in_transcript_only: false,
            mcp_meta: None,
            source_tool_assistant_uuid: None,
            permission_mode: None,
            origin: None,
            summarize_metadata: None,
        })];
        let mut options = Options::new(
            "claude-sonnet-4-6".to_string(),
            "repl_main_thread".to_string(),
        );
        options.effort_value = Some(EffortValue::Named("high".to_string()));
        options.output_format = Some(serde_json::json!({
            "type": "json_schema",
            "schema": { "type": "object" }
        }));
        options.task_budget = Some(TaskBudget {
            total: 100_000,
            remaining: Some(80_000),
        });

        let request_options = build_sdk_message_request_options(
            &messages,
            &ThinkingConfig::Enabled {
                budget_tokens: Some(1024),
            },
            &options,
            &sdk_request_beta_capture_for_testing(&[], &options),
            None,
            anthropic_sdk::RequestOptions {
                max_retries: Some(0),
                ..Default::default()
            },
        )
        .expect("request options build");

        let beta_header = request_options
            .headers
            .as_ref()
            .and_then(|headers| headers.get("anthropic-beta"))
            .and_then(|value| value.as_deref())
            .expect("anthropic-beta header");
        assert!(beta_header.contains(crate::utils::betas::CLAUDE_CODE_20250219_BETA_HEADER));
        assert!(beta_header.contains(CONTEXT_MANAGEMENT_BETA_HEADER));
        assert!(beta_header.contains(EFFORT_BETA_HEADER));
        assert!(beta_header.contains(STRUCTURED_OUTPUTS_BETA_HEADER));
        assert!(beta_header.contains(TASK_BUDGETS_BETA_HEADER));
        assert_eq!(request_options.max_retries, Some(0));
        assert!(request_options.query.is_none());

        let patch_value = |path: &str| {
            request_options
                .json_body_patches
                .iter()
                .find_map(|patch| match patch {
                    anthropic_sdk::JsonBodyPatch::Set {
                        path: patch_path,
                        value,
                    } if patch_path == path => Some(value),
                    _ => None,
                })
        };

        assert_eq!(
            patch_value("service_tier"),
            Some(&serde_json::json!("priority"))
        );
        let output_config = patch_value("output_config")
            .and_then(|value| value.as_object())
            .expect("output_config patch");
        assert_eq!(output_config["effort"], serde_json::json!("high"));
        assert_eq!(
            output_config["format"]["type"],
            serde_json::json!("json_schema")
        );
        assert_eq!(
            output_config["task_budget"]["type"],
            serde_json::json!("tokens")
        );
        assert_eq!(
            patch_value("context_management").unwrap()["edits"][0]["type"],
            serde_json::json!("clear_thinking_20251015")
        );

        crate::utils::process_env::remove("CLAUDE_CODE_EXTRA_BODY");
        crate::utils::betas::clear_betas_caches();
    }

    #[test]
    fn explicit_output_format_body_header_order_and_empty_base_edge_matches_official() {
        let _env_guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let env_keys = [
            "NODE_ENV",
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
            "CLAUDE_CODE_USE_FOUNDRY",
            "CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS",
            "DISABLE_INTERLEAVED_THINKING",
            "DISABLE_TELEMETRY",
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
            "ANTHROPIC_BETAS",
            "CLAUDE_CODE_EXTRA_BODY",
        ];
        for key in env_keys {
            crate::utils::process_env::remove(key);
        }
        crate::services::analytics::growthbook::reset_growth_book();
        crate::utils::betas::clear_betas_caches();
        let mut gate_off = crate::utils::config::GlobalConfig::default();
        gate_off.cached_growth_book_features = Some(std::collections::HashMap::from([(
            "tengu_tool_pear".to_string(),
            serde_json::json!(false),
        )]));
        crate::utils::config::set_test_global_config(Some(gate_off));

        let messages = vec![Message::User(UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::UserContent::Text(
                "return JSON".to_string(),
            )],
            is_compact_summary: false,
            plan_content: None,
            image_paste_ids: None,
            is_visible_in_transcript_only: false,
            mcp_meta: None,
            source_tool_assistant_uuid: None,
            permission_mode: None,
            origin: None,
            summarize_metadata: None,
        })];
        let explicit_format = serde_json::json!({
            "type": "json_schema",
            "schema": {"type":"object","properties":{"answer":{"type":"string"}}}
        });
        let mut options = Options::new("claude-sonnet-4-6".to_string(), "compact".to_string());
        options.output_format = Some(explicit_format.clone());

        let explicit =
            build_sdk_request_options_plan(&messages, &ThinkingConfig::Disabled, &[], &options)
                .unwrap();
        assert_eq!(explicit.output_config["format"], explicit_format);
        assert!(explicit.emit_betas);
        assert_eq!(
            explicit
                .betas
                .iter()
                .filter(|beta| beta.as_str() == STRUCTURED_OUTPUTS_BETA_HEADER)
                .count(),
            1
        );

        crate::utils::process_env::set(
            "CLAUDE_CODE_EXTRA_BODY",
            r#"{"output_config":{"other":true}}"#,
        );
        let merged_output_config =
            build_sdk_request_options_plan(&messages, &ThinkingConfig::Disabled, &[], &options)
                .unwrap();
        assert_eq!(
            merged_output_config.output_config["format"],
            explicit_format
        );
        assert_eq!(
            merged_output_config.output_config["other"],
            serde_json::json!(true)
        );
        assert_eq!(
            merged_output_config
                .betas
                .iter()
                .filter(|beta| beta.as_str() == STRUCTURED_OUTPUTS_BETA_HEADER)
                .count(),
            1
        );

        crate::utils::process_env::set(
            "CLAUDE_CODE_EXTRA_BODY",
            r#"{"output_config":{"format":{"type":"json_schema","schema":{"const":"extra"}},"other":true}}"#,
        );
        let extra_wins =
            build_sdk_request_options_plan(&messages, &ThinkingConfig::Disabled, &[], &options)
                .unwrap();
        assert_eq!(
            extra_wins.output_config["format"]["schema"]["const"],
            serde_json::json!("extra")
        );
        assert_eq!(extra_wins.output_config["other"], serde_json::json!(true));
        assert!(
            !extra_wins
                .betas
                .iter()
                .any(|beta| beta == STRUCTURED_OUTPUTS_BETA_HEADER)
        );
        crate::utils::process_env::remove("CLAUDE_CODE_EXTRA_BODY");

        crate::utils::process_env::set("CLAUDE_CODE_USE_VERTEX", "1");
        crate::utils::betas::clear_betas_caches();
        let unsupported_provider =
            build_sdk_request_options_plan(&messages, &ThinkingConfig::Disabled, &[], &options)
                .unwrap();
        assert_eq!(
            unsupported_provider.output_config["format"],
            explicit_format
        );
        assert!(
            !unsupported_provider
                .betas
                .iter()
                .any(|beta| beta == STRUCTURED_OUTPUTS_BETA_HEADER)
        );
        crate::utils::process_env::remove("CLAUDE_CODE_USE_VERTEX");

        crate::utils::process_env::set("CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS", "1");
        crate::utils::betas::clear_betas_caches();
        let kill_bypass =
            build_sdk_request_options_plan(&messages, &ThinkingConfig::Disabled, &[], &options)
                .unwrap();
        assert!(
            kill_bypass
                .betas
                .iter()
                .any(|beta| beta == STRUCTURED_OUTPUTS_BETA_HEADER)
        );

        crate::utils::process_env::set("DISABLE_INTERLEAVED_THINKING", "1");
        crate::utils::betas::clear_betas_caches();
        options.model = "claude-haiku-4-5".to_string();
        let empty_base =
            build_sdk_request_options_plan(&messages, &ThinkingConfig::Disabled, &[], &options)
                .unwrap();
        assert!(!empty_base.emit_betas);
        assert_eq!(empty_base.output_config["format"], explicit_format);
        assert!(
            empty_base
                .betas
                .iter()
                .any(|beta| beta == STRUCTURED_OUTPUTS_BETA_HEADER)
        );
        let request = build_sdk_message_request_options(
            &messages,
            &ThinkingConfig::Disabled,
            &options,
            &sdk_request_beta_capture_for_testing(&[], &options),
            None,
            anthropic_sdk::RequestOptions::default(),
        )
        .unwrap();
        assert!(
            request
                .headers
                .as_ref()
                .and_then(|headers| headers.get("anthropic-beta"))
                .is_none()
        );
        assert!(request.json_body_patches.iter().any(|patch| matches!(
            patch,
            anthropic_sdk::JsonBodyPatch::Set { path, value }
                if path == "output_config" && value["format"] == explicit_format
        )));

        for key in env_keys {
            crate::utils::process_env::remove(key);
        }
        crate::utils::config::set_test_global_config(None);
        crate::utils::betas::clear_betas_caches();
    }

    #[test]
    fn rollout_header_tool_independence_and_custom_duplicates_matches_official() {
        let _env_guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for key in [
            "NODE_ENV",
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
            "CLAUDE_CODE_USE_FOUNDRY",
            "CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS",
            "DISABLE_TELEMETRY",
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
            "ANTHROPIC_BETAS",
        ] {
            crate::utils::process_env::remove(key);
        }
        crate::services::analytics::growthbook::reset_growth_book();
        crate::utils::betas::clear_betas_caches();
        let mut gate_on = crate::utils::config::GlobalConfig::default();
        gate_on.cached_growth_book_features = Some(std::collections::HashMap::from([(
            "tengu_tool_pear".to_string(),
            serde_json::json!(true),
        )]));
        crate::utils::config::set_test_global_config(Some(gate_on));

        let messages = vec![Message::User(UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::UserContent::Text(
                "hello".to_string(),
            )],
            is_compact_summary: false,
            plan_content: None,
            image_paste_ids: None,
            is_visible_in_transcript_only: false,
            mcp_meta: None,
            source_tool_assistant_uuid: None,
            permission_mode: None,
            origin: None,
            summarize_metadata: None,
        })];
        let options = Options::new("claude-sonnet-4-6".to_string(), "compact".to_string());
        let no_tools =
            build_sdk_request_options_plan(&messages, &ThinkingConfig::Disabled, &[], &options)
                .unwrap();
        // The injected `tengu_tool_pear` cache is inert: the gate resolves from
        // the source-controlled switch table, so nothing but an explicit
        // output_format or ANTHROPIC_BETAS adds the header.
        assert!(!crate::utils::feature_flags::feature_enabled(
            crate::utils::feature_flags::FeatureFlag::StrictToolSchemas,
        ));
        assert!(
            !no_tools
                .betas
                .iter()
                .any(|beta| beta == STRUCTURED_OUTPUTS_BETA_HEADER)
        );
        let non_strict_tool = Tool {
            name: "NonStrict".to_string(),
            description: "non-strict".to_string(),
            input_schema: serde_json::json!({"type":"object","properties":{}}),
            ..Default::default()
        };
        let only_non_strict = build_sdk_request_options_plan(
            &messages,
            &ThinkingConfig::Disabled,
            &[non_strict_tool],
            &options,
        )
        .unwrap();
        // Tool independence still holds: the declared tools never move the
        // header either way.
        assert_eq!(only_non_strict.betas, no_tools.betas);
        let mut rollout_with_explicit = options.clone();
        rollout_with_explicit.output_format = Some(serde_json::json!({
            "type":"json_schema",
            "schema":{"type":"object"}
        }));
        let one_rollout_copy = build_sdk_request_options_plan(
            &messages,
            &ThinkingConfig::Disabled,
            &[],
            &rollout_with_explicit,
        )
        .unwrap();
        assert_eq!(
            one_rollout_copy
                .betas
                .iter()
                .filter(|beta| beta.as_str() == STRUCTURED_OUTPUTS_BETA_HEADER)
                .count(),
            1
        );

        crate::utils::betas::clear_betas_caches();
        let mut gate_off = crate::utils::config::GlobalConfig::default();
        gate_off.cached_growth_book_features = Some(std::collections::HashMap::from([(
            "tengu_tool_pear".to_string(),
            serde_json::json!(false),
        )]));
        crate::utils::config::set_test_global_config(Some(gate_off));
        let mut explicit_options = options.clone();
        explicit_options.output_format = Some(serde_json::json!({
            "type":"json_schema",
            "schema":{"type":"object"}
        }));
        let retry_capture = sdk_request_beta_capture_for_testing(&[], &explicit_options);
        let mut retry_options = explicit_options.clone();
        retry_options.model = "unsupported-retry-model".to_string();
        let retry_first = params_from_context(
            &messages,
            &ThinkingConfig::Disabled,
            &retry_options,
            &retry_capture,
        )
        .unwrap();
        let retry_second = params_from_context(
            &messages,
            &ThinkingConfig::Disabled,
            &retry_options,
            &retry_capture,
        )
        .unwrap();
        assert_eq!(retry_first.betas, retry_second.betas);
        assert_eq!(
            retry_first
                .betas
                .iter()
                .filter(|beta| beta.as_str() == STRUCTURED_OUTPUTS_BETA_HEADER)
                .count(),
            1
        );

        crate::utils::process_env::set(
            "ANTHROPIC_BETAS",
            format!("{0},{0}", STRUCTURED_OUTPUTS_BETA_HEADER),
        );
        crate::utils::betas::clear_betas_caches();
        let first = build_sdk_request_options_plan(
            &messages,
            &ThinkingConfig::Disabled,
            &[],
            &explicit_options,
        )
        .unwrap();
        let second = build_sdk_request_options_plan(
            &messages,
            &ThinkingConfig::Disabled,
            &[],
            &explicit_options,
        )
        .unwrap();
        assert_eq!(first.betas, second.betas);
        assert_eq!(first.output_config, second.output_config);
        assert_eq!(
            first
                .betas
                .iter()
                .filter(|beta| beta.as_str() == STRUCTURED_OUTPUTS_BETA_HEADER)
                .count(),
            2
        );
        let request = build_sdk_message_request_options(
            &messages,
            &ThinkingConfig::Disabled,
            &explicit_options,
            &sdk_request_beta_capture_for_testing(&[], &explicit_options),
            None,
            anthropic_sdk::RequestOptions::default(),
        )
        .unwrap();
        let emitted = request
            .headers
            .as_ref()
            .and_then(|headers| headers.get("anthropic-beta"))
            .and_then(|value| value.as_deref())
            .unwrap()
            .split(',')
            .map(str::to_string)
            .collect::<Vec<_>>();
        assert_eq!(emitted, first.betas);

        crate::utils::process_env::remove("ANTHROPIC_BETAS");
        crate::utils::config::set_test_global_config(None);
        crate::utils::betas::clear_betas_caches();
    }

    #[test]
    fn source_local_beta_guards_preserve_intentional_duplicates_matches_official() {
        let _env_guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for key in [
            "NODE_ENV",
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
            "CLAUDE_CODE_USE_FOUNDRY",
            "CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS",
            "ENABLE_TOOL_SEARCH",
            "ANTHROPIC_BETAS",
            "CLAUDE_CODE_EXTRA_BODY",
        ] {
            crate::utils::process_env::remove(key);
        }
        crate::utils::config::set_test_global_config(Some(
            crate::utils::config::GlobalConfig::default(),
        ));
        crate::utils::betas::clear_betas_caches();

        let messages = vec![Message::User(UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::UserContent::Text(
                "test beta ordering".to_string(),
            )],
            is_compact_summary: false,
            plan_content: None,
            image_paste_ids: None,
            is_visible_in_transcript_only: false,
            mcp_meta: None,
            source_tool_assistant_uuid: None,
            permission_mode: None,
            origin: None,
            summarize_metadata: None,
        })];
        crate::utils::process_env::set("ANTHROPIC_BETAS", ADVISOR_BETA_HEADER);
        let mut advisor_options =
            Options::new("claude-sonnet-4-6".to_string(), "compact".to_string());
        advisor_options.advisor_model = Some("claude-haiku-4-5".to_string());
        let advisor_capture = sdk_request_beta_capture_for_testing(&[], &advisor_options);
        assert_eq!(
            advisor_capture
                .betas
                .iter()
                .filter(|beta| beta.as_str() == ADVISOR_BETA_HEADER)
                .count(),
            2
        );

        crate::utils::process_env::set("CLAUDE_CODE_USE_BEDROCK", "1");
        crate::utils::process_env::set("ANTHROPIC_BETAS", TOOL_SEARCH_BETA_HEADER_3P);
        crate::utils::betas::clear_betas_caches();
        let options = Options::new(
            "claude-sonnet-4-20250514".to_string(),
            "repl_main_thread".to_string(),
        );
        let tools = vec![
            crate::tools::tool_search_tool::tool_search_tool_schema(),
            crate::tools::web_fetch_tool::web_fetch_tool_schema(),
        ];
        let plan =
            build_sdk_request_options_plan(&messages, &ThinkingConfig::Disabled, &tools, &options)
                .unwrap();
        assert_eq!(
            plan.extra_body_params["anthropic_beta"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|beta| beta.as_str() == Some(TOOL_SEARCH_BETA_HEADER_3P))
                .count(),
            2
        );

        for key in [
            "CLAUDE_CODE_USE_BEDROCK",
            "ANTHROPIC_BETAS",
            "ENABLE_TOOL_SEARCH",
        ] {
            crate::utils::process_env::remove(key);
        }
        crate::utils::config::set_test_global_config(None);
        crate::utils::betas::clear_betas_caches();
    }

    #[test]
    fn sdk_request_options_add_tool_search_beta_when_deferred_loading_is_sent() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("CLAUDE_CODE_USE_BEDROCK");
        crate::utils::process_env::remove("CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS");
        crate::utils::process_env::remove("ENABLE_TOOL_SEARCH");
        let messages = vec![Message::User(UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::UserContent::Text(
                "search tools".to_string(),
            )],
            is_compact_summary: false,
            plan_content: None,
            image_paste_ids: None,
            is_visible_in_transcript_only: false,
            mcp_meta: None,
            source_tool_assistant_uuid: None,
            permission_mode: None,
            origin: None,
            summarize_metadata: None,
        })];
        let options = Options::new(
            "claude-sonnet-4-6".to_string(),
            "repl_main_thread".to_string(),
        );
        let tools = vec![
            crate::tools::tool_search_tool::tool_search_tool_schema(),
            crate::tools::web_fetch_tool::web_fetch_tool_schema(),
        ];

        let request_options = build_sdk_message_request_options(
            &messages,
            &ThinkingConfig::Disabled,
            &options,
            &sdk_request_beta_capture_for_testing(&tools, &options),
            None,
            anthropic_sdk::RequestOptions::default(),
        )
        .expect("request options build");
        let beta_header = request_options
            .headers
            .as_ref()
            .and_then(|headers| headers.get("anthropic-beta"))
            .and_then(|value| value.as_deref())
            .expect("anthropic-beta header");

        assert!(beta_header.contains(crate::utils::betas::TOOL_SEARCH_BETA_HEADER_1P));
    }

    #[test]
    fn sdk_params_keep_tool_search_available_while_mcp_servers_pending_like_official() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("CLAUDE_CODE_USE_BEDROCK");
        crate::utils::process_env::remove("CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS");
        crate::utils::process_env::remove("ENABLE_TOOL_SEARCH");
        let messages = vec![Message::User(UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::UserContent::Text(
                "search tools".to_string(),
            )],
            is_compact_summary: false,
            plan_content: None,
            image_paste_ids: None,
            is_visible_in_transcript_only: false,
            mcp_meta: None,
            source_tool_assistant_uuid: None,
            permission_mode: None,
            origin: None,
            summarize_metadata: None,
        })];
        let tools = vec![
            crate::tools::tool_search_tool::tool_search_tool_schema(),
            crate::tools::bash_tool::bash_tool_schema(),
        ];
        let mut pending_options = Options::new(
            "claude-sonnet-4-20250514".to_string(),
            "repl_main_thread".to_string(),
        );
        pending_options.has_pending_mcp_servers = Some(true);

        let pending_params = build_sdk_message_create_params(
            &messages,
            &Vec::new(),
            &ThinkingConfig::Disabled,
            &tools,
            &pending_options,
        )
        .expect("SDK params build");
        let pending_value = serde_json::to_value(pending_params).expect("params serialize");
        let pending_tool_names = pending_value
            .get("tools")
            .and_then(|value| value.as_array())
            .expect("tools are serialized")
            .iter()
            .filter_map(|tool| tool.get("name").and_then(|value| value.as_str()))
            .collect::<Vec<_>>();
        assert!(pending_tool_names.contains(&"ToolSearch"));
        assert!(pending_tool_names.contains(&"Bash"));

        let request_options = build_sdk_message_request_options(
            &messages,
            &ThinkingConfig::Disabled,
            &pending_options,
            &sdk_request_beta_capture_for_testing(&tools, &pending_options),
            None,
            anthropic_sdk::RequestOptions::default(),
        )
        .expect("request options build");
        let beta_header = request_options
            .headers
            .as_ref()
            .and_then(|headers| headers.get("anthropic-beta"))
            .and_then(|value| value.as_deref())
            .expect("anthropic-beta header");
        assert!(beta_header.contains(crate::utils::betas::TOOL_SEARCH_BETA_HEADER_1P));

        let mut settled_options = pending_options.clone();
        settled_options.has_pending_mcp_servers = Some(false);
        let settled_params = build_sdk_message_create_params(
            &messages,
            &Vec::new(),
            &ThinkingConfig::Disabled,
            &tools,
            &settled_options,
        )
        .expect("SDK params build");
        let settled_value = serde_json::to_value(settled_params).expect("params serialize");
        let settled_tool_names = settled_value
            .get("tools")
            .and_then(|value| value.as_array())
            .expect("tools are serialized")
            .iter()
            .filter_map(|tool| tool.get("name").and_then(|value| value.as_str()))
            .collect::<Vec<_>>();
        assert!(!settled_tool_names.contains(&"ToolSearch"));
        assert!(settled_tool_names.contains(&"Bash"));
    }

    #[test]
    fn sdk_request_options_put_bedrock_only_betas_in_extra_body_like_official() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("CLAUDE_CODE_USE_BEDROCK", "1");
        crate::utils::process_env::remove("CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS");
        crate::utils::process_env::remove("ENABLE_TOOL_SEARCH");
        crate::utils::process_env::remove("DISABLE_INTERLEAVED_THINKING");
        crate::utils::process_env::remove("CLAUDE_CODE_EXTRA_BODY");
        crate::utils::betas::clear_betas_caches();
        let messages = vec![Message::User(UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::UserContent::Text(
                "search tools".to_string(),
            )],
            is_compact_summary: false,
            plan_content: None,
            image_paste_ids: None,
            is_visible_in_transcript_only: false,
            mcp_meta: None,
            source_tool_assistant_uuid: None,
            permission_mode: None,
            origin: None,
            summarize_metadata: None,
        })];
        let options = Options::new(
            "claude-sonnet-4-20250514[1m]".to_string(),
            "repl_main_thread".to_string(),
        );
        let tools = vec![
            crate::tools::tool_search_tool::tool_search_tool_schema(),
            crate::tools::web_fetch_tool::web_fetch_tool_schema(),
        ];

        let request_options = build_sdk_message_request_options(
            &messages,
            &ThinkingConfig::Disabled,
            &options,
            &sdk_request_beta_capture_for_testing(&tools, &options),
            None,
            anthropic_sdk::RequestOptions::default(),
        )
        .expect("request options build");
        let beta_header = request_options
            .headers
            .as_ref()
            .and_then(|headers| headers.get("anthropic-beta"))
            .and_then(|value| value.as_deref())
            .unwrap_or_default();
        assert!(!beta_header.contains(crate::utils::betas::TOOL_SEARCH_BETA_HEADER_3P));
        assert!(!beta_header.contains(crate::utils::betas::CONTEXT_1M_BETA_HEADER));
        assert!(!beta_header.contains(crate::utils::betas::INTERLEAVED_THINKING_BETA_HEADER));

        let anthropic_beta = request_options
            .json_body_patches
            .iter()
            .find_map(|patch| match patch {
                anthropic_sdk::JsonBodyPatch::Set { path, value } if path == "anthropic_beta" => {
                    value.as_array()
                }
                _ => None,
            })
            .expect("anthropic_beta extra body patch");
        let beta_strings = anthropic_beta
            .iter()
            .filter_map(|value| value.as_str())
            .collect::<Vec<_>>();
        assert!(beta_strings.contains(&crate::utils::betas::TOOL_SEARCH_BETA_HEADER_3P));
        assert!(beta_strings.contains(&crate::utils::betas::CONTEXT_1M_BETA_HEADER));
        assert!(beta_strings.contains(&crate::utils::betas::INTERLEAVED_THINKING_BETA_HEADER));

        crate::utils::process_env::remove("CLAUDE_CODE_USE_BEDROCK");
        crate::utils::betas::clear_betas_caches();
    }

    #[test]
    fn query_single_shot_helpers_match_official_prompt_and_options_shape() {
        let mut options = Options::new(
            "claude-opus-4-20250514".to_string(),
            "repl_main_thread".to_string(),
        );
        options.enable_prompt_caching = None;
        let output_format = serde_json::json!({
            "type": "json_schema",
            "schema": { "type": "object" }
        });

        let messages = single_user_prompt_messages("summarize this");
        let single_shot_options = single_shot_query_options(
            &options,
            Some("claude-sonnet-4-20250514".to_string()),
            Some(&output_format),
        );

        assert_eq!(single_shot_options.model, "claude-sonnet-4-20250514");
        assert_eq!(single_shot_options.enable_prompt_caching, Some(false));
        assert_eq!(single_shot_options.output_format, Some(output_format));
        assert_eq!(messages.len(), 1);
        match &messages[0] {
            Message::User(user) => assert_eq!(
                user.content,
                vec![crate::types::message::UserContent::Text(
                    "summarize this".to_string()
                )]
            ),
            other => panic!("expected user message, got {other:?}"),
        }
    }

    #[test]
    fn get_all_base_tools_are_passed_to_sdk_params() {
        let messages = vec![Message::User(UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::UserContent::Text(
                "use a tool".to_string(),
            )],
            is_compact_summary: false,
            plan_content: None,
            image_paste_ids: None,
            is_visible_in_transcript_only: false,
            mcp_meta: None,
            source_tool_assistant_uuid: None,
            permission_mode: None,
            origin: None,
            summarize_metadata: None,
        })];
        let options = Options::new(
            "claude-sonnet-4-20250514".to_string(),
            "repl_main_thread".to_string(),
        );
        let tools = crate::tools::get_all_base_tools();

        let params = build_sdk_message_create_params(
            &messages,
            &Vec::new(),
            &ThinkingConfig::Disabled,
            &tools,
            &options,
        )
        .expect("SDK params build");
        let value = serde_json::to_value(params).expect("params serialize");
        let api_tools = value
            .get("tools")
            .and_then(|value| value.as_array())
            .expect("tools are serialized");

        assert!(!api_tools.is_empty());
        assert!(
            api_tools
                .iter()
                .any(|tool| tool.get("name").and_then(|value| value.as_str()) == Some("Bash"))
        );
        assert!(api_tools.iter().any(|tool| {
            tool.get("input_schema")
                .and_then(|schema| schema.get("properties"))
                .and_then(|properties| properties.get("command"))
                .is_some()
        }));
    }

    #[test]
    fn sdk_params_mark_deferred_tools_for_tool_search_like_official() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS");
        crate::utils::process_env::remove("ENABLE_TOOL_SEARCH");
        let messages = vec![
            Message::User(UserMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![crate::types::message::UserContent::Text(
                    "use a deferred tool".to_string(),
                )],
                is_compact_summary: false,
                plan_content: None,
                image_paste_ids: None,
                is_visible_in_transcript_only: false,
                mcp_meta: None,
                source_tool_assistant_uuid: None,
                permission_mode: None,
                origin: None,
                summarize_metadata: None,
            }),
            Message::User(UserMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![crate::types::message::UserContent::ToolResult(
                    crate::types::message::ToolResult {
                        tool_use_id: crate::types::ids::ToolUseId("toolu_search".to_string()),
                        content: "{}".to_string(),
                        is_error: false,
                        content_blocks: vec![
                            crate::types::message::ToolResultContentBlock::ToolReference {
                                tool_name: "WebFetch".to_string(),
                            },
                        ],
                        tool_use_result: None,
                    },
                )],
                is_compact_summary: false,
                plan_content: None,
                image_paste_ids: None,
                is_visible_in_transcript_only: false,
                mcp_meta: None,
                source_tool_assistant_uuid: None,
                permission_mode: None,
                origin: None,
                summarize_metadata: None,
            }),
        ];
        let options = Options::new(
            "claude-sonnet-4-20250514".to_string(),
            "repl_main_thread".to_string(),
        );
        let tools = vec![
            crate::tools::tool_search_tool::tool_search_tool_schema(),
            crate::tools::web_fetch_tool::web_fetch_tool_schema(),
        ];

        let params = build_sdk_message_create_params(
            &messages,
            &Vec::new(),
            &ThinkingConfig::Disabled,
            &tools,
            &options,
        )
        .expect("SDK params build");
        let value = serde_json::to_value(params).expect("params serialize");
        let web_fetch = value
            .get("tools")
            .and_then(|value| value.as_array())
            .and_then(|tools| {
                tools.iter().find(|tool| {
                    tool.get("name").and_then(|value| value.as_str()) == Some("WebFetch")
                })
            })
            .expect("WebFetch tool should serialize");
        let tool_search = value
            .get("tools")
            .and_then(|value| value.as_array())
            .and_then(|tools| {
                tools.iter().find(|tool| {
                    tool.get("name").and_then(|value| value.as_str()) == Some("ToolSearch")
                })
            })
            .expect("ToolSearch tool should serialize");

        assert_eq!(web_fetch["defer_loading"], serde_json::json!(true));
        assert!(tool_search.get("defer_loading").is_none());
    }

    #[test]
    fn sdk_params_with_tool_search_omit_undiscovered_deferred_tools_like_official() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS");
        crate::utils::process_env::remove("ENABLE_TOOL_SEARCH");
        let messages = vec![Message::User(UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::UserContent::Text(
                "search for tools".to_string(),
            )],
            is_compact_summary: false,
            plan_content: None,
            image_paste_ids: None,
            is_visible_in_transcript_only: false,
            mcp_meta: None,
            source_tool_assistant_uuid: None,
            permission_mode: None,
            origin: None,
            summarize_metadata: None,
        })];
        let options = Options::new(
            "claude-sonnet-4-20250514".to_string(),
            "repl_main_thread".to_string(),
        );
        let tools = vec![
            crate::tools::tool_search_tool::tool_search_tool_schema(),
            crate::tools::web_fetch_tool::web_fetch_tool_schema(),
        ];

        let params = build_sdk_message_create_params(
            &messages,
            &Vec::new(),
            &ThinkingConfig::Disabled,
            &tools,
            &options,
        )
        .expect("SDK params build");
        let value = serde_json::to_value(params).expect("params serialize");
        let api_tools = value
            .get("tools")
            .and_then(|value| value.as_array())
            .expect("tools are serialized");

        assert!(api_tools.iter().any(|tool| {
            tool.get("name").and_then(|value| value.as_str()) == Some("ToolSearch")
        }));
        assert!(
            !api_tools.iter().any(|tool| {
                tool.get("name").and_then(|value| value.as_str()) == Some("WebFetch")
            })
        );
    }

    #[test]
    fn sdk_params_do_not_mark_deferred_tools_when_tool_search_request_gate_is_off() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS");
        crate::utils::process_env::remove("ENABLE_TOOL_SEARCH");
        let messages = vec![Message::User(UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::UserContent::Text(
                "use a deferred tool".to_string(),
            )],
            is_compact_summary: false,
            plan_content: None,
            image_paste_ids: None,
            is_visible_in_transcript_only: false,
            mcp_meta: None,
            source_tool_assistant_uuid: None,
            permission_mode: None,
            origin: None,
            summarize_metadata: None,
        })];
        let mut options = Options::new(
            "claude-3-5-haiku-latest".to_string(),
            "repl_main_thread".to_string(),
        );
        let tools = vec![
            crate::tools::tool_search_tool::tool_search_tool_schema(),
            crate::tools::web_fetch_tool::web_fetch_tool_schema(),
        ];

        let params = build_sdk_message_create_params(
            &messages,
            &Vec::new(),
            &ThinkingConfig::Disabled,
            &tools,
            &options,
        )
        .expect("SDK params build");
        let value = serde_json::to_value(params).expect("params serialize");
        let web_fetch = value
            .get("tools")
            .and_then(|value| value.as_array())
            .and_then(|tools| {
                tools.iter().find(|tool| {
                    tool.get("name").and_then(|value| value.as_str()) == Some("WebFetch")
                })
            })
            .expect("WebFetch tool should serialize");
        assert!(web_fetch.get("defer_loading").is_none());

        options.model = "claude-sonnet-4-20250514".to_string();
        let no_tool_search_tools = vec![crate::tools::web_fetch_tool::web_fetch_tool_schema()];
        let params = build_sdk_message_create_params(
            &messages,
            &Vec::new(),
            &ThinkingConfig::Disabled,
            &no_tool_search_tools,
            &options,
        )
        .expect("SDK params build");
        let value = serde_json::to_value(params).expect("params serialize");
        let web_fetch = value
            .get("tools")
            .and_then(|value| value.as_array())
            .and_then(|tools| tools.first())
            .expect("WebFetch tool should serialize");
        assert!(web_fetch.get("defer_loading").is_none());
    }

    #[test]
    fn get_all_base_tools_include_common_official_base_tools() {
        let names = crate::tools::get_all_base_tools()
            .into_iter()
            .map(|tool| tool.name)
            .collect::<std::collections::BTreeSet<_>>();

        for expected in [
            "Agent",
            "Bash",
            "Read",
            "Edit",
            "Write",
            "Glob",
            "Grep",
            "ExitPlanMode",
            "NotebookEdit",
            "WebFetch",
            "WebSearch",
            "TaskStop",
            "AskUserQuestion",
            "Skill",
            "ListMcpResourcesTool",
            "ReadMcpResourceTool",
            "EnterPlanMode",
            "TodoWrite",
        ] {
            assert!(names.contains(expected), "missing {expected} tool schema");
        }
        assert!(
            names.contains("TaskOutput"),
            "getAllBaseTools always includes TaskOutput before isEnabled filtering",
        );
        let active_names = crate::tools::get_tools(&crate::tool::ToolPermissionContext::default())
            .into_iter()
            .map(|tool| tool.name)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            active_names.contains("TaskOutput"),
            crate::utils::build_profile::build_audience().is_external(),
            "getTools applies TaskOutput's external-distribution isEnabled gate",
        );
    }

    #[test]
    fn max_output_tokens_projects_official_system_api_error_message() {
        let error = max_output_tokens_system_api_error_message(64_000);

        assert!(
            error
                .content
                .contains("Claude's response exceeded the 64000 output token maximum")
        );
        assert_eq!(error.api_error, "max_output_tokens");
        assert_eq!(error.error, "max_output_tokens");
        assert_eq!(error.error_details.as_deref(), Some(error.content.as_str()));
    }

    #[test]
    fn refusal_stop_reason_projects_official_system_api_error_message() {
        let error = refusal_system_api_error_message("claude-opus-4-20250514")
            .expect("refusal should produce API error message");

        assert!(error.content.contains("Claude Code is unable to respond"));
        assert_eq!(error.error, "invalid_request");
        assert!(error.content.contains("/model claude-sonnet-4-20250514"));
    }

    #[test]
    fn get_tools_filter_blanket_deny_rules_before_sdk_params() {
        let mut context = crate::tool::ToolPermissionContext::default();
        context.always_deny_rules.insert(
            crate::types::permissions::PermissionRuleSource::Session,
            vec![crate::types::permissions::PermissionRuleValue::new(
                "Bash", None,
            )],
        );

        let tools = crate::tools::get_tools(&context);

        assert!(tools.iter().all(|tool| tool.name != "Bash"));
        assert!(tools.iter().any(|tool| tool.name == "Read"));
    }

    #[test]
    fn streaming_usage_and_stop_reason_helpers_preserve_sdk_metadata() {
        let usage = anthropic_sdk::resources::messages::Usage {
            cache_creation: None,
            cache_creation_input_tokens: Some(12),
            cache_read_input_tokens: Some(34),
            inference_geo: None,
            input_tokens: 100,
            output_tokens: 0,
            server_tool_use: None,
            service_tier: None,
        };
        let initial = sdk_usage_to_token_usage(&usage);
        assert_eq!(initial.input_tokens, 100);
        assert_eq!(initial.cache_creation_input_tokens, 12);
        assert_eq!(initial.cache_read_input_tokens, 34);
        assert_eq!(initial.cache_deleted_input_tokens, 0);

        let merged = merge_sdk_delta_usage(
            Some(initial),
            &anthropic_sdk::resources::messages::MessageDeltaUsage {
                cache_creation_input_tokens: None,
                cache_read_input_tokens: Some(55),
                input_tokens: None,
                output_tokens: 77,
                server_tool_use: None,
            },
        );
        assert_eq!(merged.input_tokens, 100);
        assert_eq!(merged.output_tokens, 77);
        assert_eq!(merged.cache_creation_input_tokens, 12);
        assert_eq!(merged.cache_read_input_tokens, 55);
        assert_eq!(merged.cache_deleted_input_tokens, 0);

        assert_eq!(
            sdk_cache_deleted_input_tokens(&serde_json::json!({
                "cache_deleted_input_tokens": 64
            })),
            Some(64)
        );

        assert_eq!(
            sdk_stop_reason_to_message_stop_reason(Some(
                anthropic_sdk::resources::messages::StopReason::ToolUse
            )),
            Some(crate::types::message::StopReason::ToolUse)
        );
        assert_eq!(
            sdk_stop_reason_to_message_stop_reason(Some(
                anthropic_sdk::resources::messages::StopReason::PauseTurn
            )),
            Some(crate::types::message::StopReason::PauseTurn)
        );
        assert_eq!(
            sdk_stop_reason_to_message_stop_reason(Some(
                anthropic_sdk::resources::messages::StopReason::Refusal
            )),
            Some(crate::types::message::StopReason::Refusal)
        );
    }

    // -- verify_api_key stub --

    #[test]
    fn verify_api_key_stub_does_not_perform_network_success() {
        let result = futures::executor::block_on(verify_api_key("sk-ant-test", false));
        assert!(result.is_err());
    }

    #[test]
    fn verify_api_key_non_interactive_skips_verification() {
        let result = futures::executor::block_on(verify_api_key("sk-ant-test", true));
        assert!(result.is_ok());
        assert!(result.unwrap());
    }

    // -- get_extra_body_params --

    #[test]
    fn get_extra_body_params_empty_when_no_env() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        // Temporarily ensure env var is not set
        crate::utils::process_env::remove("CLAUDE_CODE_EXTRA_BODY");
        let result = get_extra_body_params(None);
        assert!(result.is_empty());
    }

    #[test]
    fn get_extra_body_params_with_beta_headers() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("CLAUDE_CODE_EXTRA_BODY");
        let headers = vec!["beta-1".to_string(), "beta-2".to_string()];
        let result = get_extra_body_params(Some(&headers));
        let betas = result.get("anthropic_beta").unwrap();
        assert!(betas.is_array());
        assert_eq!(betas.as_array().unwrap().len(), 2);
    }

    #[test]
    fn get_extra_body_params_deduplicates_beta_headers() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set(
            "CLAUDE_CODE_EXTRA_BODY",
            r#"{"anthropic_beta":["existing-beta"]}"#,
        );
        let headers = vec!["existing-beta".to_string(), "new-beta".to_string()];
        let result = get_extra_body_params(Some(&headers));
        let betas = result.get("anthropic_beta").unwrap().as_array().unwrap();
        // Should have existing-beta (from env) + new-beta (deduped)
        assert_eq!(betas.len(), 2);
        crate::utils::process_env::remove("CLAUDE_CODE_EXTRA_BODY");
    }

    // -- get_prompt_caching_enabled --

    #[test]
    fn prompt_caching_enabled_by_default() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("DISABLE_PROMPT_CACHING");
        assert!(get_prompt_caching_enabled("claude-sonnet-4-20250514"));
    }

    #[test]
    fn prompt_caching_disabled_globally() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("DISABLE_PROMPT_CACHING", "1");
        assert!(!get_prompt_caching_enabled("claude-sonnet-4-20250514"));
        crate::utils::process_env::remove("DISABLE_PROMPT_CACHING");
    }

    // -- get_cache_control --

    #[test]
    fn cache_control_default_is_ephemeral() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("CLAUDE_CODE_USE_BEDROCK");
        crate::utils::process_env::remove("CLAUDE_CODE_USE_VERTEX");
        crate::utils::process_env::remove("CLAUDE_CODE_USE_FOUNDRY");
        crate::utils::process_env::remove("ENABLE_PROMPT_CACHING_1H_BEDROCK");
        let cc = get_cache_control(None, None);
        assert_eq!(cc.cache_type, "ephemeral");
        assert!(cc.ttl.is_none());
        assert!(cc.scope.is_none());
    }

    #[test]
    fn cache_control_global_scope() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("CLAUDE_CODE_USE_BEDROCK");
        crate::utils::process_env::remove("ENABLE_PROMPT_CACHING_1H_BEDROCK");
        let cc = get_cache_control(Some(CacheScope::Global), None);
        assert_eq!(cc.scope, Some(CacheScope::Global));
        assert_eq!(
            serde_json::to_value(&cc).unwrap()["scope"],
            serde_json::json!("global")
        );
        // CC `...(scope === 'global' && { scope })`: org is the API default
        // and never goes on the wire.
        assert!(
            get_cache_control(Some(CacheScope::Org), None)
                .scope
                .is_none()
        );
    }

    #[test]
    fn cache_control_1h_ttl_matches_official_bedrock_opt_in_and_allowlist_gate() {
        use crate::utils::model::providers::ApiProvider;

        assert!(should_1h_cache_ttl_with_inputs(
            ApiProvider::Bedrock,
            true,
            false,
            &[],
            None,
            false,
        ));
        assert!(!should_1h_cache_ttl_with_inputs(
            ApiProvider::Bedrock,
            false,
            false,
            &["*".to_string()],
            Some("sdk"),
            false,
        ));
        assert!(!should_1h_cache_ttl_with_inputs(
            ApiProvider::Vertex,
            false,
            false,
            &["sdk".to_string()],
            Some("sdk"),
            true,
        ));
        assert!(should_1h_cache_ttl_with_inputs(
            ApiProvider::Vertex,
            false,
            true,
            &["repl_main_thread*".to_string(), "sdk".to_string()],
            Some("repl_main_thread"),
            false,
        ));
        assert!(should_1h_cache_ttl_with_inputs(
            ApiProvider::Foundry,
            false,
            true,
            &["sdk".to_string()],
            Some("sdk"),
            false,
        ));
        assert!(!should_1h_cache_ttl_with_inputs(
            ApiProvider::Foundry,
            false,
            true,
            &["agent:*".to_string()],
            Some("sdk"),
            true,
        ));
        // @cometix offset: first-party uses PromptCache1hConfig as the final
        // control. Empty allowlist + flag on = any querySource; a non-empty
        // list uses the official prefix-match rules.
        assert!(!should_1h_cache_ttl_with_inputs(
            ApiProvider::FirstParty,
            false,
            true,
            &["*".to_string()],
            Some("sdk"),
            false,
        ));
        assert!(should_1h_cache_ttl_with_inputs(
            ApiProvider::FirstParty,
            false,
            false,
            &[],
            Some("repl_main_thread"),
            true,
        ));
        assert!(!should_1h_cache_ttl_with_inputs(
            ApiProvider::FirstParty,
            false,
            true,
            &["*".to_string()],
            None,
            true,
        ));
        assert!(should_1h_cache_ttl_with_inputs(
            ApiProvider::FirstParty,
            false,
            false,
            &["repl_main_thread*".to_string()],
            Some("repl_main_thread"),
            true,
        ));
        assert!(!should_1h_cache_ttl_with_inputs(
            ApiProvider::FirstParty,
            false,
            false,
            &["repl_main_thread*".to_string()],
            Some("sdk"),
            true,
        ));
    }

    #[test]
    fn cache_control_sets_1h_ttl_for_bedrock_official_env_opt_in() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("CLAUDE_CODE_USE_BEDROCK", "1");
        crate::utils::process_env::remove("CLAUDE_CODE_USE_VERTEX");
        crate::utils::process_env::remove("CLAUDE_CODE_USE_FOUNDRY");
        crate::utils::process_env::set("ENABLE_PROMPT_CACHING_1H_BEDROCK", "1");

        let cc = get_cache_control(None, Some("compact"));
        assert_eq!(cc.ttl.as_deref(), Some("1h"));

        crate::utils::process_env::remove("CLAUDE_CODE_USE_BEDROCK");
        crate::utils::process_env::remove("ENABLE_PROMPT_CACHING_1H_BEDROCK");
    }

    #[test]
    fn cache_control_1h_allowlist_parses_official_payload_shape() {
        assert_eq!(
            prompt_cache_1h_allowlist_from_feature_value(Some(&serde_json::json!({
                "allowlist": ["repl_main_thread*", "sdk", 7]
            }))),
            ["repl_main_thread*", "sdk"]
        );
        assert!(
            prompt_cache_1h_allowlist_from_feature_value(Some(&serde_json::json!({}))).is_empty()
        );
        assert!(prompt_cache_1h_allowlist_from_feature_value(None).is_empty());
    }

    #[test]
    fn cache_control_1h_allowlist_ignores_growthbook_delivery() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let root =
            std::env::temp_dir().join(format!("cometix-prompt-cache-1h-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("create temp config dir");
        std::fs::write(
            root.join(".claude.json"),
            r#"{
                "cachedGrowthBookFeatures": {
                    "tengu_prompt_cache_1h_config": {
                        "allowlist": ["repl_main_thread*", "sdk"]
                    }
                },
                "growthBookOverrides": {
                    "tengu_prompt_cache_1h_config": {"allowlist": ["agent:*"]}
                }
            }"#,
        )
        .expect("write temp global config");

        crate::utils::process_env::set("CLAUDE_CONFIG_DIR", &root);
        crate::utils::process_env::set(
            "CLAUDE_INTERNAL_FC_OVERRIDES",
            r#"{"tengu_prompt_cache_1h_config":{"allowlist":["sdk"]}}"#,
        );
        crate::utils::process_env::remove("CLAUDE_CODE_USE_BEDROCK");
        crate::utils::process_env::remove("CLAUDE_CODE_USE_VERTEX");
        crate::utils::process_env::remove("CLAUDE_CODE_USE_FOUNDRY");
        crate::utils::process_env::remove("ENABLE_PROMPT_CACHING_1H_BEDROCK");

        crate::utils::settings::settings_cache::reset_settings_cache();
        for source in [
            crate::utils::settings::SettingSource::User,
            crate::utils::settings::SettingSource::Project,
            crate::utils::settings::SettingSource::Local,
            crate::utils::settings::SettingSource::Flag,
            crate::utils::settings::SettingSource::Policy,
        ] {
            crate::utils::settings::settings_cache::set_cached_settings_for_source(source, None);
        }

        // Official fallback `{}` keeps the non-1P allowlist empty. GrowthBook
        // disk/env payloads stay inert. First-party follows PromptCache1hConfig
        // (currently off), so ttl stays omitted here.
        assert!(prompt_cache_1h_allowlist().is_empty());
        assert!(
            get_cache_control(None, Some("repl_main_thread:main"))
                .ttl
                .is_none()
        );
        assert!(get_cache_control(None, Some("sdk")).ttl.is_none());
        assert!(
            get_cache_control(None, Some("agent:general-purpose"))
                .ttl
                .is_none()
        );

        // Official Bedrock env opt-in still drives 1h for that provider.
        crate::utils::process_env::set("CLAUDE_CODE_USE_BEDROCK", "1");
        crate::utils::process_env::set("ENABLE_PROMPT_CACHING_1H_BEDROCK", "1");
        assert_eq!(
            get_cache_control(None, Some("sdk")).ttl.as_deref(),
            Some("1h")
        );

        crate::utils::process_env::remove("CLAUDE_CODE_USE_BEDROCK");
        crate::utils::process_env::remove("ENABLE_PROMPT_CACHING_1H_BEDROCK");
        crate::utils::process_env::remove("CLAUDE_CONFIG_DIR");
        crate::utils::process_env::remove("CLAUDE_INTERNAL_FC_OVERRIDES");
        crate::utils::settings::settings_cache::reset_settings_cache();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn cache_control_1h_allowlist_reads_settings_not_growthbook() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::settings::settings_cache::reset_settings_cache();
        struct ResetCache;
        impl Drop for ResetCache {
            fn drop(&mut self) {
                crate::utils::settings::settings_cache::reset_settings_cache();
            }
        }
        let _reset = ResetCache;
        for source in [
            crate::utils::settings::SettingSource::User,
            crate::utils::settings::SettingSource::Project,
            crate::utils::settings::SettingSource::Local,
            crate::utils::settings::SettingSource::Flag,
            crate::utils::settings::SettingSource::Policy,
        ] {
            crate::utils::settings::settings_cache::set_cached_settings_for_source(source, None);
        }
        crate::utils::settings::settings_cache::set_cached_settings_for_source(
            crate::utils::settings::SettingSource::User,
            Some(crate::utils::settings::types::SettingsJson {
                prompt_cache_1h: Some(crate::utils::settings::types::PromptCache1hSettings {
                    allowlist: Some(vec!["sdk".into(), "repl_main_thread*".into()]),
                }),
                ..Default::default()
            }),
        );
        assert_eq!(
            prompt_cache_1h_allowlist(),
            vec!["sdk".to_string(), "repl_main_thread*".to_string()]
        );
    }

    // -- configure_effort_params --

    #[test]
    fn configure_effort_params_named_effort() {
        let mut output_config = serde_json::Map::new();
        let mut extra = serde_json::Map::new();
        let mut betas = Vec::new();
        let effort = EffortValue::Named("high".to_string());

        configure_effort_params(
            Some(&effort),
            &mut output_config,
            &mut extra,
            &mut betas,
            "claude-sonnet-4-6-20260101",
        );

        assert_eq!(
            output_config.get("effort").unwrap(),
            &JsonValue::String("high".to_string())
        );
        assert!(betas.contains(&EFFORT_BETA_HEADER.to_string()));
    }

    #[test]
    fn configure_effort_params_skips_if_already_set() {
        let mut output_config = serde_json::Map::new();
        output_config.insert("effort".to_string(), JsonValue::String("low".to_string()));
        let mut extra = serde_json::Map::new();
        let mut betas = Vec::new();

        configure_effort_params(
            Some(&EffortValue::Named("high".to_string())),
            &mut output_config,
            &mut extra,
            &mut betas,
            "claude-sonnet-4-6-20260101",
        );

        // Should not overwrite existing effort
        assert_eq!(
            output_config.get("effort").unwrap(),
            &JsonValue::String("low".to_string())
        );
        assert!(betas.is_empty());
    }

    // -- configure_task_budget_params --

    #[test]
    fn configure_task_budget_params_basic() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let mut output_config = serde_json::Map::new();
        let mut betas = Vec::new();
        let budget = TaskBudget {
            total: 100_000,
            remaining: Some(50_000),
        };

        // Set up env so should_include_first_party_only_betas returns true
        crate::utils::process_env::set("ANTHROPIC_API_KEY", "sk-ant-test-key");
        configure_task_budget_params(Some(&budget), &mut output_config, &mut betas);
        crate::utils::process_env::remove("ANTHROPIC_API_KEY");

        let task_budget = output_config.get("task_budget").unwrap();
        assert_eq!(
            task_budget.get("type").unwrap(),
            &JsonValue::String("tokens".to_string())
        );
        assert_eq!(task_budget.get("total").unwrap(), 100_000);
        assert_eq!(task_budget.get("remaining").unwrap(), 50_000);
        assert!(betas.contains(&TASK_BUDGETS_BETA_HEADER.to_string()));
    }

    // -- update_usage --

    #[test]
    fn update_usage_preserves_input_on_zero() {
        let usage = NonNullableUsage {
            input_tokens: 1000,
            output_tokens: 500,
            ..Default::default()
        };

        // Simulate a message_delta with input_tokens=0 (should not overwrite)
        let mut delta = serde_json::Map::new();
        delta.insert("input_tokens".to_string(), JsonValue::from(0u64));
        delta.insert("output_tokens".to_string(), JsonValue::from(750u64));

        let updated = update_usage(&usage, Some(&delta));
        assert_eq!(updated.input_tokens, 1000); // Preserved
        assert_eq!(updated.output_tokens, 750); // Updated
    }

    #[test]
    fn update_usage_with_none_returns_clone() {
        let usage = NonNullableUsage {
            input_tokens: 42,
            output_tokens: 17,
            ..Default::default()
        };
        let result = update_usage(&usage, None);
        assert_eq!(result, usage);
    }

    // -- accumulate_usage --

    #[test]
    fn accumulate_usage_sums_tokens() {
        let total = NonNullableUsage {
            input_tokens: 100,
            output_tokens: 200,
            cache_creation_input_tokens: 50,
            cache_read_input_tokens: 30,
            server_tool_use: ServerToolUse {
                web_search_requests: 1,
                web_fetch_requests: 2,
            },
            ..Default::default()
        };

        let msg = NonNullableUsage {
            input_tokens: 50,
            output_tokens: 100,
            cache_creation_input_tokens: 25,
            cache_read_input_tokens: 15,
            server_tool_use: ServerToolUse {
                web_search_requests: 1,
                web_fetch_requests: 0,
            },
            service_tier: Some("standard".to_string()),
            inference_geo: Some("us-east-1".to_string()),
            ..Default::default()
        };

        let result = accumulate_usage(&total, &msg);
        assert_eq!(result.input_tokens, 150);
        assert_eq!(result.output_tokens, 300);
        assert_eq!(result.cache_creation_input_tokens, 75);
        assert_eq!(result.cache_read_input_tokens, 45);
        assert_eq!(result.server_tool_use.web_search_requests, 2);
        assert_eq!(result.server_tool_use.web_fetch_requests, 2);
        assert_eq!(result.service_tier.as_deref(), Some("standard"));
        assert_eq!(result.inference_geo.as_deref(), Some("us-east-1"));
    }

    // -- adjust_params_for_non_streaming --

    #[test]
    fn adjust_params_caps_max_tokens() {
        let params = NonStreamingParams {
            max_tokens: 128_000,
            thinking: None,
            extra: HashMap::new(),
        };

        let adjusted = adjust_params_for_non_streaming(&params, MAX_NON_STREAMING_TOKENS);
        assert_eq!(adjusted.max_tokens, MAX_NON_STREAMING_TOKENS);
    }

    #[test]
    fn adjust_params_caps_thinking_budget() {
        let params = NonStreamingParams {
            max_tokens: 128_000,
            thinking: Some(ThinkingConfig::Enabled {
                budget_tokens: Some(100_000),
            }),
            extra: HashMap::new(),
        };

        let adjusted = adjust_params_for_non_streaming(&params, MAX_NON_STREAMING_TOKENS);
        assert_eq!(adjusted.max_tokens, MAX_NON_STREAMING_TOKENS);
        match adjusted.thinking.unwrap() {
            ThinkingConfig::Enabled { budget_tokens } => {
                assert_eq!(budget_tokens.unwrap(), MAX_NON_STREAMING_TOKENS as i64 - 1);
            }
            _ => panic!("Expected Enabled thinking config"),
        }
    }

    #[test]
    fn adjust_params_preserves_adaptive_thinking() {
        let params = NonStreamingParams {
            max_tokens: 128_000,
            thinking: Some(ThinkingConfig::Adaptive),
            extra: HashMap::new(),
        };

        let adjusted = adjust_params_for_non_streaming(&params, MAX_NON_STREAMING_TOKENS);
        assert!(matches!(adjusted.thinking, Some(ThinkingConfig::Adaptive)));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn non_streaming_oauth_refusal_precedes_request_plan_construction() {
        let _env_guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let config_dir = std::env::temp_dir().join(format!(
            "cometix-non-streaming-oauth-order-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join(".credentials.json"),
            serde_json::json!({
                "claudeAiOauth": {
                    "accessToken": "expired-access-token",
                    "refreshToken": "refresh-token",
                    "expiresAt": 1,
                    "scopes": ["user:inference"]
                }
            })
            .to_string(),
        )
        .unwrap();
        let _guards = [
            EnvVarGuard::unset("CLAUDE_CODE_OAUTH_TOKEN"),
            EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config_dir),
            EnvVarGuard::unset("CLAUDE_CODE_SIMPLE"),
            EnvVarGuard::unset("CLAUDE_CODE_USE_BEDROCK"),
            EnvVarGuard::unset("CLAUDE_CODE_USE_VERTEX"),
            EnvVarGuard::unset("CLAUDE_CODE_USE_FOUNDRY"),
            EnvVarGuard::set("CLAUDE_CODE_MAX_RETRIES", "0"),
        ];
        crate::bootstrap::state::reset_auth_file_descriptor_caches_for_testing();
        crate::utils::config::set_test_global_config(Some(
            crate::utils::config::GlobalConfig::default(),
        ));

        let plan_calls = std::sync::atomic::AtomicUsize::new(0);
        let request_options_calls = std::sync::atomic::AtomicUsize::new(0);
        let result = execute_non_streaming_request_with_initial_consecutive_529_errors(
            "claude-sonnet-4-20250514",
            "repl_main_thread",
            None,
            &ThinkingConfig::Disabled,
            || {
                plan_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(serde_json::json!({
                    "max_tokens": 1024,
                    "messages": [{"role": "user", "content": "must not build"}]
                }))
            },
            None,
            None,
            || {
                request_options_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(anthropic_sdk::RequestOptions::default())
            },
            None,
        )
        .await;

        assert!(
            result
                .expect_err("selected OAuth must hit the closed credential outlet")
                .to_string()
                .contains(
                    crate::constants::oauth::OAUTH_CREDENTIAL_SIDE_EFFECTS_UNAVAILABLE_MESSAGE
                )
        );
        assert_eq!(plan_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(
            request_options_calls.load(std::sync::atomic::Ordering::SeqCst),
            0
        );

        crate::utils::config::set_test_global_config(None);
        let _ = std::fs::remove_dir_all(config_dir);
    }

    #[test]
    fn build_non_streaming_request_body_caps_tokens_and_sets_stream_false() {
        let body = build_non_streaming_request_body(
            "claude-sonnet-4-20250514",
            || {
                Ok(serde_json::json!({
                    "max_tokens": 128000,
                    "thinking": { "type": "enabled", "budget_tokens": 100000 },
                    "messages": [{ "role": "user", "content": "hello" }],
                    "model": "ignored-model"
                }))
            },
            None,
        )
        .expect("non-streaming body builds");

        assert_eq!(body["model"], serde_json::json!("claude-sonnet-4-20250514"));
        assert_eq!(body["stream"], serde_json::json!(false));
        assert_eq!(
            body["max_tokens"],
            serde_json::json!(MAX_NON_STREAMING_TOKENS)
        );
        assert_eq!(
            body["thinking"]["budget_tokens"],
            serde_json::json!(MAX_NON_STREAMING_TOKENS as i64 - 1)
        );
        assert_eq!(body["messages"][0]["content"], serde_json::json!("hello"));
    }

    #[test]
    fn non_streaming_retry_context_updates_model_and_max_tokens_like_official() {
        let mut body = serde_json::json!({
            "max_tokens": 8000,
            "thinking": { "type": "enabled", "budget_tokens": 7000 },
            "messages": [{ "role": "user", "content": "hello" }],
            "model": "primary-model",
            "stream": false
        });
        let retry_context = crate::services::api::with_retry::RetryContext {
            model: "fallback-model".to_string(),
            thinking_config: ThinkingConfig::Enabled {
                budget_tokens: Some(5000),
            },
            fast_mode: None,
            max_tokens_override: Some(3000),
        };

        apply_retry_context_to_non_streaming_body(&mut body, &retry_context);

        assert_eq!(body["model"], serde_json::json!("fallback-model"));
        assert_eq!(body["max_tokens"], serde_json::json!(3000));
        assert_eq!(body["thinking"]["budget_tokens"], serde_json::json!(2999));
        assert_eq!(body["stream"], serde_json::json!(false));
    }

    #[test]
    fn sdk_api_error_mapping_preserves_retry_status_headers_and_body() {
        let mut headers = std::collections::HashMap::new();
        headers.insert("retry-after".to_string(), "1".to_string());
        let error = anthropic_sdk::ApiError::generate(
            Some(529),
            Some(serde_json::json!({"type": "overloaded_error", "message": "busy"})),
            None,
            Some(headers),
        );

        match sdk_api_error_to_retryable(error) {
            crate::services::api::with_retry::RetryableError::Api(api) => {
                assert_eq!(api.status, Some(529));
                assert_eq!(api.header("retry-after"), Some("1"));
                assert!(
                    api.body
                        .as_ref()
                        .is_some_and(|body| body["type"] == serde_json::json!("overloaded_error"))
                );
            }
            other => panic!("expected API retryable error, got {other}"),
        }
    }

    #[test]
    fn retry_query_source_maps_official_foreground_sources() {
        assert_eq!(
            retry_query_source_from_api_source("repl_main_thread"),
            crate::constants::query_source::RetryQuerySource::ReplMainThread
        );
        assert_eq!(
            retry_query_source_from_api_source("compact"),
            crate::constants::query_source::RetryQuerySource::Compact
        );
        assert_eq!(
            retry_query_source_from_api_source("background_title"),
            crate::constants::query_source::RetryQuerySource::Other("background_title".to_string())
        );
        // CC `withRetry.ts:68-70` spells these with COLONS, matching
        // `promptCategory.ts`'s output. They used to be spelled
        // `agent_custom`/`agent_default`/`agent_builtin` here, which no
        // `as_api_source()` string could ever produce.
        for (source, expected) in [
            (
                "agent:custom",
                crate::constants::query_source::RetryQuerySource::AgentCustom,
            ),
            (
                "agent:default",
                crate::constants::query_source::RetryQuerySource::AgentDefault,
            ),
            (
                "agent:builtin",
                crate::constants::query_source::RetryQuerySource::AgentBuiltin,
            ),
        ] {
            assert_eq!(retry_query_source_from_api_source(source), expected);
        }
        // The producer side of the same two strings, so a rename on either side
        // fails here rather than silently dropping agent turns out of the
        // foreground retry set.
        assert_eq!(
            crate::constants::query_source::QuerySource::AgentCustom.as_api_source(),
            "agent:custom"
        );
        assert_eq!(
            crate::constants::query_source::QuerySource::AgentDefault.as_api_source(),
            "agent:default"
        );
        // `FOREGROUND_529_RETRY_SOURCES.has(querySource)` is an EXACT lookup in
        // CC, so a real `agent:builtin:<agentType>` misses the `'agent:builtin'`
        // entry and falls through to the no-retry bucket. Pinned because it
        // looks like a mapping bug and is not one.
        assert_eq!(
            retry_query_source_from_api_source(
                crate::constants::query_source::QuerySource::agent_builtin("fork").as_api_source()
            ),
            crate::constants::query_source::RetryQuerySource::Other(
                "agent:builtin:fork".to_string()
            )
        );
    }

    #[test]
    fn streaming_retry_context_updates_attempt_options_like_official() {
        let base = Options::new("primary-model".to_string(), "repl_main_thread".to_string());
        let retry_context = crate::services::api::with_retry::RetryContext {
            model: "fallback-model".to_string(),
            thinking_config: ThinkingConfig::Disabled,
            fast_mode: None,
            max_tokens_override: Some(4096),
        };

        let attempt = streaming_attempt_options_from_retry_context(&base, &retry_context);

        assert_eq!(attempt.model, "fallback-model");
        assert_eq!(attempt.max_output_tokens_override, Some(4096));
        assert_eq!(attempt.query_source, "repl_main_thread");
    }

    #[test]
    fn retry_heartbeat_is_the_system_message_with_retry_yields() {
        // CC `claude.ts:1848-1856` re-yields `withRetry`'s yielded value
        // unchanged; there is no conversion between the factory
        // (utils/messages.ts:4585-4603) and the generator union component.
        let heartbeat = crate::utils::messages::create_system_api_error_message(
            "overloaded".to_string(),
            500,
            1,
            10,
        );

        let item = QueryModelStreamItem::SystemApiError(heartbeat);

        match item {
            QueryModelStreamItem::SystemApiError(
                crate::types::message::SystemMessage::ApiError {
                    error,
                    retry_in_ms,
                    retry_attempt,
                    max_retries,
                    ..
                },
            ) => {
                assert_eq!(error, "overloaded");
                assert_eq!(retry_in_ms, 500);
                assert_eq!(retry_attempt, 1);
                assert_eq!(max_retries, 10);
            }
            other => panic!("expected SystemApiError(ApiError), got {other:?}"),
        }
    }

    #[test]
    fn non_streaming_fallback_response_maps_to_typed_assistant() {
        let response = serde_json::json!({
            "id": "msg_fallback",
            "_request_id": "req_fallback",
            "type": "message",
            "role": "assistant",
            "model": "claude-fallback",
            "content": [
                { "type": "text", "text": "done" },
                {
                    "type": "tool_use",
                    "id": "toolu_1",
                    "name": "Read",
                    "input": { "file_path": "Cargo.toml" }
                }
            ],
            "stop_reason": "tool_use",
            "stop_sequence": null,
            "usage": {
                "input_tokens": 10,
                "output_tokens": 20,
                "cache_creation_input_tokens": 1,
                "cache_read_input_tokens": 2
            }
        });

        let assistant = sdk_message_response_to_assistant_message(response, &[], None).unwrap();

        assert_eq!(assistant.model.as_deref(), Some("claude-fallback"));
        assert_eq!(assistant.request_id(), Some("req_fallback"));
        assert_eq!(assistant.api_message_id(), Some("msg_fallback"));
        assert!(!assistant.uuid.is_empty());
        assert_eq!(
            assistant.stop_reason,
            Some(crate::types::message::StopReason::ToolUse)
        );
        assert!(matches!(
            &assistant.content[0],
            crate::types::message::AssistantContent::Text(text) if text == "done"
        ));
        assert!(matches!(
            &assistant.content[1],
            crate::types::message::AssistantContent::ToolUse(block)
                if block.id.0 == "toolu_1"
                    && block.name == "Read"
                    && block.input.get("file_path").and_then(|value| value.as_str()) == Some("Cargo.toml")
        ));
        let usage = assistant.usage.unwrap();
        assert_eq!(usage.input_tokens, 10);
        assert_eq!(usage.output_tokens, 20);
        assert_eq!(usage.cache_creation_input_tokens, 1);
        assert_eq!(usage.cache_read_input_tokens, 2);
    }

    // -- get_max_output_tokens_for_model --

    #[test]
    fn get_max_output_tokens_default_for_sonnet() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("CLAUDE_CODE_MAX_OUTPUT_TOKENS");
        crate::utils::process_env::remove("COMETIX_MAX_TOKENS_CAP");
        let result = get_max_output_tokens_for_model("claude-sonnet-4-20250514");
        assert_eq!(result, 32_000);
    }

    #[test]
    fn get_max_output_tokens_env_override() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("CLAUDE_CODE_MAX_OUTPUT_TOKENS", "32000suffix");
        crate::utils::process_env::remove("COMETIX_MAX_TOKENS_CAP");
        let result = get_max_output_tokens_for_model("claude-sonnet-4-20250514");
        assert_eq!(result, 32_000);

        crate::utils::process_env::set("CLAUDE_CODE_MAX_OUTPUT_TOKENS", "0");
        let result = get_max_output_tokens_for_model("claude-sonnet-4-20250514");
        assert_eq!(result, 32_000);
        crate::utils::process_env::remove("CLAUDE_CODE_MAX_OUTPUT_TOKENS");
    }

    // -- get_nonstreaming_fallback_timeout_ms --

    #[test]
    fn nonstreaming_timeout_default() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("API_TIMEOUT_MS");
        crate::utils::process_env::remove("CLAUDE_CODE_REMOTE");
        assert_eq!(get_nonstreaming_fallback_timeout_ms(), 300_000);
    }

    #[test]
    fn nonstreaming_timeout_remote() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("API_TIMEOUT_MS");
        crate::utils::process_env::set("CLAUDE_CODE_REMOTE", "1");
        assert_eq!(get_nonstreaming_fallback_timeout_ms(), 120_000);
        crate::utils::process_env::remove("CLAUDE_CODE_REMOTE");
    }

    #[test]
    fn nonstreaming_timeout_override() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("API_TIMEOUT_MS", "60000");
        assert_eq!(get_nonstreaming_fallback_timeout_ms(), 60_000);
        crate::utils::process_env::remove("API_TIMEOUT_MS");
    }

    // -- build_system_prompt_blocks --

    #[test]
    fn build_system_prompt_blocks_with_caching() {
        let prompt = vec![
            "You are a helpful assistant.".to_string(),
            "Follow these rules.".to_string(),
        ];
        let blocks = build_system_prompt_blocks(&prompt, true, false, None);
        assert_eq!(blocks.len(), 1);
        assert_eq!(
            blocks[0].text,
            "You are a helpful assistant.\n\nFollow these rules."
        );
        assert!(blocks[0].cache_control.is_some());
        assert_eq!(
            blocks[0].cache_control.as_ref().unwrap().cache_type,
            "ephemeral"
        );
        assert!(blocks[0].cache_control.as_ref().unwrap().scope.is_none());
    }

    #[test]
    fn build_system_prompt_blocks_without_caching() {
        let prompt = vec!["Hello.".to_string()];
        let blocks = build_system_prompt_blocks(&prompt, false, false, None);
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].cache_control.is_none());
    }

    #[test]
    fn build_system_prompt_blocks_splits_global_static_and_uncached_dynamic_boundary() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("CLAUDE_CODE_USE_GLOBAL_CACHE_SCOPE", "1");
        crate::utils::process_env::remove("ANTHROPIC_USE_GLOBAL_CACHE_SCOPE");
        let prompt = vec![
            "static one".to_string(),
            "static two".to_string(),
            crate::constants::prompts::SYSTEM_PROMPT_DYNAMIC_BOUNDARY.to_string(),
            "dynamic one".to_string(),
        ];

        let blocks = build_system_prompt_blocks(&prompt, true, false, None);

        crate::utils::process_env::remove("CLAUDE_CODE_USE_GLOBAL_CACHE_SCOPE");
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].text, "static one\n\nstatic two");
        assert_eq!(blocks[1].text, "dynamic one");
        assert_eq!(
            blocks[0]
                .cache_control
                .as_ref()
                .and_then(|control| control.scope),
            Some(CacheScope::Global)
        );
        assert!(blocks[1].cache_control.is_none());
    }

    #[test]
    fn build_system_prompt_blocks_places_attribution_and_prefix_like_official() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        // `shouldUseGlobalCacheScope()` = firstParty && !DISABLE_EXPERIMENTAL_BETAS.
        let _bedrock = EnvVarGuard::unset("CLAUDE_CODE_USE_BEDROCK");
        let _vertex = EnvVarGuard::unset("CLAUDE_CODE_USE_VERTEX");
        let _foundry = EnvVarGuard::unset("CLAUDE_CODE_USE_FOUNDRY");
        let _disable = EnvVarGuard::unset("CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS");
        let header = "x-anthropic-billing-header: cc_version=0.0.0.abc; cc_entrypoint=cli;";
        let prefix = crate::constants::system::get_cli_sysprompt_prefix(false, false);
        let prompt = vec![
            header.to_string(),
            prefix.to_string(),
            "static".to_string(),
            crate::constants::prompts::SYSTEM_PROMPT_DYNAMIC_BOUNDARY.to_string(),
            "dynamic".to_string(),
        ];

        // Global cache mode (`utils/api.ts:361-405`): header null, prefix null,
        // static global, dynamic null.
        let blocks = build_system_prompt_blocks(&prompt, true, false, None);
        assert_eq!(
            blocks
                .iter()
                .map(|block| block.text.as_str())
                .collect::<Vec<_>>(),
            vec![header, prefix, "static", "dynamic"]
        );
        assert!(blocks[0].cache_control.is_none());
        assert!(blocks[1].cache_control.is_none());
        assert_eq!(
            blocks[2].cache_control.as_ref().and_then(|c| c.scope),
            Some(CacheScope::Global)
        );
        assert!(blocks[3].cache_control.is_none());

        // Tool-based cache (`utils/api.ts:326-359`): header null, prefix org,
        // rest org; boundary dropped.
        let blocks = build_system_prompt_blocks(&prompt, true, true, None);
        assert_eq!(
            blocks
                .iter()
                .map(|block| block.text.as_str())
                .collect::<Vec<_>>(),
            vec![header, prefix, "static\n\ndynamic"]
        );
        assert!(blocks[0].cache_control.is_none());
        assert!(
            blocks[1]
                .cache_control
                .as_ref()
                .is_some_and(|c| c.scope.is_none())
        );
        assert!(
            blocks[2]
                .cache_control
                .as_ref()
                .is_some_and(|c| c.scope.is_none())
        );

        // Default mode (`utils/api.ts:411-433`): same shape, boundary text kept
        // inside the joined rest because nothing strips it here in CC either.
        let _disable_betas = EnvVarGuard::set("CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS", "1");
        let blocks = build_system_prompt_blocks(&prompt, true, false, None);
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0].text, header);
        assert!(blocks[0].cache_control.is_none());
        assert_eq!(blocks[1].text, prefix);
        assert!(
            blocks[1]
                .cache_control
                .as_ref()
                .is_some_and(|c| c.scope.is_none())
        );
        assert!(blocks[2].text.starts_with("static\n\n"));
        assert!(blocks[2].text.ends_with("\n\ndynamic"));
    }

    #[test]
    fn build_system_prompt_blocks_skips_global_boundary_when_tool_cache_owns_system_prompt() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("CLAUDE_CODE_USE_GLOBAL_CACHE_SCOPE", "1");
        crate::utils::process_env::remove("ANTHROPIC_USE_GLOBAL_CACHE_SCOPE");
        let prompt = vec![
            "static".to_string(),
            crate::constants::prompts::SYSTEM_PROMPT_DYNAMIC_BOUNDARY.to_string(),
            "dynamic".to_string(),
        ];

        let blocks = build_system_prompt_blocks(&prompt, true, true, None);

        crate::utils::process_env::remove("CLAUDE_CODE_USE_GLOBAL_CACHE_SCOPE");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].text, "static\n\ndynamic");
        assert!(blocks[0].cache_control.is_some());
        assert!(blocks[0].cache_control.as_ref().unwrap().scope.is_none());
    }

    // -- strip_excess_media_items --

    #[test]
    fn strip_excess_media_items_strips_oldest_typed_user_images() {
        let messages = vec![
            (Message::User(crate::types::message::UserMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![
                    crate::types::message::UserContent::Image {
                        media_type: "image/png".to_string(),
                        data: "old".to_string(),
                    },
                    crate::types::message::UserContent::Text("first".to_string()),
                ],
                is_compact_summary: false,
                plan_content: None,
                image_paste_ids: None,
                is_visible_in_transcript_only: false,
                mcp_meta: None,
                source_tool_assistant_uuid: None,
                permission_mode: None,
                origin: None,
                summarize_metadata: None,
            }),),
            (Message::User(crate::types::message::UserMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![
                    crate::types::message::UserContent::Image {
                        media_type: "image/png".to_string(),
                        data: "recent".to_string(),
                    },
                    crate::types::message::UserContent::Text("second".to_string()),
                ],
                is_compact_summary: false,
                plan_content: None,
                image_paste_ids: None,
                is_visible_in_transcript_only: false,
                mcp_meta: None,
                source_tool_assistant_uuid: None,
                permission_mode: None,
                origin: None,
                summarize_metadata: None,
            }),),
        ];

        let stripped = strip_excess_media_items(&messages, 1);

        let Message::User(first) = &stripped[0] else {
            panic!("expected first user message");
        };
        assert_eq!(first.content.len(), 1);
        assert!(matches!(
            &first.content[0],
            crate::types::message::UserContent::Text(text) if text == "first"
        ));
        let Message::User(second) = &stripped[1] else {
            panic!("expected second user message");
        };
        assert!(second.content.iter().any(|content| matches!(
            content,
            crate::types::message::UserContent::Image { data, .. } if data == "recent"
        )));
    }

    // -- strip_excess_media_items_typed --

    #[test]
    fn strip_excess_media_no_strip_when_under_limit() {
        let messages = vec![MessageParam {
            role: "user".to_string(),
            content: MessageContent::Blocks(vec![
                serde_json::json!({"type": "image", "source": {"type": "base64"}}),
                serde_json::json!({"type": "text", "text": "describe this"}),
            ]),
        }];
        let result = strip_excess_media_items_typed(messages, 5);
        assert_eq!(result.len(), 1);
        if let MessageContent::Blocks(blocks) = &result[0].content {
            assert_eq!(blocks.len(), 2); // Nothing stripped
        }
    }

    #[test]
    fn strip_excess_media_strips_oldest() {
        let messages = vec![
            MessageParam {
                role: "user".to_string(),
                content: MessageContent::Blocks(vec![
                    serde_json::json!({"type": "image", "source": {"type": "base64"}}),
                    serde_json::json!({"type": "text", "text": "first image"}),
                ]),
            },
            MessageParam {
                role: "user".to_string(),
                content: MessageContent::Blocks(vec![
                    serde_json::json!({"type": "image", "source": {"type": "base64"}}),
                    serde_json::json!({"type": "text", "text": "second image"}),
                ]),
            },
        ];
        let result = strip_excess_media_items_typed(messages, 1);
        // First image should be stripped, second preserved
        if let MessageContent::Blocks(blocks) = &result[0].content {
            assert_eq!(blocks.len(), 1); // Image removed, text kept
            assert_eq!(blocks[0].get("type").unwrap(), "text");
        }
        if let MessageContent::Blocks(blocks) = &result[1].content {
            assert_eq!(blocks.len(), 2); // Both kept
        }
    }

    // -- get_api_metadata --

    #[test]
    fn get_api_metadata_returns_valid_json_user_id() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("CLAUDE_CODE_EXTRA_METADATA");
        let meta = get_api_metadata();
        let parsed: serde_json::Value =
            serde_json::from_str(&meta.user_id).expect("user_id should be valid JSON");
        assert!(parsed.get("device_id").is_some());
    }

    #[test]
    fn get_api_metadata_device_id_matches_official_persisted_user_id() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("CLAUDE_CODE_EXTRA_METADATA");
        let mut config = crate::utils::config::GlobalConfig::default();
        config.user_id = Some("0123456789abcdef0123456789abcdef".to_string());
        let previous = crate::utils::config::replace_test_global_config(Some(config));

        let first: serde_json::Value =
            serde_json::from_str(&get_api_metadata().user_id).expect("valid JSON");
        let second: serde_json::Value =
            serde_json::from_str(&get_api_metadata().user_id).expect("valid JSON");

        crate::utils::config::replace_test_global_config(previous);
        // CC `claude.ts:521` `device_id: getOrCreateUserID()` — stable across
        // requests within a session, sourced from the persisted global config.
        assert_eq!(
            first["device_id"],
            serde_json::json!("0123456789abcdef0123456789abcdef")
        );
        assert_eq!(first["device_id"], second["device_id"]);
        // CC `claude.ts:523` `account_uuid: getOauthAccountInfo()?.accountUuid ?? ''`.
        assert!(first["account_uuid"].is_string());
        assert!(first.get("session_id").is_some());
    }

    #[test]
    fn get_api_metadata_mints_one_device_id_and_reuses_it() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("CLAUDE_CODE_EXTRA_METADATA");
        let previous = crate::utils::config::replace_test_global_config(Some(
            crate::utils::config::GlobalConfig::default(),
        ));
        let first: serde_json::Value =
            serde_json::from_str(&get_api_metadata().user_id).expect("valid JSON");
        let second: serde_json::Value =
            serde_json::from_str(&get_api_metadata().user_id).expect("valid JSON");
        crate::utils::config::replace_test_global_config(previous);
        let device_id = first["device_id"].as_str().expect("device_id");
        assert_eq!(device_id.len(), 64);
        assert_eq!(first["device_id"], second["device_id"]);
        assert_eq!(first["account_uuid"], serde_json::json!(""));
    }

    // -- stream idle / stall watchdog helpers --

    #[test]
    fn stream_idle_timeout_defaults_to_90s_like_official() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("CLAUDE_STREAM_IDLE_TIMEOUT_MS");
        assert_eq!(stream_idle_timeout_ms(), 90_000);
        crate::utils::process_env::set("CLAUDE_STREAM_IDLE_TIMEOUT_MS", "45000");
        assert_eq!(stream_idle_timeout_ms(), 45_000);
        crate::utils::process_env::set("CLAUDE_STREAM_IDLE_TIMEOUT_MS", "0");
        assert_eq!(stream_idle_timeout_ms(), 90_000);
        crate::utils::process_env::set("CLAUDE_STREAM_IDLE_TIMEOUT_MS", "not-a-number");
        assert_eq!(stream_idle_timeout_ms(), 90_000);
        crate::utils::process_env::remove("CLAUDE_STREAM_IDLE_TIMEOUT_MS");
    }

    #[test]
    fn stream_watchdog_gated_by_claude_enable_stream_watchdog() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("CLAUDE_ENABLE_STREAM_WATCHDOG");
        assert!(!stream_watchdog_enabled());
        crate::utils::process_env::set("CLAUDE_ENABLE_STREAM_WATCHDOG", "1");
        assert!(stream_watchdog_enabled());
        crate::utils::process_env::remove("CLAUDE_ENABLE_STREAM_WATCHDOG");
    }

    #[test]
    fn stream_stall_threshold_matches_official_30s() {
        assert_eq!(STREAM_STALL_THRESHOLD_MS, 30_000);
    }

    #[test]
    fn build_sdk_message_request_options_copies_abort_signal_like_official_create_signal() {
        // Maps to CC `beta.messages.create(..., { signal })`.
        let (handle, signal) = anthropic_sdk::AbortSignal::pair();
        let mut options = Options::new(
            "claude-sonnet-4-20250514".to_string(),
            "repl_main_thread".to_string(),
        );
        options.abort_signal = Some(signal);
        let request_options = build_sdk_message_request_options(
            &[],
            &ThinkingConfig::Disabled,
            &options,
            &sdk_request_beta_capture_for_testing(&[], &options),
            None,
            anthropic_sdk::RequestOptions::default(),
        )
        .expect("request options");
        assert!(request_options.signal.is_some());
        assert!(!request_options.signal.as_ref().unwrap().is_aborted());
        handle.abort();
        assert!(request_options.signal.as_ref().unwrap().is_aborted());
        assert!(
            options
                .abort_signal
                .as_ref()
                .is_some_and(anthropic_sdk::AbortSignal::is_aborted)
        );
    }

    #[test]
    fn init_request_matches_official_expanded_prompt_and_reminder_seams() {
        use crate::types::message::{AttachmentMessage, UserContent};
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let previous = crate::utils::config::replace_test_global_config(Some(Default::default()));
        let metadata =
            "<command-message>init</command-message>\n<command-name>/init</command-name>";
        let prompt = crate::commands::init::get_prompt_for_command();
        let mut expanded = crate::utils::messages::create_user_message(prompt.into());
        expanded.content = vec![UserContent::MetaText(prompt.into())];
        let messages = vec![
            Message::User(crate::utils::messages::create_user_message(metadata.into())),
            Message::User(expanded),
            Message::Attachment(AttachmentMessage::new(
                serde_json::json!({"type":"skill_listing","content":"commit: Record changes","skillCount":1,"isInitial":true}),
            )),
        ];
        let mut options =
            Options::new("claude-sonnet-4-20250514".into(), "repl_main_thread".into());
        options.enable_prompt_caching = Some(false);
        let plan = build_sdk_message_create_plan(
            &messages,
            &Vec::new(),
            &ThinkingConfig::Disabled,
            &[],
            &options,
        )
        .unwrap();
        let payload = serde_json::to_value(plan.params).unwrap();
        // CC messages.ts:1481-1529/2180/2411-2455, through actual SDK request
        // construction (including role repair), without any HTTP/model call.
        assert_eq!(
            payload["messages"],
            serde_json::json!([{"role":"user","content":[
                {"type":"text","text":"<system-reminder>\nThe following skills are available for use with the Skill tool:\n\ncommit: Record changes\n</system-reminder>\n"},
                {"type":"text","text":format!("{metadata}\n")},
                {"type":"text","text":prompt}
            ]}])
        );
        crate::utils::config::replace_test_global_config(previous);
    }
}

#[cfg(test)]
mod raw_image_tests {
    use super::*;
    use crate::types::message::{ToolResultContentBlock, UserContent};
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[test]
    fn raw_image_api_projection_matches_official_url_cache_control_and_no_mutation() {
        let image = serde_json::json!({"type":"image","source":{"type":"url","url":"https://example.test/image.png"},"cache_control":{"type":"ephemeral"}});
        let mut user = crate::utils::messages::create_user_message(String::new());
        user.content = vec![UserContent::from_image_block(image.clone(), true)];
        for add_cache in [false, true] {
            let param = user_message_to_message_param(&user, add_cache, false, None);
            assert_eq!(serde_json::to_value(&param).unwrap()["content"][0], image);
            let sdk = local_message_param_to_sdk(&param).unwrap();
            let sdk_value = serde_json::to_value(sdk).unwrap();
            assert_eq!(sdk_value["content"][0]["source"], image["source"]);
            assert_eq!(
                sdk_value["content"][0]["cache_control"],
                image["cache_control"]
            );
        }
        assert!(
            matches!(&user.content[0], UserContent::RawImage { block, is_meta: true } if block == &image)
        );
        let mut nested = crate::utils::messages::create_user_message(String::new());
        nested.content = vec![UserContent::ToolResult(crate::types::message::ToolResult {
            tool_use_id: crate::types::ids::ToolUseId("tool".to_string()),
            content: String::new(),
            is_error: false,
            content_blocks: vec![ToolResultContentBlock::RawImage(image.clone())],
            tool_use_result: None,
        })];
        let sdk =
            local_message_param_to_sdk(&user_message_to_message_param(&nested, false, false, None))
                .unwrap();
        assert_eq!(
            serde_json::to_value(sdk).unwrap()["content"][0]["content"][0]["source"],
            image["source"]
        );
    }

    #[test]
    fn raw_image_media_limit_matches_official_oldest_first_including_meta() {
        let mut old = crate::utils::messages::create_user_message("old".to_string());
        old.content.push(UserContent::RawImage { block: serde_json::json!({"type":"image","source":{"type":"url","url":"https://example.test/old"}}), is_meta: true });
        let mut recent = crate::utils::messages::create_user_message("recent".to_string());
        recent.content.push(UserContent::RawImage { block: serde_json::json!({"type":"image","source":{"type":"url","url":"https://example.test/recent"}}), is_meta: false });
        let messages =
            strip_excess_media_items(&[(Message::User(old),), (Message::User(recent),)], 1);
        assert_eq!(count_typed_message_media(&messages[0]), 0);
        assert_eq!(count_typed_message_media(&messages[1]), 1);
    }

    #[test]
    fn extra_body_and_metadata_match_official_json_owner_bom_and_cache() {
        // CC services/api/claude.ts:272–297,503–519, utils/json.ts:31–58.
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _body = EnvVarGuard::set(
            "CLAUDE_CODE_EXTRA_BODY",
            "\u{feff}{\"custom_json_owner\":true}",
        );
        let _metadata = EnvVarGuard::set(
            "CLAUDE_CODE_EXTRA_METADATA",
            "\u{feff}{\"json_owner_metadata\":123}",
        );
        crate::utils::json::SAFE_PARSE_JSON_CACHE.clear();
        let mut body = get_extra_body_params(None);
        assert_eq!(
            body.get("custom_json_owner"),
            Some(&serde_json::json!(true))
        );
        body.insert("custom_json_owner".into(), serde_json::json!(false));
        assert_eq!(
            get_extra_body_params(None).get("custom_json_owner"),
            Some(&serde_json::json!(true))
        );
        let metadata: JsonValue = serde_json::from_str(&get_api_metadata().user_id).unwrap();
        assert_eq!(
            metadata.get("json_owner_metadata"),
            Some(&serde_json::json!(123))
        );
    }
}
