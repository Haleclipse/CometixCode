//! Maps to: CC `interactiveHelpers.tsx` (single file).
//!
//! Owns `showSetupScreens` gate order, snapshot derivation, Trust /
//! ClaudeMdExternalIncludes persistence, and the setup-screen UI host
//! ([`SetupScreensHost`]) — matching official JSX in the same file.

use crate::commands::terminal_setup::terminal_setup::should_offer_terminal_setup_for;
use crate::components::claude_md_external_includes_dialog::{
    ClaudeMdExternalIncludesChoice, ClaudeMdExternalIncludesDialog,
};
use crate::components::trust_dialog::utils::{
    get_api_key_helper_sources_from_settings, get_aws_commands_sources_from_settings,
    get_bash_permission_sources_from_settings, get_dangerous_env_vars_sources_from_settings,
    get_gcp_commands_sources_from_settings, get_hooks_sources_from_settings,
    get_otel_headers_helper_sources_from_settings,
};
use crate::components::trust_dialog::{
    TrustDialog, TrustDialogAcceptResult, TrustDialogRiskSnapshot, has_any_bash_execution,
};
use crate::services::mcp::config::get_mcp_configs_by_scope_readonly;
use crate::services::mcp::types::ConfigScope;
use crate::tools::agent_tool::load_agents_dir::get_agent_definitions_with_overrides_from_env;
use crate::utils::auth::{
    GetAnthropicApiKeyOptions, get_anthropic_api_key_with_source,
    get_api_key_from_config_or_macos_keychain, get_auth_token_source, is_anthropic_auth_enabled,
    is_claude_ai_subscriber,
};
use crate::utils::claude_in_chrome::setup::should_enable_claude_in_chrome_from_readonly_runtime;
use crate::utils::claudemd::{ClaudeMdFile, discover_claude_md_files};
use crate::utils::claudemd::{
    ExternalClaudeMdInclude, discover_claude_md_files_with_external_policy,
    get_external_claude_md_includes, should_show_claude_md_external_includes_warning,
};
use crate::utils::config::{
    CustomApiKeyStatus, GlobalConfig, ProjectConfig, get_custom_api_key_status, load_global_config,
    normalize_api_key_for_config, normalize_project_path,
};
use crate::utils::config::{
    check_has_trust_dialog_accepted, get_current_project_config,
    reset_trust_dialog_accepted_cache_for_testing, save_current_project_config,
};
use crate::utils::env as runtime_env;
use crate::utils::env_utils::is_running_on_homespace;
use crate::utils::ide::IDEExtensionInstallationStatus;
use crate::utils::ide::{is_jetbrains_ide, to_ide_display_name};
use crate::utils::settings::SettingSource;
use crate::utils::settings::get_settings_for_source;
use crate::utils::status_notice_definitions::{MemoryFileInfo, StatusNoticeContext};
use std::path::Path;
use std::sync::atomic::{AtomicI32, Ordering};

use crate::components::claude_in_chrome_onboarding::ClaudeInChromeOnboarding;
use crate::components::mcp_server_approval_dialog::{
    MCPServerApprovalChoice, MCPServerApprovalDialog,
};
use crate::components::mcp_server_multiselect_dialog::{
    MCPServerMultiselectDialog, MCPServerMultiselectResult,
};
use crate::components::onboarding::Onboarding;
use crate::state::app_state_store::McpWriter;
use crate::utils::settings::SettingsWithErrors;
use iocraft::prelude::*;
use std::sync::Arc;

/// Maps to CC `interactiveHelpers.tsx` `showSetupScreens` onboarding branch props.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SetupScreensSnapshot {
    pub show_onboarding: bool,
    pub oauth_enabled: bool,
    pub api_key_needing_approval: Option<String>,
    pub offer_terminal_setup: bool,
    pub terminal_name: Option<String>,
    pub show_claude_in_chrome_onboarding: bool,
    pub claude_in_chrome_extension_installed: bool,
}

pub fn project_config_for_cwd(global_config: &GlobalConfig, cwd: &Path) -> ProjectConfig {
    let project_key = normalize_project_path(&cwd.to_string_lossy());
    global_config
        .projects
        .get(&project_key)
        .cloned()
        .unwrap_or_default()
}

fn api_key_needing_onboarding_approval(
    global_config: &GlobalConfig,
    get_env: &impl Fn(&str) -> Option<String>,
) -> Option<String> {
    if is_running_on_homespace() {
        return None;
    }
    // CC Onboarding.tsx:132-138: only an empty value is absent, and the
    // suffix is taken from the value as set; auth matches the same
    // untrimmed bytes, so a trimmed suffix would never be found approved.
    let api_key = get_env("ANTHROPIC_API_KEY").filter(|value| !value.is_empty())?;
    let truncated = normalize_api_key_for_config(&api_key);
    (get_custom_api_key_status(global_config, &truncated) == CustomApiKeyStatus::New)
        .then_some(truncated)
}

pub fn setup_screens_snapshot_from_readonly_runtime(
    global_config: &GlobalConfig,
    get_env: &impl Fn(&str) -> Option<String>,
    args: impl IntoIterator<Item = String>,
    terminal_name: Option<String>,
    platform: runtime_env::Platform,
) -> SetupScreensSnapshot {
    if crate::utils::env_utils::is_env_truthy(get_env("IS_DEMO").as_deref()) {
        return SetupScreensSnapshot::default();
    }

    let configured_theme = global_config
        .theme
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let show_onboarding =
        configured_theme.is_none() || global_config.has_completed_onboarding != Some(true);
    let enable_claude_in_chrome =
        should_enable_claude_in_chrome_from_readonly_runtime(global_config, args, get_env);

    SetupScreensSnapshot {
        show_onboarding,
        oauth_enabled: is_anthropic_auth_enabled(),
        api_key_needing_approval: api_key_needing_onboarding_approval(global_config, get_env),
        offer_terminal_setup: should_offer_terminal_setup_for(terminal_name.as_deref(), platform),
        terminal_name,
        show_claude_in_chrome_onboarding: enable_claude_in_chrome
            && global_config.has_completed_claude_in_chrome_onboarding != Some(true),
        claude_in_chrome_extension_installed: false,
    }
}

