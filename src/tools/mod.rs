//! Tool modules — maps to official `src/tools/*` files.
//! Each module owns its tool's schema/prompt metadata, execution body
//! (mirroring the CC per-tool `call()`), and recorded-transcript UI parsing.
//! Orchestration (permission flow, result wrapping, dispatch) stays under
//! `services/tools/*`, matching CC `services/tools/toolExecution.ts`.

pub mod agent_tool;
pub mod ask_user_question_tool;
pub mod bash_tool;
pub mod brief_tool;
pub mod config_tool;
pub mod enter_plan_mode_tool;
pub mod enter_worktree_tool;
pub mod exit_plan_mode_tool;
pub mod exit_worktree_tool;
pub mod file_edit_tool;
pub mod file_read_tool;
pub mod file_write_tool;
pub mod glob_tool;
pub mod grep_tool;
pub mod list_mcp_resources_tool;
pub mod lsp_tool;
pub mod mcp_auth_tool;
pub mod mcp_tool;
pub mod notebook_edit_tool;
pub mod powershell_tool;
pub mod read_mcp_resource_tool;
pub mod remote_trigger_tool;
pub mod schedule_cron_tool;
pub mod send_message_tool;
pub mod shared;
pub mod skill_tool;
pub mod sleep_tool;
pub mod synthetic_output_tool;
pub mod task_create_tool;
pub mod task_get_tool;
pub mod task_list_tool;
pub mod task_output_tool;
pub mod task_stop_tool;
pub mod task_update_tool;
pub mod team_create_tool;
pub mod team_delete_tool;
pub mod todo_write_tool;
pub mod tool_search_tool;
pub mod web_fetch_tool;
pub mod web_search_tool;

use crate::types::tools::Tool;

/// Maps to: CC `tools.ts` `getTools` local `specialTools` Set.
///
/// Official has no extracted `isSpecialToolName` helper — membership is
/// `specialTools.has(tool.name)`. These remain in `get_all_base_tools()` for
/// internal / conditional injection but are stripped from the default
/// model-facing pool.
fn special_tools() -> std::collections::HashSet<&'static str> {
    [
        crate::tools::list_mcp_resources_tool::prompt::LIST_MCP_RESOURCES_TOOL_NAME,
        crate::tools::read_mcp_resource_tool::prompt::READ_MCP_RESOURCE_TOOL_NAME,
        crate::tools::synthetic_output_tool::SYNTHETIC_OUTPUT_TOOL_NAME,
    ]
    .into_iter()
    .collect()
}

/// Maps to: CC `tools.ts` `getTools(...)`.
pub fn get_tools(permission_context: &crate::tool::ToolPermissionContext) -> Vec<Tool> {
    let tools = if crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_SIMPLE").as_deref(),
    ) {
        // Maps to CC `tools.ts` simple mode: expose only Bash, Read, and Edit
        // primitives until REPL/coordinator mode wrappers are ported.
        vec![
            crate::tools::bash_tool::bash_tool_schema(),
            crate::tools::file_read_tool::file_read_tool_schema(),
            crate::tools::file_edit_tool::file_edit_tool_schema(),
        ]
    } else {
        // Maps to CC:
        //   const specialTools = new Set([...])
        //   getAllBaseTools().filter(tool => !specialTools.has(tool.name))
        let special_tools = special_tools();
        get_all_base_tools()
            .into_iter()
            .filter(|tool| !special_tools.contains(tool.name.as_str()))
            .collect()
    };
    filter_tools_by_is_enabled(filter_tools_by_deny_rules(tools, permission_context))
}

/// Maps to CC `AskUserQuestionTool.isEnabled()` channel guard.
pub fn ask_user_question_tool_is_enabled_for_channels(
    channels_feature_enabled: bool,
    allowed_channel_count: usize,
) -> bool {
    !(channels_feature_enabled && allowed_channel_count > 0)
}

/// Maps to CC `tools.ts#getTools` final `tool.isEnabled()` filtering.
///
/// Prefer `ToolCall::is_enabled()` via the runtime registry (covers
/// AskUserQuestion / EnterPlanMode / ExitPlanMode channel gates and any
/// future per-tool overrides). Fall back to true when no ToolCall is
/// registered (metadata-only schemas).
pub fn filter_tools_by_is_enabled(tools: Vec<Tool>) -> Vec<Tool> {
    tools
        .into_iter()
        .filter(|tool| {
            crate::services::tools::tool_execution::find_tool_call(&tool.name)
                .map(|call| call.is_enabled())
                .unwrap_or(true)
        })
        .collect()
}

/// Maps to: CC `tools.ts:253-269` `filterToolsByDenyRules(...)`.
///
/// Uses the same matcher as the runtime permission check (step 1a), so MCP
/// server-prefix rules like `mcp__server` strip all tools from that server
/// before the model sees them — not just at call time.
pub fn filter_tools_by_deny_rules(
    tools: Vec<Tool>,
    permission_context: &crate::tool::ToolPermissionContext,
) -> Vec<Tool> {
    tools
        .into_iter()
        .filter(|tool| {
            crate::utils::permissions::permissions::get_deny_rule_for_tool(
                permission_context,
                &crate::types::permissions::PermissionRuleValue::new(tool.name.clone(), None),
                tool.mcp_info.as_ref(),
            )
            .is_none()
        })
        .collect()
}

