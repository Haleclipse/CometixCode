//! Maps to: CC `main.tsx`.
//!
//! Called from [`crate::entrypoints::cli`] / [`crate::cli::dispatch`] after
//! fast-paths and `CliConfig` gating. Builds `initialState` + typed `ReplProps`,
//! creates `AppStore`, spawns MCP/LSP/statusline startup (Duty #17–18), then
//! delegates direct REPL mounting to [`crate::repl_launcher::launch_repl`] or
//! bare-resume mounting to [`crate::dialog_launchers::launch_resume_chooser`].
//!
//! Single file ≙ CC `main.tsx`. Process entry is `src/bin/cometix.rs`
//! (Rust-only glue → `entrypoints/cli`; `autobins = false` frees this path).

use crate::bootstrap;
use crate::commands;
use crate::components::spinner::Spinner;
use crate::hooks::notifs::startup::startup_notifications;
use crate::hooks::notifs::statusline::status_line_trust_blocked_notification;
use crate::interactive_helpers::{
    SetupScreensSnapshot, default_setup_screens_snapshot, exit_with_error, project_config_for_cwd,
    show_setup_screens,
};
use crate::plugins;
use crate::repl_launcher::launch_repl;
use crate::screens::repl::ReplProps;
use crate::services::mcp::config::{
    all_configured_mcp_servers_readonly, get_mcp_configs_by_scope_readonly,
};
use crate::services::mcp::types::ConfigScope;
#[cfg(not(test))]
use crate::services::mcp::use_manage_mcp_connections::{
    OnConnectionAttemptHandlers, register_on_connection_attempt_handlers,
};
use crate::state::app_state_store::AppState;
use crate::state::app_state_store::McpWriter;
use crate::state::store::AppStore;
use crate::utils;
use crate::utils::config::check_has_trust_dialog_accepted;
use crate::utils::config::load_global_config;
use crate::utils::settings::get_settings_with_errors;
use crate::utils::settings::{
    SettingSource, SettingsWithErrors, has_skip_dangerous_mode_permission_prompt,
    is_setting_source_enabled,
};
use crate::utils::settings::{SettingsJson, ValidationError};
use crate::utils::status::StartupDiagnosticsSnapshot;
use crate::utils::status::mcp_client_snapshots_from_config;
use crate::utils::thinking::{
    ThinkingConfig, parse_js_decimal_i64, should_enable_thinking_by_default,
};
use iocraft::prelude::*;
#[cfg(test)]
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex};

/// Rust-owned launch aggregate for CC `main.tsx`'s `initialState` and the
/// `replProps` passed through `replLauncher.launchRepl`.
#[derive(Debug, PartialEq)]
struct InteractiveLaunch {
    initial_state: AppState,
    repl_props: ReplProps,
}

/// Atomic result of CC `main.tsx`'s paired `thinkingEnabled` /
/// `thinkingConfig` startup calculation.
#[derive(Clone, Debug, PartialEq)]
struct ResolvedThinkingLaunch {
    enabled: bool,
    config: ThinkingConfig,
}

/// Maps to: CC `main.tsx:2955-3031` command/agent loading and main-thread
/// agent lookup. Definitions are one immutable startup snapshot shared by
/// AppState and `ReplProps`.
#[derive(Clone, Debug, PartialEq)]
struct ResolvedAgentLaunch {
    definitions: Arc<crate::tools::agent_tool::load_agents_dir::AgentDefinitionsResult>,
    main_thread_agent_definition:
        Option<Arc<crate::tools::agent_tool::load_agents_dir::AgentDefinition>>,
}

fn resolve_agent_launch(
    settings: &SettingsJson,
    cli: &crate::cli::CliConfig,
) -> ResolvedAgentLaunch {
    let cwd = bootstrap::state::get_original_cwd();
    let definitions = Arc::new(
        crate::tools::agent_tool::load_agents_dir::get_agent_definitions_with_overrides_readonly(
            &cwd,
        ),
    );
    let setting = cli.agent.as_deref().or(settings.agent.as_deref());
    let main_thread_agent_definition = setting.and_then(|setting| {
        let resolved = definitions
            .active_agents
            .iter()
            .find(|agent| agent.agent_type == setting)
            .cloned()
            .map(Arc::new);
        if resolved.is_none() {
            let available = definitions
                .active_agents
                .iter()
                .map(|agent| agent.agent_type.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            utils::debug::log_for_debugging(&format!(
                "Warning: agent \"{setting}\" not found. Available agents: {available}. Using default behavior."
            ));
        }
        resolved
    });

    ResolvedAgentLaunch {
        definitions,
        main_thread_agent_definition,
    }
}

/// Maps to: CC `main.tsx:3515-3541` startup resolution of the mutable
/// `thinkingEnabled` state and immutable `thinkingConfig` REPL prop.
///
/// Step 1 uses [`should_enable_thinking_by_default`] (`utils/thinking.ts`).
/// Step 2 applies `--thinking` or deprecated max-token budget overrides.
fn resolve_thinking_launch(
    settings: &SettingsJson,
    cli: &crate::cli::CliConfig,
) -> ResolvedThinkingLaunch {
    // Maps to: CC `let thinkingEnabled = shouldEnableThinkingByDefault()`
    // then `thinkingConfig = thinkingEnabled !== false ? adaptive : disabled`.
    let default_enabled = should_enable_thinking_by_default(settings);
    let mut resolved = ResolvedThinkingLaunch {
        enabled: default_enabled,
        config: if default_enabled {
            ThinkingConfig::Adaptive
        } else {
            ThinkingConfig::Disabled
        },
    };

    match cli.thinking.as_deref() {
        Some("adaptive" | "enabled") => {
            resolved.enabled = true;
            resolved.config = ThinkingConfig::Adaptive;
        }
        Some("disabled") => {
            resolved.enabled = false;
            resolved.config = ThinkingConfig::Disabled;
        }
        _ => {
            // Maps to: CC MAX_THINKING_TOKENS env (truthy) ?? options.maxThinkingTokens.
            // Invalid non-empty env parses to NaN and blocks CLI fallback.
            let env_max = std::env::var("MAX_THINKING_TOKENS")
                .ok()
                .filter(|value| !value.is_empty());
            let max_thinking_tokens = match env_max.as_deref() {
                Some(value) => parse_js_decimal_i64(value),
                None => cli
                    .max_thinking_tokens
                    .as_deref()
                    .and_then(parse_js_decimal_i64),
            };
            if let Some(tokens) = max_thinking_tokens {
                if tokens > 0 {
                    resolved.enabled = true;
                    resolved.config = ThinkingConfig::Enabled {
                        budget_tokens: Some(tokens),
                    };
                } else if tokens == 0 {
                    resolved.enabled = false;
                    resolved.config = ThinkingConfig::Disabled;
                }
            }
            // When env is non-empty but parse fails (NaN), leave first-stage
            // result (enable already false from should_enable_thinking_by_default).
        }
    }

    resolved
}

/// Build CC `main.tsx`'s two launch outputs in one pass so the AppState enable
/// switch and required REPL config cannot be resolved from different snapshots.
fn build_interactive_launch(
    settings: &SettingsJson,
    settings_errors: &[ValidationError],
    workspace_trusted: bool,
    cli: &crate::cli::CliConfig,
) -> anyhow::Result<InteractiveLaunch> {
    build_interactive_launch_with_system_prompts(
        settings,
        settings_errors,
        workspace_trusted,
        cli,
        None,
        None,
    )
}

fn build_interactive_launch_with_system_prompts(
    settings: &SettingsJson,
    settings_errors: &[ValidationError],
    workspace_trusted: bool,
    cli: &crate::cli::CliConfig,
    system_prompt: Option<Arc<str>>,
    append_system_prompt: Option<Arc<str>>,
) -> anyhow::Result<InteractiveLaunch> {
    let thinking = resolve_thinking_launch(settings, cli);
    let agents = resolve_agent_launch(settings, cli);
    let initial_state = build_initial_app_state_with_thinking(
        settings,
        settings_errors,
        workspace_trusted,
        cli,
        thinking.enabled,
        &agents,
    )?;
    Ok(InteractiveLaunch {
        initial_state,
        repl_props: ReplProps {
            debug: cli.debug || cli.debug_to_stderr,
            disable_slash_commands: cli.disable_slash_commands,
            system_prompt,
            append_system_prompt,
            main_thread_agent_definition: agents.main_thread_agent_definition,
            thinking_config: thinking.config,
            ..ReplProps::default()
        },
    })
}

/// Assemble interactive `AppState` from merged settings (CC main.tsx ~4153).
///
/// `cli` applies Live commander overrides: `--model`, `--effort`, `--verbose`,
/// permission mode / tool allow-deny lists (CC `initialPermissionModeFromCLI` +
/// `initializeToolPermissionContext` subset).
pub fn build_initial_app_state(
    settings: &SettingsJson,
    settings_errors: &[ValidationError],
    workspace_trusted: bool,
    cli: &crate::cli::CliConfig,
) -> anyhow::Result<AppState> {
    Ok(build_interactive_launch(settings, settings_errors, workspace_trusted, cli)?.initial_state)
}

fn build_initial_app_state_with_thinking(
    settings: &SettingsJson,
    settings_errors: &[ValidationError],
    workspace_trusted: bool,
    cli: &crate::cli::CliConfig,
    thinking_enabled: bool,
    agents: &ResolvedAgentLaunch,
) -> anyhow::Result<AppState> {
    let mut initial = AppState::default();
    // Maps to: CC `main.tsx` initialState `thinkingEnabled`.
    initial.thinking_enabled = Some(thinking_enabled);
    // Maps to: CC AppStateStore.ts:469 `settings: getInitialSettings()`.
    initial.settings = std::sync::Arc::new(settings.clone());

    // Maps to: CC `main.tsx:2941,3052-3071`: only an explicit CLI model or
    // non-inherit main-thread agent model becomes the runtime override. Leaving
    // the override undefined is significant because ANTHROPIC_MODEL and saved
    // settings must remain eligible in `getUserSpecifiedModelSetting()`.
    let effective_model_override = match cli.model.as_deref() {
        Some("default") => Some(crate::utils::model::model::get_default_main_loop_model()),
        Some(model) => Some(model.to_string()),
        None => agents
            .main_thread_agent_definition
            .as_ref()
            .and_then(|agent| agent.model.as_deref())
            .filter(|model| *model != "inherit")
            .map(crate::utils::model::model::parse_user_specified_model),
    };
    crate::bootstrap::state::set_main_loop_model_override(effective_model_override.map(Some));
    // Maps to: CC `main.tsx:3068`
    // `setInitialMainLoopModel(getUserSpecifiedModelSetting() || null)` — `||`
    // rather than `??`, so an empty setting is captured as unset.
    crate::bootstrap::state::set_initial_main_loop_model(
        crate::utils::model::model::get_user_specified_model_setting()
            .filter(|model| !model.is_empty()),
    );
    initial.main_loop_model = crate::utils::model::model::get_user_specified_model_setting();
    // Maps to CC `main.tsx` initial AppState projection through
    // `getInitialAdvisorSetting()`. The feature gate intentionally keeps this
    // unset for ordinary external builds.
    initial.advisor_model = crate::utils::advisor::get_initial_advisor_setting();

    // Maps to CC main.tsx:4153 initialState assembly.
    initial.fast_mode = crate::utils::fast_mode::get_initial_fast_mode_setting(
        initial.main_loop_model.as_deref(),
        settings.fast_mode,
        settings.fast_mode_per_session_opt_in,
    );

    // Maps to: CC 2.1.198 `p4i` / `HQr`: CLI extended effort, then the
    // session-scoped ultracode seed, then the persisted effort level.
    initial.effort_value =
        crate::utils::ultracode::resolve_initial_effort_setting(cli.effort.as_deref(), settings);
    initial.ultracode = crate::utils::ultracode::consume_ultracode_setting(settings);

    let loaded_permission_rules =
        crate::utils::permissions::permissions_loader::load_all_permission_rules_from_disk();
    // CC main.tsx:2596-2604 awaits initialization before publishing AppState.
    // L1 PORTING.md Node→tokio A4: this pre-existing synchronous launch carrier
    // waits through the canonical bridge; directory validation itself stays
    // asynchronous. Only owned argument snapshots cross the bridge.
    let permission_settings = settings.clone();
    let allowed_tools = cli.allowed_tools.clone();
    let disallowed_tools = cli.disallowed_tools.clone();
    let base_tools = cli.tools.clone();
    let add_dirs = cli
        .add_dirs
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>();
    let init_result = crate::utils::process_runtime::block_on_from_sync(async move {
        crate::utils::permissions::permission_setup::initialize_tool_permission_context(
            &permission_settings,
            &loaded_permission_rules,
            &allowed_tools,
            &disallowed_tools,
            base_tools.as_deref(),
            &add_dirs,
        )
        .await
    })
    .ok_or_else(|| anyhow::anyhow!("failed to initialize permission startup runtime"))??;
    let mut permission_ctx = init_result.tool_permission_context;
    let (cli_mode, permission_notification) =
        crate::utils::permissions::permission_setup::initial_permission_mode_from_cli(
            cli.permission_mode.as_deref(),
            cli.dangerously_skip_permissions,
        );

    // Maps to: CC main.tsx `setAutoModeFlagCli(true)` intent signal.
    if crate::utils::permissions::permission_setup::is_transcript_classifier_feature_enabled()
        && (cli.permission_mode.as_deref() == Some("auto")
            || cli_mode == crate::types::permissions::PermissionMode::Auto
            || (cli.permission_mode.is_none()
                && crate::utils::permissions::permission_setup::is_default_permission_mode_auto(
                    settings,
                )))
    {
        crate::utils::permissions::auto_mode_state::set_auto_mode_flag_cli(true);
    }

    // Apply resolved mode with transition side effects (CC transitionPermissionMode
    // then assign mode). Bare `mode = cli_mode` skips setAutoModeActive / strip /
    // restore when CLI overrides settings.
    if cli_mode != permission_ctx.mode {
        use crate::utils::permissions::permission_mode::permission_mode_internal_name;
        use crate::utils::permissions::permission_setup::{
            is_auto_mode_gate_enabled, transition_permission_mode,
        };
        let from = permission_mode_internal_name(permission_ctx.mode);
        let to = permission_mode_internal_name(cli_mode);
        let mut next = transition_permission_mode(from, to, &permission_ctx);
        if cli_mode == crate::types::permissions::PermissionMode::Auto
            && !is_auto_mode_gate_enabled()
        {
            next.mode = crate::types::permissions::PermissionMode::Default;
        } else {
            next.mode = cli_mode;
        }
        permission_ctx = next;
    } else if cli_mode == crate::types::permissions::PermissionMode::Auto {
        // Settings already Auto — ensure active + strip after full rule load.
        if crate::utils::permissions::permission_setup::is_auto_mode_gate_enabled() {
            crate::utils::permissions::auto_mode_state::set_auto_mode_active(true);
            permission_ctx = crate::utils::permissions::permission_setup::strip_dangerous_permissions_for_auto_mode(
                &permission_ctx,
            );
        } else {
            permission_ctx.mode = crate::types::permissions::PermissionMode::Default;
            crate::utils::permissions::auto_mode_state::set_auto_mode_active(false);
        }
    }

    if cli.dangerously_skip_permissions
        && !crate::utils::permissions::permission_setup::is_bypass_permissions_mode_disabled()
    {
        permission_ctx.is_bypass_permissions_mode_available = true;
    }
    // CC main.tsx:2630-2634: warnings are stderr output, not model context or
    // notifications. Fatal initialization errors returned above stop startup.
    for warning in init_result.warnings {
        eprintln!("{warning}");
    }
    initial.set_tool_permission_context(permission_ctx);

    // Maps to: CC main.tsx `verbose: verbose ?? getGlobalConfig().verbose ?? false`.
    let global_config = load_global_config();
    initial.verbose = if cli.verbose {
        true
    } else {
        global_config.verbose.unwrap_or(false)
    };
    // Cometix extension (no CC counterpart) — user-authorized L2 (v3 ruling,
    // 2026-08-01); follows the CC verbose pattern (AppStateStore.ts:91).
    // Unwraps mirror the authoritative factory defaults (both true); the merge
    // path fills disk-absent keys from the factory, so these are safety nets.
    initial.expand_thinking = global_config.expand_thinking.unwrap_or(true);
    initial.expand_collapsed_read_search =
        global_config.expand_collapsed_read_search.unwrap_or(true);
    // Maps to: CC main.tsx:4069-4070 resolved agent + definitions fields.
    initial.agent_definitions = agents.definitions.clone();
    initial.agent = agents
        .main_thread_agent_definition
        .as_ref()
        .map(|agent| agent.agent_type.clone());
    if let Some(agent_type) = initial.agent.as_deref() {
        // Maps to: CC `saveAgentSetting(mainThreadAgentDefinition.agentType)`.
        crate::utils::session_storage::save_agent_setting(agent_type);
    }
    // Maps to: CC getDefaultAppState `promptSuggestionEnabled` through
    // `shouldEnablePromptSuggestion()` (env > GrowthBook > mode/team > setting).
    initial.prompt_suggestion_enabled = crate::services::prompt_suggestion::prompt_suggestion::should_enable_prompt_suggestion_with_snapshots(
        settings.prompt_suggestion_enabled,
        crate::bootstrap::state::get_is_non_interactive_session(),
        crate::utils::agent_swarms_enabled::is_agent_swarms_enabled()
            && crate::utils::teammate::is_teammate(),
        std::env::var("CLAUDE_CODE_ENABLE_PROMPT_SUGGESTION")
            .ok()
            .as_deref(),
    );
    // Maps to: CC main.tsx initialState notifications seeding.
    let mut notifications = startup_notifications(settings, settings_errors);
    if let Some(notification) = status_line_trust_blocked_notification(settings, workspace_trusted)
    {
        notifications.queue.push(notification);
    }
    if let Some(text) = permission_notification {
        notifications
            .queue
            .push(crate::context::notifications::Notification::text(
                "cli-permission-mode",
                text,
                crate::context::notifications::NotificationPriority::High,
            ));
    }
    initial.notifications = std::sync::Arc::new(notifications);
    Ok(initial)
}

/// Strongly-typed startup projection of CC `dynamicMcpConfig` and
/// `strictMcpConfig`. Discovery remains asynchronous.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct McpStartupConfig {
    pub dynamic: indexmap::IndexMap<String, crate::services::mcp::types::ScopedMcpServerConfig>,
    pub strict: bool,
    pub bare: bool,
}