fn status_notice_memory_files(files: Vec<ClaudeMdFile>) -> Vec<MemoryFileInfo> {
    files
        .into_iter()
        .map(|file| MemoryFileInfo {
            path: file.path.to_string_lossy().to_string(),
            content: file.content,
        })
        .collect()
}

pub fn status_notice_context_from_readonly_runtime(
    global_config: &GlobalConfig,
    get_env: &impl Fn(&str) -> Option<String>,
    cwd: &Path,
    memory_files: Vec<ClaudeMdFile>,
    ide_installation_status: Option<&IDEExtensionInstallationStatus>,
) -> StatusNoticeContext {
    let auth_token_source = get_auth_token_source()
        .source
        .status_label()
        .unwrap_or("none")
        .to_string();
    let api_key_source = get_anthropic_api_key_with_source(GetAnthropicApiKeyOptions {
        skip_retrieving_key_from_api_key_helper: true,
    })
    .source
    .status_label()
    .unwrap_or("none")
    .to_string();
    let ide_type = ide_installation_status.and_then(|status| status.ide_type.as_deref());

    StatusNoticeContext {
        cwd: cwd.to_string_lossy().to_string(),
        memory_files: status_notice_memory_files(memory_files),
        agent_definitions: Some(get_agent_definitions_with_overrides_from_env(cwd, get_env).into()),
        auth_token_source,
        api_key_source,
        has_console_api_key: get_api_key_from_config_or_macos_keychain().is_some(),
        is_claude_ai_subscriber: is_claude_ai_subscriber(),
        auto_install_ide_extension: global_config.auto_install_ide_extension.unwrap_or(true),
        is_supported_jetbrains_terminal: is_jetbrains_ide(ide_type),
        is_jetbrains_plugin_installed: ide_installation_status
            .map(|status| status.installed)
            .unwrap_or(false),
        ide_display_name: ide_type.map(|ide| to_ide_display_name(Some(ide))),
    }
}

#[cfg(test)]
thread_local! {
    static DEFAULT_SETUP_SNAPSHOT_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn reset_default_setup_snapshot_call_count() {
    DEFAULT_SETUP_SNAPSHOT_CALLS.with(|calls| calls.set(0));
}

#[cfg(test)]
pub(crate) fn default_setup_snapshot_call_count() -> usize {
    DEFAULT_SETUP_SNAPSHOT_CALLS.with(std::cell::Cell::get)
}

/// Startup-only runtime probe. Callers must materialize this before mounting
/// the retained TUI and pass the resulting snapshot as a launch input.
pub fn default_setup_screens_snapshot() -> SetupScreensSnapshot {
    #[cfg(test)]
    DEFAULT_SETUP_SNAPSHOT_CALLS.with(|calls| calls.set(calls.get() + 1));

    let detected = runtime_env::get();
    setup_screens_snapshot_from_readonly_runtime(
        &load_global_config(),
        &|key| std::env::var(key).ok(),
        std::env::args(),
        detected.terminal.clone(),
        detected.platform,
    )
}

pub fn default_status_notice_context(
    ide_installation_status: Option<&IDEExtensionInstallationStatus>,
) -> StatusNoticeContext {
    let cwd = std::env::current_dir().unwrap_or_default();
    status_notice_context_from_readonly_runtime(
        &load_global_config(),
        &|key| std::env::var(key).ok(),
        &cwd,
        discover_claude_md_files(),
        ide_installation_status,
    )
}

/// Props for [`SetupScreensHost`] — Maps to CC `showSetupScreens` render props
/// and completion of its awaited promise.
#[derive(Default, Props)]
struct SetupScreensHostProps<'a> {
    pub snapshot: SetupScreensSnapshot,
    pub mcp_startup: crate::main::McpStartupConfig,
    /// Maps to: CC `interactiveHelpers.tsx:218` `getSettingsWithAllErrors()` /
    /// `mcpServerApproval.tsx:16-19` — the setup phase reads settings from the
    /// source (disk snapshot resolved before mount), never from the main
    /// AppStore. Contract B clause 4b: no pre-mount store reads.
    pub startup_settings: Arc<SettingsWithErrors>,
    /// Post-approval MCP refresh target.
    ///
    /// SEAM: CC mounts a per-dialog auxiliary `<AppStateProvider>` for each
    /// approval dialog (`services/mcpServerApproval.tsx:29-45`) and therefore
    /// never writes the main store from the setup phase. Rust keeps the
    /// existing behaviour — a silent pre-mount refresh of the main store — but
    /// the writer now arrives as an explicit prop instead of being pulled from
    /// an ambient `AppStore` context, so `SetupScreensHost` has zero store
    /// context dependency (Contract B clause 4b).
    pub mcp_writer: Option<McpWriter>,
    pub on_done: HandlerMut<'a, ()>,
}

