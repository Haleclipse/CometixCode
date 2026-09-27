//! Maps to: CC `components/PromptInput/PromptInputFooter.tsx`.
//!
//! Cometix keeps this footer in normal document flow for the main-screen /
//! native-scrollback path. It mirrors the official row ordering (optional
//! status line, then left-side mode/hints plus right-side notifications) while
//! keeping status-line command execution as the only command-hook special case,
//! with no fullscreen/overlay behavior.

use super::{
    input_modes::PromptInputMode, notifications,
    prompt_input_footer_left_side::PromptInputFooterLeftSide,
};
use crate::components::status_line::{
    StatusLine, status_line_should_display, status_line_should_render,
};
use crate::context::notifications::{NotificationColor, NotificationSegment};
use crate::hooks::use_api_key_verification::VerificationStatus;
use crate::state::app_state::use_app_state;
use crate::types::permissions::PermissionMode;
use crate::utils::permissions::permission_mode::is_default_mode;
use crate::utils::theme::Theme;
use iocraft::prelude::*;

/// Shared segmented footer-row payload (Notifications inline rows + Bridge).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptFooterIndicator {
    pub text: String,
    pub segments: Vec<NotificationSegment>,
    /// Official footer pills can use inverse styling when selected.
    pub inverse: bool,
}

impl PromptFooterIndicator {
    pub(crate) fn from_segments(segments: Vec<NotificationSegment>) -> Self {
        Self::from_segments_with_inverse(segments, false)
    }

    pub(crate) fn from_segments_with_inverse(
        segments: Vec<NotificationSegment>,
        inverse: bool,
    ) -> Self {
        let text = segments
            .iter()
            .map(|segment| segment.text.as_str())
            .collect();
        Self {
            text,
            segments,
            inverse,
        }
    }

    pub(crate) fn dim(text: impl Into<String>) -> Self {
        let text = text.into();
        Self::from_segments(vec![NotificationSegment::text(text).with_dim(true)])
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
    pub api_key_status: VerificationStatus,
    /// Maps to: CC `PromptInputFooter.tsx:46` `autoUpdaterResult` (REPL-owned),
    /// forwarded to `Notifications` (`:208`).
    pub auto_updater_result: Option<crate::utils::auto_updater::AutoUpdaterResult>,
    /// Maps to: CC `PromptInputFooter.tsx:47` `isAutoUpdating` (PromptInput-owned).
    pub is_auto_updating: bool,
    /// Maps to: CC `PromptInputFooter.tsx:49` `onAutoUpdaterResult`.
    pub on_auto_updater_result: Handler<crate::utils::auto_updater::AutoUpdaterResult>,
    /// Maps to: CC `PromptInputFooter.tsx:50` `onChangeIsUpdating`.
    pub on_change_is_updating: Handler<bool>,
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
    pub status_indicator_count: usize,
    pub background_task_count: usize,
    /// Maps to: CC `BackgroundTaskStatus.tsx:199-208` — the pill text is
    /// `getPillLabel(runningTasks)` (`tasks/pillLabel.ts`), computed by the
    /// owner from the same set `background_task_count` gates on.
    pub background_tasks_label: String,
    pub teammate_count: usize,
    pub vim_mode: Option<String>,
    pub mode: PromptInputMode,
    pub is_pasting: bool,
}

/// Single source for "the left side renders a body row for footer pills"
/// (bash banner, vim indicator, tasks pill, teammate pill). The HEIGHT BUDGET
/// and the component's own render gate MUST share this predicate: the
/// 2026-08-07 prompt-box collapse ("2 tasks" under a status line squeezing
/// the input row out) was exactly this condition living in the component but
/// not in the budget — the same divergence class as the status-line-loading
/// collapse documented on `footer_height_for`.
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
    status_indicator_count: usize,
    has_footer_items: bool,
) -> bool {
    exit_hint_visible
        || is_searching
        || notification_row_count > 0
        || status_indicator_count > 0
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
    status_indicator_count: usize,
    has_footer_items: bool,
) -> usize {
    if footer_row_should_render(
        exit_hint_visible,
        is_searching,
        suppress_hint,
        is_loading,
        permission_mode,
        notification_row_count,
        status_indicator_count,
        has_footer_items,
    ) {
        (notification_row_count + status_indicator_count).max(1)
    } else {
        0
    }
}

