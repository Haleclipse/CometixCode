//! Maps to: CC `utils/sideQuery.ts`.
//!
//! Lightweight API wrapper for "side queries" outside the main conversation loop.
//!
//! Use this instead of direct `client.messages.create()` / `client.beta.messages.create()`
//! calls to ensure proper OAuth token validation with fingerprint attribution headers.
//!
//! This handles (same list as CC JSDoc on `sideQuery`):
//! - Fingerprint computation for OAuth validation
//! - Attribution header injection
//! - CLI system prompt prefix
//! - Proper betas for the model
//! - API metadata
//! - Model string normalization (strips `[1m]` suffix for API)
//!
//! # Examples (CC call-site shapes)
//!
//! ```ignore
//! // Permission explainer
//! side_query(SideQueryOptions {
//!     query_source: "permission_explainer".into(),
//!     model, system: Some(...), messages, tools, tool_choice: Some(...),
//!     ..Default::default()
//! }).await?;
//!
//! // Session search
//! side_query(SideQueryOptions {
//!     query_source: "session_search".into(),
//!     model, system: Some(...), messages,
//!     ..Default::default()
//! }).await?;
//!
//! // Model validation
//! side_query(SideQueryOptions {
//!     query_source: "model_validation".into(),
//!     model, max_tokens: Some(1),
//!     messages: vec![user_text_message("Hi")],
//!     ..Default::default()
//! }).await?;
//! ```

use crate::constants::system::{get_attribution_header, get_cli_sysprompt_prefix};
use crate::services::api::claude::get_api_metadata;
use crate::utils::fingerprint::compute_fingerprint;
use crate::utils::model::model::normalize_model_string_for_api;
use anthropic_sdk::resources::beta::messages::{
    BetaMessageContent, BetaMessageCreateParams, BetaMessageParam, BetaToolUnion,
};
use anthropic_sdk::resources::messages::{
    Message, MessageContent, MessageParam, Metadata, SystemPrompt, TextBlockParam, ThinkingConfig,
    Tool, ToolChoice, ToolUnion,
};

/// Maps to: CC `SideQueryOptions` (`utils/sideQuery.ts`:29-64).
///
/// Field docs below track the official TS property comments 1:1; do not strip
/// them when refactoring this struct.
#[derive(Clone, Debug)]
pub struct SideQueryOptions {
    /// Model to use for the query.
    /// Maps to: CC `model: string`.
    pub model: String,
    /// System prompt — string or array of text blocks (will be prefixed with CLI
    /// attribution).
    ///
    /// The attribution header is always placed in its own `TextBlockParam` block
    /// to ensure server-side parsing correctly extracts the `cc_entrypoint` value
    /// without including system prompt content.
    /// Maps to: CC `system?: string | TextBlockParam[]`.
    pub system: Option<SideQuerySystem>,
    /// Messages to send (supports cache_control on content blocks).
    /// Maps to: CC `messages: MessageParam[]`.
    pub messages: Vec<MessageParam>,
    /// Optional tools (supports both standard Tool[] and BetaToolUnion[] for
    /// custom tool types).
    /// Maps to: CC `tools?: Tool[] | BetaToolUnion[]`.
    pub tools: Option<Vec<ToolUnion>>,
    /// Optional tool choice (use `{ type: 'tool', name: 'x' }` for forced output).
    /// Maps to: CC `tool_choice?: ToolChoice`.
    pub tool_choice: Option<ToolChoice>,
    /// Optional JSON output format for structured responses.
    /// Maps to: CC `output_format?: BetaJSONOutputFormat`.
    /// Not yet wired into `MessageCreateParams.output_config` (follow-up).
    pub output_format: Option<serde_json::Value>,
    /// Max tokens (default: 1024).
    /// Maps to: CC `max_tokens?: number` (default 1024).
    pub max_tokens: Option<i64>,
    /// Max retries (default: 2).
    /// Maps to: CC `maxRetries?: number` (default 2).
    pub max_retries: Option<u32>,
    /// Abort signal.
    /// Maps to: CC `signal?: AbortSignal` → SDK `RequestOptions.signal`.
    pub signal: Option<anthropic_sdk::AbortSignal>,
    /// Skip CLI system prompt prefix (keeps attribution header for OAuth).
    /// For internal classifiers that provide their own prompt.
    /// Maps to: CC `skipSystemPromptPrefix?: boolean`.
    pub skip_system_prompt_prefix: bool,
    /// Maps to: CC `getCLISyspromptPrefix({ isNonInteractive })` when prefix is not skipped.
    pub is_non_interactive: bool,
    /// Maps to: CC `getCLISyspromptPrefix({ hasAppendSystemPrompt })`.
    pub has_append_system_prompt: bool,
    /// Temperature override.
    /// Maps to: CC `temperature?: number`.
    pub temperature: Option<f64>,
    /// Thinking budget (enables thinking), or `Disabled` to send `{ type: 'disabled' }`.
    /// Maps to: CC `thinking?: number | false`.
    pub thinking: Option<SideQueryThinking>,
    /// Stop sequences — generation stops when any of these strings is emitted.
    /// Maps to: CC `stop_sequences?: string[]`.
    pub stop_sequences: Option<Vec<String>>,
    /// Attributes this call in tengu_api_success for COGS joining against
    /// reporting.sampling_calls.
    /// Maps to: CC `querySource: QuerySource` (required in TS).
    pub query_source: String,
}

