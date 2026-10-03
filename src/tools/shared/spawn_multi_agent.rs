//! Shared teammate spawning module.
//!
//! Maps to: CC `tools/shared/spawnMultiAgent.ts`.
//!
//! AgentTool must delegate teammate creation here rather than embedding swarm
//! lifecycle logic in `tools/agent_tool/mod.rs`. This module owns the official
//! shared input/output shapes, teammate model resolution, backend mode
//! selection, team record mutation, `AppState.teamContext` tracking, and the
//! `spawnInProcessTeammate(...)` delegation boundary.
//!
//! The pane handlers build the spawn command INLINE, as CC does — see
//! [`handle_spawn_pane`] for why they do not route through
//! `PaneBackendExecutor`.

use crate::tools::agent_tool::load_agents_dir::AgentDefinition;
use crate::utils::agent_id::format_agent_id;
use crate::utils::debug::log_for_debugging;
use crate::utils::swarm::backends::registry::{
    detect_and_get_backend, is_in_process_enabled, mark_in_process_fallback,
    reset_backend_detection,
};
use crate::utils::swarm::backends::teammate_mode_snapshot::{
    TeammateMode, get_teammate_mode_from_snapshot,
};
use crate::utils::swarm::backends::types::{BackendType, is_pane_backend};
use crate::utils::swarm::constants::{
    SWARM_SESSION_NAME, SWARM_VIEW_WINDOW_NAME, TEAM_LEAD_NAME, TMUX_COMMAND,
};
use crate::utils::swarm::spawn_utils::{
    BuildInheritedCliFlagsOptions, build_inherited_env_vars, get_teammate_command, shell_quote,
};
use crate::utils::swarm::team_helpers::{
    TeamMemberRecord, generate_unique_teammate_name, sanitize_agent_name, write_team_record,
};
use crate::utils::swarm::teammate_layout_manager::{
    assign_teammate_color, create_teammate_pane_in_swarm_view, enable_pane_border_status,
    is_inside_tmux, send_command_to_pane,
};
use crate::utils::swarm::teammate_model::get_hardcoded_teammate_model_fallback;

/// Maps to: CC `tools/shared/spawnMultiAgent.ts#SpawnOutput`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpawnOutput {
    pub teammate_id: String,
    pub agent_id: String,
    pub agent_type: Option<String>,
    pub model: Option<String>,
    pub name: String,
    pub color: Option<String>,
    pub tmux_session_name: String,
    pub tmux_window_name: String,
    pub tmux_pane_id: String,
    pub team_name: Option<String>,
    pub is_splitpane: Option<bool>,
    pub plan_mode_required: Option<bool>,
}

/// Maps to: CC `tools/shared/spawnMultiAgent.ts#SpawnTeammateConfig`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpawnTeammateConfig {
    pub name: String,
    pub prompt: String,
    pub team_name: Option<String>,
    pub cwd: Option<String>,
    pub use_splitpane: Option<bool>,
    pub plan_mode_required: Option<bool>,
    pub model: Option<String>,
    pub agent_type: Option<String>,
    pub description: Option<String>,
    pub invoking_request_id: Option<String>,
}

fn now_ms() -> u64 {
    chrono::Utc::now().timestamp_millis().max(0) as u64
}

/// Maps to: CC `spawnMultiAgent.ts:72-82#getDefaultTeammateModel`.
///
/// `GlobalConfig.teammate_default_model` mirrors CC's tri-state:
/// `undefined` (never set) / `null` (the "Default" picker entry) / a string.
fn get_default_teammate_model_from_config(
    config: &crate::utils::config::GlobalConfig,
    leader_model: Option<&str>,
) -> String {
    match &config.teammate_default_model {
        // CC `:81` — undefined falls through to the hardcoded fallback.
        None => get_hardcoded_teammate_model_fallback(),
        // CC `:74-77` — null follows the leader.
        Some(None) => leader_model
            .map(ToOwned::to_owned)
            .unwrap_or_else(get_hardcoded_teammate_model_fallback),
        // CC `:78-80` — parseUserSpecifiedModel applies to the CONFIGURED
        // default only.
        Some(Some(configured)) => {
            crate::utils::model::model::parse_user_specified_model(configured)
        }
    }
}

/// Maps to: CC `spawnMultiAgent.ts:72-82#getDefaultTeammateModel`.
///
/// PRESERVE — zero Rust callers, deliberately. This IS the 1:1 of CC's
/// function: CC reads `getGlobalConfig()` inside the body (`:73`), so the
/// config load is part of the symbol. The port hoisted that read into a
/// `_from_config` parameter so [`resolve_teammate_model_with_config`] can be
/// driven by tests without touching the process-wide config cache
/// (`evaluation-position` discipline), and CC's two call sites — `:98` and
/// `:100`, both inside `resolveTeammateModel` — therefore land on that inner
/// function instead of this one. Deleting this wrapper would leave the port
/// with no symbol named after a function CC declares. Do NOT delete it under
/// "nothing calls it in Rust".
pub fn get_default_teammate_model(leader_model: Option<&str>) -> String {
    get_default_teammate_model_from_config(
        &crate::utils::config::load_global_config(),
        leader_model,
    )
}

/// Maps to: CC `spawnMultiAgent.ts:93-101#resolveTeammateModel`.
///
/// CC returns `inputModel` VERBATIM (`:100` `return inputModel ?? ...`).
/// `parseUserSpecifiedModel` is applied only inside `getDefaultTeammateModel`
/// (`:79`), to the configured default. Mapping it over the input here expanded
/// `--model opus` / an agent frontmatter `model: opus` into a full model id
/// before it reached `--model` and `TeamMemberRecord.model`, where CC carries
/// the caller's alias.
fn resolve_teammate_model_with_config(
    input_model: Option<&str>,
    leader_model: Option<&str>,
    config: &crate::utils::config::GlobalConfig,
) -> String {
    if input_model == Some("inherit") {
        return leader_model
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| get_default_teammate_model_from_config(config, leader_model));
    }
    input_model
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| get_default_teammate_model_from_config(config, leader_model))
}

/// Maps to: CC `spawnMultiAgent.ts:93-101#resolveTeammateModel`.
pub fn resolve_teammate_model(input_model: Option<&str>, leader_model: Option<&str>) -> String {
    resolve_teammate_model_with_config(
        input_model,
        leader_model,
        &crate::utils::config::load_global_config(),
    )
}

/// Maps to: CC `spawnMultiAgent.ts:208-260#buildInheritedCliFlags`.
///
/// The MODULE-LOCAL copy, called at `:418` and `:625`. It differs from the
/// `spawnUtils.ts:38-87` export of the same name in exactly two ways:
///   * it HAS the `auto` arm (`:226-231`) — teammates inherit auto mode so the
///     classifier auto-approves their tool calls too, instead of falling back
///     to `default` and blocking on prompts in a pane nobody watches;
///   * it emits NO `--teammate-mode`.
/// `spawnMultiAgent.ts:52` imports only `buildInheritedEnvVars` from spawnUtils,
/// so these handlers never see the other copy.
fn build_inherited_cli_flags_with_options(options: BuildInheritedCliFlagsOptions) -> String {
    let mut flags: Vec<String> = Vec::new();

    // CC `:217-231` — plan mode takes precedence over bypass for safety.
    if !options.plan_mode_required {
        if options.permission_mode
            == Some(crate::types::permissions::PermissionMode::BypassPermissions)
            || options.session_bypass_permissions_mode
        {
            flags.push("--dangerously-skip-permissions".to_string());
        } else if options.permission_mode
            == Some(crate::types::permissions::PermissionMode::AcceptEdits)
        {
            flags.push("--permission-mode acceptEdits".to_string());
        } else if options.permission_mode == Some(crate::types::permissions::PermissionMode::Auto) {
            // CC `:226-231`. The teammate's own startup handles the gate checks
            // and `setAutoModeActive(true)` independently.
            flags.push("--permission-mode auto".to_string());
        }
    }

    // CC `:234-237` — JS truthiness, so an empty override is skipped.
    if let Some(model_override) = options.model_override.filter(|value| !value.is_empty()) {
        flags.push(format!("--model {}", shell_quote(&model_override)));
    }

    // CC `:240-243`.
    if let Some(settings_path) = options.settings_path {
        flags.push(format!(
            "--settings {}",
            shell_quote(&settings_path.display().to_string())
        ));
    }

    // CC `:246-249`.
    for plugin_dir in options.inline_plugins {
        flags.push(format!(
            "--plugin-dir {}",
            shell_quote(&plugin_dir.display().to_string())
        ));
    }

    // CC `:252-257`.
    if let Some(chrome) = options.chrome_flag_override {
        flags.push(if chrome { "--chrome" } else { "--no-chrome" }.to_string());
    }

    flags.join(" ")
}