/// Maps to: CC `interactiveHelpers.tsx#showSetupScreens`.
///
/// The returned element completes through `on_done`, which is the retained-mode
/// equivalent of resolving CC's awaited setup promise.
/// Maps to: CC `interactiveHelpers.tsx` `exitWithError(root, message)`.
///
/// iocraft paints the current frame before honoring `SystemContext::exit()`;
/// `main::run` then performs the registered persistence cleanup and exits with
/// this code, matching Ink render → unmount → process exit ordering.
pub fn exit_with_error(
    system_context: &mut SystemContext,
    exit_code: &AtomicI32,
    message: impl Into<String>,
) -> AnyElement<'static> {
    exit_code.store(1, Ordering::SeqCst);
    system_context.exit();
    element! {
        ExitMessage(message: message.into())
    }
    .into_any()
}

#[derive(Default, Props)]
struct ExitMessageProps {
    message: String,
}

/// Maps to: CC `interactiveHelpers.tsx:108-110` `exitWithMessage`'s
/// `<Text color={color}>`, with `exitWithError`'s `color: 'error'`. The
/// colour is a theme key, resolved under the root's ThemeProvider.
#[component]
fn ExitMessage(props: &ExitMessageProps, hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<crate::utils::theme::Theme>();
    element! {
        Text(content: props.message.clone(), color: theme.error)
    }
}

pub fn show_setup_screens(
    snapshot: SetupScreensSnapshot,
    mcp_startup: crate::main::McpStartupConfig,
    startup_settings: Arc<SettingsWithErrors>,
    mcp_writer: Option<McpWriter>,
    on_done: impl FnMut(()) + Send + Sync + 'static,
) -> AnyElement<'static> {
    element! {
        SetupScreensHost(
            snapshot: snapshot,
            mcp_startup: mcp_startup,
            startup_settings: startup_settings,
            mcp_writer: mcp_writer,
            on_done: on_done,
        )
    }
    .into_any()
}

