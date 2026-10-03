//! API utility helpers.
//!
//! Maps to CC `utils/api.ts`. The API service (`services/api/claude.rs`) owns
//! request orchestration, while this module owns schema conversion helpers such
//! as `toolToAPISchema(...)`.

use crate::constants::prompts::SYSTEM_PROMPT_DYNAMIC_BOUNDARY;
use crate::constants::system::CLI_SYSPROMPT_PREFIXES;
use crate::types::message::{Message, UserContent, UserMessage};
use crate::types::tools::Tool;
use std::collections::BTreeMap;

/// Maps to: CC `utils/api.ts:80` `export type CacheScope = 'global' | 'org'`.
///
/// Serializes as the wire literal (`"global"` / `"org"`); only `Global` is
/// ever emitted on a `cache_control` block (`getCacheControl`), `Org` is the
/// API default and marks "org/default-scoped breakpoint" in
/// [`SystemPromptBlock`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CacheScope {
    Global,
    Org,
}

/// Maps to: CC `utils/api.ts:81-84` `SystemPromptBlock`.
///
/// `cache_scope: None` maps to CC `cacheScope: null` (no cache_control on the
/// block).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SystemPromptBlock {
    pub text: String,
    pub cache_scope: Option<CacheScope>,
}

/// Maps to: CC `utils/api.ts:87-90` `SWARM_FIELDS_BY_TOOL`.
///
/// Fields to filter from tool schemas when swarms are not enabled.
fn swarm_fields_by_tool(tool_name: &str) -> &'static [&'static str] {
    if tool_name == crate::tools::exit_plan_mode_tool::constants::EXIT_PLAN_MODE_V2_TOOL_NAME {
        &["launchSwarm", "teammateCount"]
    } else if tool_name == crate::tools::agent_tool::constants::AGENT_TOOL_NAME {
        &["name", "team_name", "mode"]
    } else {
        &[]
    }
}

/// Maps to: CC `utils/api.ts:96-117` `filterSwarmFieldsFromSchema`.
///
/// Filter swarm-related fields from a tool's input schema. Called at runtime
/// when `isAgentSwarmsEnabled()` returns false. Only `properties` is touched
/// (CC deletes the keys from a shallow copy of `properties`; `required` and
/// everything else pass through untouched).
fn filter_swarm_fields_from_schema(
    tool_name: &str,
    schema: serde_json::Value,
) -> serde_json::Value {
    let fields_to_remove = swarm_fields_by_tool(tool_name);
    if fields_to_remove.is_empty() {
        return schema;
    }
    let mut filtered = schema;
    if let Some(props) = filtered
        .get_mut("properties")
        .and_then(serde_json::Value::as_object_mut)
    {
        for field in fields_to_remove {
            props.remove(*field);
        }
    }
    filtered
}

/// Maps to: CC `utils/api.ts` `appendSystemContext(...)`.
pub fn append_system_context(
    system_prompt: &[String],
    context: &BTreeMap<String, String>,
) -> Vec<String> {
    let mut result = system_prompt.to_vec();
    let context_block = context
        .iter()
        .map(|(key, value)| format!("{key}: {value}"))
        .collect::<Vec<_>>()
        .join("\n");
    if !context_block.is_empty() {
        result.push(context_block);
    }
    result
}

/// Maps to: CC `utils/api.ts` `prependUserContext(...)`.
pub fn prepend_user_context(
    messages: Vec<Message>,
    context: &BTreeMap<String, String>,
) -> Vec<Message> {
    if crate::utils::process_env::var("NODE_ENV").as_deref() == Some("test") || context.is_empty() {
        return messages;
    }

    let context_text = context
        .iter()
        .map(|(key, value)| format!("# {key}\n{value}"))
        .collect::<Vec<_>>()
        .join("\n");
    let reminder = format!(
        "<system-reminder>\nAs you answer the user's questions, you can use the following context:\n{context_text}\n\n      IMPORTANT: this context may or may not be relevant to your tasks. You should not respond to this context unless it is highly relevant to your task.\n</system-reminder>\n"
    );

    let mut result = Vec::with_capacity(messages.len() + 1);
    result.push(Message::User(UserMessage {
        uuid: uuid::Uuid::new_v4().to_string(),
        timestamp: chrono::Utc::now(),
        content: vec![UserContent::Text(reminder)],
        is_compact_summary: false,
        plan_content: None,
        image_paste_ids: None,
        is_visible_in_transcript_only: false,
        mcp_meta: None,
        source_tool_assistant_uuid: None,
        permission_mode: None,
        origin: None,
        summarize_metadata: None,
    }));
    result.extend(messages);
    result
}

/// Maps to: CC `utils/api.ts:119-134` `toolToAPISchema` inline options.
///
/// One bag like CC's: the per-request projection knobs (`model`,
/// `deferLoading`) plus the `Tool.prompt(options)` inputs
/// (`getToolPermissionContext` / `tools` / `agents` / `allowedAgentTypes`,
/// api.ts:121-125) that the cache-miss branch forwards into the tool's lazy
/// description (api.ts:171-176).
#[derive(Clone, Copy)]
pub struct ToolToApiSchemaOptions<'a> {
    pub model: Option<&'a str>,
    pub defer_loading: bool,
    /// Maps to CC `options.getToolPermissionContext` (api.ts:122,172) — see
    /// `crate::tool::ToolPromptOptions` for the sync-value tier ruling.
    pub tool_permission_context: &'a crate::tool::ToolPermissionContext,
    /// Maps to CC `options.tools` (api.ts:123,173): the FULL request tool
    /// list, not the defer-filtered one (`services/api/claude.ts:1231-1239`).
    pub tools: &'a [Tool],
    /// Maps to CC `options.agents` (api.ts:124,174).
    pub agents: &'a [crate::tools::agent_tool::load_agents_dir::AgentDefinition],
    /// Maps to CC `options.allowedAgentTypes` (api.ts:125,175).
    pub allowed_agent_types: Option<&'a [String]>,
}

