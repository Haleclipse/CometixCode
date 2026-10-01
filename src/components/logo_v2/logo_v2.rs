//! Maps to: CC `components/LogoV2/LogoV2.tsx` — main-screen welcome logo.
//!
//! Official LogoV2 has three main render shapes used from `Messages.tsx`:
//! - `CondensedLogo` when there is no release-note/onboarding content.
//! - Compact full logo for narrow terminals.
//! - Horizontal full logo for wide terminals.
//!
//! Cometix keeps the old comet artwork archived below, but the rendered main
//! screen now follows the official Claude Code LogoV2 shapes.

use crate::components::logo_v2::clawd::Clawd;
use crate::components::logo_v2::condensed_logo::CondensedLogo;
use crate::components::logo_v2::emergency_tip::{EmergencyTip, TipOfFeed};
use crate::components::logo_v2::feed_column::FeedColumn;
use crate::components::logo_v2::feed_configs::{
    ProjectOnboardingStep, RecentActivityEntry, create_guest_passes_feed,
    create_project_onboarding_feed, create_recent_activity_feed, create_whats_new_feed,
};
use crate::components::logo_v2::guest_passes_upsell::should_show_guest_passes_upsell;
use crate::components::logo_v2::opus_1m_merge_notice::{
    Opus1mMergeNotice, should_show_opus_1m_merge_notice,
};
use crate::components::logo_v2::overage_credit_upsell::{
    create_overage_credit_feed, should_show_overage_credit_upsell,
};
use crate::components::logo_v2::voice_mode_notice::VoiceModeNotice;
use crate::constants::product;
use crate::project_onboarding_state;
use crate::utils::file::get_display_path;
use crate::utils::logo_v2_utils;
use crate::utils::process_env::JsTruthy;
use crate::components::offscreen_freeze::OffscreenFreeze;
use crate::utils::release_notes as release_notes_utils;
use iocraft::prelude::*;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub(crate) const LOGO_DISPLAY_NAME: &str = "Cometix Code";
const LEFT_PANEL_MAX_WIDTH: usize = 50;
const MAX_USERNAME_LENGTH: usize = 20;
const BORDER_PADDING: usize = 4;
const DIVIDER_WIDTH: usize = 1;
const CONTENT_PADDING: usize = 2;

#[allow(dead_code)]
mod archived_cometix_logo {
    //! Archived Cometix-only logo art. Keep this sealed off from the rendered
    //! main-screen path while the UI tracks official Claude Code LogoV2.

    pub(super) const NAME: &str = "CometixCode";