pub fn resolve_mcp_startup_config(
    cli: &crate::cli::CliConfig,
) -> Result<(McpStartupConfig, Vec<String>), String> {
    resolve_mcp_startup_config_with_enterprise(
        cli,
        crate::services::mcp::config::does_enterprise_mcp_config_exist_readonly(),
    )
}

fn resolve_mcp_startup_config_with_enterprise(
    cli: &crate::cli::CliConfig,
    enterprise_exists: bool,
) -> Result<(McpStartupConfig, Vec<String>), String> {
    let parsed = crate::services::mcp::config::parse_dynamic_mcp_configs_readonly(&cli.mcp_config);
    if !parsed.errors.is_empty() {
        return Err(format!(
            "Error: Invalid MCP configuration:\n{}",
            crate::services::mcp::config::format_dynamic_mcp_config_errors(&parsed.errors)
        ));
    }

    if parsed.servers.iter().any(|(name, config)| {
        config.transport != crate::services::mcp::types::Transport::Sdk
            && crate::services::mcp::normalization::normalize_name_for_mcp(name)
                == "claude-in-chrome"
    }) {
        return Err(
            "Error: Invalid MCP configuration: \"claude-in-chrome\" is a reserved MCP name."
                .to_string(),
        );
    }
    // The gated Computer-Use reserved name remains tied to its unported
    // CHICAGO_MCP startup branch; policy and enterprise boundaries are live.
    if enterprise_exists && cli.strict_mcp_config {
        return Err(
            "You cannot use --strict-mcp-config when an enterprise MCP config is present"
                .to_string(),
        );
    }
    if enterprise_exists
        && parsed
            .servers
            .values()
            .any(|config| config.transport != crate::services::mcp::types::Transport::Sdk)
    {
        return Err(
            "You cannot dynamically configure MCP servers when an enterprise MCP config is present"
                .to_string(),
        );
    }

    let warnings = if parsed.blocked.is_empty() {
        Vec::new()
    } else {
        vec![format!(
            "Warning: MCP {} blocked by enterprise policy: {}",
            if parsed.blocked.len() == 1 {
                "server"
            } else {
                "servers"
            },
            parsed.blocked.join(", ")
        )]
    };
    Ok((
        McpStartupConfig {
            dynamic: parsed.servers,
            strict: cli.strict_mcp_config,
            bare: cli.bare,
        },
        warnings,
    ))
}

fn local_mcp_configs_for_startup(
    config: &McpStartupConfig,
) -> indexmap::IndexMap<String, crate::services::mcp::types::ScopedMcpServerConfig> {
    if config.strict || config.bare {
        return config.dynamic.clone();
    }
    let global_config = load_global_config();
    let current_project_config = std::env::current_dir()
        .ok()
        .map(|cwd| project_config_for_cwd(&global_config, &cwd))
        .unwrap_or_default();
    let mut configs = all_configured_mcp_servers_readonly(&global_config, &current_project_config);
    // CLI entries have the highest settings precedence.
    configs.extend(config.dynamic.clone());
    configs
}

/// Seed MCP pending state + channel permission callbacks at mount (sync).
pub fn seed_mcp_and_channel_permissions(store: &AppStore, startup: &McpStartupConfig) {
    let configs = local_mcp_configs_for_startup(startup);
    // CC `useManageMCPConnections.ts:782-838` — merge-with-guard, not a
    // whole-state write. Stale clients need no cleanup here: the store starts
    // empty at launch, so this call can only add.
    let _stale = McpWriter::new(store.clone()).initialize_servers_as_pending(&configs);
    // B3 flip-audit: CC useManageMCPConnections.ts:188-198 — gate closed ⇒
    // no setAppState at all (:188 early return), and identity-unchanged ⇒
    // `return prev` (:190/:195, `===`). ChannelPermissionCallbacks'
    // PartialEq is Arc::ptr_eq, the exact `===` projection; None == None
    // covers the closed-gate case.
    store.set_state(|prev| {
        let seed =
            crate::services::mcp::use_manage_mcp_connections::channel_permission_callbacks_from_official_gates();
        if prev.channel_permission_callbacks == seed {
            return crate::state::store::UpdateDecision::Same(());
        }
        let mut next = (**prev).clone();
        next.channel_permission_callbacks = seed;
        crate::state::store::UpdateDecision::Replace {
            next: std::sync::Arc::new(next),
            result: (),
        }
    });
}

pub fn build_startup_diagnostics_snapshot(
    settings: &SettingsWithErrors,
) -> Arc<StartupDiagnosticsSnapshot> {
    let global_config = load_global_config();
    let current_project_config = std::env::current_dir()
        .ok()
        .map(|cwd| project_config_for_cwd(&global_config, &cwd))
        .unwrap_or_default();
    let mcp_clients = mcp_client_snapshots_from_config(&global_config, &current_project_config);
    Arc::new(StartupDiagnosticsSnapshot {
        settings_errors: settings.errors.clone(),
        installation_diagnostics: Vec::new(),
        doctor_diagnostics: Vec::new(),
        mcp_clients,
        ide_installation_status: None,
    })
}

pub fn pending_mcpjson_approval_servers(
    settings: &crate::utils::settings::SettingsJson,
) -> Vec<String> {
    let global_config = load_global_config();
    let cwd = std::env::current_dir().unwrap_or_default();
    let project_config = project_config_for_cwd(&global_config, &cwd);
    let project_servers =
        get_mcp_configs_by_scope_readonly(ConfigScope::Project, &global_config, &project_config)
            .servers;
    crate::services::mcp_server_approval::pending_project_mcp_server_names_from_configs(
        &project_servers,
        settings,
        is_setting_source_enabled(SettingSource::Project),
        has_skip_dangerous_mode_permission_prompt(),
        crate::bootstrap::state::get_is_non_interactive_session(),
    )
}

// `refresh_runtime_mcp_state_after_mcpjson_approval` deleted at P5 G10.
//
// It ran during the setup phase and wrote the main store, which is the last
// thing Contract B clause 4 requires to be silent before adoption. CC has no
// counterpart: `mcpServerApproval.tsx:30/38` mounts the dialog under its OWN
// `<AppStateProvider>` and `onDone` only resolves the promise, so the approval
// never touches the main store. The approved set reaches the session because
// the REPL-mounted `useManageMCPConnections` reads the configs after setup
// finishes.
//
// Cometix now matches that: the dialog writes local settings (and invalidates
// the settings cache — `mcp_server_approval.rs` writes the file directly, so
// the invalidation CC gets from `updateSettingsForSource` is explicit there),
// and `use_manage_mcp_connections` picks the result up at REPL mount.

/// Maps to CC `initializeLspServerManager()` after workspace trust.
pub fn initialize_lsp_if_trusted(workspace_trusted: bool) {
    if workspace_trusted {
        crate::services::lsp::manager::initialize_lsp_server_manager();
    }
}

/// Register MCP connection handlers (non-test). IDE `selection_changed`
/// delivery is REPL-owned (CC `useIdeSelection`): the REPL registers its
/// per-connection sink via `services::mcp::client::register_ide_selection_sink`.
/// Ownership ruling, P5 G10 (2026-08-04). Kept pre-mount, deliberately.
///
/// CC's counterpart is `onConnectionAttempt`
/// (`useManageMCPConnections.ts:310-322`), a `useCallback` inside the hook —
/// hook-scoped, created and discarded with the mount. Rust registers a
/// process-wide handler set instead, because the rmcp transport delivers
/// connection callbacks through a global sink rather than a closure the caller
/// threads in.
///
/// That difference used to carry a real hazard: a connection resolving before
/// `AppStateProvider` adopted the store would write through this handler while
/// the change pipeline was still unbound, i.e. a silent write where CC's
/// notifies. It no longer can. The connect effect now lives at the REPL mount
/// (`use_manage_mcp_connections`), so no connection is *initiated* before
/// adoption, and therefore no `onConnectionAttempt` can fire before it. The
/// registration being early is now inert — it installs a sink that nothing
/// reaches until the REPL is up.
///
/// What remains is shape, not behaviour: the handler set outlives any single
/// mount, so a REPL remount would not re-register (and does not need to — the
/// store handle it holds is the same adopted store). Moving registration into
/// the hook would match CC's scoping, but requires the global sink to become
/// per-registration first; that belongs with the MCP producer parity batch,
/// not here.
pub fn register_mcp_connection_handlers(store: &AppStore) {
    #[cfg(not(test))]
    {
        register_on_connection_attempt_handlers(OnConnectionAttemptHandlers::new(
            McpWriter::new(store.clone()),
            store.get().channel_permission_callbacks.clone(),
        ));
    }
    let _ = store;
}

/// Start the process-wide settings change detector.
///
/// Maps to: CC `main.tsx:689` `void settingsChangeDetector.initialize()` — a
/// launch-phase call, because the detector is a process-wide notifier. The
/// AppState side of it is the Provider-scoped subscription
/// (`AppState.tsx:104-110`), which keeps one fan-out with a single cache reset
/// per change.
///
/// Everything else this function used to spawn moved to its mount at P5 G10,
/// because each one is a mounted effect at the source and spawning it here
/// raced `AppStateProvider`'s `bind_on_change` on a multi-threaded runtime:
/// - MCP connect -> `use_manage_mcp_connections`, called from `Repl`
///   (CC `<MCPConnectionManager>` in `REPL.tsx:6134-6138`).
/// - LSP notifications -> `use_lsp_initialization_notification`, called from
///   `Repl` (CC calls `useLspInitializationNotification()` there).
/// - Statusline -> `components::status_line::use_status_line_update`
///   (CC `StatusLine.tsx` is render and update in one component). That one was
///   also a functional fix: computing it once from a launch snapshot left the
///   status line stale for the whole session.
pub fn start_settings_change_detector() {
    #[cfg(not(test))]
    crate::utils::settings::change_detector::initialize();
}

/// Maps to: CC `main.tsx` `TeammateOptions`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TeammateOptions {
    pub agent_id: Option<String>,
    pub agent_name: Option<String>,
    pub team_name: Option<String>,
    pub agent_color: Option<String>,
    pub plan_mode_required: Option<bool>,
    pub parent_session_id: Option<String>,
    pub teammate_mode: Option<utils::swarm::backends::teammate_mode_snapshot::TeammateMode>,
    pub agent_type: Option<String>,
}