/// Maps to: CC `spawnMultiAgent.ts:208-260#buildInheritedCliFlags`.
///
/// Rust does not yet carry every official CLI override in `bootstrap/state.ts`
/// (`getSessionBypassPermissionsMode`, `getChromeFlagOverride`); this keeps the
/// official boundary and threads the state CometixCode currently stores.
fn build_inherited_cli_flags(
    plan_mode_required: bool,
    permission_mode: Option<crate::types::permissions::PermissionMode>,
) -> String {
    build_inherited_cli_flags_with_options(BuildInheritedCliFlagsOptions {
        plan_mode_required,
        permission_mode,
        model_override: crate::bootstrap::state::get_main_loop_model_override().flatten(),
        settings_path: crate::bootstrap::state::get_flag_settings_path(),
        inline_plugins: crate::bootstrap::state::get_inline_plugins(),
        ..Default::default()
    })
}

/// Maps to: CC `spawnMultiAgent.ts:423-434` / `:630-641` — replace any
/// inherited `--model` with the teammate's own.
fn apply_teammate_model_flag(inherited_flags: String, model: &str) -> String {
    if model.is_empty() {
        return inherited_flags;
    }
    let mut parts = inherited_flags.split_whitespace().peekable();
    let mut kept: Vec<&str> = Vec::new();
    while let Some(part) = parts.next() {
        if part == "--model" {
            let _ = parts.next();
            continue;
        }
        kept.push(part);
    }
    let stripped = kept.join(" ");
    let quoted_model = shell_quote(model);
    if stripped.is_empty() {
        format!("--model {quoted_model}")
    } else {
        format!("{stripped} --model {quoted_model}")
    }
}

/// Maps to: CC `spawnMultiAgent.ts:404-414` / `:611-621` teammate identity args.
///
/// Unlike `PaneBackendExecutor.ts:117-126`, these handlers also emit
/// `--agent-type` (`:411`/`:618`).
fn build_teammate_identity_cli_args(
    teammate_id: &str,
    sanitized_name: &str,
    team_name: &str,
    teammate_color: &str,
    plan_mode_required: bool,
    agent_type: Option<&str>,
) -> String {
    let mut args = vec![
        format!("--agent-id {}", shell_quote(teammate_id)),
        format!("--agent-name {}", shell_quote(sanitized_name)),
        format!("--team-name {}", shell_quote(team_name)),
        format!("--agent-color {}", shell_quote(teammate_color)),
        format!(
            "--parent-session-id {}",
            shell_quote(&crate::bootstrap::state::get_session_id())
        ),
    ];
    // CC `:410-413` — `.filter(Boolean)` drops the empty-string slots, so an
    // empty `agent_type` is dropped too (JS truthiness).
    if plan_mode_required {
        args.push("--plan-mode-required".to_string());
    }
    if let Some(agent_type) = agent_type.filter(|value| !value.is_empty()) {
        args.push(format!("--agent-type {}", shell_quote(agent_type)));
    }
    args.join(" ")
}

/// Maps to: CC `spawnMultiAgent.ts:399-440` / `:606-647` — the identical
/// `cd … && env … <binary> <identity args><flags>` command both pane handlers
/// build inline.
fn build_teammate_spawn_command(
    working_dir: &str,
    teammate_id: &str,
    sanitized_name: &str,
    team_name: &str,
    teammate_color: &str,
    plan_mode_required: bool,
    agent_type: Option<&str>,
    model: &str,
    permission_mode: Option<crate::types::permissions::PermissionMode>,
) -> String {
    let binary_path = get_teammate_command();
    let teammate_args = build_teammate_identity_cli_args(
        teammate_id,
        sanitized_name,
        team_name,
        teammate_color,
        plan_mode_required,
        agent_type,
    );
    let inherited_flags = apply_teammate_model_flag(
        build_inherited_cli_flags(plan_mode_required, permission_mode),
        model,
    );
    let flags_str = if inherited_flags.is_empty() {
        String::new()
    } else {
        format!(" {inherited_flags}")
    };
    let env_str = build_inherited_env_vars();
    format!(
        "cd {} && env {} {} {}{}",
        shell_quote(working_dir),
        env_str,
        shell_quote(&binary_path),
        teammate_args,
        flags_str
    )
}

/// Maps to: CC `getAppState().toolPermissionContext.mode` as read by
/// `handleSpawnSplitPane` (`:320`/`:420`) and `handleSpawnSeparateWindow`
/// (`:560`/`:627`). The read is LIVE — a permission-mode switch between query
/// start and the spawn decides which mode the teammate process inherits.
///
/// `None` when the context carries no store (tests, headless call sites that
/// build a `ToolUseContext` directly); those fall back to the query-start
/// snapshot, the same policy as `agent_tool::live_tool_permission_context`.
fn live_permission_mode(
    context: &crate::tool::ToolUseContext,
) -> crate::types::permissions::PermissionMode {
    context
        .get_app_state()
        .map(|state| state.tool_permission_context.mode)
        .unwrap_or(context.tool_permission_context.mode)
}

/// Maps to: CC `getAppState().mainLoopModel` as read by all three handlers
/// (`:313`/`:553`/`:848`) when resolving the teammate model. Same live-vs-
/// snapshot rule as [`live_permission_mode`].
fn live_main_loop_model(context: &crate::tool::ToolUseContext) -> Option<String> {
    context
        .get_app_state()
        .and_then(|state| state.main_loop_model.clone())
        .or_else(|| context.main_loop_model.clone())
}

/// Maps to the `{ stdout, stderr, code }` result returned by CC
/// `execFileNoThrow(TMUX_COMMAND, ...)` calls in `spawnMultiAgent.ts`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct TmuxCommandResult {
    stdout: String,
    stderr: String,
    code: i32,
}

fn run_tmux(args: &[String]) -> TmuxCommandResult {
    let mut command = std::process::Command::new(TMUX_COMMAND);
    // CC inherits process.env (execFileNoThrow); the carrier is its counterpart.
    crate::utils::subprocess_env::apply_process_env_std(&mut command);
    let output = command.args(args).output();
    match output {
        Ok(output) => TmuxCommandResult {
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            code: output.status.code().unwrap_or(1),
        },
        Err(error) => TmuxCommandResult {
            stdout: String::new(),
            stderr: error.to_string(),
            code: 1,
        },
    }
}

/// Maps to: CC `spawnMultiAgent.ts#hasSession` args.
pub fn has_session_args(session_name: &str) -> Vec<String> {
    vec![
        "has-session".to_string(),
        "-t".to_string(),
        session_name.to_string(),
    ]
}

/// Maps to: CC `spawnMultiAgent.ts#ensureSession` creation args.
pub fn new_session_args(session_name: &str) -> Vec<String> {
    vec![
        "new-session".to_string(),
        "-d".to_string(),
        "-s".to_string(),
        session_name.to_string(),
    ]
}

/// Maps to: CC `spawnMultiAgent.ts#handleSpawnSeparateWindow` `windowName`.
pub fn separate_window_name(sanitized_name: &str) -> String {
    format!(
        "teammate-{}",
        crate::utils::swarm::team_helpers::sanitize_name(sanitized_name)
    )
}

/// Maps to: CC `spawnMultiAgent.ts#handleSpawnSeparateWindow` `new-window` args.
pub fn new_window_args(session_name: &str, window_name: &str) -> Vec<String> {
    vec![
        "new-window".to_string(),
        "-t".to_string(),
        session_name.to_string(),
        "-n".to_string(),
        window_name.to_string(),
        "-P".to_string(),
        "-F".to_string(),
        "#{pane_id}".to_string(),
    ]
}

/// Maps to: CC `spawnMultiAgent.ts#handleSpawnSeparateWindow` `send-keys` args.
pub fn send_window_command_args(
    session_name: &str,
    window_name: &str,
    spawn_command: &str,
) -> Vec<String> {
    vec![
        "send-keys".to_string(),
        "-t".to_string(),
        format!("{session_name}:{window_name}"),
        spawn_command.to_string(),
        "Enter".to_string(),
    ]
}

/// Maps to: CC `spawnMultiAgent.ts#hasSession`.
async fn has_session(session_name: &str) -> bool {
    run_tmux(&has_session_args(session_name)).code == 0
}

/// Maps to: CC `spawnMultiAgent.ts#ensureSession`.
async fn ensure_session(session_name: &str) -> Result<(), String> {
    if has_session(session_name).await {
        return Ok(());
    }
    let result = run_tmux(&new_session_args(session_name));
    if result.code != 0 {
        return Err(format!(
            "Failed to create tmux session '{session_name}': {}",
            if result.stderr.is_empty() {
                "Unknown error".to_string()
            } else {
                result.stderr
            }
        ));
    }
    Ok(())
}

