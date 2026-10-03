//! Maps to: CC `components/PromptInput/Notifications.tsx` +
//! `context/notifications.tsx` for the main-screen-safe subset.
//!
//! Status rows match CC `NotificationContent`: inline overage/apiKeyHelper/auth/
//! debug/verbose; mounts of root components (`IdeStatusIndicator`, `TokenWarning`,
//! `AutoUpdaterWrapper`, `MemoryUsageIndicator`) and PromptInput-local
//! `SandboxPromptFooterHint`. No parallel `*_footer.rs` adapter layer.

use super::prompt_input_footer::PromptFooterIndicator;
use crate::components::auto_updater_wrapper::{
    AutoUpdaterWrapper, auto_update_hint_row_count_from_app,
};
use crate::utils::auto_updater::InstallStatus;

/// Maps to: CC `PromptInput/Notifications.tsx:119-122` `shouldShowAutoUpdater`
/// — defined by its consumer (this file), imported by the wrapper's height
/// helper (O-2: owner direction mirrors CC).
pub fn should_show_auto_updater(
    ide_selection_visible: bool,
    is_updating: bool,
    result_status: Option<InstallStatus>,
) -> bool {
    !ide_selection_visible || is_updating || result_status != Some(InstallStatus::Success)
}
use crate::components::ide_status_indicator::{
    IdeStatusIndicator, ide_hint_row_count, ide_status_indicator_text,
};
use crate::components::token_warning::{TokenWarning, token_warning_hint_row_count};
use crate::context::notifications::use_notifications;
use crate::context::notifications::{NotificationColor, NotificationSegment};
use crate::hooks::notifs::ide_status_indicator::IdeConnectionStatus;
use crate::hooks::use_api_key_verification::VerificationStatus;
use crate::services::compact::auto_compact::is_auto_compact_enabled;
use crate::state::store::AppStore;
use crate::utils::auth::{get_api_key_helper_elapsed_ms, get_subscription_type};
use crate::utils::config::load_global_config;
use crate::utils::env_utils::is_env_truthy;
use crate::utils::format::format_duration;
use crate::utils::theme::Theme;
use iocraft::prelude::*;
use std::time::Duration;

/// Maps to: CC `Notifications.tsx` slow apiKeyHelper warning threshold.
pub const API_KEY_HELPER_SLOW_THRESHOLD_MS: u64 = 10_000;

const DEFAULT_MODEL: &str = "claude-sonnet-4-5";

/// Maps to: CC `Notifications.tsx` overage-mode footer row.
pub fn overage_indicator(
    is_in_overage_mode: bool,
    is_team_or_enterprise: bool,
) -> Option<PromptFooterIndicator> {
    if is_in_overage_mode && !is_team_or_enterprise {
        Some(PromptFooterIndicator::dim("Now using extra usage"))
    } else {
        None
    }
}

/// Maps to: CC `Notifications.tsx` auth-error footer row.
pub fn auth_status_indicator(
    api_key_status: VerificationStatus,
    is_remote_mode: bool,
) -> Option<PromptFooterIndicator> {
    if !matches!(
        api_key_status,
        VerificationStatus::Invalid | VerificationStatus::Missing
    ) {
        return None;
    }

    Some(PromptFooterIndicator::colored(
        if is_remote_mode {
            "Authentication error · Try again"
        } else {
            "Not logged in · Run /login"
        },
        NotificationColor::Error,
    ))
}

/// Maps to: CC `Notifications.tsx` slow apiKeyHelper warning row (inline).
pub fn api_key_helper_slow_indicator(elapsed_ms: Option<u64>) -> Option<PromptFooterIndicator> {
    let elapsed_ms = elapsed_ms.filter(|ms| *ms >= API_KEY_HELPER_SLOW_THRESHOLD_MS)?;
    let duration = format_duration(elapsed_ms);
    Some(PromptFooterIndicator::from_segments(vec![
        NotificationSegment::text("apiKeyHelper is taking a while ")
            .with_color(NotificationColor::Warning),
        NotificationSegment::text(format!("({duration})")).with_dim(true),
    ]))
}

fn configured_api_key_helper_present(store: &AppStore) -> bool {
    let state = store.get();
    if state
        .settings
        .api_key_helper
        .as_ref()
        .is_some_and(|value| !value.trim().is_empty())
    {
        return true;
    }
    load_global_config()
        .api_key_helper
        .as_ref()
        .is_some_and(|value| !value.trim().is_empty())
}

/// Height budget for Notifications rows. Wake-dependent rows (apiKeyHelper /
/// memory / sandbox) are computed live — no mirrored bag (D9).
pub fn direct_footer_row_count(
    has_current_notification: bool,
    verbose: bool,
    is_brief_only: bool,
    main_loop_model: Option<&str>,
    api_key_status: VerificationStatus,
    token_usage: u64,
    api_key_helper_slow: bool,
    memory_visible: bool,
    sandbox_hint_visible: bool,
) -> usize {
    let mut count = usize::from(has_current_notification);
    count += usize::from(api_key_helper_slow);
    if !matches!(
        api_key_status,
        VerificationStatus::Invalid | VerificationStatus::Missing
    ) && verbose
    {
        count += 1;
    }
    let model = main_loop_model.unwrap_or(DEFAULT_MODEL);
    count += token_warning_hint_row_count(token_usage, model, is_brief_only);
    count += usize::from(memory_visible);
    count += usize::from(sandbox_hint_visible);
    count
}

