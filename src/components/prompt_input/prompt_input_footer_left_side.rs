//! Maps to: CC `components/PromptInput/PromptInputFooterLeftSide.tsx`.
//!
//! This is the retained owner of the prompt footer's left side.  Keep the
//! early-return ordering and placement here rather than folding these rows into
//! `PromptInputFooter`: exit/paste notices, history search, Vim INSERT status,
//! prompt mode, permission mode, task/team pills, and idle/loading hints.
//!
//! External-build exclusions from the source component remain intentional:
//! ANT coordinator/Tungsten, proactive/Kairos, PR polling, remote-session links,
//! fullscreen selection hints, and production voice state are not synthesized.

use super::input_modes::PromptInputMode;
use crate::types::permissions::PermissionMode;
use crate::utils::permissions::permission_mode::{
    is_default_mode, permission_mode_display_label, permission_mode_symbol,
};
use crate::utils::theme::Theme;
use iocraft::prelude::*;

#[derive(Default, Props)]
pub struct PromptInputFooterLeftSideProps {
    pub exit_hint: Option<String>,
    pub is_pasting: bool,
    pub vim_mode: Option<String>,
    pub mode: PromptInputMode,
    pub suppress_hint: bool,
    pub is_searching: bool,
    pub history_query: String,
    pub history_failed_match: bool,
    pub is_loading: bool,
    pub permission_mode: PermissionMode,
    pub background_task_count: usize,
    /// Maps to: CC `BackgroundTaskStatus.tsx:199-208` — the tasks pill renders
    /// `getPillLabel(runningTasks)`; the owner computes it from the same set
    /// the count gates on.
    pub background_tasks_label: String,
    pub teammate_count: usize,
}

#[component]
pub fn PromptInputFooterLeftSide(
    props: &PromptInputFooterLeftSideProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    // Maps to: CC footer pills reading `AppState.footerSelection`.
    let selected_footer_item = crate::state::app_state::use_app_state(&mut hooks, |state| {
        state.footer_selection.map(|item| item.as_str().to_string())
    });
    // CC HistorySearchInput renders its field through TextInput, whose cursor
    // inversion is `isTerminalFocused && !accessibilityEnabled`.
    let history_cursor_shown = crate::components::text_input::text_input_can_show_cursor(
        hooks.use_terminal_focus(),
        crate::components::text_input::accessibility_enabled_from_env(),
    );

    // Source ordering is significant: exit and active paste replace every
    // other left-side item rather than being appended to the status row.
    if let Some(ref hint) = props.exit_hint {
        return element! {
            Text(content: hint.clone(), color: theme.inactive, wrap: TextWrap::NoWrap)
        }
        .into_any();
    }
    if props.is_pasting {
        return element! {
            Text(content: "Pasting text…".to_string(), color: theme.inactive, wrap: TextWrap::NoWrap)
        }
        .into_any();
    }

    let show_vim = props.vim_mode.as_deref() == Some("INSERT") && !props.is_searching;
    let show_hint = !props.suppress_hint && !show_vim;

    element! {
        View(
            flex_direction: FlexDirection::Row,
            justify_content: JustifyContent::FLEX_START,
            column_gap: 1u32,
            flex_shrink: 1.0f32,
            overflow: Overflow::Hidden,
        ) {
            #(if props.is_searching {
                let label = if props.history_failed_match {
                    "no matching prompt:"
                } else {
                    "search prompts:"
                };
                Some(element! {
                    View(flex_direction: FlexDirection::Row, overflow: Overflow::Hidden) {
                        Text(content: format!("{label} "), color: theme.inactive, wrap: TextWrap::NoWrap)
                        Text(content: props.history_query.clone(), color: theme.inactive, wrap: TextWrap::NoWrap)
                        Text(content: " ".to_string(), invert: history_cursor_shown, wrap: TextWrap::NoWrap)
                    }
                })
            } else {
                None
            })
            #(if show_vim {
                Some(element! {
                    Text(content: "-- INSERT --".to_string(), color: theme.inactive, wrap: TextWrap::NoWrap)
                })
            } else {
                None
            })
            ModeIndicator(
                mode: props.mode,
                show_hint: show_hint,
                is_loading: props.is_loading,
                permission_mode: props.permission_mode,
                background_task_count: props.background_task_count,
                background_tasks_label: props.background_tasks_label.clone(),
                teammate_count: props.teammate_count,
                selected_footer_item: selected_footer_item,
            )
        }
    }
    .into_any()
}

#[derive(Default, Props)]
struct ModeIndicatorProps {
    mode: PromptInputMode,
    show_hint: bool,
    is_loading: bool,
    permission_mode: PermissionMode,
    background_task_count: usize,
    background_tasks_label: String,
    teammate_count: usize,
    selected_footer_item: Option<String>,
}

