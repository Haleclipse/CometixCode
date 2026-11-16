//! Maps to: CC `components/PromptInput/PromptInputFooter.tsx`.
//!
//! Nyan keeps this footer in normal document flow for the main-screen /
//! native-scrollback path. It mirrors the official left-side mode/hints plus
//! right-side notifications, with no fullscreen/overlay behavior.

use super::{
    input_modes::PromptInputMode, notifications,
    prompt_input_footer_left_side::PromptInputFooterLeftSide, prompt_input_info_line,
};
use crate::components::agent_panel;
use crate::context::notifications::{NotificationColor, NotificationSegment};
use crate::hooks::use_credential_status::CredentialStatus;
use crate::types::permissions::PermissionMode;
use crate::utils::permissions::permission_mode::is_default_mode;
use iocraft::prelude::*;

/// Segmented notification footer-row payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptFooterIndicator {
    pub text: String,
    pub segments: Vec<NotificationSegment>,
}

impl PromptFooterIndicator {
    pub(crate) fn from_segments(segments: Vec<NotificationSegment>) -> Self {
        let text = segments
            .iter()
            .map(|segment| segment.text.as_str())
            .collect();
        Self { text, segments }
    }

    pub(crate) fn inactive(text: impl Into<String>) -> Self {
        Self::from_segments(vec![NotificationSegment::text(text)])
    }

    pub(crate) fn colored(text: impl Into<String>, color: NotificationColor) -> Self {
        Self::from_segments(vec![NotificationSegment::text(text).with_color(color)])
    }
}

#[derive(Default, Props)]
pub struct FooterProps {
    /// Maps to: CC `PromptInputFooter.apiKeyStatus` → `Notifications`.
    pub credential_status: CredentialStatus,
    /// REPL-owned newer release, forwarded to `Notifications`.
    pub update_available: Option<crate::utils::auto_updater::UpdateAvailable>,
    /// Maps to: CC `PromptInputFooter.tsx:63` `ideSelection` (REPL-owned),
    /// forwarded to `Notifications` (`:215`).
    pub ide_selection: Option<crate::hooks::use_ide_selection::IdeSelection>,
    /// Maps to: CC `PromptInputFooter.tsx:67` `messages` prop, forwarded to
    /// `Notifications` (`:212`) for its component-local tokenUsage derivation.
    pub messages: std::sync::Arc<Vec<crate::types::message::Message>>,
    /// Maps to: CC `PromptInputFooter` → `Notifications.debug` prop flow.
    pub debug: bool,
    pub exit_hint: Option<String>,
    pub suppress_hint: bool,
    pub is_searching: bool,
    /// Maps to: CC `PromptInputFooter` `historyQuery` + `setHistoryQuery`
    /// (PromptInput.tsx:3075-3076), passed through to the left side.
    pub history_query: Option<State<String>>,
    pub history_failed_match: bool,
    pub is_loading: bool,
    pub permission_mode: PermissionMode,
    pub background_task_count: usize,
    /// Maps to: CC `BackgroundTaskStatus.tsx:199-208` — the pill text is
    /// `getPillLabel(runningTasks)` (`tasks/pillLabel.ts`), computed by the
    /// owner from the same set `background_task_count` gates on.
    pub background_tasks_label: String,
    pub teammate_count: usize,
    /// Effective footer selection from PromptInput: a pill that stopped
    /// rendering is never drawn selected.
    pub tasks_selected: bool,
    pub teams_selected: bool,
    pub vim_mode: Option<String>,
    pub mode: PromptInputMode,
    pub is_pasting: bool,
    /// Terminal columns for the info line's narrow-screen degradation
    /// (`docs/nyan/tui.md` §3.2). `None` reads the process terminal.
    pub info_line_width: Option<u32>,
    /// @nyan: the agent panel is mounted above the info line (§6). PromptInput
    /// derives this from the same plan that feeds its height budget.
    pub agent_panel_visible: bool,
    /// @nyan: the panel owns the footer selection, so the mode row drops its
    /// `↓ agents` hint (the panel's own key-hint line covers it).
    pub agent_panel_focused: bool,
    /// @nyan: a row completed within the last 30 s; the mode row shows
    /// `/tasks to see agents` (§6.1).
    pub finished_hint_active: bool,
}