fn api_key_helper_slow_now(store: &AppStore) -> bool {
    if !configured_api_key_helper_present(store) {
        return false;
    }
    get_api_key_helper_elapsed_ms() >= API_KEY_HELPER_SLOW_THRESHOLD_MS
}

/// Maps to: CC `components/PromptInput/Notifications.tsx:66-190` `Notifications`
/// and `:199-359` `NotificationContent` row visibility.
/// L1 (React/Ink → iocraft): projects the same visible branches into retained
/// footer height without moving their auth, subscription, or environment policy.
/// `token_usage` is caller-derived from that caller's `messages` prop via
/// [`token_usage_from_messages`] (CC Notifications.tsx:80-83 — not AppState).
/// `auto_updater_result`/`is_auto_updating` are caller-threaded from their
/// REPL/PromptInput owners (CC Notifications.tsx:53-55 props — not AppState).
/// `ide_selection` is the REPL-owned selection state, prop-drilled through
/// PromptInput/Footer (CC Notifications.tsx:60,75 props — not AppState).
pub fn direct_footer_row_count_from_app(
    state: &crate::state::app_state_store::AppState,
    has_current_notification: bool,
    debug: bool,
    api_key_status: VerificationStatus,
    token_usage: u64,
    auto_updater_result: Option<&crate::utils::auto_updater::AutoUpdaterResult>,
    is_auto_updating: bool,
    ide_selection: Option<&crate::hooks::use_ide_selection::IdeSelection>,
) -> usize {
    let sandbox_enabled =
        crate::utils::sandbox::sandbox_adapter::get_sandbox_enabled_setting(&state.settings);
    let sandbox_hint_visible =
        sandbox_enabled && super::sandbox_prompt_footer_hint::sandbox_hint_recent_count() > 0;
    let memory_visible = crate::components::memory_usage_indicator::memory_hint_visible_now();
    // apiKeyHelper: prefer live elapsed; store handle optional at height sites
    // that only have AppState (tests pass false via direct_footer_row_count).
    let api_key_helper_slow = {
        let helper_configured = state
            .settings
            .api_key_helper
            .as_ref()
            .is_some_and(|value| !value.trim().is_empty())
            || load_global_config()
                .api_key_helper
                .as_ref()
                .is_some_and(|value| !value.trim().is_empty());
        helper_configured && get_api_key_helper_elapsed_ms() >= API_KEY_HELPER_SLOW_THRESHOLD_MS
    };
    let mut count = direct_footer_row_count(
        has_current_notification,
        state.verbose,
        state.is_brief_only,
        state.main_loop_model.as_deref(),
        api_key_status,
        token_usage,
        api_key_helper_slow,
        memory_visible,
        sandbox_hint_visible,
    );
    // Documented deviation from CC's operand order, identical in shape to
    // `prompt_input_footer::bridge_status_indicator_count_from_app`: CC reads
    // an in-memory `getSubscriptionType()`, while the Rust call performs real
    // token-file/keychain/settings IO. `overage_indicator` returns `None`
    // whenever `is_using_overage` is false, so gating on the cheap AppState
    // field first yields an identical result and keeps hot render frames free
    // of auth IO (pinned by `logo_header_hot_path_avoids_uncached_auth_io...`
    // in screens/repl.rs, which only reaches this path now that REPL test
    // harnesses mount a real AppStateProvider).
    if state.claude_ai_limits.is_using_overage {
        let subscription_type = get_subscription_type();
        let is_team_or_enterprise =
            matches!(subscription_type.as_deref(), Some("team" | "enterprise"));
        if overage_indicator(
            state.claude_ai_limits.is_using_overage,
            is_team_or_enterprise,
        )
        .is_some()
        {
            count += 1;
        }
    }
    let is_remote_mode = is_env_truthy(crate::utils::process_env::var("CLAUDE_CODE_REMOTE").as_deref());
    if auth_status_indicator(api_key_status, is_remote_mode).is_some() {
        count += 1;
    }
    if debug {
        count += 1;
    }
    let ide_connected = crate::components::ide_status_indicator::ide_client_connected(&state.mcp);
    let ide_status = ide_connected.then_some(IdeConnectionStatus::Connected);
    let ide_rows = ide_hint_row_count(ide_status, ide_selection);
    count += ide_rows;
    count += auto_update_hint_row_count_from_app(
        state,
        auto_updater_result,
        is_auto_updating,
        ide_rows > 0,
    );
    count
}

/// Maps to: CC `Notifications.tsx:80-83` `tokenUsage` useMemo body:
/// `tokenCountFromLastAPIResponse(getMessagesAfterCompactBoundary(messages))`.
pub fn token_usage_from_messages(messages: &[crate::types::message::Message]) -> u64 {
    let messages_for_token_count =
        crate::utils::messages::get_messages_after_compact_boundary(messages);
    crate::utils::tokens::token_count_from_last_api_response(&messages_for_token_count).max(0)
        as u64
}

/// P6: overage flag (CC `useClaudeAiLimits().isUsingOverage`).
pub fn set_overage_mode(store: &AppStore, enabled: bool) {
    store.replace_with(|state| {
        let mut limits = (*state.claude_ai_limits).clone();
        limits.is_using_overage = enabled;
        state.claude_ai_limits = std::sync::Arc::new(limits);
    });
}