    pub(super) const COMET_ART: [&str; 13] = [
        "⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⢀⣠⣾⣿",
        "⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⢀⣴⣿⣿⠀⠀⠀⢀⣴⣿⣿⡿⠁",
        "⠀⠀⠀⠀⠀⠀⠀⠀⢀⣴⣿⣿⣿⠇⠀⣠⣶⣿⣿⣿⡟⠁⠀",
        "⠀⠀⠀⠀⠀⠀⢀⣴⣿⣿⣿⣿⣟⣴⣾⣿⣿⣿⣿⠏⠀⠀⠀",
        "⠀⠀⠀⠀⢀⣴⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⠃⣀⣤⣴⡄",
        "⠀⠀⢀⣴⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⡟⠁",
        "⠀⣴⣿⣿⣿⣿⠿⠟⠛⠛⠿⣿⣿⣿⣿⣿⣿⣿⣿⣿⠏⠀⠀",
        "⢰⣿⣿⣿⡿⠁⠀⠀⠀⠀⠀⠈⢿⣿⣿⣿⣿⣿⣿⠃⠀⠀⠀",
        "⣿⣿⣿⣿⡇⠀⠀⠀⠀⠀⠀⠀⢸⣿⣿⣿⣿⡿⠁⠀⠀⠀⠀",
        "⢻⣿⣿⣿⣷⡀⠀⠀⠀⠀⠀⢀⣼⣿⣿⣿⠟⠀⠀⠀⠀⠀⠀",
        "⠈⢿⣿⣿⣿⣿⣶⣤⣤⣤⣶⣿⣿⣿⣿⠏⠀⠀⠀⠀⠀⠀⠀",
        "⠀⠀⠻⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⡿⠋⠀⠀⠀⠀⠀⠀⠀⠀",
        "⠀⠀⠀⠀⠉⠛⠻⠿⠿⠿⠛⠋⠁⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀",
    ];
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum LogoLayoutMode {
    #[default]
    Condensed,
    Compact,
    Horizontal,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct LogoDisplayData {
    pub(crate) version: String,
    pub(crate) cwd: String,
    pub(crate) billing_type: String,
    pub(crate) model_display_name: String,
    pub(crate) agent_name: Option<String>,
    pub(crate) username: Option<String>,
    pub(crate) organization_name: Option<String>,
    pub(crate) show_project_onboarding: bool,
    pub(crate) project_onboarding_steps: Vec<ProjectOnboardingStep>,
    pub(crate) recent_activity: Vec<RecentActivityEntry>,
    pub(crate) has_release_notes: bool,
    pub(crate) release_notes: Vec<String>,
    pub(crate) show_guest_passes_upsell: bool,
    pub(crate) guest_passes_reward_text: Option<String>,
    pub(crate) show_overage_credit_upsell: bool,
    pub(crate) overage_credit_amount: Option<String>,
    pub(crate) emergency_tip: TipOfFeed,
    pub(crate) last_shown_emergency_tip: Option<String>,
    pub(crate) opus_1m_merge_enabled: bool,
    pub(crate) opus_1m_merge_seen_count: u32,
    pub(crate) voice_feature_enabled: bool,
    pub(crate) voice_mode_enabled: bool,
    pub(crate) settings_voice_enabled: bool,
    pub(crate) voice_notice_seen_count: u32,
    pub(crate) company_announcement: Option<String>,
    pub(crate) show_sandbox_status: bool,
    pub(crate) tmux_session: Option<String>,
    pub(crate) tmux_prefix: Option<String>,
    pub(crate) tmux_prefix_conflicts: bool,
}

#[cfg(test)]
thread_local! {
    static LOGO_DISPLAY_DATA_CURRENT_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn reset_logo_display_data_current_call_count() {
    LOGO_DISPLAY_DATA_CURRENT_CALLS.with(|calls| calls.set(0));
}

#[cfg(test)]
pub(crate) fn logo_display_data_current_call_count() -> usize {
    LOGO_DISPLAY_DATA_CURRENT_CALLS.with(std::cell::Cell::get)
}

impl LogoDisplayData {
    fn should_show_full_logo(&self, force_full: bool) -> bool {
        force_full || self.show_project_onboarding || self.has_release_notes
    }

    fn current() -> Self {
        #[cfg(test)]
        LOGO_DISPLAY_DATA_CURRENT_CALLS.with(|calls| calls.set(calls.get() + 1));

        let cwd = std::env::current_dir()
            .ok()
            .and_then(|path| path.to_str().map(get_display_path))
            .unwrap_or_else(|| ".".to_string());
        let global_config = crate::utils::config::load_global_config();
        let account = global_config.oauth_account.as_ref();
        let project_onboarding_steps = project_onboarding_state::get_steps();
        let project_config = crate::utils::config::get_current_project_config();
        let settings = crate::utils::settings::get_initial_settings();
        let session_id = crate::bootstrap::state::get_session_id();
        let is_demo = crate::utils::process_env::var("IS_DEMO").truthy().is_some();
        let show_project_onboarding =
            project_onboarding_state::should_show_project_onboarding_for_config_and_steps(
                &project_config,
                &project_onboarding_steps,
                is_demo,
            );
        // Maps to CC `LogoV2.tsx` release-note render data:
        // `checkForReleaseNotesSync(config.lastReleaseNotesSeen)` gates full
        // logo mode, while `getRecentReleaseNotesSync(3)` populates the
        // "What's new" feed. Safe Cometix reads only the existing changelog
        // cache file; it does not fetch GitHub, write cache/config, or mark
        // `lastReleaseNotesSeen` as the current version.
        let changelog_content = release_notes_utils::read_cached_changelog_readonly();
        let release_status = release_notes_utils::check_for_release_notes_sync_readonly(
            product::VERSION,
            global_config.last_release_notes_seen.as_deref(),
            &changelog_content,
        );
        let release_notes = release_notes_utils::get_recent_release_notes_for_logo_from_changelog(
            &changelog_content,
            3,
        );
        // Maps to CC `utils/logoV2Utils.ts` `getRecentActivitySync()` and
        // `components/LogoV2/feedConfigs.tsx` `createRecentActivityFeed`.
        // Rust reads existing lite session metadata only; it does not load full
        // transcripts, mutate session state, or generate titles.
        let recent_activity = logo_v2_utils::get_recent_activity_readonly(&session_id)
            .into_iter()
            .map(|entry| RecentActivityEntry {
                summary: entry.summary,
                first_prompt: entry.first_prompt,
                timestamp: Some(entry.timestamp),
            })
            .collect::<Vec<_>>();
        // Maps to CC `components/LogoV2/LogoV2.tsx` company announcement
        // useState initializer. Cometix reads the already-merged settings only;
        // it does not mutate startup counters or emit analytics. `Math.random()`
        // is represented by stable per-session entropy to avoid flicker across
        // retained renders while preserving the official first-startup rule.
        let company_announcement = select_company_announcement_readonly(
            settings.company_announcements.as_deref(),
            global_config.num_startups,
            stable_entropy(&session_id),
        );
        // Maps to CC `components/LogoV2/GuestPassesUpsell.tsx`
        // `useShowGuestPassesUpsell()`. Cometix consumes existing cache/config
        // snapshots only: no eligibility fetch, refresh-reset write,
        // impression increment, or analytics event is emitted.
        let now_ms = current_time_millis();
        let (guest_passes_snapshot, guest_passes_reward_text) =
            crate::services::api::referral::guest_passes_snapshot_from_readonly_config(
                &global_config,
                now_ms,
            );
        let show_guest_passes_upsell = should_show_guest_passes_upsell(&guest_passes_snapshot);
        // Maps to CC `components/LogoV2/OverageCreditUpsell.tsx`
        // `useShowOverageCreditUpsell()`. Cache refresh and impression writes
        // remain deferred; this reads only fresh cached grant data.
        let overage_grant_info =
            crate::services::api::overage_credit_grant::ui_grant_info_from_cached_readonly(
                &global_config,
                now_ms,
            );
        let overage_credit_amount = overage_grant_info
            .as_ref()
            .and_then(|info| info.amount.clone());
        let show_overage_credit_upsell = should_show_overage_credit_upsell(
            overage_grant_info.as_ref(),
            global_config.has_visited_extra_usage.unwrap_or(false),
            global_config.overage_credit_upsell_seen_count.unwrap_or(0),
        );
        // Maps to CC `components/LogoV2/EmergencyTip.tsx` `getTipOfFeed()`.
        // The `tengu-top-of-feed-tip` payload comes from the source-controlled
        // switch table; no GrowthBook service, exposure logging, refresh, or
        // config write runs.
        let emergency_tip = crate::components::logo_v2::emergency_tip::get_tip_of_feed();
        // Maps to CC `components/LogoV2/Opus1mMergeNotice.tsx`
        // `shouldShowOpus1mMergeNotice()`: the notice component remains pure,
        // and the official seen-count increment remains deferred.
        let opus_1m_merge_enabled = crate::utils::model::model::is_opus_1m_merge_enabled();
        // Maps to CC `voice/voiceModeEnabled.ts` `isVoiceModeEnabled()` and
        // `components/LogoV2/VoiceModeNotice.tsx` cached mount predicate. Voice
        // runtime/streaming and config writes are intentionally not invoked.
        let voice_feature_enabled = cfg!(feature = "voice_mode");
        let voice_mode_enabled = crate::voice::voice_mode_enabled::is_voice_mode_enabled();
        // Maps to CC `components/LogoV2/LogoV2.tsx:89` `showSandboxStatus =
        // SandboxManager.isSandboxingEnabled()`.
        let show_sandbox_status = crate::utils::sandbox::sandbox_adapter::is_sandboxing_enabled();

        // Maps to CC `utils/logoV2Utils.ts` `getLogoDisplayData()`:
        // `billingType = isClaudeAISubscriber() ? getSubscriptionName() : 'API Usage Billing'`.
        // Never surface oauthAccount.billingType (`stripe_subscription`, etc.).
        let billing_type = logo_billing_type_label(
            crate::utils::auth::is_claude_ai_subscriber(),
            crate::utils::auth::get_subscription_type().as_deref(),
        );
        // Maps to CC LogoV2 / CondensedLogo:
        // `renderModelSetting(useMainLoopModel())` — resolved model marketing name,
        // not the settings picker label "Default (recommended)".
        let model_display_name = crate::utils::model::model::render_model_name(
            &crate::utils::model::model::get_main_loop_model(),
        );

        Self {
            version: product::VERSION.to_string(),
            cwd,
            billing_type,
            model_display_name,
            agent_name: settings
                .agent
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string),
            username: account.and_then(|account| account.display_name.clone()),
            organization_name: (!is_demo)
                .then(|| account.and_then(|account| account.organization_name.clone()))
                .flatten(),
            show_project_onboarding,
            project_onboarding_steps: project_onboarding_state::feed_steps(
                &project_onboarding_steps,
            ),
            recent_activity,
            has_release_notes: release_status.has_release_notes,
            release_notes,
            show_guest_passes_upsell,
            guest_passes_reward_text,
            show_overage_credit_upsell,
            overage_credit_amount,
            emergency_tip,
            last_shown_emergency_tip: global_config.last_shown_emergency_tip.clone(),
            opus_1m_merge_enabled,
            opus_1m_merge_seen_count: global_config.opus_1m_merge_notice_seen_count.unwrap_or(0),
            voice_feature_enabled,
            voice_mode_enabled,
            settings_voice_enabled: settings.voice_enabled.unwrap_or(false),
            voice_notice_seen_count: global_config.voice_notice_seen_count.unwrap_or(0),
            company_announcement,
            show_sandbox_status,
            tmux_session: crate::utils::process_env::var("CLAUDE_CODE_TMUX_SESSION").truthy(),
            // CC interpolates the raw value with no truthiness guard
            // (LogoV2.tsx:207-208), so an empty prefix is kept as-is.
            tmux_prefix: crate::utils::process_env::var("CLAUDE_CODE_TMUX_PREFIX"),
            tmux_prefix_conflicts: crate::utils::process_env::var(
                "CLAUDE_CODE_TMUX_PREFIX_CONFLICTS",
            )
            .truthy()
            .is_some(),
        }
    }

    #[cfg(test)]
    pub(crate) fn fixture() -> Self {
        Self {
            version: "1.2.3".to_string(),
            cwd: "/code/claude".to_string(),
            billing_type: "API Usage Billing".to_string(),
            model_display_name: "Default (recommended)".to_string(),
            agent_name: None,
            username: None,
            organization_name: None,
            show_project_onboarding: false,
            project_onboarding_steps: Vec::new(),
            recent_activity: Vec::new(),
            has_release_notes: false,
            release_notes: Vec::new(),
            show_guest_passes_upsell: false,
            guest_passes_reward_text: None,
            show_overage_credit_upsell: false,
            overage_credit_amount: None,
            emergency_tip: TipOfFeed::default(),
            last_shown_emergency_tip: None,
            opus_1m_merge_enabled: false,
            opus_1m_merge_seen_count: 0,
            voice_feature_enabled: false,
            voice_mode_enabled: false,
            settings_voice_enabled: false,
            voice_notice_seen_count: 0,
            company_announcement: None,
            show_sandbox_status: false,
            tmux_session: None,
            tmux_prefix: None,
            tmux_prefix_conflicts: false,
        }
    }
}

#[derive(Default, Props)]
struct LogoShapeProps {
    columns: usize,
    data: LogoDisplayData,
}

#[derive(Default, Props)]
struct LogoRuntimeNoticesProps {
    mode: LogoLayoutMode,
    data: LogoDisplayData,
}

#[component]
pub fn Logo(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let (columns, _rows) = hooks.use_terminal_size();
    let columns = columns.max(1) as usize;
    let data = LogoDisplayData::current();
    let show_full_logo = data.should_show_full_logo(crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_FORCE_FULL_LOGO").as_deref(),
    ));
    let mode = select_logo_layout_mode(columns, show_full_logo);

    let logo = official_logo_variant(mode, columns, data);
    element! {
        Fragment {
            #(logo)
        }
    }
}

fn official_logo_variant(
    mode: LogoLayoutMode,
    columns: usize,
    data: LogoDisplayData,
) -> AnyElement<'static> {
    let logo = match mode {
        LogoLayoutMode::Condensed => element! {
            CondensedLogo(
                columns: columns,
                data: data.clone(),
                show_guest_passes_upsell: data.show_guest_passes_upsell,
                guest_passes_reward_text: data.guest_passes_reward_text.clone(),
                show_overage_credit_upsell: data.show_overage_credit_upsell,
                overage_credit_amount: data.overage_credit_amount.clone(),
            )
        }
        .into_any(),
        LogoLayoutMode::Compact => element! {
            CompactLogo(columns: columns, data: data.clone())
        }
        .into_any(),
        LogoLayoutMode::Horizontal => element! {
            HorizontalLogo(columns: columns, data: data.clone())
        }
        .into_any(),
    };

