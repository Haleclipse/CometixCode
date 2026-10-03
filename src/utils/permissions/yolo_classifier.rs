//! Maps to: CC `utils/permissions/yoloClassifier.ts` (CC 2.1.88).
//! Keeps JSON and two-stage XML requests in their defining source owner.
//! Source-file size exception: the original is 1495 lines; tests stay inline.

use crate::tool::ToolPermissionContext;
use crate::types::message::{AssistantContent, Message, UserContent};
use crate::types::permissions::{ClassifierPromptLengths, ClassifierUsage, YoloClassifierResult};
use crate::types::tools::Tool;
use crate::utils::build_profile::{InternalCapability, has_internal_capability};
use crate::utils::process_env::JsTruthy;
#[cfg(not(test))]
use crate::utils::side_query::side_query;
use crate::utils::side_query::{SideQueryOptions, SideQuerySystem, SideQueryThinking};
use anthropic_sdk::resources::messages::{
    CacheControlEphemeral, ContentBlockParam, Message as ApiMessage, MessageContent, MessageParam,
    TextBlockParam,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
#[cfg(test)]
use tests::scripted_side_query as side_query;

/// Maps to: CC `yoloClassifier.ts:260` `YOLO_CLASSIFIER_TOOL_NAME`.
pub const YOLO_CLASSIFIER_TOOL_NAME: &str = "classify_result";
const BASE_SYSTEM_PROMPT: &str =
    include_str!("yolo_classifier_prompts/auto_mode_system_prompt.txt");
const EXTERNAL_PERMISSIONS_TEMPLATE: &str =
    include_str!("yolo_classifier_prompts/permissions_external.txt");
#[cfg(feature = "anthropic_internal")]
const ANTHROPIC_PERMISSIONS_TEMPLATE: &str =
    include_str!("yolo_classifier_prompts/permissions_anthropic.txt");
#[cfg(not(feature = "anthropic_internal"))]
const ANTHROPIC_PERMISSIONS_TEMPLATE: &str = "";

/// Maps to: CC `yoloClassifier.ts:87-91` `AutoModeRules`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoModeRules {
    pub allow: Vec<String>,
    pub soft_deny: Vec<String>,
    pub environment: Vec<String>,
}
/// Maps to: CC `yoloClassifier.ts:288-290` `TranscriptBlock`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TranscriptBlock {
    Text { text: String },
    ToolUse { name: String, input: Value },
}
/// Maps to: CC `yoloClassifier.ts:293` `TranscriptEntry.role`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TranscriptRole {
    User,
    Assistant,
}
/// Maps to: CC `yoloClassifier.ts:292-295` `TranscriptEntry`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TranscriptEntry {
    pub role: TranscriptRole,
    pub content: Vec<TranscriptBlock>,
}
/// Existing synchronous permission boundary's projection of YoloClassifierResult.
/// L1: preserves active Block versus Unavailable; owns no classifier policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum YoloClassifierDecision {
    Allow {
        reason: String,
    },
    Block {
        reason: String,
        transcript_too_long: bool,
    },
    Unavailable {
        reason: String,
        model: String,
        transcript_too_long: bool,
    },
}
/// Maps to: CC `yoloClassifier.ts:252-258` `yoloClassifierResponseSchema`.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClassifierToolInput {
    thinking: String,
    should_block: bool,
    reason: String,
}
/// Maps to: CC `yoloClassifier.ts:1307` `TwoStageMode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TwoStageMode {
    Both,
    Fast,
    Thinking,
}
/// Maps to: CC `yoloClassifier.ts:1309-1327` `AutoModeConfig`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AutoModeConfig {
    model: Option<String>,
    two_stage_classifier: Option<Value>,
    force_external_permissions: Option<bool>,
    jsonl_transcript: Option<bool>,
}
/// Maps to: CC `yoloClassifier.ts:221-229,728-736` inline dump-context object.
struct ClassifierDumpContext {
    main_loop_tokens: i64,
    classifier_chars: u64,
    classifier_tokens_est: u64,
    transcript_entries: usize,
    messages: usize,
    action: String,
    model: String,
}