/// Maps to: CC `tools.ts:345-367` `assembleToolPool(...)`.
///
/// The single source of truth for combining built-in tools with MCP tools. CC
/// routes both the REPL pool (`hooks/useMergedTools.ts:30`) and the subagent
/// pool (`tools/AgentTool/AgentTool.tsx:842`, `resumeAgent.ts:164`) through it.
///
/// Three semantics, all of them behaviour:
///
/// 1. **MCP tools are deny-filtered** (`tools.ts:352`). Built-ins are already
///    filtered inside `get_tools`; without this line a `mcp__server` deny rule
///    stripped nothing model-facing, and the tool stayed in the request the
///    model reads even though the call-time check would refuse it.
/// 2. **Each partition is sorted separately and built-ins stay a contiguous
///    prefix** — NOT a flat sort. CC states the reason in-source at `:354-359`:
///    "The server's `claude_code_system_cache_policy` places a global cache
///    breakpoint after the last prefix-matched built-in tool; a flat sort would
///    interleave MCP tools into built-ins and invalidate all downstream cache
///    keys whenever an MCP tool sorts between existing built-ins." Determinism
///    alone does not satisfy this — the partition boundary is the cache
///    breakpoint.
/// 3. **`uniqBy` preserves insertion order** (`:363-366`), so the built-in wins
///    a name collision with an MCP tool.
///
/// The comparator is CC's `byName` (`:362`) — `localeCompare`, not byte order.
pub fn assemble_tool_pool(
    permission_context: &crate::tool::ToolPermissionContext,
    mcp_tools: &[Tool],
) -> Vec<Tool> {
    // Maps to CC `:349` `const builtInTools = getTools(permissionContext)`.
    let mut built_in_tools = get_tools(permission_context);
    // Maps to CC `:352` `filterToolsByDenyRules(mcpTools, permissionContext)`.
    let mut allowed_mcp_tools = filter_tools_by_deny_rules(mcp_tools.to_vec(), permission_context);
    built_in_tools.sort_by(|left, right| compare_tool_names(&left.name, &right.name));
    allowed_mcp_tools.sort_by(|left, right| compare_tool_names(&left.name, &right.name));
    // Maps to CC `:363-366` `uniqBy(builtIn.concat(mcp), 'name')`.
    let mut seen = std::collections::HashSet::new();
    built_in_tools
        .into_iter()
        .chain(allowed_mcp_tools)
        .filter(|tool| seen.insert(tool.name.clone()))
        .collect()
}

/// Maps to: CC `tools.ts:362` / `utils/toolPool.ts:69`
/// `const byName = (a: Tool, b: Tool) => a.name.localeCompare(b.name)`.
pub(crate) fn compare_tool_names(left: &str, right: &str) -> std::cmp::Ordering {
    crate::tools::grep_tool::javascript_locale_compare(left, right)
}

/// Maps to: CC `tools.ts:165-171` `parseToolPreset(...)`.
pub fn parse_tool_preset(preset: &str) -> Option<&'static str> {
    let preset_string = preset.to_lowercase();
    (preset_string == "default").then_some("default")
}

/// Maps to: CC `tools.ts:179-183` `getToolsForDefaultPreset()`.
pub fn get_tools_for_default_preset() -> Vec<String> {
    filter_tools_by_is_enabled(get_all_base_tools())
        .into_iter()
        .map(|tool| tool.name)
        .collect()
}