/// Maps to: CC `spawnMultiAgent.ts#handleSpawnInProcess`.
pub async fn handle_spawn_in_process(
    input: SpawnTeammateConfig,
    context: &crate::tool::ToolUseContext,
    selected_agent: Option<AgentDefinition>,
    tool_use_id: Option<&str>,
) -> Result<SpawnOutput, String> {
    if input.name.trim().is_empty() || input.prompt.trim().is_empty() {
        return Err("name and prompt are required for spawn operation".to_string());
    }

    let team_name = input
        .team_name
        .clone()
        .or_else(crate::utils::swarm::team_helpers::current_team_name)
        .ok_or_else(|| {
            "team_name is required for spawn operation. Either provide team_name in input or call spawnTeam first to establish team context."
                .to_string()
        })?;

    // CC spawnMultiAgent.ts:989 (handleSpawnInProcess) `readTeamFileAsync` —
    // a pure disk read; this is a read-modify-WRITE chain, so a memory-first
    // read here would write a stale leader copy back over teammate on-disk
    // updates.
    let Some(mut team_record) = crate::utils::swarm::team_helpers::read_team_file(&team_name)
    else {
        return Err(format!(
            "Team \"{team_name}\" does not exist. Call spawnTeam first to create the team."
        ));
    };

    let unique_name = generate_unique_teammate_name(&input.name, Some(&team_name));
    let sanitized_name = sanitize_agent_name(&unique_name);
    let teammate_id = format_agent_id(&sanitized_name, &team_name);
    let teammate_color = assign_teammate_color(&teammate_id)
        .official_name()
        .to_string();
    // CC `:848` `resolveTeammateModel(input.model, getAppState().mainLoopModel)`.
    let model = resolve_teammate_model(
        input.model.as_deref(),
        live_main_loop_model(context).as_deref(),
    );
    let plan_mode_required = input.plan_mode_required.unwrap_or(false);

    // CC `:877-887` — resolve the custom agent definition and log the lookup.
    log_for_debugging(&format!(
        "[handleSpawnInProcess] agent_type={}, found={}",
        input.agent_type.as_deref().unwrap_or("undefined"),
        selected_agent.is_some()
    ));

    // Maps to CC `handleSpawnInProcess` delegating to
    // `utils/swarm/spawnInProcess.ts#spawnInProcessTeammate`.
    let spawn = crate::utils::swarm::spawn_in_process::spawn_in_process_teammate(
        crate::utils::swarm::spawn_in_process::InProcessSpawnConfig {
            name: sanitized_name.clone(),
            team_name: team_name.clone(),
            prompt: input.prompt.clone(),
            color: Some(teammate_color.clone()),
            plan_mode_required,
            model: Some(model.clone()),
        },
        crate::utils::swarm::spawn_in_process::SpawnContext {
            tool_use_id: tool_use_id.map(ToOwned::to_owned),
        },
    )
    .await;

    if !spawn.success {
        return Err(spawn
            .error
            .unwrap_or_else(|| "Failed to spawn in-process teammate".to_string()));
    }

    // CC `:906-908`.
    log_for_debugging(&format!(
        "[handleSpawnInProcess] spawn result: taskId={}, hasContext={}, hasAbort={}",
        spawn.task_id.as_deref().unwrap_or("undefined"),
        spawn.teammate_context.is_some(),
        spawn.abort_controller.is_some()
    ));

    // Maps to CC `handleSpawnInProcess` immediately calling
    // `utils/swarm/inProcessRunner.ts#startInProcessTeammate` after task
    // registration. Rust's runner is an incremental first-turn port; it still
    // lives under `utils/swarm/in_process_runner.rs`, not AgentTool.
    if let (Some(task_id), Some(abort_controller), Some(teammate_context)) = (
        spawn.task_id.clone(),
        spawn.abort_controller.clone(),
        spawn.teammate_context.clone(),
    ) {
        crate::utils::swarm::in_process_runner::start_in_process_teammate(
            crate::utils::swarm::in_process_runner::InProcessRunnerConfig {
                identity: crate::tasks::in_process_teammate_task::TeammateIdentity {
                    agent_id: teammate_id.clone(),
                    agent_name: sanitized_name.clone(),
                    team_name: team_name.clone(),
                    color: Some(teammate_color.clone()),
                    plan_mode_required,
                    parent_session_id: teammate_context.parent_session_id.clone(),
                },
                task_id,
                prompt: input.prompt.clone(),
                description: input.description.clone(),
                model: Some(model.clone()),
                allowed_tools: None,
                agent_definition: selected_agent.clone(),
                teammate_context,
                tool_use_context: context.clone().with_messages(Vec::new()),
                abort_controller,
                invoking_request_id: input.invoking_request_id.clone(),
            },
        )?;
        // CC `:935-937`.
        log_for_debugging(&format!(
            "[handleSpawnInProcess] Started agent execution for {teammate_id}"
        ));
    }

    // Maps to CC `spawnMultiAgent.ts:1005` — and the two teammates-map writes
    // at `:958`/`:980`. The in-process handler records `cwd: getCwd()`
    // UNCONDITIONALLY; unlike the pane and separate-window handlers it never
    // destructures `cwd` off the input at all. Reading `input.cwd` here gave
    // the in-process backend a working directory CC's can never have.
    let cwd = crate::bootstrap::state::get_original_cwd()
        .display()
        .to_string();
    team_record.members.push(TeamMemberRecord {
        agent_id: teammate_id.clone(),
        name: sanitized_name.clone(),
        agent_type: input.agent_type.clone(),
        model: Some(model.clone()),
        prompt: Some(input.prompt),
        color: Some(teammate_color.clone()),
        plan_mode_required: Some(plan_mode_required),
        joined_at_ms: now_ms(),
        tmux_pane_id: "in-process".to_string(),
        cwd,
        worktree_path: None,
        session_id: None,
        subscriptions: Vec::new(),
        backend_type: Some("in-process".to_string()),
        is_active: Some(true),
        mode: None,
    });
    write_team_record(team_record);

    // CC `:940-986` — track the teammate in `AppState.teamContext`, auto-
    // registering the leader when spawning without a prior spawnTeam call.
    track_teammate_in_app_state(
        context,
        TrackTeammateInAppState {
            teammate_id: teammate_id.clone(),
            team_name: team_name.clone(),
            sanitized_name: sanitized_name.clone(),
            agent_type: input.agent_type.clone(),
            teammate_color: teammate_color.clone(),
            tmux_session_name: "in-process".to_string(),
            tmux_pane_id: "in-process".to_string(),
            cwd: crate::bootstrap::state::get_original_cwd()
                .display()
                .to_string(),
            register_leader: true,
        },
    );

    Ok(SpawnOutput {
        teammate_id: teammate_id.clone(),
        agent_id: teammate_id,
        agent_type: input.agent_type,
        model: Some(model),
        name: sanitized_name,
        color: Some(teammate_color),
        tmux_session_name: "in-process".to_string(),
        tmux_window_name: "in-process".to_string(),
        tmux_pane_id: "in-process".to_string(),
        team_name: Some(team_name),
        is_splitpane: Some(false),
        plan_mode_required: Some(plan_mode_required),
    })
}

/// Arguments for [`track_teammate_in_app_state`].
struct TrackTeammateInAppState {
    teammate_id: String,
    team_name: String,
    sanitized_name: String,
    agent_type: Option<String>,
    teammate_color: String,
    tmux_session_name: String,
    tmux_pane_id: String,
    cwd: String,
    /// CC `:942-962` — only `handleSpawnInProcess` auto-registers the leader.
    /// The two pane handlers (`:452-472`, `:665-685`) keep
    /// `leadAgentId: prev.teamContext?.leadAgentId ?? ''`.
    register_leader: bool,
}

