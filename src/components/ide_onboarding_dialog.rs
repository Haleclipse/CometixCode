//! Maps to: CC `components/IdeOnboardingDialog.tsx`.
//!
//! Safety boundary: official `markDialogAsShown()` writes
//! `hasIdeOnboardingBeenShown[terminal] = true` while rendering. Nyan keeps
//! the visible onboarding UI and emits a one-shot `on_mark_shown(terminal)`
//! callback for future runtime wiring; it does not write global config.

use crate::components::design_system::dialog::Dialog;
use crate::utils::ide::{
    IDEExtensionInstallationStatus, ide_mention_shortcut, is_jetbrains_ide, to_ide_display_name,
};
use crate::utils::theme::Theme;
use iocraft::prelude::*;

#[derive(Default, Props)]
pub struct IdeOnboardingDialogProps<'a> {
    pub on_done: HandlerMut<'a, ()>,
    pub on_mark_shown: HandlerMut<'a, String>,
    pub installation_status: Option<IDEExtensionInstallationStatus>,
    /// Already-known terminal IDE type. Maps to official `getTerminalIdeType()`
    /// fallback without process/env probing from the component.
    pub terminal_ide_type: Option<String>,
    /// Runtime platform string (`darwin`, `linux`, `win32`) used only for the
    /// official mention shortcut copy.
    pub platform: Option<String>,
    /// Already-known terminal name used by `markDialogAsShown`; defaults to
    /// official `unknown` when absent.
    pub terminal: Option<String>,
}

/// Maps to: CC `IdeOnboardingDialog.tsx` IDE metadata derivation.
pub fn ide_onboarding_metadata(
    installation_status: Option<&IDEExtensionInstallationStatus>,
    terminal_ide_type: Option<&str>,
    platform: Option<&str>,
) -> (String, Option<String>, &'static str, &'static str) {
    let ide_type = installation_status
        .and_then(|status| status.ide_type.as_deref())
        .or(terminal_ide_type);
    let ide_name = to_ide_display_name(ide_type);
    let plugin_or_extension = if is_jetbrains_ide(ide_type) {
        "plugin"
    } else {
        "extension"
    };
    let subtitle = installation_status
        .and_then(|status| status.installed_version.as_deref())
        .map(|version| format!("installed {plugin_or_extension} v{version}"));
    (
        ide_name,
        subtitle,
        plugin_or_extension,
        ide_mention_shortcut(platform),
    )
}

