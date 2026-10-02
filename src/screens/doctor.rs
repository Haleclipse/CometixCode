//! Maps to: CC `screens/Doctor.tsx`.
//!
//! Safe main-screen implementation of the Doctor screen. The official screen
//! performs async diagnostic probes, dist-tag network fetches, settings error
//! hooks, MCP/keybinding/plugin/sandbox warning hooks, and PID lock cleanup.
//! Cometix now delegates local diagnostic shaping to `utils/doctor_diagnostic.rs`
//! (the official `utils/doctorDiagnostic.ts` boundary) while keeping network,
//! installer mutation, settings writes, process cleanup, and real external
//! command execution disabled in this screen.

use crate::components::design_system::pane::Pane;
use crate::components::keybinding_warnings::{KeybindingWarningItem, KeybindingWarnings};
use crate::components::mcp::mcp_parsing_warnings::McpParsingWarnings;
use crate::components::press_enter_to_continue::PressEnterToContinue;
use crate::components::sandbox::sandbox_doctor_section::SandboxDoctorSection;
use crate::components::validation_errors_list::ValidationErrorsList;
use crate::constants::figures;
use crate::tools::agent_tool::load_agents_dir::FailedAgentFile;
use crate::types::plugin::{PluginError, get_plugin_error_message};
use crate::utils::doctor_context_warnings::{
    ContextWarnings, DoctorContextWarningsInput, check_context_warnings,
};
use crate::utils::doctor_diagnostic::{DiagnosticInfo, RipgrepMode, get_doctor_diagnostic};
use crate::utils::env_validation::{
    EnvVarValidationResult, EnvVarValidationStatus, validate_bounded_int_env_var,
};
use crate::utils::native_installer::pid_lock::{
    LockInfo, cleanup_stale_locks, get_all_lock_info, is_pid_based_locking_enabled,
};
use crate::utils::settings::ValidationError;
use crate::utils::shell::output_limits::{BASH_MAX_OUTPUT_DEFAULT, BASH_MAX_OUTPUT_UPPER_LIMIT};
use crate::utils::task::output_formatting::{TASK_MAX_OUTPUT_DEFAULT, TASK_MAX_OUTPUT_UPPER_LIMIT};
use iocraft::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq)]
struct EnvValidationWarning {
    name: &'static str,
    result: EnvVarValidationResult,
}

/// Maps to: CC `screens/Doctor.tsx` `envValidationErrors` memo and
/// `utils/envValidation.ts` `validateBoundedIntEnvVar` call site.
fn doctor_validate_env_value(
    name: &'static str,
    value: Option<&str>,
    default: usize,
    upper_limit: usize,
) -> Option<EnvValidationWarning> {
    let result = validate_bounded_int_env_var(name, value, default, upper_limit);
    (result.status != EnvVarValidationStatus::Valid)
        .then_some(EnvValidationWarning { name, result })
}

/// Maps to: CC `screens/Doctor.tsx` `envValidationErrors` `envVars.map(...)`.
fn doctor_validate_env_var(
    name: &'static str,
    default: usize,
    upper_limit: usize,
) -> Option<EnvValidationWarning> {
    let value = crate::utils::process_env::var(name);
    doctor_validate_env_value(name, value.as_deref(), default, upper_limit)
}

/// Maps to: CC `screens/Doctor.tsx` `envValidationErrors`.
fn env_validation_warnings() -> Vec<EnvValidationWarning> {
    let max_output_tokens = crate::utils::context::get_model_max_output_tokens("claude-opus-4-6");
    [
        (
            "BASH_MAX_OUTPUT_LENGTH",
            BASH_MAX_OUTPUT_DEFAULT,
            BASH_MAX_OUTPUT_UPPER_LIMIT,
        ),
        (
            "TASK_MAX_OUTPUT_LENGTH",
            TASK_MAX_OUTPUT_DEFAULT,
            TASK_MAX_OUTPUT_UPPER_LIMIT,
        ),
        (
            "CLAUDE_CODE_MAX_OUTPUT_TOKENS",
            max_output_tokens.default as usize,
            max_output_tokens.upper_limit as usize,
        ),
    ]
    .into_iter()
    .filter_map(|(name, default, upper)| doctor_validate_env_var(name, default, upper))
    .collect()
}