/// Maps to: CC `utils/api.ts:68-76,119-261` `BetaToolWithExtras` and
/// `toolToAPISchema`.
///
/// `defer_loading` is a per-request overlay. The cached base intentionally keys
/// ordinary built-ins by name and the current source-backed `inputJSONSchema`
/// carriers (MCP and StructuredOutput) by `name:serializedSchema`. Rust's Tool
/// metadata erased the property-presence distinction, so these are the two live
/// families that must retain it.
pub fn tool_to_api_schema(
    tool: &Tool,
    options: ToolToApiSchemaOptions<'_>,
) -> anyhow::Result<anthropic_sdk::resources::messages::ToolUnion> {
    let cache_key = if tool.is_mcp || tool.name == "StructuredOutput" {
        format!(
            "{}:{}",
            tool.name,
            serde_json::to_string(&tool.input_schema)?
        )
    } else {
        tool.name.clone()
    };
    let base = {
        let mut cache = crate::utils::tool_schema_cache::get_tool_schema_cache();
        if let Some(base) = cache.get(&cache_key) {
            base.clone()
        } else {
            let strict_tools_enabled = crate::utils::feature_flags::feature_enabled(
                crate::utils::feature_flags::FeatureFlag::StrictToolSchemas,
            );
            // Maps to CC `api.ts:157-167`: use the tool's JSON schema, then
            // filter out swarm-related fields when swarms are not enabled so
            // external non-EAP users don't see swarm features in the schema.
            let mut input_schema = tool.input_schema.clone();
            if !crate::utils::agent_swarms_enabled::is_agent_swarms_enabled() {
                input_schema = filter_swarm_fields_from_schema(&tool.name, input_schema);
            }
            let base = crate::utils::tool_schema_cache::CachedSchema {
                name: tool.name.clone(),
                // Maps to CC `api.ts:171-176` `description: await tool.prompt({
                // getToolPermissionContext, tools, agents, allowedAgentTypes
                // })` — kept inline like CC: the lazy read runs only inside
                // this cache-miss branch, so mid-session churn in the prompt
                // inputs cannot re-serialize the tool block (see the
                // `api.ts:136-140` byte-stability comment and
                // toolSchemaCache.ts).
                description: tool.prompt(&crate::tool::ToolPromptOptions {
                    tool_permission_context: options.tool_permission_context,
                    tools: options.tools,
                    agents: options.agents,
                    allowed_agent_types: options.allowed_agent_types,
                }),
                input_schema,
                strict: (strict_tools_enabled
                    && tool.strict == Some(true)
                    && options
                        .model
                        .is_some_and(crate::utils::betas::model_supports_structured_outputs))
                .then_some(true),
                eager_input_streaming: (crate::utils::model::providers::get_api_provider()
                    == crate::utils::model::providers::ApiProvider::FirstParty
                    && crate::utils::model::providers::is_first_party_anthropic_base_url()
                    && (crate::utils::feature_flags::feature_enabled(
                        crate::utils::feature_flags::FeatureFlag::FineGrainedToolStreaming,
                    ) || crate::utils::env_utils::is_env_truthy(
                        crate::utils::process_env::var("CLAUDE_CODE_ENABLE_FINE_GRAINED_TOOL_STREAMING")
                            .as_deref(),
                    )))
                .then_some(true),
            };
            cache.insert(cache_key, base.clone());
            base
        }
    };

    let disable_experimental_betas = crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS")
            .as_deref(),
    );
    let strict = (!disable_experimental_betas)
        .then_some(base.strict)
        .flatten();
    let defer_loading = (!disable_experimental_betas && options.defer_loading).then_some(true);
    let eager_input_streaming = (!disable_experimental_betas)
        .then_some(base.eager_input_streaming)
        .flatten();

    if defer_loading.is_some() || eager_input_streaming.is_some() {
        let beta_tool = anthropic_sdk::resources::beta::messages::BetaTool {
            input_schema: serde_json::from_value(base.input_schema)?,
            name: base.name,
            stainless_helper: None,
            allowed_callers: None,
            cache_control: None,
            defer_loading,
            description: Some(base.description),
            eager_input_streaming,
            input_examples: None,
            strict,
            type_name: None,
        };
        return Ok(anthropic_sdk::resources::messages::ToolUnion::Raw(
            serde_json::to_value(beta_tool)?,
        ));
    }

    Ok(anthropic_sdk::resources::messages::ToolUnion::Custom(
        anthropic_sdk::resources::messages::Tool {
            input_schema: base.input_schema,
            name: base.name,
            cache_control: None,
            description: Some(base.description),
            eager_input_streaming: None,
            strict,
            type_name: None,
        },
    ))
}