/// Maps to: CC `spawnMultiAgent.ts:452-472` / `:665-685` / `:940-986` — the
/// `setAppState(prev => ({ ...prev, teamContext: { … } }))` every handler runs
/// right after spawning.
///
/// This write is synchronous with the tool result in CC. Cometix previously had
/// no writer at all and relied on `use_inbox_poller.rs#current_team_context_from_team_record`
/// rebuilding the projection from the team FILE on the next REPL poll tick —
/// which is stale between the spawn and that tick, never runs at all in
/// headless (no poll loop), and cannot recover `tmuxSessionName` for pane
/// teammates because the team file does not persist it.
fn track_teammate_in_app_state(
    context: &crate::tool::ToolUseContext,
    params: TrackTeammateInAppState,
) {
    use crate::hooks::use_inbox_poller::{InboxPollerTeamContext, InboxPollerTeammateInfo};

    let spawned_at = now_ms();
    context.set_app_state(move |state| {
        let previous = state.team_context.as_deref().cloned();
        let mut teammates = previous
            .as_ref()
            .map(|context| context.teammates.clone())
            .unwrap_or_default();

        // CC `:943-946` — `needsLeaderSetup = !prev.teamContext?.leadAgentId`.
        let previous_lead = previous
            .as_ref()
            .map(|context| context.lead_agent_id.clone())
            .unwrap_or_default();
        let needs_leader_setup = params.register_leader && previous_lead.is_empty();
        let lead_agent_id = if needs_leader_setup {
            format_agent_id(TEAM_LEAD_NAME, &params.team_name)
        } else {
            previous_lead
        };

        // CC `:950-962` — the leader entry exists so inbox polling can find it.
        if needs_leader_setup {
            let lead_color = assign_teammate_color(&lead_agent_id)
                .official_name()
                .to_string();
            teammates.insert(
                lead_agent_id.clone(),
                InboxPollerTeammateInfo {
                    name: TEAM_LEAD_NAME.to_string(),
                    agent_type: Some(TEAM_LEAD_NAME.to_string()),
                    color: Some(lead_color),
                    tmux_session_name: Some("in-process".to_string()),
                    tmux_pane_id: Some("leader".to_string()),
                    cwd: Some(params.cwd.clone()),
                    worktree_path: None,
                    spawned_at: Some(spawned_at),
                    backend_type: None,
                },
            );
        }

        teammates.insert(
            params.teammate_id.clone(),
            InboxPollerTeammateInfo {
                name: params.sanitized_name.clone(),
                agent_type: params.agent_type.clone(),
                color: Some(params.teammate_color.clone()),
                tmux_session_name: Some(params.tmux_session_name.clone()),
                tmux_pane_id: Some(params.tmux_pane_id.clone()),
                cwd: Some(params.cwd.clone()),
                worktree_path: None,
                spawned_at: Some(spawned_at),
                backend_type: None,
            },
        );

        state.team_context = Some(std::sync::Arc::new(InboxPollerTeamContext {
            // CC `:456` `teamName ?? prev.teamContext?.teamName ?? 'default'` —
            // `teamName` is non-optional by this point in all three handlers.
            team_name: params.team_name.clone(),
            // CC `:457` `prev.teamContext?.teamFilePath ?? ''`.
            team_file_path: previous
                .as_ref()
                .map(|context| context.team_file_path.clone())
                .unwrap_or_default(),
            lead_agent_id,
            // CC writes exactly four keys over `...prev.teamContext`
            // (`:455-471`): teamName, teamFilePath, leadAgentId, teammates.
            // Everything else is whatever the spread carried — ABSENT when
            // there was no prior teamContext. Defaulting these to leader values
            // would be an invention; `useInboxPoller.ts`'s own projection is
            // what fills them for a leader session.
            self_agent_id: previous
                .as_ref()
                .and_then(|context| context.self_agent_id.clone()),
            self_agent_name: previous
                .as_ref()
                .and_then(|context| context.self_agent_name.clone()),
            is_leader: previous.as_ref().and_then(|context| context.is_leader),
            self_agent_color: previous
                .as_ref()
                .and_then(|context| context.self_agent_color.clone()),
            teammates,
        }));
    });
}

/// Maps to: CC `spawnMultiAgent.ts:760-834#registerOutOfProcessTeammateTask`
/// parameter object (`:773-784`).
struct RegisterOutOfProcessTeammateTask<'a> {
    teammate_id: &'a str,
    sanitized_name: &'a str,
    team_name: &'a str,
    color: Option<String>,
    prompt: &'a str,
    plan_mode_required: bool,
    model: Option<String>,
    pane_id: String,
    inside_tmux: bool,
    backend_type: BackendType,
    tool_use_id: Option<&'a str>,
}

/// Maps to: CC `spawnMultiAgent.ts:760-834#registerOutOfProcessTeammateTask`.
fn register_out_of_process_teammate_task(params: RegisterOutOfProcessTeammateTask<'_>) -> String {
    let task_id = crate::utils::swarm::spawn_in_process::generate_in_process_teammate_task_id();
    let description = format!(
        "{}: {}{}",
        params.sanitized_name,
        params.prompt.chars().take(50).collect::<String>(),
        if params.prompt.chars().count() > 50 {
            "..."
        } else {
            ""
        }
    );
    let state = crate::tasks::in_process_teammate_task::register_in_process_teammate_task(
        crate::tasks::in_process_teammate_task::RegisterInProcessTeammateParams {
            task_id: task_id.clone(),
            identity: crate::tasks::in_process_teammate_task::TeammateIdentity {
                agent_id: params.teammate_id.to_string(),
                agent_name: params.sanitized_name.to_string(),
                team_name: params.team_name.to_string(),
                color: params.color,
                plan_mode_required: params.plan_mode_required,
                parent_session_id: crate::bootstrap::state::get_session_id(),
            },
            description,
            prompt: params.prompt.to_string(),
            selected_agent: None,
            model: params.model,
            permission_mode: if params.plan_mode_required {
                crate::types::permissions::PermissionMode::Plan
            } else {
                crate::types::permissions::PermissionMode::Default
            },
            tool_use_id: params.tool_use_id.map(ToOwned::to_owned),
        },
    );

    // CC `:821-833` — when abort is signaled, kill the pane using the backend
    // that created it. Without this the abort in
    // `kill_in_process_teammate` (the target of TaskStop and the prompt-input
    // kill) flipped the task status while the `cometix` process kept running in
    // an orphan pane.
    if let Some(abort_controller) = state.abort_controller {
        let _ = register_pane_kill_on_abort(
            abort_controller,
            params.pane_id,
            params.inside_tmux,
            params.backend_type,
        );
    }

    task_id
}

/// Maps to: CC `spawnMultiAgent.ts:825-833`
/// `abortController.signal.addEventListener('abort', …, { once: true })`.
///
/// The port's `addEventListener('abort')` shape is awaiting
/// `AbortController::signal().aborted()` (same as
/// `in_process_runner.rs:191`). The wait must outlive the spawning turn — CC's
/// listener lives as long as the teammate — so it goes on the PROCESS runtime
/// via `runtime_handle_for_detached_work()` (PORTING.md § "Node-async → tokio"
/// A3). A bare `Handle::try_current()` here would land on the per-query
/// runtime and be destroyed the moment the spawn turn ended, i.e. always
/// before the abort it is waiting for.
fn register_pane_kill_on_abort(
    abort_controller: crate::tool::AbortController,
    pane_id: String,
    inside_tmux: bool,
    backend_type: BackendType,
) -> bool {
    // CC `:828` `if (isPaneBackend(backendType))` — evaluated inside the
    // listener, but the value is captured at registration and cannot change.
    if !is_pane_backend(backend_type) {
        return false;
    }
    let Some(handle) = crate::utils::process_runtime::runtime_handle_for_detached_work() else {
        log_for_debugging(
            "[registerOutOfProcessTeammateTask] no process runtime available; \
             pane kill-on-abort not registered",
        );
        return false;
    };
    handle.spawn(async move {
        let mut signal = abort_controller.signal();
        signal.aborted().await;
        // CC `:829` `void getBackendByType(backendType).killPane(paneId, !insideTmux)`.
        match backend_type {
            BackendType::Tmux => {
                crate::utils::swarm::backends::tmux_backend::TmuxBackend::new()
                    .kill_pane(&pane_id, !inside_tmux)
                    .await;
            }
            BackendType::ITerm2 => {
                crate::utils::swarm::backends::iterm_backend::ITermBackend::new()
                    .kill_pane(&pane_id, !inside_tmux)
                    .await;
            }
            // `is_pane_backend` already excluded this.
            BackendType::InProcess => {}
        }
    });
    true
}

