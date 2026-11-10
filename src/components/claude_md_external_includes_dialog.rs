//! Maps to: CC `components/ClaudeMdExternalIncludesDialog.tsx`.
//!
//! Dialog UI + choice callback. Project-config writes
//! (`hasClaudeMdExternalIncludesApproved` /
//! `hasClaudeMdExternalIncludesWarningShown`) are applied by
//! `interactive_helpers::apply_claude_md_external_includes_choice` at the
//! setup-screen boundary (same TrustDialog pattern). Analytics omitted.

use crate::components::custom_select::{Select, SelectLayout, SelectOptionData};
use crate::components::design_system::dialog::Dialog;
use crate::utils::claudemd::ExternalClaudeMdInclude;
use crate::utils::theme::Theme;
use iocraft::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaudeMdExternalIncludesChoice {
    Yes,
    No,
}

impl ClaudeMdExternalIncludesChoice {
    pub fn value(self) -> &'static str {
        match self {
            Self::Yes => "yes",
            Self::No => "no",
        }
    }
}

/// Maps to: CC `components/ClaudeMdExternalIncludesDialog.tsx` `Select` options.
pub fn claude_md_external_includes_options() -> Vec<SelectOptionData> {
    vec![
        SelectOptionData {
            label: "Yes, allow external imports".to_string(),
            value: ClaudeMdExternalIncludesChoice::Yes.value().to_string(),
            ..SelectOptionData::default()
        },
        SelectOptionData {
            label: "No, disable external imports".to_string(),
            value: ClaudeMdExternalIncludesChoice::No.value().to_string(),
            ..SelectOptionData::default()
        },
    ]
}

fn choice_from_value(value: &str) -> ClaudeMdExternalIncludesChoice {
    match value {
        "yes" => ClaudeMdExternalIncludesChoice::Yes,
        _ => ClaudeMdExternalIncludesChoice::No,
    }
}

#[derive(Default, Props)]
pub struct ClaudeMdExternalIncludesDialogProps<'a> {
    pub on_done: HandlerMut<'a, ClaudeMdExternalIncludesChoice>,
    pub is_standalone_dialog: bool,
    pub external_includes: Vec<ExternalClaudeMdInclude>,
}