/// Maps to: CC `main.tsx#extractTeammateOptions`.
pub fn extract_teammate_options(argv: &[String]) -> Result<TeammateOptions, String> {
    let teammate_mode =
        match utils::cli_args::eager_parse_cli_flag("--teammate-mode", argv.iter().cloned())
            .as_deref()
        {
            Some("auto") => {
                Some(utils::swarm::backends::teammate_mode_snapshot::TeammateMode::Auto)
            }
            Some("tmux") => {
                Some(utils::swarm::backends::teammate_mode_snapshot::TeammateMode::Tmux)
            }
            Some("in-process") => {
                Some(utils::swarm::backends::teammate_mode_snapshot::TeammateMode::InProcess)
            }
            Some(value) => {
                return Err(format!(
                    "Error: --teammate-mode must be one of auto, tmux, or in-process (got {value})"
                ));
            }
            None => None,
        };

    Ok(TeammateOptions {
        agent_id: utils::cli_args::eager_parse_cli_flag("--agent-id", argv.iter().cloned()),
        agent_name: utils::cli_args::eager_parse_cli_flag("--agent-name", argv.iter().cloned()),
        team_name: utils::cli_args::eager_parse_cli_flag("--team-name", argv.iter().cloned()),
        agent_color: utils::cli_args::eager_parse_cli_flag("--agent-color", argv.iter().cloned()),
        plan_mode_required: argv
            .iter()
            .any(|arg| arg == "--plan-mode-required")
            .then_some(true),
        parent_session_id: utils::cli_args::eager_parse_cli_flag(
            "--parent-session-id",
            argv.iter().cloned(),
        ),
        teammate_mode,
        agent_type: utils::cli_args::eager_parse_cli_flag("--agent-type", argv.iter().cloned()),
    })
}

/// Maps to: CC `main.tsx` teammate identity setup before `setup()`.
pub fn configure_teammate_from_cli(argv: &[String]) -> Result<(), String> {
    if !utils::agent_swarms_enabled::is_agent_swarms_enabled() {
        return Ok(());
    }

    let teammate_opts = extract_teammate_options(argv)?;
    let has_any_identity = teammate_opts.agent_id.is_some()
        || teammate_opts.agent_name.is_some()
        || teammate_opts.team_name.is_some();
    let has_all_identity = teammate_opts.agent_id.is_some()
        && teammate_opts.agent_name.is_some()
        && teammate_opts.team_name.is_some();

    if has_any_identity && !has_all_identity {
        return Err(
            "Error: --agent-id, --agent-name, and --team-name must all be provided together"
                .to_string(),
        );
    }

    if has_all_identity {
        utils::teammate::set_dynamic_team_context(Some(utils::teammate::DynamicTeamContext {
            agent_id: teammate_opts.agent_id.expect("checked above"),
            agent_name: teammate_opts.agent_name.expect("checked above"),
            team_name: teammate_opts.team_name.expect("checked above"),
            color: teammate_opts.agent_color,
            plan_mode_required: teammate_opts.plan_mode_required.unwrap_or(false),
            parent_session_id: teammate_opts.parent_session_id,
        }));
    }

    if let Some(mode) = teammate_opts.teammate_mode {
        utils::swarm::backends::teammate_mode_snapshot::set_cli_teammate_mode_override(mode);
    }

    // Maps to: CC `main.tsx` `storedTeammateOpts?.agentType` prompt addendum seam.
    let _agent_type_for_future_system_prompt_addendum = teammate_opts.agent_type;

    Ok(())
}

/// Maps to CC `main.tsx#loadSettingsFromFlag`.
fn load_settings_from_flag(settings_value: &str) -> Result<(), String> {
    use sha2::Digest;

    let trimmed = settings_value.trim();
    let path = if trimmed.starts_with('{') && trimmed.ends_with('}') {
        serde_json::from_str::<serde_json::Value>(trimmed)
            .map_err(|_| "Error: Invalid JSON provided to --settings".to_string())?;
        let digest = sha2::Sha256::digest(trimmed.as_bytes());
        let hash = digest
            .iter()
            .take(16)
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let path = std::env::temp_dir().join(format!("claude-settings-{hash}.json"));
        std::fs::write(&path, trimmed)
            .map_err(|error| format!("Error processing --settings: {error}"))?;
        path
    } else {
        let path = std::path::PathBuf::from(trimmed);
        let resolved = if path.is_absolute() {
            path
        } else {
            std::env::current_dir().unwrap_or_default().join(path)
        };
        match std::fs::read_to_string(&resolved) {
            Ok(_) => resolved.canonicalize().unwrap_or(resolved),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(format!(
                    "Error: Settings file not found: {}",
                    resolved.display()
                ));
            }
            Err(error) => return Err(format!("Error processing --settings: {error}")),
        }
    };
    bootstrap::state::set_flag_settings_path(Some(path));
    Ok(())
}

/// Maps to CC `main.tsx#loadSettingSourcesFromFlag`.
fn load_setting_sources_from_flag(setting_sources_arg: &str) -> Result<(), String> {
    let sources = utils::settings::constants::parse_setting_sources_flag(setting_sources_arg)?
        .into_iter()
        .map(|source| match source {
            utils::settings::SettingSource::User => "userSettings",
            utils::settings::SettingSource::Project => "projectSettings",
            utils::settings::SettingSource::Local => "localSettings",
            _ => unreachable!("flag parser only returns editable sources"),
        })
        .map(str::to_string)
        .collect();
    bootstrap::state::set_allowed_setting_sources(sources);
    Ok(())
}

/// Maps to CC `main.tsx#eagerLoadSettings`.
fn eager_load_settings<I, S>(argv: I) -> Result<(), String>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let argv = argv.into_iter().map(Into::into).collect::<Vec<_>>();
    if let Some(settings_value) =
        utils::cli_args::eager_parse_cli_flag("--settings", argv.iter().cloned())
    {
        load_settings_from_flag(&settings_value)?;
    }
    if let Some(setting_sources_arg) =
        utils::cli_args::eager_parse_cli_flag("--setting-sources", argv.iter().cloned())
    {
        load_setting_sources_from_flag(&setting_sources_arg)?;
    }
    Ok(())
}

/// Rust startup adapter that applies argv-backed startup state shared by the
/// interactive and headless entry paths. Named settings behavior remains in
/// `eager_load_settings`; this adapter owns only cross-entrypoint sequencing.
pub fn apply_live_startup_flags(argv: &[String]) -> Result<(), String> {
    eager_load_settings(argv.iter().cloned())?;
    let add_dirs = utils::cli_args::eager_parse_cli_flag_values("--add-dir", argv.iter().cloned())
        .into_iter()
        .map(std::path::PathBuf::from)
        .collect::<Vec<_>>();
    bootstrap::state::set_additional_directories_for_claude_md(add_dirs);
    if let Some(agents_json) =
        utils::cli_args::eager_parse_cli_flag("--agents", argv.iter().cloned())
    {
        let parsed_agents = serde_json::from_str::<serde_json::Value>(&agents_json).ok();
        bootstrap::state::set_cli_agents_json(parsed_agents);
    }
    let inline_plugins =
        utils::cli_args::eager_parse_cli_flag_repeated("--plugin-dir", argv.iter().cloned())
            .into_iter()
            .map(std::path::PathBuf::from)
            .collect::<Vec<_>>();
    bootstrap::state::set_inline_plugins(inline_plugins);
    bootstrap::state::set_use_cowork_plugins(
        argv.iter()
            .any(|arg| arg == "--cowork" || arg == "--cowork=true"),
    );
    Ok(())
}

#[derive(Props)]
struct MainProps {
    app_store: AppStore,
    startup_settings: Arc<SettingsWithErrors>,
    /// Immutable output of the pre-render `showSetupScreens` probes.
    ///
    /// Maps to CC `main.tsx:3248-3260` plus Onboarding's mount-once
    /// `useState(() => isAnthropicAuthEnabled())`: startup/keychain IO is
    /// resolved before the retained root mounts, never from `Main::update`.
    setup_snapshot: Arc<SetupScreensSnapshot>,
    repl_props: ReplProps,
    session_launch: commands::resume::CliSessionLaunch,
    resume_filter_by_pr: Option<crate::screens::resume_conversation::ResumeFilterByPr>,
    mcp_startup: Arc<McpStartupConfig>,
    /// Pre-render keybinding snapshot/runtime. Loading and watcher setup happen
    /// before the retained root mounts.
    keybinding_runtime: crate::keybindings::keybinding_context::KeybindingRuntime,
    /// Rust process adapter for CC `exitWithError(..., exitCode: 1)`.
    exit_code: Arc<AtomicI32>,
}

impl Default for MainProps {
    fn default() -> Self {
        Self {
            app_store: AppStore::new(AppState::default(), None),
            startup_settings: Arc::new(SettingsWithErrors {
                settings: SettingsJson::default(),
                errors: Vec::new(),
                policy_settings: None,
            }),
            setup_snapshot: Arc::new(SetupScreensSnapshot::default()),
            repl_props: ReplProps::default(),
            session_launch: commands::resume::CliSessionLaunch::None,
            resume_filter_by_pr: None,
            mcp_startup: Arc::new(McpStartupConfig::default()),
            keybinding_runtime:
                crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings(),
            exit_code: Arc::new(AtomicI32::new(0)),
        }
    }
}