fn doctor_auto_update_channel() -> String {
    crate::utils::settings::load_settings_from_disk()
        .settings
        .auto_updates_channel
        .or_else(|| crate::utils::process_env::var("COMETIX_AUTO_UPDATE_CHANNEL"))
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "latest".to_string())
}

/// Maps to CC `screens/Doctor.tsx` `VersionLockInfo`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VersionLockInfo {
    pub enabled: bool,
    pub locks: Vec<LockInfo>,
    pub locks_dir: String,
    pub stale_locks_cleaned: usize,
}

/// Maps to CC `Doctor.tsx` PID-lock effect branch.
///
/// Safety divergence: CC calls `cleanupStaleLocks(locksDir)` before reading
/// locks. Cometix's `cleanup_stale_locks` is safe-disabled and removes
/// nothing, so this remains a read-only Doctor diagnostic.
fn get_doctor_version_lock_info_readonly() -> VersionLockInfo {
    if !is_pid_based_locking_enabled() {
        return VersionLockInfo {
            enabled: false,
            locks: Vec::new(),
            locks_dir: String::new(),
            stale_locks_cleaned: 0,
        };
    }

    let locks_dir = crate::utils::xdg::get_xdg_state_home()
        .join("claude")
        .join("locks");
    let stale_locks_cleaned = cleanup_stale_locks(&locks_dir);
    let locks = get_all_lock_info(&locks_dir);
    VersionLockInfo {
        enabled: true,
        locks,
        locks_dir: locks_dir.display().to_string(),
        stale_locks_cleaned,
    }
}

fn doctor_search_status(diagnostic: &DiagnosticInfo) -> String {
    let status = if diagnostic.ripgrep_status.working {
        "OK"
    } else {
        "Not working"
    };
    let mode = match diagnostic.ripgrep_status.mode {
        RipgrepMode::Embedded => "bundled".to_string(),
        RipgrepMode::Builtin => "vendor".to_string(),
        RipgrepMode::System => diagnostic
            .ripgrep_status
            .system_path
            .clone()
            .unwrap_or_else(|| "system".to_string()),
    };
    format!("{status} ({mode})")
}
/// Maps to: CC `screens/Doctor.tsx` `Props`.
#[derive(Default, Props)]
pub struct DoctorProps<'a> {
    /// Rust-side equivalent of official `onDone(...)` dismissal callback.
    pub on_close: HandlerMut<'a, ()>,
    /// Maps to CC `screens/Doctor.tsx` `<KeybindingWarnings />`, with the
    /// runtime keybinding loader snapshot supplied by the caller. Keeping this
    /// explicit avoids filesystem reads in the safe Doctor screen.
    pub keybinding_customization_enabled: bool,
    pub keybindings_path: String,
    pub keybinding_warnings: Vec<KeybindingWarningItem>,
    /// Maps to CC `useSettingsErrors()` after filtering MCP metadata out.
    pub settings_errors: Vec<ValidationError>,
    /// Maps to CC `Doctor.tsx` `versionLockInfo`.
    pub version_lock_info: Option<VersionLockInfo>,
    /// Maps to CC `Doctor.tsx` `agentInfo.failedFiles`.
    pub agent_parse_errors: Vec<FailedAgentFile>,
    /// Maps to CC `Doctor.tsx` `pluginsErrors` from AppState.
    pub plugin_errors: Vec<PluginError>,
    /// Maps to CC `doctorContextWarnings.ts#checkContextWarnings(...)` result.
    pub context_warnings: Option<ContextWarnings>,
}