/// Single source for "the left side renders a body row for footer pills"
/// (bash banner, vim indicator, tasks pill, teammate pill). The HEIGHT BUDGET
/// and the component's own render gate MUST share this predicate.
pub fn footer_has_items(
    mode: PromptInputMode,
    vim_mode_active: bool,
    background_task_count: usize,
    teammate_count: usize,
) -> bool {
    mode == PromptInputMode::Bash
        || vim_mode_active
        || background_task_count > 0
        || teammate_count > 0
}

pub fn footer_row_should_render(
    exit_hint_visible: bool,
    is_searching: bool,
    suppress_hint: bool,
    _is_loading: bool,
    permission_mode: PermissionMode,
    notification_row_count: usize,
    has_footer_items: bool,
) -> bool {
    exit_hint_visible
        || is_searching
        || notification_row_count > 0
        || !is_default_mode(permission_mode)
        || !suppress_hint
        || has_footer_items
}

fn footer_body_height_for(
    exit_hint_visible: bool,
    is_searching: bool,
    suppress_hint: bool,
    is_loading: bool,
    permission_mode: PermissionMode,
    notification_row_count: usize,
    has_footer_items: bool,
) -> usize {
    if footer_row_should_render(
        exit_hint_visible,
        is_searching,
        suppress_hint,
        is_loading,
        permission_mode,
        notification_row_count,
        has_footer_items,
    ) {
        notification_row_count.max(1)
    } else {
        0
    }
}

/// Total footer height: the agent panel (when visible), the built-in info line
/// and the body row(s).
///
/// @nyan: the info line is always present (docs/nyan/tui.md §3.1) and is NOT
/// affected by `suppress_hint`, so it is added outside the body predicate.
/// The component below mounts it on exactly the same condition.
/// `agent_panel_height` must come from [`agent_panel::PanelPlan::total_lines`]
/// — the same value the panel renders.
pub fn footer_height_with_panel(
    exit_hint_visible: bool,
    is_searching: bool,
    suppress_hint: bool,
    is_loading: bool,
    permission_mode: PermissionMode,
    notification_row_count: usize,
    has_footer_items: bool,
    agent_panel_height: usize,
) -> usize {
    agent_panel_height
        + prompt_input_info_line::INFO_LINE_HEIGHT
        + footer_body_height_for(
            exit_hint_visible,
            is_searching,
            suppress_hint,
            is_loading,
            permission_mode,
            notification_row_count,
            has_footer_items,
        )
}