/// Retained-mode mapping of the awaited interactive tail in CC `main.tsx`:
/// `showSetupScreens → loadConversationForResume/processResumedConversation
/// → replLauncher.launchRepl` (or `dialogLaunchers.launchResumeChooser`).
#[component]
fn Main(props: &MainProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    // Maps to: CC ink/components/App.tsx:304-310 + termio/csi.ts:301.
    // Configure the framework root to push CSI >1u, not iocraft's generic
    // REPORT_EVENT_TYPES default: Kitty encodes both Esc press and release
    // as bare ESC under flag 2, so Crossterm sees two Press events per tap.
    hooks
        .use_context_mut::<SystemContext>()
        .set_keyboard_enhancement_flags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES);

    // The official setup/auth probes are startup work, not React render work.
    // `run()` resolves this immutable snapshot after the async keychain
    // prefetch barrier and before mounting the retained tree.
    let setup_snapshot = (*props.setup_snapshot).clone();
    let mut setup_complete = hooks.use_state(|| false);
    let mut plugin_hooks_loaded = hooks.use_state(|| false);
    let mut resolved_commands =
        hooks.use_state(|| Option::<Arc<Vec<crate::commands::Command>>>::None);
    let mut resolved_initial_tools =
        hooks.use_state(|| Option::<Arc<Vec<crate::types::tools::Tool>>>::None);
    let mut resume_started = hooks.use_state(|| false);
    let mut resume_caches_cleared = hooks.use_state(|| false);
    let mut resume_worktree_paths = hooks.use_state(|| Option::<Vec<String>>::None);
    let mut resume_picker_requested = hooks.use_state(|| false);
    let mut resumed_repl_props = hooks.use_state(|| Option::<ReplProps>::None);
    let mut resume_error = hooks.use_state(|| Option::<String>::None);
    // The source Ink clipboard imports the application process executor. The
    // iocraft replacement receives that dependency once for this retained root;
    // all child output handles share the same clipboard probe cache.
    let (clipboard_stdout, _) = hooks.use_output();
    let clipboard = hooks.use_const(move || {
        iocraft::Clipboard::new(Arc::new(
            crate::utils::exec_file_no_throw::ExecFileClipboardBackend,
        ))
        .with_output(clipboard_stdout)
    });
    let restore_channel = hooks.use_const(|| {
        Arc::new(async_channel::unbounded::<
            Result<Option<crate::utils::session_restore::ProcessedResume>, String>,
        >())
    });
    let restore_tx = restore_channel.0.clone();
    let restore_rx = restore_channel.1.clone();

    hooks.use_future({
        let app_store = props.app_store.clone();
        let base_repl_props = props.repl_props.clone();
        async move {
            while let Ok(restored) = restore_rx.recv().await {
                match restored {
                    Ok(Some(processed)) => {
                        app_store.replace_with(|state| processed.apply_to_app_state(state));
                        let mut repl_props = base_repl_props.clone();
                        repl_props.apply_processed_resume(&processed);
                        resumed_repl_props.set(Some(repl_props));
                    }
                    Ok(None) => resume_picker_requested.set(true),
                    Err(error) => resume_error.set(Some(error)),
                }
            }
        }
    });
    // Keep all iocraft hooks above retained phase branching. React has the same
    // rule; violating it caused the bare `-r` loading→selector panic.
    let keybinding_runtime = crate::keybindings::keybinding_provider_setup::use_keybinding_setup(
        &mut hooks,
        props.keybinding_runtime.clone(),
    );
    let current_theme = *crate::utils::theme::current();

    // Maps to: CC `main.tsx` awaiting `loadPluginHooks()` after setup and
    // before SessionStart/REPL launch. Registered hooks remain separate from
    // the settings hook snapshot.
    if setup_complete.get() && !plugin_hooks_loaded.get() {
        let policy =
            crate::utils::hooks::hooks_config_snapshot::load_hooks_config_from_settings_sources();
        if !policy.allow_managed_hooks_only && !policy.disable_all_hooks {
            if let Err(error) = crate::utils::plugins::load_plugin_hooks::load_plugin_hooks() {
                crate::utils::debug::log_for_debugging(&format!(
                    "Warning: Failed to load plugin hooks after setup: {error}"
                ));
            }
        }
        crate::utils::plugins::load_plugin_hooks::setup_plugin_hook_hot_reload();
        plugin_hooks_loaded.set(true);
    }

    // Maps to CC `getCommands(currentCwd)` after setup/plugin registration and
    // before `launchRepl(..., { commands })`.
    if setup_complete.get() && plugin_hooks_loaded.get() && resolved_commands.read().is_none() {
        let cwd = crate::bootstrap::state::get_original_cwd();
        // Interactive startup intentionally does not wait for MCP discovery.
        // `initialTools` stays the launcher-provided array (normally empty);
        // connection updates populate flat AppState.mcp.tools/commands later.
        resolved_initial_tools.set(Some(props.repl_props.initial_tools.clone()));
        resolved_commands.set(Some(Arc::new(crate::commands::get_commands(&cwd))));
    }

    if setup_complete.get()
        && !resume_caches_cleared.get()
        && !matches!(
            &props.session_launch,
            commands::resume::CliSessionLaunch::None
        )
    {
        commands::clear::caches::clear_session_caches(&std::collections::HashSet::new());
        resume_caches_cleared.set(true);
    }

    let phase = if !setup_complete.get() {
        // Contract B clause 4b: the setup phase runs before the provider mounts,
        // so it receives settings/MCP-writer as explicit inputs instead of
        // reading the store from an ambient context.
        show_setup_screens(
            setup_snapshot,
            (*props.mcp_startup).clone(),
            props.startup_settings.clone(),
            Some(McpWriter::new(props.app_store.clone())),
            {
                let store = props.app_store.clone();
                move |_| {
                    // Maps to CC `main.tsx:4050` — `initialState` (including
                    // `settings: getInitialSettings()`) is built AFTER
                    // `await showSetupScreens()` (:3250). Cometix's retained
                    // launch phase must build the store BEFORE setup (the
                    // setup screens need an MCP writer), so the source's
                    // post-setup settings read is replayed here, at the same
                    // point in the sequence. Without it the adopted root
                    // carries the pre-setup snapshot and every setup-screen
                    // write (onboarding theme, trust, MCP approval) is lost.
                    //
                    // The explicit cache reset is STRONGER than CC, which
                    // relies on its write path having invalidated the cache.
                    // Cometix's setup-screen write paths have not all been
                    // audited against `update_settings_for_source`, so the
                    // reset makes the reconciliation independent of that
                    // audit; narrowing it is follow-up work, not a behaviour
                    // difference (a reset can only cost one disk read).
                    crate::utils::settings::settings_cache::reset_settings_cache();
                    let fresh = Arc::new(crate::utils::settings::get_initial_settings());
                    store.replace_with(|state| state.settings = fresh);

                    // Maps to CC main.tsx background startup prefetches: quota
                    // traffic starts only after setup/trust and remains off the
                    // retained frame.
                    crate::services::claude_ai_limits::spawn_quota_status_preflight();
                    setup_complete.set(true)
                }
            },
        )
    } else if resolved_commands.read().is_none() || resolved_initial_tools.read().is_none() {
        element! {
            View(flex_direction: FlexDirection::Row) {
                Spinner
                Text(content: " Loading commands…".to_string(), wrap: TextWrap::NoWrap)
            }
        }
        .into_any()
    } else {
        let commands = resolved_commands
            .read()
            .clone()
            .expect("commands resolved before launcher phase");
        let initial_tools = resolved_initial_tools
            .read()
            .clone()
            .expect("initial tools resolved before launcher phase");
        let mut launch_repl_props = props.repl_props.clone();
        launch_repl_props.commands = commands.clone();
        launch_repl_props.initial_tools = initial_tools.clone();
        // CC `REPL.tsx:6136-6137` hands these to `<MCPConnectionManager>`; the
        // REPL owns the connection effect, so the startup config rides down as
        // a REPL prop rather than being captured by a pre-mount spawn.
        launch_repl_props.mcp_startup = Some(props.mcp_startup.clone());
        let should_open_picker = matches!(
            &props.session_launch,
            commands::resume::CliSessionLaunch::OpenPicker { .. }
        ) || (matches!(
            &props.session_launch,
            commands::resume::CliSessionLaunch::Resume { .. }
        ) && resume_picker_requested.get());

        if should_open_picker {
            let initial_search_query = match &props.session_launch {
                commands::resume::CliSessionLaunch::OpenPicker {
                    initial_search_query,
                } => initial_search_query.clone(),
                commands::resume::CliSessionLaunch::Resume { value } => Some(value.clone()),
                _ => None,
            };
            let cached_worktree_paths = { resume_worktree_paths.read().clone() };
            let worktree_paths = cached_worktree_paths.unwrap_or_else(|| {
                let cwd = crate::bootstrap::state::get_original_cwd();
                let paths = crate::utils::get_worktree_paths::get_worktree_paths(
                    &cwd.display().to_string(),
                );
                resume_worktree_paths.set(Some(paths.clone()));
                paths
            });
            crate::dialog_launchers::launch_resume_chooser(
                props.app_store.clone(),
                props.startup_settings.clone(),
                worktree_paths,
                initial_search_query,
                props.resume_filter_by_pr.clone(),
                launch_repl_props.clone(),
            )
        } else {
            match &props.session_launch {
                commands::resume::CliSessionLaunch::None => launch_repl(
                    props.app_store.clone(),
                    props.startup_settings.clone(),
                    launch_repl_props.clone(),
                ),
                commands::resume::CliSessionLaunch::Continue
                | commands::resume::CliSessionLaunch::Resume { .. } => {
                    if let Some(mut repl_props) = resumed_repl_props.read().clone() {
                        repl_props.commands = commands.clone();
                        repl_props.initial_tools = initial_tools.clone();
                        // CC main.tsx → launchRepl retains the same dynamic MCP
                        // launch props on resume as on a fresh session.
                        repl_props.mcp_startup = Some(props.mcp_startup.clone());
                        launch_repl(
                            props.app_store.clone(),
                            props.startup_settings.clone(),
                            repl_props,
                        )
                    } else if let Some(error) = resume_error.read().clone() {
                        // Maps to CC `exitWithError(root, message)`: paint the
                        // fatal message in this frame, then unmount. `run()`
                        // performs graceful persistence cleanup before exit(1).
                        exit_with_error(
                            &mut hooks.use_context_mut::<SystemContext>(),
                            &props.exit_code,
                            error,
                            current_theme.error,
                        )
                    } else {
                        if !resume_started.get() {
                            resume_started.set(true);
                            let launch = props.session_launch.clone();
                            let initial_agent =
                                props.repl_props.main_thread_agent_definition.clone();
                            let agent_definitions = props.app_store.get().agent_definitions.clone();
                            let restore_tx = restore_tx.clone();
                            std::thread::spawn(move || {
                                let restored = (|| {
                                    let project = crate::bootstrap::state::get_original_cwd()
                                        .display()
                                        .to_string();
                                    let target = match launch {
                                        commands::resume::CliSessionLaunch::Continue => {
                                            commands::resume::load_for_continue(&project).map_err(
                                                |error| match error {
                                                    commands::resume::ResumeError::NoConversations => {
                                                        "No conversation found to continue".to_string()
                                                    }
                                                    other => other.to_string(),
                                                },
                                            )?
                                        }
                                        commands::resume::CliSessionLaunch::Resume { value } => {
                                            let is_uuid = uuid::Uuid::parse_str(&value).is_ok();
                                            match commands::resume::load_for_cli_resume(
                                                &project, &value,
                                            ) {
                                                Ok(target) => target,
                                                Err(error)
                                                    if !is_uuid
                                                        && matches!(
                                                            &error,
                                                            commands::resume::ResumeError::NoConversations
                                                                | commands::resume::ResumeError::SessionNotFound { .. }
                                                                | commands::resume::ResumeError::MultipleMatches { .. }
                                                        ) =>
                                                {
                                                    return Ok(None);
                                                }
                                                Err(commands::resume::ResumeError::EmptySession {
                                                    session_id,
                                                }) => {
                                                    return Err(format!(
                                                        "No conversation found with session ID: {session_id}"
                                                    ));
                                                }
                                                Err(error)
                                                    if is_uuid
                                                        && matches!(
                                                            error,
                                                            commands::resume::ResumeError::NoConversations
                                                                | commands::resume::ResumeError::SessionNotFound { .. }
                                                        ) =>
                                                {
                                                    return Err(format!(
                                                        "No conversation found with session ID: {value}"
                                                    ));
                                                }
                                                Err(_) => {
                                                    return Err(format!(
                                                        "Failed to resume session {value}"
                                                    ));
                                                }
                                            }
                                        }
                                        _ => unreachable!(
                                            "only continue/direct resume reaches restore worker"
                                        ),
                                    };
                                    let loaded =
                                        utils::conversation_recovery::load_conversation_for_resume(
                                            &target,
                                        )?;
                                    utils::session_restore::process_resumed_conversation(
                                        loaded,
                                        initial_agent,
                                        agent_definitions,
                                    )
                                    .map(Some)
                                })();
                                let _ = restore_tx.send_blocking(restored);
                            });
                        }
                        element! {
                            View(flex_direction: FlexDirection::Row) {
                                Spinner
                                Text(
                                    content: " Resuming conversation…".to_string(),
                                    wrap: TextWrap::NoWrap,
                                )
                            }
                        }
                        .into_any()
                    }
                }
                commands::resume::CliSessionLaunch::OpenPicker { .. } => {
                    unreachable!("picker launch handled above")
                }
            }
        }
    };

    // Contract B clause 2: no root-level raw `AppStore` context. The store
    // reaches the tree only through `components::app::App` →
    // `state::app_state::AppStateProvider`, which is its sole provider.
    // The startup settings snapshot likewise travels as a typed prop
    // (`SetupScreensHostProps` / `AppProps`), so no ambient
    // `Arc<SettingsWithErrors>` context is published either.
    element! {
        ContextProvider(value: Context::owned(keybinding_runtime)) {
            ContextProvider(value: Context::owned(current_theme)) {
                ContextProvider(value: Context::owned(clipboard.clone())) {
                    // Node process.exitCode representation: reuse the existing
                    // process-owned status consumed after the render loop.
                    ContextProvider(value: Context::owned(props.exit_code.clone())) {
                        #(phase)
                    }
                }
            }
        }
    }
}

/// Maps to: CC `main.tsx:1106-1111` — the raw-argv scan that decides the
/// session's interactivity before Commander has parsed anything.
///
/// ```text
/// const cliArgs = process.argv.slice(2)
/// const hasPrintFlag = cliArgs.includes('-p') || cliArgs.includes('--print')
/// const hasInitOnlyFlag = cliArgs.includes('--init-only')
/// const hasSdkUrl = cliArgs.some(arg => arg.startsWith('--sdk-url'))
/// const isNonInteractive =
///   hasPrintFlag || hasInitOnlyFlag || hasSdkUrl || !process.stdout.isTTY
/// ```
///
/// Raw argv on purpose, on both sides: the value is needed before the parser
/// runs (CC's comment at `:1104` — "set isInteractiveSession before init()
/// [because] telemetry initialization calls auth functions that need this
/// flag"). So `--sdk-url` needs no `CliConfig` field to be honoured here, and
/// `-p` counts wherever it appears — CC's `includes` does not know which
/// argument is an option and which is a value either.
///
/// `stdout_is_tty` is a parameter rather than a read of the real handle so a
/// test can drive this exact function; same shape as
/// `utils/terminal_notification.rs#terminal_progress_environment_snapshot_from_env`.
pub fn compute_is_non_interactive(cli_args: &[String], stdout_is_tty: bool) -> bool {
    let has_print_flag = cli_args.iter().any(|arg| arg == "-p" || arg == "--print");
    let has_init_only_flag = cli_args.iter().any(|arg| arg == "--init-only");
    let has_sdk_url = cli_args.iter().any(|arg| arg.starts_with("--sdk-url"));
    has_print_flag || has_init_only_flag || has_sdk_url || !stdout_is_tty
}

/// Maps to: CC `main.tsx:1113-1120` — the effect half of the same block.
///
/// CC's `stopCapturingEarlyInput()` (`:1114-1116`) has no counterpart: the port
/// has no `utils/earlyInput.ts` (no `seedEarlyInput`/capture buffer exists to
/// stop), so only the `setIsInteractive` line ports. `initializeEntrypoint`
/// (`:1123`) is likewise unported — nothing in the port writes
/// `CLAUDE_CODE_ENTRYPOINT`.
pub(crate) fn apply_is_interactive(cli_args: &[String], stdout_is_tty: bool) {
    let is_non_interactive = compute_is_non_interactive(cli_args, stdout_is_tty);
    let is_interactive = !is_non_interactive;
    crate::bootstrap::state::set_is_interactive(is_interactive);
}

/// Production entry for [`apply_is_interactive`], supplying the real stdout
/// handle for CC's `!process.stdout.isTTY` leg.
///
/// Called from `entrypoints/cli.rs#run` before [`crate::cli::parse_cli_config`]:
/// `cli/dispatch.rs#dispatch` routes `--print` into `cli/print.rs` and never
/// reaches [`run`], so anything downstream of the parser is too late for the
/// headless session this flag exists to describe.
pub fn initialize_is_interactive(cli_args: &[String]) {
    use std::io::IsTerminal;
    apply_is_interactive(cli_args, std::io::stdout().is_terminal());
}

/// Maps to: CC `main.tsx:2108-2170` — the print action's `systemPrompt` /
/// `appendSystemPrompt` resolution (`--system-prompt[-file]`,
/// `--append-system-prompt[-file]`), returned as the
/// `(customSystemPrompt, appendSystemPrompt)` pair `main.tsx` later hands
/// `cli/print.ts#ask` and, through it, `QueryEngine.ts:141-142`.
///
/// CC has no named function here — the block is inline in the commander
/// action — so this is the extraction. `--foo` and `--foo-file` together are
/// a parse-time error in `cli/parse.rs` (CC `:2111-2117` / `:2145-2151`), so
/// only the file read remains: ENOENT and other read failures both surface as
/// the returned error (CC `:2124-2138` writes them to stderr and exits 1).
///
/// `which` selects one half so the headless engine can resolve lazily, only
/// when an SDK `initialize` override (`print.ts:4369-4375`) has not already
/// supplied that half.
pub(crate) fn resolve_headless_system_prompt(
    config: &crate::cli::CliConfig,
    which: HeadlessSystemPromptSlot,
) -> Result<Option<String>, String> {
    let (inline, file) = match which {
        HeadlessSystemPromptSlot::Custom => (&config.system_prompt, &config.system_prompt_file),
        HeadlessSystemPromptSlot::Append => (
            &config.append_system_prompt,
            &config.append_system_prompt_file,
        ),
    };
    match (inline, file) {
        (Some(prompt), _) => Ok(Some(prompt.clone())),
        (_, Some(path)) => std::fs::read_to_string(path)
            .map(Some)
            .map_err(|error| format!("failed to read {}: {error}", path.display())),
        _ => Ok(None),
    }
}

/// The two halves of `main.tsx:2108-2170`: `systemPrompt` (`:2109-2140`) and
/// `appendSystemPrompt` (`:2143-2170`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HeadlessSystemPromptSlot {
    Custom,
    Append,
}