    element! {
        View(flex_direction: FlexDirection::Column) {
            #(logo)
            LogoRuntimeNotices(mode: mode, data: data)
        }
    }
    .into_any()
}

#[component]
fn LogoRuntimeNotices(
    props: &LogoRuntimeNoticesProps,
    hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<crate::utils::theme::Theme>();
    let data = &props.data;
    let show_extended_notices = matches!(
        props.mode,
        LogoLayoutMode::Condensed | LogoLayoutMode::Horizontal
    );
    let opus_notice_should_show =
        should_show_opus_1m_merge_notice(data.opus_1m_merge_enabled, data.opus_1m_merge_seen_count);
    let tmux_rows = show_extended_notices.then(|| ()).and_then(|_| {
        data.tmux_session.as_ref().map(|session| {
            let prefix = data
                .tmux_prefix
                .clone()
                .unwrap_or_else(|| "C-b".to_string());
            let detach = if data.tmux_prefix_conflicts {
                format!("Detach: {prefix} {prefix} d (press prefix twice - Claude uses {prefix})")
            } else {
                format!("Detach: {prefix} d")
            };
            (session.clone(), detach)
        })
    });
    let emergency_tip = show_extended_notices.then(|| {
        element! {
            EmergencyTip(
                tip: data.emergency_tip.clone(),
                last_shown_tip: data.last_shown_emergency_tip.clone(),
            )
        }
    });
    let announcement = show_extended_notices
        .then(|| data.company_announcement.clone())
        .flatten()
        .map(|announcement| {
            let organization = data.organization_name.clone();
            element! {
                View(padding_left: 2u32, flex_direction: FlexDirection::Column) {
                    #(organization.map(|organization| element! {
                        Text(content: format!("Message from {organization}:"), color: theme.inactive, wrap: TextWrap::NoWrap)
                    }))
                    Text(content: announcement, wrap: TextWrap::NoWrap)
                }
            }
        });
    let sandbox_notice = data
        .show_sandbox_status
        .then(|| matches!(props.mode, LogoLayoutMode::Compact | LogoLayoutMode::Horizontal))
        .filter(|show| *show)
        .map(|_| element! {
            View(padding_left: 2u32, flex_direction: FlexDirection::Column) {
                Text(content: "Your bash commands will be sandboxed. Disable with /sandbox.", color: theme.warning, wrap: TextWrap::NoWrap)
            }
        });
    // Maps to: CC LogoV2.tsx debug-mode banner (`Debug mode enabled` + Logging to).
    let debug_notice = crate::utils::debug::is_debug_mode().then(|| {
        let sink = if crate::utils::debug::is_debug_to_stderr() {
            "stderr".to_string()
        } else {
            crate::utils::file::get_display_path(
                &crate::utils::debug::get_debug_log_path().to_string_lossy(),
            )
        };
        element! {
            View(padding_left: 2u32, flex_direction: FlexDirection::Column) {
                Text(content: "Debug mode enabled", color: theme.warning, wrap: TextWrap::NoWrap)
                Text(content: format!("Logging to: {sink}"), color: theme.inactive, wrap: TextWrap::NoWrap)
            }
        }
    });

    element! {
        View(flex_direction: FlexDirection::Column) {
            VoiceModeNotice(
                feature_enabled: data.voice_feature_enabled,
                voice_mode_enabled: data.voice_mode_enabled,
                settings_voice_enabled: data.settings_voice_enabled,
                voice_notice_seen_count: data.voice_notice_seen_count,
                opus_1m_merge_notice_should_show: opus_notice_should_show,
            )
            Opus1mMergeNotice(
                opus_1m_merge_enabled: data.opus_1m_merge_enabled,
                seen_count: data.opus_1m_merge_seen_count,
            )
            #(debug_notice)
            #(emergency_tip)
            #(tmux_rows.map(|(session, detach)| element! {
                 View(padding_left: 2u32, flex_direction: FlexDirection::Column) {
                     Text(content: format!("tmux session: {session}"), color: theme.inactive, wrap: TextWrap::NoWrap)
                     Text(content: detach, color: theme.inactive, wrap: TextWrap::NoWrap)
                 }
             }))
            #(announcement)
            #(sandbox_notice)
        }
    }
}