#[component]
pub fn Footer(props: &FooterProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    // Rows `Notifications` renders directly (CC NotificationContent). Computed
    // here (component-owned) from the same contexts Notifications reads, so
    // every mount point — production or test harness — stays consistent.
    // Maps to: CC `useNotification()` reading `AppState.notifications.current`.
    // Read through the subscribed selector, not through `use_notifications`:
    // that writer carries no subscription, so a notification arriving from a
    // background task would not wake this footer.
    let has_current_notification = crate::state::app_state::use_app_state(&mut hooks, |state| {
        state.notifications.current.is_some()
    });
    // Maps to: CC Notifications.tsx:80-83 tokenUsage useMemo ([messages]
    // identity dep); the retained footer height budget needs the same value.
    let token_usage = hooks.use_memo(
        {
            let messages = std::sync::Arc::clone(&props.messages);
            move || notifications::token_usage_from_messages(&messages)
        },
        std::sync::Arc::as_ptr(&props.messages) as usize,
    );
    // Height computers read the PromptInput-scoped wake epoch (L1
    // `PromptInput-scoped footer layout wake`, PORTING.md): the flip bump
    // wakes the PromptInput owner, which re-renders this Footer; the epoch
    // read keeps both height sites (this one and prompt_input/mod.rs) on the
    // same carrier.
    let _footer_layout_epoch = hooks
        .try_use_context::<super::footer_layout_wake::FooterLayoutWake>()
        .map(|wake| wake.epoch())
        .unwrap_or(0);
    // The non-AppState inputs are captured by value: the selector is retained
    // by the subscription hook so it can re-project off-render, and it is
    // re-boxed every render, so each pass still closes over that pass's props.
    let notification_row_count = {
        let debug = props.debug;
        let credential_status = props.credential_status.clone();
        let update_available = props.update_available.clone();
        let ide_selection = props.ide_selection.clone();
        crate::state::app_state::use_app_state(&mut hooks, move |state| {
            notifications::direct_footer_row_count_from_app(
                state,
                has_current_notification,
                debug,
                &credential_status,
                token_usage,
                update_available.as_ref(),
                ide_selection.as_ref(),
            )
        })
    };
    let suppress_hint = props.suppress_hint || props.is_searching;
    let exit_hint_visible = props.exit_hint.is_some();
    let has_footer_items = footer_has_items(
        props.mode,
        props.vim_mode.is_some(),
        props.background_task_count,
        props.teammate_count,
    );
    let footer_body_height = footer_body_height_for(
        exit_hint_visible,
        props.is_searching,
        suppress_hint,
        props.is_loading,
        props.permission_mode,
        notification_row_count,
        has_footer_items,
    );
    let has_footer_row = footer_body_height > 0;
    let has_right_side = notification_row_count > 0;

    // @nyan: the built-in info line replaces the deleted statusLine and is
    // always present, independent of `suppress_hint` (docs/nyan/tui.md §3.1).
    // `footer_height_for` counts exactly this row.
    let info_line = element! {
        prompt_input_info_line::InfoLine(
            token_usage: token_usage,
            width: props.info_line_width,
        )
    }
    .into_any();

    // @nyan: the agent panel sits between the prompt input and the info line
    // (§6, §2). It renders nothing when it has no rows or the terminal is too
    // short, and the footer height budget reserves exactly its line count.
    let agent_panel = element! { agent_panel::AgentPanel }.into_any();

    element! {
        View(flex_direction: FlexDirection::Column, width: 100pct) {
            #(Some(agent_panel))
            #(Some(info_line))
            #(if has_footer_row {
                Some(element! {
                    View(
                        flex_direction: FlexDirection::Row,
                        justify_content: JustifyContent::SPACE_BETWEEN,
                        column_gap: 1u32,
                        width: 100pct,
                        height: footer_body_height as u32,
                        padding_left: 2u32,
                        padding_right: 2u32,
                        overflow: Overflow::Hidden,
                    ) {
                        View(flex_direction: FlexDirection::Row, flex_shrink: 1.0f32, overflow: Overflow::Hidden) {
                            PromptInputFooterLeftSide(
                                exit_hint: props.exit_hint.clone(),
                                is_pasting: props.is_pasting,
                                vim_mode: props.vim_mode.clone(),
                                mode: props.mode,
                                suppress_hint: suppress_hint,
                                is_searching: props.is_searching,
                                history_query: props.history_query,
                                history_failed_match: props.history_failed_match,
                                is_loading: props.is_loading,
                                permission_mode: props.permission_mode,
                                background_task_count: props.background_task_count,
                                background_tasks_label: props.background_tasks_label.clone(),
                                teammate_count: props.teammate_count,
                                // CC :130-132 `pillSelected` also clears when the
                                // pointer is inside CoordinatorTaskPanel
                                // (`coordinatorTaskIndex >= 0`). The panel is not
                                // ported and the index never leaves its -1
                                // default, so it is `tasksSelected`; restore the
                                // formula when the panel lands.
                                tasks_selected: props.tasks_selected,
                                teams_selected: props.teams_selected,
                                // @nyan: agent-panel hints (§6).
                                agent_panel_visible: props.agent_panel_visible,
                                agent_panel_focused: props.agent_panel_focused,
                                finished_hint_active: props.finished_hint_active,
                            )
                        }
                        #(if has_right_side {
                            Some(element! {
                                View(flex_direction: FlexDirection::Column, flex_shrink: 1.0f32, overflow: Overflow::Hidden) {
                                    #(if notification_row_count > 0 {
                                        // Maps to: CC PromptInputFooter.tsx:206-214
                                        // — forward the auto-updater value/callback
                                        // props to Notifications.
                                        Some(element! {
                                            notifications::Notifications(
                                                credential_status: props.credential_status.clone(),
                                                update_available: props.update_available.clone(),
                                                debug: props.debug,
                                                messages: props.messages.clone(),
                                                ide_selection: props.ide_selection.clone(),
                                                suppressed: false,
                                                inline: true,
                                            )
                                        })
                                    } else {
                                        None
                                    })
                                }
                            })
                        } else {
                            None
                        })
                    }
                })
            } else {
                None
            })
        }
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::notifications::{Notification, NotificationsState};
    use crate::utils::theme;

    #[test]
    fn footer_height_matches_hint_and_notification_rows() {
        // @nyan: +1 for the built-in info line, which is always present.
        assert_eq!(
            footer_height_with_panel(
                false,
                false,
                false,
                true,
                PermissionMode::Default,
                1,
                false,
                0,
            ),
            2,
            "loading hint and notification share the official footer row above the info line"
        );
        assert_eq!(
            footer_height_with_panel(false, false, true, false, PermissionMode::Plan, 0, false, 0),
            2,
            "active permission mode renders even when shortcut hints are suppressed"
        );
        assert_eq!(
            footer_height_with_panel(
                false,
                false,
                true,
                false,
                PermissionMode::Default,
                2,
                false,
                0,
            ),
            3,
            "right-side status indicators keep their official stacked footer rows"
        );
    }

    #[test]
    fn footer_height_adds_the_agent_panel_rows_above_the_info_line() {
        // No body row: panel + info line exactly.
        assert_eq!(
            footer_height_with_panel(
                false,
                false,
                true,
                false,
                PermissionMode::Default,
                0,
                false,
                5,
            ),
            6,
            "agent panel rows precede the always-present info line"
        );
        // With a body row: panel + info line + body row.
        assert_eq!(
            footer_height_with_panel(
                false,
                false,
                false,
                false,
                PermissionMode::Default,
                0,
                true,
                3,
            ),
            5
        );
    }

    #[derive(Default, Props)]
    struct FooterWithAgentPanelProps {
        store: Option<crate::state::store::AppStore>,
        agent_panel_visible: bool,
    }

    #[component]
    fn FooterWithAgentPanel(
        props: &FooterWithAgentPanelProps,
        mut hooks: Hooks,
    ) -> impl Into<AnyElement<'static>> {
        let store = hooks.use_const(|| props.store.clone().expect("footer probe store"));
        let agent_panel_visible = props.agent_panel_visible;
        element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                crate::state::app_state::AppStateProvider(
                    prebuilt_store: Some(store),
                    children: crate::state::app_state::ProviderChildren::new(move || element! {
                        Footer(
                            suppress_hint: false,
                            agent_panel_visible: agent_panel_visible,
                        )
                    }.into_any()),
                )
            }
        }
    }

    fn shell_task_store() -> crate::state::store::AppStore {
        use crate::state::app_state_store::TaskState;
        use std::sync::Arc;
        let mut state = crate::state::app_state_store::AppState::default();
        state.tasks = Arc::new(std::collections::BTreeMap::from([(
            "shell-1".to_string(),
            Arc::new(TaskState::LocalShell(
                crate::tasks::local_shell_task::guards::LocalShellTaskState {
                    id: "shell-1".into(),
                    task_type: "local_bash".into(),
                    status: "running".into(),
                    description: "a local shell".into(),
                    command: "cargo test --lib".into(),
                    result: None,
                    notified: false,
                    shell_command: None,
                    last_reported_total_lines: 0,
                    is_backgrounded: true,
                    agent_id: None,
                    tool_use_id: None,
                    kind: None,
                    start_time_ms: 1,
                    end_time_ms: None,
                },
            )),
        )]));
        crate::state::store::AppStore::new(state, None)
    }

    #[test]
    fn footer_renders_the_agent_panel_above_the_info_line_and_mode_row() {
        let canvas = element!(FooterWithAgentPanel(
            store: Some(shell_task_store()),
            agent_panel_visible: true,
        ))
        .render(Some(100));
        let text = canvas.to_string();
        let lines = text.lines().collect::<Vec<_>>();
        assert_eq!(
            lines.len(),
            3,
            "panel, info line, mode row; canvas=\n{text}"
        );
        assert!(lines[0].contains("cargo test --lib"), "canvas=\n{text}");
        // The middle line is the always-present info line (directory/model
        // segments vary with the environment).
        assert!(!lines[1].contains("cargo test"), "canvas=\n{text}");
        assert!(lines[2].contains("? for shortcuts"), "canvas=\n{text}");
        assert!(lines[2].contains("↓ agents"), "canvas=\n{text}");
    }

    #[test]
    fn footer_height_budgets_the_pill_row() {
        assert_eq!(
            footer_height_with_panel(
                false,
                false,
                true,
                false,
                PermissionMode::Default,
                0,
                true,
                0,
            ),
            2,
            "footer pills own one body row plus the info line"
        );
        assert!(footer_has_items(PromptInputMode::Prompt, false, 2, 0));
        assert!(footer_has_items(PromptInputMode::Bash, false, 0, 0));
        assert!(!footer_has_items(PromptInputMode::Prompt, false, 0, 0));
    }

    #[test]
    fn footer_height_keeps_the_info_line_when_no_body_row_renders() {
        assert_eq!(
            footer_height_with_panel(
                false,
                false,
                true,
                false,
                PermissionMode::Default,
                0,
                false,
                0,
            ),
            1,
            "the info line survives a fully suppressed hint row"
        );
    }

    #[component]
    fn FooterLoadingWithPermissionMode(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let store = hooks.use_state(|| {
            crate::state::store::AppStore::new(
                crate::state::app_state_store::AppState::default(),
                None,
            )
        });
        element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                crate::state::app_state::AppStateProvider(
                    prebuilt_store: Some(store.read().clone()),
                    children: crate::state::app_state::ProviderChildren::new(|| element! {
                        Footer(
                            suppress_hint: false,
                            is_loading: true,
                            permission_mode: PermissionMode::AcceptEdits,
                        )
                    }.into_any()),
                )
            }
        }
    }

    #[test]
    fn footer_renders_loading_and_permission_mode_without_interrupt_hint() {
        let text = element!(FooterLoadingWithPermissionMode)
            .render(Some(100))
            .to_string();

        // Nyan-specific deviation (product requirement — skip in parity
        // audits): CC renders "accept edits on"; Nyan drops the " on".
        assert!(text.contains("⏵⏵ accept edits"), "canvas=\n{text}");
        assert!(!text.contains("accept edits on"), "canvas=\n{text}");
        assert!(text.contains("shift+tab to cycle"), "canvas=\n{text}");
        // Nyan-specific deviation (product requirement — skip in parity
        // audits): CC appends "esc to interrupt" while loading
        // (PromptInputFooterLeftSide hintParts); Nyan does not clone it.
        assert!(!text.contains("esc to interrupt"), "canvas=\n{text}");
        assert!(!text.contains("? for shortcuts"), "canvas=\n{text}");
    }

    #[component]
    fn FooterWithNotification(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let notifications = hooks.use_state(|| {
            let mut initial = crate::state::app_state_store::AppState::default();
            initial.notifications = std::sync::Arc::new(NotificationsState {
                current: Some(Notification::text(
                    "demo",
                    "footer notification",
                    crate::context::notifications::NotificationPriority::Medium,
                )),
                queue: Vec::new(),
            });
            crate::state::store::AppStore::new(initial, None)
        });
        element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                crate::state::app_state::AppStateProvider(
                    prebuilt_store: Some(notifications.read().clone()),
                    children: crate::state::app_state::ProviderChildren::new(|| element! {
                        Footer(suppress_hint: false)
                    }.into_any()),
                )
            }
        }
    }

    #[test]
    fn footer_places_runtime_notification_in_same_footer_row() {
        let text = element!(FooterWithNotification)
            .render(Some(100))
            .to_string();

        assert!(text.contains("? for shortcuts"), "canvas=\n{text}");
        assert!(text.contains("footer notification"), "canvas=\n{text}");
    }

    #[component]
    fn FooterWithStatusIndicators(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let store = hooks.use_const(|| {
            let mut initial = crate::state::app_state_store::AppState::default();
            initial.verbose = true;
            // no footer_indicators bag (D9)
            crate::state::store::AppStore::new(initial, None)
        });
        // CC: tokenUsage derives from the messages prop (Notifications.tsx:80-83).
        let messages = hooks.use_const(|| {
            std::sync::Arc::new(vec![crate::types::message::Message::Assistant(
                crate::types::message::AssistantMessage {
                    uuid: uuid::Uuid::new_v4().to_string(),
                    timestamp: chrono::Utc::now(),
                    content: vec![crate::types::message::AssistantContent::Text("hi".into())],
                    model: Some("claude".into()),
                    stop_reason: None,
                    usage: Some(crate::types::message::TokenUsage {
                        input_tokens: 70,
                        output_tokens: 7,
                        ..Default::default()
                    }),
                },
            )])
        });
        element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                crate::state::app_state::AppStateProvider(
                    prebuilt_store: Some(store.clone()),
                    children: crate::state::app_state::ProviderChildren::new(move || element! {
                        Footer(debug: true, messages: messages.clone(), suppress_hint: false)
                    }.into_any()),
                )
            }
        }
    }

    #[test]
    fn footer_renders_status_indicators_as_right_side_rows() {
        let text = element!(FooterWithStatusIndicators)
            .render(Some(100))
            .to_string();

        assert!(text.contains("? for shortcuts"), "canvas=\n{text}");
        assert!(text.contains("Debug mode"), "canvas=\n{text}");
        // @nyan: the verbose `N tokens` row moved to the info line (tui.md
        // §3.4), so the debug rows no longer carry it.
        assert!(!text.contains("77 tokens"), "canvas=\n{text}");
        assert!(!text.contains(" tokens"), "canvas=\n{text}");
    }

    #[derive(Default, Props)]
    struct FooterWithTasksPillProps {
        raw_selection: Option<crate::state::app_state_store::FooterItem>,
        tasks_selected: bool,
    }

    /// A tasks pill under an AppState whose raw `footer_selection` may
    /// disagree with the `tasks_selected` prop.
    #[component]
    fn FooterWithTasksPill(
        props: &FooterWithTasksPillProps,
        mut hooks: Hooks,
    ) -> impl Into<AnyElement<'static>> {
        let raw_selection = props.raw_selection;
        let store = hooks.use_const(move || {
            let mut initial = crate::state::app_state_store::AppState::default();
            initial.footer_selection = raw_selection;
            crate::state::store::AppStore::new(initial, None)
        });
        let tasks_selected = props.tasks_selected;
        element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                crate::state::app_state::AppStateProvider(
                    prebuilt_store: Some(store.clone()),
                    children: crate::state::app_state::ProviderChildren::new(move || element! {
                        Footer(
                            suppress_hint: false,
                            background_task_count: 1usize,
                            background_tasks_label: "1 shell".to_string(),
                            tasks_selected: tasks_selected,
                        )
                    }.into_any()),
                )
            }
        }
    }

    /// The background of the tasks pill's first cell.
    fn tasks_pill_background(
        raw_selection: Option<crate::state::app_state_store::FooterItem>,
        tasks_selected: bool,
    ) -> Option<Color> {
        let canvas = element!(
            FooterWithTasksPill(raw_selection: raw_selection, tasks_selected: tasks_selected)
        )
        .render(Some(100));
        let text = canvas.to_string();
        let (y, line) = text
            .lines()
            .enumerate()
            .find(|(_, line)| line.contains("1 shell"))
            .unwrap_or_else(|| panic!("no pill in canvas=\n{text}"));
        let x = line[..line.find("1 shell").unwrap()].chars().count();
        canvas.cell(x, y).and_then(|cell| cell.background_color)
    }

    #[test]
    fn footer_pills_follow_the_owners_selection_not_the_raw_app_state() {
        use crate::state::app_state_store::FooterItem;
        // CC PromptInput.tsx:663-682 hands the pills the owner's effective
        // selection as props; the raw AppState value does not decide it.
        let highlight = Some(theme::current().suggestion);
        assert_eq!(tasks_pill_background(Some(FooterItem::Tasks), false), None);
        assert_eq!(tasks_pill_background(None, true), highlight);
    }
}