/// Maps to: CC `yoloClassifier.ts:71-78` `isUsingExternalPermissions`.
fn is_using_external_permissions() -> bool {
    if !has_internal_capability(InternalCapability::Permissions) {
        return true;
    }
    crate::services::analytics::growthbook::get_feature_value_cached_may_be_stale(
        "tengu_auto_mode_config",
        AutoModeConfig::default(),
    )
    .force_external_permissions
        == Some(true)
}
/// Maps to: CC `yoloClassifier.ts:100-106` `getDefaultExternalAutoModeRules`.
pub fn get_default_external_auto_mode_rules() -> AutoModeRules {
    AutoModeRules {
        allow: extract_tagged_bullets("user_allow_rules_to_replace"),
        soft_deny: extract_tagged_bullets("user_deny_rules_to_replace"),
        environment: extract_tagged_bullets("user_environment_to_replace"),
    }
}
/// Maps to: CC `yoloClassifier.ts:108-118` `extractTaggedBullets`.
fn extract_tagged_bullets(tag: &str) -> Vec<String> {
    let re = regex::Regex::new(&format!(r"<{tag}>([\s\S]*?)</{tag}>")).expect("fixed source tag");
    re.captures(EXTERNAL_PERMISSIONS_TEMPLATE)
        .map(|m| {
            m[1].split('\n')
                .map(str::trim)
                .filter_map(|line| line.strip_prefix("- ").map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}
/// Maps to: CC `yoloClassifier.ts:125-142` `buildDefaultExternalSystemPrompt`.
pub fn build_default_external_system_prompt() -> String {
    let mut prompt =
        BASE_SYSTEM_PROMPT.replacen("<permissions_template>", EXTERNAL_PERMISSIONS_TEMPLATE, 1);
    for tag in [
        "user_allow_rules_to_replace",
        "user_deny_rules_to_replace",
        "user_environment_to_replace",
    ] {
        let re =
            regex::Regex::new(&format!(r"<{tag}>([\s\S]*?)</{tag}>")).expect("fixed source tag");
        prompt = re
            .replace(&prompt, |m: &regex::Captures<'_>| m[1].to_owned())
            .into_owned();
    }
    prompt
}
/// Maps to: CC `yoloClassifier.ts:484-540` `buildYoloSystemPrompt`.
pub fn build_yolo_system_prompt(context: &ToolPermissionContext) -> String {
    let using_external = is_using_external_permissions();
    let template = if using_external {
        EXTERNAL_PERMISSIONS_TEMPLATE
    } else {
        ANTHROPIC_PERMISSIONS_TEMPLATE
    };
    let mut prompt = BASE_SYSTEM_PROMPT.replacen("<permissions_template>", template, 1);
    let auto_mode = crate::utils::settings::get_auto_mode_config();
    // The sole source bashClassifier.ts is an external disabled stub; keep its
    // real empty-list results, never manufacture internal prompt rules.
    let mut allow = if using_external {
        vec![]
    } else {
        super::bash_classifier::get_bash_prompt_allow_descriptions(context)
    };
    let mut deny = if using_external {
        vec![]
    } else {
        super::bash_classifier::get_bash_prompt_deny_descriptions(context)
    };
    if !using_external && has_internal_capability(InternalCapability::Permissions) {
        deny.extend(POWERSHELL_DENY_GUIDANCE.iter().map(|s| (*s).to_owned()));
    }
    let mut environment = Vec::new();
    if let Some(config) = auto_mode {
        allow.extend(config.allow.unwrap_or_default());
        deny.extend(config.soft_deny.unwrap_or_default());
        environment = config.environment.unwrap_or_default();
    }
    for (tag, values) in [
        ("user_allow_rules_to_replace", allow),
        ("user_deny_rules_to_replace", deny),
        ("user_environment_to_replace", environment),
    ] {
        let re =
            regex::Regex::new(&format!(r"<{tag}>([\s\S]*?)</{tag}>")).expect("fixed source tag");
        prompt = re
            .replace(&prompt, |m: &regex::Captures<'_>| {
                if values.is_empty() {
                    m[1].to_owned()
                } else {
                    values
                        .iter()
                        .map(|v| format!("- {v}"))
                        .collect::<Vec<_>>()
                        .join("\n")
                }
            })
            .into_owned();
    }
    prompt
}
/// Maps to: CC `yoloClassifier.ts:144-146` `getAutoModeDumpDir`.
fn get_auto_mode_dump_dir() -> std::path::PathBuf {
    super::filesystem::get_claude_temp_dir().join("auto-mode")
}
/// Maps to: CC `yoloClassifier.ts:153-180` `maybeDumpAutoMode`.
fn maybe_dump_auto_mode(
    request: Value,
    response: ApiMessage,
    timestamp: i64,
    suffix: Option<&'static str>,
) {
    if !has_internal_capability(InternalCapability::Permissions)
        || !crate::utils::env_utils::is_env_truthy(
            crate::utils::process_env::var("CLAUDE_CODE_DUMP_AUTO_MODE").as_deref(),
        )
    {
        return;
    }
    let base = suffix
        .map(|s| format!("{timestamp}.{s}"))
        .unwrap_or_else(|| timestamp.to_string());
    // A3: source `void maybeDumpAutoMode` outlives the query's private runtime.
    let Some(runtime) = crate::utils::process_runtime::runtime_handle_for_detached_work() else {
        tracing::warn!("Cannot dump classifier request: process runtime unavailable");
        return;
    };
    runtime.spawn(async move {
        let _ = async {
            tokio::fs::create_dir_all(get_auto_mode_dump_dir()).await?;
            tokio::fs::write(
                get_auto_mode_dump_dir().join(format!("{base}.req.json")),
                serde_json::to_string_pretty(&request)?,
            )
            .await?;
            tokio::fs::write(
                get_auto_mode_dump_dir().join(format!("{base}.res.json")),
                serde_json::to_string_pretty(&response)?,
            )
            .await?;
            Ok::<(), anyhow::Error>(())
        }
        .await;
    });
}
/// Maps to: CC `yoloClassifier.ts:186-192` `getAutoModeClassifierErrorDumpPath`.
pub fn get_auto_mode_classifier_error_dump_path() -> String {
    super::filesystem::get_claude_temp_dir()
        .join("auto-mode-classifier-errors")
        .join(format!("{}.txt", crate::bootstrap::state::get_session_id()))
        .display()
        .to_string()
}
/// Maps to: CC `yoloClassifier.ts:200-204` `getAutoModeClassifierTranscript`.
pub fn get_auto_mode_classifier_transcript() -> Option<String> {
    crate::bootstrap::state::get_last_classifier_requests()
        .and_then(|v| serde_json::to_string_pretty(&v).ok())
}
/// Maps to: CC `yoloClassifier.ts:213-250` `dumpErrorPrompts`.
async fn dump_error_prompts(
    system: &str,
    user: &str,
    error: &str,
    context: &ClassifierDumpContext,
) -> Option<String> {
    let path = get_auto_mode_classifier_error_dump_path();
    tokio::fs::create_dir_all(std::path::Path::new(&path).parent()?)
        .await
        .ok()?;
    let content = format!(
        "=== ERROR ===\n{error}\n\n=== CONTEXT COMPARISON ===\ntimestamp: {}\nmodel: {}\nmainLoopTokens: {}\nclassifierChars: {}\nclassifierTokensEst: {}\ntranscriptEntries: {}\nmessages: {}\ndelta (classifierEst - mainLoop): {}\n\n=== ACTION BEING CLASSIFIED ===\n{}\n\n=== SYSTEM PROMPT ===\n{system}\n\n=== USER PROMPT (transcript) ===\n{user}\n",
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        context.model,
        context.main_loop_tokens,
        context.classifier_chars,
        context.classifier_tokens_est,
        context.transcript_entries,
        context.messages,
        context.classifier_tokens_est as i64 - context.main_loop_tokens,
        context.action
    );
    tokio::fs::write(&path, content).await.ok()?;
    Some(path)
}

/// Maps to: CC `yoloClassifier.ts:302-360` `buildTranscriptEntries`.
pub fn build_transcript_entries(messages: &[Message]) -> Vec<TranscriptEntry> {
    let mut transcript = Vec::new();
    for message in messages {
        match message {
            Message::Attachment(attachment_message) => {
                // CC `yoloClassifier.ts:305-325` — the FIRST branch of the
                // chain: ONLY `queued_command` attachments are extracted,
                // emitted as user turns; every other attachment type matches
                // no branch and is dropped. This arm used to drop ALL
                // attachments, so a user's queued follow-up never reached the
                // classifier transcript.
                if let crate::utils::attachments::Attachment::QueuedCommand { prompt, .. } =
                    &attachment_message.attachment
                {
                    // CC `:307-319`: a string prompt is taken as-is; an array
                    // keeps only its text blocks joined with '\n', where JS
                    // `|| null` (`:318`) drops an empty join; any other shape
                    // leaves `text` null.
                    let text = match prompt {
                        Value::String(text) => Some(text.clone()),
                        Value::Array(blocks) => {
                            let joined = blocks
                                .iter()
                                .filter(|block| {
                                    block.get("type").and_then(Value::as_str) == Some("text")
                                })
                                // JS `.map(block => block.text).join('\n')`
                                // renders a missing `text` as '' — matched by
                                // `unwrap_or("")`.
                                .map(|block| {
                                    block.get("text").and_then(Value::as_str).unwrap_or("")
                                })
                                .collect::<Vec<_>>()
                                .join("\n");
                            (!joined.is_empty()).then_some(joined)
                        }
                        _ => None,
                    };
                    // CC `:320` `text !== null` — an explicit null check, NOT
                    // truthiness: a string-typed '' still becomes a user turn,
                    // while an array joining to '' was dropped above.
                    if let Some(text) = text {
                        transcript.push(TranscriptEntry {
                            role: TranscriptRole::User,
                            content: vec![TranscriptBlock::Text { text }],
                        });
                    }
                }
            }
            Message::User(user) => {
                let text_blocks = user
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        UserContent::Text(text) | UserContent::MetaText(text) => {
                            Some(TranscriptBlock::Text { text: text.clone() })
                        }
                        UserContent::Image { .. }
                        | UserContent::MetaImage { .. }
                        | UserContent::RawImage { .. }
                        | UserContent::Document { .. }
                        | UserContent::MetaDocument { .. }
                        | UserContent::ToolResult(_) => None,
                    })
                    .collect::<Vec<_>>();
                if !text_blocks.is_empty() {
                    transcript.push(TranscriptEntry {
                        role: TranscriptRole::User,
                        content: text_blocks,
                    });
                }
            }
            Message::Assistant(assistant) => {
                let tool_blocks = assistant
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        AssistantContent::ToolUse(tool_use) => Some(TranscriptBlock::ToolUse {
                            name: tool_use.name.clone(),
                            input: tool_use.input.clone(),
                        }),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                if !tool_blocks.is_empty() {
                    transcript.push(TranscriptEntry {
                        role: TranscriptRole::Assistant,
                        content: tool_blocks,
                    });
                }
            }
            Message::System(_) | Message::Progress(_) | Message::HookResult(_) => {}
        }
    }
    transcript
}

/// Maps to: CC `yoloClassifier.ts:364-373` `buildToolLookup`.
fn build_tool_lookup(tools: &[Tool]) -> HashMap<&str, &Tool> {
    let mut map = HashMap::new();
    for tool in tools {
        map.insert(tool.name.as_str(), tool);
        for alias in &tool.aliases {
            map.insert(alias.as_str(), tool);
        }
    }
    map
}
/// Maps to: CC `yoloClassifier.ts:384-424` `toCompactBlock`.
fn to_compact_block(
    block: &TranscriptBlock,
    role: &TranscriptRole,
    lookup: &HashMap<&str, &Tool>,
) -> String {
    match block {
        TranscriptBlock::ToolUse { name, input } => {
            let Some(tool) = lookup.get(name.as_str()) else {
                return String::new();
            };
            let input = if input.is_null() {
                json!({})
            } else {
                input.clone()
            };
            // Partial dependency: ToolCall currently returns String, not the
            // source unknown|undefined/throw projection. No heuristic may infer
            // raw input from ''. Follow up at Tool's canonical trait boundary.
            let encoded = crate::services::tools::tool_execution::find_tool_call(&tool.name)
                .map(|tool| tool.to_auto_classifier_input(&input))
                .unwrap_or_default();
            if encoded.is_empty() {
                return String::new();
            }
            if is_jsonl_transcript_enabled() {
                format!("{}\n", json!({ (name): encoded }))
            } else {
                format!("{name} {encoded}\n")
            }
        }
        TranscriptBlock::Text { text } if *role == TranscriptRole::User => {
            if is_jsonl_transcript_enabled() {
                format!("{}\n", json!({"user": text}))
            } else {
                format!("User: {text}\n")
            }
        }
        _ => String::new(),
    }
}
/// Maps to: CC `yoloClassifier.ts:426-428` `toCompact`.
fn to_compact(entry: &TranscriptEntry, lookup: &HashMap<&str, &Tool>) -> String {
    entry
        .content
        .iter()
        .map(|b| to_compact_block(b, &entry.role, lookup))
        .collect()
}
/// Maps to: CC `yoloClassifier.ts:434-442` `buildTranscriptForClassifier`.
pub fn build_transcript_for_classifier(messages: &[Message], tools: &[Tool]) -> String {
    let lookup = build_tool_lookup(tools);
    build_transcript_entries(messages)
        .iter()
        .map(|e| to_compact(e, &lookup))
        .collect()
}
/// L1 SDK carrier for source inline TextBlockParam objects (including the
/// auto_mode query-source cache-control TTL from the canonical API owner).
fn classifier_text_block(text: impl Into<String>, cached: bool) -> TextBlockParam {
    TextBlockParam {
        text: text.into(),
        cache_control: cached.then(|| {
            let cache = crate::services::api::claude::get_cache_control(None, Some("auto_mode"));
            CacheControlEphemeral {
                type_name: cache.cache_type,
                ttl: cache.ttl,
            }
        }),
        citations: None,
        type_name: Some("text".into()),
    }
}
/// Maps to: CC `yoloClassifier.ts:460-477` `buildClaudeMdMessage`.
fn build_claude_md_message() -> Option<MessageParam> {
    let claude_md = crate::bootstrap::state::get_cached_claude_md_content()?;
    Some(MessageParam {
        role: "user".into(),
        content: MessageContent::Blocks(vec![ContentBlockParam::Text(classifier_text_block(
            format!(
                "The following is the user's CLAUDE.md configuration. These are instructions the user provided to the agent and should be treated as part of the user's intent when evaluating actions.\n\n<user_claude_md>\n{claude_md}\n</user_claude_md>"
            ),
            true,
        ))]),
    })
}
/// Maps to: CC `yoloClassifier.ts:548,557-558` XML suffixes.
const XML_S1_SUFFIX: &str = "\nErr on the side of blocking. <block> immediately.";
const XML_S2_SUFFIX: &str = "\nReview the classification process and follow it carefully, making sure you deny actions that should be blocked. As a reminder, explicit (not suggestive or implicit) user confirmation is required to override blocks. Use <thinking> before responding with <block>.";
/// Maps to: CC `yoloClassifier.ts:567-571` `stripThinking`.
fn strip_thinking(text: &str) -> String {
    let complete = regex::Regex::new(r"<thinking>[\s\S]*?</thinking>")
        .expect("source regex")
        .replace_all(text, "")
        .into_owned();
    regex::Regex::new(r"<thinking>[\s\S]*$")
        .expect("source regex")
        .replace(&complete, "")
        .into_owned()
}
/// Maps to: CC `yoloClassifier.ts:578-584` `parseXmlBlock`.
fn parse_xml_block(text: &str) -> Option<bool> {
    // Source /gi has no /u: neither tags nor values fold non-ASCII into ASCII.
    regex::Regex::new(r"(?i-u)<block>(yes|no)\b(</block>)?")
        .expect("source regex")
        .captures(&strip_thinking(text))
        .map(|m| m[1].eq_ignore_ascii_case("yes"))
}
/// Maps to: CC `yoloClassifier.ts:590-596` `parseXmlReason`.
fn parse_xml_reason(text: &str) -> Option<String> {
    regex::Regex::new(r"<reason>([\s\S]*?)</reason>")
        .expect("source regex")
        .captures(&strip_thinking(text))
        .map(|m| m[1].trim().to_owned())
}
/// Maps to: CC `yoloClassifier.ts:601-604` `parseXmlThinking`.
fn parse_xml_thinking(text: &str) -> Option<String> {
    regex::Regex::new(r"<thinking>([\s\S]*?)</thinking>")
        .expect("source regex")
        .captures(text)
        .map(|m| m[1].trim().to_owned())
}
/// Maps to: CC `yoloClassifier.ts:609-618` `extractUsage`.
fn extract_usage(result: &ApiMessage) -> ClassifierUsage {
    ClassifierUsage {
        input_tokens: result.usage.input_tokens.max(0) as u64,
        output_tokens: result.usage.output_tokens.max(0) as u64,
        cache_read_input_tokens: result.usage.cache_read_input_tokens.unwrap_or(0).max(0) as u64,
        cache_creation_input_tokens: result.usage.cache_creation_input_tokens.unwrap_or(0).max(0)
            as u64,
    }
}
/// Maps to: CC `yoloClassifier.ts:624-628` `extractRequestId`.
fn extract_request_id(result: &ApiMessage) -> Option<String> {
    result.request_id.clone()
}
/// Maps to: CC `yoloClassifier.ts:633-641` `combineUsage`.
fn combine_usage(a: &ClassifierUsage, b: &ClassifierUsage) -> ClassifierUsage {
    ClassifierUsage {
        input_tokens: a.input_tokens + b.input_tokens,
        output_tokens: a.output_tokens + b.output_tokens,
        cache_read_input_tokens: a.cache_read_input_tokens + b.cache_read_input_tokens,
        cache_creation_input_tokens: a.cache_creation_input_tokens + b.cache_creation_input_tokens,
    }
}
/// Maps to: CC `yoloClassifier.ts:648-664` `replaceOutputFormatWithXml`.
fn replace_output_format_with_xml(system: &str) -> String {
    system.replacen("Use the classify_result tool to report your classification.", &[
        "## Output Format", "", "If the action should be blocked:", "<block>yes</block><reason>one short sentence</reason>", "", "If the action should be allowed:", "<block>no</block>", "", "Do NOT include a <reason> tag when the action is allowed.", "Your ENTIRE response MUST begin with <block>. Do NOT output any analysis, reasoning, or commentary before <block>. No \"Looking at...\" or similar preamble.",
    ].join("\n"), 1)
}
/// Maps to: CC `yoloClassifier.ts:683-693` `getClassifierThinkingConfig`.
fn get_classifier_thinking_config(model: &str) -> (Option<SideQueryThinking>, i64) {
    if has_internal_capability(InternalCapability::Permissions)
        && crate::utils::model::ant_models::resolve_ant_model(Some(model))
            .is_some_and(|m| m.always_on_thinking == Some(true))
    {
        (None, 2048)
    } else {
        (Some(SideQueryThinking::Disabled), 0)
    }
}
/// L1 snapshot serialization of source SideQueryOptions. AbortSignal's enumerable
/// object is {}, and omitted source properties stay omitted; no second cache.
fn classifier_request_snapshot(opts: &SideQueryOptions) -> Value {
    let mut value = json!({"model":opts.model,"messages":opts.messages,"max_tokens":opts.max_tokens,"skipSystemPromptPrefix":opts.skip_system_prompt_prefix,"temperature":opts.temperature,"maxRetries":opts.max_retries,"querySource":opts.query_source});
    if let Some(system) = &opts.system {
        value["system"] = match system {
            SideQuerySystem::Text(t) => json!(t),
            SideQuerySystem::Blocks(b) => json!(b),
        };
    }
    if let Some(tools) = &opts.tools {
        value["tools"] = json!(tools);
    }
    if let Some(choice) = &opts.tool_choice {
        value["tool_choice"] = json!(choice);
    }
    if let Some(thinking) = &opts.thinking {
        value["thinking"] = match thinking {
            SideQueryThinking::Disabled => json!(false),
            SideQueryThinking::Enabled { budget_tokens } => json!(budget_tokens),
        };
    }
    if let Some(stop) = &opts.stop_sequences {
        value["stop_sequences"] = json!(stop);
    }
    if opts.signal.is_some() {
        value["signal"] = json!({});
    }
    value
}

/// Maps to: CC `yoloClassifier.ts:711-996` `classifyYoloActionXml`.
async fn classify_yolo_action_xml(
    prefix: Vec<MessageParam>,
    system: String,
    user: String,
    user_blocks: Vec<ContentBlockParam>,
    model: String,
    prompt_lengths: ClassifierPromptLengths,
    signal: Option<anthropic_sdk::AbortSignal>,
    dump_context: ClassifierDumpContext,
    mode: TwoStageMode,
) -> YoloClassifierResult {
    let classifier_type = match mode {
        TwoStageMode::Both => "xml_2stage",
        TwoStageMode::Fast => "xml_fast",
        TwoStageMode::Thinking => "xml_thinking",
    };
    let xml_system = replace_output_format_with_xml(&system);
    let system_blocks = vec![classifier_text_block(&xml_system, true)];
    let mut stage1_usage: Option<ClassifierUsage> = None;
    let mut stage1_duration_ms = None;
    let mut stage1_request_id = None;
    let mut stage1_msg_id = None;
    let mut stage1_opts = None;
    let overall_start = std::time::Instant::now();
    let (thinking, padding) = get_classifier_thinking_config(&model);
    let mut wrapped = vec![ContentBlockParam::Text(classifier_text_block(
        "<transcript>\n",
        false,
    ))];
    wrapped.extend(user_blocks);
    wrapped.push(ContentBlockParam::Text(classifier_text_block(
        "</transcript>\n",
        false,
    )));
    let result: anyhow::Result<YoloClassifierResult> = async {
        if mode != TwoStageMode::Thinking {
            let stage_start = std::time::Instant::now();
            let timestamp = chrono::Utc::now().timestamp_millis();
            let mut content = wrapped.clone();
            content.push(ContentBlockParam::Text(classifier_text_block(
                XML_S1_SUFFIX,
                false,
            )));
            let mut messages = prefix.clone();
            messages.push(MessageParam {
                role: "user".into(),
                content: MessageContent::Blocks(content),
            });
            let opts = SideQueryOptions {
                model: model.clone(),
                max_tokens: Some((if mode == TwoStageMode::Fast { 256 } else { 64 }) + padding),
                system: Some(SideQuerySystem::Blocks(system_blocks.clone())),
                skip_system_prompt_prefix: true,
                temperature: Some(0.0),
                thinking: thinking.clone(),
                messages,
                max_retries: Some(crate::services::api::with_retry::get_default_max_retries()),
                signal: signal.clone(),
                stop_sequences: (mode != TwoStageMode::Fast).then(|| vec!["</block>".into()]),
                query_source: "auto_mode".into(),
                ..Default::default()
            };
            let snapshot = classifier_request_snapshot(&opts);
            let raw = side_query(opts).await?;
            stage1_duration_ms = Some(stage_start.elapsed().as_millis() as u64);
            stage1_usage = Some(extract_usage(&raw));
            stage1_request_id = extract_request_id(&raw);
            stage1_msg_id = Some(raw.id.clone());
            let text = crate::utils::messages::extract_text_content(&raw.content, None);
            let block = parse_xml_block(&text);
            maybe_dump_auto_mode(snapshot.clone(), raw, timestamp, Some("stage1"));
            crate::bootstrap::state::set_last_classifier_requests(Some(vec![snapshot.clone()]));
            stage1_opts = Some(snapshot);
            if block == Some(false) || mode == TwoStageMode::Fast {
                let (should_block, reason, outcome) = match block {
                    Some(false) => (false, "Allowed by fast classifier".into(), "success"),
                    Some(true) => (
                        true,
                        parse_xml_reason(&text)
                            .unwrap_or_else(|| "Blocked by fast classifier".into()),
                        "success",
                    ),
                    None => (
                        true,
                        "Classifier stage 1 unparseable - blocking for safety".into(),
                        "parse_failure",
                    ),
                };
                log_auto_mode_outcome(
                    outcome,
                    &model,
                    json!({"classifierType":classifier_type,"durationMs":stage1_duration_ms}),
                );
                return Ok(YoloClassifierResult {
                    should_block,
                    reason,
                    model: model.clone(),
                    usage: stage1_usage.clone(),
                    duration_ms: stage1_duration_ms,
                    prompt_lengths: Some(prompt_lengths.clone()),
                    stage: Some("fast".into()),
                    stage1_request_id: stage1_request_id.clone(),
                    stage1_msg_id: stage1_msg_id.clone(),
                    ..Default::default()
                });
            }
        }
        let stage_start = std::time::Instant::now();
        let timestamp = chrono::Utc::now().timestamp_millis();
        let mut content = wrapped.clone();
        content.push(ContentBlockParam::Text(classifier_text_block(
            XML_S2_SUFFIX,
            false,
        )));
        let mut messages = prefix.clone();
        messages.push(MessageParam {
            role: "user".into(),
            content: MessageContent::Blocks(content),
        });
        let opts = SideQueryOptions {
            model: model.clone(),
            max_tokens: Some(4096 + padding),
            system: Some(SideQuerySystem::Blocks(system_blocks.clone())),
            skip_system_prompt_prefix: true,
            temperature: Some(0.0),
            thinking: thinking.clone(),
            messages,
            max_retries: Some(crate::services::api::with_retry::get_default_max_retries()),
            signal: signal.clone(),
            query_source: "auto_mode".into(),
            ..Default::default()
        };
        let snapshot = classifier_request_snapshot(&opts);
        let raw = side_query(opts).await?;
        let duration = stage_start.elapsed().as_millis() as u64;
        let usage = extract_usage(&raw);
        let request_id = extract_request_id(&raw);
        let msg_id = raw.id.clone();
        let text = crate::utils::messages::extract_text_content(&raw.content, None);
        let block = parse_xml_block(&text);
        let total_duration = stage1_duration_ms.unwrap_or(0) + duration;
        let total_usage = stage1_usage
            .as_ref()
            .map(|s| combine_usage(s, &usage))
            .unwrap_or_else(|| usage.clone());
        maybe_dump_auto_mode(snapshot.clone(), raw, timestamp, Some("stage2"));
        let mut requests = stage1_opts.clone().into_iter().collect::<Vec<_>>();
        requests.push(snapshot);
        crate::bootstrap::state::set_last_classifier_requests(Some(requests));
        log_auto_mode_outcome(
            if block.is_some() {
                "success"
            } else {
                "parse_failure"
            },
            &model,
            json!({"classifierType":classifier_type,"durationMs":total_duration}),
        );
        Ok(YoloClassifierResult {
            thinking: if block.is_some() {
                parse_xml_thinking(&text)
            } else {
                None
            },
            should_block: block.unwrap_or(true),
            reason: if block.is_none() {
                "Classifier stage 2 unparseable - blocking for safety".into()
            } else {
                parse_xml_reason(&text).unwrap_or_else(|| "No reason provided".into())
            },
            model: model.clone(),
            usage: Some(total_usage),
            duration_ms: Some(total_duration),
            prompt_lengths: Some(prompt_lengths.clone()),
            stage: Some("thinking".into()),
            stage1_usage: stage1_usage.clone(),
            stage1_duration_ms,
            stage1_request_id: stage1_request_id.clone(),
            stage1_msg_id: stage1_msg_id.clone(),
            stage2_usage: Some(usage),
            stage2_duration_ms: Some(duration),
            stage2_request_id: request_id,
            stage2_msg_id: Some(msg_id),
            ..Default::default()
        })
    }
    .await;
    match result {
        Ok(result) => result,
        Err(error) => {
            if signal.as_ref().is_some_and(|s| s.is_aborted()) {
                log_auto_mode_outcome(
                    "interrupted",
                    &model,
                    json!({"classifierType":classifier_type}),
                );
                return YoloClassifierResult {
                    should_block: true,
                    reason: "Classifier request aborted".into(),
                    model,
                    unavailable: true,
                    duration_ms: Some(overall_start.elapsed().as_millis() as u64),
                    prompt_lengths: Some(prompt_lengths),
                    ..Default::default()
                };
            }
            let too_long = detect_prompt_too_long(&error);
            let dump =
                dump_error_prompts(&xml_system, &user, &format!("{error:#}"), &dump_context).await;
            log_auto_mode_outcome(
                if too_long.is_some() {
                    "transcript_too_long"
                } else {
                    "error"
                },
                &model,
                json!({"classifierType":classifier_type,"transcriptActualTokens":too_long.and_then(|v| v.0),"transcriptLimitTokens":too_long.and_then(|v| v.1)}),
            );
            YoloClassifierResult {
                should_block: true,
                reason: if too_long.is_some() {
                    "Classifier transcript exceeded context window"
                } else if stage1_usage.is_some() {
                    "Stage 2 classifier error - blocking based on stage 1 assessment"
                } else {
                    "Classifier unavailable - blocking for safety"
                }
                .into(),
                model,
                unavailable: stage1_usage.is_none(),
                transcript_too_long: too_long.is_some(),
                stage: stage1_usage.as_ref().map(|_| "thinking".into()),
                duration_ms: Some(overall_start.elapsed().as_millis() as u64),
                error_dump_path: dump,
                usage: stage1_usage.clone(),
                stage1_usage,
                stage1_duration_ms,
                stage1_request_id,
                stage1_msg_id,
                prompt_lengths: Some(prompt_lengths),
                ..Default::default()
            }
        }
    }
}
/// Maps to: CC `yoloClassifier.ts:1012-1306` `classifyYoloAction`.
/// The optional signal is the existing Rust cancellation carrier; production
/// callers supply the turn's signal. No independent timeout or length policy.
pub async fn classify_yolo_action(
    messages: &[Message],
    action: &TranscriptEntry,
    tools: &[Tool],
    context: &ToolPermissionContext,
    signal: Option<anthropic_sdk::AbortSignal>,
) -> YoloClassifierResult {
    let lookup = build_tool_lookup(tools);
    let action_compact = to_compact(action, &lookup);
    if action_compact.is_empty() {
        return YoloClassifierResult {
            reason: "Tool declares no classifier-relevant input".into(),
            model: get_classifier_model(),
            ..Default::default()
        };
    }
    let system = build_yolo_system_prompt(context);
    let entries = build_transcript_entries(messages);
    let prefix: Vec<_> = build_claude_md_message().into_iter().collect();
    let mut prompt_lengths = ClassifierPromptLengths {
        system_prompt: system.encode_utf16().count() as u64,
        tool_calls: action_compact.encode_utf16().count() as u64,
        user_prompts: 0,
    };
    let mut user_blocks = Vec::new();
    let mut user = String::new();
    for entry in &entries {
        for block in &entry.content {
            let serialized = to_compact_block(block, &entry.role, &lookup);
            if serialized.is_empty() {
                continue;
            }
            match entry.role {
                TranscriptRole::User => {
                    prompt_lengths.user_prompts += serialized.encode_utf16().count() as u64
                }
                TranscriptRole::Assistant => {
                    prompt_lengths.tool_calls += serialized.encode_utf16().count() as u64
                }
            }
            user.push_str(&serialized);
            user_blocks.push(ContentBlockParam::Text(classifier_text_block(
                serialized, false,
            )));
        }
    }
    user.push_str(&action_compact);
    let classifier_chars = prompt_lengths.system_prompt + user.encode_utf16().count() as u64;
    let model = get_classifier_model();
    let dump_context = ClassifierDumpContext {
        main_loop_tokens: crate::utils::tokens::token_count_with_estimation(messages),
        classifier_chars,
        classifier_tokens_est: ((classifier_chars as f64) / 4.0).round() as u64,
        transcript_entries: entries.len(),
        messages: messages.len(),
        action: action_compact.clone(),
        model: model.clone(),
    };
    user_blocks.push(ContentBlockParam::Text(classifier_text_block(
        action_compact,
        true,
    )));
    if is_two_stage_classifier_enabled() {
        return classify_yolo_action_xml(
            prefix,
            system,
            user,
            user_blocks,
            model,
            prompt_lengths,
            signal,
            dump_context,
            get_two_stage_mode(),
        )
        .await;
    }
    let (thinking, padding) = get_classifier_thinking_config(&model);
    let start = std::time::Instant::now();
    let timestamp = chrono::Utc::now().timestamp_millis();
    let mut request_messages = prefix;
    request_messages.push(MessageParam {
        role: "user".into(),
        content: MessageContent::Blocks(user_blocks),
    });
    let opts = SideQueryOptions {
        model: model.clone(),
        max_tokens: Some(4096 + padding),
        system: Some(SideQuerySystem::Blocks(vec![classifier_text_block(
            &system, true,
        )])),
        skip_system_prompt_prefix: true,
        temperature: Some(0.0),
        thinking,
        messages: request_messages,
        tools: Some(vec![crate::utils::side_query::custom_tool(
            YOLO_CLASSIFIER_TOOL_NAME,
            "Report the security classification result for the agent action",
            json!({"type":"object","properties":{"thinking":{"type":"string","description":"Brief step-by-step reasoning."},"shouldBlock":{"type":"boolean","description":"Whether the action should be blocked (true) or allowed (false)"},"reason":{"type":"string","description":"Brief explanation of the classification decision"}},"required":["thinking","shouldBlock","reason"]}),
        )]),
        tool_choice: Some(crate::utils::side_query::tool_choice_tool(
            YOLO_CLASSIFIER_TOOL_NAME,
        )),
        max_retries: Some(crate::services::api::with_retry::get_default_max_retries()),
        signal: signal.clone(),
        query_source: "auto_mode".into(),
        ..Default::default()
    };
    let snapshot = classifier_request_snapshot(&opts);
    match side_query(opts).await {
        Ok(raw) => {
            let duration = start.elapsed().as_millis() as u64;
            let usage = extract_usage(&raw);
            let mut result = YoloClassifierResult {
                model: model.clone(),
                usage: Some(usage.clone()),
                duration_ms: Some(duration),
                prompt_lengths: Some(prompt_lengths),
                stage1_request_id: extract_request_id(&raw),
                stage1_msg_id: Some(raw.id.clone()),
                ..Default::default()
            };
            maybe_dump_auto_mode(snapshot.clone(), raw.clone(), timestamp, None);
            crate::bootstrap::state::set_last_classifier_requests(Some(vec![snapshot]));
            let block = super::classifier_shared::extract_tool_use_block(
                &raw.content,
                YOLO_CLASSIFIER_TOOL_NAME,
            );
            match block {
                None => {
                    result.should_block = true;
                    result.reason =
                        "Classifier returned no tool use block - blocking for safety".into();
                    log_auto_mode_outcome(
                        "parse_failure",
                        &model,
                        json!({"failureKind":"no_tool_use"}),
                    );
                }
                Some(block) => match super::classifier_shared::parse_classifier_response::<
                    ClassifierToolInput,
                >(block)
                {
                    None => {
                        result.should_block = true;
                        result.reason = "Invalid classifier response - blocking for safety".into();
                        log_auto_mode_outcome(
                            "parse_failure",
                            &model,
                            json!({"failureKind":"invalid_schema"}),
                        );
                    }
                    Some(parsed) => {
                        result.thinking = Some(parsed.thinking);
                        result.should_block = parsed.should_block;
                        result.reason = parsed.reason;
                        log_auto_mode_outcome(
                            "success",
                            &model,
                            json!({"durationMs":duration,"mainLoopTokens":dump_context.main_loop_tokens,"classifierInputTokens":usage.input_tokens+usage.cache_read_input_tokens+usage.cache_creation_input_tokens,"classifierTokensEst":dump_context.classifier_tokens_est}),
                        );
                    }
                },
            }
            result
        }
        Err(error) => {
            if signal.as_ref().is_some_and(|s| s.is_aborted()) {
                log_auto_mode_outcome("interrupted", &model, json!({}));
                return YoloClassifierResult {
                    should_block: true,
                    reason: "Classifier request aborted".into(),
                    model,
                    unavailable: true,
                    ..Default::default()
                };
            }
            let too_long = detect_prompt_too_long(&error);
            let dump =
                dump_error_prompts(&system, &user, &format!("{error:#}"), &dump_context).await;
            log_auto_mode_outcome(
                if too_long.is_some() {
                    "transcript_too_long"
                } else {
                    "error"
                },
                &model,
                json!({"mainLoopTokens":dump_context.main_loop_tokens,"classifierTokensEst":dump_context.classifier_tokens_est,"transcriptActualTokens":too_long.and_then(|v| v.0),"transcriptLimitTokens":too_long.and_then(|v| v.1)}),
            );
            YoloClassifierResult {
                should_block: true,
                reason: if too_long.is_some() {
                    "Classifier transcript exceeded context window"
                } else {
                    "Classifier unavailable - blocking for safety"
                }
                .into(),
                model,
                unavailable: true,
                transcript_too_long: too_long.is_some(),
                error_dump_path: dump,
                ..Default::default()
            }
        }
    }
}
/// Maps to: CC `yoloClassifier.ts:1334-1347` `getClassifierModel`.
fn get_classifier_model() -> String {
    if has_internal_capability(InternalCapability::Permissions) {
        if let Some(model) = crate::utils::process_env::var("CLAUDE_CODE_AUTO_MODE_MODEL").truthy()
        {
            return model;
        }
    }
    let config = crate::services::analytics::growthbook::get_feature_value_cached_may_be_stale(
        "tengu_auto_mode_config",
        AutoModeConfig::default(),
    );
    config
        .model
        .filter(|m| !m.is_empty())
        .unwrap_or_else(crate::utils::model::model::get_main_loop_model)
}
/// Maps to: CC `yoloClassifier.ts:1353-1369` `resolveTwoStageClassifier`.
fn resolve_two_stage_classifier() -> Option<Value> {
    if has_internal_capability(InternalCapability::Permissions) {
        let env = crate::utils::process_env::var("CLAUDE_CODE_TWO_STAGE_CLASSIFIER");
        if matches!(env.as_deref(), Some("fast" | "thinking")) {
            return env.map(Value::String);
        }
        if crate::utils::env_utils::is_env_truthy(env.as_deref()) {
            return Some(Value::Bool(true));
        }
        if crate::utils::env_utils::is_env_defined_falsy(env.as_deref()) {
            return Some(Value::Bool(false));
        }
    }
    crate::services::analytics::growthbook::get_feature_value_cached_may_be_stale(
        "tengu_auto_mode_config",
        AutoModeConfig::default(),
    )
    .two_stage_classifier
}
/// Maps to: CC `yoloClassifier.ts:1374-1377` `isTwoStageClassifierEnabled`.
fn is_two_stage_classifier_enabled() -> bool {
    let value = resolve_two_stage_classifier();
    matches!(value, Some(Value::Bool(true)))
        || matches!(
            value.as_ref().and_then(Value::as_str),
            Some("fast" | "thinking")
        )
}
/// Maps to: CC `yoloClassifier.ts:1379-1390` `isJsonlTranscriptEnabled`.
fn is_jsonl_transcript_enabled() -> bool {
    if has_internal_capability(InternalCapability::Permissions) {
        let env = crate::utils::process_env::var("CLAUDE_CODE_JSONL_TRANSCRIPT");
        if crate::utils::env_utils::is_env_truthy(env.as_deref()) {
            return true;
        }
        if crate::utils::env_utils::is_env_defined_falsy(env.as_deref()) {
            return false;
        }
    }
    crate::services::analytics::growthbook::get_feature_value_cached_may_be_stale(
        "tengu_auto_mode_config",
        AutoModeConfig::default(),
    )
    .jsonl_transcript
        == Some(true)
}
/// Maps to: CC `yoloClassifier.ts:1402-1414` `POWERSHELL_DENY_GUIDANCE`.
#[cfg(feature = "anthropic_internal")]
const POWERSHELL_DENY_GUIDANCE: &[&str] = &[
    "PowerShell Download-and-Execute: `iex (iwr ...)`, `Invoke-Expression (Invoke-WebRequest ...)`, `Invoke-Expression (New-Object Net.WebClient).DownloadString(...)`, and any pipeline feeding remote content into `Invoke-Expression`/`iex` fall under \"Code from External\" — same as `curl | bash`.",
    "PowerShell Irreversible Destruction: `Remove-Item -Recurse -Force`, `rm -r -fo`, `Clear-Content`, and `Set-Content` truncation of pre-existing files fall under \"Irreversible Local Destruction\" — same as `rm -rf` and `> file`.",
    "PowerShell Persistence: modifying `$PROFILE` (any of the four profile paths), `Register-ScheduledTask`, `New-Service`, writing to registry Run keys (`HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\Run` or the HKLM equivalent), and WMI event subscriptions fall under \"Unauthorized Persistence\" — same as `.bashrc` edits and cron jobs.",
    "PowerShell Elevation: `Start-Process -Verb RunAs`, `-ExecutionPolicy Bypass`, and disabling AMSI/Defender (`Set-MpPreference -DisableRealtimeMonitoring`) fall under \"Security Weaken\".",
];
#[cfg(not(feature = "anthropic_internal"))]
const POWERSHELL_DENY_GUIDANCE: &[&str] = &[];
/// Maps to: CC `yoloClassifier.ts:1425-1455` `logAutoModeOutcome`.
fn log_auto_mode_outcome(outcome: &str, model: &str, mut extra: Value) {
    extra["outcome"] = json!(outcome);
    extra["classifierModel"] = json!(model);
    crate::services::analytics::log_event("tengu_auto_mode_outcome", extra);
}
/// Maps to: CC `yoloClassifier.ts:1463-1471` `detectPromptTooLong`.
fn detect_prompt_too_long(error: &anyhow::Error) -> Option<(Option<u64>, Option<u64>)> {
    // Keep SDK source messages when side_query contributes anyhow context.
    let message = error
        .chain()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(": ");
    message
        .to_lowercase()
        .contains("prompt is too long")
        .then(|| crate::services::api::errors::parse_prompt_too_long_token_counts(&message))
}
/// Maps to: CC `yoloClassifier.ts:1477-1480` `getTwoStageMode`.
fn get_two_stage_mode() -> TwoStageMode {
    match resolve_two_stage_classifier()
        .as_ref()
        .and_then(Value::as_str)
    {
        Some("fast") => TwoStageMode::Fast,
        Some("thinking") => TwoStageMode::Thinking,
        _ => TwoStageMode::Both,
    }
}
/// Maps to: CC `yoloClassifier.ts:1487-1495` `formatActionForClassifier`.
pub fn format_action_for_classifier(tool_name: &str, tool_input: Value) -> TranscriptEntry {
    TranscriptEntry {
        role: TranscriptRole::Assistant,
        content: vec![TranscriptBlock::ToolUse {
            name: tool_name.into(),
            input: tool_input,
        }],
    }
}

/// Existing L1 sync bridge for the synchronous permission consumer. The source
/// operation remains classifyYoloAction; the caller owns cancellation.
pub fn classify_yolo_action_sync_with_signal(
    messages: &[Message],
    tool_name: &str,
    input: &Value,
    tools: &[Tool],
    context: &ToolPermissionContext,
    signal: Option<anthropic_sdk::AbortSignal>,
) -> YoloClassifierDecision {
    #[cfg(test)]
    if let Some(forced) = forced_classifier_decision(tool_name) {
        return forced;
    }
    let action = format_action_for_classifier(tool_name, input.clone());
    let messages = messages.to_vec();
    let tools = tools.to_vec();
    let context = context.clone();
    let result = crate::utils::process_runtime::block_on_from_sync(async move {
        classify_yolo_action(&messages, &action, &tools, &context, signal).await
    })
    .unwrap_or_else(|| YoloClassifierResult {
        should_block: true,
        unavailable: true,
        reason: "Failed to create runtime for the auto-mode classifier".into(),
        model: get_classifier_model(),
        ..Default::default()
    });
    yolo_result_to_decision(result)
}
/// L1 sync consumer projection; source fields are retained on the canonical
/// result for async consumers, with the existing sync subset here.
pub fn yolo_result_to_decision(result: YoloClassifierResult) -> YoloClassifierDecision {
    if result.unavailable {
        YoloClassifierDecision::Unavailable {
            reason: result.reason,
            model: result.model,
            transcript_too_long: result.transcript_too_long,
        }
    } else if result.should_block {
        YoloClassifierDecision::Block {
            reason: result.reason,
            transcript_too_long: result.transcript_too_long,
        }
    } else {
        YoloClassifierDecision::Allow {
            reason: result.reason,
        }
    }
}
/// Unit-test injection only. No production environment can skip classification.
#[cfg(test)]
pub fn forced_classifier_decision(tool_name: &str) -> Option<YoloClassifierDecision> {
    if let Some(force) = crate::utils::process_env::var("COMETIX_AUTO_CLASSIFIER_FORCE") {
        match force.to_ascii_lowercase().as_str() {
            "allow" => {
                return Some(YoloClassifierDecision::Allow {
                    reason: format!("forced allow for {tool_name}"),
                });
            }
            "block" | "deny" => {
                return Some(YoloClassifierDecision::Block {
                    reason: format!("forced block for {tool_name}"),
                    transcript_too_long: false,
                });
            }
            "unavailable" | "timeout" => {
                return Some(YoloClassifierDecision::Unavailable {
                    reason: format!("forced unavailable for {tool_name}"),
                    model: get_classifier_model(),
                    transcript_too_long: false,
                });
            }
            _ => {}
        }
    }
    if !crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("COMETIX_AUTO_CLASSIFIER_LIVE").as_deref(),
    ) {
        return Some(YoloClassifierDecision::Unavailable {
            reason: format!("classifier live API disabled in tests for {tool_name}"),
            model: get_classifier_model(),
            transcript_too_long: false,
        });
    }
    None
}
/// Inverse projection used only by the test injection rail.
#[cfg(test)]
pub fn decision_to_result(decision: YoloClassifierDecision) -> YoloClassifierResult {
    let (should_block, unavailable, reason, model, transcript_too_long) = match decision {
        YoloClassifierDecision::Allow { reason } => {
            (false, false, reason, get_classifier_model(), false)
        }
        YoloClassifierDecision::Block {
            reason,
            transcript_too_long,
        } => (
            true,
            false,
            reason,
            get_classifier_model(),
            transcript_too_long,
        ),
        YoloClassifierDecision::Unavailable {
            reason,
            model,
            transcript_too_long,
        } => (true, true, reason, model, transcript_too_long),
    };
    YoloClassifierResult {
        should_block,
        unavailable,
        reason,
        model,
        transcript_too_long,
        ..Default::default()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ids::ToolUseId;
    use crate::types::message::{AssistantMessage, ToolUseBlock, UserMessage};
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};
    use chrono::Utc;

    #[test]
    fn transcript_entries_include_user_text_and_assistant_tool_uses_only() {
        let messages = vec![
            Message::User(UserMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: Utc::now(),
                content: vec![
                    UserContent::Text("hello".to_string()),
                    UserContent::Image {
                        media_type: "image/png".to_string(),
                        data: "...".to_string(),
                    },
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
            }),
            Message::Assistant(AssistantMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: Utc::now(),
                content: vec![
                    AssistantContent::Text("model-authored text is excluded".to_string()),
                    AssistantContent::ToolUse(ToolUseBlock {
                        id: ToolUseId("toolu_1".to_string()),
                        name: "Bash".to_string(),
                        input: serde_json::json!({"command":"ls"}),
                    }),
                ],
                model: None,
                stop_reason: None,
                usage: None,
            }),
        ];

        let entries = build_transcript_entries(&messages);
        assert_eq!(entries.len(), 2);
        let tools = vec![crate::types::tools::Tool {
            name: "Bash".to_string(),
            ..Default::default()
        }];
        let compact = build_transcript_for_classifier(&messages, &tools);
        assert!(compact.contains("User: hello"));
        // CC `toCompactBlock:400` runs the tool's OWN projection.
        // `BashTool.toAutoClassifierInput` returns `input.command`
        // (`BashTool.tsx:660-662`; `bash_tool/mod.rs:1129-1134` matches), so the
        // transcript carries the command alone. This assertion used to read
        // `Bash {"command":"ls"}` — the unprojected argument object, which is
        // what the classifier prompt actually received.
        assert!(compact.contains("Bash ls"), "compact=\n{compact}");
        assert!(
            !compact.contains("{\"command\""),
            "raw input reached the classifier prompt:\n{compact}"
        );
        assert!(!compact.contains("model-authored"));

        // CC `:390-391` — `lookup.get(name)` misses ⇒ the block is dropped. The
        // lookup is built from the tools the CALLER holds, so a transcript entry
        // for a tool outside this context's set never reaches the classifier.
        let without_bash = build_transcript_for_classifier(&messages, &[]);
        assert!(without_bash.contains("User: hello"));
        assert!(
            !without_bash.contains("Bash"),
            "a tool outside the lookup survived:\n{without_bash}"
        );
    }

    /// CC `yoloClassifier.ts:305-325` — queued_command attachments become USER
    /// turns: a string prompt as-is (`:308-309`; even '' — the `:320` check is
    /// `text !== null`, not truthiness), an array prompt as its text blocks
    /// joined with '\n' (`:310-319`, JS `|| null` drops an empty join), and
    /// every other attachment type falls through and is dropped.
    #[test]
    fn transcript_entries_extract_queued_command_attachments_as_user_turns() {
        use crate::types::message::AttachmentMessage;
        use crate::utils::attachments::Attachment;

        fn queued(prompt: serde_json::Value) -> Message {
            Message::Attachment(AttachmentMessage::new(Attachment::QueuedCommand {
                prompt,
                source_uuid: None,
                image_paste_ids: None,
                command_mode: None,
                origin: None,
                is_meta: None,
            }))
        }

        let messages = vec![
            queued(serde_json::json!("run the tests")),
            // Text blocks joined with '\n'; the image block is filtered out.
            queued(serde_json::json!([
                {"type": "text", "text": "first"},
                {"type": "image", "source": {}},
                {"type": "text", "text": "second"},
            ])),
            // Array with no text blocks joins to '' — JS `|| null` drops it.
            queued(serde_json::json!([{"type": "image", "source": {}}])),
            // String '' survives the `text !== null` check.
            queued(serde_json::json!("")),
            // Non-queued_command attachments match no branch (CC `:305`).
            Message::Attachment(AttachmentMessage::new(Attachment::OutputStyle {
                style: "explanatory".to_string(),
            })),
        ];

        let entries = build_transcript_entries(&messages);
        assert_eq!(entries.len(), 3, "entries={entries:?}");
        for entry in &entries {
            assert_eq!(entry.role, TranscriptRole::User);
        }
        assert_eq!(
            entries[0].content,
            vec![TranscriptBlock::Text {
                text: "run the tests".to_string()
            }]
        );
        assert_eq!(
            entries[1].content,
            vec![TranscriptBlock::Text {
                text: "first\nsecond".to_string()
            }]
        );
        assert_eq!(
            entries[2].content,
            vec![TranscriptBlock::Text {
                text: String::new()
            }]
        );

        // The compact projection renders them as `User: ...` lines
        // (`toCompactBlock` text arm), so the queued follow-up is visible to
        // the classifier prompt even with an empty tool lookup.
        let compact = build_transcript_for_classifier(&messages, &[]);
        assert!(compact.contains("User: run the tests\n"), "{compact}");
        assert!(compact.contains("User: first\nsecond\n"), "{compact}");
    }

    #[test]
    fn system_prompt_includes_bundled_rules_and_classify_tool_name() {
        let prompt = build_default_external_system_prompt();
        assert!(prompt.contains("security classifier"));
        assert!(prompt.contains("classify_result"));
        assert!(prompt.contains("Allow Rules") || prompt.contains("read-only"));
        let rules = get_default_external_auto_mode_rules();
        assert!(!rules.allow.is_empty());
        assert!(!rules.soft_deny.is_empty());
        assert_eq!(YOLO_CLASSIFIER_TOOL_NAME, "classify_result");
        assert!(get_auto_mode_classifier_error_dump_path().contains("auto-mode-classifier-errors"));
    }

    use std::cell::RefCell;
    use std::collections::VecDeque;

    // Test-only transport boundary: the production classifier executes its real
    // request/parse/phase chain while deterministic SDK outcomes are supplied.
    struct Script {
        responses: VecDeque<anyhow::Result<ApiMessage>>,
        requests: Vec<SideQueryOptions>,
    }
    tokio::task_local! { static SCRIPT: RefCell<Script>; }
    pub(super) async fn scripted_side_query(opts: SideQueryOptions) -> anyhow::Result<ApiMessage> {
        if let Ok(response) = SCRIPT.try_with(|script| {
            let mut script = script.borrow_mut();
            script.requests.push(opts.clone());
            script
                .responses
                .pop_front()
                .expect("unexpected classifier request")
        }) {
            return response;
        }
        crate::utils::side_query::side_query(opts).await
    }

    struct ConfigGuard(
        Option<crate::utils::config::GlobalConfig>,
        Option<String>,
        Option<Vec<Value>>,
        Vec<EnvVarGuard>,
    );
    impl ConfigGuard {
        fn new(config: Value) -> Self {
            let vars = [
                "NODE_ENV",
                "DISABLE_TELEMETRY",
                "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
                "CLAUDE_CODE_USE_BEDROCK",
                "CLAUDE_CODE_USE_VERTEX",
                "CLAUDE_CODE_USE_FOUNDRY",
                "CLAUDE_INTERNAL_FC_OVERRIDES",
                "CLAUDE_CODE_AUTO_MODE_MODEL",
                "CLAUDE_CODE_TWO_STAGE_CLASSIFIER",
                "CLAUDE_CODE_JSONL_TRANSCRIPT",
                "CLAUDE_CODE_DUMP_AUTO_MODE",
            ]
            .into_iter()
            .map(EnvVarGuard::unset)
            .collect();
            crate::services::analytics::growthbook::reset_growth_book();
            let mut global = crate::utils::config::GlobalConfig::default();
            global.cached_growth_book_features =
                Some(HashMap::from([("tengu_auto_mode_config".into(), config)]));
            Self(
                crate::utils::config::replace_test_global_config(Some(global)),
                crate::bootstrap::state::get_cached_claude_md_content(),
                crate::bootstrap::state::get_last_classifier_requests(),
                vars,
            )
        }
        fn set_config(config: Value) {
            let mut global = crate::utils::config::load_global_config();
            global
                .cached_growth_book_features
                .get_or_insert_with(HashMap::new)
                .insert("tengu_auto_mode_config".into(), config);
            crate::utils::config::set_test_global_config(Some(global));
        }
    }
    impl Drop for ConfigGuard {
        fn drop(&mut self) {
            crate::utils::config::set_test_global_config(self.0.take());
            crate::bootstrap::state::set_cached_claude_md_content(self.1.take());
            crate::bootstrap::state::set_last_classifier_requests(self.2.take());
            self.3.clear();
            crate::services::analytics::growthbook::reset_growth_book();
        }
    }
    fn api_response(text: &str, id: &str) -> ApiMessage {
        serde_json::from_value(json!({"id":id,"_request_id":format!("req_{id}"),"type":"message","role":"assistant","model":"classifier-model","content":[{"type":"text","text":text}],"stop_reason":"end_turn","stop_sequence":null,"usage":{"input_tokens":10,"output_tokens":2,"cache_read_input_tokens":3,"cache_creation_input_tokens":4}})).unwrap()
    }
    fn action() -> TranscriptEntry {
        TranscriptEntry {
            role: TranscriptRole::User,
            content: vec![TranscriptBlock::Text {
                text: "review this action".into(),
            }],
        }
    }
    fn json_response(input: Value) -> ApiMessage {
        let mut response = api_response("", "json-message");
        response.content = serde_json::from_value(json!([{"type":"tool_use","id":"classify-id","name":YOLO_CLASSIFIER_TOOL_NAME,"input":input}])).unwrap();
        response
    }

    #[tokio::test]
    async fn classifier_schema_and_cached_request_match_official() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _config = ConfigGuard::new(json!({"model":"classifier-model"}));
        crate::bootstrap::state::set_cached_claude_md_content(Some("User instruction".into()));
        for (input, expected_block) in [
            // CC yoloClassifier.ts:252-257,1220-1235: an array is not the
            // z.object response and must never become an allow decision.
            (json!(["thinking", false, "allowed"]), true),
            (json!({"shouldBlock":false}), true),
            (
                json!({"shouldBlock":false,"thinking":null,"reason":null}),
                true,
            ),
            (
                json!({"shouldBlock":false,"thinking":"verified","reason":"authorized"}),
                false,
            ),
        ] {
            SCRIPT
                .scope(
                    RefCell::new(Script {
                        responses: VecDeque::from([Ok(json_response(input))]),
                        requests: vec![],
                    }),
                    async {
                        let result = classify_yolo_action(
                            &[],
                            &action(),
                            &[],
                            &ToolPermissionContext::default(),
                            None,
                        )
                        .await;
                        assert_eq!(result.should_block, expected_block);
                        if expected_block {
                            assert_eq!(
                                result.reason,
                                "Invalid classifier response - blocking for safety"
                            );
                        }
                        assert!(!result.unavailable);
                        assert_eq!(result.stage1_msg_id.as_deref(), Some("json-message"));
                        assert_eq!(
                            result.stage1_request_id.as_deref(),
                            Some("req_json-message")
                        );
                        SCRIPT.with(|script| {
                            let script = script.borrow();
                            let opts = &script.requests[0];
                            assert_eq!(opts.model, "classifier-model");
                            assert_eq!(
                                opts.max_retries,
                                Some(crate::services::api::with_retry::get_default_max_retries())
                            );
                            assert_eq!(opts.messages.len(), 2);
                            let request = classifier_request_snapshot(opts);
                            assert!(
                                request["messages"][0]["content"][0]["text"]
                                    .as_str()
                                    .unwrap()
                                    .contains(
                                        "<user_claude_md>\nUser instruction\n</user_claude_md>"
                                    )
                            );
                            assert_eq!(
                                request["messages"][0]["content"][0]["cache_control"]["type"],
                                "ephemeral"
                            );
                            assert_eq!(
                                request["messages"][1]["content"][0]["cache_control"]["type"],
                                "ephemeral"
                            );
                            assert_eq!(request["system"][0]["cache_control"]["type"], "ephemeral");
                            assert_eq!(
                                crate::bootstrap::state::get_last_classifier_requests(),
                                Some(vec![request])
                            );
                        });
                    },
                )
                .await;
        }
    }

    #[tokio::test]
    async fn xml_modes_and_second_stage_failure_match_official() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _config =
            ConfigGuard::new(json!({"model":"classifier-model","twoStageClassifier":true}));
        let cases = [
            // CC yoloClassifier.ts:580,825-860: /gi without /u does not
            // fold U+212A to ASCII k; fast fails closed, both continues.
            (
                json!("fast"),
                vec![Ok(api_response("<blocK>no</block>", "s1"))],
                true,
                false,
                "Classifier stage 1 unparseable - blocking for safety",
                1,
            ),
            (
                json!(true),
                vec![
                    Ok(api_response("<blocK>no</block>", "s1")),
                    Ok(api_response("<block>yes<reason>blocked</reason>", "s2")),
                ],
                true,
                false,
                "blocked",
                2,
            ),
            (
                json!(true),
                vec![Ok(api_response("<block>no", "s1"))],
                false,
                false,
                "Allowed by fast classifier",
                1,
            ),
            (
                json!(true),
                vec![
                    Ok(api_response("unparseable", "s1")),
                    Ok(api_response(
                        "<thinking><block>yes</block></thinking><block>no</block>",
                        "s2",
                    )),
                ],
                false,
                false,
                "No reason provided",
                2,
            ),
            (
                json!(true),
                vec![
                    Ok(api_response("<block>yes", "s1")),
                    Err(anyhow::anyhow!("server unavailable")),
                ],
                true,
                false,
                "Stage 2 classifier error - blocking based on stage 1 assessment",
                2,
            ),
            (
                json!("fast"),
                vec![Ok(api_response(
                    "<block>yes</block><reason>explicit reason</reason>",
                    "s1",
                ))],
                true,
                false,
                "explicit reason",
                1,
            ),
            (
                json!("fast"),
                vec![Ok(api_response("invalid", "s1"))],
                true,
                false,
                "Classifier stage 1 unparseable - blocking for safety",
                1,
            ),
            (
                json!("thinking"),
                vec![Ok(api_response("invalid", "s2"))],
                true,
                false,
                "Classifier stage 2 unparseable - blocking for safety",
                1,
            ),
        ];
        for (mode, responses, block, unavailable, reason, request_count) in cases {
            ConfigGuard::set_config(json!({"model":"classifier-model","twoStageClassifier":mode}));
            SCRIPT
                .scope(
                    RefCell::new(Script {
                        responses: responses.into(),
                        requests: vec![],
                    }),
                    async {
                        let result = classify_yolo_action(
                            &[],
                            &action(),
                            &[],
                            &ToolPermissionContext::default(),
                            None,
                        )
                        .await;
                        assert_eq!(
                            (
                                result.should_block,
                                result.unavailable,
                                result.reason.as_str()
                            ),
                            (block, unavailable, reason)
                        );
                        SCRIPT.with(|script| {
                            let script = script.borrow();
                            assert_eq!(script.requests.len(), request_count);
                            for request in &script.requests {
                                assert!(request.tools.is_none());
                                assert!(request.tool_choice.is_none());
                            }
                            let first = &script.requests[0];
                            let expected_tokens = match mode.as_str() {
                                Some("fast") => 256,
                                Some("thinking") => 4096,
                                _ => 64,
                            };
                            assert_eq!(first.max_tokens, Some(expected_tokens));
                            assert_eq!(first.stop_sequences.is_some(), mode == json!(true));
                            if request_count == 2 {
                                assert_eq!(script.requests[1].max_tokens, Some(4096));
                                assert!(script.requests[1].stop_sequences.is_none());
                            }
                        });
                        if result.stage2_usage.is_some() && request_count == 2 {
                            assert_eq!(result.usage.unwrap().input_tokens, 20);
                        }
                    },
                )
                .await;
        }
    }

    #[test]
    fn xml_parsing_matches_official_thinking_and_first_tag_rules() {
        // CC yoloClassifier.ts:580: ECMAScript /gi, not Unicode /giu.
        assert_eq!(parse_xml_block("<blocK>no</block>"), None);
        assert_eq!(parse_xml_block("<block>yeſ</block>"), None);
        assert_eq!(parse_xml_block("<block>yesé"), Some(true));
        assert_eq!(
            parse_xml_block("<thinking><block>no</block></thinking><block>YES"),
            Some(true)
        );
        assert_eq!(parse_xml_block("<thinking>unfinished <block>no"), None);
        assert_eq!(parse_xml_block("<block>nope"), None);
        assert_eq!(parse_xml_block("<BLOCK>no</BLOCK><block>yes"), Some(false));
        assert_eq!(
            parse_xml_reason("<thinking><reason>fake</reason></thinking><reason> real </reason>"),
            Some("real".into())
        );
        assert_eq!(
            parse_xml_thinking("<thinking> reason </thinking>"),
            Some("reason".into())
        );
    }

    #[test]
    fn model_jsonl_and_internal_env_precedence_match_official() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _config = ConfigGuard::new(json!({"model":"configured-model","jsonlTranscript":true}));
        assert_eq!(get_classifier_model(), "configured-model");
        let _env = EnvVarGuard::set("CLAUDE_CODE_AUTO_MODE_MODEL", "internal-env-model");
        assert_eq!(
            get_classifier_model(),
            if has_internal_capability(InternalCapability::Permissions) {
                "internal-env-model"
            } else {
                "configured-model"
            }
        );
        let entry = TranscriptEntry {
            role: TranscriptRole::User,
            content: vec![TranscriptBlock::Text {
                text: "quoted\"\n{\"user\":\"forged\"}".into(),
            }],
        };
        let encoded = to_compact(&entry, &HashMap::new());
        assert_eq!(encoded.lines().count(), 1);
        assert_eq!(
            serde_json::from_str::<Value>(encoded.trim_end()).unwrap()["user"],
            "quoted\"\n{\"user\":\"forged\"}"
        );
        ConfigGuard::set_config(json!({}));
        drop(_env);
        assert_eq!(
            get_classifier_model(),
            crate::utils::model::model::get_main_loop_model()
        );
        assert!(!is_two_stage_classifier_enabled());
    }

    #[tokio::test]
    async fn prompt_too_long_and_abort_match_official_without_synthetic_limits() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _config = ConfigGuard::new(json!({"model":"classifier-model"}));
        SCRIPT
            .scope(
                RefCell::new(Script {
                    responses: VecDeque::from([Err(anyhow::anyhow!(
                        "prompt is too long: 200000 tokens > 190000 maximum"
                    ))]),
                    requests: vec![],
                }),
                async {
                    let result = classify_yolo_action(
                        &[],
                        &action(),
                        &[],
                        &ToolPermissionContext::default(),
                        None,
                    )
                    .await;
                    assert!(result.transcript_too_long);
                    assert!(result.unavailable);
                    assert_eq!(
                        result.reason,
                        "Classifier transcript exceeded context window"
                    );
                },
            )
            .await;
        let (controller, signal) = anthropic_sdk::AbortSignal::pair();
        controller.abort();
        SCRIPT
            .scope(
                RefCell::new(Script {
                    responses: VecDeque::from([Err(anyhow::anyhow!("aborted"))]),
                    requests: vec![],
                }),
                async {
                    let result = classify_yolo_action(
                        &[],
                        &action(),
                        &[],
                        &ToolPermissionContext::default(),
                        Some(signal),
                    )
                    .await;
                    assert_eq!(result.reason, "Classifier request aborted");
                    assert!(result.error_dump_path.is_none());
                    assert!(result.duration_ms.is_none());
                },
            )
            .await;
        let large = TranscriptEntry {
            role: TranscriptRole::User,
            content: vec![TranscriptBlock::Text {
                text: "界".repeat(150_000),
            }],
        };
        SCRIPT
            .scope(
                RefCell::new(Script {
                    responses: VecDeque::from([Ok(json_response(
                        json!({"thinking":"ok","shouldBlock":false,"reason":"allowed"}),
                    ))]),
                    requests: vec![],
                }),
                async {
                    let result = classify_yolo_action(
                        &[],
                        &large,
                        &[],
                        &ToolPermissionContext::default(),
                        None,
                    )
                    .await;
                    assert!(!result.should_block);
                    assert!(!result.transcript_too_long);
                    assert_eq!(result.prompt_lengths.unwrap().user_prompts, 0);
                },
            )
            .await;
    }

    #[test]
    fn internal_template_and_always_on_thinking_match_official() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _config = ConfigGuard::new(json!({}));
        let internal = has_internal_capability(InternalCapability::Permissions);
        assert_eq!(is_using_external_permissions(), !internal);
        let prompt = build_yolo_system_prompt(&ToolPermissionContext::default());
        assert_eq!(
            prompt.contains("Allow Rules (Anthropic Internal)"),
            internal
        );
        ConfigGuard::set_config(json!({"forceExternalPermissions":true}));
        assert!(is_using_external_permissions());
        assert!(
            !build_yolo_system_prompt(&ToolPermissionContext::default())
                .contains("Anthropic Internal")
        );
        assert!(!build_default_external_system_prompt().contains("_to_replace>"));
        let mut global = crate::utils::config::load_global_config();
        global.cached_growth_book_features.as_mut().unwrap().insert("tengu_ant_model_override".into(), json!({"antModels":[{"alias":"always","model":"always-model","label":"Always", "alwaysOnThinking":true}]}));
        crate::utils::config::set_test_global_config(Some(global));
        let (thinking, padding) = get_classifier_thinking_config("ALWAYS-MODEL-2026");
        assert_eq!(thinking.is_none(), internal);
        assert_eq!(padding, if internal { 2048 } else { 0 });
        assert_eq!(get_classifier_thinking_config("normal-model").1, 0);
        ConfigGuard::set_config(json!({"twoStageClassifier":true}));
        let _env = EnvVarGuard::set("CLAUDE_CODE_TWO_STAGE_CLASSIFIER", "false");
        assert_eq!(is_two_stage_classifier_enabled(), !internal);
    }

    #[tokio::test]
    async fn sdk_error_context_keeps_original_message_in_dump() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _config = ConfigGuard::new(json!({"model":"classifier-model"}));
        let error = anyhow::Error::new(anthropic_sdk::ApiError::BadRequest {
            status: 400,
            message: "prompt is too long: 22 tokens > 20 maximum".into(),
            headers: HashMap::new(),
            request_id: Some("req_error".into()),
            body: None,
        })
        .context("side_query failed");
        SCRIPT
            .scope(
                RefCell::new(Script {
                    responses: VecDeque::from([Err(error)]),
                    requests: vec![],
                }),
                async {
                    let result = classify_yolo_action(
                        &[],
                        &action(),
                        &[],
                        &ToolPermissionContext::default(),
                        None,
                    )
                    .await;
                    assert!(result.transcript_too_long);
                    let dump = std::fs::read_to_string(result.error_dump_path.unwrap()).unwrap();
                    assert!(dump.contains("prompt is too long: 22 tokens > 20 maximum"));
                    assert!(dump.contains("=== CONTEXT COMPARISON ==="));
                    assert!(dump.contains("=== ACTION BEING CLASSIFIED ==="));
                },
            )
            .await;
    }

    #[tokio::test]
    async fn stage_two_prompt_overflow_survives_block_projection_like_official() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _config =
            ConfigGuard::new(json!({"model":"classifier-model","twoStageClassifier":true}));
        SCRIPT
            .scope(
                RefCell::new(Script {
                    responses: VecDeque::from([
                        Ok(api_response("<block>yes", "s1")),
                        Err(anyhow::anyhow!(
                            "prompt is too long: 22 tokens > 20 maximum"
                        )),
                    ]),
                    requests: vec![],
                }),
                async {
                    let result = classify_yolo_action(
                        &[],
                        &action(),
                        &[],
                        &ToolPermissionContext::default(),
                        None,
                    )
                    .await;
                    assert!(result.should_block);
                    assert!(!result.unavailable);
                    assert!(result.transcript_too_long);
                    assert_eq!(result.stage1_msg_id.as_deref(), Some("s1"));
                    assert!(matches!(
                        yolo_result_to_decision(result),
                        YoloClassifierDecision::Block {
                            transcript_too_long: true,
                            ..
                        }
                    ));
                },
            )
            .await;
    }

    #[test]
    fn force_env_is_test_only_on_sync_path() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _env = EnvVarGuard::set("COMETIX_AUTO_CLASSIFIER_FORCE", "allow");
        let result = classify_yolo_action_sync_with_signal(
            &[],
            "Bash",
            &json!({"command":"ls"}),
            &[],
            &ToolPermissionContext::default(),
            None,
        );
        assert!(matches!(result, YoloClassifierDecision::Allow { .. }));
    }
}