/// Maps to: CC `utils/api.ts:321-434` `splitSysPromptPrefix(systemPrompt, options?)`.
///
/// Splits the system prompt into cache-scoped blocks. The attribution header
/// (`x-anthropic-billing-header…`) and the CLI sysprompt prefix
/// (`CLI_SYSPROMPT_PREFIXES`) are identified by content, not position:
///
/// 1. MCP tools present (`skip_global_cache_for_system_prompt`, global cache
///    feature on): attribution (`None`) → prefix (`Org`) → everything else
///    concatenated (`Org`); the boundary marker is dropped.
/// 2. Global cache mode with boundary marker found: attribution (`None`) →
///    prefix (`None`) → static content before the boundary (`Global`) →
///    dynamic content after it (`None`).
/// 3. Default (3P providers, feature off, or boundary missing): attribution
///    (`None`) → prefix (`Org`) → everything else concatenated (`Org`).
///
/// Empty strings are skipped everywhere (CC `if (!prompt) continue`).
pub fn split_sys_prompt_prefix(
    system_prompt: &[String],
    skip_global_cache_for_system_prompt: bool,
) -> Vec<SystemPromptBlock> {
    let use_global_cache_feature = crate::utils::betas::should_use_global_cache_scope();
    if use_global_cache_feature && skip_global_cache_for_system_prompt {
        // CC: logEvent('tengu_sysprompt_using_tool_based_cache', …) — joins
        // with analytics.
        let mut attribution_header: Option<&str> = None;
        let mut system_prompt_prefix: Option<&str> = None;
        let mut rest: Vec<&str> = Vec::new();
        for prompt in system_prompt {
            if prompt.is_empty() {
                continue;
            }
            if prompt == SYSTEM_PROMPT_DYNAMIC_BOUNDARY {
                continue; // Skip boundary
            }
            if prompt.starts_with("x-anthropic-billing-header") {
                attribution_header = Some(prompt);
            } else if CLI_SYSPROMPT_PREFIXES.contains(prompt.as_str()) {
                system_prompt_prefix = Some(prompt);
            } else {
                rest.push(prompt);
            }
        }

        let mut result = Vec::new();
        if let Some(attribution_header) = attribution_header {
            result.push(SystemPromptBlock {
                text: attribution_header.to_string(),
                cache_scope: None,
            });
        }
        if let Some(system_prompt_prefix) = system_prompt_prefix {
            result.push(SystemPromptBlock {
                text: system_prompt_prefix.to_string(),
                cache_scope: Some(CacheScope::Org),
            });
        }
        let rest_joined = rest.join("\n\n");
        if !rest_joined.is_empty() {
            result.push(SystemPromptBlock {
                text: rest_joined,
                cache_scope: Some(CacheScope::Org),
            });
        }
        return result;
    }

    if use_global_cache_feature {
        if let Some(boundary_index) = system_prompt
            .iter()
            .position(|block| block == SYSTEM_PROMPT_DYNAMIC_BOUNDARY)
        {
            let mut attribution_header: Option<&str> = None;
            let mut system_prompt_prefix: Option<&str> = None;
            let mut static_blocks: Vec<&str> = Vec::new();
            let mut dynamic_blocks: Vec<&str> = Vec::new();

            for (i, block) in system_prompt.iter().enumerate() {
                if block.is_empty() || block == SYSTEM_PROMPT_DYNAMIC_BOUNDARY {
                    continue;
                }
                if block.starts_with("x-anthropic-billing-header") {
                    attribution_header = Some(block);
                } else if CLI_SYSPROMPT_PREFIXES.contains(block.as_str()) {
                    system_prompt_prefix = Some(block);
                } else if i < boundary_index {
                    static_blocks.push(block);
                } else {
                    dynamic_blocks.push(block);
                }
            }

            let mut result = Vec::new();
            if let Some(attribution_header) = attribution_header {
                result.push(SystemPromptBlock {
                    text: attribution_header.to_string(),
                    cache_scope: None,
                });
            }
            if let Some(system_prompt_prefix) = system_prompt_prefix {
                result.push(SystemPromptBlock {
                    text: system_prompt_prefix.to_string(),
                    cache_scope: None,
                });
            }
            let static_joined = static_blocks.join("\n\n");
            if !static_joined.is_empty() {
                result.push(SystemPromptBlock {
                    text: static_joined,
                    cache_scope: Some(CacheScope::Global),
                });
            }
            let dynamic_joined = dynamic_blocks.join("\n\n");
            if !dynamic_joined.is_empty() {
                result.push(SystemPromptBlock {
                    text: dynamic_joined,
                    cache_scope: None,
                });
            }
            // CC: logEvent('tengu_sysprompt_boundary_found', …) — joins with
            // analytics.
            return result;
        }
        // CC: logEvent('tengu_sysprompt_missing_boundary_marker', …) — joins
        // with analytics.
    }

    let mut attribution_header: Option<&str> = None;
    let mut system_prompt_prefix: Option<&str> = None;
    let mut rest: Vec<&str> = Vec::new();
    for block in system_prompt {
        if block.is_empty() {
            continue;
        }
        if block.starts_with("x-anthropic-billing-header") {
            attribution_header = Some(block);
        } else if CLI_SYSPROMPT_PREFIXES.contains(block.as_str()) {
            system_prompt_prefix = Some(block);
        } else {
            rest.push(block);
        }
    }

    let mut result = Vec::new();
    if let Some(attribution_header) = attribution_header {
        result.push(SystemPromptBlock {
            text: attribution_header.to_string(),
            cache_scope: None,
        });
    }
    if let Some(system_prompt_prefix) = system_prompt_prefix {
        result.push(SystemPromptBlock {
            text: system_prompt_prefix.to_string(),
            cache_scope: Some(CacheScope::Org),
        });
    }
    let rest_joined = rest.join("\n\n");
    if !rest_joined.is_empty() {
        result.push(SystemPromptBlock {
            text: rest_joined,
            cache_scope: Some(CacheScope::Org),
        });
    }
    result
}