/// Maps to: CC `spawnMultiAgent.ts:305-539#handleSpawnSplitPane`.
///
/// **Inlined, not delegated.** This used to call
/// `PaneBackendExecutor::spawn(...)`, which is the port of a DIFFERENT CC
/// function (`PaneBackendExecutor.ts:79-200`). The two are not
/// interchangeable, and routing production traffic through the wrong one cost
/// three behaviors:
///   * the executor calls the `spawnUtils.ts` `buildInheritedCliFlags`, which
///     has no `auto` arm and appends `--teammate-mode`; an auto-mode leader
///     therefore spawned teammates that fell back to `default` and then blocked
///     on prompts in a pane nobody watches;
///   * the executor takes no `ToolUseContext`-independent live state, so the
///     permission mode and main-loop model came from the query-start snapshot
///     instead of `getAppState()` (`:313`/`:320`/`:420`);
///   * `AppState.teamContext` (`:452-472`) was never written.
/// CC's own `PaneBackendExecutor` is reached only through
/// `registry.ts#getTeammateExecutor`, which has ZERO production callers in
/// 2.1.88 — it is a vestigial abstraction, not this path's owner. The Rust
/// executor is kept as a faithful port of that class (its `kill` is reachable
/// from `team_helpers.rs`), it just no longer serves this handler.
pub async fn handle_spawn_pane(
    input: SpawnTeammateConfig,
    context: &crate::tool::ToolUseContext,
    detection: crate::utils::swarm::backends::types::BackendDetectionResult,
    tool_use_id: Option<&str>,
) -> Result<SpawnOutput, String> {
    if input.name.trim().is_empty() || input.prompt.trim().is_empty() {
        return Err("name and prompt are required for spawn operation".to_string());
    }

    let team_name = input
        .team_name
        .clone()
        .or_else(crate::utils::swarm::team_helpers::current_team_name)
        .ok_or_else(|| {
            "team_name is required for spawn operation. Either provide team_name in input or call spawnTeam first to establish team context."
                .to_string()
        })?;

    // CC spawnMultiAgent.ts:489 (handleSpawnSplitPane) `readTeamFileAsync` —
    // a pure disk read; this is a read-modify-WRITE chain, so a memory-first
    // read here would write a stale leader copy back over teammate on-disk
    // updates.
    let Some(mut team_record) = crate::utils::swarm::team_helpers::read_team_file(&team_name)
    else {
        return Err(format!(
            "Team \"{team_name}\" does not exist. Call spawnTeam first to create the team."
        ));
    };

    let unique_name = generate_unique_teammate_name(&input.name, Some(&team_name));
    let sanitized_name = sanitize_agent_name(&unique_name);
    let teammate_id = format_agent_id(&sanitized_name, &team_name);
    let teammate_color = assign_teammate_color(&teammate_id);
    let teammate_color_name = teammate_color.official_name().to_string();
    // CC `:313` `resolveTeammateModel(input.model, getAppState().mainLoopModel)`.
    let model = resolve_teammate_model(
        input.model.as_deref(),
        live_main_loop_model(context).as_deref(),
    );
    let plan_mode_required = input.plan_mode_required.unwrap_or(false);
    // Maps to CC `spawnMultiAgent.ts:337` `const workingDir = cwd || getCwd()`
    // — truthiness, so an empty string falls back instead of becoming the
    // working directory.
    let cwd = input
        .cwd
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            crate::bootstrap::state::get_original_cwd()
                .display()
                .to_string()
        });

    // CC `:379` — checked BEFORE the pane is created; it decides both the
    // socket used for `sendCommandToPane`/`killPane` and the session name.
    let inside_tmux = is_inside_tmux().await;

    // CC `:388-397` — create the pane, then enable border status on the first
    // teammate when inside tmux.
    let pane = create_teammate_pane_in_swarm_view(&sanitized_name, teammate_color).await?;
    let pane_id = pane.pane_id;
    if pane.is_first_teammate && inside_tmux {
        enable_pane_border_status(None, false).await?;
    }

    // CC `:399-440`.
    let spawn_command = build_teammate_spawn_command(
        &cwd,
        &teammate_id,
        &sanitized_name,
        &team_name,
        &teammate_color_name,
        plan_mode_required,
        input.agent_type.as_deref(),
        &model,
        Some(live_permission_mode(context)),
    );

    // CC `:444` — swarm socket when running outside tmux.
    send_command_to_pane(&pane_id, &spawn_command, !inside_tmux).await?;

    // CC `:447-448`.
    let session_name = if inside_tmux {
        "current".to_string()
    } else {
        SWARM_SESSION_NAME.to_string()
    };
    let window_name = if inside_tmux {
        "current".to_string()
    } else {
        SWARM_VIEW_WINDOW_NAME.to_string()
    };

    // CC `:452-472`.
    track_teammate_in_app_state(
        context,
        TrackTeammateInAppState {
            teammate_id: teammate_id.clone(),
            team_name: team_name.clone(),
            sanitized_name: sanitized_name.clone(),
            agent_type: input.agent_type.clone(),
            teammate_color: teammate_color_name.clone(),
            tmux_session_name: session_name.clone(),
            tmux_pane_id: pane_id.clone(),
            cwd: cwd.clone(),
            register_leader: false,
        },
    );

    // CC `:475-486`.
    register_out_of_process_teammate_task(RegisterOutOfProcessTeammateTask {
        teammate_id: &teammate_id,
        sanitized_name: &sanitized_name,
        team_name: &team_name,
        color: Some(teammate_color_name.clone()),
        prompt: &input.prompt,
        plan_mode_required,
        model: Some(model.clone()),
        pane_id: pane_id.clone(),
        inside_tmux,
        backend_type: detection.backend_type.as_backend_type(),
        tool_use_id,
    });

    team_record.members.push(TeamMemberRecord {
        agent_id: teammate_id.clone(),
        name: sanitized_name.clone(),
        agent_type: input.agent_type.clone(),
        model: Some(model.clone()),
        prompt: Some(input.prompt.clone()),
        color: Some(teammate_color_name.clone()),
        plan_mode_required: Some(plan_mode_required),
        joined_at_ms: now_ms(),
        tmux_pane_id: pane_id.clone(),
        cwd,
        worktree_path: None,
        session_id: None,
        subscriptions: Vec::new(),
        backend_type: Some(
            detection
                .backend_type
                .as_backend_type()
                .as_str()
                .to_string(),
        ),
        is_active: Some(true),
        mode: None,
    });
    write_team_record(team_record);

    // CC `:513-521` — the teammate's inbox poller picks this up as its first
    // turn. Previously this lived inside `PaneBackendExecutor::spawn`.
    crate::utils::teammate_mailbox::write_to_mailbox(
        &sanitized_name,
        crate::utils::teammate_mailbox::TeammateMessageInput {
            from: TEAM_LEAD_NAME.to_string(),
            text: input.prompt,
            timestamp: chrono::Utc::now().to_rfc3339(),
            color: None,
            summary: None,
        },
        Some(&team_name),
    )
    .map_err(|error| format!("Failed to deliver initial teammate prompt: {error}"))?;

    Ok(SpawnOutput {
        teammate_id: teammate_id.clone(),
        agent_id: teammate_id,
        agent_type: input.agent_type,
        model: Some(model),
        name: sanitized_name,
        color: Some(teammate_color_name),
        tmux_session_name: session_name,
        tmux_window_name: window_name,
        tmux_pane_id: pane_id,
        team_name: Some(team_name),
        is_splitpane: Some(true),
        plan_mode_required: Some(plan_mode_required),
    })
}