/// Maps to: CC `main.tsx:2755-2785` — the print action's `tools` value, i.e.
/// the `tools` argument `cli/print.ts#ask` receives and feeds `buildAllTools`
/// as `initialTools` (`print.ts:1481`).
///
/// ```text
/// let tools = getTools(toolPermissionContext)                        // :2755
/// if (feature('COORDINATOR_MODE') && isEnvTruthy(CLAUDE_CODE_COORDINATOR_MODE))
///   tools = applyCoordinatorToolFilter(tools)                        // :2757-2767
/// if (isSyntheticOutputToolEnabled({ isNonInteractiveSession }) && options.jsonSchema)
///   tools = [...tools, createSyntheticOutputTool(jsonSchema).tool]   // :2771-2785
/// ```
///
/// CC has no named function here either; the block is inline in the action.
/// `argv_json_schema` is the parsed `--json-schema` value (`:2776`). The
/// coordinator gate is `is_coordinator_mode()`, which folds the
/// build-audience feature check in. An invalid schema yields no tool and no
/// error (`:2796-2801`, telemetry-only branch), so `create_synthetic_output_tool`
/// failures are swallowed here as in CC.
pub(crate) fn build_headless_tools(
    permission_context: &crate::tool::ToolPermissionContext,
    argv_json_schema: Option<&serde_json::Value>,
) -> Vec<crate::types::tools::Tool> {
    let mut tools = crate::tools::get_tools(permission_context);
    if crate::coordinator::coordinator_mode::is_coordinator_mode() {
        tools = crate::utils::tool_pool::apply_coordinator_tool_filter(tools);
    }
    // `:619` `const isNonInteractiveSession = getIsNonInteractiveSession()`.
    let is_non_interactive_session = crate::bootstrap::state::get_is_non_interactive_session();
    let json_schema = argv_json_schema.filter(|_| {
        crate::tools::synthetic_output_tool::is_synthetic_output_tool_enabled(
            is_non_interactive_session,
        )
    });
    if let Some(schema) = json_schema {
        // `:2780-2785` — `'tool' in syntheticOutputResult` is the Ok arm.
        if let Ok(tool) =
            crate::tools::synthetic_output_tool::create_synthetic_output_tool(schema.clone())
        {
            tools.push(tool);
        }
    }
    tools
}