pub fn footer_height_for(
    status_line_configured: bool,
    exit_hint_visible: bool,
    is_searching: bool,
    suppress_hint: bool,
    is_loading: bool,
    permission_mode: PermissionMode,
    notification_row_count: usize,
    status_indicator_count: usize,
    has_footer_items: bool,
) -> usize {
    // Height MUST track the same condition as the mount
    // (`status_line_should_render`), not the text. They used to differ, on the
    // theory that CC renders nothing while `statusLineText` is undefined — but
    // CC reserves the row (`StatusLine.tsx:397-406`, and says why at `:393`:
    // "a 0→1 row change when the command finishes steals a row"). Tracking the
    // text here meant that while a configured status line was still loading,
    // the hint row was already suppressed on its account
    // (`PromptInputFooter.tsx:136` suppresses on SETTINGS) while its own row
    // was not yet counted — so the footer budget went to zero and the prompt
    // box collapsed.
    usize::from(status_line_should_render(
        status_line_configured,
        exit_hint_visible,
    )) + footer_body_height_for(
        exit_hint_visible,
        is_searching,
        suppress_hint,
        is_loading,
        permission_mode,
        notification_row_count,
        status_indicator_count,
        has_footer_items,
    )
}

#[component]
pub fn Footer(props: &FooterProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    // Maps to: CC StatusLine.tsx:195 `useAppState(s => s.statusLineText)` +
    // PromptInputFooter.tsx:136 `statusLineShouldDisplay(settings)` — both
    // re-derived from live AppState per render, never snapshotted.
    let status_line_text =
        use_app_state(&mut hooks, |state| state.status_line_text.clone()).unwrap_or_default();
    let status_line_configured = use_app_state(&mut hooks, |state| {
        status_line_should_display(&state.settings)
    });
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
        let api_key_status = props.api_key_status;
        let auto_updater_result = props.auto_updater_result.clone();
        let is_auto_updating = props.is_auto_updating;
        let ide_selection = props.ide_selection.clone();
        crate::state::app_state::use_app_state(&mut hooks, move |state| {
            notifications::direct_footer_row_count_from_app(
                state,
                has_current_notification,
                debug,
                api_key_status,
                token_usage,
                auto_updater_result.as_ref(),
                is_auto_updating,
                ide_selection.as_ref(),
            )
        })
    };
    let suppress_hint = props.suppress_hint || status_line_configured || props.is_searching;
    let exit_hint_visible = props.exit_hint.is_some();
    let has_status_line = status_line_should_render(status_line_configured, exit_hint_visible);
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
        props.status_indicator_count,
        has_footer_items,
    );
    let has_footer_row = footer_body_height > 0;
    let has_right_side = notification_row_count > 0 || props.status_indicator_count > 0;

    if !has_status_line && !has_footer_row {
        return element! { View(width: 0u32, height: 0u32) }.into_any();
    }

    element! {
        View(flex_direction: FlexDirection::Column, width: 100pct) {
            #(if has_status_line {
                // Maps to: CC `PromptInputFooter.tsx:179-183` — the source
                // passes `messagesRef`, `lastAssistantMessageId` and `vimMode`;
                // StatusLine reads permission mode / model from AppState itself.
                Some(element! {
                    StatusLine(
                        text: status_line_text.clone(),
                        messages: Some(std::sync::Arc::clone(&props.messages)),
                        last_assistant_message_id: crate::components::status_line::get_last_assistant_message_id(&props.messages),
                        vim_mode: props.vim_mode.clone(),
                    )
                })
            } else {
                None
            })
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
                                                api_key_status: props.api_key_status,
                                                auto_updater_result: props.auto_updater_result.clone(),
                                                is_auto_updating: props.is_auto_updating,
                                                on_auto_updater_result: props.on_auto_updater_result.clone(),
                                                on_change_is_updating: props.on_change_is_updating.clone(),
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
                                    #(if props.status_indicator_count > 0 {
                                        Some(element! { BridgeStatusIndicator })
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