/// Component implementation for [`show_setup_screens`].
/// Gate order: onboarding → trust → mcpjson → ClaudeMd → chrome → complete.
#[component]
fn SetupScreensHost<'a>(
    props: &mut SetupScreensHostProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    // Contract B clause 4b: the setup phase runs BEFORE the AppStateProvider
    // mounts, so it must not touch the main store's context. Settings arrive as
    // a source-shaped prop (CC reads them from disk) and the post-approval MCP
    // refresh target is an explicit prop (see `SetupScreensHostProps`).
    let setup_screens_snapshot = props.snapshot.clone();
    let mcp_startup = props.mcp_startup.clone();
    let startup_settings = props.startup_settings.clone();
    let mcp_writer = props.mcp_writer.clone();

    // Gate visibility from props each render; dismissal is local state.
    // (Do not seed use_state from props — iocraft may first-mount with Default props.)
    let mut onboarding_dismissed = hooks.use_state(|| false);
    let mut chrome_dismissed = hooks.use_state(|| false);
    let mut trust_dismissed = hooks.use_state(|| false);
    let mut claude_md_dismissed = hooks.use_state(|| false);
    let trust_risk_snapshot = hooks.use_const(build_trust_dialog_risk_snapshot);
    let claude_md_external_includes = hooks.use_const(load_claude_md_external_includes_for_setup);

    // Maps to: CC `interactiveHelpers.tsx:218` + `mcpServerApproval.tsx:16-19`
    // — both derive the pending list from the settings snapshot read off disk.
    let mut pending_mcpjson_servers_state = hooks.use_state({
        let startup_settings = startup_settings.clone();
        move || crate::main::pending_mcpjson_approval_servers(&startup_settings.settings)
    });

    // Maps to: CC main.tsx `[STARTUP] Running showSetupScreens()...` once per mount.
    let mut setup_screens_started = hooks.use_state(|| false);
    let mut setup_screens_completed_logged = hooks.use_state(|| false);
    if !setup_screens_started.get() {
        setup_screens_started.set(true);
        crate::utils::debug::log_for_debugging("[STARTUP] Running showSetupScreens()...");
    }

    let show_onboarding = setup_screens_snapshot.show_onboarding && !onboarding_dismissed.get();
    let show_claude_in_chrome_onboarding =
        setup_screens_snapshot.show_claude_in_chrome_onboarding && !chrome_dismissed.get();
    let show_trust_dialog = should_show_trust_dialog() && !trust_dismissed.get();
    let show_claude_md_external_includes =
        should_show_claude_md_external_includes_gate() && !claude_md_dismissed.get();

    let pending_mcpjson_servers = { pending_mcpjson_servers_state.read().clone() };
    let setup_gate = resolve_setup_screen_gate(
        show_onboarding,
        show_trust_dialog,
        !pending_mcpjson_servers.is_empty(),
        show_claude_md_external_includes,
        show_claude_in_chrome_onboarding,
    );

    let gate_element = match setup_gate {
        SetupScreenGate::Onboarding => {
            // Maps to: CC `interactiveHelpers.tsx:171-182` — onboarding is a
            // `showSetupDialog(…, { onChangeAppState })`, i.e. its own
            // `<AppStateProvider><KeybindingSetup>` (:127-128), which the theme
            // step's ThemePicker reads. The other setup dialogs read no
            // AppState yet and keep only the KeybindingSetup below (seam).
            let snapshot = setup_screens_snapshot.clone();
            let keybinding_key = format!("{setup_gate:?}");
            // SEAM: CC's `getDefaultAppState()` seeds `settings:
            // getInitialSettings()` (AppStateStore.ts:469), which the picker's
            // syntax toggle reads. Seeding it here waits for the env redesign:
            // the port's onChangeAppState re-applies settings env on any
            // settings write that carries one, which a toggle would then do
            // before trust.
            element! {
                crate::state::app_state::AppStateProvider(
                    on_change_app_state: Some(crate::state::on_change_app_state::default_on_change()),
                    children: crate::state::app_state::ProviderChildren::new(move || {
                        let snapshot = snapshot.clone();
                        let mut dismissed = onboarding_dismissed;
                        element! {
                            crate::keybindings::keybinding_provider_setup::KeybindingSetup(key: keybinding_key.clone()) {
                                Onboarding(
                                    on_done: move |_| {
                                        dismissed.set(true);
                                    },
                                    oauth_enabled: snapshot.oauth_enabled,
                                    api_key_needing_approval: snapshot.api_key_needing_approval.clone(),
                                    offer_terminal_setup: snapshot.offer_terminal_setup,
                                    terminal_name: snapshot.terminal_name.clone(),
                                )
                            }
                        }
                        .into_any()
                    }),
                )
            }
            .into_any()
        }
        SetupScreenGate::Trust => {
            let snapshot = trust_risk_snapshot.clone();
            element! {
                TrustDialog(
                    snapshot: snapshot,
                    on_done: move |result| {
                        apply_trust_dialog_accept(&result);
                        trust_dismissed.set(true);
                    },
                    on_reject: move |_| {
                        reject_trust_dialog_and_exit();
                    },
                )
            }
            .into_any()
        }
        SetupScreenGate::Mcpjson => {
            if pending_mcpjson_servers.len() == 1 {
                let server_name = pending_mcpjson_servers[0].clone();
                let mut pending_state = pending_mcpjson_servers_state;
                element! {
                    MCPServerApprovalDialog(
                        server_name: server_name.clone(),
                        on_choice: move |choice: MCPServerApprovalChoice| {
                            let service_choice = match choice {
                                MCPServerApprovalChoice::YesAll => crate::services::mcp_server_approval::McpjsonServerApprovalChoice::YesAll,
                                MCPServerApprovalChoice::Yes => crate::services::mcp_server_approval::McpjsonServerApprovalChoice::Yes,
                                MCPServerApprovalChoice::No => crate::services::mcp_server_approval::McpjsonServerApprovalChoice::No,
                            };
                            match crate::services::mcp_server_approval::apply_single_mcpjson_server_approval_choice_to_local_settings(&server_name, service_choice) {
                                Ok(_) => {
                                    // CC's approval dialogs mount their OWN
                                    // `<AppStateProvider>` (`mcpServerApproval.tsx:30/38`)
                                    // and `onDone` merely resolves the promise —
                                    // they never touch the main store. The
                                    // approved set reaches the session because
                                    // the REPL-mounted `useManageMCPConnections`
                                    // reads the configs after setup completes.
                                    // Cometix now does the same: the write above
                                    // lands in local settings, and
                                    // `use_manage_mcp_connections` picks it up at
                                    // mount (P5 G10). Writing the main store here
                                    // was the last pre-mount store write that
                                    // Contract B clause 4 requires to be silent.
                                    pending_state.set(Vec::new());
                                }
                                Err(error) => tracing::warn!(server = %server_name, error = %error, "failed to write MCP .mcp.json approval choice"),
                            }
                        },
                    )
                }
                .into_any()
            } else {
                let server_names = pending_mcpjson_servers.clone();
                let mut pending_state = pending_mcpjson_servers_state;
                element! {
                    MCPServerMultiselectDialog(
                        server_names: server_names.clone(),
                        on_submit: move |result: MCPServerMultiselectResult| {
                            match crate::services::mcp_server_approval::apply_multi_mcpjson_server_approval_result_to_local_settings(&result.approved_servers, &result.rejected_servers) {
                                Ok(_) => {
                                    // CC's approval dialogs mount their OWN
                                    // `<AppStateProvider>` (`mcpServerApproval.tsx:30/38`)
                                    // and `onDone` merely resolves the promise —
                                    // they never touch the main store. The
                                    // approved set reaches the session because
                                    // the REPL-mounted `useManageMCPConnections`
                                    // reads the configs after setup completes.
                                    // Cometix now does the same: the write above
                                    // lands in local settings, and
                                    // `use_manage_mcp_connections` picks it up at
                                    // mount (P5 G10). Writing the main store here
                                    // was the last pre-mount store write that
                                    // Contract B clause 4 requires to be silent.
                                    pending_state.set(Vec::new());
                                }
                                Err(error) => tracing::warn!(servers = ?server_names, error = %error, "failed to write MCP .mcp.json approval result"),
                            }
                        },
                    )
                }
                .into_any()
            }
        }
        SetupScreenGate::ClaudeMdExternalIncludes => {
            let includes = claude_md_external_includes.clone();
            element! {
                ClaudeMdExternalIncludesDialog(
                    is_standalone_dialog: true,
                    external_includes: includes,
                    on_done: move |choice| {
                        apply_claude_md_external_includes_choice(choice);
                        claude_md_dismissed.set(true);
                    },
                )
            }
            .into_any()
        }
        SetupScreenGate::ClaudeInChrome => {
            let snapshot = setup_screens_snapshot.clone();
            element! {
                ClaudeInChromeOnboarding(
                    is_extension_installed: snapshot.claude_in_chrome_extension_installed,
                    on_done: move |_| {
                        chrome_dismissed.set(true);
                    },
                )
            }
            .into_any()
        }
        SetupScreenGate::Ready => {
            if !setup_screens_completed_logged.get() {
                setup_screens_completed_logged.set(true);
                crate::utils::debug::log_for_debugging("[STARTUP] showSetupScreens() completed");
                (props.on_done)(());
            }
            element! { Fragment }.into_any()
        }
    };
    // Onboarding already carries its showSetupDialog wrappers.
    if matches!(setup_gate, SetupScreenGate::Ready | SetupScreenGate::Onboarding) {
        return gate_element;
    }
    // Maps to: CC `interactiveHelpers.tsx:121-131` `showSetupDialog` — every
    // setup dialog renders inside its own `<KeybindingSetup>`. Keyed by gate,
    // so each dialog gets a fresh one, as each CC `showDialog` render does.
    element! {
        crate::keybindings::keybinding_provider_setup::KeybindingSetup(key: format!("{setup_gate:?}")) {
            #(Some(gate_element))
        }
    }
    .into_any()
}