#[component]
fn ModeIndicator(props: &ModeIndicatorProps, hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();

    // Matches the source's first ModeIndicator return. Bash mode replaces the
    // permission/task/hint composition.
    if props.mode == PromptInputMode::Bash {
        return element! {
            Text(content: "! for bash mode".to_string(), color: theme.bash_border, wrap: TextWrap::NoWrap)
        }
        .into_any();
    }

    let has_mode = !is_default_mode(props.permission_mode);
    let has_tasks = props.background_task_count > 0;
    let has_teams = props.teammate_count > 0;
    let primary_item_count =
        usize::from(has_mode) + usize::from(has_tasks) + usize::from(has_teams);
    let show_mode_hint = primary_item_count < 2;
    // Cometix-specific deviation (product requirement — skip in parity audits):
    // CC pushes hintParts ("esc to interrupt" while loading, gated only on
    // showHint) into the same row. Cometix explicitly does NOT clone that tip
    // text, so the slot stays empty while loading and only the
    // "? for shortcuts" idle fallback remains (CC's empty-parts condition).
    let show_shortcuts_hint = props.show_hint && !props.is_loading && primary_item_count == 0;

    element! {
        View(flex_direction: FlexDirection::Row, column_gap: 1u32, overflow: Overflow::Hidden) {
            #(if has_mode {
                Some(element! {
                    PermissionModePart(mode: props.permission_mode, show_hint: show_mode_hint)
                })
            } else {
                None
            })
            #(if has_tasks {
                Some(element! {
                    Text(
                        // Maps to: CC BackgroundTaskStatus.tsx:199-203 — the
                        // SummaryPill renders getPillLabel(runningTasks); the
                        // owner passes the computed label down.
                        content: props.background_tasks_label.clone(),
                        color: if props.selected_footer_item.as_deref() == Some("tasks") { theme.inverse_text } else { theme.inactive },
                        background_color: if props.selected_footer_item.as_deref() == Some("tasks") { Some(theme.suggestion) } else { None },
                        wrap: TextWrap::NoWrap,
                    )
                })
            } else {
                None
            })
            #(if has_teams {
                Some(element! {
                    Text(
                        content: format!("{} teammate{}", props.teammate_count, if props.teammate_count == 1 { "" } else { "s" }),
                        color: if props.selected_footer_item.as_deref() == Some("teams") { theme.inverse_text } else { theme.inactive },
                        background_color: if props.selected_footer_item.as_deref() == Some("teams") { Some(theme.suggestion) } else { None },
                        wrap: TextWrap::NoWrap,
                    )
                })
            } else {
                None
            })
            #(if props.show_hint && has_tasks && !has_teams {
                Some(element! {
                    Text(
                        content: if props.selected_footer_item.as_deref() == Some("tasks") { "Enter to view tasks".to_string() } else { "↓ to manage".to_string() },
                        color: theme.inactive,
                        wrap: TextWrap::NoWrap,
                    )
                })
            } else if props.show_hint && has_teams && !has_tasks && props.selected_footer_item.as_deref() == Some("teams") {
                Some(element! {
                    Text(content: "· Enter to view".to_string(), color: theme.inactive, wrap: TextWrap::NoWrap)
                })
            } else if show_shortcuts_hint {
                Some(element! {
                    Text(content: "? for shortcuts".to_string(), color: theme.inactive, wrap: TextWrap::NoWrap)
                })
            } else {
                None
            })
        }
    }
    .into_any()
}

#[derive(Default, Props)]
struct PermissionModePartProps {
    mode: PermissionMode,
    show_hint: bool,
}

