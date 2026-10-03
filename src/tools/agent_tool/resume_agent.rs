//! Maps to CC `tools/AgentTool/resumeAgent.ts`.
//!
//! This module owns stopped/evicted background-agent resume. It loads the
//! sidechain transcript through `utils/session_storage`, reconstructs the
//! worker prompt messages and the content-replacement state, restores the
//! original worktree, registers a `LocalAgentTask`, and restarts `run_agent` in
//! the background. Fork resume (CC `resumeAgent.ts:102-148`, `:180-190`)
//! recovers the PARENT's rendered system prompt
//! (`toolUseContext.renderedSystemPrompt`, CC `Tool.ts:293-299`, or the
//! `getSystemPrompt` + `buildEffectiveSystemPrompt` recompute fallback) and
//! reruns the worker with `override.systemPrompt` + `useExactTools: true` on
//! the parent's own tool pool. Fork SPAWN is live
//! (`fork_subagent.rs#is_fork_subagent_enabled` follows
//! `scripts/build.ts:45`), so a resumed fork reaches this path in any session
//! the gate's two runtime vetoes do not withhold it from.
//!
//! Naming/shape note — the private helpers below
//! ([`resolve_resumed_agent_definition`], [`recover_fork_parent_system_prompt`],
//! [`require_fork_parent_system_prompt`], [`resolve_resumed_worktree_path`],
//! [`bump_resumed_worktree_mtime`]) are inline blocks in CC
//! (`resumeAgent.ts:80-97`, `:99-112`, `:116-148`). They are extracted here as
//! TEST SEAMS, the pattern this codebase already uses for
//! `constants/prompts.rs#get_system_prompt_with_settings` and
//! `agent_tool/mod.rs#validate_required_mcp_servers_for_agent_with_poll`: the
//! only production entry, `resume_agent_background`, needs a sidechain
//! transcript on disk, a task registry, a process runtime handle and a detached
//! spawn before any of these decisions is reachable, so pinning CC's edge cases
//! (JS truthiness on `meta?.agentType`, the FORK arm ordering, stat-is-a-
//! directory, the `:143-147` error sentence) through it is not possible. Each
//! one is driven directly by tests in this file; none is a bare
//! block-extraction. [`recover_fork_parent_system_prompt`] additionally has a
//! SECOND production caller — CC carries a byte-identical copy of that body at
//! `AgentTool.tsx:727-754`, ported as `mod.rs#fork_parent_system_prompt_for_spawn`.

use super::built_in::general_purpose_agent::general_purpose_agent;
use super::load_agents_dir::{AgentDefinition, AgentDefinitionSource};
use super::run_agent::{self, RunAgentInput};
use crate::types::message::{Message, UserContent, UserMessage};

/// Maps to CC `resumeAgent.ts#ResumeAgentResult`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResumeAgentResult {
    pub agent_id: String,
    pub description: String,
    pub output_file: String,
}

/// Maps to CC `resumeAgent.ts#resumeAgentBackground` parameter object.
pub struct ResumeAgentBackgroundParams<'a> {
    pub agent_id: &'a str,
    pub prompt: &'a str,
    pub tool_use_context: &'a crate::tool::ToolUseContext,
    pub invoking_request_id: Option<&'a str>,
}