/// Maps to: CC `spawnMultiAgent.ts:545-753#handleSpawnSeparateWindow`.
pub async fn handle_spawn_separate_window(
    input: SpawnTeammateConfig,
    context: &crate::tool::ToolUseContext,
    tool_use_id: Option<&str>,
) -> Result<SpawnOutput, String> {
    if input.name.trim().is_empty() || input.prompt.trim().is_empty() {
        return Err("name and prompt are required for spawn operation".to_string());
    }

    let team_name = input
        .team_name
        .clone()
        .or_else(crate::utils::swarm::team_helpers::current_team_name)
        .ok_or_else(|| {
            "team_name is required for spawn operation. Either provide team_name in input or call spawnTeam first to establish team context."
                .to_string()
        })?;

    // CC spawnMultiAgent.ts:703 (handleSpawnSeparateWindow)
    // `readTeamFileAsync` — a pure disk read; this is a read-modify-WRITE
    // chain, so a memory-first read here would write a stale leader copy back
    // over teammate on-disk updates.
    let Some(mut team_record) = crate::utils::swarm::team_helpers::read_team_file(&team_name)
    else {
        return Err(format!(
            "Team \"{team_name}\" does not exist. Call spawnTeam first to create the team."
        ));
    };

    let unique_name = generate_unique_teammate_name(&input.name, Some(&team_name));
    let sanitized_name = sanitize_agent_name(&unique_name);
    let teammate_id = format_agent_id(&sanitized_name, &team_name);
    let window_name = separate_window_name(&sanitized_name);
    // Maps to CC `spawnMultiAgent.ts:578`, same `cwd || getCwd()` truthiness as
    // the split-pane handler above.
    let cwd = input
        .cwd
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            crate::bootstrap::state::get_original_cwd()
                .display()
                .to_string()
        });
    let teammate_color = assign_teammate_color(&teammate_id);
    let teammate_color_name = teammate_color.official_name().to_string();
    // CC `:553` `resolveTeammateModel(input.model, getAppState().mainLoopModel)`.
    let model = resolve_teammate_model(
        input.model.as_deref(),
        live_main_loop_model(context).as_deref(),
    );
    let plan_mode_required = input.plan_mode_required.unwrap_or(false);

    ensure_session(SWARM_SESSION_NAME).await?;

    let create_window_result = run_tmux(&new_window_args(SWARM_SESSION_NAME, &window_name));
    if create_window_result.code != 0 {
        return Err(format!(
            "Failed to create tmux window: {}",
            create_window_result.stderr
        ));
    }
    let pane_id = create_window_result.stdout.trim().to_string();

    // CC `:606-647` — built inline here too, off the same module-local
    // `buildInheritedCliFlags` as the split-pane handler (`:625`).
    let spawn_command = build_teammate_spawn_command(
        &cwd,
        &teammate_id,
        &sanitized_name,
        &team_name,
        &teammate_color_name,
        plan_mode_required,
        input.agent_type.as_deref(),
        &model,
        Some(live_permission_mode(context)),
    );

    let send_keys_result = run_tmux(&send_window_command_args(
        SWARM_SESSION_NAME,
        &window_name,
        &spawn_command,
    ));
    if send_keys_result.code != 0 {
        return Err(format!(
            "Failed to send command to tmux window: {}",
            send_keys_result.stderr
        ));
    }

    // CC `:665-685`.
    track_teammate_in_app_state(
        context,
        TrackTeammateInAppState {
            teammate_id: teammate_id.clone(),
            team_name: team_name.clone(),
            sanitized_name: sanitized_name.clone(),
            agent_type: input.agent_type.clone(),
            teammate_color: teammate_color_name.clone(),
            tmux_session_name: SWARM_SESSION_NAME.to_string(),
            tmux_pane_id: pane_id.clone(),
            cwd: cwd.clone(),
            register_leader: false,
        },
    );

    // CC `:687-700` — separate-window spawns are always outside tmux (external
    // swarm session), so `insideTmux: false` / `backendType: 'tmux'` are
    // hard-coded there too.
    register_out_of_process_teammate_task(RegisterOutOfProcessTeammateTask {
        teammate_id: &teammate_id,
        sanitized_name: &sanitized_name,
        team_name: &team_name,
        color: Some(teammate_color_name.clone()),
        prompt: &input.prompt,
        plan_mode_required,
        model: Some(model.clone()),
        pane_id: pane_id.clone(),
        inside_tmux: false,
        backend_type: BackendType::Tmux,
        tool_use_id,
    });

    team_record.members.push(TeamMemberRecord {
        agent_id: teammate_id.clone(),
        name: sanitized_name.clone(),
        agent_type: input.agent_type.clone(),
        model: Some(model.clone()),
        prompt: Some(input.prompt.clone()),
        color: Some(teammate_color_name.clone()),
        plan_mode_required: Some(plan_mode_required),
        joined_at_ms: now_ms(),
        tmux_pane_id: pane_id.clone(),
        cwd,
        worktree_path: None,
        session_id: None,
        subscriptions: Vec::new(),
        backend_type: Some("tmux".to_string()),
        is_active: Some(true),
        mode: None,
    });
    write_team_record(team_record);

    crate::utils::teammate_mailbox::write_to_mailbox(
        &sanitized_name,
        crate::utils::teammate_mailbox::TeammateMessageInput {
            from: TEAM_LEAD_NAME.to_string(),
            text: input.prompt,
            timestamp: chrono::Utc::now().to_rfc3339(),
            color: None,
            summary: None,
        },
        Some(&team_name),
    )
    .map_err(|error| format!("Failed to deliver initial teammate prompt: {error}"))?;

    Ok(SpawnOutput {
        teammate_id: teammate_id.clone(),
        agent_id: teammate_id,
        agent_type: input.agent_type,
        model: Some(model),
        name: sanitized_name,
        color: Some(teammate_color_name),
        tmux_session_name: SWARM_SESSION_NAME.to_string(),
        tmux_window_name: window_name,
        tmux_pane_id: pane_id,
        team_name: Some(team_name),
        is_splitpane: Some(false),
        plan_mode_required: Some(plan_mode_required),
    })
}

/// Maps to: CC `spawnMultiAgent.ts` iTerm2 setup prompt block inside
/// `handleSpawnSplitPane`.
async fn resolve_it2_setup_if_needed(
    detection: crate::utils::swarm::backends::types::BackendDetectionResult,
    context: &crate::tool::ToolUseContext,
) -> Result<crate::utils::swarm::backends::types::BackendDetectionResult, String> {
    if !detection.needs_it2_setup {
        return Ok(detection);
    }

    let Some(prompt_sink) = context.it2_setup_prompt_sink.0.as_ref() else {
        return Err(
            "Teammate spawn cancelled - iTerm2 setup required and no interactive ToolJSX prompt is available"
                .to_string(),
        );
    };

    let tmux_available = crate::utils::swarm::backends::detection::is_tmux_available().await;
    let setup_result = prompt_sink(tmux_available).await;
    match setup_result {
        crate::utils::swarm::it2_setup_prompt::It2SetupPromptResult::Cancelled => {
            return Err("Teammate spawn cancelled - iTerm2 setup required".to_string());
        }
        crate::utils::swarm::it2_setup_prompt::It2SetupPromptResult::Installed
        | crate::utils::swarm::it2_setup_prompt::It2SetupPromptResult::UseTmux => {
            reset_backend_detection();
        }
    }

    // CC `:372-375` — the re-detect is NOT wrapped, so its error propagates.
    let redetected = detect_and_get_backend().await?;
    if redetected.needs_it2_setup {
        return Err("Teammate spawn cancelled - iTerm2 setup did not complete".to_string());
    }
    Ok(redetected)
}

