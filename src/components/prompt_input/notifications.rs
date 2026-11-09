//! Maps to: CC `components/PromptInput/Notifications.tsx` +
//! `context/notifications.tsx` for the main-screen-safe subset.
//!
//! Status rows match CC `NotificationContent`: inline apiKeyHelper/auth/
//! debug/verbose; mounts of root components (`IdeStatusIndicator`, `TokenWarning`,
//! `UpdateNotifier`, `MemoryUsageIndicator`) and PromptInput-local
//! `SandboxPromptFooterHint`. No parallel `*_footer.rs` adapter layer.

use super::prompt_input_footer::PromptFooterIndicator;
use crate::components::ide_status_indicator::{IdeStatusIndicator, ide_hint_row_count};
use crate::components::token_warning::{TokenWarning, token_warning_hint_row_count};
use crate::components::update_notifier::{UpdateNotifier, update_notifier_row_count};
use crate::context::notifications::use_notifications;
use crate::context::notifications::{NotificationColor, NotificationSegment};
use crate::hooks::notifs::ide_status_indicator::IdeConnectionStatus;
use crate::hooks::use_credential_status::CredentialStatus;
use crate::services::compact::auto_compact::is_auto_compact_enabled;
use crate::state::store::AppStore;
use crate::utils::auth::get_api_key_helper_elapsed_ms;
use crate::utils::config::load_global_config;
use crate::utils::env_utils::is_env_truthy;
use crate::utils::format::format_duration;
use crate::utils::theme::Theme;
use iocraft::prelude::*;
use std::time::Duration;

/// Maps to: CC `Notifications.tsx` slow apiKeyHelper warning threshold.
pub const API_KEY_HELPER_SLOW_THRESHOLD_MS: u64 = 10_000;

const DEFAULT_MODEL: &str = "claude-sonnet-4-5";