#[cfg(test)]
mod setup_screens_snapshot_tests {
    use super::*;
    use crate::utils::config::CustomApiKeyResponses;
    use std::fs;
    use std::path::Path;

    #[test]
    fn api_key_needing_approval_keeps_the_key_as_set() {
        // CC Onboarding.tsx:132-138: `normalizeApiKeyForConfig` of the raw
        // value, as auth later matches it; only an empty value is absent.
        let _lock = crate::utils::env_utils::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _homespace = crate::utils::env_utils::EnvVarGuard::unset("COO_RUNNING_ON_HOMESPACE");
        let config = GlobalConfig::default();
        let with_key = |value: &'static str| {
            move |key: &str| (key == "ANTHROPIC_API_KEY").then(|| value.to_string())
        };
        assert_eq!(
            api_key_needing_onboarding_approval(&config, &with_key("sk-ant-test-key\r")),
            Some(normalize_api_key_for_config("sk-ant-test-key\r"))
        );
        assert_eq!(api_key_needing_onboarding_approval(&config, &with_key("")), None);
    }

    #[test]
    fn setup_screens_snapshot_matches_official_onboarding_gate_and_api_key_step() {
        let mut config = GlobalConfig::default();
        let snapshot = setup_screens_snapshot_from_readonly_runtime(
            &config,
            &|_| None,
            Vec::<String>::new(),
            Some("vscode".to_string()),
            runtime_env::Platform::MacOS,
        );
        assert!(snapshot.show_onboarding);
        assert!(snapshot.oauth_enabled);
        assert!(snapshot.offer_terminal_setup);

        config.theme = Some("dark".to_string());
        config.has_completed_onboarding = Some(true);
        let completed = setup_screens_snapshot_from_readonly_runtime(
            &config,
            &|_| None,
            Vec::<String>::new(),
            Some("kitty".to_string()),
            runtime_env::Platform::MacOS,
        );
        assert!(!completed.show_onboarding);
        assert!(!completed.offer_terminal_setup);

        let with_new_key = setup_screens_snapshot_from_readonly_runtime(
            &config,
            &|key| {
                (key == "ANTHROPIC_API_KEY")
                    .then(|| "sk-ant-abcdefghijklmnopqrstuvwxyz".to_string())
            },
            Vec::<String>::new(),
            None,
            runtime_env::Platform::Linux,
        );
        assert_eq!(
            with_new_key.api_key_needing_approval.as_deref(),
            Some("ghijklmnopqrstuvwxyz")
        );

        config.custom_api_key_responses = Some(CustomApiKeyResponses {
            approved: Some(vec!["ghijklmnopqrstuvwxyz".to_string()]),
            rejected: None,
        });
        let approved_key = setup_screens_snapshot_from_readonly_runtime(
            &config,
            &|key| {
                (key == "ANTHROPIC_API_KEY")
                    .then(|| "sk-ant-abcdefghijklmnopqrstuvwxyz".to_string())
            },
            Vec::<String>::new(),
            None,
            runtime_env::Platform::Linux,
        );
        assert!(approved_key.api_key_needing_approval.is_none());

        let demo = setup_screens_snapshot_from_readonly_runtime(
            &GlobalConfig::default(),
            &|key| (key == "IS_DEMO").then(|| "1".to_string()),
            Vec::<String>::new(),
            None,
            runtime_env::Platform::Linux,
        );
        assert!(!demo.show_onboarding);
    }

    #[test]
    fn setup_screens_snapshot_matches_official_claude_in_chrome_gate() {
        let mut config = GlobalConfig::default();
        config.theme = Some("dark".to_string());
        config.has_completed_onboarding = Some(true);

        let by_flag = setup_screens_snapshot_from_readonly_runtime(
            &config,
            &|_| None,
            vec!["cometix".to_string(), "--chrome".to_string()],
            None,
            runtime_env::Platform::Linux,
        );
        assert!(by_flag.show_claude_in_chrome_onboarding);

        let by_no_flag = setup_screens_snapshot_from_readonly_runtime(
            &config,
            &|_| None,
            vec!["cometix".to_string(), "--no-chrome".to_string()],
            None,
            runtime_env::Platform::Linux,
        );
        assert!(!by_no_flag.show_claude_in_chrome_onboarding);

        let by_env = setup_screens_snapshot_from_readonly_runtime(
            &config,
            &|key| (key == "CLAUDE_CODE_ENABLE_CFC").then(|| "true".to_string()),
            Vec::<String>::new(),
            None,
            runtime_env::Platform::Linux,
        );
        assert!(by_env.show_claude_in_chrome_onboarding);

        config.claude_in_chrome_default_enabled = Some(true);
        let env_falsy_wins = setup_screens_snapshot_from_readonly_runtime(
            &config,
            &|key| (key == "CLAUDE_CODE_ENABLE_CFC").then(|| "false".to_string()),
            Vec::<String>::new(),
            None,
            runtime_env::Platform::Linux,
        );
        assert!(!env_falsy_wins.show_claude_in_chrome_onboarding);

        let by_config = setup_screens_snapshot_from_readonly_runtime(
            &config,
            &|_| None,
            Vec::<String>::new(),
            None,
            runtime_env::Platform::Linux,
        );
        assert!(by_config.show_claude_in_chrome_onboarding);

        config.has_completed_claude_in_chrome_onboarding = Some(true);
        let completed = setup_screens_snapshot_from_readonly_runtime(
            &config,
            &|_| None,
            vec!["cometix".to_string(), "--chrome".to_string()],
            None,
            runtime_env::Platform::Linux,
        );
        assert!(!completed.show_claude_in_chrome_onboarding);
    }

    #[test]
    fn status_notice_context_from_readonly_runtime_builds_auth_memory_and_ide_snapshot() {
        // `auth_token_source` and `api_key_source` come from
        // `get_auth_token_source()` / `get_anthropic_api_key_with_source()`,
        // which read the process environment directly — CC does the same
        // (`utils/auth.ts:125` reads `process.env.ANTHROPIC_AUTH_TOKEN`). The
        // `get_env` parameter below only feeds the agent-definition overrides,
        // so these two assertions need the real variable set.
        let _env_guard = crate::utils::env_utils::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _token = crate::utils::env_utils::EnvVarGuard::set("ANTHROPIC_AUTH_TOKEN", "token");

        let mut global_config = GlobalConfig::default();
        global_config.primary_api_key = Some("sk-console".to_string());
        global_config.auto_install_ide_extension = Some(true);
        let memory_file = ClaudeMdFile {
            path: Path::new("/repo/CLAUDE.md").to_path_buf(),
            source: crate::utils::claudemd::ClaudeMdSource::Project,
            kind: crate::utils::claudemd::ClaudeMdKind::Project,
            content: "project memory".to_string(),
            parent: None,
            is_nested: false,
        };
        let ide_status = IDEExtensionInstallationStatus {
            installed: false,
            error: None,
            installed_version: None,
            ide_type: Some("pycharm".to_string()),
        };

        // `api_key_source` resolves through `load_global_config()`
        // (`auth.rs:534`), not the `global_config` argument — that one only
        // feeds `auto_install_ide_extension`. Inject the same config so the
        // /login-managed key is actually visible to the lookup.
        crate::utils::config::set_test_global_config(Some(global_config.clone()));

        let context = status_notice_context_from_readonly_runtime(
            &global_config,
            &|key| (key == "ANTHROPIC_AUTH_TOKEN").then(|| "token".to_string()),
            Path::new("/repo"),
            vec![memory_file],
            Some(&ide_status),
        );
        crate::utils::config::set_test_global_config(None);

        assert_eq!(context.cwd, "/repo");
        assert_eq!(context.memory_files[0].path, "/repo/CLAUDE.md");
        assert_eq!(context.auth_token_source, "ANTHROPIC_AUTH_TOKEN");
        assert_eq!(context.api_key_source, "/login managed key");
        assert!(context.has_console_api_key);
        assert!(context.is_supported_jetbrains_terminal);
        assert_eq!(context.ide_display_name.as_deref(), Some("PyCharm"));
    }

    #[test]
    fn auth_token_and_api_key_guards_restore_distinct_sentinels() {
        let _env_guard = crate::utils::env_utils::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _restore_token = crate::utils::env_utils::EnvVarGuard::preserve("ANTHROPIC_AUTH_TOKEN");
        let _restore_api_key = crate::utils::env_utils::EnvVarGuard::preserve("ANTHROPIC_API_KEY");
        crate::utils::process_env::set("ANTHROPIC_AUTH_TOKEN", "token-sentinel");
        crate::utils::process_env::set("ANTHROPIC_API_KEY", "api-key-sentinel");

        {
            let _token = crate::utils::env_utils::EnvVarGuard::set(
                "ANTHROPIC_AUTH_TOKEN",
                "token-under-test",
            );
            let _api_key = crate::utils::env_utils::EnvVarGuard::set(
                "ANTHROPIC_API_KEY",
                "api-key-under-test",
            );
        }

        assert_eq!(
            crate::utils::process_env::var("ANTHROPIC_AUTH_TOKEN").as_deref(),
            Some("token-sentinel")
        );
        assert_eq!(
            crate::utils::process_env::var("ANTHROPIC_API_KEY").as_deref(),
            Some("api-key-sentinel")
        );
    }

    #[test]
    fn project_config_for_cwd_uses_normalized_readonly_global_config_key() {
        let mut global_config = GlobalConfig::default();
        let mut project_config = ProjectConfig::default();
        project_config.mcp_servers = Some(serde_json::json!({"alpha":{}}));
        global_config.projects.insert(
            normalize_project_path("/tmp/cometix-project"),
            project_config,
        );

        let loaded = project_config_for_cwd(&global_config, Path::new("/tmp/cometix-project"));
        assert!(
            loaded
                .mcp_servers
                .as_ref()
                .is_some_and(|servers| servers.get("alpha").is_some())
        );
    }

    #[test]
    fn status_notice_context_uses_official_api_key_approval_for_conflict_source() {
        // Same two fixtures the sibling test above documents and this one was
        // missing. `api_key_source` does not read the `global_config` argument
        // or the `get_env` closure — it goes through
        // `get_anthropic_api_key_with_source()`, which reads the process
        // environment and `load_global_config()` (auth.rs:534), exactly as CC
        // reads `process.env` in `utils/auth.ts`. Without both, the lookup saw
        // the scratch home's empty config and no ANTHROPIC_API_KEY, so the
        // source was "none" and this looked like an approval-logic bug.
        let _env_guard = crate::utils::env_utils::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _api_key =
            crate::utils::env_utils::EnvVarGuard::set("ANTHROPIC_API_KEY", "sk-ant-test");

        let mut global_config = GlobalConfig {
            primary_api_key: Some("sk-console".to_string()),
            ..Default::default()
        };
        let env_key = |key: &str| (key == "ANTHROPIC_API_KEY").then(|| "sk-ant-test".to_string());

        crate::utils::config::set_test_global_config(Some(global_config.clone()));
        let unapproved = status_notice_context_from_readonly_runtime(
            &global_config,
            &env_key,
            Path::new("/repo"),
            Vec::new(),
            None,
        );
        assert_eq!(unapproved.api_key_source, "/login managed key");

        global_config.custom_api_key_responses = Some(CustomApiKeyResponses {
            approved: Some(vec![normalize_api_key_for_config("sk-ant-test")]),
            rejected: None,
        });
        // Re-inject: the approval list is what the lookup consults, and it also
        // lives in the loaded global config, not the argument.
        crate::utils::config::set_test_global_config(Some(global_config.clone()));
        let approved = status_notice_context_from_readonly_runtime(
            &global_config,
            &env_key,
            Path::new("/repo"),
            Vec::new(),
            None,
        );
        crate::utils::config::set_test_global_config(None);

        assert_eq!(approved.api_key_source, "ANTHROPIC_API_KEY");
        assert!(crate::utils::status_notice_definitions::get_active_notices(&approved).contains(
            &crate::utils::status_notice_definitions::StatusNoticeId::ApiKeyConflict,
        ));
    }

    #[test]
    fn status_notice_context_from_readonly_runtime_loads_agent_definitions_snapshot() {
        let root = std::env::temp_dir().join(format!(
            "cometix-status-notice-agent-defs-{}",
            uuid::Uuid::new_v4()
        ));
        let config_home = root.join("config");
        let cwd = root.join("repo");
        fs::create_dir_all(config_home.join("agents")).expect("create agent dir");
        fs::create_dir_all(&cwd).expect("create cwd");
        fs::write(
            config_home.join("agents/reviewer.md"),
            format!(
                "---\nname: reviewer\ndescription: {}\n---\nPrompt body is not executed.",
                "x".repeat(
                    (crate::utils::status_notice_helpers::AGENT_DESCRIPTIONS_THRESHOLD as usize
                        + 1)
                        * 4
                )
            ),
        )
        .expect("write agent definition");

        let context = status_notice_context_from_readonly_runtime(
            &GlobalConfig::default(),
            &|key| match key {
                "CLAUDE_CONFIG_DIR" => Some(config_home.display().to_string()),
                "HOME" => Some(root.display().to_string()),
                _ => None,
            },
            &cwd,
            Vec::new(),
            None,
        );
        let agent_definitions = context
            .agent_definitions
            .as_ref()
            .expect("agent definitions snapshot");
        let reviewer = agent_definitions
            .active_agents
            .iter()
            .find(|agent| agent.agent_type == "reviewer")
            .expect("custom reviewer agent");

        assert_eq!(reviewer.source, "userSettings");
        assert!(
            crate::utils::status_notice_definitions::get_active_notices(&context).contains(
                &crate::utils::status_notice_definitions::StatusNoticeId::LargeAgentDescriptions,
            )
        );

        let _ = fs::remove_dir_all(root);
    }
}