/// System prompt input: string or array of text blocks.
/// Maps to: CC `system?: string | TextBlockParam[]`.
#[derive(Clone, Debug)]
pub enum SideQuerySystem {
    Text(String),
    Blocks(Vec<TextBlockParam>),
}

/// Thinking override for side queries.
/// Maps to: CC `thinking?: number | false`.
#[derive(Clone, Debug)]
pub enum SideQueryThinking {
    /// Send `{ type: 'disabled' }` (CC `thinking: false`).
    Disabled,
    /// Enable with budget tokens (CC `thinking: number`).
    Enabled { budget_tokens: i64 },
}

impl Default for SideQueryOptions {
    fn default() -> Self {
        Self {
            model: String::new(),
            system: None,
            messages: Vec::new(),
            tools: None,
            tool_choice: None,
            output_format: None,
            max_tokens: None,
            max_retries: None,
            signal: None,
            skip_system_prompt_prefix: false,
            is_non_interactive: false,
            has_append_system_prompt: false,
            temperature: None,
            thinking: None,
            stop_sequences: None,
            query_source: "side_query".to_string(),
        }
    }
}

/// Build beta list for a side_query request.
/// Maps to: CC `sideQuery` `betas` construction (+ STRUCTURED_OUTPUTS when applicable).
pub fn side_query_betas_for_request(model: &str, has_output_format: bool) -> Vec<String> {
    use crate::utils::betas::{
        STRUCTURED_OUTPUTS_BETA_HEADER, get_model_betas, model_supports_structured_outputs,
    };
    let mut betas = get_model_betas(model);
    if has_output_format
        && model_supports_structured_outputs(model)
        && !betas.iter().any(|b| b == STRUCTURED_OUTPUTS_BETA_HEADER)
    {
        betas.push(STRUCTURED_OUTPUTS_BETA_HEADER.to_string());
    }
    betas
}

/// Extract text from first user message for fingerprint computation.
/// Maps to: CC `extractFirstUserMessageText` (`sideQuery.ts`:69-79).
fn extract_first_user_message_text(messages: &[MessageParam]) -> String {
    let Some(first_user) = messages.iter().find(|m| m.role == "user") else {
        return String::new();
    };
    match &first_user.content {
        MessageContent::Text(text) => text.clone(),
        MessageContent::Blocks(blocks) => blocks
            .iter()
            .find_map(|block| match block {
                anthropic_sdk::resources::messages::ContentBlockParam::Text(t) => {
                    Some(t.text.clone())
                }
                _ => None,
            })
            .unwrap_or_default(),
    }
}

fn text_block(text: impl Into<String>) -> TextBlockParam {
    TextBlockParam {
        text: text.into(),
        cache_control: None,
        citations: None,
        type_name: Some("text".to_string()),
    }
}