/// Maps to: CC `spawnMultiAgent.ts:1040-1078#handleSpawn` and
/// `:1088-1093#spawnTeammate` (a one-line delegation).
pub async fn spawn_teammate(
    config: SpawnTeammateConfig,
    context: &crate::tool::ToolUseContext,
    selected_agent: Option<AgentDefinition>,
    tool_use_id: Option<&str>,
) -> Result<SpawnOutput, String> {
    // CC `:1045-1047`.
    if is_in_process_enabled() {
        return handle_spawn_in_process(config, context, selected_agent, tool_use_id).await;
    }

    // CC `:1053-1069` — pre-flight detection, narrowly scoped so user
    // cancellation and other spawn errors propagate normally.
    let detection = match detect_and_get_backend().await {
        Ok(detection) => detection,
        Err(error) => {
            // CC `:1059-1061` — only fall back silently in auto mode. With an
            // explicit `teammateMode: 'tmux'` the ORIGINAL error propagates, so
            // the user sees the actionable install instructions (or the iTerm2
            // it2 message) rather than a substituted one.
            if get_teammate_mode_from_snapshot() != TeammateMode::Auto {
                return Err(error);
            }
            // CC `:1062-1064`.
            log_for_debugging(&format!(
                "[handleSpawn] No pane backend available, falling back to in-process: {error}"
            ));
            // CC `:1067`.
            mark_in_process_fallback();
            return handle_spawn_in_process(config, context, selected_agent, tool_use_id).await;
        }
    };

    // CC `:1073-1077`.
    let use_split_pane = config.use_splitpane != Some(false);
    if !use_split_pane {
        return handle_spawn_separate_window(config, context, tool_use_id).await;
    }

    // CC `:343-376` — the iTerm2 setup prompt lives inside
    // `handleSpawnSplitPane`; Cometix hoists it so the pane handler receives a
    // settled detection result.
    let detection = resolve_it2_setup_if_needed(detection, context).await?;

    handle_spawn_pane(config, context, detection, tool_use_id).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    /// Seeds the team in memory AND on disk — the spawn read-modify-write
    /// chain now goes through `read_team_file` (CC readTeamFileAsync), so the
    /// returned guards must stay alive for the test body.
    fn reset_team(name: &str) -> (EnvVarGuard, EnvVarGuard, EnvVarGuard) {
        let root = std::env::temp_dir().join(format!(
            "cometix-spawn-multi-agent-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let config_guard = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root);
        let io_guard = EnvVarGuard::set("COMETIX_TEST_TEAM_FILE_IO", "1");
        let write_guard = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        crate::utils::swarm::team_helpers::clear_team_tool_state_for_test();
        crate::utils::swarm::teammate_layout_manager::clear_teammate_colors();
        let record = crate::utils::swarm::team_helpers::create_team_record(
            name.to_string(),
            None,
            Some(TEAM_LEAD_NAME.to_string()),
            None,
            "/tmp".to_string(),
        );
        crate::utils::swarm::team_helpers::write_team_record(record);
        (config_guard, io_guard, write_guard)
    }

    #[test]
    fn resolve_teammate_model_handles_inherit_like_official() {
        assert!(resolve_teammate_model(Some("inherit"), Some("haiku")).contains("haiku"));
        assert!(resolve_teammate_model(Some("opus"), Some("haiku")).contains("opus"));
    }

    /// Maps to: CC `spawnMultiAgent.ts:93-101` — `resolveTeammateModel` returns
    /// `inputModel` VERBATIM. `parseUserSpecifiedModel` (`:79`) applies to the
    /// CONFIGURED default only.
    #[test]
    fn resolve_teammate_model_passes_the_input_through_unexpanded() {
        let mut config = crate::utils::config::GlobalConfig::default();
        config.teammate_default_model = Some(Some("haiku".to_string()));

        // The caller's alias survives to `--model` and TeamMemberRecord.model.
        assert_eq!(
            resolve_teammate_model_with_config(Some("opus"), Some("leader-model"), &config),
            "opus"
        );
        // An explicit full id is equally untouched.
        assert_eq!(
            resolve_teammate_model_with_config(
                Some("claude-opus-4-6-20260401"),
                Some("leader-model"),
                &config
            ),
            "claude-opus-4-6-20260401"
        );
        // `inherit` still substitutes the leader model (`:97-99`).
        assert_eq!(
            resolve_teammate_model_with_config(Some("inherit"), Some("leader-model"), &config),
            "leader-model"
        );
        // ... and falls through to the default when there is no leader model.
        assert!(
            resolve_teammate_model_with_config(Some("inherit"), None, &config).contains("haiku")
        );
        // No input at all => the configured default, which IS parsed (`:79`).
        assert!(
            resolve_teammate_model_with_config(None, Some("leader-model"), &config)
                .contains("haiku")
        );
    }

    /// Maps to: CC `spawnMultiAgent.ts:72-82#getDefaultTeammateModel` tri-state.
    #[test]
    fn default_teammate_model_respects_config_null_string_and_undefined() {
        let undefined = crate::utils::config::GlobalConfig::default();
        assert!(
            get_default_teammate_model_from_config(&undefined, Some("leader-model"))
                .contains("opus")
        );

        let mut inherit_leader = crate::utils::config::GlobalConfig::default();
        inherit_leader.teammate_default_model = Some(None);
        assert_eq!(
            get_default_teammate_model_from_config(&inherit_leader, Some("leader-model")),
            "leader-model"
        );
        assert!(get_default_teammate_model_from_config(&inherit_leader, None).contains("opus"));

        let mut configured = crate::utils::config::GlobalConfig::default();
        configured.teammate_default_model = Some(Some("haiku".to_string()));
        assert!(
            get_default_teammate_model_from_config(&configured, Some("leader-model"))
                .contains("haiku")
        );
    }

    /// Maps to: CC `spawnMultiAgent.ts:208-260` vs `spawnUtils.ts:38-87` — the
    /// two same-named functions that must NOT be merged.
    #[test]
    fn module_local_cli_flags_carry_auto_and_omit_teammate_mode() {
        use crate::types::permissions::PermissionMode;

        // CC `:226-231` — the arm the spawnUtils copy does not have. Without it
        // an auto-mode leader spawned teammates that fell back to `default`.
        let auto = build_inherited_cli_flags_with_options(BuildInheritedCliFlagsOptions {
            permission_mode: Some(PermissionMode::Auto),
            ..Default::default()
        });
        assert_eq!(auto, "--permission-mode auto");

        // CC `:208-260` never pushes `--teammate-mode`; `spawnUtils.ts:77-78`
        // always does.
        for mode in [
            PermissionMode::Default,
            PermissionMode::AcceptEdits,
            PermissionMode::BypassPermissions,
            PermissionMode::Auto,
        ] {
            let flags = build_inherited_cli_flags_with_options(BuildInheritedCliFlagsOptions {
                permission_mode: Some(mode),
                ..Default::default()
            });
            assert!(
                !flags.contains("--teammate-mode"),
                "mode={mode:?} flags={flags}"
            );
        }
        assert!(
            crate::utils::swarm::spawn_utils::build_inherited_cli_flags_with_options(
                BuildInheritedCliFlagsOptions {
                    permission_mode: Some(PermissionMode::Auto),
                    ..Default::default()
                }
            )
            .contains("--teammate-mode")
        );

        // CC `:217-219` — plan mode suppresses the whole permission branch,
        // auto included.
        let planned = build_inherited_cli_flags_with_options(BuildInheritedCliFlagsOptions {
            plan_mode_required: true,
            permission_mode: Some(PermissionMode::Auto),
            ..Default::default()
        });
        assert_eq!(planned, "");

        assert_eq!(
            build_inherited_cli_flags_with_options(BuildInheritedCliFlagsOptions {
                permission_mode: Some(PermissionMode::BypassPermissions),
                ..Default::default()
            }),
            "--dangerously-skip-permissions"
        );
    }

    /// Maps to: CC `spawnMultiAgent.ts:399-440` — the identity args plus the
    /// `--model` replacement at `:423-434`.
    #[test]
    fn pane_spawn_command_matches_official_shape_and_replaces_inherited_model() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _command_guard = EnvVarGuard::set(
            crate::utils::swarm::constants::TEAMMATE_COMMAND_ENV_VAR,
            "/bin/claude code",
        );

        let command = build_teammate_spawn_command(
            "/tmp/project dir",
            "reviewer@alpha team",
            "reviewer",
            "alpha team",
            "green",
            true,
            Some("general-purpose"),
            "opus",
            Some(crate::types::permissions::PermissionMode::Auto),
        );
        assert!(command.starts_with("cd '/tmp/project dir' && env CLAUDECODE=1"));
        assert!(command.contains("'/bin/claude code' --agent-id 'reviewer@alpha team'"));
        assert!(command.contains("--agent-name reviewer"));
        assert!(command.contains("--team-name 'alpha team'"));
        assert!(command.contains("--agent-color green"));
        assert!(command.contains("--plan-mode-required"));
        // CC `:411` — the spawnMultiAgent handlers emit `--agent-type`;
        // `PaneBackendExecutor.ts:117-126` does not.
        assert!(command.contains("--agent-type general-purpose"));
        assert!(command.ends_with(" --model opus"));
        // plan_mode_required suppresses the permission flag (CC `:217-219`),
        // and `--teammate-mode` never appears on this path.
        assert!(!command.contains("--permission-mode"));
        assert!(!command.contains("--teammate-mode"));
    }

    /// CC `:423-434` — an inherited `--model` is stripped, not duplicated.
    #[test]
    fn teammate_model_flag_replaces_any_inherited_one() {
        assert_eq!(
            apply_teammate_model_flag(
                "--permission-mode auto --model inherited --settings /tmp/s.json".to_string(),
                "opus"
            ),
            "--permission-mode auto --settings /tmp/s.json --model opus"
        );
        assert_eq!(
            apply_teammate_model_flag(String::new(), "opus"),
            "--model opus"
        );
        assert_eq!(
            apply_teammate_model_flag("--chrome".to_string(), ""),
            "--chrome"
        );
    }

    /// Maps to: CC `spawnMultiAgent.ts:828` `if (isPaneBackend(backendType))`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pane_kill_on_abort_is_registered_only_for_pane_backends() {
        assert!(!register_pane_kill_on_abort(
            crate::tool::AbortController::default(),
            "in-process".to_string(),
            false,
            BackendType::InProcess,
        ));
        assert!(register_pane_kill_on_abort(
            crate::tool::AbortController::default(),
            "%99".to_string(),
            true,
            BackendType::Tmux,
        ));
    }

    /// Maps to: CC `spawnMultiAgent.ts:789` + `:825-833` — the controller minted
    /// for an out-of-process teammate is the one `killInProcessTeammate` aborts,
    /// and it carries the pane-kill listener.
    ///
    /// The regression: `register_out_of_process_teammate_task` took no
    /// paneId/insideTmux/backendType and attached nothing, so TaskStop and the
    /// prompt-input kill flipped the task to `killed` while the teammate
    /// `cometix` process kept running in an orphan pane. This asserts the
    /// controller has a live subscriber path — the listener body itself runs
    /// `tmux kill-pane`, which a unit test must not execute.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn killing_an_out_of_process_teammate_wakes_the_pane_kill_listener() {
        let _task_lock = crate::tasks::in_process_teammate_task::TEST_IN_PROCESS_TEAMMATE_TASK_LOCK
            .lock()
            .unwrap();
        crate::tasks::in_process_teammate_task::clear_in_process_teammate_tasks_for_test();

        let task_id = register_out_of_process_teammate_task(RegisterOutOfProcessTeammateTask {
            teammate_id: "reviewer@alpha",
            sanitized_name: "reviewer",
            team_name: "alpha",
            color: Some("green".to_string()),
            prompt: "review the change",
            plan_mode_required: false,
            model: Some("opus".to_string()),
            pane_id: "%7".to_string(),
            inside_tmux: true,
            backend_type: BackendType::Tmux,
            tool_use_id: Some("toolu_spawn"),
        });

        let controller =
            crate::tasks::in_process_teammate_task::get_in_process_teammate_task(&task_id)
                .expect("task registered")
                .abort_controller
                .expect("out-of-process teammates carry an abort controller");
        let mut listener = controller.signal();
        assert!(!controller.is_aborted());

        assert!(crate::tasks::in_process_teammate_task::kill_in_process_teammate(&task_id));
        assert!(controller.is_aborted());
        tokio::time::timeout(std::time::Duration::from_secs(5), listener.aborted())
            .await
            .expect("the abort must wake a listener subscribed to this controller");

        crate::tasks::in_process_teammate_task::clear_in_process_teammate_tasks_for_test();
    }

    #[test]
    fn separate_window_tmux_args_match_official_handler() {
        assert_eq!(
            has_session_args(SWARM_SESSION_NAME),
            vec!["has-session", "-t", SWARM_SESSION_NAME]
        );
        assert_eq!(
            new_session_args(SWARM_SESSION_NAME),
            vec!["new-session", "-d", "-s", SWARM_SESSION_NAME]
        );
        assert_eq!(
            separate_window_name("reviewer@east"),
            "teammate-reviewer-east"
        );
        assert_eq!(
            new_window_args(SWARM_SESSION_NAME, "teammate-reviewer"),
            vec![
                "new-window",
                "-t",
                SWARM_SESSION_NAME,
                "-n",
                "teammate-reviewer",
                "-P",
                "-F",
                "#{pane_id}",
            ]
        );
        assert_eq!(
            send_window_command_args(SWARM_SESSION_NAME, "teammate-reviewer", "echo hi"),
            vec![
                "send-keys",
                "-t",
                "claude-swarm:teammate-reviewer",
                "echo hi",
                "Enter",
            ]
        );
    }

    #[tokio::test]
    async fn it2_setup_prompt_cancel_matches_official_spawn_error() {
        let detection = crate::utils::swarm::backends::types::BackendDetectionResult {
            backend_type: crate::utils::swarm::backends::types::PaneBackendType::ITerm2,
            is_native: true,
            needs_it2_setup: true,
        };
        let called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let called_for_sink = called.clone();
        let mut context = crate::tool::ToolUseContext::default();
        context.it2_setup_prompt_sink =
            crate::tool::It2SetupPromptSink(Some(std::sync::Arc::new(move |_tmux_available| {
                called_for_sink.store(true, std::sync::atomic::Ordering::SeqCst);
                Box::pin(async {
                    crate::utils::swarm::it2_setup_prompt::It2SetupPromptResult::Cancelled
                })
            })));

        let error = resolve_it2_setup_if_needed(detection, &context)
            .await
            .unwrap_err();
        assert_eq!(error, "Teammate spawn cancelled - iTerm2 setup required");
        assert!(called.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn it2_setup_without_tool_jsx_sink_is_guarded_not_silent() {
        let detection = crate::utils::swarm::backends::types::BackendDetectionResult {
            backend_type: crate::utils::swarm::backends::types::PaneBackendType::ITerm2,
            is_native: true,
            needs_it2_setup: true,
        };
        let error = resolve_it2_setup_if_needed(detection, &crate::tool::ToolUseContext::default())
            .await
            .unwrap_err();
        assert!(
            error.contains("iTerm2 setup required")
                && error.contains("no interactive ToolJSX prompt"),
            "error={error}"
        );
    }

    // multi_thread on purpose: the in-process spawn path detaches the teammate
    // runner via `runtime_handle_for_detached_work()`, whose fallback REFUSES
    // an unpublished `current_thread` ambient (that shape is a turn-scoped
    // per-query runtime in production — #150). The multi_thread test runtime
    // is the legitimate "ambient IS the process runtime" fallback target.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn spawn_teammate_uses_shared_boundary_and_updates_team_record() {
        let _backend_lock = crate::utils::swarm::backends::registry::TEST_BACKEND_REGISTRY_LOCK
            .lock()
            .unwrap();
        crate::utils::swarm::backends::registry::reset_backend_detection();
        crate::utils::swarm::backends::teammate_mode_snapshot::reset_teammate_mode_snapshot_for_test();
        crate::utils::swarm::backends::teammate_mode_snapshot::set_cli_teammate_mode_override(
            crate::utils::swarm::backends::teammate_mode_snapshot::TeammateMode::InProcess,
        );
        let _task_lock = crate::tasks::in_process_teammate_task::TEST_IN_PROCESS_TEAMMATE_TASK_LOCK
            .lock()
            .unwrap();
        let _team_state_lock = crate::utils::swarm::team_helpers::TEST_TEAM_HELPERS_LOCK
            .lock()
            .unwrap();
        crate::tasks::in_process_teammate_task::clear_in_process_teammate_tasks_for_test();
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _guards = reset_team("alpha");

        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        let context = crate::tool::ToolUseContext::default().with_app_store(store.clone());

        let output = spawn_teammate(
            SpawnTeammateConfig {
                name: "reviewer@east".to_string(),
                prompt: "review the change".to_string(),
                team_name: Some("alpha".to_string()),
                cwd: Some("/tmp/project".to_string()),
                use_splitpane: Some(true),
                plan_mode_required: Some(true),
                model: Some("inherit".to_string()),
                agent_type: Some("general-purpose".to_string()),
                description: Some("review".to_string()),
                invoking_request_id: Some("req-1".to_string()),
            },
            &context,
            None,
            Some("toolu_spawn"),
        )
        .await
        .unwrap();

        assert_eq!(output.status_for_test(), "teammate_spawned");
        assert_eq!(output.teammate_id, "reviewer-east@alpha");
        assert_eq!(output.name, "reviewer-east");
        assert_eq!(output.team_name.as_deref(), Some("alpha"));
        assert_eq!(output.tmux_session_name, "in-process");
        assert_eq!(output.plan_mode_required, Some(true));

        let record = crate::utils::swarm::team_helpers::read_team_file("alpha").unwrap();
        assert_eq!(record.members.len(), 2);
        let member = record
            .members
            .iter()
            .find(|member| member.agent_id == "reviewer-east@alpha")
            .unwrap();
        assert_eq!(member.backend_type.as_deref(), Some("in-process"));
        assert_eq!(member.tmux_pane_id, "in-process");
        // CC's in-process handler writes `cwd: getCwd()` unconditionally
        // (`spawnMultiAgent.ts:1005`, and `:958`/`:980` for the teammates map)
        // and never destructures `cwd` off the input. The `/tmp/project` above
        // is supplied on purpose and ignored on purpose.
        assert_eq!(
            member.cwd,
            crate::bootstrap::state::get_original_cwd()
                .display()
                .to_string()
        );
        assert_ne!(member.cwd, "/tmp/project");

        let task = crate::tasks::in_process_teammate_task::find_teammate_task_by_agent_id(
            "reviewer-east@alpha",
        )
        .unwrap();
        assert_eq!(task.tool_use_id.as_deref(), Some("toolu_spawn"));

        // CC `:940-986` — `setAppState(... teamContext ...)` runs synchronously
        // with the tool result. There used to be NO writer at all on this path;
        // `AppState.teamContext` only appeared on the next REPL poll tick (and
        // never at all in headless, which has no poll loop).
        let team_context = store
            .get()
            .team_context
            .clone()
            .expect("the spawn path must write AppState.teamContext");
        assert_eq!(team_context.team_name, "alpha");
        // CC `:943-946` — leader auto-registration when spawning without a
        // prior spawnTeam call.
        assert_eq!(team_context.lead_agent_id, "team-lead@alpha");
        let lead = team_context
            .teammates
            .get("team-lead@alpha")
            .expect("CC `:950-962` adds the leader entry for inbox polling");
        assert_eq!(lead.name, TEAM_LEAD_NAME);
        assert_eq!(lead.tmux_session_name.as_deref(), Some("in-process"));
        assert_eq!(lead.tmux_pane_id.as_deref(), Some("leader"));
        let teammate = team_context
            .teammates
            .get("reviewer-east@alpha")
            .expect("the spawned teammate is tracked");
        assert_eq!(teammate.name, "reviewer-east");
        assert_eq!(teammate.agent_type.as_deref(), Some("general-purpose"));
        assert_eq!(teammate.tmux_session_name.as_deref(), Some("in-process"));
        assert_eq!(teammate.tmux_pane_id.as_deref(), Some("in-process"));
        assert!(teammate.color.is_some());

        crate::utils::swarm::backends::registry::reset_backend_detection();
        crate::utils::swarm::backends::teammate_mode_snapshot::reset_teammate_mode_snapshot_for_test();
    }

    impl SpawnOutput {
        fn status_for_test(&self) -> &'static str {
            "teammate_spawned"
        }
    }
}