/// Maps to: CC `showSetupScreens` sequential gates (interactive path only).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupScreenGate {
    Onboarding,
    Trust,
    Mcpjson,
    ClaudeMdExternalIncludes,
    ClaudeInChrome,
    Ready,
}

/// Maps to: CC `interactiveHelpers.tsx` CLAUBBIT / demo skip of trust.
pub fn should_skip_trust_dialog_for_env() -> bool {
    crate::utils::env_utils::is_env_truthy(std::env::var("CLAUBBIT").ok().as_deref())
        || crate::utils::env_utils::is_env_truthy(std::env::var("IS_DEMO").ok().as_deref())
}

/// Whether TrustDialog must still be shown (CC fast-path when already accepted).
pub fn should_show_trust_dialog() -> bool {
    if should_skip_trust_dialog_for_env() {
        return false;
    }
    !check_has_trust_dialog_accepted()
}

/// Maps to: CC showSetupScreens ClaudeMdExternalIncludes gate predicate.
/// Skipped under CLAUBBIT/IS_DEMO with the same trust-block as official.
pub fn should_show_claude_md_external_includes_gate() -> bool {
    if should_skip_trust_dialog_for_env() {
        return false;
    }
    should_show_claude_md_external_includes_warning()
}

/// Snapshot of external includes for the standalone setup dialog.
pub fn load_claude_md_external_includes_for_setup() -> Vec<ExternalClaudeMdInclude> {
    let additional_dirs = crate::bootstrap::state::get_additional_directories_for_claude_md();
    let include_default_discovery = !crate::utils::env_utils::is_env_truthy(
        std::env::var("CLAUDE_CODE_SIMPLE").ok().as_deref(),
    );
    let files = discover_claude_md_files_with_external_policy(
        include_default_discovery,
        &additional_dirs,
        true,
    );
    get_external_claude_md_includes(&files)
}