/// Maps to: CC `utils/api.ts#normalizeToolInput:566-580`, ExitPlanMode branch.
/// Tool name and agent ID are the fields this source branch consumes. Other
/// tool-specific source branches retain their existing ToolCall adapters.
pub fn normalize_tool_input(
    tool_name: &str,
    input: &serde_json::Value,
    agent_id: Option<&str>,
) -> serde_json::Value {
    if tool_name != crate::tools::exit_plan_mode_tool::constants::EXIT_PLAN_MODE_V2_TOOL_NAME {
        return input.clone();
    }
    let plan = crate::utils::plans::get_plan(agent_id);
    let path = crate::utils::plans::get_plan_file_path(agent_id);
    let snapshot = crate::utils::plans::persist_file_snapshot_if_remote();
    if let Some(runtime) = crate::utils::process_runtime::runtime_handle_for_detached_work() {
        runtime.spawn(snapshot);
    } else {
        std::thread::spawn(move || {
            let _ = crate::utils::process_runtime::block_on_from_sync(snapshot);
        });
    }
    if let Some(plan) = plan {
        let mut input = match input {
            serde_json::Value::Object(input) => input.clone(),
            serde_json::Value::Array(items) => items
                .iter()
                .enumerate()
                .map(|(index, item)| (index.to_string(), item.clone()))
                .collect(),
            _ => serde_json::Map::new(),
        };
        input.insert("plan".to_string(), serde_json::json!(plan));
        input.insert("planFilePath".to_string(), serde_json::json!(path));
        input.into()
    } else {
        input.clone()
    }
}