/// Maps to CC `resumeAgent.ts#resumeAgentBackground`.
pub async fn resume_agent_background(
    params: ResumeAgentBackgroundParams<'_>,
) -> anyhow::Result<ResumeAgentResult> {
    let _invoking_request_id = params.invoking_request_id;
    let transcript = crate::utils::session_storage::get_agent_transcript(params.agent_id)
        .ok_or_else(|| anyhow::anyhow!("No transcript found for agent ID: {}", params.agent_id))?;
    let metadata =
        crate::utils::session_storage::read_agent_metadata(params.agent_id).unwrap_or(None);

    // CC resumeAgent.ts:70-74 sanitizes with `filterUnresolvedToolUses`
    // (utils/messages.ts:2795-2840, all-orphan `every` semantics), NOT the
    // any-orphan `filterIncompleteToolCalls` — that one only serves
    // forkContextMessages (runAgent.ts:370-378). The any-orphan form dropped
    // partially-resolved assistant rows and orphaned their paired
    // tool_results (API 400 risk).
    let sidechain_records =
        crate::utils::tool_result_storage::content_replacement_records_from_values(
            &transcript.content_replacements,
        );
    // All three filters are `utils/messages.ts` exports that CC imports here
    // together (`resumeAgent.ts:13-18`), so all three come from
    // `crate::utils::messages`.
    let resumed_messages = crate::utils::messages::filter_whitespace_only_assistant_messages(
        crate::utils::messages::filter_orphaned_thinking_only_messages(
            crate::utils::messages::filter_unresolved_tool_uses(transcript.messages),
        ),
    );
    // Maps to CC `resumeAgent.ts:75-79` `reconstructForSubagentResume(
    // toolUseContext.contentReplacementState, resumedMessages,
    // transcript.contentReplacements)` — computed from the sanitized transcript
    // BEFORE the new prompt row is appended.
    let resumed_replacement_state =
        crate::utils::tool_result_storage::reconstruct_for_subagent_resume(
            params.tool_use_context.content_replacement_state.as_ref(),
            &resumed_messages,
            &sidechain_records,
        );
    // Maps to CC `resumeAgent.ts:80-97`.
    let resumed_worktree_path = resolve_resumed_worktree_path(metadata.as_ref());
    if let Some(worktree_path) = resumed_worktree_path.as_deref() {
        bump_resumed_worktree_mtime(worktree_path);
    }

    // Maps to CC `resumeAgent.ts:99-112`: the FORK arm comes FIRST (`:102-104`
    // `meta?.agentType === FORK_AGENT.agentType` selects `FORK_AGENT` and sets
    // `isResumedFork`), before the activeAgents lookup.
    let (selected_agent, is_resumed_fork) = resolve_resumed_agent_definition(
        metadata.as_ref(),
        &params.tool_use_context.agent_definitions.active_agents,
    );

    // Maps to CC `resumeAgent.ts:116-148`: a resumed fork reruns under the
    // PARENT's system prompt — the turn-start rendered bytes when the carrier
    // holds them, otherwise the recompute fallback.
    let fork_parent_system_prompt = if is_resumed_fork {
        Some(require_fork_parent_system_prompt(
            recover_fork_parent_system_prompt(params.tool_use_context),
        )?)
    } else {
        None
    };

    let mut prompt_messages = resumed_messages;
    prompt_messages.push(Message::User(UserMessage {
        uuid: uuid::Uuid::new_v4().to_string(),
        timestamp: chrono::Utc::now(),
        content: vec![UserContent::Text(params.prompt.to_string())],
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

    let description = metadata
        .as_ref()
        .and_then(|metadata| metadata.description.clone())
        .unwrap_or_else(|| "(resumed)".to_string());

    // Maps to CC `resumeAgent.ts:230` `void runWithAgentContext(...)` — the
    // resumed run is detached onto Node's process event loop, so it outlives
    // the `SendMessage`/tool turn that resumed it. `Handle::try_current()`
    // would instead bind it to that turn's private per-query runtime
    // (`query.rs#spawn_query`), which is dropped when the turn resolves.
    let Some(handle) = crate::utils::process_runtime::runtime_handle_for_detached_work() else {
        return Err(anyhow::anyhow!(
            "Agent background execution requires an active async runtime; no background agent was started."
        ));
    };

    let task = crate::tasks::local_agent_task::register_async_agent_with_store(
        crate::tasks::local_agent_task::RegisterAsyncAgentParams {
            agent_id: params.agent_id.to_string(),
            description: description.clone(),
            prompt: params.prompt.to_string(),
            selected_agent: selected_agent.clone(),
            // Maps to CC `resumeAgent.ts:204` `toolUseId:
            // toolUseContext.toolUseId` — the resume registration remembers
            // which tool_use row launched it.
            tool_use_id: params.tool_use_context.tool_use_id.clone(),
        },
        params
            .tool_use_context
            .app_store
            .tasks_store
            .clone()
            .or_else(|| params.tool_use_context.app_store.store.clone()),
    );

    let mut background_context = params.tool_use_context.clone();
    background_context.abort_controller = task.abort_controller.clone();
    // Maps to CC `resumeAgent.ts:227-231` `wrapWithCwd` — the whole resumed run
    // executes inside `runWithCwdOverride(resumedWorktreePath, …)` so `getCwd()`
    // (and the system prompt `runAgent` recomputes under it) sees the restored
    // worktree. This port carries that override on the context, the same way
    // `AgentTool.call` does at `mod.rs#with_cwd_override`.
    if let Some(worktree_path) = resumed_worktree_path.as_deref() {
        background_context.cwd_override = Some(std::path::PathBuf::from(worktree_path));
    }
    // Maps to CC `resumeAgent.ts:170` `canUseTool` — the PARENT's permission
    // callback. #156: no separate forwarding — CC hands it to `runAgent` only
    // so `query()` threads it to the child's tool execution, and this port's
    // equivalent already rides `background_context` itself: the owned
    // `ToolUseContext.can_use_tool` carrier is cloned into the subagent context
    // by `create_subagent_context` (`ToolUseContext` clone) and read at the
    // `tool_execution.rs` decide sites, while the `ask` dialog leg is the
    // inherited `interactive_permission_sink`
    // (`run_agent.rs#resolve_agent_permission_request`). The former
    // re-borrowing closure here fed `RunAgentInput.can_use_tool`, which #141
    // deliberately never read — it was dead on arrival and is deleted, not
    // re-wired.
    let agent_id_for_run = params.agent_id.to_string();
    let prompt_owned = params.prompt.to_string();
    let description_for_run = description.clone();
    let selected_agent_for_run = selected_agent.clone();
    // Maps to CC `resumeAgent.ts:162-164` `workerTools = isResumedFork ?
    // toolUseContext.options.tools : assembleToolPool(...)` — the fork arm
    // reuses the PARENT's own tool pool so `useExactTools` can keep the
    // request prefix byte-identical.
    let worker_tools = is_resumed_fork.then(|| params.tool_use_context.tools.clone());
    let worktree_path_for_run = resumed_worktree_path.clone();
    let worktree_path_for_notification = resumed_worktree_path.clone();
    let agent_context = crate::utils::agent_context::subagent_resume_context(
        agent_id_for_run.clone(),
        selected_agent_for_run.agent_type.clone(),
        super::load_agents_dir::is_built_in_agent(&selected_agent_for_run),
        params.invoking_request_id.map(ToOwned::to_owned),
    );

    handle.spawn(async move {
        crate::utils::agent_context::run_with_agent_context(agent_context, || async move {
            let result = run_agent::run_agent(RunAgentInput {
                agent_definition: &selected_agent_for_run,
                prompt: &prompt_owned,
                description: Some(&description_for_run),
                model_override: None,
                context: &background_context,
                // Maps to: CC `resumeAgent.ts:175-178`.
                query_source: crate::utils::prompt_category::get_query_source_for_agent(
                    Some(selected_agent_for_run.agent_type.as_str()),
                    super::load_agents_dir::is_built_in_agent(&selected_agent_for_run),
                ),
                // Maps to CC `resumeAgent.ts:174` (and :213 on the fork-resume
                // params): resumed agents always run `isAsync: true` under
                // `runAsyncAgentLifecycle`.
                is_async: true,
                can_show_permission_prompts: None,
                // Maps to CC `resumeAgent.ts:162-164`/`:186`: the non-fork resume
                // passes `assembleToolPool(workerPermissionContext,
                // appState.mcp.tools)` where `workerPermissionContext` is the app
                // state's context with `mode: selectedAgent.permissionMode ??
                // 'acceptEdits'`. `None` selects exactly that computation inside
                // `run_agent` — CC only hoists it into its callers to break a
                // `runAgent`↔`tools.ts` import cycle Rust does not have. The
                // fork arm carries the PARENT's own pool (`worker_tools`).
                available_tools: worker_tools,
                // Maps to CC `resumeAgent.ts:187-189` `forkContextMessages:
                // undefined` — an EXPLICIT pin, not an omission: "Transcript
                // already contains the parent context slice from the original
                // fork. Re-supplying it would cause duplicate tool_use IDs."
                fork_context_messages: None,
                preserve_tool_use_results: false,
                transcript_subdir: None,
                // Maps to CC `resumeAgent.ts:183-185` + `:238-242`: the
                // fork-resume override carries the recovered parent prompt
                // ("cache-identical prefix"; non-fork stays `None` so runAgent
                // recomputes under the restored cwd), and every resume adds
                // `agentId` + `abortController:
                // agentBackgroundTask.abortController!` on top.
                //
                // CC `:187-189` also pins `forkContextMessages: undefined`
                // ("Transcript already contains the parent context slice…
                // duplicate tool_use IDs"); `RunAgentInput` has no
                // forkContextMessages carrier yet, and this caller would pass
                // nothing anyway.
                r#override: run_agent::RunAgentOverride {
                    system_prompt: fork_parent_system_prompt,
                    abort_controller: Some(background_context.abort_controller.clone()),
                    agent_id: Some(&agent_id_for_run),
                    ..Default::default()
                },
                // Maps to CC `resumeAgent.ts:190` `...(isResumedFork &&
                // { useExactTools: true })`.
                use_exact_tools: is_resumed_fork,
                allowed_tools: None,
                // Maps to CC `resumeAgent.ts:190-192` — re-persisted so the
                // restored worktree survives `runAgent`'s `writeAgentMetadata`
                // overwrite.
                worktree_path: worktree_path_for_run.as_deref(),
                parent_tool_use_id: None,
                on_progress: None,
                background_task_id: Some(&agent_id_for_run),
                // Maps to CC `resumeAgent.ts:194` `contentReplacementState:
                // resumedReplacementState`.
                content_replacement_state: resumed_replacement_state,
                background_signal: None,
                on_message: None,
                prompt_messages: Some(prompt_messages),
            })
            .await;
            // Maps to CC `resumeAgent.ts:232-256`: resume drives the SAME
            // `runAsyncAgentLifecycle` as the async-from-start path, so it gets
            // the same status-before-worktree ordering and the same
            // `getWorktreeResult` seam — here CC's `:254-255` `resumedWorktreePath
            // ? { worktreePath: resumedWorktreePath } : {}`, which is a plain
            // read with no git exec.
            //
            // SEAM (unported, whole feature): CC `:250-253` also passes
            // `enableSummarization: isCoordinatorMode() ||
            // isForkSubagentEnabled() || getSdkAgentProgressSummariesEnabled()`
            // (the async-from-start twin is `AgentTool.tsx:1019-1022`). That
            // flag has exactly ONE consumer — `agentToolUtils.ts:543-553` turns
            // it into the `onCacheSafeParams` callback handed to
            // `makeStream`, i.e. `runAgent`'s `onCacheSafeParams`
            // (`runAgent.ts:304`, fired at `:721-729` with the agent's system
            // prompt / user+system context / subagent ToolUseContext /
            // initialMessages), and the callback starts
            // `startAgentSummarization(taskId, agentId, params,
            // rootSetAppState)` (`services/AgentSummary/agentSummary.ts:46`,
            // 179 lines) whose `stop` is called at `agentToolUtils.ts:595`.
            // None of that chain exists in this port: `RunAgentInput` has no
            // `on_cache_safe_params`, there is no `services/agent_summary`
            // module, and `tasks/local_agent_task.rs` has no
            // `updateAgentSummary` writer (`LocalAgentTask.tsx:454`) — the
            // only trace is the `QuerySource::AgentSummary` enum variant. A
            // parameter added here would feed nothing, so the flag is recorded
            // rather than faked; porting it is an
            // `agentSummary.ts` + `updateAgentSummary` + `onCacheSafeParams`
            // batch, not a resume-site change.
            super::finish_async_agent_run(
                &agent_id_for_run,
                result,
                &background_context,
                "Resumed background agent unexpectedly requested foreground background transfer",
                move || super::WorktreeCleanupResult {
                    worktree_path: worktree_path_for_notification,
                    worktree_branch: None,
                },
            )
            .await;
        })
        .await;
    });

    Ok(ResumeAgentResult {
        agent_id: params.agent_id.to_string(),
        description,
        output_file: task.output_file,
    })
}

/// Maps to CC `resumeAgent.ts:100-112`. The FORK arm comes FIRST (`:102-104`
/// `meta?.agentType === FORK_AGENT.agentType` — a direct equality, so it wins
/// even over an activeAgents entry that happens to be named "fork") and sets
/// `isResumedFork` (the second tuple member). Then `meta?.agentType` (JS
/// truthy, so an EMPTY recorded type takes the else branch) is looked up in
/// `toolUseContext.options.agentDefinitions.activeAgents`, falling back to
/// `GENERAL_PURPOSE_AGENT`.
///
/// `active_agents` is the REQUEST's snapshot, never a fresh disk read: a
/// mid-session edit to an agent file must not change which definition a resume
/// started under this turn's context runs with. Same rule as
/// `AgentTool.tsx:504-505`, ported at `agent_tool/mod.rs#selected_agent_definition`.
///
/// CC deliberately skips `filterDeniedAgents` here (`:99` "original spawn
/// already passed permission checks"), so this lookup does not re-gate either.
fn resolve_resumed_agent_definition(
    metadata: Option<&crate::utils::session_storage::AgentMetadata>,
    active_agents: &[AgentDefinition],
) -> (AgentDefinition, bool) {
    if metadata
        .is_some_and(|metadata| metadata.agent_type == super::fork_subagent::FORK_SUBAGENT_TYPE)
    {
        return (super::fork_subagent::fork_agent_definition(), true);
    }

    let Some(agent_type) = metadata
        .map(|metadata| metadata.agent_type.as_str())
        .filter(|agent_type| !agent_type.is_empty())
    else {
        return (general_purpose_agent(), false);
    };

    (
        active_agents
            .iter()
            .find(|agent| agent.agent_type == agent_type)
            .cloned()
            .unwrap_or_else(general_purpose_agent),
        false,
    )
}

/// Maps to CC `resumeAgent.ts:117-142` — recover the PARENT's system prompt
/// for a fork resume.
///
/// `:118-119`: `if (toolUseContext.renderedSystemPrompt)` prefers the bytes
/// frozen at the parent's turn start (`Tool.ts:293-299`; re-rendering "can
/// diverge (GrowthBook cold→warm) and bust the cache"). JS truthiness on an
/// ARRAY: `[]` is truthy, so an empty rendered prompt is still used — the
/// check is `is_some()`, not non-emptiness.
///
/// `:120-142`: otherwise recompute — the 4-arg `getSystemPrompt(tools,
/// mainLoopModel, additionalWorkingDirectories, mcpClients)` plus
/// `buildEffectiveSystemPrompt` with the main-thread agent definition and the
/// context's custom/append overrides.
///
/// Shared with the fork SPAWN path: CC carries a byte-identical second copy of
/// this body at `AgentTool.tsx:727-754`, reached when `isForkPath` needs the
/// parent's prompt for `override.systemPrompt`. One owner here rather than two
/// transcriptions of the same 25 lines; the spawn caller is
/// `mod.rs#fork_parent_system_prompt_for_spawn`. Only the `throw` that follows
/// it is resume-only (`:143-147`), so it stays in
/// [`require_fork_parent_system_prompt`] and not in this function.
pub(super) fn recover_fork_parent_system_prompt(
    tool_use_context: &crate::tool::ToolUseContext,
) -> Option<crate::utils::system_prompt::SystemPrompt> {
    if let Some(rendered) = tool_use_context.rendered_system_prompt.clone() {
        return Some(rendered);
    }

    let app_state = tool_use_context.get_app_state();
    // Maps to CC `:121-125` `appState.agent ? appState.agentDefinitions
    // .activeAgents.find(a => a.agentType === appState.agent) : undefined`
    // (JS truthy: an empty agent type skips the lookup).
    let main_thread_agent_definition = app_state.as_ref().and_then(|state| {
        state
            .agent
            .as_deref()
            .filter(|agent_type| !agent_type.is_empty())
            .and_then(|agent_type| {
                state
                    .agent_definitions
                    .active_agents
                    .iter()
                    .find(|agent| agent.agent_type == agent_type)
                    .cloned()
            })
    });
    // Maps to CC `:126-128` `Array.from(appState.toolPermissionContext
    // .additionalWorkingDirectories.keys())`. Without a live store (a
    // condition CC cannot express) the context's own snapshot is the same map.
    let additional_working_directories: Vec<String> = match app_state.as_ref() {
        Some(state) => state
            .tool_permission_context
            .additional_working_directories
            .keys()
            .cloned()
            .collect(),
        None => tool_use_context
            .tool_permission_context
            .additional_working_directories
            .keys()
            .cloned()
            .collect(),
    };
    // Maps to CC `:129-134` — `toolUseContext.options.mainLoopModel` is
    // non-optional in CC; the Option here is the port's carrier, resolved the
    // same way `run_agent.rs#resolve_agent_model` resolves the parent model.
    let model = tool_use_context
        .main_loop_model
        .clone()
        .unwrap_or_else(crate::utils::model::model::get_main_loop_model);
    let default_system_prompt = crate::constants::prompts::get_system_prompt(
        &tool_use_context.tools,
        &model,
        &additional_working_directories,
        &tool_use_context.mcp_state.clients,
    );
    // Maps to CC `:135-141`.
    let options = crate::utils::system_prompt::ToolUseContextOptions {
        main_loop_model: Some(model),
    };
    Some(crate::utils::system_prompt::build_effective_system_prompt(
        crate::utils::system_prompt::BuildEffectiveSystemPromptArgs {
            main_thread_agent_definition: main_thread_agent_definition.as_ref(),
            tool_use_context_options: Some(&options),
            custom_system_prompt: tool_use_context.custom_system_prompt.as_deref(),
            default_system_prompt,
            append_system_prompt: tool_use_context.append_system_prompt.as_deref(),
            override_system_prompt: None,
        },
    ))
}

/// Maps to CC `resumeAgent.ts:143-147` `if (!forkParentSystemPrompt) { throw
/// new Error('Cannot resume fork agent: unable to reconstruct parent system
/// prompt') }`.
///
/// Defensive in CC too: both recovery branches assign, and JS falsiness on a
/// SystemPrompt value means null/undefined only — an EMPTY array is truthy and
/// passes. The guard exists so the sentence stays byte-exact if a recovery
/// path ever becomes fallible.
fn require_fork_parent_system_prompt(
    recovered: Option<crate::utils::system_prompt::SystemPrompt>,
) -> anyhow::Result<crate::utils::system_prompt::SystemPrompt> {
    recovered.ok_or_else(|| {
        anyhow::anyhow!("Cannot resume fork agent: unable to reconstruct parent system prompt")
    })
}

/// Maps to CC `resumeAgent.ts:80-92` — best-effort worktree restore.
///
/// `meta?.worktreePath ? …` is JS-truthy, so an empty recorded path never
/// reaches the stat. A successful stat that is not a directory yields
/// `undefined` silently; only a REJECTED stat logs, because CC attaches the log
/// to the rejection handler alone. Either way the caller falls back to the
/// parent cwd "rather than crashing on chdir later".
fn resolve_resumed_worktree_path(
    metadata: Option<&crate::utils::session_storage::AgentMetadata>,
) -> Option<String> {
    let worktree_path = metadata
        .and_then(|metadata| metadata.worktree_path.as_deref())
        .filter(|worktree_path| !worktree_path.is_empty())?;
    match std::fs::metadata(worktree_path) {
        Ok(stat) if stat.is_dir() => Some(worktree_path.to_string()),
        Ok(_) => None,
        Err(_) => {
            crate::utils::debug::log_for_debugging(&format!(
                "Resumed worktree {worktree_path} no longer exists; falling back to parent cwd"
            ));
            None
        }
    }
}

/// Maps to CC `resumeAgent.ts:93-97` — "Bump mtime so stale-worktree cleanup
/// doesn't delete a just-resumed worktree (#22355)".
///
/// Deviation: CC's `fsp.utimes` is path-based and its rejection would propagate
/// out of `resumeAgentBackground`. Rust's only std equivalent takes an open
/// handle, and opening a DIRECTORY is not portable, so a failure here is a
/// Rust-side platform limitation rather than a CC error condition — it is
/// logged and the resume continues.
fn bump_resumed_worktree_mtime(worktree_path: &str) {
    let now = std::time::SystemTime::now();
    let result = std::fs::File::open(worktree_path).and_then(|handle| {
        handle.set_times(
            std::fs::FileTimes::new()
                .set_accessed(now)
                .set_modified(now),
        )
    });
    if let Err(error) = result {
        tracing::debug!(
            worktree_path,
            error = %error,
            "failed to bump resumed worktree mtime"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::TEST_ENV_LOCK;

    #[test]
    fn resume_agent_without_transcript_reports_official_error_boundary() {
        let result =
            futures::executor::block_on(resume_agent_background(ResumeAgentBackgroundParams {
                agent_id: "a0123456789abcdef",
                prompt: "continue",
                tool_use_context: &crate::tool::ToolUseContext::default(),
                invoking_request_id: None,
            }));
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("No transcript found for agent ID")
        );
    }

    #[test]
    fn resolve_resumed_agent_definition_falls_back_to_general_purpose() {
        let metadata = crate::utils::session_storage::AgentMetadata {
            agent_type: "missing-agent".to_string(),
            worktree_path: None,
            description: None,
        };
        let (agent, is_resumed_fork) = resolve_resumed_agent_definition(Some(&metadata), &[]);
        assert_eq!(agent.agent_type, "general-purpose");
        assert_eq!(agent.source, AgentDefinitionSource::BuiltIn);
        assert!(!is_resumed_fork);
    }

    fn agent_metadata(agent_type: &str) -> crate::utils::session_storage::AgentMetadata {
        crate::utils::session_storage::AgentMetadata {
            agent_type: agent_type.to_string(),
            worktree_path: None,
            description: None,
        }
    }

    /// CC `resumeAgent.ts:105-108` reads
    /// `toolUseContext.options.agentDefinitions.activeAgents` — the request's
    /// snapshot. A definition that exists only in the snapshot must be found,
    /// and the fallback must not depend on what is on disk.
    #[test]
    fn resolve_resumed_agent_definition_reads_the_request_snapshot() {
        let snapshot = vec![AgentDefinition::new(
            "snapshot-only",
            "exists solely in this turn's definitions",
            AgentDefinitionSource::ProjectSettings,
        )];

        let (found, found_is_fork) =
            resolve_resumed_agent_definition(Some(&agent_metadata("snapshot-only")), &snapshot);
        assert_eq!(found.agent_type, "snapshot-only");
        assert_eq!(found.source, AgentDefinitionSource::ProjectSettings);
        assert!(!found_is_fork);

        let (missing, missing_is_fork) =
            resolve_resumed_agent_definition(Some(&agent_metadata("other")), &snapshot);
        assert_eq!(missing.agent_type, "general-purpose");
        assert!(!missing_is_fork);
    }

    /// CC `resumeAgent.ts:105` `else if (meta?.agentType)` is JS truthiness:
    /// `''` skips the lookup entirely, whitespace does NOT.
    #[test]
    fn resolve_resumed_agent_definition_uses_official_truthiness_not_trim() {
        let snapshot = vec![AgentDefinition::new(
            "  ",
            "a whitespace-named definition",
            AgentDefinitionSource::ProjectSettings,
        )];

        assert_eq!(
            resolve_resumed_agent_definition(Some(&agent_metadata("")), &snapshot)
                .0
                .agent_type,
            "general-purpose"
        );
        assert_eq!(
            resolve_resumed_agent_definition(Some(&agent_metadata("  ")), &snapshot)
                .0
                .agent_type,
            "  "
        );
        assert_eq!(
            resolve_resumed_agent_definition(None, &snapshot)
                .0
                .agent_type,
            "general-purpose"
        );
    }

    /// CC `resumeAgent.ts:82-92`: truthy path only, stat must resolve to a
    /// DIRECTORY, and every other outcome falls back to the parent cwd.
    #[test]
    fn resolve_resumed_worktree_path_keeps_only_an_existing_directory() {
        let root = std::env::temp_dir().join(format!(
            "cometix-resume-worktree-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).expect("create temp worktree root");
        let file = root.join("not-a-directory");
        std::fs::write(&file, "x").expect("write temp file");

        let mut metadata = agent_metadata("general-purpose");
        assert_eq!(resolve_resumed_worktree_path(Some(&metadata)), None);
        assert_eq!(resolve_resumed_worktree_path(None), None);

        metadata.worktree_path = Some(String::new());
        assert_eq!(resolve_resumed_worktree_path(Some(&metadata)), None);

        metadata.worktree_path = Some(root.join("gone").to_string_lossy().to_string());
        assert_eq!(resolve_resumed_worktree_path(Some(&metadata)), None);

        metadata.worktree_path = Some(file.to_string_lossy().to_string());
        assert_eq!(resolve_resumed_worktree_path(Some(&metadata)), None);

        let kept = root.to_string_lossy().to_string();
        metadata.worktree_path = Some(kept.clone());
        assert_eq!(resolve_resumed_worktree_path(Some(&metadata)), Some(kept));

        let _ = std::fs::remove_dir_all(&root);
    }

    /// CC `resumeAgent.ts:93-97` — the surviving worktree gets a fresh mtime so
    /// stale-worktree cleanup does not delete it right after a resume.
    #[test]
    fn bump_resumed_worktree_mtime_refreshes_the_directory_timestamp() {
        let root = std::env::temp_dir().join(format!(
            "cometix-resume-worktree-touch-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).expect("create temp worktree root");
        let stale =
            std::time::SystemTime::now() - std::time::Duration::from_secs(60 * 60 * 24 * 40);
        std::fs::File::open(&root)
            .expect("open temp worktree directory")
            .set_times(std::fs::FileTimes::new().set_modified(stale))
            .expect("age the temp worktree directory");
        let before = std::fs::metadata(&root)
            .expect("stat aged worktree")
            .modified()
            .expect("aged worktree mtime");

        bump_resumed_worktree_mtime(&root.to_string_lossy());

        let after = std::fs::metadata(&root)
            .expect("stat bumped worktree")
            .modified()
            .expect("bumped worktree mtime");
        assert!(
            after > before,
            "expected {after:?} to be newer than {before:?}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// CC `resumeAgent.ts:102-104`: the FORK arm is a direct equality checked
    /// FIRST, before the activeAgents lookup — a snapshot definition that
    /// happens to be named "fork" must NOT shadow the synthetic FORK_AGENT.
    ///
    /// Old shape: `resolve_resumed_agent_definition` had no fork arm and
    /// `resume_agent_background` refused fork metadata outright.
    #[test]
    fn resolve_resumed_agent_definition_selects_fork_agent_before_the_snapshot_lookup() {
        let shadow = vec![AgentDefinition::new(
            crate::tools::agent_tool::fork_subagent::FORK_SUBAGENT_TYPE,
            "a user agent that reuses the fork name",
            AgentDefinitionSource::ProjectSettings,
        )];

        let (agent, is_resumed_fork) = resolve_resumed_agent_definition(
            Some(&agent_metadata(
                crate::tools::agent_tool::fork_subagent::FORK_SUBAGENT_TYPE,
            )),
            &shadow,
        );
        assert!(is_resumed_fork);
        assert_eq!(
            agent.agent_type,
            crate::tools::agent_tool::fork_subagent::FORK_SUBAGENT_TYPE
        );
        // The synthetic built-in definition, not the snapshot shadow.
        assert_eq!(agent.source, AgentDefinitionSource::BuiltIn);
        assert_eq!(agent.max_turns, Some(200));
    }

    /// CC `resumeAgent.ts:118-119`: when the parent's rendered prompt bytes
    /// are on the context, fork resume uses them VERBATIM — no recompute. JS
    /// truthiness on an ARRAY means even an empty rendered prompt is used.
    ///
    /// Old shape: `ToolUseContext.rendered_system_prompt` did not exist and
    /// fork resume was refused before any prompt recovery ran.
    #[test]
    fn recover_fork_parent_system_prompt_prefers_the_rendered_turn_start_bytes() {
        let mut context = crate::tool::ToolUseContext::default();
        context.rendered_system_prompt =
            Some(vec!["PARENT_SECTION_A".to_string(), "APPEND".to_string()]);
        assert_eq!(
            recover_fork_parent_system_prompt(&context),
            Some(vec!["PARENT_SECTION_A".to_string(), "APPEND".to_string()])
        );

        // `[]` is truthy in CC's `if (toolUseContext.renderedSystemPrompt)`.
        context.rendered_system_prompt = Some(Vec::new());
        assert_eq!(
            recover_fork_parent_system_prompt(&context),
            Some(Vec::new())
        );
    }

    /// CC `resumeAgent.ts:120-142`: without the rendered carrier, the fallback
    /// recomputes through the 4-arg `getSystemPrompt(tools, mainLoopModel,
    /// additionalWorkingDirectories, mcpClients)`. Seeding an additional
    /// working directory and finding it in the produced env section proves the
    /// third argument actually reached `computeSimpleEnvInfo`.
    ///
    /// Old shape: `get_system_prompt` was 2-arg — the working-directory and
    /// MCP-client plumbing did not exist anywhere on this path.
    #[test]
    fn recover_fork_parent_system_prompt_recomputes_with_the_four_arg_plumbing() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("CLAUDE_CODE_SIMPLE");
        crate::utils::process_env::remove("CLAUDE_CODE_COORDINATOR_MODE");
        crate::utils::process_env::set("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "1");

        let mut context = crate::tool::ToolUseContext::default();
        assert!(context.rendered_system_prompt.is_none());
        context.main_loop_model = Some("claude-sonnet-4-6".to_string());
        context
            .tool_permission_context
            .additional_working_directories
            .insert(
                "/workspace/fork-resume-extra".to_string(),
                crate::types::permissions::AdditionalWorkingDirectory {
                    path: "/workspace/fork-resume-extra".to_string(),
                    source: crate::types::permissions::PermissionRuleSource::Session,
                },
            );

        let recovered = recover_fork_parent_system_prompt(&context)
            .expect("recompute fallback yields a prompt");
        crate::utils::process_env::remove("CLAUDE_CODE_DISABLE_AUTO_MEMORY");

        assert!(!recovered.is_empty());
        let env_section = recovered
            .iter()
            .find(|section| section.starts_with("# Environment"))
            .expect("recomputed prompt carries the env section");
        assert!(env_section.contains("Additional working directories:"));
        assert!(env_section.contains("/workspace/fork-resume-extra"));
    }

    /// CC `resumeAgent.ts:143-147` — the exact sentence, and the JS-falsy
    /// boundary: only a MISSING prompt throws; an empty array passes.
    ///
    /// Old shape: the port refused fork resume with its own
    /// "Fork agent resume is not implemented..." sentence instead.
    #[test]
    fn require_fork_parent_system_prompt_pins_official_sentence_and_truthiness() {
        assert_eq!(
            require_fork_parent_system_prompt(None)
                .unwrap_err()
                .to_string(),
            "Cannot resume fork agent: unable to reconstruct parent system prompt"
        );
        assert_eq!(
            require_fork_parent_system_prompt(Some(Vec::new())).unwrap(),
            Vec::<String>::new()
        );
    }
}