/// P6: apply limits service snapshot to AppState overage flag.
pub fn apply_claude_ai_limits(
    store: &AppStore,
    limits: &crate::services::claude_ai_limits::ClaudeAiLimits,
) {
    store.replace_with(|state| {
        state.claude_ai_limits = std::sync::Arc::new(limits.clone());
    });
}

fn notification_color(color: Option<NotificationColor>, theme: &Theme) -> Color {
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
pub struct NotificationsProps {
    /// Maps to: CC `NotificationsProps.apiKeyStatus`, passed from REPL through
    /// PromptInput rather than read from AppState.
    pub api_key_status: VerificationStatus,
    /// Maps to: CC `NotificationsProps.autoUpdaterResult` (Notifications.tsx:53)
    /// — REPL-owned state, prop-drilled through PromptInput/Footer.
    pub auto_updater_result: Option<crate::utils::auto_updater::AutoUpdaterResult>,
    /// Maps to: CC `NotificationsProps.isAutoUpdating` (Notifications.tsx:54)
    /// — PromptInput-owned state.
    pub is_auto_updating: bool,
    /// Maps to: CC `NotificationsProps.onAutoUpdaterResult` (Notifications.tsx:58)
    /// — REPL `setAutoUpdaterResult`, forwarded to `AutoUpdaterWrapper`.
    pub on_auto_updater_result: Handler<crate::utils::auto_updater::AutoUpdaterResult>,
    /// Maps to: CC `NotificationsProps.onChangeIsUpdating` (Notifications.tsx:59)
    /// — PromptInput `setIsAutoUpdating`, forwarded to `AutoUpdaterWrapper`.
    pub on_change_is_updating: Handler<bool>,
    /// Maps to: CC `NotificationsProps.debug`, forwarded from REPL launch props.
    pub debug: bool,
    /// Maps to: CC NotificationsProps.messages (Notifications.tsx:57,72).
    pub messages: std::sync::Arc<Vec<crate::types::message::Message>>,
    /// Maps to: CC `NotificationsProps.ideSelection` (Notifications.tsx:60,75)
    /// — REPL-owned state (REPL.tsx:1111), prop-drilled through
    /// PromptInput/Footer; never read from AppState.
    pub ide_selection: Option<crate::hooks::use_ide_selection::IdeSelection>,
    /// Hide the visible row while completion suggestions own the footer area.
    pub suppressed: bool,
    /// Render as the official footer right-side item instead of as a full-width
    /// document-flow row. This stays main-screen safe; it only changes layout.
    pub inline: bool,
}

#[component]
pub fn Notifications(
    props: &NotificationsProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let context = use_notifications(&mut hooks);
    // Maps to: CC `useNotification()` — `useAppState(s => s.notifications.current)`.
    // Read here, above the `props.suppressed` early return: this is a hook, and
    // iocraft resolves hooks by call index, so a read placed after the return
    // would be skipped on suppressed renders and shift every later hook.
    let notification = crate::state::app_state::use_app_state(&mut hooks, |state| {
        state.notifications.current.clone()
    });
    let verbose = crate::state::app_state::use_app_state(&mut hooks, |state| state.verbose);
    let is_brief_only =
        crate::state::app_state::use_app_state(&mut hooks, |state| state.is_brief_only);
    let main_loop_model = crate::state::app_state::use_app_state(&mut hooks, |state| {
        state
            .main_loop_model
            .clone()
            .unwrap_or_else(|| DEFAULT_MODEL.to_string())
    });
    // Maps to: CC Notifications.tsx:75 — ideSelection arrives as a prop from
    // the REPL owner; only the connected status derives from AppState.mcp
    // (CC `useIdeConnectionStatus(mcpClients)`).
    let ide_selection = props.ide_selection.clone();
    let ide_connected = crate::state::app_state::use_app_state(&mut hooks, |state| {
        crate::components::ide_status_indicator::ide_client_connected(&state.mcp)
    });
    let api_key_status = props.api_key_status;
    let is_in_overage_mode = crate::state::app_state::use_app_state(&mut hooks, |state| {
        state.claude_ai_limits.is_using_overage
    });
    let debug = props.debug;
    // Maps to: CC Notifications.tsx:80-83 tokenUsage useMemo. Dependency is the
    // messages Arc identity, matching CC's `[messages]` reference-equality dep.
    let token_usage = hooks.use_memo(
        {
            let messages = std::sync::Arc::clone(&props.messages);
            move || token_usage_from_messages(&messages)
        },
        std::sync::Arc::as_ptr(&props.messages) as usize,
    );
    let is_remote_mode = is_env_truthy(crate::utils::process_env::var("CLAUDE_CODE_REMOTE").as_deref());
    // Same documented operand-order deviation as
    // `direct_footer_row_count_from_app` / the bridge indicator: CC's
    // `getSubscriptionType()` is an in-memory read, the Rust one performs
    // token-file/keychain/settings IO. `overage_indicator` is `None` whenever
    // `is_in_overage_mode` is false, so the cheap AppState gate runs first and
    // hot frames stay free of auth IO. Result is identical.
    let is_team_or_enterprise = is_in_overage_mode
        && matches!(
            get_subscription_type().as_deref(),
            Some("team" | "enterprise")
        );
    let theme = hooks.use_context::<Theme>();
    let store = hooks.try_use_context::<AppStore>().map(|s| s.clone());

    // Maps to: CC NotificationContent local `apiKeyHelperSlow` + 1s poll
    // (Notifications.tsx:237-250). Local state owns the display ms; the
    // PromptInput-scoped FooterLayoutWake is bumped on slow-visibility flips
    // only, so the height budget re-computes without waking AppStore.
    let footer_layout_wake = hooks
        .try_use_context::<super::footer_layout_wake::FooterLayoutWake>()
        .map(|wake| wake.clone());
    let api_key_helper_elapsed = hooks.use_state(|| Option::<u64>::None);
    hooks.use_future({
        let mut api_key_helper_elapsed = api_key_helper_elapsed;
        let store = store.clone();
        let footer_layout_wake = footer_layout_wake.clone();
        async move {
            let Some(store) = store else {
                return;
            };
            let mut was_slow = false;
            loop {
                let slow = api_key_helper_slow_now(&store);
                if !configured_api_key_helper_present(&store) {
                    if api_key_helper_elapsed.read().is_some() {
                        api_key_helper_elapsed.set(None);
                    }
                } else {
                    let ms = get_api_key_helper_elapsed_ms();
                    api_key_helper_elapsed.set(Some(ms));
                }
                if was_slow != slow {
                    was_slow = slow;
                    if let Some(wake) = footer_layout_wake.as_ref() {
                        wake.bump();
                    }
                }
                futures_timer::Delay::new(Duration::from_secs(1)).await;
            }
        }
    });

    if props.suppressed {
        return element! { View(width: 0u32, height: 0u32) }.into_any();
    }

    let api_key_elapsed = api_key_helper_elapsed.read().clone();

    let mut head_rows: Vec<FooterRow> = Vec::new();
    let mut mid_rows: Vec<FooterRow> = Vec::new();
    let mut tail_rows: Vec<FooterRow> = Vec::new();

    if let Some(current) = notification {
        let segments = if current.segments.is_empty() {
            vec![NotificationSegment::text(current.text.clone())]
        } else {
            current.segments.clone()
        };
        head_rows.push(FooterRow {
            segments,
            inverse: false,
            default_color: current.color,
        });
    }

    if let Some(indicator) = overage_indicator(is_in_overage_mode, is_team_or_enterprise) {
        head_rows.push(FooterRow::from_indicator(indicator));
    }

    if let Some(indicator) = api_key_helper_slow_indicator(api_key_elapsed) {
        mid_rows.push(FooterRow::from_indicator(indicator));
    }

    if let Some(indicator) = auth_status_indicator(api_key_status, is_remote_mode) {
        tail_rows.push(FooterRow::from_indicator(indicator));
    }
    if debug {
        tail_rows.push(FooterRow::from_indicator(PromptFooterIndicator::colored(
            "Debug mode",
            NotificationColor::Warning,
        )));
    }
    if !matches!(
        api_key_status,
        VerificationStatus::Invalid | VerificationStatus::Missing
    ) && verbose
    {
        tail_rows.push(FooterRow::from_indicator(PromptFooterIndicator::dim(
            format!("{token_usage} tokens"),
        )));
    }

    let ide_status = ide_connected.then_some(IdeConnectionStatus::Connected);
    let ide_visible = ide_status_indicator_text(ide_status, ide_selection.as_ref()).is_some();
    // Maps to: CC Notifications.tsx:119-122 — gate from the prop-drilled
    // REPL/PromptInput values (no AppState involvement).
    let show_auto_updater = should_show_auto_updater(
        ide_visible,
        props.is_auto_updating,
        props.auto_updater_result.as_ref().map(|r| r.status),
    );
    // Maps to: CC `showSuccessMessage={!isShowingCompactMessage}`.
    let show_success = crate::services::compact::auto_compact::calculate_token_warning_state(
        token_usage as i64,
        &main_loop_model,
    )
    .is_above_warning_threshold
        == false;

    let (col_pl, col_pr) = if props.inline {
        (0u32, 0u32)
    } else {
        (2u32, 1u32)
    };
    element! {
        View(
            flex_direction: FlexDirection::Column,
            flex_shrink: 1.0f32,
            padding_left: col_pl,
            padding_right: col_pr,
            overflow: Overflow::Hidden,
        ) {
            // CC order: Ide → notif/overage → apiKeyHelper → auth/debug/verbose
            // → TokenWarning → AutoUpdater → Memory → Sandbox.
            IdeStatusIndicator(
                ide_status: ide_status,
                ide_selection: ide_selection,
            )
            #(head_rows.into_iter().map(|row| footer_row_view(row, &theme)))
            #(mid_rows.into_iter().map(|row| footer_row_view(row, &theme)))
            #(tail_rows.into_iter().map(|row| footer_row_view(row, &theme)))
            #(if !is_brief_only {
                Some(element! {
                    TokenWarning(
                        token_usage: token_usage as i64,
                        model: main_loop_model.clone(),
                        auto_compact_enabled: Some(is_auto_compact_enabled()),
                    )
                })
            } else {
                None
            })
            #(if show_auto_updater {
                // Maps to: CC Notifications.tsx:343-349 — forward the CC
                // callback/value props; showSuccessMessage={!isShowingCompactMessage}.
                Some(element! {
                    AutoUpdaterWrapper(
                        is_updating: props.is_auto_updating,
                        auto_updater_result: props.auto_updater_result.clone(),
                        on_auto_updater_result: props.on_auto_updater_result.clone(),
                        on_change_is_updating: props.on_change_is_updating.clone(),
                        show_success_message: show_success,
                        verbose: verbose,
                    )
                })
            } else {
                None
            })
            crate::components::memory_usage_indicator::MemoryUsageIndicator
            super::sandbox_prompt_footer_hint::SandboxPromptFooterHint
        }
    }
    .into_any()
}