#[component]
fn CompactLogo(props: &LogoShapeProps, hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<crate::utils::theme::Theme>();
    let columns = props.columns.max(1);
    let layout_width = 4usize;
    let mut welcome_message = format_welcome_message(props.data.username.as_deref());
    if display_width(&welcome_message) > columns.saturating_sub(layout_width) {
        welcome_message = format_welcome_message(None);
    }

    let cwd_width = cwd_available_width(&props.data, columns.saturating_sub(layout_width));
    let truncated_cwd = truncate_path(&props.data.cwd, cwd_width.max(10));
    let cwd_line = if let Some(agent) = props.data.agent_name.as_deref() {
        format!("@{agent} · {truncated_cwd}")
    } else {
        truncated_cwd
    };

    element! {
        OffscreenFreeze {
            View(
                flex_direction: FlexDirection::Column,
                border_style: BorderStyle::Round,
                border_color: theme.claude,
                border_text: Some(BorderText {
                    content: format!(" {LOGO_DISPLAY_NAME} "),
                    position: BorderTextPosition::Top,
                    align: BorderTextAlign::Start,
                    offset: 1,
                }),
                padding_x: 1u32,
                padding_y: 1u32,
                align_items: AlignItems::CENTER,
                width: columns as u32,
            ) {
                Text(content: welcome_message, bold: true, wrap: TextWrap::NoWrap)
                View(margin_y: 1u32) {
                    Clawd
                }
                Text(content: props.data.model_display_name.clone(), dim: true, wrap: TextWrap::NoWrap)
                Text(content: props.data.billing_type.clone(), dim: true, wrap: TextWrap::NoWrap)
                Text(content: cwd_line, dim: true, wrap: TextWrap::NoWrap)
            }
        }
    }
}