/// Maps to: CC `main.tsx#main` interactive path after cli.tsx dispatches here.
///
/// Mount shape: resolve launch inputs → `show_setup_screens` →
/// `repl_launcher::launch_repl` / `dialog_launchers::launch_resume_chooser`.
pub fn run(config: crate::cli::CliConfig) {
    let argv = config.argv.clone();
    utils::tls_provider::install_crypto_provider();

    utils::debug::init_from_parts(
        config.debug || config.mcp_debug,
        config.debug_to_stderr,
        config.debug_file.clone(),
        config.debug_filter.as_deref(),
    );
    utils::debug::announce_debug_startup_if_enabled();
    // Maps to CC `main.tsx:1-25`: launch both macOS keychain reads before
    // the remaining startup work so they run in parallel, then join before
    // settings/auth initialization below. This must never run from TUI update.
    utils::secure_storage::keychain_prefetch::start_keychain_prefetch();
    plugins::bundled::init_builtin_plugins();
    if let Err(error) = apply_live_startup_flags(&argv) {
        eprintln!("{error}");
        utils::cleanup_registry::exit_process(1);
    }
    // Maps to CC `main.tsx:1248-1258` preAction barrier. Any wait happens
    // before the render root exists; subsequent auth reads are memory hits.
    utils::secure_storage::keychain_prefetch::ensure_keychain_prefetch_completed();
    utils::managed_env::apply_safe_config_environment_variables();
    // Maps to CC main.tsx:1273-1274 preAction; setup.ts:371 uses the same
    // idempotent sink entry. Run before subcommands and MCP startup producers.
    if let Err(error) = utils::sinks::init_sinks() {
        eprintln!("{error}");
        utils::cleanup_registry::exit_process(1);
    }
    // Maps to CC main.tsx's fire-and-forget orphaned-plugin exclusion warm.
    // Bare mode skips plugin initialization entirely. DEVIATION(SCOPE): plugin
    // orphan GC belongs to the later Plugin batch; non-bare startup warms the
    // current disk state and retains Glob's lazy fallback.
    let is_bare = config.bare || utils::env_utils::is_bare_mode();
    if !is_bare {
        utils::plugins::orphaned_plugin_filter::warm_plugin_cache_exclusions_in_background();
    }
    if let Err(error) = configure_teammate_from_cli(&argv) {
        eprintln!("{error}");
        utils::cleanup_registry::exit_process(1);
    }

    let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
    // This is the port's stand-in for Node's single process event loop: the
    // one runtime that lives as long as the process. CC's `void promise(...)`
    // detached work (background agents) rides that loop and outlives whichever
    // turn started it; here it must ride THIS runtime, because
    // `query.rs#spawn_query` builds and drops a private per-turn runtime.
    utils::process_runtime::set_process_runtime_handle(rt.handle().clone());
    if let Some(exit_code) = commands::mcp::maybe_run_mcp_xaa_cli(&argv, &rt) {
        utils::cleanup_registry::exit_process(exit_code);
    }

    // Maps to: CC main.tsx `[STARTUP] Running setup()...`
    let setup_started = std::time::Instant::now();
    utils::debug::log_for_debugging("[STARTUP] Running setup()...");

    // Maps to: CC setup.ts:115-158, interactive-only interrupted Terminal.app
    // recovery before setup continues. This is the existing setup integration
    // boundary; backup policy remains in utils/appleTerminalBackup's owner.
    if !bootstrap::state::get_is_non_interactive_session() {
        use utils::apple_terminal_backup::RestoreResult;
        match rt.block_on(utils::apple_terminal_backup::check_and_restore_terminal_backup()) {
            Ok(RestoreResult::Restored) => println!("{}", chalk::Chalk::new().yellow().apply(
                "Detected an interrupted Terminal.app setup. Your original settings have been restored. You may need to restart Terminal.app for the changes to take effect.")),
            Ok(RestoreResult::Failed { backup_path }) => eprintln!("{}", chalk::Chalk::new().red().apply(&format!(
                "Failed to restore Terminal.app settings. Please manually restore your original settings with: defaults import com.apple.Terminal {backup_path}."))),
            Ok(RestoreResult::NoBackup) => {},
            Err(error) => utils::log::log_error(utils::log::LogError::new(error.to_string())),
        }
    }

    let startup_settings = Arc::new(get_settings_with_errors());
    let (mcp_startup, mcp_warnings) = match resolve_mcp_startup_config(&config) {
        Ok(result) => result,
        Err(error) => {
            eprintln!("{error}");
            utils::cleanup_registry::exit_process(1);
        }
    };
    for warning in mcp_warnings {
        eprintln!("{warning}");
    }
    let mcp_startup = Arc::new(mcp_startup);
    // Maps to CC `Project.shouldSkipPersistence()` cleanupPeriodDays=0 gate.
    bootstrap::state::set_session_persistence_disabled(
        config.session_persistence == Some(false)
            || startup_settings.settings.cleanup_period_days == Some(0),
    );
    let workspace_trusted = check_has_trust_dialog_accepted();

    let (system_prompt, append_system_prompt) =
        match utils::system_prompt::resolve_cli_system_prompts(
            config.system_prompt.as_deref(),
            config.system_prompt_file.as_deref(),
            config.append_system_prompt.as_deref(),
            config.append_system_prompt_file.as_deref(),
        ) {
            Ok((custom, append)) => (custom.map(Arc::<str>::from), append.map(Arc::<str>::from)),
            Err(error) => {
                eprintln!("{error}");
                utils::cleanup_registry::exit_process(1);
            }
        };

    let mut session_launch = commands::resume::resolve_cli_session_launch(
        config.continue_session,
        config.resume.as_deref(),
    );
    let resume_filter_by_pr = config
        .from_pr
        .as_deref()
        .map(crate::screens::resume_conversation::ResumeFilterByPr::from_cli_value);
    if resume_filter_by_pr.is_some()
        && matches!(&session_launch, commands::resume::CliSessionLaunch::None)
    {
        session_launch = commands::resume::CliSessionLaunch::OpenPicker {
            initial_search_query: None,
        };
    }

    // Maps to CC `showSetupScreens(...)` launch boundary and Onboarding's
    // mount-once auth initializer. Materialize once; `Main` only reads this
    // Arc and therefore performs no auth/config/process IO during a frame.
    // Maps to: CC main.tsx:3230-3233; recording starts before Ink mounts.
    {
        let _runtime = rt.enter();
        if let Err(error) = utils::asciicast::install_asciicast_recorder() {
            eprintln!("{error}");
            utils::cleanup_registry::exit_process(1);
        }
    }
    let setup_snapshot = Arc::new(default_setup_screens_snapshot());

    // Maps to CC `loadKeybindingsSyncWithWarnings()` before the interactive
    // provider mounts. Config reads never enter a retained frame.
    let startup_keybindings =
        crate::keybindings::load_user_bindings::load_keybindings_sync_with_warnings();
    let keybinding_runtime = crate::keybindings::keybinding_context::KeybindingRuntime::new(
        startup_keybindings.bindings.clone(),
    );

    let launch = match build_interactive_launch_with_system_prompts(
        &startup_settings.settings,
        &startup_settings.errors,
        workspace_trusted,
        &config,
        system_prompt,
        append_system_prompt,
    ) {
        Ok(launch) => launch,
        Err(error) => {
            eprintln!("{error}");
            utils::cleanup_registry::exit_process(1);
        }
    };
    // Maps to: CC `AppState.tsx:73-78` — `onChangeAppState` is installed by the
    // provider at mount, so every launch-phase write below stays silent
    // (Contract B clause 4). `AppStateProvider` calls `bind_on_change` when it
    // adopts this store.
    //
    // Pre-mount writer classification (Contract B clause 4, settled P5 G6).
    // All of the writers below run BEFORE adoption, so all of them are silent:
    // no `on_change`, and no listeners exist yet (the provider registers the
    // version counter at mount). Their state is therefore already in the root
    // the provider adopts — CC reaches the same first paint by running the
    // equivalent mount effects before its first commit.
    //
    //   * `seed_mcp_and_channel_permissions` / `register_mcp_connection_handlers`
    //     — CC `useManageMCPConnections.ts:178-198` is a REPL-mounted effect
    //     WITH unmount cleanup (:194-197 clears the callbacks). Kept
    //     pre-mount: the Rust connection manager is process-scoped and has no
    //     remount cycle to clean up for. SEAM: a future remount would not
    //     re-seed.
    //   * `claude_ai_limits::bind_app_store` — permanently exempt (approved
    //     L1 `Compile-time distribution capability projection`).
    //   * `sync_keybinding_warning_notification` + the watcher callback — CC
    //     `KeybindingProviderSetup.tsx:68-104` runs inside the provider
    //     (`REPL.tsx:182`). Kept here because the Rust keybinding runtime is
    //     mounted by the retained root ABOVE the provider; the notification it
    //     writes lands in the adopted root either way. SEAM: ownership差,
    //     no observable difference at first paint.
    //   * settings watch — DELETED at G5; ownership moved to the provider
    //     subscription (`AppState.tsx:104-110`) with the detector as the
    //     process-wide notifier (`main.tsx:689`).
    //   * startup effects (MCP connect / LSP notification /
    //     statusline) — OPEN RACE, corrected 2026-08-03 (P5 re-review). This
    //     comment previously claimed the writes "land AFTER adoption and
    //     therefore DO notify". Nothing enforces that: the tasks are spawned
    //     before `render_loop()` onto a MULTI-THREADED runtime
    //     (`Runtime::new()`, :1361), so they can genuinely run on another
    //     worker before the Provider's `use_state` initializer binds the
    //     pipeline. A write that lands first is silent where CC's would
    //     notify. `bind_on_change` entering the store turn removes the
    //     torn-adoption half of the hazard (a bind can no longer land in the
    //     middle of an in-flight write) but not the ordering half.
    //     Correct fix, deferred to P5 G10: these are MOUNTED effects at the
    //     source — `useManageMCPConnections.ts` is a REPL effect (`REPL.tsx`)
    //     — so they belong inside the component tree, where "after mount" is
    //     structural rather than asserted. Adding a synchronization primitive
    //     here would encode the wrong shape.
    //   * resume restore, synchronous path — applied to
    //     `launch.initial_state` before this construction (CC merges into
    //     `initialState` the same way, `sessionRestore.ts:534-550`), so it is
    //     not a store write at all.
    //   * resume restore, async path (`Main`'s restore `use_future`) — a real
    //     store write that lands after `Main` mounts, i.e. notified once the
    //     provider has adopted. CC has no counterpart write (it resolves
    //     resume before mounting), so this stays a documented seam until the
    //     pre-provider adoption work lands.
    let store = AppStore::new(launch.initial_state, None);
    let repl_props = launch.repl_props;
    initialize_lsp_if_trusted(workspace_trusted);
    seed_mcp_and_channel_permissions(&store, &mcp_startup);
    register_mcp_connection_handlers(&store);
    // L1 equivalent of `useClaudeAiLimits()` + the warning notification hook:
    // one launch-owned listener projects the service snapshot into AppState and
    // is dropped after the retained tree unmounts.
    let _claude_ai_limits_subscription =
        crate::services::claude_ai_limits::bind_app_store(store.clone());
    crate::keybindings::keybinding_provider_setup::sync_keybinding_warning_notification(
        &store,
        &startup_keybindings.warnings,
    );
    // Maps to CC `initializeKeybindingWatcher()` + subscription. The watcher
    // and all reload I/O live outside the retained tree.
    let _keybinding_watcher = {
        let runtime = keybinding_runtime.clone();
        let store = store.clone();
        crate::keybindings::load_user_bindings::initialize_keybinding_watcher(move |result| {
            runtime.replace_bindings(result.bindings);
            crate::keybindings::keybinding_provider_setup::sync_keybinding_warning_notification(
                &store,
                &result.warnings,
            );
        })
    };

    utils::debug::log_for_debugging(&format!(
        "[STARTUP] setup() completed in {}ms",
        setup_started.elapsed().as_millis()
    ));

    let exit_code = Arc::new(AtomicI32::new(0));
    let mount = || {
        element! {
            Main(
                app_store: store.clone(),
                startup_settings: startup_settings.clone(),
                setup_snapshot: setup_snapshot.clone(),
                repl_props: repl_props.clone(),
                session_launch: session_launch.clone(),
                resume_filter_by_pr: resume_filter_by_pr.clone(),
                mcp_startup: mcp_startup.clone(),
                keybinding_runtime: keybinding_runtime.clone(),
                exit_code: exit_code.clone(),
            )
        }
        .into_any()
    };

    if utils::debug::frame_profile_enabled() || utils::debug::frame_timing_log_path().is_some() {
        let stats = Arc::new(Mutex::new(RenderFrameProfileStats::default()));
        let stats_for_callback = Arc::clone(&stats);
        let profile_to_stderr = utils::debug::frame_profile_enabled();
        // Maps to: CC `interactiveHelpers.tsx:427-450` — bench-only JSONL,
        // same record shape (CC field names; sync append so no frames are
        // dropped on abrupt exit) so one analysis script consumes both
        // sides. Fields CC has and iocraft does not (optimize, patches,
        // yogaVisited/CacheHits/Live) are omitted rather than faked;
        // canvasHeight/changedCells/layoutMeasures are the Rust extras.
        let mut timing_log = utils::debug::frame_timing_log_path().and_then(|path| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .ok()
        });
        let render_result = rt.block_on(async {
            start_settings_change_detector();
            let result = mount()
                .render_loop()
                .stdout(utils::asciicast::RecordingStdout(std::io::stdout()))
                .on_frame_profile(move |event| {
                    if profile_to_stderr {
                        eprintln!(
                            "cometix-frame duration={:?} update={:?} layout={:?} draw={:?} repaint_check={:?} write={:?} canvas={}x{} changed_cells={} diff_rows={} measures={} repaint={:?}",
                            event.duration,
                            event.phases.update,
                            event.phases.layout,
                            event.phases.draw,
                            event.phases.repaint_check,
                            event.phases.terminal_write,
                            event.phases.canvas_width,
                            event.phases.canvas_height,
                            event.phases.changed_cells,
                            event.phases.diff_rows_scanned,
                            event.phases.layout_measures,
                            event.repaint.as_ref().map(|repaint| repaint.reason),
                        );
                    }
                    if let Some(file) = timing_log.as_mut() {
                        use std::io::Write as _;
                        let millis = |duration: std::time::Duration| duration.as_secs_f64() * 1e3;
                        // CC gates the expensive rss/cpu samples behind
                        // CLAUDE_CODE_FRAME_TIMING_SAMPLE_EVERY; mirror it so
                        // long captures can trade sample density for overhead.
                        let sample = utils::debug::frame_timing_sample_tick();
                        let mut line = serde_json::json!({
                            // CC records `at: Date.now()`; frame cadence
                            // (inter-frame gaps) is read off this field.
                            "at": std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_millis() as u64)
                                .unwrap_or(0),
                            "total": millis(event.duration),
                            "commit": millis(event.phases.update),
                            "yoga": millis(event.phases.layout),
                            "renderer": millis(event.phases.draw),
                            "diff": millis(event.phases.repaint_check),
                            "cellScan": millis(event.phases.changed_cell_scan),
                            "write": millis(event.phases.terminal_write),
                            "taffyMeasured": event.phases.layout_measures,
                            "taffyLive": event.phases.layout_nodes,
                            "eventSnapshot": millis(event.phases.event_snapshot),
                            "canvasAlloc": millis(event.phases.canvas_alloc),
                            "canvasSwap": millis(event.phases.canvas_swap),
                            "syncWrap": millis(event.phases.sync_wrap),
                            "settleRounds": event.phases.settle_rounds,
                            "diffRows": event.phases.diff_rows_scanned,
                            "canvasWidth": event.phases.canvas_width,
                            "canvasHeight": event.phases.canvas_height,
                            "changedCells": event.phases.changed_cells,
                        });
                        if sample {
                            let (cpu_user, cpu_system) =
                                utils::debug::process_cpu_usage_micros();
                            line["rss"] = serde_json::json!(
                                crate::hooks::use_memory_usage::process_rss_bytes()
                            );
                            line["cpu"] = serde_json::json!(
                                {"user": cpu_user, "system": cpu_system}
                            );
                        }
                        let _ = writeln!(file, "{line}");
                    }
                    stats_for_callback.lock().unwrap().record(&event);
                })
                .await;
            if let Err(error) = crate::cost_tracker::save_current_session_costs() {
                utils::debug::log_for_debugging(&format!(
                    "Failed to save current session costs during shutdown: {error}"
                ));
            }
            if let Err(error) = utils::session_storage::flush_session_storage().await {
                utils::debug::log_for_debugging(&format!(
                    "Failed to flush session storage during shutdown: {error}"
                ));
            }
            if let Err(error) = utils::session_storage::re_append_session_metadata() {
                utils::debug::log_for_debugging(&format!(
                    "Failed to re-append session metadata during shutdown: {error}"
                ));
            }
            utils::swarm::team_helpers::cleanup_session_teams().await;
            result
        });
        utils::asciicast::dispose_asciicast_recorder();
        render_result.unwrap();
        let requested_exit_code = exit_code.load(Ordering::SeqCst);
        if requested_exit_code != 0 {
            utils::cleanup_registry::exit_process(requested_exit_code);
        }

        let stats = stats.lock().unwrap();
        eprintln!(
            "cometix-frame-summary frames={} repaint_ratio={:.2} avg={:?} avg_update={:?} avg_layout={:?} avg_draw={:?} avg_write={:?} max_changed_cells={} max_diff_rows={}",
            stats.frames,
            stats.repaint_ratio(),
            stats.average_duration(),
            stats.average_update(),
            stats.average_layout(),
            stats.average_draw(),
            stats.average_terminal_write(),
            stats.max_changed_cells,
            stats.max_diff_rows_scanned,
        );
    } else {
        let render_result = rt.block_on(async {
            start_settings_change_detector();
            let result = mount()
                .render_loop()
                .stdout(utils::asciicast::RecordingStdout(std::io::stdout()))
                .await;
            if let Err(error) = crate::cost_tracker::save_current_session_costs() {
                utils::debug::log_for_debugging(&format!(
                    "Failed to save current session costs during shutdown: {error}"
                ));
            }
            if let Err(error) = utils::session_storage::flush_session_storage().await {
                utils::debug::log_for_debugging(&format!(
                    "Failed to flush session storage during shutdown: {error}"
                ));
            }
            if let Err(error) = utils::session_storage::re_append_session_metadata() {
                utils::debug::log_for_debugging(&format!(
                    "Failed to re-append session metadata during shutdown: {error}"
                ));
            }
            utils::swarm::team_helpers::cleanup_session_teams().await;
            result
        });
        utils::asciicast::dispose_asciicast_recorder();
        render_result.unwrap();
        let requested_exit_code = exit_code.load(Ordering::SeqCst);
        if requested_exit_code != 0 {
            utils::cleanup_registry::exit_process(requested_exit_code);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{StreamExt, stream};
    use std::fs;
    use std::time::Duration;

    fn argv(args: &[&str]) -> Vec<String> {
        std::iter::once("cometix".to_string())
            .chain(args.iter().map(|arg| arg.to_string()))
            .collect()
    }

    /// Moves the process into `root` for one `Main` test and moves it back
    /// unconditionally.
    ///
    /// The cwd and `original_cwd` are process state, not test state: leaving
    /// either pointed at a deleted temp directory makes every later test that
    /// resolves a relative path or reads `projectSettings` read the wrong root.
    /// A plain restore at the end of the body only runs when the body reaches
    /// it, so a single failing assertion here used to fail the rest of the run.
    struct MainProcessStateGuard {
        previous_cwd: std::path::PathBuf,
        previous_original_cwd: std::path::PathBuf,
        previous_env: Vec<crate::utils::env_utils::EnvVarGuard>,
        root: std::path::PathBuf,
    }

    impl MainProcessStateGuard {
        fn enter(root: &std::path::Path, env_keys: &[&'static str]) -> Self {
            let guard = Self {
                previous_cwd: std::env::current_dir().unwrap(),
                previous_original_cwd: crate::bootstrap::state::get_original_cwd(),
                previous_env: env_keys
                    .iter()
                    .map(|key| crate::utils::env_utils::EnvVarGuard::preserve(key))
                    .collect(),
                root: root.to_path_buf(),
            };
            std::env::set_current_dir(root).unwrap();
            crate::bootstrap::state::set_original_cwd(root);
            guard
        }
    }

    impl Drop for MainProcessStateGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.previous_cwd);
            crate::bootstrap::state::set_original_cwd(&self.previous_original_cwd);
            self.previous_env.clear();
            utils::config::set_test_global_config(None);
            crate::utils::hooks::hooks_config_snapshot::reset_hooks_config_snapshot();
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn append_hook_marker_command(path: &std::path::Path) -> String {
        #[cfg(windows)]
        {
            format!("echo start>>\"{}\"", path.display())
        }
        #[cfg(not(windows))]
        {
            let path = path.display().to_string().replace('\'', "'\\''");
            format!("printf '%s\\n' 'start' >> '{path}'")
        }
    }

    #[test]
    fn live_setting_sources_flag_updates_the_allowed_source_boundary() {
        struct LiveFlagStateRestore {
            allowed_sources: Vec<String>,
            additional_dirs: Vec<std::path::PathBuf>,
            cli_agents: Option<serde_json::Value>,
            inline_plugins: Vec<std::path::PathBuf>,
            use_cowork_plugins: bool,
        }

        impl LiveFlagStateRestore {
            fn capture() -> Self {
                Self {
                    allowed_sources: bootstrap::state::get_allowed_setting_sources(),
                    additional_dirs: bootstrap::state::get_additional_directories_for_claude_md(),
                    cli_agents: bootstrap::state::get_cli_agents_json(),
                    inline_plugins: bootstrap::state::get_inline_plugins(),
                    use_cowork_plugins: bootstrap::state::get_use_cowork_plugins(),
                }
            }
        }

        impl Drop for LiveFlagStateRestore {
            fn drop(&mut self) {
                bootstrap::state::set_allowed_setting_sources(self.allowed_sources.clone());
                bootstrap::state::set_additional_directories_for_claude_md(
                    self.additional_dirs.clone(),
                );
                bootstrap::state::set_cli_agents_json(self.cli_agents.clone());
                bootstrap::state::set_inline_plugins(self.inline_plugins.clone());
                bootstrap::state::set_use_cowork_plugins(self.use_cowork_plugins);
            }
        }

        let _lock = utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let _restore = LiveFlagStateRestore::capture();
        apply_live_startup_flags(&argv(&["--setting-sources", "user,local"])).unwrap();
        assert_eq!(
            bootstrap::state::get_allowed_setting_sources(),
            vec!["userSettings".to_string(), "localSettings".to_string()]
        );

        let error =
            apply_live_startup_flags(&argv(&["--setting-sources", "workspace"])).unwrap_err();
        assert_eq!(
            error,
            "Invalid setting source: workspace. Valid options are: user, project, local"
        );
        assert_eq!(
            bootstrap::state::get_allowed_setting_sources(),
            vec!["userSettings".to_string(), "localSettings".to_string()]
        );
    }

    #[test]
    fn dynamic_mcp_startup_enforces_official_enterprise_conflicts_and_sdk_exemption() {
        let non_sdk_json = serde_json::json!({
            "mcpServers": { "docs": { "command": "docs-server" } }
        })
        .to_string();
        let sdk_json = serde_json::json!({
            "mcpServers": { "sdk": { "type": "sdk", "name": "host" } }
        })
        .to_string();

        let reserved = crate::cli::CliConfig {
            mcp_config: vec![
                serde_json::json!({
                    "mcpServers": { "claude-in-chrome": { "command": "fake" } }
                })
                .to_string(),
            ],
            ..crate::cli::CliConfig::default()
        };
        assert!(
            resolve_mcp_startup_config_with_enterprise(&reserved, false)
                .unwrap_err()
                .contains("reserved MCP name")
        );

        let strict = crate::cli::CliConfig {
            mcp_config: vec![non_sdk_json.clone()],
            strict_mcp_config: true,
            ..crate::cli::CliConfig::default()
        };
        assert!(
            resolve_mcp_startup_config_with_enterprise(&strict, true)
                .unwrap_err()
                .contains("--strict-mcp-config")
        );

        let dynamic = crate::cli::CliConfig {
            mcp_config: vec![non_sdk_json],
            ..crate::cli::CliConfig::default()
        };
        assert!(
            resolve_mcp_startup_config_with_enterprise(&dynamic, true)
                .unwrap_err()
                .contains("cannot dynamically configure MCP servers")
        );

        let sdk = crate::cli::CliConfig {
            mcp_config: vec![sdk_json],
            ..crate::cli::CliConfig::default()
        };
        let (resolved, warnings) = resolve_mcp_startup_config_with_enterprise(&sdk, true).unwrap();
        assert!(warnings.is_empty());
        assert_eq!(
            resolved.dynamic["sdk"].transport,
            crate::services::mcp::types::Transport::Sdk
        );
    }

    #[test]
    fn strict_dynamic_mcp_startup_seeds_pending_clients_with_empty_flat_capabilities() {
        let config = crate::services::mcp::types::ScopedMcpServerConfig {
            name: None,
            scope: ConfigScope::Dynamic,
            transport: crate::services::mcp::types::Transport::Stdio,
            command: Some("docs-server".to_string()),
            args: Vec::new(),
            env: BTreeMap::new(),
            url: None,
            headers: BTreeMap::new(),
            headers_helper: None,
            oauth: None,
            ide_running_in_windows: None,
            ide_name: None,
            auth_token: None,
            id: None,
            plugin_source: None,
        };
        let startup = McpStartupConfig {
            dynamic: indexmap::IndexMap::from([("docs".to_string(), config)]),
            strict: true,
            bare: false,
        };
        let store = AppStore::new(AppState::default(), None);

        seed_mcp_and_channel_permissions(&store, &startup);

        let state = store.get();
        assert_eq!(state.mcp.clients.len(), 1);
        assert_eq!(state.mcp.clients[0].client.name, "docs");
        assert_eq!(
            state.mcp.clients[0].client.status,
            crate::services::mcp::types::McpServerConnectionType::Pending
        );
        assert!(state.mcp.tools.is_empty());
        assert!(state.mcp.commands.is_empty());
        assert!(state.mcp.resources.is_empty());
    }

    #[test]
    fn main_completes_setup_before_direct_resume_session_start_hooks() {
        // Mirror the process runtime published by the production entrypoint.
        crate::utils::process_runtime::initialize_test_process_runtime();
        let _env_guard = utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let cwd = std::env::temp_dir().join(format!(
            "cometix-main-resume-order-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&cwd).unwrap();
        let cwd = fs::canonicalize(&cwd).unwrap();
        let _process_state =
            MainProcessStateGuard::enter(&cwd, &["CLAUDE_CODE_SIMPLE", "CLAUBBIT"]);
        let _projects_guard =
            utils::session_storage::set_test_projects_dir_override(cwd.join("projects"));
        crate::utils::process_env::remove("CLAUDE_CODE_SIMPLE");
        crate::utils::process_env::set("CLAUBBIT", "1");
        utils::config::set_test_global_config(Some(utils::config::GlobalConfig::default()));
        utils::config::reset_trust_dialog_accepted_cache_for_testing();
        // This test is about ORDER — setup finishes before the resume
        // SessionStart hook fires — but the empty global config above leaves the
        // workspace untrusted, and CC skips ALL hooks in an untrusted
        // interactive session (`utils/hooks.ts:1994-1999`, ported into
        // `services::hooks::should_skip_hook_execution`). Without this the hook
        // never runs and the assertion below reads as an ordering bug.
        let _trust = crate::services::hooks::test_support::SessionTrustGuard::accepted();

        let marker_path = cwd.join("resume-hook-order.txt");
        let settings = SettingsJson {
            hooks: Some(serde_json::json!({
                "SessionStart": [{
                    "matcher": "resume",
                    "hooks": [{
                        "type": "command",
                        "command": append_hook_marker_command(&marker_path),
                        "timeout": 5
                    }]
                }]
            })),
            ..SettingsJson::default()
        };
        crate::utils::hooks::hooks_config_snapshot::reset_hooks_config_snapshot();
        crate::utils::hooks::hooks_config_snapshot::capture_hooks_config_snapshot(
            &settings, None, false,
        );
        let startup_settings = Arc::new(SettingsWithErrors {
            settings: settings.clone(),
            errors: Vec::new(),
            policy_settings: None,
        });
        let session_id = uuid::Uuid::new_v4().to_string();
        let session_file =
            utils::session_storage::get_session_file_path(&cwd.display().to_string(), &session_id);
        fs::create_dir_all(session_file.parent().unwrap()).unwrap();
        fs::write(
            &session_file,
            format!(
                "{}\n",
                serde_json::json!({
                    "type": "user",
                    "uuid": uuid::Uuid::new_v4().to_string(),
                    "parentUuid": null,
                    "sessionId": session_id,
                    "cwd": cwd.display().to_string(),
                    "timestamp": "2026-07-12T00:00:00.000Z",
                    "message": {"role": "user", "content": "resume after setup"}
                })
            ),
        )
        .unwrap();
        let snapshot = SetupScreensSnapshot {
            show_onboarding: true,
            oauth_enabled: false,
            api_key_needing_approval: None,
            offer_terminal_setup: false,
            theme_name: Some(utils::theme::ThemeName::Dark),
            terminal_name: Some("kitty".to_string()),
            show_claude_in_chrome_onboarding: false,
            claude_in_chrome_extension_installed: false,
        };
        let make_store = || {
            AppStore::new(
                build_initial_app_state(&settings, &[], true, &crate::cli::CliConfig::default())
                    .unwrap(),
                None,
            )
        };
        crate::interactive_helpers::reset_default_setup_snapshot_call_count();

        let mut before_setup = element! {
            Main(
                app_store: make_store(),
                startup_settings: startup_settings.clone(),
                setup_snapshot: Arc::new(snapshot.clone()),
                repl_props: ReplProps::default(),
                session_launch: commands::resume::CliSessionLaunch::Resume {
                    value: session_id.clone(),
                },
            )
        };
        let first = futures::executor::block_on(
            before_setup
                .mock_terminal_render_loop(
                    MockTerminalConfig::with_events(stream::empty()).with_size(120, 30),
                )
                .next(),
        )
        .expect("setup frame");
        assert!(first.to_string().contains("Welcome to Claude Code"));
        assert_eq!(
            crate::interactive_helpers::default_setup_snapshot_call_count(),
            0,
            "retained setup frame re-ran startup auth/config probes"
        );
        assert!(
            !marker_path.exists(),
            "resume hook ran before setup completed"
        );

        let mut after_setup = element! {
            Main(
                app_store: make_store(),
                startup_settings: startup_settings.clone(),
                setup_snapshot: Arc::new(snapshot),
                repl_props: ReplProps::default(),
                session_launch: commands::resume::CliSessionLaunch::Resume {
                    value: session_id.clone(),
                },
            )
        };
        let events = vec![
            TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Enter)),
            TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Enter)),
        ];
        let frames = futures::executor::block_on(async {
            // One Enter per onboarding step (theme, then security). They have
            // to land in separate render passes: delivered inside the same one,
            // the step that the first Enter advances to never mounts a handler
            // for the second, and setup stalls on the security screen.
            let events = stream::unfold(events.into_iter(), |mut events| async move {
                let event = events.next()?;
                futures_timer::Delay::new(Duration::from_millis(100)).await;
                Some((event, events))
            });
            let mut render_loop = Box::pin(after_setup.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(120, 30),
            ));
            let mut frames = Vec::new();
            for _ in 0..24 {
                if let Some(canvas) = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(500)).await;
                    None
                })
                .await
                {
                    frames.push(canvas.to_string());
                }
                if marker_path.exists() {
                    break;
                }
            }
            frames
        });
        assert!(
            marker_path.exists(),
            "resume hook did not run after setup; frames=\n{}",
            frames.join("\n--- frame ---\n")
        );
        assert_eq!(fs::read_to_string(&marker_path).unwrap().trim(), "start");
        assert_eq!(
            crate::interactive_helpers::default_setup_snapshot_call_count(),
            0,
            "setup completion/resume frames re-ran startup auth/config probes"
        );
    }

    #[test]
    fn direct_uuid_resume_failure_renders_exit_with_error_and_requests_exit_one() {
        let _env_guard = utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-main-fatal-resume-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&root).unwrap();
        let root = fs::canonicalize(root).unwrap();
        let _process_state = MainProcessStateGuard::enter(&root, &["CLAUBBIT"]);
        crate::utils::process_env::set("CLAUBBIT", "1");
        utils::config::set_test_global_config(Some(utils::config::GlobalConfig::default()));
        utils::config::reset_trust_dialog_accepted_cache_for_testing();
        let _projects_guard =
            utils::session_storage::set_test_projects_dir_override(root.join("projects"));
        let startup_settings = Arc::new(SettingsWithErrors {
            settings: SettingsJson::default(),
            errors: Vec::new(),
            policy_settings: None,
        });
        let missing_session_id = uuid::Uuid::new_v4().to_string();
        let exit_code = Arc::new(AtomicI32::new(0));
        let mut app = element! {
            Main(
                app_store: AppStore::new(AppState::default(), None),
                startup_settings: startup_settings,
                setup_snapshot: Arc::new(SetupScreensSnapshot::default()),
                repl_props: ReplProps::default(),
                session_launch: commands::resume::CliSessionLaunch::Resume {
                    value: missing_session_id.clone(),
                },
                exit_code: exit_code.clone(),
            )
        };

        let frames = futures::executor::block_on(async {
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(stream::empty()).with_size(120, 20),
            ));
            let mut frames = Vec::new();
            for _ in 0..30 {
                match crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(150)).await;
                    None
                })
                .await
                {
                    Some(canvas) => frames.push(canvas.to_string()),
                    None if exit_code.load(Ordering::SeqCst) == 1 => break,
                    None => {}
                }
                if exit_code.load(Ordering::SeqCst) == 1
                    && frames.last().is_some_and(|frame| {
                        frame.contains("No conversation found with session ID:")
                    })
                {
                    break;
                }
            }
            frames
        });

        assert_eq!(
            exit_code.load(Ordering::SeqCst),
            1,
            "frames=\n{}",
            frames.join("\n--- frame ---\n")
        );
        assert!(
            frames.iter().any(|frame| frame.contains(&format!(
                "No conversation found with session ID: {missing_session_id}"
            ))),
            "frames=\n{}",
            frames.join("\n--- frame ---\n")
        );
    }

    #[test]
    fn non_unique_cli_resume_value_opens_filtered_picker_after_setup() {
        let _env_guard = utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-main-resume-search-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&root).unwrap();
        let root = fs::canonicalize(root).unwrap();
        let _process_state = MainProcessStateGuard::enter(&root, &["CLAUBBIT"]);
        crate::utils::process_env::set("CLAUBBIT", "1");
        utils::config::set_test_global_config(Some(utils::config::GlobalConfig::default()));
        utils::config::reset_trust_dialog_accepted_cache_for_testing();
        let _projects_guard =
            utils::session_storage::set_test_projects_dir_override(root.join("projects"));

        let session_id = uuid::Uuid::new_v4().to_string();
        let session_file =
            utils::session_storage::get_session_file_path(&root.display().to_string(), &session_id);
        fs::create_dir_all(session_file.parent().unwrap()).unwrap();
        fs::write(
            &session_file,
            format!(
                "{}\n",
                serde_json::json!({
                    "type": "user",
                    "uuid": uuid::Uuid::new_v4().to_string(),
                    "sessionId": session_id,
                    "cwd": root.display().to_string(),
                    "timestamp": "2026-07-12T00:00:00.000Z",
                    "message": {
                        "role": "user",
                        "content": "needle startup picker"
                    }
                })
            ),
        )
        .unwrap();

        let startup_settings = Arc::new(SettingsWithErrors {
            settings: SettingsJson::default(),
            errors: Vec::new(),
            policy_settings: None,
        });
        let store = AppStore::new(
            build_initial_app_state(
                &startup_settings.settings,
                &[],
                true,
                &crate::cli::CliConfig::default(),
            )
            .unwrap(),
            None,
        );
        let snapshot = SetupScreensSnapshot::default();
        let mut app = element! {
            Main(
                app_store: store,
                startup_settings: startup_settings,
                setup_snapshot: Arc::new(snapshot),
                repl_props: ReplProps::default(),
                session_launch: commands::resume::CliSessionLaunch::Resume {
                    value: "needle startup picker".to_string(),
                },
            )
        };
        let text = futures::executor::block_on(async {
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(stream::empty()).with_size(100, 30),
            ));
            let mut last = String::new();
            for _ in 0..30 {
                if let Some(canvas) = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(200)).await;
                    None
                })
                .await
                {
                    last = canvas.to_string();
                }
                if last.contains("Resume Session") && last.contains("needle startup picker") {
                    break;
                }
            }
            last
        });

        assert!(text.contains("Resume Session"), "canvas=\n{text}");
        assert!(text.contains("needle startup picker"), "canvas=\n{text}");
        assert!(
            !text.contains("No conversations found to resume"),
            "non-unique search must enter the chooser, not fail startup; canvas=\n{text}"
        );
    }

    #[test]
    fn extract_teammate_options_matches_official_hidden_cli_shape() {
        let options = extract_teammate_options(&argv(&[
            "--agent-id",
            "reviewer@alpha",
            "--agent-name=reviewer",
            "--team-name",
            "alpha",
            "--agent-color",
            "green",
            "--plan-mode-required",
            "--parent-session-id",
            "parent-session",
            "--teammate-mode",
            "tmux",
            "--agent-type",
            "general-purpose",
        ]))
        .unwrap();

        assert_eq!(options.agent_id.as_deref(), Some("reviewer@alpha"));
        assert_eq!(options.agent_name.as_deref(), Some("reviewer"));
        assert_eq!(options.team_name.as_deref(), Some("alpha"));
        assert_eq!(options.agent_color.as_deref(), Some("green"));
        assert_eq!(options.plan_mode_required, Some(true));
        assert_eq!(options.parent_session_id.as_deref(), Some("parent-session"));
        assert_eq!(
            options.teammate_mode,
            Some(utils::swarm::backends::teammate_mode_snapshot::TeammateMode::Tmux)
        );
        assert_eq!(options.agent_type.as_deref(), Some("general-purpose"));
    }

    #[test]
    fn configure_teammate_from_cli_sets_dynamic_context_and_mode_override_like_official() {
        let _teammate_lock = utils::teammate::TEST_TEAMMATE_CONTEXT_LOCK.lock().unwrap();
        let _backend_lock = utils::swarm::backends::registry::TEST_BACKEND_REGISTRY_LOCK
            .lock()
            .unwrap();
        let _env_lock = utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS", "1");
        utils::teammate::clear_dynamic_team_context();
        utils::swarm::backends::teammate_mode_snapshot::reset_teammate_mode_snapshot_for_test();

        configure_teammate_from_cli(&argv(&[
            "--agent-id",
            "reviewer@alpha",
            "--agent-name",
            "reviewer",
            "--team-name",
            "alpha",
            "--agent-color",
            "green",
            "--plan-mode-required",
            "--parent-session-id",
            "parent-session",
            "--teammate-mode",
            "in-process",
        ]))
        .unwrap();

        assert!(utils::teammate::is_teammate());
        assert_eq!(
            utils::teammate::get_agent_name().as_deref(),
            Some("reviewer")
        );
        assert_eq!(
            utils::teammate::get_team_name(None).as_deref(),
            Some("alpha")
        );
        assert_eq!(
            utils::teammate::get_teammate_color().as_deref(),
            Some("green")
        );
        assert!(utils::teammate::is_plan_mode_required());
        assert_eq!(
            utils::teammate::get_parent_session_id().as_deref(),
            Some("parent-session")
        );
        assert_eq!(
            utils::swarm::backends::teammate_mode_snapshot::get_cli_teammate_mode_override(),
            Some(utils::swarm::backends::teammate_mode_snapshot::TeammateMode::InProcess)
        );

        utils::teammate::clear_dynamic_team_context();
        utils::swarm::backends::teammate_mode_snapshot::reset_teammate_mode_snapshot_for_test();
        crate::utils::process_env::remove("CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS");
    }

    #[test]
    fn configure_teammate_from_cli_requires_identity_triplet_like_official() {
        let _env_lock = utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS", "1");
        let error = configure_teammate_from_cli(&argv(&[
            "--agent-id",
            "reviewer@alpha",
            "--team-name",
            "alpha",
        ]))
        .unwrap_err();

        assert!(error.contains("--agent-id, --agent-name, and --team-name"));
        crate::utils::process_env::remove("CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS");
    }

    #[test]
    fn build_initial_app_state_applies_cli_model_effort_verbose_and_permissions() {
        let cli = crate::cli::parse_cli_config(&argv(&[
            "--model",
            "sonnet",
            "--effort",
            "medium",
            "--verbose",
            "--permission-mode",
            "plan",
            "--allowedTools",
            "Read",
            "--disallowedTools",
            "Bash",
        ]));
        let settings = SettingsJson::default();
        let state = build_initial_app_state(&settings, &[], true, &cli).unwrap();

        assert_eq!(state.main_loop_model.as_deref(), Some("sonnet"));
        assert_eq!(
            state.effort_value,
            Some(crate::utils::effort::EffortValue::Named("medium".into()))
        );
        assert!(state.verbose);
        assert_eq!(
            state.tool_permission_context.mode,
            crate::types::permissions::PermissionMode::Plan
        );
        assert!(
            state
                .tool_permission_context
                .always_allow_rules
                .contains_key(&crate::types::permissions::PermissionRuleSource::CliArg)
        );
        assert!(
            state
                .tool_permission_context
                .always_deny_rules
                .contains_key(&crate::types::permissions::PermissionRuleSource::CliArg)
        );
    }

    #[test]
    fn initial_state_without_explicit_override_preserves_environment_model_precedence() {
        let _env_guard = utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let previous_override = crate::bootstrap::state::get_main_loop_model_override();
        let _model =
            crate::utils::env_utils::EnvVarGuard::set("ANTHROPIC_MODEL", "claude-opus-4-6");
        let settings = SettingsJson {
            model: Some("claude-haiku-4-5-20251001".to_string()),
            ..SettingsJson::default()
        };
        let agents = ResolvedAgentLaunch {
            definitions: Arc::new(
                crate::tools::agent_tool::load_agents_dir::AgentDefinitionsResult::default(),
            ),
            main_thread_agent_definition: None,
        };

        let state = build_initial_app_state_with_thinking(
            &settings,
            &[],
            true,
            &crate::cli::CliConfig::default(),
            false,
            &agents,
        )
        .unwrap();

        assert_eq!(state.main_loop_model.as_deref(), Some("claude-opus-4-6"));
        assert_eq!(
            crate::bootstrap::state::get_main_loop_model_override(),
            None,
            "saved settings must not be installed as a runtime override"
        );

        crate::bootstrap::state::set_main_loop_model_override(previous_override);
    }

    /// Maps to: CC `main.tsx:3068`
    /// `setInitialMainLoopModel(getUserSpecifiedModelSetting() || null)` — the
    /// launch-time capture `modelOptions.ts:490` falls back to once `/model`
    /// clears the live setting. `||` rather than `??`, so an empty setting is
    /// captured as unset.
    #[test]
    fn startup_captures_the_launch_model_setting_like_official() {
        let _env_guard = utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let previous_override = crate::bootstrap::state::get_main_loop_model_override();
        let previous_initial = crate::bootstrap::state::get_initial_main_loop_model();
        let root = std::env::temp_dir().join(format!(
            "cometix-initial-model-{}",
            uuid::Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("settings.json"), "{}").unwrap();
        let _config = crate::utils::env_utils::EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root);
        let _model = crate::utils::env_utils::EnvVarGuard::unset("ANTHROPIC_MODEL");
        utils::settings::settings_cache::reset_settings_cache();

        let agents = ResolvedAgentLaunch {
            definitions: Arc::new(
                crate::tools::agent_tool::load_agents_dir::AgentDefinitionsResult::default(),
            ),
            main_thread_agent_definition: None,
        };
        let launch = |cli: &crate::cli::CliConfig| {
            crate::bootstrap::state::set_initial_main_loop_model(Some("stale".to_string()));
            build_initial_app_state_with_thinking(
                &SettingsJson::default(),
                &[],
                true,
                cli,
                false,
                &agents,
            )
            .unwrap();
            crate::bootstrap::state::get_initial_main_loop_model()
        };

        let pinned = launch(&crate::cli::parse_cli_config(&argv(&[
            "--model",
            "claude-sonnet-4-5-20250929",
        ])));
        assert_eq!(pinned.as_deref(), Some("claude-sonnet-4-5-20250929"));
        assert_eq!(
            launch(&crate::cli::parse_cli_config(&argv(&["--model", ""]))),
            None
        );
        assert_eq!(launch(&crate::cli::CliConfig::default()), None);

        crate::bootstrap::state::set_main_loop_model_override(previous_override);
        crate::bootstrap::state::set_initial_main_loop_model(previous_initial);
        drop((_config, _model));
        utils::settings::settings_cache::reset_settings_cache();
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn initial_state_uses_resolved_main_thread_agent_object_and_model_like_official() {
        let mut agent = crate::tools::agent_tool::load_agents_dir::AgentDefinition::new(
            "reviewer",
            "reviews code",
            crate::tools::agent_tool::load_agents_dir::AgentDefinitionSource::ProjectSettings,
        );
        agent.model = Some("sonnet".to_string());
        let agent = Arc::new(agent);
        let agents = ResolvedAgentLaunch {
            definitions: Arc::new(
                crate::tools::agent_tool::load_agents_dir::AgentDefinitionsResult {
                    active_agents: vec![agent.as_ref().clone()],
                    all_agents: vec![agent.as_ref().clone()],
                    failed_files: Vec::new(),
                    allowed_agent_types: None,
                },
            ),
            main_thread_agent_definition: Some(agent.clone()),
        };

        let state = build_initial_app_state_with_thinking(
            &SettingsJson::default(),
            &[],
            true,
            &crate::cli::CliConfig::default(),
            true,
            &agents,
        )
        .unwrap();

        assert_eq!(state.agent.as_deref(), Some("reviewer"));
        assert_eq!(
            state.main_loop_model.as_deref(),
            Some(crate::utils::model::model::DEFAULT_SONNET_MODEL)
        );
        assert_eq!(state.agent_definitions.active_agents.len(), 1);
    }

    #[test]
    fn build_interactive_launch_matches_official_repl_launch_props() {
        let mut cli = crate::cli::CliConfig::default();
        cli.debug_to_stderr = true;
        cli.disable_slash_commands = true;
        let launch = build_interactive_launch_with_system_prompts(
            &SettingsJson::default(),
            &[],
            true,
            &cli,
            Some(Arc::<str>::from("custom system")),
            Some(Arc::<str>::from("append system")),
        )
        .unwrap();

        assert!(launch.repl_props.debug);
        assert!(launch.repl_props.disable_slash_commands);
        assert_eq!(
            launch.repl_props.system_prompt.as_deref(),
            Some("custom system")
        );
        assert_eq!(
            launch.repl_props.append_system_prompt.as_deref(),
            Some("append system")
        );
    }

    #[test]
    fn processed_resume_is_applied_to_app_state_and_repl_initial_props_before_mount() {
        let cli = crate::cli::CliConfig::default();
        let mut launch =
            build_interactive_launch(&SettingsJson::default(), &[], true, &cli).unwrap();
        let target = crate::commands::resume::ResumeTarget {
            session_id: "startup-resume".to_string(),
            project_path: Some("/tmp/project".to_string()),
            entries: vec![serde_json::json!({
                "type": "user",
                "uuid": "resume-user",
                "timestamp": "2026-07-12T00:00:00.000Z",
                "message": {"role": "user", "content": "startup restored"}
            })],
            turn_interruption_state: crate::utils::conversation::TurnInterruptionState::None,
            metadata: crate::commands::resume::ResumeMetadata {
                agent_name: Some("Restored".to_string()),
                agent_color: Some("blue".to_string()),
                ..crate::commands::resume::ResumeMetadata::default()
            },
            entrypoint: None,
        };
        let loaded = crate::utils::session_restore::ResumeLoadResult::try_from(&target)
            .expect("resume should load");
        let processed = crate::utils::session_restore::process_resumed_conversation(
            loaded,
            launch.repl_props.main_thread_agent_definition.clone(),
            launch.initial_state.agent_definitions.clone(),
        )
        .expect("resume should process");

        processed.apply_to_app_state(&mut launch.initial_state);
        launch.repl_props.apply_processed_resume(&processed);

        let initial_messages = launch
            .repl_props
            .initial_messages
            .as_ref()
            .expect("REPL initialMessages");
        assert!(Arc::ptr_eq(initial_messages, &processed.messages));
        assert_eq!(
            launch.repl_props.initial_agent_name.as_deref(),
            Some("Restored")
        );
        assert_eq!(
            launch
                .initial_state
                .standalone_agent_context
                .as_ref()
                .map(|context| (context.name.as_str(), context.color.as_deref())),
            Some(("Restored", Some("blue")))
        );
        assert_eq!(
            launch
                .repl_props
                .initial_resume_restore_stores
                .session_id
                .as_deref(),
            Some("startup-resume")
        );
    }

    #[test]
    fn build_interactive_launch_matches_official_default_thinking_setting() {
        let _lock = utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("MAX_THINKING_TOKENS");
        let cli = crate::cli::CliConfig::default();

        let enabled = build_interactive_launch(&SettingsJson::default(), &[], true, &cli).unwrap();
        assert_eq!(enabled.initial_state.thinking_enabled, Some(true));
        assert_eq!(enabled.repl_props.thinking_config, ThinkingConfig::Adaptive);

        let mut settings = SettingsJson::default();
        settings.always_thinking_enabled = Some(false);
        let disabled = build_interactive_launch(&settings, &[], true, &cli).unwrap();
        assert_eq!(disabled.initial_state.thinking_enabled, Some(false));
        assert_eq!(
            disabled.repl_props.thinking_config,
            ThinkingConfig::Disabled
        );
    }

    #[test]
    fn build_interactive_launch_matches_official_thinking_cli_precedence() {
        let _lock = utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("MAX_THINKING_TOKENS");
        let settings = SettingsJson::default();
        let cli = crate::cli::parse_cli_config(&argv(&["--max-thinking-tokens", "8000"]));
        let launch = build_interactive_launch(&settings, &[], true, &cli).unwrap();
        assert_eq!(launch.initial_state.thinking_enabled, Some(true));
        assert_eq!(
            launch.repl_props.thinking_config,
            ThinkingConfig::Enabled {
                budget_tokens: Some(8000),
            }
        );

        // `--thinking` wins over max-thinking-tokens (CC main.tsx).
        let cli = crate::cli::parse_cli_config(&argv(&[
            "--thinking",
            "disabled",
            "--max-thinking-tokens",
            "8000",
        ]));
        let launch = build_interactive_launch(&settings, &[], true, &cli).unwrap();
        assert_eq!(launch.initial_state.thinking_enabled, Some(false));
        assert_eq!(launch.repl_props.thinking_config, ThinkingConfig::Disabled);
    }

    #[test]
    fn build_interactive_launch_matches_official_env_budget_precedence() {
        let _lock = utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("MAX_THINKING_TOKENS", "4096tokens");
        let settings = SettingsJson::default();
        let cli = crate::cli::parse_cli_config(&argv(&["--max-thinking-tokens", "8000"]));
        let launch = build_interactive_launch(&settings, &[], true, &cli).unwrap();
        assert_eq!(launch.initial_state.thinking_enabled, Some(true));
        assert_eq!(
            launch.repl_props.thinking_config,
            ThinkingConfig::Enabled {
                budget_tokens: Some(4096),
            }
        );

        // A non-empty invalid env value is truthy in JS, parses to NaN, and
        // still prevents falling back to the CLI max-token value.
        crate::utils::process_env::set("MAX_THINKING_TOKENS", "invalid");
        let launch = build_interactive_launch(&settings, &[], true, &cli).unwrap();
        assert_eq!(launch.initial_state.thinking_enabled, Some(false));
        assert_eq!(launch.repl_props.thinking_config, ThinkingConfig::Disabled);
        crate::utils::process_env::remove("MAX_THINKING_TOKENS");
    }

    fn cli_args(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| (*arg).to_string()).collect()
    }

    /// Each of the four `main.tsx:1110-1111` disjuncts, alone.
    ///
    /// `stdout_is_tty: true` on the first three so the TTY leg cannot be what
    /// makes them pass — the whole expression is an `||`, and a test that let
    /// two legs fire at once would still be green with three of them deleted.
    #[test]
    fn is_non_interactive_follows_official_four_way_disjunction() {
        // hasPrintFlag — both spellings (`:1107`).
        assert!(compute_is_non_interactive(&cli_args(&["-p"]), true));
        assert!(compute_is_non_interactive(&cli_args(&["--print"]), true));
        assert!(compute_is_non_interactive(
            &cli_args(&["--print", "summarise this"]),
            true
        ));

        // hasInitOnlyFlag (`:1108`).
        assert!(compute_is_non_interactive(
            &cli_args(&["--init-only"]),
            true
        ));

        // hasSdkUrl (`:1109`) — `startsWith`, so the `=` form counts and there
        // is no `CliConfig` field behind it; the port parses no `--sdk-url`.
        assert!(compute_is_non_interactive(&cli_args(&["--sdk-url"]), true));
        assert!(compute_is_non_interactive(
            &cli_args(&["--sdk-url=ws://localhost:1234"]),
            true
        ));
        assert!(compute_is_non_interactive(
            &cli_args(&["--sdk-url", "ws://localhost:1234"]),
            true
        ));

        // !process.stdout.isTTY (`:1111`) — alone, with an empty argv.
        assert!(compute_is_non_interactive(&[], false));

        // All four off: the interactive session.
        assert!(!compute_is_non_interactive(&[], true));
        assert!(!compute_is_non_interactive(
            &cli_args(&["--model", "sonnet", "--verbose"]),
            true
        ));
    }

    /// The near-misses. `includes`/`startsWith` are exact on the first three
    /// legs, so a prefix match there would make `--print-mode` or `--pretty`
    /// headless, and `-p` must not be found inside a longer token.
    #[test]
    fn is_non_interactive_does_not_match_neighbouring_flags() {
        for arg in ["--printer", "--print-mode", "--pretty", "-print", "--init"] {
            assert!(
                !compute_is_non_interactive(&cli_args(&[arg]), true),
                "{arg} is not one of main.tsx:1107-1109's flags"
            );
        }
        // `--init-only` is `includes`, so only the exact token counts.
        assert!(!compute_is_non_interactive(
            &cli_args(&["--init-only-please"]),
            true
        ));
    }

    /// `main.tsx:1119-1120` — `setIsInteractive(!isNonInteractive)`, the effect
    /// the four legs exist to produce.
    #[test]
    fn apply_is_interactive_writes_the_official_state_slot() {
        let _lock = utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let _guard = crate::bootstrap::state::IsInteractiveGuard::capture();

        apply_is_interactive(&cli_args(&["-p", "hello"]), true);
        assert!(!crate::bootstrap::state::get_is_interactive());
        assert!(crate::bootstrap::state::get_is_non_interactive_session());

        apply_is_interactive(&[], true);
        assert!(crate::bootstrap::state::get_is_interactive());
        assert!(!crate::bootstrap::state::get_is_non_interactive_session());

        // A piped stdout is headless on its own, with no flag at all.
        apply_is_interactive(&[], false);
        assert!(!crate::bootstrap::state::get_is_interactive());
        assert!(crate::bootstrap::state::get_is_non_interactive_session());
    }

    /// `entrypoints/cli.rs#run` feeds `argv[1..]`; CC feeds
    /// `process.argv.slice(2)`. Both drop exactly the program name, and the
    /// binary name must not be scanned — a binary at `/tmp/--print` would
    /// otherwise flip the session.
    #[test]
    fn startup_scan_uses_the_official_argv_slice() {
        let full = argv(&["--print", "hi"]);
        let scanned: Vec<String> = full.iter().skip(1).cloned().collect();
        assert_eq!(scanned, cli_args(&["--print", "hi"]));
        assert!(compute_is_non_interactive(&scanned, true));

        let program_only = vec!["--print".to_string()];
        let scanned: Vec<String> = program_only.iter().skip(1).cloned().collect();
        assert!(!compute_is_non_interactive(&scanned, true));
    }
}