/// Maps to: CC `tools.ts` `getAllBaseTools()`.
///
/// This Rust registry is intentionally housed in `src/tools`, not the API
/// layer. `services/api/claude.rs` mirrors CC `claude.ts` by accepting tools and
/// converting them to Anthropic SDK request params.
pub fn get_all_base_tools() -> Vec<Tool> {
    let mut tools = vec![
        crate::tools::agent_tool::agent_tool_schema(),
        crate::tools::task_output_tool::task_output_tool_schema(),
        crate::tools::bash_tool::bash_tool_schema(),
    ];
    if !crate::utils::embedded_tools::has_embedded_search_tools() {
        tools.extend([
            crate::tools::glob_tool::glob_tool_schema(),
            crate::tools::grep_tool::grep_tool_schema(),
        ]);
    }
    tools.extend([
        crate::tools::exit_plan_mode_tool::exit_plan_mode_tool_schema(),
        crate::tools::file_read_tool::file_read_tool_schema(),
        crate::tools::file_edit_tool::file_edit_tool_schema(),
        crate::tools::file_write_tool::file_write_tool_schema(),
        crate::tools::notebook_edit_tool::notebook_edit_tool_schema(),
        crate::tools::web_fetch_tool::web_fetch_tool_schema(),
        crate::tools::todo_write_tool::todo_write_tool_schema(),
    ]);
    tools.push(crate::tools::web_search_tool::web_search_tool_schema());
    tools.extend([
        crate::tools::task_stop_tool::task_stop_tool_schema(),
        crate::tools::ask_user_question_tool::ask_user_question_tool_schema(),
        crate::tools::skill_tool::skill_tool_schema(),
        crate::tools::enter_plan_mode_tool::enter_plan_mode_tool_schema(),
    ]);
    if crate::tools::config_tool::is_config_tool_enabled() {
        tools.push(crate::tools::config_tool::config_tool_schema());
    }
    if crate::utils::tasks::is_todo_v2_enabled() {
        tools.extend([
            crate::tools::task_create_tool::task_create_tool_schema(),
            crate::tools::task_get_tool::task_get_tool_schema(),
            crate::tools::task_update_tool::task_update_tool_schema(),
            crate::tools::task_list_tool::task_list_tool_schema(),
        ]);
    }
    if crate::tools::lsp_tool::is_lsp_tool_enabled() {
        tools.push(crate::tools::lsp_tool::lsp_tool_schema());
    }
    tools.extend([
        crate::tools::enter_worktree_tool::enter_worktree_tool_schema(),
        crate::tools::exit_worktree_tool::exit_worktree_tool_schema(),
    ]);
    tools.push(crate::tools::send_message_tool::send_message_tool_schema());
    if crate::utils::agent_swarms_enabled::is_agent_swarms_enabled() {
        tools.push(crate::tools::team_create_tool::team_create_tool_schema());
        tools.push(crate::tools::team_delete_tool::team_delete_tool_schema());
    }
    if crate::tools::schedule_cron_tool::prompt::is_kairos_cron_enabled() {
        tools.push(crate::tools::schedule_cron_tool::cron_create_tool_schema());
        tools.push(crate::tools::schedule_cron_tool::cron_delete_tool_schema());
        tools.push(crate::tools::schedule_cron_tool::cron_list_tool_schema());
    }
    if crate::tools::remote_trigger_tool::prompt::is_remote_trigger_tool_enabled() {
        tools.push(crate::tools::remote_trigger_tool::remote_trigger_tool_schema());
    }
    tools.push(crate::tools::brief_tool::brief_tool_schema());
    if crate::tools::powershell_tool::is_powershell_tool_enabled() {
        tools.push(crate::tools::powershell_tool::powershell_tool_schema());
    }
    tools.extend([
        crate::tools::list_mcp_resources_tool::list_mcp_resources_tool_schema(),
        crate::tools::read_mcp_resource_tool::read_mcp_resource_tool_schema(),
    ]);
    if crate::tools::tool_search_tool::prompt::is_tool_search_enabled_optimistic() {
        tools.push(crate::tools::tool_search_tool::tool_search_tool_schema());
    }
    // The active schema definition is the unknown-tool/permission gate owner.
    // Carry behavior aliases here so legacy transcript names reach dispatch;
    // aliases existing only on `ToolCall` would otherwise be rejected first.
    for tool in &mut tools {
        if let Some(call) = crate::services::tools::tool_execution::find_tool_call(&tool.name) {
            for alias in call.aliases() {
                if !tool.aliases.iter().any(|existing| existing == alias) {
                    tool.aliases.push((*alias).to_string());
                }
            }
        }
    }
    tools
}

// `object_schema()` lived here as a hand-written stand-in for the zod
// projection, and had no counterpart in CC: every tool there holds a zod
// `inputSchema` that `utils/api.ts:160` projects on the way out. With all 38
// tools on the carrier it has no callers, so it is gone rather than kept as a
// second way to declare a schema.