/// Maps to: CC `utils/api.ts:685–703#normalizeToolInputForAPI`, ExitPlanMode branch.
/// Only the API copy loses the fields injected for execution/transcript recovery.
pub fn normalize_tool_input_for_api(
    tool: &crate::types::tools::Tool,
    input: &serde_json::Value,
) -> serde_json::Value {
    if tool.name == crate::tools::exit_plan_mode_tool::constants::EXIT_PLAN_MODE_V2_TOOL_NAME {
        if let Some(input) = input.as_object() {
            if input.contains_key("plan") || input.contains_key("planFilePath") {
                let mut rest = input.clone();
                rest.remove("plan");
                rest.remove("planFilePath");
                return rest.into();
            }
        }
    }
    input.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[test]
    fn normalize_tool_input_for_api_matches_official_bun_exit_plan_oracle() {
        let mut tool = crate::tools::exit_plan_mode_tool::exit_plan_mode_tool_schema();
        for (input, expected) in [
            (
                serde_json::json!({"plan":"body","planFilePath":"/plan.md","keep":true}),
                serde_json::json!({"keep":true}),
            ),
            (
                serde_json::json!({"plan":"","keep":true}),
                serde_json::json!({"keep":true}),
            ),
            (
                serde_json::json!({"planFilePath":"/plan.md","keep":true}),
                serde_json::json!({"keep":true}),
            ),
            (
                serde_json::json!({"keep":true}),
                serde_json::json!({"keep":true}),
            ),
            (serde_json::json!([1, 2]), serde_json::json!([1, 2])),
            (serde_json::Value::Null, serde_json::Value::Null),
            (serde_json::json!("plan"), serde_json::json!("plan")),
        ] {
            let before = input.clone();
            assert_eq!(normalize_tool_input_for_api(&tool, &input), expected);
            assert_eq!(input, before);
        }
        tool.name = "Unknown".into();
        let input = serde_json::json!({"plan":"body","planFilePath":"/plan.md","keep":true});
        assert_eq!(normalize_tool_input_for_api(&tool, &input), input);
    }

    /// Default-option builder for the CC-shaped one-bag options
    /// (`api.ts:119-134`): empty prompt inputs, like CC serialization tests
    /// that pass no MCP tools / agents and a default permission context.
    fn schema_options(model: Option<&str>, defer_loading: bool) -> ToolToApiSchemaOptions<'_> {
        static DEFAULT_PERMISSION_CONTEXT: std::sync::LazyLock<crate::tool::ToolPermissionContext> =
            std::sync::LazyLock::new(crate::tool::ToolPermissionContext::default);
        ToolToApiSchemaOptions {
            model,
            defer_loading,
            tool_permission_context: &DEFAULT_PERMISSION_CONTEXT,
            tools: &[],
            agents: &[],
            allowed_agent_types: None,
        }
    }

    #[test]
    fn append_system_context_and_prepend_user_context_match_official_shape() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("NODE_ENV");
        let mut context = BTreeMap::new();
        context.insert("cwd".to_string(), "/tmp/project".to_string());
        context.insert("mode".to_string(), "default".to_string());

        let system = append_system_context(&["base".to_string()], &context);
        assert_eq!(system.len(), 2);
        assert!(system[1].contains("cwd: /tmp/project"));
        assert!(system[1].contains("mode: default"));

        let messages = vec![Message::User(UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![UserContent::Text("hello".to_string())],
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
        let with_context = prepend_user_context(messages.clone(), &context);
        assert_eq!(with_context.len(), 2);
        assert!(matches!(
            &with_context[0],
            Message::User(user)
                if matches!(
                    user.content.first(),
                    Some(UserContent::Text(text))
                        if text.contains("<system-reminder>")
                            && text.contains("# cwd\n/tmp/project")
                )
        ));

        crate::utils::process_env::set("NODE_ENV", "test");
        assert_eq!(prepend_user_context(messages.clone(), &context), messages);
        crate::utils::process_env::remove("NODE_ENV");
    }

    #[test]
    fn tool_to_api_schema_projects_tool_metadata_like_official_helper() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS");
        let tool = Tool {
            name: "Example".to_string(),
            description: "Example prompt".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "value": { "type": "string" }
                },
                "required": ["value"],
                "additionalProperties": false
            }),
            strict: Some(true),
            ..Default::default()
        };

        crate::utils::tool_schema_cache::clear_tool_schema_cache();
        let mut config = crate::utils::config::GlobalConfig::default();
        config.cached_growth_book_features = Some(std::collections::HashMap::from([(
            "tengu_tool_pear".to_string(),
            serde_json::json!(true),
        )]));
        crate::utils::config::set_test_global_config(Some(config));
        let schema = tool_to_api_schema(&tool, schema_options(Some("claude-sonnet-4-6"), false))
            .expect("schema projection should succeed");
        let value = serde_json::to_value(schema).expect("SDK tool should serialize");
        assert_eq!(
            value.get("name").and_then(|value| value.as_str()),
            Some("Example")
        );
        assert_eq!(
            value.get("description").and_then(|value| value.as_str()),
            Some("Example prompt")
        );
        assert!(value.get("strict").is_none());
        assert!(
            value
                .get("input_schema")
                .and_then(|schema| schema.get("properties"))
                .and_then(|properties| properties.get("value"))
                .is_some()
        );
        crate::utils::config::set_test_global_config(None);
        crate::utils::tool_schema_cache::clear_tool_schema_cache();
    }

    #[test]
    fn structured_output_cache_key_includes_each_workflow_schema_like_official() {
        crate::utils::tool_schema_cache::clear_tool_schema_cache();
        let make_tool = |property: &str| Tool {
            name: "StructuredOutput".to_string(),
            description: "Return structured output".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {(property): {"type": "string"}},
                "required": [property],
                "additionalProperties": false
            }),
            strict: Some(true),
            ..Default::default()
        };
        let first = tool_to_api_schema(
            &make_tool("first"),
            schema_options(Some("claude-sonnet-4-6"), false),
        )
        .expect("first workflow schema");
        let second = tool_to_api_schema(
            &make_tool("second"),
            schema_options(Some("claude-sonnet-4-6"), false),
        )
        .expect("second workflow schema");
        let first = serde_json::to_value(first).unwrap();
        let second = serde_json::to_value(second).unwrap();
        assert!(first["input_schema"]["properties"].get("first").is_some());
        assert!(second["input_schema"]["properties"].get("second").is_some());
        assert_eq!(
            crate::utils::tool_schema_cache::get_tool_schema_cache().len(),
            2
        );
        crate::utils::tool_schema_cache::clear_tool_schema_cache();
    }

    #[test]
    #[test]
    fn tool_to_api_schema_strips_swarm_fields_when_agent_swarms_are_off() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _teams = EnvVarGuard::unset("CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS");
        crate::utils::tool_schema_cache::clear_tool_schema_cache();
        let tool = crate::tools::agent_tool::agent_tool_schema();
        let raw_props = tool
            .input_schema
            .get("properties")
            .and_then(|value| value.as_object())
            .expect("Agent schema has properties");
        assert!(raw_props.contains_key("name"));
        assert!(raw_props.contains_key("team_name"));
        assert!(raw_props.contains_key("mode"));

        let value = serde_json::to_value(
            tool_to_api_schema(&tool, schema_options(Some("claude-sonnet-4-6"), false))
                .expect("schema projection should succeed"),
        )
        .expect("SDK tool should serialize");
        crate::utils::tool_schema_cache::clear_tool_schema_cache();
        let props = value
            .get("input_schema")
            .and_then(|schema| schema.get("properties"))
            .and_then(|properties| properties.as_object())
            .expect("projected Agent schema has properties");
        // CC `api.ts:163-167` `filterSwarmFieldsFromSchema` when swarms are off.
        assert!(!props.contains_key("name"));
        assert!(!props.contains_key("team_name"));
        assert!(!props.contains_key("mode"));
        assert!(props.contains_key("prompt"));
    }

    #[test]
    fn tool_to_api_schema_marks_deferred_tools_like_official() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS");
        let tool = crate::tools::web_fetch_tool::web_fetch_tool_schema();
        crate::utils::tool_schema_cache::clear_tool_schema_cache();

        let schema = tool_to_api_schema(&tool, schema_options(Some("claude-sonnet-4-6"), true))
            .expect("schema projection should succeed");
        let value = serde_json::to_value(schema).expect("SDK tool should serialize");

        assert_eq!(
            value.get("name").and_then(|value| value.as_str()),
            Some("WebFetch")
        );
        assert_eq!(
            value.get("defer_loading").and_then(|value| value.as_bool()),
            Some(true)
        );
    }

    #[test]
    fn tool_to_api_schema_strips_experimental_fields_when_betas_disabled() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS", "1");
        let mut tool = crate::tools::web_fetch_tool::web_fetch_tool_schema();
        tool.strict = Some(true);
        crate::utils::tool_schema_cache::clear_tool_schema_cache();

        let schema = tool_to_api_schema(&tool, schema_options(Some("claude-sonnet-4-6"), true))
            .expect("schema projection should succeed");
        let value = serde_json::to_value(schema).expect("SDK tool should serialize");

        assert!(value.get("strict").is_none());
        assert!(value.get("defer_loading").is_none());
        assert_eq!(
            value.get("name").and_then(|value| value.as_str()),
            Some("WebFetch")
        );
        assert!(value.get("input_schema").is_some());
        crate::utils::process_env::remove("CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS");
    }

    #[test]
    fn tool_base_cache_and_live_kill_switch_chronology_matches_official() {
        let _env_guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for key in [
            "NODE_ENV",
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
            "CLAUDE_CODE_USE_FOUNDRY",
            "CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS",
            "CLAUDE_CODE_ENABLE_FINE_GRAINED_TOOL_STREAMING",
            "ANTHROPIC_BASE_URL",
            "DISABLE_TELEMETRY",
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
        ] {
            crate::utils::process_env::remove(key);
        }
        crate::utils::tool_schema_cache::clear_tool_schema_cache();
        // The cached gate stays on for the whole chronology to prove it never
        // reaches `strict`; the session-stable base is exercised through the
        // fine-grained-streaming env lever instead.
        let mut config = crate::utils::config::GlobalConfig::default();
        config.cached_growth_book_features = Some(std::collections::HashMap::from([(
            "tengu_tool_pear".to_string(),
            serde_json::json!(true),
        )]));
        crate::utils::config::set_test_global_config(Some(config));
        crate::utils::process_env::set("CLAUDE_CODE_ENABLE_FINE_GRAINED_TOOL_STREAMING", "1");

        let tool = Tool {
            name: "Read".to_string(),
            description: "Read a file".to_string(),
            input_schema: serde_json::json!({"type":"object","properties":{}}),
            strict: Some(true),
            ..Default::default()
        };
        let value = serde_json::to_value(
            tool_to_api_schema(&tool, schema_options(Some("claude-sonnet-4-6"), true)).unwrap(),
        )
        .unwrap();
        assert!(value.get("strict").is_none());
        assert_eq!(value["defer_loading"], serde_json::json!(true));
        assert_eq!(value["eager_input_streaming"], serde_json::json!(true));
        assert!(value.get("output_schema").is_none());

        crate::utils::process_env::remove("CLAUDE_CODE_ENABLE_FINE_GRAINED_TOOL_STREAMING");
        crate::utils::process_env::set("CLAUDE_CODE_USE_VERTEX", "1");
        let cached = serde_json::to_value(
            tool_to_api_schema(&tool, schema_options(Some("unsupported-model"), false)).unwrap(),
        )
        .unwrap();
        assert_eq!(cached["eager_input_streaming"], serde_json::json!(true));
        assert!(cached.get("defer_loading").is_none());

        crate::utils::process_env::set("CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS", "1");
        let killed =
            serde_json::to_value(tool_to_api_schema(&tool, schema_options(None, true)).unwrap())
                .unwrap();
        assert!(killed.get("strict").is_none());
        assert!(killed.get("defer_loading").is_none());
        assert!(killed.get("eager_input_streaming").is_none());
        crate::utils::process_env::remove("CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS");
        let revealed =
            serde_json::to_value(tool_to_api_schema(&tool, schema_options(None, false)).unwrap())
                .unwrap();
        assert!(revealed.get("strict").is_none());
        assert_eq!(revealed["eager_input_streaming"], serde_json::json!(true));

        crate::utils::tool_schema_cache::clear_tool_schema_cache();
        let recomputed = serde_json::to_value(
            tool_to_api_schema(&tool, schema_options(Some("claude-sonnet-4-6"), false)).unwrap(),
        )
        .unwrap();
        assert!(recomputed.get("strict").is_none());
        assert!(recomputed.get("eager_input_streaming").is_none());

        crate::utils::process_env::remove("CLAUDE_CODE_USE_VERTEX");
        crate::utils::process_env::set("CLAUDE_CODE_ENABLE_FINE_GRAINED_TOOL_STREAMING", "1");
        let still_cached_off = serde_json::to_value(
            tool_to_api_schema(&tool, schema_options(Some("claude-sonnet-4-6"), false)).unwrap(),
        )
        .unwrap();
        assert!(still_cached_off.get("strict").is_none());
        assert!(still_cached_off.get("eager_input_streaming").is_none());

        crate::utils::process_env::remove("CLAUDE_CODE_ENABLE_FINE_GRAINED_TOOL_STREAMING");
        crate::utils::config::set_test_global_config(None);
        crate::utils::tool_schema_cache::clear_tool_schema_cache();
    }

    #[test]
    fn independent_schema_and_beta_cache_chronology_matches_official() {
        let _env_guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for key in [
            "NODE_ENV",
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
            "CLAUDE_CODE_USE_FOUNDRY",
            "CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS",
            "CLAUDE_CODE_ENABLE_FINE_GRAINED_TOOL_STREAMING",
            "ANTHROPIC_BASE_URL",
            "DISABLE_TELEMETRY",
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
            "ANTHROPIC_BETAS",
        ] {
            crate::utils::process_env::remove(key);
        }
        let tool = Tool {
            name: "Read".to_string(),
            description: "Read a file".to_string(),
            input_schema: serde_json::json!({"type":"object","properties":{}}),
            strict: Some(true),
            ..Default::default()
        };
        // The provider env is the shared stimulus for both caches now that the
        // strict-tools gate is a source-controlled constant.
        let mut config = crate::utils::config::GlobalConfig::default();
        config.cached_growth_book_features = Some(std::collections::HashMap::from([(
            "tengu_tool_pear".to_string(),
            serde_json::json!(true),
        )]));
        crate::utils::config::set_test_global_config(Some(config));
        crate::utils::process_env::set("CLAUDE_CODE_ENABLE_FINE_GRAINED_TOOL_STREAMING", "1");

        let mut missing_model = tool.clone();
        missing_model.name = "MissingModel".to_string();
        let mut unsupported_model = tool.clone();
        unsupported_model.name = "UnsupportedModel".to_string();
        let mut non_strict = tool.clone();
        non_strict.name = "NonStrict".to_string();
        non_strict.strict = None;
        for schema in [
            tool_to_api_schema(&missing_model, schema_options(None, false)).unwrap(),
            tool_to_api_schema(
                &unsupported_model,
                schema_options(Some("claude-sonnet-4-20250514"), false),
            )
            .unwrap(),
            tool_to_api_schema(
                &non_strict,
                schema_options(Some("claude-sonnet-4-6"), false),
            )
            .unwrap(),
        ] {
            assert!(
                serde_json::to_value(schema)
                    .unwrap()
                    .get("strict")
                    .is_none()
            );
        }

        crate::utils::tool_schema_cache::clear_tool_schema_cache();
        crate::utils::betas::clear_betas_caches();
        crate::utils::process_env::set("CLAUDE_CODE_USE_VERTEX", "1");
        assert!(
            !crate::utils::betas::get_all_model_betas("claude-sonnet-4-6")
                .iter()
                .any(|beta| beta == crate::utils::betas::CONTEXT_MANAGEMENT_BETA_HEADER)
        );
        crate::utils::process_env::remove("CLAUDE_CODE_USE_VERTEX");
        let eager_body = serde_json::to_value(
            tool_to_api_schema(&tool, schema_options(Some("claude-sonnet-4-6"), false)).unwrap(),
        )
        .unwrap();
        assert_eq!(eager_body["eager_input_streaming"], serde_json::json!(true));
        assert!(eager_body.get("strict").is_none());
        assert!(
            !crate::utils::betas::get_all_model_betas("claude-sonnet-4-6")
                .iter()
                .any(|beta| beta == crate::utils::betas::CONTEXT_MANAGEMENT_BETA_HEADER)
        );

        crate::utils::tool_schema_cache::clear_tool_schema_cache();
        crate::utils::betas::clear_betas_caches();
        crate::utils::process_env::set("CLAUDE_CODE_USE_VERTEX", "1");
        let non_eager_body = serde_json::to_value(
            tool_to_api_schema(&tool, schema_options(Some("claude-sonnet-4-6"), false)).unwrap(),
        )
        .unwrap();
        assert!(non_eager_body.get("eager_input_streaming").is_none());
        crate::utils::process_env::remove("CLAUDE_CODE_USE_VERTEX");
        assert!(
            crate::utils::betas::get_all_model_betas("claude-sonnet-4-6")
                .iter()
                .any(|beta| beta == crate::utils::betas::CONTEXT_MANAGEMENT_BETA_HEADER)
        );
        assert!(
            !crate::utils::betas::get_all_model_betas("claude-sonnet-4-6")
                .iter()
                .any(|beta| beta == crate::utils::betas::STRUCTURED_OUTPUTS_BETA_HEADER)
        );
        let same_non_eager_body = serde_json::to_value(
            tool_to_api_schema(&tool, schema_options(Some("claude-sonnet-4-6"), false)).unwrap(),
        )
        .unwrap();
        assert!(same_non_eager_body.get("eager_input_streaming").is_none());

        crate::utils::process_env::remove("CLAUDE_CODE_ENABLE_FINE_GRAINED_TOOL_STREAMING");
        crate::utils::config::set_test_global_config(None);
        crate::utils::betas::clear_betas_caches();
        crate::utils::tool_schema_cache::clear_tool_schema_cache();
    }

    #[test]
    fn kill_switch_live_strip_and_independent_memo_lifecycle_match_official() {
        let _env_guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for key in [
            "NODE_ENV",
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
            "CLAUDE_CODE_USE_FOUNDRY",
            "CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS",
            "CLAUDE_CODE_ENABLE_FINE_GRAINED_TOOL_STREAMING",
            "ANTHROPIC_BASE_URL",
            "DISABLE_TELEMETRY",
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
            "ANTHROPIC_BETAS",
        ] {
            crate::utils::process_env::remove(key);
        }
        let mut config = crate::utils::config::GlobalConfig::default();
        config.cached_growth_book_features = Some(std::collections::HashMap::from([(
            "tengu_tool_pear".to_string(),
            serde_json::json!(true),
        )]));
        crate::utils::config::set_test_global_config(Some(config));
        crate::utils::process_env::set("CLAUDE_CODE_ENABLE_FINE_GRAINED_TOOL_STREAMING", "1");
        let tool = Tool {
            name: "ReadKillLifecycle".to_string(),
            description: "Read a file".to_string(),
            input_schema: serde_json::json!({"type":"object","properties":{}}),
            strict: Some(true),
            ..Default::default()
        };

        crate::utils::tool_schema_cache::clear_tool_schema_cache();
        crate::utils::betas::clear_betas_caches();
        crate::utils::process_env::set("CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS", "1");
        let killed_header = crate::utils::betas::get_all_model_betas("claude-sonnet-4-6");
        let killed_body = serde_json::to_value(
            tool_to_api_schema(&tool, schema_options(Some("claude-sonnet-4-6"), false)).unwrap(),
        )
        .unwrap();
        assert!(
            !killed_header
                .iter()
                .any(|beta| beta == crate::utils::betas::CONTEXT_MANAGEMENT_BETA_HEADER)
        );
        assert!(killed_body.get("eager_input_streaming").is_none());
        assert!(killed_body.get("strict").is_none());

        crate::utils::process_env::remove("CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS");
        let unhidden_body = serde_json::to_value(
            tool_to_api_schema(&tool, schema_options(Some("claude-sonnet-4-6"), false)).unwrap(),
        )
        .unwrap();
        assert_eq!(
            unhidden_body["eager_input_streaming"],
            serde_json::json!(true)
        );
        assert!(unhidden_body.get("strict").is_none());
        assert!(
            !crate::utils::betas::get_all_model_betas("claude-sonnet-4-6")
                .iter()
                .any(|beta| beta == crate::utils::betas::CONTEXT_MANAGEMENT_BETA_HEADER)
        );

        crate::utils::tool_schema_cache::clear_tool_schema_cache();
        crate::utils::betas::clear_betas_caches();
        assert!(
            crate::utils::betas::get_all_model_betas("claude-sonnet-4-6")
                .iter()
                .any(|beta| beta == crate::utils::betas::CONTEXT_MANAGEMENT_BETA_HEADER)
        );
        let warm_body = serde_json::to_value(
            tool_to_api_schema(&tool, schema_options(Some("claude-sonnet-4-6"), false)).unwrap(),
        )
        .unwrap();
        assert_eq!(warm_body["eager_input_streaming"], serde_json::json!(true));
        assert!(warm_body.get("strict").is_none());
        crate::utils::process_env::set("CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS", "1");
        let hidden_warm_body = serde_json::to_value(
            tool_to_api_schema(&tool, schema_options(Some("claude-sonnet-4-6"), false)).unwrap(),
        )
        .unwrap();
        assert!(hidden_warm_body.get("eager_input_streaming").is_none());
        assert!(
            crate::utils::betas::get_all_model_betas("claude-sonnet-4-6")
                .iter()
                .any(|beta| beta == crate::utils::betas::CONTEXT_MANAGEMENT_BETA_HEADER)
        );

        crate::utils::process_env::remove("CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS");
        crate::utils::process_env::remove("CLAUDE_CODE_ENABLE_FINE_GRAINED_TOOL_STREAMING");
        crate::utils::config::set_test_global_config(None);
        crate::utils::betas::clear_betas_caches();
        crate::utils::tool_schema_cache::clear_tool_schema_cache();
    }

    #[test]
    fn tool_schema_cache_keys_and_per_request_overlays_matches_official() {
        let _env_guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        crate::utils::process_env::remove("CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS");
        crate::utils::tool_schema_cache::clear_tool_schema_cache();
        crate::utils::config::set_test_global_config(Some(
            crate::utils::config::GlobalConfig::default(),
        ));

        let built_in_a = Tool {
            name: "Example".to_string(),
            description: "first".to_string(),
            input_schema: serde_json::json!({"type":"object","properties":{"a":{"type":"string"}}}),
            ..Default::default()
        };
        let mut built_in_b = built_in_a.clone();
        built_in_b.description = "second".to_string();
        built_in_b.input_schema =
            serde_json::json!({"type":"object","properties":{"b":{"type":"string"}}});

        let first = serde_json::to_value(
            tool_to_api_schema(&built_in_a, schema_options(None, true)).unwrap(),
        )
        .unwrap();
        let second = serde_json::to_value(
            tool_to_api_schema(&built_in_b, schema_options(None, false)).unwrap(),
        )
        .unwrap();
        assert_eq!(first["description"], serde_json::json!("first"));
        assert_eq!(second["description"], serde_json::json!("first"));
        assert!(first.get("defer_loading").is_some());
        assert!(second.get("defer_loading").is_none());
        assert!(second["input_schema"]["properties"].get("a").is_some());

        let mut mcp_a = built_in_a;
        mcp_a.name = "mcp__server__tool".to_string();
        mcp_a.is_mcp = true;
        let mut mcp_b = built_in_b;
        mcp_b.name = "mcp__server__tool".to_string();
        mcp_b.is_mcp = true;
        let mut mcp_same_schema = mcp_a.clone();
        mcp_same_schema.description = "third".to_string();
        let mcp_first =
            serde_json::to_value(tool_to_api_schema(&mcp_a, schema_options(None, false)).unwrap())
                .unwrap();
        let mcp_second =
            serde_json::to_value(tool_to_api_schema(&mcp_b, schema_options(None, false)).unwrap())
                .unwrap();
        let mcp_same = serde_json::to_value(
            tool_to_api_schema(&mcp_same_schema, schema_options(None, false)).unwrap(),
        )
        .unwrap();
        assert!(mcp_first["input_schema"]["properties"].get("a").is_some());
        assert!(mcp_second["input_schema"]["properties"].get("b").is_some());
        assert_eq!(mcp_same["description"], serde_json::json!("first"));

        crate::utils::config::set_test_global_config(None);
        crate::utils::tool_schema_cache::clear_tool_schema_cache();
    }
}