fn home_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(std::path::PathBuf::from)
}

fn paths_equal(a: &Path, b: &Path) -> bool {
    let canon_a = a.canonicalize().unwrap_or_else(|_| a.to_path_buf());
    let canon_b = b.canonicalize().unwrap_or_else(|_| b.to_path_buf());
    canon_a == canon_b
}

/// Build the risk snapshot TrustDialog renders (settings + project MCP).
pub fn build_trust_dialog_risk_snapshot() -> TrustDialogRiskSnapshot {
    let cwd = std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf());
    let cwd_display = cwd.display().to_string();
    let is_home_dir = home_dir().is_some_and(|home| paths_equal(&home, &cwd));

    let project_settings = get_settings_for_source(SettingSource::Project);
    let local_settings = get_settings_for_source(SettingSource::Local);

    let hooks = get_hooks_sources_from_settings(project_settings.as_ref(), local_settings.as_ref());
    let bash = get_bash_permission_sources_from_settings(
        project_settings.as_ref(),
        local_settings.as_ref(),
    );
    let api_key = get_api_key_helper_sources_from_settings(
        project_settings.as_ref(),
        local_settings.as_ref(),
    );
    let aws =
        get_aws_commands_sources_from_settings(project_settings.as_ref(), local_settings.as_ref());
    let gcp =
        get_gcp_commands_sources_from_settings(project_settings.as_ref(), local_settings.as_ref());
    let otel = get_otel_headers_helper_sources_from_settings(
        project_settings.as_ref(),
        local_settings.as_ref(),
    );
    let dangerous = get_dangerous_env_vars_sources_from_settings(
        project_settings.as_ref(),
        local_settings.as_ref(),
    );

    let global_config = load_global_config();
    let project_config = get_current_project_config();
    let project_servers =
        get_mcp_configs_by_scope_readonly(ConfigScope::Project, &global_config, &project_config);
    let has_mcp_servers = !project_servers.servers.is_empty();

    let has_bash = has_any_bash_execution(&bash, &[]);

    TrustDialogRiskSnapshot {
        cwd: cwd_display,
        is_home_dir,
        has_trust_dialog_accepted: check_has_trust_dialog_accepted(),
        has_mcp_servers,
        has_hooks: !hooks.is_empty(),
        has_bash_execution: has_bash,
        has_slash_command_bash: false,
        has_skills_bash: false,
        has_api_key_helper: !api_key.is_empty(),
        has_aws_commands: !aws.is_empty(),
        has_gcp_commands: !gcp.is_empty(),
        has_otel_headers_helper: !otel.is_empty(),
        has_dangerous_env_vars: !dangerous.is_empty(),
    }
}