fn footer_row_view(row: FooterRow, theme: &Theme) -> AnyElement<'static> {
    let FooterRow {
        segments,
        inverse,
        default_color,
    } = row;
    element! {
        View(
            flex_direction: FlexDirection::Row,
            height: 1u32,
            flex_shrink: 1.0f32,
            overflow: Overflow::Hidden,
        ) {
            #(segments.into_iter().map(|segment| {
                let segment_color =
                    notification_color(segment.color.or(default_color), theme);
                element! {
                    Text(
                        content: segment.text.clone(),
                        color: segment_color,
                        dim: segment.dim,
                        invert: inverse,
                        wrap: TextWrap::NoWrap,
                    )
                }
            }))
        }
    }
    .into_any()
}

struct FooterRow {
    segments: Vec<NotificationSegment>,
    inverse: bool,
    default_color: Option<NotificationColor>,
}

impl FooterRow {
    fn from_indicator(indicator: PromptFooterIndicator) -> Self {
        Self {
            segments: indicator.segments,
            inverse: indicator.inverse,
            default_color: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::notifications::{Notification, NotificationPriority, NotificationsState};
    use crate::utils::auto_updater::{AutoUpdaterResult, InstallStatus};
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK, TestEnvGuard};
    use std::path::PathBuf;

    const NOTIFICATIONS_RUNTIME_ENV_KEYS: &[&str] = &[
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "ANTHROPIC_UNIX_SOCKET",
        "CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR",
        "CLAUDE_CODE_MANAGED_SETTINGS_PATH",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR",
        "CLAUDE_CODE_REMOTE",
        "CLAUDE_CODE_SIMPLE",
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_FOUNDRY",
        "CLAUDE_CODE_USE_VERTEX",
        "CLAUDE_CONFIG_DIR",
        "NODE_ENV",
    ];

    struct NotificationsRuntimeFixture {
        _lock: TestEnvGuard<'static>,
        previous_env: Vec<EnvVarGuard>,
        previous_config: Option<crate::utils::config::GlobalConfig>,
        previous_cwd: PathBuf,
        previous_original_cwd: PathBuf,
        previous_allowed_sources: Vec<String>,
        previous_flag_settings_path: Option<PathBuf>,
        previous_flag_settings_inline: Option<serde_json::Value>,
        root: PathBuf,
    }

    impl NotificationsRuntimeFixture {
        fn new(subscription_type: Option<&str>, remote: bool) -> Self {
            let lock = TEST_ENV_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let previous_env = NOTIFICATIONS_RUNTIME_ENV_KEYS
                .iter()
                .map(|key| EnvVarGuard::unset(*key))
                .collect::<Vec<_>>();
            let previous_cwd = std::env::current_dir().expect("current cwd");
            let previous_original_cwd = crate::bootstrap::state::get_original_cwd();
            let previous_allowed_sources = crate::bootstrap::state::get_allowed_setting_sources();
            let previous_flag_settings_path = crate::bootstrap::state::get_flag_settings_path();
            let previous_flag_settings_inline = crate::bootstrap::state::get_flag_settings_inline();
            let previous_config = crate::utils::config::replace_test_global_config(Some(
                crate::utils::config::GlobalConfig::default(),
            ));
            let root = std::env::temp_dir().join(format!(
                "cometix-notifications-runtime-{}",
                uuid::Uuid::new_v4().simple()
            ));
            let workspace = root.join("workspace");
            std::fs::create_dir_all(workspace.join(".claude"))
                .expect("create notifications fixture");

            crate::utils::process_env::set("CLAUDE_CONFIG_DIR", &root);
            crate::utils::process_env::set(
                "CLAUDE_CODE_MANAGED_SETTINGS_PATH",
                root.join("missing-managed-settings.json"),
            );
            if remote {
                crate::utils::process_env::set("CLAUDE_CODE_REMOTE", "1");
            }
            std::env::set_current_dir(&workspace).expect("set notifications fixture cwd");
            crate::bootstrap::state::set_original_cwd(&workspace);
            crate::bootstrap::state::set_allowed_setting_sources(vec![
                "userSettings".to_string(),
                "projectSettings".to_string(),
                "localSettings".to_string(),
            ]);
            crate::bootstrap::state::set_flag_settings_path(None);
            crate::bootstrap::state::set_flag_settings_inline(None);
            crate::bootstrap::state::reset_auth_file_descriptor_caches_for_testing();
            crate::services::mock_rate_limits::reset_for_test();
            crate::utils::secure_storage::mac_os_keychain_helpers::clear_keychain_cache();
            std::fs::write(root.join("settings.json"), b"{}")
                .expect("write notifications settings fixture");
            if let Some(subscription_type) = subscription_type {
                std::fs::write(
                    root.join(".credentials.json"),
                    serde_json::to_vec(&serde_json::json!({
                        "claudeAiOauth": {
                            "accessToken": "notifications-test-token",
                            "refreshToken": "notifications-refresh-token",
                            "expiresAt": 4_102_444_800_000u64,
                            "scopes": ["user:inference"],
                            "subscriptionType": subscription_type,
                            "rateLimitTier": null
                        }
                    }))
                    .expect("serialize notifications credentials fixture"),
                )
                .expect("write notifications credentials fixture");
            }

            Self {
                _lock: lock,
                previous_env,
                previous_config,
                previous_cwd,
                previous_original_cwd,
                previous_allowed_sources,
                previous_flag_settings_path,
                previous_flag_settings_inline,
                root,
            }
        }
    }

    impl Drop for NotificationsRuntimeFixture {
        fn drop(&mut self) {
            crate::bootstrap::state::set_allowed_setting_sources(
                self.previous_allowed_sources.clone(),
            );
            crate::bootstrap::state::set_flag_settings_path(
                self.previous_flag_settings_path.clone(),
            );
            crate::bootstrap::state::set_flag_settings_inline(
                self.previous_flag_settings_inline.clone(),
            );
            let _ = crate::utils::config::replace_test_global_config(self.previous_config.take());
            let _ = std::env::set_current_dir(&self.previous_cwd);
            crate::bootstrap::state::set_original_cwd(&self.previous_original_cwd);
            drop(std::mem::take(&mut self.previous_env));
            crate::services::mock_rate_limits::reset_for_test();
            crate::utils::secure_storage::mac_os_keychain_helpers::clear_keychain_cache();
            crate::bootstrap::state::reset_auth_file_descriptor_caches_for_testing();
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[component]
    fn NotificationsHarness(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let notifications = NotificationsState {
            current: Some(
                Notification::text(
                    "demo",
                    "Mock notification visible",
                    NotificationPriority::Medium,
                )
                .with_color(NotificationColor::Warning),
            ),
            queue: Vec::new(),
        };
        let state = hooks.use_state(|| {
            let mut initial = crate::state::app_state_store::AppState::default();
            initial.notifications = std::sync::Arc::new(notifications);
            crate::state::store::AppStore::new(initial, None)
        });
        element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                crate::state::app_state::AppStateProvider(
                    prebuilt_store: Some(state.read().clone()),
                    children: crate::state::app_state::ProviderChildren::new(|| element! {
                        Notifications
                    }.into_any()),
                )
            }
        }
    }

    #[component]
    fn SegmentedNotificationsHarness(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let notifications = NotificationsState {
            current: Some(
                Notification::text(
                    "plugin-autoupdate-restart",
                    "Plugin updated: formatter · Run /reload-plugins to apply",
                    NotificationPriority::Low,
                )
                .with_segments(vec![
                    NotificationSegment::text("Plugin updated: formatter")
                        .with_color(NotificationColor::Success),
                    NotificationSegment::text(" · Run /reload-plugins to apply").with_dim(true),
                ]),
            ),
            queue: Vec::new(),
        };
        let state = hooks.use_state(|| {
            let mut initial = crate::state::app_state_store::AppState::default();
            initial.notifications = std::sync::Arc::new(notifications);
            crate::state::store::AppStore::new(initial, None)
        });
        element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                crate::state::app_state::AppStateProvider(
                    prebuilt_store: Some(state.read().clone()),
                    children: crate::state::app_state::ProviderChildren::new(|| element! {
                        Notifications
                    }.into_any()),
                )
            }
        }
    }

    #[test]
    fn notifications_render_current_text_notification() {
        let text = element!(NotificationsHarness).render(None).to_string();
        assert!(
            text.contains("Mock notification visible"),
            "canvas=\n{text}"
        );
    }

    #[test]
    fn notifications_render_segmented_notification_visible_text() {
        let text = element!(SegmentedNotificationsHarness)
            .render(None)
            .to_string();
        assert!(
            text.contains("Plugin updated: formatter · Run /reload-plugins to apply"),
            "canvas=\n{text}"
        );
    }

    #[test]
    fn auth_status_indicator_matches_official_local_and_remote_copy() {
        let local = auth_status_indicator(VerificationStatus::Missing, false).unwrap();
        let remote = auth_status_indicator(VerificationStatus::Invalid, true).unwrap();
        assert_eq!(local.text, "Not logged in · Run /login");
        assert_eq!(remote.text, "Authentication error · Try again");
        assert_eq!(local.segments[0].color, Some(NotificationColor::Error));
        assert_eq!(remote.segments[0].color, Some(NotificationColor::Error));
        assert!(auth_status_indicator(VerificationStatus::Valid, false).is_none());
    }

    #[test]
    fn api_key_helper_slow_indicator_uses_official_threshold() {
        assert!(api_key_helper_slow_indicator(None).is_none());
        assert!(api_key_helper_slow_indicator(Some(9_999)).is_none());
        let indicator = api_key_helper_slow_indicator(Some(10_000)).unwrap();
        assert_eq!(indicator.text, "apiKeyHelper is taking a while (10s)");
    }

    #[test]
    fn notifications_render_footer_indicator_rows_from_snapshot() {
        let _runtime = NotificationsRuntimeFixture::new(None, false);
        let mut initial = crate::state::app_state_store::AppState::default();
        initial.verbose = true;
        initial.claude_ai_limits =
            std::sync::Arc::new(crate::services::claude_ai_limits::ClaudeAiLimits {
                is_using_overage: true,
                ..Default::default()
            });
        let store = crate::state::store::AppStore::new(initial, None);
        // CC: tokenUsage is derived from the messages prop, not AppState.
        let messages = std::sync::Arc::new(vec![assistant_message_with_usage(40, 2)]);
        let text = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                crate::state::app_state::AppStateProvider(
                    prebuilt_store: Some(store),
                    children: crate::state::app_state::ProviderChildren::new(move || element! {
                        Notifications(
                            debug: true,
                            messages: messages.clone(),
                            suppressed: false,
                            inline: true,
                        )
                    }.into_any()),
                )
            }
        }
        .render(Some(120))
        .to_string();
        assert!(text.contains("Now using extra usage"), "canvas=\n{text}");
        assert!(text.contains("Debug mode"), "canvas=\n{text}");
        assert!(text.contains("42 tokens"), "canvas=\n{text}");
    }

    #[test]
    fn direct_footer_row_count_includes_memory_wake_channel() {
        assert_eq!(
            direct_footer_row_count(
                false,
                false,
                false,
                None,
                VerificationStatus::Valid,
                0,
                false,
                true,
                false,
            ),
            1
        );
        assert_eq!(
            direct_footer_row_count(
                false,
                false,
                false,
                None,
                VerificationStatus::Valid,
                0,
                false,
                false,
                false,
            ),
            0
        );
    }

    #[test]
    fn notifications_render_auth_error_row_from_repl_prop_matches_official() {
        let _runtime = NotificationsRuntimeFixture::new(None, false);
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        let text = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                crate::state::app_state::AppStateProvider(
                    prebuilt_store: Some(store),
                    children: crate::state::app_state::ProviderChildren::new(|| element! {
                        Notifications(
                            api_key_status: VerificationStatus::Missing,
                            suppressed: false,
                            inline: true,
                        )
                    }.into_any()),
                )
            }
        }
        .render(Some(120))
        .to_string();
        assert!(
            text.contains("Not logged in · Run /login"),
            "canvas=\n{text}"
        );
    }

    #[test]
    fn notifications_remote_auth_copy_uses_canonical_process_env() {
        let _runtime = NotificationsRuntimeFixture::new(None, true);
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        let text = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                crate::state::app_state::AppStateProvider(
                    prebuilt_store: Some(store),
                    children: crate::state::app_state::ProviderChildren::new(|| element! {
                        Notifications(
                            api_key_status: VerificationStatus::Missing,
                            suppressed: false,
                            inline: true,
                        )
                    }.into_any()),
                )
            }
        }
        .render(Some(120))
        .to_string();
        assert!(
            text.contains("Authentication error · Try again"),
            "canvas=\n{text}"
        );
    }

    #[test]
    fn notifications_overage_gate_uses_canonical_subscription_state() {
        let _runtime = NotificationsRuntimeFixture::new(Some("team"), false);
        assert_eq!(get_subscription_type().as_deref(), Some("team"));

        let mut initial = crate::state::app_state_store::AppState::default();
        initial.claude_ai_limits =
            std::sync::Arc::new(crate::services::claude_ai_limits::ClaudeAiLimits {
                is_using_overage: true,
                ..Default::default()
            });
        let store = crate::state::store::AppStore::new(initial, None);
        let text = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                crate::state::app_state::AppStateProvider(
                    prebuilt_store: Some(store),
                    children: crate::state::app_state::ProviderChildren::new(|| element! {
                        Notifications(
                            api_key_status: VerificationStatus::Valid,
                            suppressed: false,
                            inline: true,
                        )
                    }.into_any()),
                )
            }
        }
        .render(Some(120))
        .to_string();
        assert!(
            !text.contains("Now using extra usage"),
            "team subscription must suppress overage row; canvas=\n{text}"
        );
    }

    /// Maps to: CC Notifications.tsx:75,284 — the ⧉ row renders from the
    /// REPL-owned ideSelection PROP (with connected status derived from
    /// AppState.mcp), and the CC-shaped identity-change reset object
    /// (useIdeSelection.ts:77-82) renders nothing.
    #[test]
    fn notifications_render_ide_selection_row_from_repl_prop() {
        use crate::hooks::use_ide_selection::{IdeSelection, ide_selection_reset};
        use crate::services::mcp::types::McpServerConnectionType;
        use crate::services::mcp::types::{McpClientSnapshot, McpServerSnapshot};
        use crate::state::app_state_store::McpState;

        let connected_ide_state = || {
            let mut initial = crate::state::app_state_store::AppState::default();
            initial.mcp = std::sync::Arc::new(McpState {
                clients: vec![McpServerSnapshot {
                    connection_id: None,
                    client: McpClientSnapshot {
                        name: "ide".into(),
                        status: McpServerConnectionType::Connected,
                        reconnect_attempt: None,
                        max_reconnect_attempts: None,
                        ide_name: None,
                        server_version: None,
                        error: None,
                    },
                    config: None,
                    supports_resources: false,
                    tools: Vec::new(),
                    prompts: Vec::new(),
                    resources: Vec::new(),
                }],
                ..McpState::default()
            });
            crate::state::store::AppStore::new(initial, None)
        };

        let selected = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                crate::state::app_state::AppStateProvider(
                    prebuilt_store: Some(connected_ide_state()),
                    children: crate::state::app_state::ProviderChildren::new(|| element! {
                        Notifications(
                            ide_selection: Some(IdeSelection {
                                text: Some("abc".to_string()),
                                line_count: 3,
                                file_path: Some("/tmp/demo.rs".to_string()),
                                line_start: Some(1),
                            }),
                            suppressed: false,
                            inline: true,
                        )
                    }.into_any()),
                )
            }
        }
        .render(Some(120))
        .to_string();
        assert!(
            selected.contains("⧉ 3 lines selected"),
            "canvas=\n{selected}"
        );

        let reset = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                crate::state::app_state::AppStateProvider(
                    prebuilt_store: Some(connected_ide_state()),
                    children: crate::state::app_state::ProviderChildren::new(|| element! {
                        Notifications(
                            ide_selection: Some(ide_selection_reset()),
                            suppressed: false,
                            inline: true,
                        )
                    }.into_any()),
                )
            }
        }
        .render(Some(120))
        .to_string();
        assert!(
            !reset.contains('⧉'),
            "the CC reset object must hide the row; canvas=\n{reset}"
        );
    }

    #[test]
    fn should_show_auto_updater_hides_success_when_ide_selection_visible() {
        assert!(!should_show_auto_updater(
            true,
            false,
            Some(InstallStatus::Success)
        ));
        assert!(should_show_auto_updater(
            true,
            true,
            Some(InstallStatus::Success)
        ));
    }

    /// Maps to: CC Notifications.tsx:119-122 gate + the child render bodies —
    /// the height row is driven by the REPL-owned result / PromptInput-owned
    /// isAutoUpdating values (plus AppState verbose), not stale AppState. A
    /// success result renders an empty row as executed (committed-null
    /// updateSemver), so it counts 0 unless verbose adds the versions row;
    /// failure statuses and in-flight installs count 1.
    #[test]
    fn auto_update_height_counts_as_executed_rows_from_owner_threaded_values() {
        let state = crate::state::app_state_store::AppState::default();
        let success = AutoUpdaterResult {
            version: Some("1.0.0".into()),
            status: InstallStatus::Success,
            notifications: Vec::new(),
        };
        let failure = AutoUpdaterResult {
            version: Some("1.0.0".into()),
            status: InstallStatus::InstallFailed,
            notifications: Vec::new(),
        };
        assert_eq!(
            auto_update_hint_row_count_from_app(&state, Some(&success), false, false),
            0,
            "success copy never commits (CC render-phase discard)"
        );
        assert_eq!(
            auto_update_hint_row_count_from_app(&state, Some(&failure), false, false),
            1
        );
        assert_eq!(
            auto_update_hint_row_count_from_app(&state, None, true, false),
            1
        );
        assert_eq!(
            auto_update_hint_row_count_from_app(&state, None, false, false),
            0
        );

        let mut verbose_state = crate::state::app_state_store::AppState::default();
        verbose_state.verbose = true;
        assert_eq!(
            auto_update_hint_row_count_from_app(&verbose_state, Some(&success), false, false),
            1,
            "verbose renders the versions row whenever the child gate passes"
        );
        assert_eq!(
            auto_update_hint_row_count_from_app(&verbose_state, Some(&success), false, true),
            0,
            "a visible ⧉ ide row suppresses the success auto-updater row \
             (CC Notifications.tsx:119-122 shouldShowAutoUpdater)"
        );
    }

    fn assistant_message_with_usage(
        input_tokens: u64,
        output_tokens: u64,
    ) -> crate::types::message::Message {
        use crate::types::message::{AssistantContent, AssistantMessage, Message, TokenUsage};

        Message::Assistant(AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![AssistantContent::Text("hi".into())],
            model: Some("claude".into()),
            stop_reason: None,
            usage: Some(TokenUsage {
                input_tokens,
                output_tokens,
                ..Default::default()
            }),
        })
    }

    fn compact_boundary_message() -> crate::types::message::Message {
        crate::types::message::Message::System(
            crate::types::message::SystemMessage::compact_boundary(None),
        )
    }

    /// Maps to: CC Notifications.tsx:80-83 — tokenUsage counts only messages
    /// after the last compact boundary, so a trailing boundary yields zero
    /// until a new usage-bearing API response arrives.
    #[test]
    fn token_usage_derives_from_messages_after_compact_boundary_matches_official() {
        let with_usage = vec![assistant_message_with_usage(100, 50)];
        assert_eq!(token_usage_from_messages(&with_usage), 150);

        let compacted = vec![
            assistant_message_with_usage(100, 50),
            compact_boundary_message(),
        ];
        assert_eq!(
            token_usage_from_messages(&compacted),
            0,
            "post-compact, pre-new-response count must drop pre-boundary usage"
        );
    }

    #[test]
    fn set_overage_mode_writes_app_state() {
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        set_overage_mode(&store, true);
        assert!(store.get().claude_ai_limits.is_using_overage);
    }
}