/// Maps to: CC `screens/Doctor.tsx` `Doctor`.
#[component]
pub fn Doctor<'a>(props: &mut DoctorProps<'a>, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<crate::utils::theme::Theme>();
    let mut should_close = hooks.use_state(|| false);

    let runtime = hooks
        .try_use_context::<crate::keybindings::keybinding_context::KeybindingRuntime>()
        .map(|runtime| runtime.clone());
    let handlers: crate::keybindings::use_keybinding::KeybindingHandlers = vec![
        (
            "confirm:yes".to_string(),
            Box::new(move || {
                should_close.set(true);
                true
            }),
        ),
        (
            "confirm:no".to_string(),
            Box::new(move || {
                should_close.set(true);
                true
            }),
        ),
    ];
    crate::keybindings::use_keybinding::use_keybindings(
        &mut hooks,
        runtime,
        handlers,
        crate::keybindings::types::ContextName::Confirmation,
        || true,
    );

    if should_close.get() {
        should_close.set(false);
        (props.on_close)(());
    }

    let diagnostic = get_doctor_diagnostic();
    // CC `Doctor.tsx:157-168` `useMemo(..., [])`: validated once per mount, so
    // its debug logging does not repeat on every frame.
    let env_warnings = hooks.use_memo(env_validation_warnings, ());
    let auto_update_channel = doctor_auto_update_channel();
    let search_status = doctor_search_status(&diagnostic);
    let keybinding_warnings = props.keybinding_warnings.clone();
    let keybindings_path = props.keybindings_path.clone();
    let keybinding_customization_enabled = props.keybinding_customization_enabled;
    let settings_errors = props.settings_errors.clone();
    let version_lock_info = props
        .version_lock_info
        .clone()
        .unwrap_or_else(get_doctor_version_lock_info_readonly);
    let agent_parse_errors = props.agent_parse_errors.clone();
    let plugin_errors = props.plugin_errors.clone();
    let context_warnings = props
        .context_warnings
        .clone()
        .unwrap_or_else(|| check_context_warnings(&DoctorContextWarningsInput::default()));
    let unreachable_rules_warning = context_warnings.unreachable_rules_warning.clone();
    let claude_md_warning = context_warnings.claude_md_warning.clone();
    let agent_warning = context_warnings.agent_warning.clone();
    let mcp_warning = context_warnings.mcp_warning.clone();
    let has_context_usage_warnings =
        claude_md_warning.is_some() || agent_warning.is_some() || mcp_warning.is_some();
    let warning_symbol = figures::get().warning.to_string();

    element! {
        Pane {
            View(flex_direction: FlexDirection::Column) {
                Text(content: "Diagnostics".to_string(), weight: Weight::Bold)
                Text(content: format!("└ Currently running: {} ({})", diagnostic.installation_type.as_str(), diagnostic.version))
                #(diagnostic.package_manager.as_ref().map(|package_manager| element! {
                    Text(content: format!("└ Package manager: {package_manager}"))
                }))
                Text(content: format!("└ Path: {}", diagnostic.installation_path))
                Text(content: format!("└ Invoked: {}", diagnostic.invoked_binary))
                Text(content: format!("└ Config install method: {}", diagnostic.config_install_method))
                Text(content: format!("└ Search: {search_status}"))
                #(diagnostic.recommendation.as_ref().map(|recommendation| element! {
                    View(margin_top: 1u32, flex_direction: FlexDirection::Column) {
                        Text(content: format!("Recommendation: {}", recommendation.lines().next().unwrap_or(recommendation)), color: theme.warning)
                        #(recommendation.lines().nth(1).map(|line| element! {
                            Text(content: line.to_string(), color: theme.inactive)
                        }))
                    }
                }))
                #(if diagnostic.multiple_installations.len() > 1 {
                    Some(element! {
                        View(margin_top: 1u32, flex_direction: FlexDirection::Column) {
                            Text(content: "Warning: Multiple installations found".to_string(), color: theme.warning)
                            #(diagnostic.multiple_installations.iter().map(|installation| element! {
                                Text(content: format!("└ {} at {}", installation.install_type, installation.path))
                            }))
                        }
                    })
                } else {
                    None
                })
                #(if diagnostic.warnings.is_empty() {
                    None
                } else {
                    Some(element! {
                        View(margin_top: 1u32, flex_direction: FlexDirection::Column) {
                            #(diagnostic.warnings.iter().map(|warning| element! {
                                View(flex_direction: FlexDirection::Column) {
                                    Text(content: format!("Warning: {}", warning.issue), color: theme.warning)
                                    Text(content: format!("Fix: {}", warning.fix))
                                }
                            }))
                        }
                    })
                })
            }

            #(if settings_errors.is_empty() {
                None
            } else {
                Some(element! {
                    View(margin_top: 1u32, margin_bottom: 1u32, flex_direction: FlexDirection::Column) {
                        Text(content: "Invalid Settings".to_string(), weight: Weight::Bold)
                        ValidationErrorsList(errors: settings_errors)
                    }
                })
            })

            View(margin_top: 1u32, flex_direction: FlexDirection::Column) {
                Text(content: "Updates".to_string(), weight: Weight::Bold)
                Text(content: format!("└ Auto-updates: {}", if diagnostic.package_manager.is_some() { "Managed by package manager" } else { diagnostic.auto_updates.as_str() }))
                #(diagnostic.has_update_permissions.map(|has_permissions| element! {
                    Text(content: format!("└ Update permissions: {}", if has_permissions { "Yes" } else { "No (requires sudo)" }))
                }))
                Text(content: format!("└ Auto-update channel: {auto_update_channel}"))
                Text(content: "└ Failed to fetch versions".to_string(), dim: true)
            }

            SandboxDoctorSection()

            McpParsingWarnings()

            KeybindingWarnings(
                enabled: keybinding_customization_enabled,
                keybindings_path: keybindings_path,
                warnings: keybinding_warnings,
            )

            #(if env_warnings.is_empty() {
                None
            } else {
                Some(element! {
                    View(margin_top: 1u32, flex_direction: FlexDirection::Column) {
                        Text(content: "Environment Variables".to_string(), weight: Weight::Bold)
                        #(env_warnings.into_iter().map(|warning| {
                            let color = if warning.result.status == EnvVarValidationStatus::Capped { Some(theme.warning) } else { Some(theme.error) };
                            let message = warning.result.message.unwrap_or_default();
                            element! {
                                View(flex_direction: FlexDirection::Row) {
                                    Text(content: format!("└ {}: ", warning.name), wrap: TextWrap::NoWrap)
                                    Text(content: message, color: color, wrap: TextWrap::NoWrap)
                                }
                            }
                        }))
                    }
                }.into_any())
            })

            #(if version_lock_info.enabled {
                Some(element! {
                    View(margin_top: 1u32, flex_direction: FlexDirection::Column) {
                        Text(content: "Version Locks".to_string(), weight: Weight::Bold)
                        #(if version_lock_info.stale_locks_cleaned > 0 {
                            Some(element! {
                                Text(content: format!("└ Cleaned {} stale lock(s)", version_lock_info.stale_locks_cleaned), color: theme.inactive)
                            })
                        } else {
                            None
                        })
                        #(if version_lock_info.locks.is_empty() {
                            Some(element! {
                                Text(content: "└ No active version locks".to_string(), color: theme.inactive)
                            }.into_any())
                        } else {
                            Some(element! {
                                View(flex_direction: FlexDirection::Column) {
                                    #(version_lock_info.locks.into_iter().map(|lock| {
                                        let status = if lock.is_process_running { "(running)" } else { "(stale)" };
                                        let status_color = if lock.is_process_running { None } else { Some(theme.warning) };
                                        element! {
                                            View(flex_direction: FlexDirection::Row) {
                                                Text(content: format!("└ {}: PID {} ", lock.version, lock.pid), wrap: TextWrap::NoWrap)
                                                Text(content: status.to_string(), color: status_color, wrap: TextWrap::NoWrap)
                                            }
                                        }
                                    }))
                                }
                            }.into_any())
                        })
                    }
                })
            } else {
                None
            })

            #(if agent_parse_errors.is_empty() {
                None
            } else {
                Some(element! {
                    View(margin_top: 1u32, flex_direction: FlexDirection::Column) {
                        Text(content: "Agent Parse Errors".to_string(), weight: Weight::Bold, color: theme.error)
                        Text(content: format!("└ Failed to parse {} agent file(s):", agent_parse_errors.len()), color: theme.error)
                        #(agent_parse_errors.into_iter().map(|file| element! {
                            Text(content: format!("  └ {}: {}", file.path.display(), file.error), color: theme.inactive, wrap: TextWrap::Wrap)
                        }))
                    }
                })
            })

            #(if plugin_errors.is_empty() {
                None
            } else {
                Some(element! {
                    View(margin_top: 1u32, flex_direction: FlexDirection::Column) {
                        Text(content: "Plugin Errors".to_string(), weight: Weight::Bold, color: theme.error)
                        Text(content: format!("└ {} plugin error(s) detected:", plugin_errors.len()), color: theme.error)
                        #(plugin_errors.into_iter().map(|error| {
                            let source = if error.source().trim().is_empty() { "unknown".to_string() } else { error.source().to_owned() };
                            let message = get_plugin_error_message(&error);
                            element! {
                                Text(content: format!("  └ {source}: {message}"), color: theme.inactive, wrap: TextWrap::Wrap)
                            }
                        }))
                    }
                })
            })

            #(unreachable_rules_warning.map(|warning| {
                let symbol = warning_symbol.clone();
                element! {
                    View(margin_top: 1u32, flex_direction: FlexDirection::Column) {
                        Text(content: "Unreachable Permission Rules".to_string(), weight: Weight::Bold, color: theme.warning)
                        View(flex_direction: FlexDirection::Row) {
                            Text(content: "└ ".to_string(), wrap: TextWrap::NoWrap)
                            Text(content: format!("{symbol} {}", warning.message), color: theme.warning, wrap: TextWrap::Wrap)
                        }
                        #(warning.details.into_iter().map(|detail| element! {
                            Text(content: format!("  └ {detail}"), color: theme.inactive, wrap: TextWrap::Wrap)
                        }))
                    }
                }
            }))

            #(if has_context_usage_warnings {
                Some(element! {
                    View(margin_top: 1u32, flex_direction: FlexDirection::Column) {
                        Text(content: "Context Usage Warnings".to_string(), weight: Weight::Bold)
                        #(claude_md_warning.clone().map(|warning| {
                            let symbol = warning_symbol.clone();
                            element! {
                                View(flex_direction: FlexDirection::Column) {
                                    View(flex_direction: FlexDirection::Row) {
                                        Text(content: "└ ".to_string(), wrap: TextWrap::NoWrap)
                                        Text(content: format!("{symbol} {}", warning.message), color: theme.warning, wrap: TextWrap::Wrap)
                                    }
                                    Text(content: "  └ Files:".to_string())
                                    #(warning.details.into_iter().map(|detail| element! {
                                        Text(content: format!("    └ {detail}"), color: theme.inactive, wrap: TextWrap::Wrap)
                                    }))
                                }
                            }
                        }))
                        #(agent_warning.clone().map(|warning| {
                            let symbol = warning_symbol.clone();
                            element! {
                                View(flex_direction: FlexDirection::Column) {
                                    View(flex_direction: FlexDirection::Row) {
                                        Text(content: "└ ".to_string(), wrap: TextWrap::NoWrap)
                                        Text(content: format!("{symbol} {}", warning.message), color: theme.warning, wrap: TextWrap::Wrap)
                                    }
                                    Text(content: "  └ Top contributors:".to_string())
                                    #(warning.details.into_iter().map(|detail| element! {
                                        Text(content: format!("    └ {detail}"), color: theme.inactive, wrap: TextWrap::Wrap)
                                    }))
                                }
                            }
                        }))
                        #(mcp_warning.clone().map(|warning| {
                            let symbol = warning_symbol.clone();
                            element! {
                                View(flex_direction: FlexDirection::Column) {
                                    View(flex_direction: FlexDirection::Row) {
                                        Text(content: "└ ".to_string(), wrap: TextWrap::NoWrap)
                                        Text(content: format!("{symbol} {}", warning.message), color: theme.warning, wrap: TextWrap::Wrap)
                                    }
                                    Text(content: "  └ MCP servers:".to_string())
                                    #(warning.details.into_iter().map(|detail| element! {
                                        Text(content: format!("    └ {detail}"), color: theme.inactive, wrap: TextWrap::Wrap)
                                    }))
                                }
                            }
                        }))
                    }
                })
            } else {
                None
            })

            View(margin_top: 1u32, flex_direction: FlexDirection::Row) {
                PressEnterToContinue()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::doctor_context_warnings::{
        ContextWarning, ContextWarningSeverity, ContextWarningType,
    };
    use crate::utils::theme;
    use futures::{StreamExt, stream};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn key(code: KeyCode) -> TerminalEvent {
        TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code))
    }

    fn canvas_lines(canvas: &Canvas) -> Vec<String> {
        (0..canvas.height())
            .map(|y| {
                let mut line = String::new();
                for x in 0..canvas.width() {
                    if let Some(text) = canvas.cell(x, y).and_then(|cell| cell.text()) {
                        line.push_str(text);
                    } else {
                        line.push(' ');
                    }
                }
                line.trim_end().to_string()
            })
            .collect()
    }

    fn render_doctor_canvas_after(events: Vec<TerminalEvent>) -> (Canvas, usize) {
        let close_count = Arc::new(Mutex::new(0usize));
        let close_for_handler = Arc::clone(&close_count);
        let current_theme = *theme::current();

        let canvases = futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(
                    crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings()
                )) {
                    ContextProvider(value: Context::owned(current_theme)) {
                        Doctor(
                        on_close: move |_| {
                            *close_for_handler.lock().expect("close mutex") += 1;
                        },
                        )
                    }
                }
            };
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(stream::iter(events)).with_size(120, 36),
            ));
            let mut canvases = Vec::new();
            loop {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(100)).await;
                    None
                })
                .await;
                let Some(canvas) = next else {
                    break;
                };
                canvases.push(canvas);
                if canvases.len() >= 12 {
                    break;
                }
            }
            canvases
        });

        let canvas = canvases
            .last()
            .expect("mock render should produce a canvas")
            .clone();
        let close_count = *close_count.lock().expect("close mutex");
        (canvas, close_count)
    }

    #[test]
    fn doctor_panel_renders_official_diagnostics_and_updates_shape() {
        let (canvas, close_count) = render_doctor_canvas_after(Vec::new());
        let text = canvas_lines(&canvas).join("\n");

        assert_eq!(close_count, 0);
        assert!(text.contains("Diagnostics"), "canvas=\n{text}");
        assert!(text.contains("└ Currently running:"), "canvas=\n{text}");
        assert!(text.contains("└ Path:"), "canvas=\n{text}");
        assert!(text.contains("└ Invoked:"), "canvas=\n{text}");
        assert!(text.contains("└ Config install method:"), "canvas=\n{text}");
        assert!(text.contains("└ Search:"), "canvas=\n{text}");
        assert!(text.contains("Updates"), "canvas=\n{text}");
        assert!(text.contains("└ Auto-updates:"), "canvas=\n{text}");
        assert!(text.contains("└ Auto-update channel:"), "canvas=\n{text}");
        assert!(text.contains("Press Enter to continue…"), "canvas=\n{text}");
    }

    #[test]
    fn doctor_panel_enter_and_esc_dismiss_like_official_confirmation_keys() {
        let (_canvas, enter_close_count) = render_doctor_canvas_after(vec![key(KeyCode::Enter)]);
        assert_eq!(enter_close_count, 1);

        let (_canvas, esc_close_count) = render_doctor_canvas_after(vec![key(KeyCode::Esc)]);
        assert_eq!(esc_close_count, 1);
    }

    #[test]
    fn doctor_renders_keybinding_warnings_at_official_boundary() {
        let current_theme = *theme::current();
        let canvas = element! {
            ContextProvider(value: Context::owned(current_theme)) {
                Doctor(
                    keybinding_customization_enabled: true,
                    keybindings_path: "/repo/.claude/keybindings.json".to_string(),
                    keybinding_warnings: vec![KeybindingWarningItem::error(
                        "Unknown action custom:missing",
                        Some("Remove or rename the binding"),
                    )],
                )
            }
        }
        .render(Some(120));
        let text = canvas_lines(&canvas).join("\n");

        assert!(
            text.contains("Keybinding Configuration Issues"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("/repo/.claude/keybindings.json"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("Unknown action custom:missing"),
            "canvas=\n{text}"
        );
    }

    #[test]
    fn doctor_renders_invalid_settings_section_at_official_boundary() {
        let current_theme = *theme::current();
        let canvas = element! {
            ContextProvider(value: Context::owned(current_theme)) {
                Doctor(
                    settings_errors: vec![ValidationError {
                        file: Some("settings.json".to_string()),
                        path: "model".to_string(),
                        message: "Invalid model".to_string(),
                        expected: None,
                        invalid_value: None,
                        doc_link: None,
                        suggestion: Some("Use a supported model".to_string()),
                    }],
                )
            }
        }
        .render(Some(120));
        let text = canvas_lines(&canvas).join("\n");

        assert!(text.contains("Invalid Settings"), "canvas=\n{text}");
        assert!(text.contains("settings.json"), "canvas=\n{text}");
        assert!(text.contains("Invalid model"), "canvas=\n{text}");
        assert!(text.contains("Use a supported model"), "canvas=\n{text}");
    }

    #[test]
    fn doctor_renders_version_locks_section_at_official_boundary() {
        let current_theme = *theme::current();
        let canvas = element! {
            ContextProvider(value: Context::owned(current_theme)) {
                Doctor(version_lock_info: Some(VersionLockInfo {
                    enabled: true,
                    stale_locks_cleaned: 0,
                    locks_dir: "/state/claude/locks".to_string(),
                    locks: vec![LockInfo {
                        version: "2.1.159".to_string(),
                        pid: 4242,
                        is_process_running: false,
                        exec_path: "/usr/local/bin/claude".to_string(),
                        acquired_at: 1234,
                        lock_file_path: std::path::PathBuf::from("/state/claude/locks/2.1.159.lock"),
                    }],
                }))
            }
        }
        .render(Some(140));
        let text = canvas_lines(&canvas).join("\n");

        assert!(text.contains("Version Locks"), "canvas=\n{text}");
        assert!(
            text.contains("└ 2.1.159: PID 4242 (stale)"),
            "canvas=\n{text}"
        );
    }

    #[test]
    fn doctor_renders_agent_and_plugin_error_sections_at_official_boundary() {
        let current_theme = *theme::current();
        let canvas = element! {
            ContextProvider(value: Context::owned(current_theme)) {
                Doctor(
                    agent_parse_errors: vec![FailedAgentFile {
                        path: std::path::PathBuf::from("/repo/.claude/agents/bad.md"),
                        error: "missing required description".to_string(),
                    }],
                    plugin_errors: vec![PluginError::GenericError {
                        source: "github:owner/plugin".to_string(),
                        plugin: None,
                        error: "failed to parse plugin manifest".to_string(),
                    }],
                )
            }
        }
        .render(Some(140));
        let text = canvas_lines(&canvas).join("\n");

        assert!(text.contains("Agent Parse Errors"), "canvas=\n{text}");
        assert!(
            text.contains("└ Failed to parse 1 agent file(s):"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("/repo/.claude/agents/bad.md: missing required description"),
            "canvas=\n{text}"
        );
        assert!(text.contains("Plugin Errors"), "canvas=\n{text}");
        assert!(
            text.contains("└ 1 plugin error(s) detected:"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("github:owner/plugin: failed to parse plugin manifest"),
            "canvas=\n{text}"
        );
    }

    #[test]
    fn doctor_renders_context_warning_sections_at_official_boundary() {
        let current_theme = *theme::current();
        let context_warnings = ContextWarnings {
            unreachable_rules_warning: Some(ContextWarning {
                warning_type: ContextWarningType::UnreachableRules,
                severity: ContextWarningSeverity::Warning,
                message: "1 unreachable permission rule detected".to_string(),
                details: vec![
                    "Bash(cargo test:*): Shadowed by \"Bash\" ask rule".to_string(),
                    "  Fix: Remove the ask rule".to_string(),
                ],
                current_value: 1,
                threshold: 0,
            }),
            claude_md_warning: Some(ContextWarning {
                warning_type: ContextWarningType::ClaudeMdFiles,
                severity: ContextWarningSeverity::Warning,
                message: "Large CLAUDE.md file detected".to_string(),
                details: vec!["CLAUDE.md: 40,001 chars".to_string()],
                current_value: 1,
                threshold: 40_000,
            }),
            agent_warning: Some(ContextWarning {
                warning_type: ContextWarningType::AgentDescriptions,
                severity: ContextWarningSeverity::Warning,
                message: "Large agent descriptions".to_string(),
                details: vec!["reviewer: ~20,000 tokens".to_string()],
                current_value: 20_000,
                threshold: 15_000,
            }),
            mcp_warning: Some(ContextWarning {
                warning_type: ContextWarningType::McpTools,
                severity: ContextWarningSeverity::Warning,
                message: "Large MCP tools context".to_string(),
                details: vec!["github: 2 tools (~30,000 tokens)".to_string()],
                current_value: 30_000,
                threshold: 25_000,
            }),
        };

        let canvas = element! {
            ContextProvider(value: Context::owned(current_theme)) {
                Doctor(context_warnings: Some(context_warnings))
            }
        }
        .render(Some(140));
        let text = canvas_lines(&canvas).join("\n");

        assert!(
            text.contains("Unreachable Permission Rules"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("1 unreachable permission rule detected"),
            "canvas=\n{text}"
        );
        assert!(text.contains("Context Usage Warnings"), "canvas=\n{text}");
        assert!(text.contains("Files:"), "canvas=\n{text}");
        assert!(text.contains("Top contributors:"), "canvas=\n{text}");
        assert!(text.contains("MCP servers:"), "canvas=\n{text}");
        assert!(text.contains("CLAUDE.md: 40,001 chars"), "canvas=\n{text}");
        assert!(text.contains("reviewer: ~20,000 tokens"), "canvas=\n{text}");
        assert!(
            text.contains("github: 2 tools (~30,000 tokens)"),
            "canvas=\n{text}"
        );
    }

    #[test]
    fn doctor_env_validation_warnings_match_official_bounded_integer_shape() {
        let invalid = doctor_validate_env_value(
            "BASH_MAX_OUTPUT_LENGTH",
            Some("not-a-number"),
            BASH_MAX_OUTPUT_DEFAULT,
            BASH_MAX_OUTPUT_UPPER_LIMIT,
        )
        .expect("invalid env value warning");
        let capped = doctor_validate_env_value(
            "TASK_MAX_OUTPUT_LENGTH",
            Some("200000"),
            TASK_MAX_OUTPUT_DEFAULT,
            TASK_MAX_OUTPUT_UPPER_LIMIT,
        )
        .expect("capped env value warning");
        let valid_prefix = doctor_validate_env_value(
            "BASH_MAX_OUTPUT_LENGTH",
            Some("42suffix"),
            BASH_MAX_OUTPUT_DEFAULT,
            BASH_MAX_OUTPUT_UPPER_LIMIT,
        );

        assert_eq!(invalid.name, "BASH_MAX_OUTPUT_LENGTH");
        assert_eq!(invalid.result.status, EnvVarValidationStatus::Invalid);
        assert_eq!(
            invalid.result.message.as_deref(),
            Some("Invalid value \"not-a-number\" (using default: 30000)")
        );
        assert_eq!(capped.name, "TASK_MAX_OUTPUT_LENGTH");
        assert_eq!(capped.result.status, EnvVarValidationStatus::Capped);
        assert_eq!(
            capped.result.message.as_deref(),
            Some("Capped from 200000 to 160000")
        );
        assert!(valid_prefix.is_none());
    }
}