/// Maps to: CC `Notifications.tsx` auth-error footer row.
pub fn auth_status_indicator(
    credential_status: &CredentialStatus,
    is_remote_mode: bool,
) -> Option<PromptFooterIndicator> {
    let CredentialStatus::Missing { provider } = credential_status else {
        return None;
    };

    Some(PromptFooterIndicator::colored(
        if is_remote_mode {
            "Authentication error · Try again".to_string()
        } else {
            format!(
                "No credential for provider {provider} · check settings.nyan.providers.{provider}.credential"
            )
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
    credential_missing: bool,
    token_usage: u64,
    api_key_helper_slow: bool,
    memory_visible: bool,
    sandbox_hint_visible: bool,
) -> usize {
    let mut count = usize::from(has_current_notification);
    count += usize::from(api_key_helper_slow);
    if !credential_missing && verbose {
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
/// footer height without moving their auth or environment policy.
/// `token_usage` is caller-derived from that caller's `messages` prop via
/// [`token_usage_from_messages`] (CC Notifications.tsx:80-83 — not AppState).
/// `update_available` is caller-threaded from the REPL owner.
/// `ide_selection` is the REPL-owned selection state, prop-drilled through
/// PromptInput/Footer (CC Notifications.tsx:60,75 props — not AppState).
pub fn direct_footer_row_count_from_app(
    state: &crate::state::app_state_store::AppState,
    has_current_notification: bool,
    debug: bool,
    credential_status: &CredentialStatus,
    token_usage: u64,
    update_available: Option<&crate::utils::auto_updater::UpdateAvailable>,
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
        Some(&crate::utils::model::model::profile_model(
            state.main_loop_profile.as_deref(),
        )),
        credential_status.is_missing(),
        token_usage,
        api_key_helper_slow,
        memory_visible,
        sandbox_hint_visible,
    );
    let is_remote_mode = is_env_truthy(crate::utils::process_env::var("NYAN_REMOTE").as_deref());
    if auth_status_indicator(credential_status, is_remote_mode).is_some() {
        count += 1;
    }
    if debug {
        count += 1;
    }
    let ide_connected = crate::components::ide_status_indicator::ide_client_connected(&state.mcp);
    let ide_status = ide_connected.then_some(IdeConnectionStatus::Connected);
    let ide_rows = ide_hint_row_count(ide_status, ide_selection);
    count += ide_rows;
    count += update_notifier_row_count(update_available);
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

fn notification_color(color: Option<NotificationColor>, theme: &Theme) -> Color {
    match color {
        Some(NotificationColor::Error) => theme.error,
        Some(NotificationColor::Warning) => theme.warning,
        Some(NotificationColor::Success) => theme.success,
        Some(NotificationColor::Claude) => theme.claude,
        Some(NotificationColor::Text) => theme.text,
        Some(NotificationColor::Ide) => theme.ide,
        Some(NotificationColor::Suggestion) => theme.suggestion,
        None => theme.inactive,
    }
}

#[derive(Default, Props)]
pub struct NotificationsProps {
    /// Maps to: CC `NotificationsProps.apiKeyStatus`, passed from REPL through
    /// PromptInput rather than read from AppState.
    pub credential_status: CredentialStatus,
    /// REPL-owned newer release, prop-drilled through PromptInput/Footer.
    pub update_available: Option<crate::utils::auto_updater::UpdateAvailable>,
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
        crate::utils::model::model::profile_model(state.main_loop_profile.as_deref())
    });
    // Maps to: CC Notifications.tsx:75 — ideSelection arrives as a prop from
    // the REPL owner; only the connected status derives from AppState.mcp
    // (CC `useIdeConnectionStatus(mcpClients)`).
    let ide_selection = props.ide_selection.clone();
    let ide_connected = crate::state::app_state::use_app_state(&mut hooks, |state| {
        crate::components::ide_status_indicator::ide_client_connected(&state.mcp)
    });
    let credential_status = props.credential_status.clone();
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
    let is_remote_mode = is_env_truthy(crate::utils::process_env::var("NYAN_REMOTE").as_deref());
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

    let api_key_elapsed = *api_key_helper_elapsed.read();

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

    if let Some(indicator) = api_key_helper_slow_indicator(api_key_elapsed) {
        mid_rows.push(FooterRow::from_indicator(indicator));
    }

    if let Some(indicator) = auth_status_indicator(&credential_status, is_remote_mode) {
        tail_rows.push(FooterRow::from_indicator(indicator));
    }
    if debug {
        tail_rows.push(FooterRow::from_indicator(PromptFooterIndicator::colored(
            "Debug mode",
            NotificationColor::Warning,
        )));
    }
    if !credential_status.is_missing() && verbose {
        tail_rows.push(FooterRow::from_indicator(PromptFooterIndicator::dim(
            format!("{token_usage} tokens"),
        )));
    }

    let ide_status = ide_connected.then_some(IdeConnectionStatus::Connected);
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
            // CC order: Ide → notif → apiKeyHelper → auth/debug/verbose
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
            UpdateNotifier(
                update_available: props.update_available.clone(),
                verbose: verbose,
            )
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
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK, TestEnvGuard};
    use std::path::PathBuf;

    const NOTIFICATIONS_RUNTIME_ENV_KEYS: &[&str] = &[
        "NYAN_API_KEY_FILE_DESCRIPTOR",
        "NYAN_REMOTE",
        "NYAN_SIMPLE",
        "NYAN_CONFIG_DIR",
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
        fn new(remote: bool) -> Self {
            let lock = TEST_ENV_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let previous_env = NOTIFICATIONS_RUNTIME_ENV_KEYS
                .iter()
                .map(|key| EnvVarGuard::unset(key))
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
                "nyan-notifications-runtime-{}",
                uuid::Uuid::new_v4().simple()
            ));
            let workspace = root.join("workspace");
            std::fs::create_dir_all(workspace.join(".nyan")).expect("create notifications fixture");

            crate::utils::process_env::set("NYAN_CONFIG_DIR", &root);
            if remote {
                crate::utils::process_env::set("NYAN_REMOTE", "1");
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
            crate::utils::secure_storage::mac_os_keychain_helpers::clear_keychain_cache();
            std::fs::write(root.join("settings.json"), b"{}")
                .expect("write notifications settings fixture");

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

    fn missing(provider: &str) -> CredentialStatus {
        CredentialStatus::Missing {
            provider: provider.to_string(),
        }
    }

    #[test]
    fn auth_status_indicator_names_the_provider_locally_and_stays_generic_remotely() {
        let local = auth_status_indicator(&missing("deepseek"), false).unwrap();
        let remote = auth_status_indicator(&missing("deepseek"), true).unwrap();
        assert_eq!(
            local.text,
            "No credential for provider deepseek · check settings.nyan.providers.deepseek.credential"
        );
        assert_eq!(remote.text, "Authentication error · Try again");
        assert_eq!(local.segments[0].color, Some(NotificationColor::Error));
        assert_eq!(remote.segments[0].color, Some(NotificationColor::Error));
        assert!(auth_status_indicator(&CredentialStatus::Ready, false).is_none());
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
        let _runtime = NotificationsRuntimeFixture::new(false);
        let mut initial = crate::state::app_state_store::AppState::default();
        initial.verbose = true;
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
        assert!(text.contains("Debug mode"), "canvas=\n{text}");
        assert!(text.contains("42 tokens"), "canvas=\n{text}");
    }

    #[test]
    fn direct_footer_row_count_includes_memory_wake_channel() {
        assert_eq!(
            direct_footer_row_count(false, false, false, None, false, 0, false, true, false),
            1
        );
        assert_eq!(
            direct_footer_row_count(false, false, false, None, false, 0, false, false, false),
            0
        );
    }

    #[test]
    fn notifications_render_auth_error_row_from_repl_prop_matches_official() {
        let _runtime = NotificationsRuntimeFixture::new(false);
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
                            credential_status: missing("cpa"),
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
            text.contains(
                "No credential for provider cpa · check settings.nyan.providers.cpa.credential"
            ),
            "canvas=\n{text}"
        );
    }

    #[test]
    fn notifications_remote_auth_copy_uses_canonical_process_env() {
        let _runtime = NotificationsRuntimeFixture::new(true);
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
                            credential_status: missing("cpa"),
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

    /// Maps to: CC Notifications.tsx:75,284 — the ⧉ row renders from the
    /// REPL-owned ideSelection PROP (with connected status derived from
    /// AppState.mcp), and the CC-shaped identity-change reset object
    /// (useIdeSelection.ts:77-82) renders nothing.
    #[test]
    fn notifications_render_ide_selection_row_from_repl_prop() {
        use crate::hooks::use_ide_selection::{IdeSelection, ide_selection_reset};
        use crate::services::mcp::types::{McpServerConnection, ScopedMcpServerConfig};
        use crate::state::app_state_store::McpState;

        let connected_ide_state = || {
            let mut initial = crate::state::app_state_store::AppState::default();
            initial.mcp = std::sync::Arc::new(McpState {
                clients: vec![McpServerConnection::connected_for_test(
                    "ide",
                    ScopedMcpServerConfig::stdio_for_test("ide"),
                )],
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
    fn update_notifier_height_tracks_the_owner_threaded_version() {
        let available = crate::utils::auto_updater::UpdateAvailable {
            version: "1.1.0".to_string(),
        };
        assert_eq!(update_notifier_row_count(Some(&available)), 1);
        assert_eq!(update_notifier_row_count(None), 0);
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
}