/// Maps to: CC `components/ClaudeMdExternalIncludesDialog.tsx`
/// `ClaudeMdExternalIncludesDialog`.
#[component]
pub fn ClaudeMdExternalIncludesDialog<'a>(
    props: &mut ClaudeMdExternalIncludesDialogProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let mut focused_index = hooks.use_state(|| 0usize);
    let mut pending_choice = hooks.use_state(|| Option::<ClaudeMdExternalIncludesChoice>::None);
    let options = claude_md_external_includes_options();
    let option_count = options.len().max(1);
    let external_includes = props.external_includes.clone();

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
                _ => {}
            }
        }
    });

    let choice = { *pending_choice.read() };
    if let Some(choice) = choice {
        pending_choice.set(None);
        (props.on_done)(choice);
    }

    element! {
        Dialog(
            title: "Allow external AGENTS.md file imports?".to_string(),
            color: Some(theme.warning),
            hide_border: !props.is_standalone_dialog,
            hide_input_guide: !props.is_standalone_dialog,
            on_cancel: move |_| {
                pending_choice.set(Some(ClaudeMdExternalIncludesChoice::No));
            },
        ) {
            Text(
                content: "This project's AGENTS.md imports files outside the current working\ndirectory. Never allow this for third-party repositories.".to_string(),
                wrap: TextWrap::Wrap,
            )

            #(if external_includes.is_empty() {
                None
            } else {
                Some(element! {
                    View(flex_direction: FlexDirection::Column) {
                        Text(content: "External imports:".to_string(), color: theme.inactive)
                        #(external_includes.into_iter().map(|include| element! {
                            Text(content: format!("  {}", include.path), color: theme.inactive, wrap: TextWrap::Wrap)
                        }))
                    }
                })
            })

            Text(
                content: "Important: Only use nyan with files you trust. Accessing\nuntrusted files may pose security risks".to_string(),
                color: theme.inactive,
                wrap: TextWrap::Wrap,
            )

            Select(
                options: options,
                focused_index: focused_index.get().min(option_count - 1),
                visible_option_count: option_count,
                layout: SelectLayout::CompactVertical,
                hide_indexes: true,
            )
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
    fn claude_md_external_includes_options_match_official_copy_and_order() {
        let options = claude_md_external_includes_options();

        assert_eq!(options[0].label, "Yes, allow external imports");
        assert_eq!(options[0].value, "yes");
        assert_eq!(options[1].label, "No, disable external imports");
        assert_eq!(options[1].value, "no");
    }

    #[test]
    fn claude_md_external_includes_dialog_renders_official_copy_list_link_and_options() {
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                ClaudeMdExternalIncludesDialog(
                    is_standalone_dialog: true,
                    external_includes: vec![
                        ExternalClaudeMdInclude::new("/outside/AGENTS.md", "/repo/AGENTS.md"),
                        ExternalClaudeMdInclude::new("/shared/rules.md", "/repo/AGENTS.md"),
                    ],
                )
            }
        }
        .render(Some(110))
        .to_string();

        assert!(
            text.contains("Allow external AGENTS.md file imports?"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("This project's AGENTS.md imports files outside the current working"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("Never allow this for third-party repositories."),
            "canvas=\n{text}"
        );
        assert!(text.contains("External imports:"), "canvas=\n{text}");
        assert!(text.contains("/outside/AGENTS.md"), "canvas=\n{text}");
        assert!(text.contains("/shared/rules.md"), "canvas=\n{text}");
        assert!(
            text.contains("Important: Only use nyan with files you trust."),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("Yes, allow external imports"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("No, disable external imports"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("Enter to confirm · Esc to cancel"),
            "standalone dialog should keep official input guide; canvas=\n{text}"
        );
    }

    #[test]
    fn claude_md_external_includes_non_standalone_hides_default_input_guide() {
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                ClaudeMdExternalIncludesDialog(is_standalone_dialog: false)
            }
        }
        .render(Some(100))
        .to_string();

        assert!(
            text.contains("Allow external AGENTS.md file imports?"),
            "canvas=\n{text}"
        );
        assert!(
            !text.contains("Enter to confirm · Esc to cancel"),
            "non-standalone dialog should hide input guide; canvas=\n{text}"
        );
    }

    #[test]
    fn claude_md_external_includes_default_enter_calls_on_done_with_yes() {
        let done = Arc::new(Mutex::new(Option::<ClaudeMdExternalIncludesChoice>::None));
        let done_for_handler = Arc::clone(&done);

        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(*theme::current())) {
                    ClaudeMdExternalIncludesDialog(
                        is_standalone_dialog: true,
                        on_done: move |choice| {
                            *done_for_handler.lock().expect("done mutex") = Some(choice);
                        },
                    )
                }
            };
            let mut render_loop = Box::pin(
                app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(stream::iter(vec![key(KeyCode::Enter)]))
                        .with_size(110, 24),
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

        assert_eq!(
            *done.lock().expect("done mutex"),
            Some(ClaudeMdExternalIncludesChoice::Yes)
        );
    }

    #[test]
    fn claude_md_external_includes_down_enter_calls_on_done_with_no() {
        let done = Arc::new(Mutex::new(Option::<ClaudeMdExternalIncludesChoice>::None));
        let done_for_handler = Arc::clone(&done);

        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(*theme::current())) {
                    ClaudeMdExternalIncludesDialog(
                        is_standalone_dialog: true,
                        on_done: move |choice| {
                            *done_for_handler.lock().expect("done mutex") = Some(choice);
                        },
                    )
                }
            };
            let mut render_loop = Box::pin(
                app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(stream::iter(vec![
                        key(KeyCode::Down),
                        key(KeyCode::Enter),
                    ]))
                    .with_size(110, 24),
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

        assert_eq!(
            *done.lock().expect("done mutex"),
            Some(ClaudeMdExternalIncludesChoice::No)
        );
    }
}