#[component]
fn HorizontalLogo(props: &LogoShapeProps, hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<crate::utils::theme::Theme>();
    let columns = props.columns.max(1);
    let welcome_message = format_welcome_message(props.data.username.as_deref());
    let model_line = if let Some(organization) = props.data.organization_name.as_deref() {
        format!(
            "{} · {} · {}",
            props.data.model_display_name, props.data.billing_type, organization
        )
    } else {
        format!(
            "{} · {}",
            props.data.model_display_name, props.data.billing_type
        )
    };
    let cwd_line = format_cwd_line(&props.data, LEFT_PANEL_MAX_WIDTH);
    let optimal_left_width = calculate_optimal_left_width(&welcome_message, &cwd_line, &model_line);
    let LayoutDimensions {
        left_width,
        right_width,
        ..
    } = calculate_layout_dimensions(columns, LogoLayoutMode::Horizontal, optimal_left_width);
    let feeds = if props.data.show_project_onboarding {
        vec![
            create_project_onboarding_feed(&props.data.project_onboarding_steps),
            create_recent_activity_feed(&props.data.recent_activity),
        ]
    } else if props.data.show_guest_passes_upsell {
        vec![
            create_recent_activity_feed(&props.data.recent_activity),
            create_guest_passes_feed(props.data.guest_passes_reward_text.as_deref()),
        ]
    } else if props.data.show_overage_credit_upsell {
        vec![
            create_recent_activity_feed(&props.data.recent_activity),
            create_overage_credit_feed(props.data.overage_credit_amount.as_deref()),
        ]
    } else {
        vec![
            create_recent_activity_feed(&props.data.recent_activity),
            create_whats_new_feed(&props.data.release_notes),
        ]
    };

    element! {
        OffscreenFreeze {
            View(
                flex_direction: FlexDirection::Column,
                border_style: BorderStyle::Round,
                border_color: theme.claude,
                border_text: Some(BorderText {
                    content: format!(" {LOGO_DISPLAY_NAME} v{} ", props.data.version),
                    position: BorderTextPosition::Top,
                    align: BorderTextAlign::Start,
                    offset: 3,
                }),
            ) {
                View(flex_direction: FlexDirection::Row, padding_x: 1u32, column_gap: 1u32) {
                    View(
                        flex_direction: FlexDirection::Column,
                        width: left_width as u32,
                        justify_content: JustifyContent::SPACE_BETWEEN,
                        align_items: AlignItems::CENTER,
                        min_height: 9u32,
                    ) {
                        View(margin_top: 1u32) {
                            Text(content: welcome_message, bold: true, wrap: TextWrap::NoWrap)
                        }
                        Clawd
                        View(flex_direction: FlexDirection::Column, align_items: AlignItems::CENTER) {
                            Text(content: model_line, dim: true, wrap: TextWrap::NoWrap)
                            Text(content: cwd_line, dim: true, wrap: TextWrap::NoWrap)
                        }
                    }
                    View(
                        height: 9u32,
                        border_style: BorderStyle::Single,
                        border_color: theme.claude,
                        border_dim_color: true,
                        border_top: false,
                        border_bottom: false,
                        border_left: false,
                    )
                    FeedColumn(
                        feeds: feeds,
                        max_width: right_width,
                    )
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LayoutDimensions {
    left_width: usize,
    right_width: usize,
    total_width: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ModelBillingDisplay {
    pub(crate) should_split: bool,
    pub(crate) truncated_model: String,
    pub(crate) truncated_billing: String,
}

fn select_logo_layout_mode(columns: usize, force_full: bool) -> LogoLayoutMode {
    if !force_full {
        LogoLayoutMode::Condensed
    } else if columns >= 70 {
        LogoLayoutMode::Horizontal
    } else {
        LogoLayoutMode::Compact
    }
}

/// Maps to: CC `components/LogoV2/LogoV2.tsx` company announcement selection.
fn select_company_announcement_readonly(
    announcements: Option<&[String]>,
    num_startups: u64,
    entropy: u64,
) -> Option<String> {
    let announcements = announcements?
        .iter()
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    if announcements.is_empty() {
        return None;
    }
    if num_startups == 1 {
        return Some(announcements[0].to_string());
    }
    let index = (entropy as usize) % announcements.len();
    Some(announcements[index].to_string())
}

fn stable_entropy(value: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

fn current_time_millis() -> i64 {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(duration) => duration.as_millis().min(i64::MAX as u128) as i64,
        Err(error) => -(error.duration().as_millis().min(i64::MAX as u128) as i64),
    }
}

fn calculate_layout_dimensions(
    columns: usize,
    layout_mode: LogoLayoutMode,
    optimal_left_width: usize,
) -> LayoutDimensions {
    if layout_mode == LogoLayoutMode::Horizontal {
        let left_width = optimal_left_width;
        let used_space = BORDER_PADDING + CONTENT_PADDING + DIVIDER_WIDTH + left_width;
        let available_for_right = columns.saturating_sub(used_space);
        let mut right_width = available_for_right.max(30);
        let uncapped_total = left_width + right_width + DIVIDER_WIDTH + CONTENT_PADDING;
        let total_width = uncapped_total.min(columns.saturating_sub(BORDER_PADDING));

        if total_width < uncapped_total {
            right_width = total_width.saturating_sub(left_width + DIVIDER_WIDTH + CONTENT_PADDING);
        }

        return LayoutDimensions {
            left_width,
            right_width,
            total_width,
        };
    }

    let total_width = columns
        .saturating_sub(BORDER_PADDING)
        .min(LEFT_PANEL_MAX_WIDTH + 20);
    LayoutDimensions {
        left_width: total_width,
        right_width: total_width,
        total_width,
    }
}

fn calculate_optimal_left_width(welcome_message: &str, cwd_line: &str, model_line: &str) -> usize {
    let content_width = display_width(welcome_message)
        .max(display_width(cwd_line))
        .max(display_width(model_line))
        .max(20);
    (content_width + 4).min(LEFT_PANEL_MAX_WIDTH)
}

fn format_welcome_message(username: Option<&str>) -> String {
    match username.filter(|name| !name.is_empty() && name.chars().count() <= MAX_USERNAME_LENGTH) {
        Some(username) => format!("Welcome back {username}!"),
        None => "Welcome back!".to_string(),
    }
}

/// Maps to: CC `getLogoDisplayData().billingType` selection.
pub(crate) fn logo_billing_type_label(
    is_claude_ai_subscriber: bool,
    subscription_type: Option<&str>,
) -> String {
    if is_claude_ai_subscriber {
        crate::utils::auth::claude_ai_subscription_name(subscription_type).to_string()
    } else {
        "API Usage Billing".to_string()
    }
}

pub(crate) fn format_model_and_billing(
    model_name: &str,
    billing_type: &str,
    available_width: usize,
) -> ModelBillingDisplay {
    let separator = " · ";
    let combined_width =
        display_width(model_name) + display_width(separator) + display_width(billing_type);
    let should_split = combined_width > available_width;

    if should_split {
        return ModelBillingDisplay {
            should_split: true,
            truncated_model: truncate_to_width(model_name, available_width),
            truncated_billing: truncate_to_width(billing_type, available_width),
        };
    }

    ModelBillingDisplay {
        should_split: false,
        truncated_model: truncate_to_width(
            model_name,
            available_width
                .saturating_sub(display_width(billing_type) + display_width(separator))
                .max(10),
        ),
        truncated_billing: billing_type.to_string(),
    }
}

pub(crate) fn format_cwd_line(data: &LogoDisplayData, available_width: usize) -> String {
    let cwd_width = cwd_available_width(data, available_width);
    let truncated_cwd = truncate_path(&data.cwd, cwd_width.max(10));
    if let Some(agent) = data.agent_name.as_deref() {
        format!("@{agent} · {truncated_cwd}")
    } else {
        truncated_cwd
    }
}

fn cwd_available_width(data: &LogoDisplayData, available_width: usize) -> usize {
    if let Some(agent) = data.agent_name.as_deref() {
        available_width
            .saturating_sub(display_width("@") + display_width(agent) + display_width(" · "))
    } else {
        available_width
    }
}

fn truncate_path(path: &str, max_length: usize) -> String {
    if display_width(path) <= max_length {
        return path.to_string();
    }

    let separator = "/";
    let ellipsis = "…";
    let ellipsis_width = 1;
    let separator_width = 1;
    let parts: Vec<&str> = path.split(separator).collect();
    let first = parts.first().copied().unwrap_or("");
    let last = parts.last().copied().unwrap_or("");
    let first_width = display_width(first);
    let last_width = display_width(last);

    if parts.len() == 1 {
        return truncate_to_width(path, max_length);
    }

    if first.is_empty() && ellipsis_width + separator_width + last_width >= max_length {
        return format!(
            "{separator}{}",
            truncate_to_width(last, max_length.saturating_sub(separator_width).max(1))
        );
    }

    if !first.is_empty() && ellipsis_width * 2 + separator_width + last_width >= max_length {
        return format!(
            "{ellipsis}{separator}{}",
            truncate_to_width(
                last,
                max_length
                    .saturating_sub(ellipsis_width + separator_width)
                    .max(1),
            )
        );
    }

    if parts.len() == 2 {
        let available_for_first =
            max_length.saturating_sub(ellipsis_width + separator_width + last_width);
        return format!(
            "{}{ellipsis}{separator}{last}",
            truncate_to_width_no_ellipsis(first, available_for_first)
        );
    }

    let mut available = max_length as isize
        - first_width as isize
        - last_width as isize
        - ellipsis_width as isize
        - (2 * separator_width) as isize;

    if available <= 0 {
        let available_for_first =
            max_length.saturating_sub(last_width + ellipsis_width + 2 * separator_width);
        return format!(
            "{}{separator}{ellipsis}{separator}{last}",
            truncate_to_width_no_ellipsis(first, available_for_first)
        );
    }

    let mut middle_parts = Vec::new();
    for part in parts
        .iter()
        .rev()
        .skip(1)
        .take(parts.len().saturating_sub(2))
    {
        let part_width = display_width(part) + separator_width;
        if part_width as isize <= available {
            middle_parts.insert(0, *part);
            available -= part_width as isize;
        } else {
            break;
        }
    }

    if middle_parts.is_empty() {
        return format!("{first}{separator}{ellipsis}{separator}{last}");
    }

    format!(
        "{first}{separator}{ellipsis}{separator}{}{separator}{last}",
        middle_parts.join(separator)
    )
}

pub(crate) fn truncate_to_width(text: &str, max_width: usize) -> String {
    if display_width(text) <= max_width {
        return text.to_string();
    }
    if max_width <= 1 {
        return "…".to_string();
    }

    let mut width = 0usize;
    let mut result = String::new();
    for ch in text.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + ch_width > max_width - 1 {
            break;
        }
        result.push(ch);
        width += ch_width;
    }
    result.push('…');
    result
}

fn truncate_to_width_no_ellipsis(text: &str, max_width: usize) -> String {
    if display_width(text) <= max_width {
        return text.to_string();
    }
    if max_width == 0 {
        return String::new();
    }

    let mut width = 0usize;
    let mut result = String::new();
    for ch in text.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + ch_width > max_width {
            break;
        }
        result.push(ch);
        width += ch_width;
    }
    result
}

pub(crate) fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;

    fn render_variant(mode: LogoLayoutMode, columns: usize) -> String {
        render_variant_with_data(mode, columns, LogoDisplayData::fixture())
    }

    fn render_variant_with_data(
        mode: LogoLayoutMode,
        columns: usize,
        data: LogoDisplayData,
    ) -> String {
        let current_theme = *theme::current();
        let canvas = element! {
            ContextProvider(value: Context::owned(current_theme)) {
                #(official_logo_variant(mode, columns, data))
            }
        }
        .render(Some(columns));
        canvas.to_string()
    }

    #[test]
    fn logo_mode_selection_matches_official_force_full_shape() {
        assert_eq!(
            select_logo_layout_mode(120, false),
            LogoLayoutMode::Condensed
        );
        assert_eq!(select_logo_layout_mode(69, true), LogoLayoutMode::Compact);
        assert_eq!(
            select_logo_layout_mode(70, true),
            LogoLayoutMode::Horizontal
        );
    }

    #[test]
    fn logo_full_mode_gate_matches_official_release_notes_onboarding_and_force() {
        let mut data = LogoDisplayData::fixture();
        assert!(!data.should_show_full_logo(false));
        assert!(data.should_show_full_logo(true));

        data.show_project_onboarding = true;
        assert!(data.should_show_full_logo(false));

        data.show_project_onboarding = false;
        data.release_notes = vec!["New release note".to_string()];
        assert!(!data.should_show_full_logo(false));
        data.has_release_notes = true;
        assert!(data.should_show_full_logo(false));
    }

    #[test]
    fn condensed_logo_matches_official_clawd_identity_block() {
        let text = render_variant(LogoLayoutMode::Condensed, 80);
        assert!(text.contains("Cometix Code v1.2.3"), "canvas=\n{text}");
        assert!(text.contains("▛███▜"), "canvas=\n{text}");
        assert!(
            text.contains("Default (recommended) · API Usage Billing"),
            "canvas=\n{text}"
        );
        assert!(text.contains("/code/claude"), "canvas=\n{text}");
        assert!(
            !text.contains(archived_cometix_logo::NAME),
            "archived Cometix logo must not render; canvas=\n{text}"
        );
    }

    #[test]
    fn compact_logo_uses_official_round_border_and_clawd() {
        let text = render_variant(LogoLayoutMode::Compact, 60);
        assert!(text.contains("Cometix Code"), "canvas=\n{text}");
        assert!(text.contains("Welcome back!"), "canvas=\n{text}");
        assert!(text.contains("▛███▜"), "canvas=\n{text}");
        assert!(text.contains("╭"), "canvas=\n{text}");
        assert!(
            !text.contains(archived_cometix_logo::COMET_ART[0]),
            "archived comet art must not render; canvas=\n{text}"
        );
    }

    #[test]
    fn horizontal_logo_uses_official_two_column_feed_shell() {
        let text = render_variant(LogoLayoutMode::Horizontal, 100);
        assert!(text.contains("Cometix Code v1.2.3"), "canvas=\n{text}");
        assert!(text.contains("Recent activity"), "canvas=\n{text}");
        assert!(text.contains("No recent activity"), "canvas=\n{text}");
        assert!(text.contains("What's new"), "canvas=\n{text}");
        assert!(
            text.contains("Check the Claude Code changelog for updates"),
            "canvas=\n{text}"
        );
    }

    #[test]
    fn horizontal_logo_prefers_project_onboarding_feed_when_official_gate_is_true() {
        let mut data = LogoDisplayData::fixture();
        data.show_project_onboarding = true;
        data.project_onboarding_steps = vec![ProjectOnboardingStep {
            text: "Run /init to create a CLAUDE.md file with instructions for Claude".to_string(),
            is_enabled: true,
            is_complete: false,
        }];

        let text = render_variant_with_data(LogoLayoutMode::Horizontal, 100, data);
        assert!(text.contains("Tips for getting started"), "canvas=\n{text}");
        assert!(
            text.contains("Run /init to create a CLAUDE.md"),
            "canvas=\n{text}"
        );
        assert!(text.contains("Recent activity"), "canvas=\n{text}");
        assert!(!text.contains("What's new"), "canvas=\n{text}");
    }

    #[test]
    fn horizontal_logo_prefers_upsell_feeds_between_onboarding_and_whats_new() {
        let mut guest = LogoDisplayData::fixture();
        guest.show_guest_passes_upsell = true;
        guest.guest_passes_reward_text = Some("$5".to_string());
        let text = render_variant_with_data(LogoLayoutMode::Horizontal, 100, guest);
        assert!(text.contains("3 guest passes"), "canvas=\n{text}");
        assert!(
            text.contains("Share Claude Code and earn $5"),
            "canvas=\n{text}"
        );
        assert!(!text.contains("What's new"), "canvas=\n{text}");

        let mut overage = LogoDisplayData::fixture();
        overage.show_overage_credit_upsell = true;
        overage.overage_credit_amount = Some("$10".to_string());
        let text = render_variant_with_data(LogoLayoutMode::Horizontal, 100, overage);
        assert!(text.contains("$10 in extra usage"), "canvas=\n{text}");
        assert!(
            text.contains("On us. Works on third-party apps"),
            "canvas=\n{text}"
        );
    }

    #[test]
    fn logo_runtime_notices_render_from_snapshot_without_side_effects() {
        let mut data = LogoDisplayData::fixture();
        data.opus_1m_merge_enabled = true;
        data.emergency_tip = TipOfFeed {
            tip: "Important service notice".to_string(),
            color: crate::components::logo_v2::emergency_tip::EmergencyTipColor::Warning,
        };
        data.tmux_session = Some("cc-work".to_string());
        data.tmux_prefix = Some("C-a".to_string());
        data.tmux_prefix_conflicts = true;
        data.company_announcement = Some("Deploy freeze tonight".to_string());
        data.organization_name = Some("Acme".to_string());

        let text = render_variant_with_data(LogoLayoutMode::Condensed, 120, data);
        assert!(
            text.contains("Opus now defaults to 1M context"),
            "canvas=\n{text}"
        );
        assert!(text.contains("Important service notice"), "canvas=\n{text}");
        assert!(text.contains("tmux session: cc-work"), "canvas=\n{text}");
        assert!(
            text.contains("Detach: C-a C-a d (press prefix twice - Claude uses C-a)"),
            "canvas=\n{text}"
        );
        assert!(text.contains("Message from Acme:"), "canvas=\n{text}");
        assert!(text.contains("Deploy freeze tonight"), "canvas=\n{text}");
    }

    #[test]
    fn compact_logo_suppresses_extended_runtime_notices_like_official() {
        let mut data = LogoDisplayData::fixture();
        data.emergency_tip = TipOfFeed {
            tip: "Important service notice".to_string(),
            color: crate::components::logo_v2::emergency_tip::EmergencyTipColor::Warning,
        };
        data.tmux_session = Some("cc-work".to_string());
        data.company_announcement = Some("Deploy freeze tonight".to_string());
        data.show_sandbox_status = true;

        let text = render_variant_with_data(LogoLayoutMode::Compact, 60, data);

        assert!(
            !text.contains("Important service notice"),
            "canvas=\n{text}"
        );
        assert!(!text.contains("tmux session: cc-work"), "canvas=\n{text}");
        assert!(!text.contains("Deploy freeze tonight"), "canvas=\n{text}");
        assert!(
            text.contains("Your bash commands will be sandboxed"),
            "canvas=\n{text}"
        );
    }

    #[test]
    fn condensed_logo_suppresses_sandbox_notice_like_official() {
        let mut data = LogoDisplayData::fixture();
        data.show_sandbox_status = true;

        let text = render_variant_with_data(LogoLayoutMode::Condensed, 100, data);

        assert!(
            !text.contains("Your bash commands will be sandboxed"),
            "canvas=\n{text}"
        );
    }

    #[test]
    fn company_announcement_selection_matches_official_first_startup_rule() {
        let announcements = vec![
            "First".to_string(),
            "Second".to_string(),
            "Third".to_string(),
        ];

        assert_eq!(
            select_company_announcement_readonly(Some(&announcements), 1, 2).as_deref(),
            Some("First")
        );
        assert_eq!(
            select_company_announcement_readonly(Some(&announcements), 2, 4).as_deref(),
            Some("Second")
        );
        assert!(select_company_announcement_readonly(Some(&[]), 1, 0).is_none());
    }

    #[test]
    fn logo_layout_dimensions_match_official_thresholds() {
        let dims = calculate_layout_dimensions(100, LogoLayoutMode::Horizontal, 40);
        assert_eq!(dims.left_width, 40);
        assert!(dims.right_width <= 53);
        assert!(dims.total_width <= 96);

        assert_eq!(
            calculate_optimal_left_width("Welcome back!", "/code/claude", "Default · API"),
            24
        );
        assert_eq!(format_welcome_message(Some("Ada")), "Welcome back Ada!");
        assert_eq!(
            format_welcome_message(Some("averyveryveryverylongusername")),
            "Welcome back!"
        );
    }

    #[test]
    fn logo_path_and_model_truncation_match_official_shapes() {
        assert_eq!(
            truncate_path("/Users/me/project/src/main.rs", 18),
            "/…/src/main.rs"
        );
        let split = format_model_and_billing("Default (recommended)", "API Usage Billing", 20);
        assert!(split.should_split);
        assert_eq!(split.truncated_billing, "API Usage Billing");
    }

    #[test]
    fn logo_billing_type_uses_subscription_name_not_raw_billing_rail() {
        // Official getLogoDisplayData: subscriber → getSubscriptionName().
        assert_eq!(logo_billing_type_label(true, Some("max")), "Claude Max");
        assert_eq!(logo_billing_type_label(true, Some("pro")), "Claude Pro");
        assert_eq!(logo_billing_type_label(true, Some("team")), "Claude Team");
        assert_eq!(
            logo_billing_type_label(true, Some("enterprise")),
            "Claude Enterprise"
        );
        assert_eq!(logo_billing_type_label(true, None), "Claude API");
        // Non-subscriber always shows API usage billing — ignore plan tokens.
        assert_eq!(
            logo_billing_type_label(false, Some("max")),
            "API Usage Billing"
        );
        // oauthAccount.billingType rails like stripe_subscription are not labels.
        assert_ne!(
            logo_billing_type_label(true, Some("max")),
            "stripe_subscription"
        );
    }
}