/// Maps to: CC `PromptInputFooter.tsx` `BridgeStatusIndicator`. Reads the
/// flat `AppState.repl_bridge_*` fields and delegates the status label/color
/// to the canonical `bridge_status_util::get_bridge_status` port (CC calls
/// `bridgeStatusUtil.getBridgeStatus`, PromptInputFooter.tsx:257-262);
/// `selected` is `footer_selection == Bridge`.
///
/// The gates are explicit params because they are NOT AppState in CC either:
/// `feature_enabled` mirrors PromptInputFooter.tsx:241 `feature('BRIDGE_MODE')`
/// and `entitlement_enabled` mirrors :255 `isBridgeEnabled()` — both evaluated
/// live at the call sites (`FeatureFlag::BridgeMode` /
/// `bridge::bridge_enabled::is_bridge_enabled`).
pub fn bridge_status_indicator(
    state: &crate::state::app_state_store::AppState,
    feature_enabled: bool,
    entitlement_enabled: bool,
    selected: bool,
) -> Option<PromptFooterIndicator> {
    if !feature_enabled || !entitlement_enabled || !state.repl_bridge_enabled {
        return None;
    }

    // Maps to: CC PromptInputFooter.tsx:257-262 — the footer calls the
    // canonical `bridgeStatusUtil.getBridgeStatus` with `error: undefined`
    // (failed state is surfaced via notification, not a footer pill), so the
    // failed branch is unreachable here.
    let status = crate::bridge::bridge_status_util::get_bridge_status(
        None,
        state.repl_bridge_connected,
        state.repl_bridge_session_active,
        state.repl_bridge_reconnecting,
    );

    // Maps to: CC PromptInputFooter.tsx:265-267 — implicit (config-driven)
    // remote shows only the reconnecting state.
    if !state.repl_bridge_explicit && status.label != "Remote Control reconnecting" {
        return None;
    }

    let mut segments =
        vec![NotificationSegment::text(status.label).with_color(bridge_status_color(status.color))];
    if selected {
        segments.push(NotificationSegment::text(" · Enter to view").with_dim(true));
    }
    Some(PromptFooterIndicator::from_segments_with_inverse(
        segments, selected,
    ))
}

/// Consumer-side mapping from the canonical bridge status color
/// (`bridge/bridge_status_util.rs`, CC `bridgeStatusUtil.ts` color keys) to
/// the footer's segment color vocabulary.
fn bridge_status_color(
    color: crate::bridge::bridge_status_util::BridgeStatusColor,
) -> NotificationColor {
    match color {
        crate::bridge::bridge_status_util::BridgeStatusColor::Error => NotificationColor::Error,
        crate::bridge::bridge_status_util::BridgeStatusColor::Warning => NotificationColor::Warning,
        crate::bridge::bridge_status_util::BridgeStatusColor::Success => NotificationColor::Success,
    }
}

/// Bridge-only right-side stack count (CC `BridgeStatusIndicator`). The
/// feature gate is a cheap constant; the entitlement is lazy.
///
/// Documented deviation from CC's operand order: CC evaluates
/// `!isBridgeEnabled() || !enabled` (PromptInputFooter.tsx:255) with an
/// in-memory-cached `isBridgeEnabled`. The Rust entitlement read performs
/// real config/token-file/keychain IO per call, so the cheap
/// `repl_bridge_enabled` AppState gate short-circuits first — the conjunction
/// result is identical, and hot render frames stay free of auth IO (pinned by
/// `logo_header_hot_path_avoids_uncached_auth_io...` in screens/repl.rs).
pub fn bridge_status_indicator_count_from_app(
    state: &crate::state::app_state_store::AppState,
    feature_enabled: bool,
    entitlement_enabled: impl FnOnce() -> bool,
) -> usize {
    if !feature_enabled || !state.repl_bridge_enabled {
        return 0;
    }
    usize::from(
        bridge_status_indicator(
            state,
            feature_enabled,
            entitlement_enabled(),
            state.footer_selection == Some(crate::state::app_state_store::FooterItem::Bridge),
        )
        .is_some(),
    )
}

