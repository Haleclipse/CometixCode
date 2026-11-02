//! Maps to: CC `components/ManagedSettingsSecurityDialog/ManagedSettingsSecurityDialog.tsx`.
//!
//! The official component asks the user to approve organization-managed
//! settings that can execute code or intercept traffic. Nyan preserves the
//! component boundary, copy, dangerous-setting list, and accept/reject callback
//! semantics; it does not exit the process from this UI boundary.

pub mod utils;

use crate::components::custom_select::{Select, SelectLayout, SelectOptionData};
use crate::components::permissions::permission_dialog::PermissionDialog;
use crate::utils::settings::SettingsJson;
use crate::utils::theme::Theme;
use iocraft::prelude::*;
use utils::{extract_dangerous_settings, format_dangerous_settings_list};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManagedSettingsSecurityChoice {
    Accept,
    Exit,
}

impl ManagedSettingsSecurityChoice {
    fn value(self) -> &'static str {
        match self {
            Self::Accept => "accept",
            Self::Exit => "exit",
        }
    }
}

fn choice_from_value(value: &str) -> ManagedSettingsSecurityChoice {
    match value {
        "accept" => ManagedSettingsSecurityChoice::Accept,
        _ => ManagedSettingsSecurityChoice::Exit,
    }
}

/// Maps to: CC `ManagedSettingsSecurityDialog.tsx` `Select` options.
pub fn managed_settings_security_options() -> Vec<SelectOptionData> {
    vec![
        SelectOptionData {
            label: "Yes, I trust these settings".to_string(),
            value: ManagedSettingsSecurityChoice::Accept.value().to_string(),
            ..SelectOptionData::default()
        },
        SelectOptionData {
            label: "No, exit nyan".to_string(),
            value: ManagedSettingsSecurityChoice::Exit.value().to_string(),
            ..SelectOptionData::default()
        },
    ]
}

#[derive(Default, Props)]
pub struct ManagedSettingsSecurityDialogProps<'a> {
    pub settings: SettingsJson,
    pub focused_index: usize,
    pub exit_pending: bool,
    pub exit_key_name: Option<String>,
    pub on_accept: HandlerMut<'a, ()>,
    pub on_reject: HandlerMut<'a, ()>,
}

/// Maps to: CC `ManagedSettingsSecurityDialog.tsx#ManagedSettingsSecurityDialog`.
#[component]
pub fn ManagedSettingsSecurityDialog<'a>(
    props: &mut ManagedSettingsSecurityDialogProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let dangerous = extract_dangerous_settings(Some(&props.settings));
    let settings_list = format_dangerous_settings_list(&dangerous);
    let options = managed_settings_security_options();
    let option_count = options.len().max(1);
    let mut focused_index = hooks.use_state(|| props.focused_index.min(option_count - 1));
    let mut pending_choice = hooks.use_state(|| Option::<ManagedSettingsSecurityChoice>::None);

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
                    pending_choice.set(Some(ManagedSettingsSecurityChoice::Exit));
                }
                _ => {}
            }
        }
    });

    let pending = { *pending_choice.read() };
    if let Some(choice) = pending {
        pending_choice.set(None);
        match choice {
            ManagedSettingsSecurityChoice::Accept => (props.on_accept)(()),
            ManagedSettingsSecurityChoice::Exit => (props.on_reject)(()),
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
        "Enter to confirm · Esc to exit".to_string()
    };

    element! {
        PermissionDialog(
            color: Some(theme.warning),
            title_color: Some(theme.warning),
            title: "Managed settings require approval".to_string(),
        ) {
            View(flex_direction: FlexDirection::Column, row_gap: 1u32, padding_top: 1u32) {
                Text(
                    content: "Your organization has configured managed settings that could allow\nexecution of arbitrary code or interception of your prompts and\nresponses.".to_string(),
                    wrap: TextWrap::Wrap,
                )
                View(flex_direction: FlexDirection::Column) {
                    Text(content: "Settings requiring approval:".to_string(), color: theme.inactive, wrap: TextWrap::NoWrap)
                    #(settings_list.into_iter().map(|item| element! {
                        View(padding_left: 2u32, flex_direction: FlexDirection::Row) {
                            Text(content: "· ".to_string(), color: theme.inactive, wrap: TextWrap::NoWrap)
                            Text(content: item, wrap: TextWrap::NoWrap)
                        }
                    }))
                }
                Text(
                    content: "Only accept if you trust your organization's IT administration\nand expect these settings to be configured.".to_string(),
                    wrap: TextWrap::Wrap,
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
    }
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

    #[test]
    fn managed_settings_security_dialog_renders_official_copy_options_and_list() {
        let settings = SettingsJson {
            api_key_helper: Some("helper".to_string()),
            env: Some(std::sync::Arc::new(indexmap::IndexMap::from([(
                "ANTHROPIC_BASE_URL".to_string(),
                "https://proxy".to_string(),
            )]))),
            hooks: Some(serde_json::json!({ "PreToolUse": [{ "command": "echo" }] })),
            ..SettingsJson::default()
        };

        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                ManagedSettingsSecurityDialog(settings: settings)
            }
        }
        .render(Some(120))
        .to_string();

        assert!(
            text.contains("Managed settings require approval"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("Your organization has configured managed settings"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("Settings requiring approval:"),
            "canvas=\n{text}"
        );
        assert!(text.contains("apiKeyHelper"), "canvas=\n{text}");
        assert!(text.contains("ANTHROPIC_BASE_URL"), "canvas=\n{text}");
        assert!(text.contains("hooks"), "canvas=\n{text}");
        assert!(
            text.contains("Yes, I trust these settings"),
            "canvas=\n{text}"
        );
        assert!(text.contains("No, exit nyan"), "canvas=\n{text}");
        assert!(
            text.contains("Enter to confirm · Esc to exit"),
            "canvas=\n{text}"
        );
    }

    #[test]
    fn managed_settings_security_dialog_accept_and_reject_callbacks_match_select_values() {
        let accepted = Arc::new(Mutex::new(0usize));
        let rejected = Arc::new(Mutex::new(0usize));
        let accepted_for_handler = Arc::clone(&accepted);
        let rejected_for_handler = Arc::clone(&rejected);

        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(*theme::current())) {
                    ManagedSettingsSecurityDialog(
                        settings: SettingsJson::default(),
                        on_accept: move |_| *accepted_for_handler.lock().expect("accepted mutex") += 1,
                        on_reject: move |_| *rejected_for_handler.lock().expect("rejected mutex") += 1,
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

        assert_eq!(*accepted.lock().expect("accepted mutex"), 1);
        assert_eq!(*rejected.lock().expect("rejected mutex"), 0);
    }
}