/// Maps to: CC `components/IdeOnboardingDialog.tsx` `IdeOnboardingDialog`.
#[component]
pub fn IdeOnboardingDialog<'a>(
    props: &mut IdeOnboardingDialogProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let mut pending_done = hooks.use_state(|| false);
    let mut marked_shown = hooks.use_state(|| false);

    if !marked_shown.get() {
        marked_shown.set(true);
        (props.on_mark_shown)(
            props
                .terminal
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
        );
    }

    let runtime = hooks
        .try_use_context::<crate::keybindings::keybinding_context::KeybindingRuntime>()
        .map(|runtime| runtime.clone());
    crate::keybindings::use_keybinding::use_keybinding(
        &mut hooks,
        runtime,
        "confirm:yes",
        crate::keybindings::types::ContextName::Confirmation,
        || true,
        move || {
            pending_done.set(true);
            true
        },
    );

    if pending_done.get() {
        pending_done.set(false);
        (props.on_done)(());
    }

    let (ide_name, subtitle, _plugin_or_extension, mention_shortcut) = ide_onboarding_metadata(
        props.installation_status.as_ref(),
        props.terminal_ide_type.as_deref(),
        props.platform.as_deref(),
    );
    let title = format!("✻ Welcome to nyan for {ide_name}");

    element! {
        Fragment {
            Dialog(
                title: title,
                subtitle: subtitle,
                color: Some(theme.ide),
                hide_input_guide: true,
                on_cancel: move |_| pending_done.set(true),
            ) {
                View(flex_direction: FlexDirection::Column, gap: 1u32) {
                    View(flex_direction: FlexDirection::Row) {
                        Text(content: "• nyan has context of ".to_string(), wrap: TextWrap::NoWrap)
                        Text(content: "⧉ open files".to_string(), color: theme.suggestion, wrap: TextWrap::NoWrap)
                        Text(content: " and ".to_string(), wrap: TextWrap::NoWrap)
                        Text(content: "⧉ selected lines".to_string(), color: theme.suggestion, wrap: TextWrap::NoWrap)
                    }
                    View(flex_direction: FlexDirection::Row) {
                        Text(content: "• Review nyan's changes ".to_string(), wrap: TextWrap::NoWrap)
                        Text(content: "+11".to_string(), color: theme.diff_added_word, wrap: TextWrap::NoWrap)
                        Text(content: " ".to_string(), wrap: TextWrap::NoWrap)
                        Text(content: "-22".to_string(), color: theme.diff_removed_word, wrap: TextWrap::NoWrap)
                        Text(content: " in the comfort of your IDE".to_string(), wrap: TextWrap::NoWrap)
                    }
                    View(flex_direction: FlexDirection::Row) {
                        Text(content: "• Cmd+Esc".to_string(), wrap: TextWrap::NoWrap)
                        Text(content: " for Quick Launch".to_string(), dim: true, wrap: TextWrap::NoWrap)
                    }
                    View(flex_direction: FlexDirection::Row) {
                        Text(content: format!("• {mention_shortcut}"), wrap: TextWrap::NoWrap)
                        Text(content: " to reference files or lines in your input".to_string(), dim: true, wrap: TextWrap::NoWrap)
                    }
                }
            }
            View(padding_left: 1u32, padding_right: 1u32) {
                Text(
                    content: "Press Enter to continue".to_string(),
                    dim: true,
                    italic: true,
                    wrap: TextWrap::NoWrap,
                )
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
    fn ide_onboarding_dialog_renders_official_vscode_copy() {
        let status = IDEExtensionInstallationStatus {
            installed: true,
            error: None,
            installed_version: Some("1.2.3".to_string()),
            ide_type: Some("vscode".to_string()),
        };
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                IdeOnboardingDialog(
                    installation_status: Some(status),
                    platform: Some("darwin".to_string()),
                )
            }
        }
        .render(Some(160))
        .to_string();

        assert!(
            text.contains("✻ Welcome to nyan for VS Code"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("installed extension v1.2.3"),
            "canvas=\n{text}"
        );
        assert!(text.contains("• nyan has context of"), "canvas=\n{text}");
        assert!(text.contains("⧉ open files"), "canvas=\n{text}");
        assert!(text.contains("⧉ selected lines"), "canvas=\n{text}");
        assert!(
            text.contains("• Review nyan's changes +11 -22 in the comfort of your IDE"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("• Cmd+Esc for Quick Launch"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("• Cmd+Option+K to reference files or lines in your input"),
            "canvas=\n{text}"
        );
        assert!(text.contains("Press Enter to continue"), "canvas=\n{text}");
    }

    #[test]
    fn ide_onboarding_dialog_renders_jetbrains_plugin_subtitle_and_non_macos_shortcut() {
        let status = IDEExtensionInstallationStatus {
            installed: true,
            error: None,
            installed_version: Some("2026.1".to_string()),
            ide_type: Some("pycharm".to_string()),
        };
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                IdeOnboardingDialog(
                    installation_status: Some(status),
                    platform: Some("linux".to_string()),
                )
            }
        }
        .render(Some(160))
        .to_string();

        assert!(
            text.contains("Welcome to nyan for PyCharm"),
            "canvas=\n{text}"
        );
        assert!(text.contains("installed plugin v2026.1"), "canvas=\n{text}");
        assert!(
            text.contains("• Ctrl+Alt+K to reference files or lines in your input"),
            "canvas=\n{text}"
        );
    }

    #[test]
    fn ide_onboarding_metadata_falls_back_to_terminal_ide_type() {
        let (ide_name, subtitle, plugin_or_extension, shortcut) =
            ide_onboarding_metadata(None, Some("cursor"), Some("linux"));
        assert_eq!(ide_name, "Cursor");
        assert_eq!(subtitle, None);
        assert_eq!(plugin_or_extension, "extension");
        assert_eq!(shortcut, "Ctrl+Alt+K");
    }

    #[test]
    fn ide_onboarding_dialog_marks_shown_once_and_enter_completes() {
        let marks = Arc::new(Mutex::new(Vec::<String>::new()));
        let dones = Arc::new(Mutex::new(0usize));
        let marks_for_handler = Arc::clone(&marks);
        let dones_for_handler = Arc::clone(&dones);

        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(
                    crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings()
                )) {
                    ContextProvider(value: Context::owned(*theme::current())) {
                        IdeOnboardingDialog(
                        terminal: Some("vscode".to_string()),
                        terminal_ide_type: Some("vscode".to_string()),
                        platform: Some("darwin".to_string()),
                        on_mark_shown: move |terminal| {
                            marks_for_handler.lock().expect("marks mutex").push(terminal);
                        },
                        on_done: move |_| {
                            *dones_for_handler.lock().expect("dones mutex") += 1;
                        },
                        )
                    }
                }
            };
            let mut render_loop = Box::pin(
                app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(stream::iter(vec![key(KeyCode::Enter)]))
                        .with_size(120, 24),
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

        assert_eq!(marks.lock().expect("marks mutex").as_slice(), &["vscode"]);
        assert_eq!(*dones.lock().expect("dones mutex"), 1);
    }
}