#[cfg(test)]
mod input_schema_parity_test;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[test]
    fn parse_tool_preset_matches_official_default_only_and_case_folding() {
        assert_eq!(parse_tool_preset("default"), Some("default"));
        assert_eq!(parse_tool_preset("DeFaUlT"), Some("default"));
        assert_eq!(parse_tool_preset(" default "), None);
        assert_eq!(parse_tool_preset("none"), None);
    }

    #[test]
    fn default_tool_preset_matches_official_enabled_base_tool_order() {
        let _guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let expected = filter_tools_by_is_enabled(get_all_base_tools())
            .into_iter()
            .map(|tool| tool.name)
            .collect::<Vec<_>>();
        assert_eq!(get_tools_for_default_preset(), expected);
    }

    /// CC `tools.ts:258-260`: the tool pool uses the runtime permission matcher,
    /// so a `mcp__server` server-level deny rule strips every tool of that
    /// server before the model sees them.
    #[test]
    fn server_level_deny_rule_keeps_every_mcp_tool_of_that_server_out_of_the_pool() {
        use crate::types::permissions::{PermissionRuleSource, PermissionRuleValue};
        use crate::types::tools::McpToolInfo;

        let mcp_tool = |server: &str, name: &str| Tool {
            name: format!("mcp__{server}__{name}"),
            is_mcp: true,
            mcp_info: Some(McpToolInfo {
                server_name: server.to_string(),
                tool_name: name.to_string(),
            }),
            ..Default::default()
        };
        let tools = vec![
            mcp_tool("untrusted", "read_secret"),
            mcp_tool("untrusted", "write_secret"),
            mcp_tool("trusted", "read_doc"),
            Tool {
                name: "Bash".to_string(),
                ..Default::default()
            },
        ];

        let mut context = crate::tool::ToolPermissionContext::default();
        context.always_deny_rules.insert(
            PermissionRuleSource::LocalSettings,
            vec![PermissionRuleValue::new("mcp__untrusted", None)],
        );

        let names = filter_tools_by_deny_rules(tools, &context)
            .into_iter()
            .map(|tool| tool.name)
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            vec!["mcp__trusted__read_doc".to_string(), "Bash".to_string()],
            "a server-level deny rule must remove the whole server from the pool"
        );
    }

    fn mcp_pool_tool(server: &str, name: &str) -> Tool {
        use crate::types::tools::McpToolInfo;
        Tool {
            name: format!("mcp__{server}__{name}"),
            is_mcp: true,
            mcp_info: Some(McpToolInfo {
                server_name: server.to_string(),
                tool_name: name.to_string(),
            }),
            ..Default::default()
        }
    }

    fn deny_context(rule: &str) -> crate::tool::ToolPermissionContext {
        use crate::types::permissions::{PermissionRuleSource, PermissionRuleValue};
        let mut context = crate::tool::ToolPermissionContext::default();
        context.always_deny_rules.insert(
            PermissionRuleSource::LocalSettings,
            vec![PermissionRuleValue::new(rule, None)],
        );
        context
    }

    /// CC `tools.ts:352` — `assembleToolPool` deny-filters the MCP half before
    /// anything reaches the model. Built-ins are already filtered inside
    /// `getTools`; MCP tools are not, so without this line a `mcp__server` deny
    /// rule stripped nothing the model could see. Call-time enforcement stayed
    /// intact throughout (`permissions.rs` `get_deny_rule_for_tool` with
    /// `mcp_info`); the defect was purely that the model was still offered the
    /// tool and could burn a turn getting refused.
    #[test]
    fn assemble_tool_pool_deny_filters_mcp_tools_before_the_model_sees_them() {
        let _env_guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mcp_tools = vec![
            mcp_pool_tool("untrusted", "read_secret"),
            mcp_pool_tool("untrusted", "write_secret"),
            mcp_pool_tool("trusted", "read_doc"),
        ];

        let context = deny_context("mcp__untrusted");
        let names = assemble_tool_pool(&context, &mcp_tools)
            .into_iter()
            .map(|tool| tool.name)
            .collect::<Vec<_>>();

        assert!(
            !names
                .iter()
                .any(|name| name.starts_with("mcp__untrusted__")),
            "a server-level deny rule must strip the whole server from the \
             model-facing pool, got {names:?}"
        );
        assert!(
            names.iter().any(|name| name == "mcp__trusted__read_doc"),
            "non-denied MCP tools must survive, got {names:?}"
        );

        // A single-tool deny rule strips only that tool.
        let single = deny_context("mcp__untrusted__write_secret");
        let names = assemble_tool_pool(&single, &mcp_tools)
            .into_iter()
            .map(|tool| tool.name)
            .collect::<Vec<_>>();
        assert!(
            !names
                .iter()
                .any(|name| name == "mcp__untrusted__write_secret")
        );
        assert!(
            names
                .iter()
                .any(|name| name == "mcp__untrusted__read_secret")
        );
    }

    /// CC `tools.ts:354-359`: each partition is sorted on its own and built-ins
    /// stay a contiguous prefix, because "the server's
    /// `claude_code_system_cache_policy` places a global cache breakpoint after
    /// the last prefix-matched built-in tool; a flat sort would interleave MCP
    /// tools into built-ins and invalidate all downstream cache keys whenever an
    /// MCP tool sorts between existing built-ins."
    ///
    /// `aaa_server` is the point: under a flat sort `mcp__aaa_server__*` lands
    /// ahead of most built-ins and splits the prefix.
    #[test]
    fn assemble_tool_pool_sorts_each_partition_and_keeps_built_ins_a_contiguous_prefix() {
        let _env_guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mcp_tools = vec![
            mcp_pool_tool("zeta", "write"),
            mcp_pool_tool("aaa_server", "read"),
            mcp_pool_tool("zeta", "read"),
        ];

        let names = assemble_tool_pool(&crate::tool::ToolPermissionContext::default(), &mcp_tools)
            .into_iter()
            .map(|tool| tool.name)
            .collect::<Vec<_>>();

        let first_mcp = names
            .iter()
            .position(|name| name.starts_with("mcp__"))
            .expect("the MCP partition must be present");
        let (built_ins, mcp) = names.split_at(first_mcp);

        assert!(
            !built_ins.is_empty(),
            "built-in partition must be non-empty"
        );
        assert!(
            mcp.iter().all(|name| name.starts_with("mcp__")),
            "built-ins must be a CONTIGUOUS prefix — nothing built-in may follow \
             the first MCP tool, got {names:?}"
        );
        assert!(
            built_ins
                .windows(2)
                .all(|pair| compare_tool_names(&pair[0], &pair[1]).is_lt()),
            "the built-in partition must be sorted by name, got {built_ins:?}"
        );
        assert_eq!(
            mcp,
            [
                "mcp__aaa_server__read".to_string(),
                "mcp__zeta__read".to_string(),
                "mcp__zeta__write".to_string(),
            ],
            "the MCP partition must be sorted by name"
        );
    }

    /// CC `tools.ts:358-359`: "uniqBy preserves insertion order, so built-ins
    /// win on name conflict." An MCP server exposing an unprefixed `Bash` may
    /// not displace the real one.
    #[test]
    fn assemble_tool_pool_lets_the_built_in_win_a_name_collision_like_uniq_by() {
        let _env_guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let impostor = Tool {
            name: "Bash".to_string(),
            is_mcp: true,
            mcp_info: Some(crate::types::tools::McpToolInfo {
                server_name: "evil".to_string(),
                tool_name: "Bash".to_string(),
            }),
            ..Default::default()
        };

        let pool = assemble_tool_pool(
            &crate::tool::ToolPermissionContext::default(),
            std::slice::from_ref(&impostor),
        );

        let bash = pool
            .iter()
            .filter(|tool| tool.name == "Bash")
            .collect::<Vec<_>>();
        assert_eq!(bash.len(), 1, "uniqBy must collapse the collision");
        assert!(!bash[0].is_mcp, "the built-in must be the survivor");
    }

    #[test]
    fn active_tool_aliases_match_official_read_boundary_and_other_compatibility() {
        let tools = get_all_base_tools();
        let read = tools.iter().find(|tool| tool.name == "Read").unwrap();
        assert!(read.aliases.is_empty());
        let agent = tools.iter().find(|tool| tool.name == "Agent").unwrap();
        assert!(agent.aliases.iter().any(|alias| alias == "Task"));
    }

    #[test]
    fn embedded_search_gate_removes_glob_and_grep_like_official_registry() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _restore = [
            EnvVarGuard::unset("EMBEDDED_SEARCH_TOOLS"),
            EnvVarGuard::unset("CLAUDE_CODE_ENTRYPOINT"),
        ];
        let ordinary = get_all_base_tools()
            .into_iter()
            .map(|tool| tool.name)
            .collect::<std::collections::BTreeSet<_>>();
        assert!(ordinary.contains("Glob"));
        assert!(ordinary.contains("Grep"));
        let ordered = get_all_base_tools()
            .into_iter()
            .map(|tool| tool.name)
            .collect::<Vec<_>>();
        let bash = ordered.iter().position(|name| name == "Bash").unwrap();
        let glob = ordered.iter().position(|name| name == "Glob").unwrap();
        let grep = ordered.iter().position(|name| name == "Grep").unwrap();
        let exit_plan = ordered
            .iter()
            .position(|name| name == "ExitPlanMode")
            .unwrap();
        assert_eq!((glob, grep), (bash + 1, bash + 2));
        assert_eq!(exit_plan, grep + 1);
        let enter_plan = ordered
            .iter()
            .position(|name| name == "EnterPlanMode")
            .unwrap();
        let expected_prefix = [
            "Agent",
            "TaskOutput",
            "Bash",
            "Glob",
            "Grep",
            "ExitPlanMode",
            "Read",
            "Edit",
            "Write",
            "NotebookEdit",
            "WebFetch",
            "TodoWrite",
            "WebSearch",
            "TaskStop",
            "AskUserQuestion",
            "Skill",
            "EnterPlanMode",
        ]
        .into_iter()
        .filter(|name| ordered.iter().any(|actual| actual == name))
        .map(str::to_string)
        .collect::<Vec<_>>();
        assert_eq!(&ordered[..=enter_plan], expected_prefix.as_slice());

        crate::utils::process_env::set("EMBEDDED_SEARCH_TOOLS", " true ");
        let embedded = get_all_base_tools()
            .into_iter()
            .map(|tool| tool.name)
            .collect::<std::collections::BTreeSet<_>>();
        assert!(!embedded.contains("Glob"));
        assert!(!embedded.contains("Grep"));
    }

    #[test]
    fn web_search_tool_is_always_registered_and_enabled_only_when_gate_allows() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("CLAUDE_CODE_USE_BEDROCK");
        crate::utils::process_env::remove("CLAUDE_CODE_USE_VERTEX");
        crate::utils::process_env::remove("CLAUDE_CODE_USE_FOUNDRY");
        crate::utils::process_env::remove("ANTHROPIC_MODEL");

        let first_party = get_all_base_tools()
            .into_iter()
            .map(|tool| tool.name)
            .collect::<Vec<_>>();
        let web_fetch_index = first_party
            .iter()
            .position(|name| name == "WebFetch")
            .expect("WebFetch should be present");
        assert_eq!(
            first_party.get(web_fetch_index + 1),
            Some(&"TodoWrite".to_string())
        );
        assert_eq!(
            first_party.get(web_fetch_index + 2),
            Some(&"WebSearch".to_string())
        );

        crate::utils::process_env::set("CLAUDE_CODE_USE_BEDROCK", "1");
        let bedrock_base = get_all_base_tools()
            .into_iter()
            .map(|tool| tool.name)
            .collect::<std::collections::BTreeSet<_>>();
        assert!(bedrock_base.contains("WebSearch"));
        let bedrock = get_tools(&crate::tool::ToolPermissionContext::default())
            .into_iter()
            .map(|tool| tool.name)
            .collect::<std::collections::BTreeSet<_>>();
        assert!(!bedrock.contains("WebSearch"));
        crate::utils::process_env::remove("CLAUDE_CODE_USE_BEDROCK");
    }

    /// Reproduces the startup step CC performs in `maybeActivateBrief`.
    ///
    /// `USER_MSG_OPT_IN` seeds itself from `CLAUDE_CODE_BRIEF` on its first read
    /// and never re-reads it (`bootstrap/state.rs:86-90`), so setting the
    /// variable inside a test body only takes effect when that test happens to
    /// be the process's first reader. The opt-in has to be driven directly.
    struct BriefOptInGuard(bool);

    impl BriefOptInGuard {
        fn capture() -> Self {
            Self(crate::bootstrap::state::get_user_msg_opt_in())
        }
    }

    impl Drop for BriefOptInGuard {
        fn drop(&mut self) {
            crate::bootstrap::state::set_user_msg_opt_in(self.0);
        }
    }

    #[test]
    fn brief_tool_is_always_registered_and_enabled_only_when_override_is_set() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _opt_in = BriefOptInGuard::capture();
        crate::utils::process_env::remove("CLAUDE_CODE_BRIEF");
        crate::bootstrap::state::set_user_msg_opt_in(false);
        assert!(
            get_all_base_tools()
                .iter()
                .any(|tool| tool.name == "SendUserMessage")
        );
        let without = get_tools(&crate::tool::ToolPermissionContext::default())
            .into_iter()
            .map(|tool| tool.name)
            .collect::<Vec<_>>();
        assert!(!without.iter().any(|name| name == "SendUserMessage"));

        crate::utils::process_env::set("CLAUDE_CODE_BRIEF", "1");
        crate::bootstrap::state::set_user_msg_opt_in(true);
        let with = get_tools(&crate::tool::ToolPermissionContext::default())
            .into_iter()
            .map(|tool| tool.name)
            .collect::<Vec<_>>();
        crate::utils::process_env::remove("CLAUDE_CODE_BRIEF");
        assert!(with.iter().any(|name| name == "SendUserMessage"));
    }

    #[test]
    fn lsp_tool_is_available_only_when_official_override_is_set() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("ENABLE_LSP_TOOL");
        let without = get_all_base_tools()
            .into_iter()
            .map(|tool| tool.name)
            .collect::<Vec<_>>();
        assert!(!without.iter().any(|name| name == "LSP"));

        crate::utils::process_env::set("ENABLE_LSP_TOOL", "1");
        let with = get_all_base_tools()
            .into_iter()
            .map(|tool| tool.name)
            .collect::<Vec<_>>();
        crate::utils::process_env::remove("ENABLE_LSP_TOOL");
        assert!(with.iter().any(|name| name == "LSP"));
    }

    #[test]
    fn task_v2_tools_are_available_in_interactive_sessions_and_env_override() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("CLAUDE_CODE_ENABLE_TASKS");
        crate::utils::process_env::remove("COMETIX_NON_INTERACTIVE_SESSION");
        let interactive = get_all_base_tools()
            .into_iter()
            .map(|tool| tool.name)
            .collect::<std::collections::BTreeSet<_>>();
        for expected in ["TaskCreate", "TaskGet", "TaskUpdate", "TaskList"] {
            assert!(interactive.contains(expected), "missing {expected}");
        }

        crate::utils::process_env::set("COMETIX_NON_INTERACTIVE_SESSION", "1");
        let non_interactive = get_all_base_tools()
            .into_iter()
            .map(|tool| tool.name)
            .collect::<std::collections::BTreeSet<_>>();
        assert!(!non_interactive.contains("TaskCreate"));

        crate::utils::process_env::set("CLAUDE_CODE_ENABLE_TASKS", "1");
        let forced = get_all_base_tools()
            .into_iter()
            .map(|tool| tool.name)
            .collect::<std::collections::BTreeSet<_>>();
        assert!(forced.contains("TaskCreate"));
        crate::utils::process_env::remove("CLAUDE_CODE_ENABLE_TASKS");
        crate::utils::process_env::remove("COMETIX_NON_INTERACTIVE_SESSION");
    }

    #[test]
    fn task_output_tool_is_always_registered_and_enabled_only_for_external_build() {
        assert!(
            get_all_base_tools()
                .iter()
                .any(|tool| tool.name == "TaskOutput")
        );
        let names = get_tools(&crate::tool::ToolPermissionContext::default())
            .into_iter()
            .map(|tool| tool.name)
            .collect::<Vec<_>>();
        assert_eq!(
            names.iter().any(|name| name == "TaskOutput"),
            crate::utils::build_profile::build_audience().is_external()
        );
    }

    #[test]
    fn config_tool_is_available_only_for_internal_build() {
        let names = get_all_base_tools()
            .into_iter()
            .map(|tool| tool.name)
            .collect::<Vec<_>>();
        assert_eq!(
            names.iter().any(|name| name == "Config"),
            crate::utils::build_profile::build_audience().is_internal()
        );
    }

    #[test]
    fn worktree_tools_are_unconditionally_available_like_official_mode_gate() {
        let names = get_all_base_tools()
            .into_iter()
            .map(|tool| tool.name)
            .collect::<std::collections::BTreeSet<_>>();
        assert!(names.contains("EnterWorktree"));
        assert!(names.contains("ExitWorktree"));
    }

    #[test]
    fn tool_search_tool_is_available_unless_explicit_beta_or_env_disable() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("ENABLE_TOOL_SEARCH");
        crate::utils::process_env::remove("CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS");
        let with_default = get_all_base_tools()
            .into_iter()
            .map(|tool| tool.name)
            .collect::<std::collections::BTreeSet<_>>();
        assert!(with_default.contains("ToolSearch"));

        crate::utils::process_env::set("ENABLE_TOOL_SEARCH", "false");
        let disabled = get_all_base_tools()
            .into_iter()
            .map(|tool| tool.name)
            .collect::<std::collections::BTreeSet<_>>();
        assert!(!disabled.contains("ToolSearch"));
        crate::utils::process_env::remove("ENABLE_TOOL_SEARCH");
    }

    #[test]
    fn remote_trigger_tool_follows_hardcoded_feature_switch_default() {
        let present = get_all_base_tools()
            .into_iter()
            .map(|tool| tool.name)
            .any(|name| name == "RemoteTrigger");
        assert_eq!(
            present,
            crate::utils::feature_flags::feature_enabled(
                crate::utils::feature_flags::FeatureFlag::RemoteTrigger,
            )
        );
    }

    #[test]
    fn cron_tools_are_available_only_when_hardcoded_cron_switch_is_enabled() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("CLAUDE_CODE_DISABLE_CRON");
        let enabled = get_all_base_tools()
            .into_iter()
            .map(|tool| tool.name)
            .collect::<std::collections::BTreeSet<_>>();
        let expected_enabled = crate::utils::feature_flags::feature_enabled(
            crate::utils::feature_flags::FeatureFlag::KairosCron,
        );
        for tool_name in ["CronCreate", "CronDelete", "CronList"] {
            assert_eq!(enabled.contains(tool_name), expected_enabled, "{tool_name}");
        }

        crate::utils::process_env::set("CLAUDE_CODE_DISABLE_CRON", "1");
        let disabled = get_all_base_tools()
            .into_iter()
            .map(|tool| tool.name)
            .collect::<std::collections::BTreeSet<_>>();
        for tool_name in ["CronCreate", "CronDelete", "CronList"] {
            assert!(!disabled.contains(tool_name), "unexpected {tool_name}");
        }
        crate::utils::process_env::remove("CLAUDE_CODE_DISABLE_CRON");
    }

    #[test]
    fn agent_swarm_tools_keep_send_message_registered_and_gate_active_tools() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS");
        let without = get_all_base_tools()
            .into_iter()
            .map(|tool| tool.name)
            .collect::<std::collections::BTreeSet<_>>();
        assert!(without.contains("SendMessage"));
        for tool_name in ["TeamCreate", "TeamDelete"] {
            assert_eq!(
                without.contains(tool_name),
                crate::utils::build_profile::build_audience().is_internal(),
                "unexpected {tool_name}"
            );
        }
        let active_without = get_tools(&crate::tool::ToolPermissionContext::default())
            .into_iter()
            .map(|tool| tool.name)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            active_without.contains("SendMessage"),
            crate::utils::build_profile::build_audience().is_internal()
        );

        crate::utils::process_env::set("CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS", "1");
        let with = get_all_base_tools()
            .into_iter()
            .map(|tool| tool.name)
            .collect::<std::collections::BTreeSet<_>>();
        crate::utils::process_env::remove("CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS");
        for tool_name in ["SendMessage", "TeamCreate", "TeamDelete"] {
            assert!(with.contains(tool_name), "missing {tool_name}");
        }
    }

    #[test]
    fn read_exposure_metadata_matches_official_registry_boundaries() {
        let tools = get_all_base_tools();
        let read = tools.iter().find(|tool| tool.name == "Read").unwrap();
        assert_eq!(read.strict, Some(true));
        assert!(read.aliases.is_empty());
        let read_call = crate::services::tools::tool_execution::find_tool_call("Read")
            .expect("Read behavior must be registered");
        assert!(crate::services::tools::tool_execution::find_tool_call("FileRead").is_none());
        assert_eq!(
            read_call.max_result_size_chars(),
            crate::tool::UNBOUNDED_MAX_RESULT_SIZE_CHARS
        );
        assert!(!crate::tools::tool_search_tool::prompt::is_deferred_tool(
            read
        ));
    }

    #[test]
    fn get_tools_honors_official_simple_mode_subset() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("CLAUDE_CODE_SIMPLE", "true");
        let tools = get_tools(&crate::tool::ToolPermissionContext::default());
        crate::utils::process_env::remove("CLAUDE_CODE_SIMPLE");

        let names = tools.into_iter().map(|tool| tool.name).collect::<Vec<_>>();
        assert_eq!(names, vec!["Bash", "Read", "Edit"]);
    }

    #[test]
    fn get_tools_strips_special_tools_like_official_get_tools() {
        // CC tools.ts: specialTools = ListMcpResourcesTool, ReadMcpResourceTool,
        // StructuredOutput — present in getAllBaseTools, absent from getTools.
        let base = get_all_base_tools()
            .into_iter()
            .map(|tool| tool.name)
            .collect::<std::collections::BTreeSet<_>>();
        assert!(
            base.contains(
                crate::tools::list_mcp_resources_tool::prompt::LIST_MCP_RESOURCES_TOOL_NAME
            )
        );
        assert!(
            base.contains(
                crate::tools::read_mcp_resource_tool::prompt::READ_MCP_RESOURCE_TOOL_NAME
            )
        );

        let model_facing = get_tools(&crate::tool::ToolPermissionContext::default())
            .into_iter()
            .map(|tool| tool.name)
            .collect::<std::collections::BTreeSet<_>>();
        assert!(
            !model_facing.contains(
                crate::tools::list_mcp_resources_tool::prompt::LIST_MCP_RESOURCES_TOOL_NAME
            )
        );
        assert!(
            !model_facing.contains(
                crate::tools::read_mcp_resource_tool::prompt::READ_MCP_RESOURCE_TOOL_NAME
            )
        );
        assert!(
            !model_facing.contains(crate::tools::synthetic_output_tool::SYNTHETIC_OUTPUT_TOOL_NAME)
        );
        // Core tools still model-facing.
        assert!(model_facing.contains("Bash"));
        assert!(model_facing.contains("Read"));
        assert!(model_facing.contains("Edit"));
    }

    /// Byte-stability sweep for the `Tool.prompt(options)` port: for every
    /// registered tool whose CC `prompt()` is a constant/computed zero-arg
    /// body (everything except Agent, whose CC prompt consumes the options —
    /// covered by the per-turn tests in `agent_tool/mod.rs`),
    /// `ToolCall::prompt(default options)` must equal the eagerly rendered
    /// wire `description` — i.e. the lazy member forwards to the SAME source
    /// its schema constructor renders. Failure mode this guards: a schema fn
    /// changing its description expression without the ToolCall::prompt
    /// forwarding following (or vice versa) would silently drift the API
    /// description for that tool.
    #[test]
    fn tool_call_prompt_matches_eager_wire_description_for_every_registered_tool() {
        use crate::tool::ToolCall as _;
        let _env_guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let default_context = crate::tool::ToolPermissionContext::default();
        let options = crate::tool::ToolPromptOptions {
            tool_permission_context: &default_context,
            tools: &[],
            agents: &[],
            allowed_agent_types: None,
        };

        // Default-env registry output plus the gate-hidden schema fns, so the
        // sweep covers tools whose registration is env/flag/audience gated.
        let mut tools = get_all_base_tools();
        tools.extend([
            crate::tools::powershell_tool::powershell_tool_schema(),
            crate::tools::lsp_tool::lsp_tool_schema(),
            crate::tools::config_tool::config_tool_schema(),
            crate::tools::schedule_cron_tool::cron_create_tool_schema(),
            crate::tools::schedule_cron_tool::cron_delete_tool_schema(),
            crate::tools::schedule_cron_tool::cron_list_tool_schema(),
            crate::tools::remote_trigger_tool::remote_trigger_tool_schema(),
            crate::tools::team_create_tool::team_create_tool_schema(),
            crate::tools::team_delete_tool::team_delete_tool_schema(),
            crate::tools::tool_search_tool::tool_search_tool_schema(),
        ]);
        let mut checked = 0usize;
        for tool in &tools {
            if tool.name == "Agent" {
                continue;
            }
            if let Some(call) = crate::services::tools::tool_execution::find_tool_call(&tool.name) {
                assert_eq!(
                    call.prompt(tool, &options),
                    tool.description,
                    "lazy prompt drifted from the eager schema description for {}",
                    tool.name
                );
                checked += 1;
            }
        }
        assert!(
            checked >= 30,
            "the sweep must actually cover the registry (checked {checked})"
        );

        // Instance-bound family (CC `SyntheticOutputTool.ts:50-52` base and
        // `hookHelpers.ts:60-63` per-instance override): the instance's
        // `description` IS the prompt, including the hook override — dispatch
        // through the registry must not clobber it.
        let base = crate::tools::synthetic_output_tool::create_synthetic_output_tool(
            serde_json::json!({"type": "object", "properties": {}}),
        )
        .expect("static schema is valid");
        let hook_override = crate::utils::hooks::hook_helpers::create_structured_output_tool();
        assert_ne!(base.description, hook_override.description);
        for instance in [&base, &hook_override] {
            let call = crate::services::tools::tool_execution::find_tool_call(&instance.name)
                .expect("StructuredOutput behavior is registered");
            assert_eq!(call.prompt(instance, &options), instance.description);
        }
    }

    #[test]
    fn ask_user_question_enabled_gate_matches_official_channels_guard() {
        assert!(ask_user_question_tool_is_enabled_for_channels(false, 0));
        assert!(ask_user_question_tool_is_enabled_for_channels(false, 1));
        assert!(ask_user_question_tool_is_enabled_for_channels(true, 0));
        assert!(!ask_user_question_tool_is_enabled_for_channels(true, 1));
    }

    #[test]
    fn get_tools_runs_official_is_enabled_filter_without_dropping_default_question_tool() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let previous_channels = crate::bootstrap::state::get_allowed_channels();
        crate::bootstrap::state::set_allowed_channels(vec![
            crate::bootstrap::state::ChannelEntry::server("telegram", false),
        ]);
        let tools = get_tools(&crate::tool::ToolPermissionContext::default())
            .into_iter()
            .map(|tool| tool.name)
            .collect::<std::collections::BTreeSet<_>>();
        crate::bootstrap::state::set_allowed_channels(previous_channels);

        // The hardcoded feature-switch table keeps `KAIROS`/`KAIROS_CHANNELS`
        // off by default, so AskUserQuestion remains enabled just like CC when
        // that build-feature expression is false.
        assert!(
            tools.contains(
                crate::tools::ask_user_question_tool::prompt::ASK_USER_QUESTION_TOOL_NAME
            )
        );
    }
}