/// Maps to: CC `sideQuery(opts): Promise<BetaMessage>` (`sideQuery.ts`:107-222).
///
/// This IS the wrapper that handles OAuth attribution (CC biome-ignore note:
/// do not bypass this for side calls).
pub async fn side_query(opts: SideQueryOptions) -> anyhow::Result<Message> {
    // Defaults match CC destructuring: max_tokens = 1024, maxRetries = 2.
    let max_tokens = opts.max_tokens.unwrap_or(1024);
    let max_retries = opts.max_retries.unwrap_or(2);

    // Maps to: CC `getAnthropicClient({ maxRetries, model, source: 'side_query' })`.
    let client = crate::services::api::client::get_anthropic_client(
        crate::services::api::client::GetAnthropicClientOptions {
            api_key: None,
            max_retries,
            model: Some(opts.model.clone()),
            source: Some("side_query".to_string()),
        },
    )
    .await?
    .build()?;

    // Maps to: CC `const betas = [...getModelBetas(model)]` (+ structured-outputs
    // beta when output_format is set and modelSupportsStructuredOutputs).
    let betas = side_query_betas_for_request(&opts.model, opts.output_format.is_some());

    // Extract first user message text for fingerprint.
    let message_text = extract_first_user_message_text(&opts.messages);
    // Maps to: CC `computeFingerprint(messageText, MACRO.VERSION)`.
    let fingerprint = compute_fingerprint(&message_text, crate::constants::product::VERSION);
    let attribution_header = get_attribution_header(&fingerprint);

    // Build system as array to keep attribution header in its own block
    // (prevents server-side parsing from including system content in cc_entrypoint).
    // Maps to: CC systemBlocks construction (`sideQuery.ts`:148-167).
    let mut system_blocks: Vec<TextBlockParam> = Vec::new();
    if !attribution_header.is_empty() {
        system_blocks.push(text_block(attribution_header));
    }
    // Skip CLI system prompt prefix for internal classifiers that provide their own prompt.
    // Maps to: CC getCLISyspromptPrefix({ isNonInteractive, hasAppendSystemPrompt }).
    if !opts.skip_system_prompt_prefix {
        system_blocks.push(text_block(get_cli_sysprompt_prefix(
            opts.is_non_interactive,
            opts.has_append_system_prompt,
        )));
    }
    match &opts.system {
        Some(SideQuerySystem::Text(text)) => system_blocks.push(text_block(text.clone())),
        Some(SideQuerySystem::Blocks(blocks)) => system_blocks.extend(blocks.iter().cloned()),
        None => {}
    }

    // Maps to: CC thinkingConfig from `thinking === false` / `thinking !== undefined`.
    let thinking_config = match &opts.thinking {
        Some(SideQueryThinking::Disabled) => Some(ThinkingConfig::Disabled),
        Some(SideQueryThinking::Enabled { budget_tokens }) => Some(ThinkingConfig::Enabled {
            // CC: budget_tokens: Math.min(thinking, max_tokens - 1)
            budget_tokens: (*budget_tokens).min(max_tokens.saturating_sub(1)),
        }),
        None => None,
    };

    // Maps to: CC `normalizeModelStringForAPI(model)`.
    let normalized_model = normalize_model_string_for_api(&opts.model);
    // Maps to: CC `metadata: getAPIMetadata()`.
    let meta = get_api_metadata();

    // Maps to: CC `client.beta.messages.create({ ..., betas }, { signal })`.
    let mut messages = Vec::with_capacity(opts.messages.len());
    for message in &opts.messages {
        messages.push(BetaMessageParam {
            role: message.role.clone(),
            content: match &message.content {
                MessageContent::Text(text) => BetaMessageContent::Text(text.clone()),
                MessageContent::Blocks(blocks) => BetaMessageContent::Blocks(serde_json::from_value(
                    serde_json::to_value(blocks)?,
                )?),
            },
        });
    }
    let params = BetaMessageCreateParams {
        max_tokens,
        messages,
        model: normalized_model,
        betas: (!betas.is_empty()).then_some(betas),
        metadata: Some(Metadata {
            user_id: Some(meta.user_id),
        }),
        // Maps to: CC `...(output_format && { output_config: { format: output_format } })`.
        output_config: opts.output_format.as_ref().map(|schema| {
            anthropic_sdk::resources::messages::OutputConfig {
                effort: None,
                format: Some(anthropic_sdk::resources::messages::JsonOutputFormat {
                    schema: schema.clone(),
                    type_name: "json_schema".to_string(),
                }),
            }
        }),
        stop_sequences: opts.stop_sequences.clone(),
        stream: Some(false),
        system: if system_blocks.is_empty() {
            None
        } else {
            Some(SystemPrompt::Blocks(system_blocks))
        },
        temperature: opts.temperature,
        thinking: thinking_config,
        tool_choice: opts.tool_choice.clone(),
        tools: opts
            .tools
            .as_ref()
            .map(|tools| tools.iter().cloned().map(BetaToolUnion::Base).collect()),
        ..Default::default()
    };

    let start = std::time::Instant::now();
    let response = client
        .beta()
        .messages()
        .create_with_options(
            &params,
            Some(&anthropic_sdk::RequestOptions {
                // CC passes only `{ signal }` here: inherit the client retry
                // budget. A request override of zero disables retries entirely.
                max_retries: None,
                signal: opts.signal.clone(),
                ..Default::default()
            }),
        )
        .await
        .map_err(|e| anyhow::Error::new(e).context("side_query failed"))?;

    // Maps to: CC `logEvent('tengu_api_success', { requestId, querySource, model, ... })`
    // + `setLastApiCompletionTimestamp(now)`. Full analytics wiring is a follow-up;
    // duration/query_source kept so the hook site is obvious.
    let _ = (start.elapsed(), &opts.query_source);

    serde_json::from_value(serde_json::to_value(response)?)
        .map_err(|error| anyhow::anyhow!("side_query response: {error}"))
}