#[component]
fn PermissionModePart(
    props: &PermissionModePartProps,
    hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let color = match props.mode {
        // CC has no `bubble` entry in PERMISSION_MODE_CONFIG, so getModeConfig
        // serves the default config's `text` color.
        PermissionMode::Default | PermissionMode::Bubble => theme.text,
        PermissionMode::AcceptEdits => theme.auto_accept,
        PermissionMode::Plan => theme.plan_mode,
        // CC: auto mode uses warning/amber, not autoAccept violet.
        PermissionMode::Auto => theme.warning,
        PermissionMode::DontAsk | PermissionMode::BypassPermissions => theme.error,
    };
    let symbol = permission_mode_symbol(props.mode);
    let label = permission_mode_display_label(props.mode);
    let prefix = if symbol.is_empty() {
        String::new()
    } else {
        format!("{symbol} ")
    };

    element! {
        View(flex_direction: FlexDirection::Row, flex_shrink: 0.0f32) {
            // Cometix-specific deviation (product requirement — skip in parity
            // audits): CC's modePart appends " on" after the lowercased mode
            // title (`{permissionModeTitle(mode).toLowerCase()} on`); Cometix
            // renders the bare label without the " on" suffix.
            Text(content: format!("{prefix}{label}"), color: color, wrap: TextWrap::NoWrap)
            #(props.show_hint.then(|| element! {
                Text(content: " (shift+tab to cycle)".to_string(), color: theme.inactive, wrap: TextWrap::NoWrap)
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;
    use futures::StreamExt;

    fn render(props: PromptInputFooterLeftSideProps) -> String {
        let mut app = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                // Reads `state.footer_selection` to decide which pill is
                // highlighted. Default state (nothing selected) is the fixture:
                // every test through this harness drives the parts from props.
                crate::state::app_state::AppStateProvider(
                    children: crate::state::app_state::ProviderChildren::new(move || element! {
                        PromptInputFooterLeftSide(
                            exit_hint: props.exit_hint.clone(),
                            is_pasting: props.is_pasting,
                            vim_mode: props.vim_mode.clone(),
                            mode: props.mode,
                            suppress_hint: props.suppress_hint,
                            is_searching: props.is_searching,
                            history_query: props.history_query.clone(),
                            history_failed_match: props.history_failed_match,
                            is_loading: props.is_loading,
                            permission_mode: props.permission_mode,
                            background_task_count: props.background_task_count,
                            background_tasks_label: props.background_tasks_label.clone(),
                            teammate_count: props.teammate_count,
                        )
                    }.into_any()),
                )
            }
        };
        let canvas = futures::executor::block_on(async {
            let mut render_loop = Box::pin(
                app.mock_terminal_render_loop(MockTerminalConfig::default().with_size(100, 8)),
            );
            render_loop.next().await.expect("footer should render")
        });
        (0..canvas.height())
            .map(|y| {
                (0..canvas.width())
                    .filter_map(|x| canvas.cell(x, y).and_then(|cell| cell.text()))
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn shells_label(count: usize) -> String {
        crate::tasks::pill_label::get_pill_label(&vec![
            crate::tasks::pill_label::PillTask::LocalBash { is_monitor: false };
            count
        ])
    }

    #[test]
    fn exit_and_paste_notices_replace_all_other_parts() {
        let exit = render(PromptInputFooterLeftSideProps {
            exit_hint: Some("Press ctrl+c again to exit".to_string()),
            mode: PromptInputMode::Bash,
            background_task_count: 2,
            background_tasks_label: shells_label(2),
            ..Default::default()
        });
        assert!(exit.contains("Press ctrl+c again to exit"));
        assert!(!exit.contains("bash mode"));
        assert!(!exit.contains("2 shells"));

        let paste = render(PromptInputFooterLeftSideProps {
            is_pasting: true,
            mode: PromptInputMode::Bash,
            ..Default::default()
        });
        assert!(paste.contains("Pasting text…"));
        assert!(!paste.contains("bash mode"));
    }

    #[test]
    fn vim_indicator_is_insert_only_and_hidden_during_search() {
        let insert = render(PromptInputFooterLeftSideProps {
            vim_mode: Some("INSERT".to_string()),
            ..Default::default()
        });
        assert!(insert.contains("-- INSERT --"));
        assert!(!insert.contains("? for shortcuts"));

        let normal = render(PromptInputFooterLeftSideProps {
            vim_mode: Some("NORMAL".to_string()),
            ..Default::default()
        });
        assert!(!normal.contains("-- NORMAL --"));

        let search = render(PromptInputFooterLeftSideProps {
            vim_mode: Some("INSERT".to_string()),
            is_searching: true,
            history_query: "abc".to_string(),
            ..Default::default()
        });
        assert!(search.contains("search prompts: abc"));
        assert!(!search.contains("-- INSERT --"));
    }

    #[test]
    fn permission_cycle_hint_hides_when_two_primary_items_compete() {
        let mode_only = render(PromptInputFooterLeftSideProps {
            permission_mode: PermissionMode::Plan,
            ..Default::default()
        });
        assert!(mode_only.contains("shift+tab to cycle"));

        let with_tasks = render(PromptInputFooterLeftSideProps {
            permission_mode: PermissionMode::Plan,
            background_task_count: 1,
            background_tasks_label: shells_label(1),
            ..Default::default()
        });
        assert!(!with_tasks.contains("shift+tab to cycle"));
    }

    #[test]
    fn bash_mode_owns_the_mode_indicator_position() {
        let text = render(PromptInputFooterLeftSideProps {
            mode: PromptInputMode::Bash,
            permission_mode: PermissionMode::Plan,
            background_task_count: 2,
            background_tasks_label: shells_label(2),
            ..Default::default()
        });
        assert!(text.contains("! for bash mode"));
        assert!(!text.contains("plan mode"));
        assert!(!text.contains("2 shells"));
    }

    #[test]
    fn tasks_pill_renders_the_pill_label_verbatim() {
        // Maps to: CC BackgroundTaskStatus.tsx:199-208 — the pill text is
        // getPillLabel(runningTasks), e.g. "1 local agent" for a single
        // backgrounded agent (tasks/pillLabel.ts:37-38), not "N tasks".
        let text = render(PromptInputFooterLeftSideProps {
            background_task_count: 1,
            background_tasks_label: crate::tasks::pill_label::get_pill_label(&[
                crate::tasks::pill_label::PillTask::LocalAgent,
            ]),
            ..Default::default()
        });
        assert!(text.contains("1 local agent"), "canvas=\n{text}");
        assert!(text.contains("↓ to manage"), "canvas=\n{text}");
        assert!(!text.contains("1 task"), "canvas=\n{text}");
    }
}