fn indicator_color(color: Option<NotificationColor>, theme: &Theme) -> Color {
    match color {
        Some(NotificationColor::Error) => theme.error,
        Some(NotificationColor::Warning) => theme.warning,
        Some(NotificationColor::Success) => theme.success,
        Some(NotificationColor::Claude) => theme.claude,
        Some(NotificationColor::Text) => theme.text,
        Some(NotificationColor::Ide) => theme.ide,
        Some(NotificationColor::Suggestion) => theme.suggestion,
        Some(NotificationColor::FastMode) => theme.fast_mode,
        None => theme.inactive,
    }
}

#[derive(Default, Props)]
pub struct BridgeStatusIndicatorProps;

/// Maps to: CC `PromptInputFooter.tsx` inline `BridgeStatusIndicator`.
#[component]
pub fn BridgeStatusIndicator(
    _props: &BridgeStatusIndicatorProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    // Maps to: CC PromptInputFooter.tsx:241 `if (!feature('BRIDGE_MODE'))
    // return null` — build gate checked before any state read.
    if !crate::utils::feature_flags::feature_enabled(
        crate::utils::feature_flags::FeatureFlag::BridgeMode,
    ) {
        return element! { View(width: 0u32, height: 0u32) }.into_any();
    }
    let theme = hooks.use_context::<Theme>();
    let indicator = use_app_state(&mut hooks, |state| {
        // Cheap AppState gate first, then the CC PromptInputFooter.tsx:255
        // `isBridgeEnabled()` entitlement. Same documented operand-order
        // deviation as `bridge_status_indicator_count_from_app`: the Rust
        // entitlement read does real config/keychain IO, and `&&` keeps the
        // result identical.
        if !state.repl_bridge_enabled {
            return None;
        }
        bridge_status_indicator(
            state,
            true,
            crate::bridge::bridge_enabled::is_bridge_enabled(),
            state.footer_selection == Some(crate::state::app_state_store::FooterItem::Bridge),
        )
    });

    let Some(indicator) = indicator else {
        return element! { View(width: 0u32, height: 0u32) }.into_any();
    };

    element! {
        View(flex_direction: FlexDirection::Row, height: 1u32, overflow: Overflow::Hidden) {
            #(indicator.segments.iter().map(|segment| {
                // Maps to: CC PromptInputFooter.tsx:270-277 — the selected
                // pill renders `color={bridgeSelected ? 'background' :
                // status.color}` with `inverse`; the nested dim suffix sits
                // inside the same background-keyed inverse Text.
                let segment_color = if indicator.inverse {
                    theme.background
                } else {
                    indicator_color(segment.color, &theme)
                };
                element! {
                    Text(
                        content: segment.text.clone(),
                        color: segment_color,
                        dim: segment.dim,
                        invert: indicator.inverse,
                        wrap: TextWrap::NoWrap,
                    )
                }
            }))
        }
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::notifications::{Notification, NotificationsState};
    use crate::utils::settings::types::{SettingsJson, StatusLineSettings};
    use crate::utils::theme;

    #[test]
    fn footer_height_matches_statusline_hint_and_notification_rows() {
        assert_eq!(
            footer_height_for(
                true,
                false,
                false,
                true,
                false,
                PermissionMode::Default,
                0,
                0,
                false,
            ),
            1,
            "configured status line with text suppresses shortcut hint and owns one row"
        );
        assert_eq!(
            footer_height_for(
                false,
                false,
                false,
                false,
                true,
                PermissionMode::Default,
                1,
                0,
                false,
            ),
            1,
            "loading hint and notification share the official footer row"
        );
        assert_eq!(
            footer_height_for(
                false,
                false,
                false,
                true,
                false,
                PermissionMode::Plan,
                0,
                0,
                false,
            ),
            1,
            "active permission mode renders even when shortcut hints are suppressed"
        );
        assert_eq!(
            footer_height_for(
                false,
                false,
                false,
                true,
                false,
                PermissionMode::Default,
                0,
                2,
                false,
            ),
            2,
            "right-side status indicators keep their official stacked footer rows"
        );
    }

    #[test]
    fn footer_height_budgets_the_pill_row_alongside_a_status_line() {
        // Regression: the 2026-08-07 prompt-box collapse. A configured status
        // line suppresses hints (budget row 1 = status line), and the tasks
        // pill renders a body row the old budget did not count — the input
        // row got squeezed out. The pill predicate must add the body row.
        assert_eq!(
            footer_height_for(
                true,
                false,
                false,
                true,
                false,
                PermissionMode::Default,
                0,
                0,
                true,
            ),
            2,
            "status line row + footer-pill body row must both be budgeted"
        );
        assert_eq!(
            footer_height_for(
                false,
                false,
                false,
                true,
                false,
                PermissionMode::Default,
                0,
                0,
                true,
            ),
            1,
            "footer pills alone own one body row"
        );
        assert!(footer_has_items(PromptInputMode::Prompt, false, 2, 0));
        assert!(footer_has_items(PromptInputMode::Bash, false, 0, 0));
        assert!(!footer_has_items(PromptInputMode::Prompt, false, 0, 0));
    }

    #[component]
    fn FooterWithStatusLine(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let status_line_store = hooks.use_state(|| {
            let mut initial = crate::state::app_state_store::AppState::default();
            initial.status_line_text = Some("\u{1b}[32mrepo main\u{1b}[0m".to_string());
            // Hint suppression + padding derive from settings (CC
            // PromptInputFooter.tsx:136 / StatusLine.tsx:391).
            initial.settings = std::sync::Arc::new(SettingsJson {
                status_line: Some(StatusLineSettings {
                    kind: Some("command".to_string()),
                    command: "printf ok".to_string(),
                    padding: Some(1),
                }),
                ..Default::default()
            });
            crate::state::store::AppStore::new(initial, None)
        });
        element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                crate::state::app_state::AppStateProvider(
                    prebuilt_store: Some(status_line_store.read().clone()),
                    children: crate::state::app_state::ProviderChildren::new(|| element! {
                        Footer(
                            suppress_hint: false,
                            permission_mode: PermissionMode::Default,
                        )
                    }.into_any()),
                )
            }
        }
    }

    #[test]
    fn footer_renders_statusline_above_left_side_and_suppresses_default_hint() {
        let text = element!(FooterWithStatusLine).render(Some(80)).to_string();

        assert!(text.contains("repo main"), "canvas=\n{text}");
        assert!(!text.contains("? for shortcuts"), "canvas=\n{text}");
    }

    #[component]
    fn FooterWithMixedStatusLine(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let status_line_store = hooks.use_state(|| {
            let mut initial = crate::state::app_state_store::AppState::default();
            initial.status_line_text = Some("plain \u{1b}[32mgreen\u{1b}[0m".to_string());
            // Configured status line with no padding (CC StatusLine.tsx:391
            // `?? 0` default keeps column 0 assertions valid).
            initial.settings = std::sync::Arc::new(SettingsJson {
                status_line: Some(StatusLineSettings {
                    kind: Some("command".to_string()),
                    command: "printf ok".to_string(),
                    padding: None,
                }),
                ..Default::default()
            });
            crate::state::store::AppStore::new(initial, None)
        });
        element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                crate::state::app_state::AppStateProvider(
                    prebuilt_store: Some(status_line_store.read().clone()),
                    children: crate::state::app_state::ProviderChildren::new(|| element! {
                        Footer(
                            suppress_hint: true,
                            permission_mode: PermissionMode::Default,
                        )
                    }.into_any()),
                )
            }
        }
    }

    #[test]
    fn footer_statusline_uses_inactive_default_color_without_forcing_dim() {
        let theme = *theme::current();
        let canvas = element!(FooterWithMixedStatusLine).render(Some(80));
        let text = canvas.to_string();

        assert!(text.starts_with("plain green"), "canvas=\n{text}");
        let plain_style = canvas.resolved_text_style(0, 0).expect("plain style");
        assert_eq!(plain_style.color, Some(theme.inactive));
        assert_eq!(plain_style.weight, Weight::Normal);

        let green_style = canvas.resolved_text_style(6, 0).expect("green style");
        assert_eq!(green_style.color, Some(Color::DarkGreen));
        assert_eq!(green_style.weight, Weight::Normal);
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

        // Cometix-specific deviation (product requirement — skip in parity
        // audits): CC renders "accept edits on"; Cometix drops the " on".
        assert!(text.contains("⏵⏵ accept edits"), "canvas=\n{text}");
        assert!(!text.contains("accept edits on"), "canvas=\n{text}");
        assert!(text.contains("shift+tab to cycle"), "canvas=\n{text}");
        // Cometix-specific deviation (product requirement — skip in parity
        // audits): CC appends "esc to interrupt" while loading
        // (PromptInputFooterLeftSide hintParts); Cometix does not clone it.
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
        assert!(text.contains("77 tokens"), "canvas=\n{text}");
    }

    /// Flat AppState fixture for bridge visibility tests (CC AppStateStore.ts
    /// replBridge* fields). Truthful seam: production has no producer that
    /// sets `repl_bridge_enabled` true (CC settings-screen toggle and
    /// useReplBridge transport are unported), so these fixture states are
    /// test-only reachable even though the two gates are now live-computed
    /// (BridgeMode build gate is on; the isBridgeEnabled entitlement can be
    /// genuinely true for a claude.ai subscriber with a cached
    /// `tengu_ccr_bridge` GrowthBook value).
    fn bridge_fixture(
        enabled: bool,
        explicit: bool,
        connected: bool,
        session_active: bool,
        reconnecting: bool,
    ) -> crate::state::app_state_store::AppState {
        let mut state = crate::state::app_state_store::AppState::default();
        state.repl_bridge_enabled = enabled;
        state.repl_bridge_explicit = explicit;
        state.repl_bridge_connected = connected;
        state.repl_bridge_session_active = session_active;
        state.repl_bridge_reconnecting = reconnecting;
        state
    }

    #[test]
    fn bridge_status_indicator_matches_official_footer_visibility_and_copy() {
        // Gate args mirror PromptInputFooter.tsx:241 feature('BRIDGE_MODE')
        // and :255 isBridgeEnabled().
        let explicit_connected = bridge_fixture(true, true, true, false, false);
        assert!(bridge_status_indicator(&explicit_connected, false, true, false).is_none());
        assert!(bridge_status_indicator(&explicit_connected, true, false, false).is_none());
        assert!(
            bridge_status_indicator(
                &bridge_fixture(false, true, true, false, false),
                true,
                true,
                false,
            )
            .is_none()
        );
        assert!(
            bridge_status_indicator(
                &bridge_fixture(true, false, true, false, false),
                true,
                true,
                false,
            )
            .is_none()
        );

        let active = bridge_status_indicator(
            &bridge_fixture(true, true, false, true, false),
            true,
            true,
            false,
        )
        .unwrap();
        assert_eq!(active.text, "Remote Control active");
        assert_eq!(active.segments[0].color, Some(NotificationColor::Success));
        assert!(!active.inverse);

        let connecting = bridge_status_indicator(
            &bridge_fixture(true, true, false, false, false),
            true,
            true,
            false,
        )
        .unwrap();
        assert_eq!(connecting.text, "Remote Control connecting…");
        assert_eq!(
            connecting.segments[0].color,
            Some(NotificationColor::Warning)
        );
    }

    #[test]
    fn bridge_status_indicator_allows_implicit_reconnecting_and_selected_suffix() {
        let reconnecting = bridge_status_indicator(
            &bridge_fixture(true, false, false, false, true),
            true,
            true,
            true,
        )
        .unwrap();

        assert_eq!(
            reconnecting.text,
            "Remote Control reconnecting · Enter to view"
        );
        assert_eq!(
            reconnecting.segments[0].color,
            Some(NotificationColor::Warning)
        );
        assert!(reconnecting.segments[1].dim);
        assert!(reconnecting.inverse);
    }
}
