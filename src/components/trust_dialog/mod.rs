//! Maps to: CC `components/TrustDialog/TrustDialog.tsx`.
//!
//! Workspace trust approval dialog. Official accept writes project/session trust
//! and official reject exits the process. Nyan preserves the dialog boundary,
//! copy, option values, source/risk calculation, and callbacks, but leaves
//! trust persistence/process exit to the caller for this safe UI slice.

pub mod utils;

use crate::components::custom_select::{Select, SelectLayout, SelectOptionData};
use crate::components::permissions::permission_dialog::PermissionDialog;
use crate::utils::theme::Theme;
use iocraft::prelude::*;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrustDialogRiskSnapshot {
    pub cwd: String,
    pub is_home_dir: bool,
    pub has_trust_dialog_accepted: bool,
    pub has_mcp_servers: bool,
    pub has_hooks: bool,
    pub has_bash_execution: bool,
    pub has_slash_command_bash: bool,
    pub has_skills_bash: bool,
    pub has_api_key_helper: bool,
    pub has_aws_commands: bool,
    pub has_gcp_commands: bool,
    pub has_otel_headers_helper: bool,
    pub has_dangerous_env_vars: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrustDialogCommandSnapshot {
    pub command_type: String,
    pub loaded_from: String,
    pub source: String,
    pub allowed_tools: Vec<String>,
}

fn command_allows_bash(command: &TrustDialogCommandSnapshot) -> bool {
    command.allowed_tools.iter().any(|tool| {
        tool == crate::tools::bash_tool::tool_name::BASH_TOOL_NAME || tool.starts_with("Bash(")
    })
}

/// Maps to: CC `TrustDialog.tsx#hasSlashCommandBash` calculation.
pub fn has_slash_command_bash(commands: &[TrustDialogCommandSnapshot]) -> bool {
    commands.iter().any(|command| {
        command.command_type == "prompt"
            && command.loaded_from == "commands_DEPRECATED"
            && matches!(command.source.as_str(), "projectSettings" | "localSettings")
            && command_allows_bash(command)
    })
}

/// Maps to: CC `TrustDialog.tsx#hasSkillsBash` calculation.
pub fn has_skills_bash(commands: &[TrustDialogCommandSnapshot]) -> bool {
    commands.iter().any(|command| {
        command.command_type == "prompt"
            && matches!(command.loaded_from.as_str(), "skills" | "plugin")
            && matches!(
                command.source.as_str(),
                "projectSettings" | "localSettings" | "plugin"
            )
            && command_allows_bash(command)
    })
}

/// Maps to: CC `TrustDialog.tsx#hasAnyBashExecution` calculation.
pub fn has_any_bash_execution(
    bash_setting_sources: &[String],
    commands: &[TrustDialogCommandSnapshot],
) -> bool {
    !bash_setting_sources.is_empty()
        || has_slash_command_bash(commands)
        || has_skills_bash(commands)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrustDialogChoice {
    EnableAll,
    Exit,
}

impl TrustDialogChoice {
    fn value(self) -> &'static str {
        match self {
            Self::EnableAll => "enable_all",
            Self::Exit => "exit",
        }
    }
}

fn choice_from_value(value: &str) -> TrustDialogChoice {
    match value {
        "enable_all" => TrustDialogChoice::EnableAll,
        _ => TrustDialogChoice::Exit,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrustDialogAcceptResult {
    pub is_home_dir: bool,
    pub would_set_session_trust_accepted: bool,
    pub would_persist_project_trust: bool,
    pub risk_snapshot: TrustDialogRiskSnapshot,
}

/// Maps to: CC `TrustDialog.tsx` `Select` options.
pub fn trust_dialog_options() -> Vec<SelectOptionData> {
    vec![
        SelectOptionData {
            label: "Yes, I trust this folder".to_string(),
            value: TrustDialogChoice::EnableAll.value().to_string(),
            ..SelectOptionData::default()
        },
        SelectOptionData {
            label: "No, exit".to_string(),
            value: TrustDialogChoice::Exit.value().to_string(),
            ..SelectOptionData::default()
        },
    ]
}

#[derive(Default, Props)]
pub struct TrustDialogProps<'a> {
    pub snapshot: TrustDialogRiskSnapshot,
    pub focused_index: usize,
    pub exit_pending: bool,
    pub exit_key_name: Option<String>,
    pub on_done: HandlerMut<'a, TrustDialogAcceptResult>,
    /// Safe Nyan hook for the official `gracefulShutdownSync(...)` branch.
    pub on_reject: HandlerMut<'a, ()>,
}

/// Maps to: CC `TrustDialog.tsx#TrustDialog`.
#[component]
pub fn TrustDialog<'a>(
    props: &mut TrustDialogProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let options = trust_dialog_options();
    let option_count = options.len().max(1);
    let mut focused_index = hooks.use_state(|| props.focused_index.min(option_count - 1));
    let mut pending_choice = hooks.use_state(|| Option::<TrustDialogChoice>::None);

    if props.snapshot.has_trust_dialog_accepted {
        (props.on_done)(TrustDialogAcceptResult {
            is_home_dir: props.snapshot.is_home_dir,
            would_set_session_trust_accepted: false,
            would_persist_project_trust: false,
            risk_snapshot: props.snapshot.clone(),
        });
        return element! { View(width: 0u32, height: 0u32) }.into_any();
    }

    hooks.use_terminal_events({
        let mut focused_index = focused_index;
        let mut pending_choice = pending_choice;
        let options = options.clone();
        move |event| {
            let TerminalEvent::Key(KeyEvent { code, kind, .. }) = event else {
                return;
            };
            if kind == KeyEventKind::Release {
                return;
            }
            match code {
                KeyCode::Up | KeyCode::Char('k') => {
                    focused_index.set(focused_index.get().saturating_sub(1));
                }
                KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                    focused_index
                        .set((focused_index.get() + 1).min(options.len().saturating_sub(1)));
                }
                KeyCode::Enter => {
                    if let Some(option) = options.get(focused_index.get()) {
                        pending_choice.set(Some(choice_from_value(&option.value)));
                    }
                }
                KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                    pending_choice.set(Some(TrustDialogChoice::Exit));
                }
                _ => {}
            }
        }
    });

    let pending = { *pending_choice.read() };
    if let Some(choice) = pending {
        pending_choice.set(None);
        match choice {
            TrustDialogChoice::EnableAll => {
                let is_home_dir = props.snapshot.is_home_dir;
                (props.on_done)(TrustDialogAcceptResult {
                    is_home_dir,
                    would_set_session_trust_accepted: is_home_dir,
                    would_persist_project_trust: !is_home_dir,
                    risk_snapshot: props.snapshot.clone(),
                });
            }
            TrustDialogChoice::Exit => {
                // Official path: `gracefulShutdownSync(...)`.
                // Safe Nyan path: no process exit from the component.
                (props.on_reject)(());
            }
        }
    }

    let footer = if props.exit_pending {
        format!(
            "Press {} again to exit",
            props
                .exit_key_name
                .as_deref()
                .filter(|name| !name.is_empty())
                .unwrap_or("Ctrl-C")
        )
    } else {
        "Enter to confirm · Esc to cancel".to_string()
    };

    element! {
        PermissionDialog(
            color: Some(theme.warning),
            title_color: Some(theme.warning),
            title: "Accessing workspace:".to_string(),
        ) {
            View(flex_direction: FlexDirection::Column, row_gap: 1u32, padding_top: 1u32) {
                Text(content: props.snapshot.cwd.clone(), weight: Weight::Bold, wrap: TextWrap::NoWrap)
                Text(
                    content: "Quick safety check: Is this a project you created or one you trust?\n(Like your own code, a well-known open source project, or work from\nyour team). If not, take a moment to review what's in this folder\nfirst.".to_string(),
                    wrap: TextWrap::Wrap,
                )
                Text(
                    content: "nyan'll be able to read, edit, and execute files here.".to_string(),
                    wrap: TextWrap::Wrap,
                )
                Text(
                    content: "Security guide: https://code.claude.com/docs/en/security".to_string(),
                    color: theme.inactive,
                    wrap: TextWrap::NoWrap,
                )
                Select(
                    options: options,
                    focused_index: focused_index.get().min(option_count - 1),
                    visible_option_count: option_count,
                    layout: SelectLayout::CompactVertical,
                    hide_indexes: true,
                )
                Text(content: footer, color: theme.inactive, wrap: TextWrap::NoWrap)
            }
        }
    }.into_any()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;
    use futures::{StreamExt, stream};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn key(code: KeyCode) -> TerminalEvent {
        TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code))
    }

    fn snapshot() -> TrustDialogRiskSnapshot {
        TrustDialogRiskSnapshot {
            cwd: "/repo".to_string(),
            has_mcp_servers: true,
            has_hooks: true,
            has_bash_execution: true,
            ..TrustDialogRiskSnapshot::default()
        }
    }

    #[test]
    fn trust_dialog_bash_execution_detection_matches_official_command_filters() {
        let slash = TrustDialogCommandSnapshot {
            command_type: "prompt".to_string(),
            loaded_from: "commands_DEPRECATED".to_string(),
            source: "projectSettings".to_string(),
            allowed_tools: vec!["Bash(git status)".to_string()],
        };
        let skill = TrustDialogCommandSnapshot {
            command_type: "prompt".to_string(),
            loaded_from: "skills".to_string(),
            source: "plugin".to_string(),
            allowed_tools: vec!["Bash".to_string()],
        };
        let ignored = TrustDialogCommandSnapshot {
            command_type: "prompt".to_string(),
            loaded_from: "commands_DEPRECATED".to_string(),
            source: "userSettings".to_string(),
            allowed_tools: vec!["Bash(rm -rf /)".to_string()],
        };

        assert!(has_slash_command_bash(std::slice::from_ref(&slash)));
        assert!(has_skills_bash(std::slice::from_ref(&skill)));
        assert!(!has_slash_command_bash(std::slice::from_ref(&ignored)));
        assert!(!has_skills_bash(&[ignored]));
        assert!(has_any_bash_execution(&[], &[slash]));
        assert!(has_any_bash_execution(
            &[".claude/settings.json".to_string()],
            &[]
        ));
    }

    #[test]
    fn trust_dialog_renders_official_workspace_copy_options_and_footer() {
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                TrustDialog(snapshot: snapshot())
            }
        }
        .render(Some(120))
        .to_string();

        assert!(text.contains("Accessing workspace:"), "canvas=\n{text}");
        assert!(text.contains("/repo"), "canvas=\n{text}");
        assert!(
            text.contains("Quick safety check: Is this a project you created"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("nyan'll be able to read, edit, and execute files here."),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("https://code.claude.com/docs/en/security"),
            "canvas=\n{text}"
        );
        assert!(text.contains("Yes, I trust this folder"), "canvas=\n{text}");
        assert!(text.contains("No, exit"), "canvas=\n{text}");
        assert!(
            text.contains("Enter to confirm · Esc to cancel"),
            "canvas=\n{text}"
        );
    }

    #[test]
    fn trust_dialog_accept_reports_home_vs_project_write_intent_without_writing() {
        let results = Arc::new(Mutex::new(Vec::<TrustDialogAcceptResult>::new()));
        let results_for_handler = Arc::clone(&results);
        let mut data = snapshot();
        data.is_home_dir = true;

        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(*theme::current())) {
                    TrustDialog(
                        snapshot: data,
                        on_done: move |result| results_for_handler.lock().expect("results mutex").push(result),
                    )
                }
            };
            let mut render_loop = Box::pin(
                app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(stream::iter(vec![key(KeyCode::Enter)]))
                        .with_size(120, 30),
                ),
            );
            for _ in 0..8 {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(100)).await;
                    None
                })
                .await;
                if next.is_none() {
                    break;
                }
            }
        });

        let results = results.lock().expect("results mutex");
        assert_eq!(results.len(), 1);
        assert!(results[0].would_set_session_trust_accepted);
        assert!(!results[0].would_persist_project_trust);
        assert!(!crate::utils::session_storage::is_session_write_enabled());
    }
}