/// Apply TrustDialog accept — maps to CC `onChange('enable_all')` side effects.
pub fn apply_trust_dialog_accept(result: &TrustDialogAcceptResult) {
    if result.would_set_session_trust_accepted {
        crate::bootstrap::state::set_session_trust_accepted(true);
    }
    if result.would_persist_project_trust {
        let _ = save_current_project_config(|project| {
            project.has_trust_dialog_accepted = Some(true);
        });
    }
    // Allow check_has_trust_dialog_accepted to re-latch after mid-session accept.
    reset_trust_dialog_accepted_cache_for_testing();
    let _ = check_has_trust_dialog_accepted();
}

/// Maps to: CC `ClaudeMdExternalIncludesDialog` `handleSelection` config writes.
pub fn apply_claude_md_external_includes_choice(choice: ClaudeMdExternalIncludesChoice) {
    let approved = matches!(choice, ClaudeMdExternalIncludesChoice::Yes);
    let _ = save_current_project_config(|project| {
        project.has_claude_md_external_includes_approved = Some(approved);
        project.has_claude_md_external_includes_warning_shown = Some(true);
    });
}

/// Maps to: CC `gracefulShutdownSync(1)` on TrustDialog reject.
pub fn reject_trust_dialog_and_exit() -> ! {
    crate::utils::cleanup_registry::exit_process(1)
}

/// Resolve the next setup gate given live flags (CC showSetupScreens order).
pub fn resolve_setup_screen_gate(
    show_onboarding: bool,
    show_trust: bool,
    pending_mcpjson: bool,
    show_claude_md_external: bool,
    show_chrome: bool,
) -> SetupScreenGate {
    if show_onboarding {
        SetupScreenGate::Onboarding
    } else if show_trust {
        SetupScreenGate::Trust
    } else if pending_mcpjson {
        SetupScreenGate::Mcpjson
    } else if show_claude_md_external {
        SetupScreenGate::ClaudeMdExternalIncludes
    } else if show_chrome {
        SetupScreenGate::ClaudeInChrome
    } else {
        SetupScreenGate::Ready
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_screen_gate_order_matches_official_show_setup_screens() {
        assert_eq!(
            resolve_setup_screen_gate(true, true, true, true, true),
            SetupScreenGate::Onboarding
        );
        assert_eq!(
            resolve_setup_screen_gate(false, true, true, true, true),
            SetupScreenGate::Trust
        );
        assert_eq!(
            resolve_setup_screen_gate(false, false, true, true, true),
            SetupScreenGate::Mcpjson
        );
        assert_eq!(
            resolve_setup_screen_gate(false, false, false, true, true),
            SetupScreenGate::ClaudeMdExternalIncludes
        );
        assert_eq!(
            resolve_setup_screen_gate(false, false, false, false, true),
            SetupScreenGate::ClaudeInChrome
        );
        assert_eq!(
            resolve_setup_screen_gate(false, false, false, false, false),
            SetupScreenGate::Ready
        );
    }

    #[test]
    fn apply_trust_dialog_accept_sets_session_trust_for_home_dir() {
        crate::bootstrap::state::set_session_trust_accepted(false);
        reset_trust_dialog_accepted_cache_for_testing();
        apply_trust_dialog_accept(&TrustDialogAcceptResult {
            is_home_dir: true,
            would_set_session_trust_accepted: true,
            would_persist_project_trust: false,
            risk_snapshot: TrustDialogRiskSnapshot::default(),
        });
        assert!(crate::bootstrap::state::get_session_trust_accepted());
        assert!(check_has_trust_dialog_accepted());
        crate::bootstrap::state::set_session_trust_accepted(false);
        reset_trust_dialog_accepted_cache_for_testing();
    }
}