/// Build a custom `ToolUnion` from name + JSON schema (e.g. classifier tools).
/// Convenience for CC call sites that build `BetaToolUnion` / custom Tool inline.
pub fn custom_tool(name: &str, description: &str, input_schema: serde_json::Value) -> ToolUnion {
    ToolUnion::Custom(Tool {
        name: name.to_string(),
        description: Some(description.to_string()),
        input_schema,
        cache_control: None,
        eager_input_streaming: None,
        strict: None,
        type_name: Some("custom".to_string()),
    })
}

/// Forced tool choice by name. Maps to CC `{ type: 'tool', name }`.
pub fn tool_choice_tool(name: &str) -> ToolChoice {
    ToolChoice::Tool {
        name: name.to_string(),
        disable_parallel_tool_use: None,
    }
}

/// Convenience: user message with plain text content.
pub fn user_text_message(text: impl Into<String>) -> MessageParam {
    MessageParam {
        role: "user".to_string(),
        content: MessageContent::Text(text.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    /// A real side_query -> canonical client -> SDK HTTP fixture. Only the
    /// endpoint/auth environment is substituted; retries and aborts use the SDK.
    struct HttpFixture {
        root: std::path::PathBuf,
        original_cwd: std::path::PathBuf,
        _env: Vec<EnvVarGuard>,
        requests: tokio::sync::mpsc::UnboundedReceiver<(u32, serde_json::Value)>,
        server: tokio::task::JoinHandle<()>,
    }

    impl HttpFixture {
        /// None holds the HTTP response open until the caller aborts.
        async fn new(statuses: Vec<Option<u16>>) -> Self {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            crate::utils::tls_provider::install_crypto_provider();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let root = std::env::temp_dir().join(format!("cc-side-query-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&root).unwrap();
            let original_cwd = crate::bootstrap::state::get_original_cwd();
            crate::bootstrap::state::set_original_cwd(&root);
            let mut env = vec![
                EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root),
                EnvVarGuard::set("CLAUDE_CODE_SIMPLE", "1"),
                EnvVarGuard::set("ANTHROPIC_API_KEY", "sk-ant-local-side-query-test"),
                EnvVarGuard::set("ANTHROPIC_BASE_URL", format!("http://{address}")),
                EnvVarGuard::set("API_TIMEOUT_MS", "5000"),
                EnvVarGuard::set("NO_PROXY", "*"),
                EnvVarGuard::set("no_proxy", "*"),
            ];
            for key in [
                "CLAUDE_CODE_USE_BEDROCK",
                "CLAUDE_CODE_USE_VERTEX",
                "CLAUDE_CODE_USE_FOUNDRY",
                "ANTHROPIC_AUTH_TOKEN",
                "ANTHROPIC_CUSTOM_HEADERS",
                "ANTHROPIC_UNIX_SOCKET",
                "USE_STAGING_OAUTH",
            ] {
                env.push(EnvVarGuard::unset(key));
            }
            crate::utils::settings::settings_cache::reset_settings_cache();
            let (tx, requests) = tokio::sync::mpsc::unbounded_channel();
            let server = tokio::spawn(async move {
                tokio::time::timeout(std::time::Duration::from_secs(10), async move {
                    for status in statuses {
                        let (mut stream, _) = listener.accept().await.unwrap();
                        let mut request = Vec::new();
                        let header_end = loop {
                            let mut buffer = [0;4096];
                            let count = stream.read(&mut buffer).await.unwrap();
                            assert_ne!(count,0,"HTTP request ended before headers");
                            request.extend_from_slice(&buffer[..count]);
                            if let Some(end) = request.windows(4).position(|bytes|bytes == b"\r\n\r\n") {
                                break end + 4;
                            }
                        };
                        let headers = String::from_utf8_lossy(&request[..header_end]);
                        assert!(headers.starts_with("POST /v1/messages?beta=true"));
                        let header = |name:&str| headers.lines().find_map(|line| {
                            let (key,value) = line.split_once(':')?;
                            key.eq_ignore_ascii_case(name).then(||value.trim())
                        });
                        let length:usize = header("content-length").unwrap().parse().unwrap();
                        let retry_count:u32 = header("x-stainless-retry-count").unwrap().parse().unwrap();
                        while request.len() - header_end < length {
                            let mut buffer = [0;4096];
                            let count = stream.read(&mut buffer).await.unwrap();
                            assert_ne!(count,0,"HTTP request ended before body");
                            request.extend_from_slice(&buffer[..count]);
                        }
                        let body = serde_json::from_slice(&request[header_end..header_end + length]).unwrap();
                        tx.send((retry_count,body)).unwrap();
                        let Some(status) = status else {
                            // Keep the real request pending; the abort test
                            // cancels only after receiving its wire payload.
                            std::future::pending::<()>().await;
                            unreachable!();
                        };
                        let body = if status == 200 {
                            serde_json::json!({"id":"msg_side_retry","type":"message","role":"assistant",
                                "model":"claude-sonnet-4-6","content":[{"type":"text","text":"retried successfully"}],
                                "container":null,"context_management":null,
                                "stop_reason":"end_turn","stop_sequence":null,"usage":{"input_tokens":1,"output_tokens":2}})
                        } else {
                            serde_json::json!({"type":"error","error":{"type":"api_error","message":"local retry fixture"}})
                        }.to_string();
                        let reason = if status == 200 { "OK" } else { "Internal Server Error" };
                        stream.write_all(format!("HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\nretry-after-ms: 1\r\nrequest-id: req_side_fixture\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
                    }
                }).await.expect("local side-query server deadline");
            });
            Self {
                root,
                original_cwd,
                _env: env,
                requests,
                server,
            }
        }
    }

    impl Drop for HttpFixture {
        fn drop(&mut self) {
            self.server.abort();
            crate::bootstrap::state::set_original_cwd(&self.original_cwd);
            crate::utils::settings::settings_cache::reset_settings_cache();
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// CC sideQuery.ts:115-133,178-200: the client owns maxRetries and the
    /// request inherits it. This exercises real failed/successful HTTP attempts.
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn side_query_matches_official_client_retry_budget_on_http_wire() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        for (max_retries, failures) in [(None, 2usize), (Some(1), 1), (Some(0), 0)] {
            let statuses = if max_retries == Some(0) {
                vec![Some(500)]
            } else {
                let mut statuses = vec![Some(500); failures];
                statuses.push(Some(200));
                statuses
            };
            let attempts = statuses.len();
            let mut fixture = HttpFixture::new(statuses).await;
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(10),
                side_query(SideQueryOptions {
                    model: "claude-sonnet-4-6".into(),
                    messages: vec![user_text_message("retry wire sentinel")],
                    max_retries,
                    ..Default::default()
                }),
            )
            .await
            .expect("side-query retry deadline");
            if max_retries == Some(0) {
                let error = result.expect_err("zero retries must expose the first HTTP failure");
                assert!(matches!(error.downcast_ref::<anthropic_sdk::ApiError>(),
                    Some(anthropic_sdk::ApiError::InternalServerError { status:500,request_id:Some(id),.. }) if id == "req_side_fixture"));
            } else {
                let response =
                    result.expect("configured retries must reach the successful response");
                assert_eq!(response.id, "msg_side_retry");
                assert_eq!(
                    serde_json::to_value(response).unwrap()["content"][0]["text"],
                    "retried successfully"
                );
            }
            (&mut fixture.server)
                .await
                .expect("local HTTP server completed");
            for index in 0..attempts {
                let (retry_count, body) = fixture.requests.try_recv().expect("actual HTTP attempt");
                assert_eq!(retry_count, index as u32);
                assert_eq!(body["messages"][0]["content"], "retry wire sentinel");
            }
            assert!(fixture.requests.try_recv().is_err());
        }
    }

    /// The original await propagates SDK APIUserAbortError identity; wrapping
    /// it as a string would break downstream classifier cancellation checks.
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn side_query_matches_official_live_abort_error_identity() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let mut fixture = HttpFixture::new(vec![None]).await;
        let (handle, signal) = anthropic_sdk::AbortSignal::pair();
        let query = side_query(SideQueryOptions {
            model: "claude-sonnet-4-6".into(),
            messages: vec![user_text_message("abort wire sentinel")],
            signal: Some(signal),
            ..Default::default()
        });
        let cancellation = async {
            let (retry_count, body) = fixture.requests.recv().await.expect("live HTTP request");
            assert_eq!(retry_count, 0);
            assert_eq!(body["messages"][0]["content"], "abort wire sentinel");
            handle.abort();
        };
        let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            tokio::join!(query, cancellation)
        })
        .await
        .expect("side-query cancellation deadline");
        let error = result.expect_err("the pending HTTP request must be aborted");
        assert!(matches!(error.downcast_ref::<anthropic_sdk::ApiError>(),
            Some(anthropic_sdk::ApiError::UserAbort { message }) if message == "Request was aborted."));
        assert!(fixture.requests.try_recv().is_err(), "abort must not retry");
    }

    #[test]
    fn extract_first_user_message_text_from_string_content() {
        let messages = vec![user_text_message("hello classifier")];
        assert_eq!(
            extract_first_user_message_text(&messages),
            "hello classifier"
        );
    }

    #[test]
    fn custom_tool_helper_sets_name_and_schema() {
        let tool = custom_tool(
            "classify_result",
            "Report classification",
            serde_json::json!({"type": "object"}),
        );
        match tool {
            ToolUnion::Custom(t) => {
                assert_eq!(t.name, "classify_result");
                assert!(t.description.is_some());
            }
            _ => panic!("expected Custom tool"),
        }
    }

    #[test]
    fn structured_outputs_beta_pushed_only_when_format_and_model_support() {
        use crate::utils::betas::STRUCTURED_OUTPUTS_BETA_HEADER;
        let with_format = side_query_betas_for_request("claude-sonnet-4-6", true);
        // Support depends on provider env; if supported, header must appear.
        if crate::utils::betas::model_supports_structured_outputs("claude-sonnet-4-6") {
            assert!(
                with_format
                    .iter()
                    .any(|b| b == STRUCTURED_OUTPUTS_BETA_HEADER),
                "betas={with_format:?}"
            );
        }
        let without = side_query_betas_for_request("claude-sonnet-4-6", false);
        // When no output_format, we must not push solely for this request shape
        // beyond what get_model_betas already includes.
        let only_from_format = with_format
            .iter()
            .filter(|b| *b == STRUCTURED_OUTPUTS_BETA_HEADER)
            .count()
            > without
                .iter()
                .filter(|b| *b == STRUCTURED_OUTPUTS_BETA_HEADER)
                .count();
        if crate::utils::betas::model_supports_structured_outputs("claude-sonnet-4-6") {
            assert!(
                only_from_format || without.iter().any(|b| b == STRUCTURED_OUTPUTS_BETA_HEADER)
            );
        }
        // Never use the outdated hardcoded date.
        assert!(!with_format.iter().any(|b| b.contains("2025-11-13")));
    }
}
