//! Maps to: CC `components/DesktopUpsell/DesktopUpsellStartup.tsx`.
//!
//! Safety boundary: official startup upsell reads GrowthBook/global config,
//! increments `desktopUpsellSeenCount`, writes `desktopUpsellDismissed`, logs
//! analytics, and can enter `DesktopHandoff`. Cometix keeps the official gate
//! logic pure through `DesktopUpsellStartupSnapshot` and emits callback results
//! describing intended writes. It does not read/write config, log analytics,
//! open URLs, flush sessions, launch desktop, or shut down the process.

use crate::components::custom_select::{Select, SelectLayout, SelectOptionData};
use crate::components::design_system::dialog::Dialog;
use crate::components::desktop_handoff::{DesktopHandoff, DesktopHandoffDone, DesktopHandoffState};
use iocraft::prelude::*;

/// Maps to CC `DesktopUpsellConfig`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DesktopUpsellConfig {
    pub enable_shortcut_tip: bool,
    pub enable_startup_dialog: bool,
}

impl Default for DesktopUpsellConfig {
    fn default() -> Self {
        Self {
            enable_shortcut_tip: false,
            enable_startup_dialog: false,
        }
    }
}

fn bool_field(value: &serde_json::Value, snake_case: &str, camel_case: &str) -> Option<bool> {
    value
        .get(snake_case)
        .or_else(|| value.get(camel_case))
        .and_then(serde_json::Value::as_bool)
}

/// Maps to CC `getDesktopUpsellConfig()` default/dynamic-config value parsing.
pub fn desktop_upsell_config_from_value(value: Option<&serde_json::Value>) -> DesktopUpsellConfig {
    let Some(value) = value else {
        return DesktopUpsellConfig::default();
    };
    DesktopUpsellConfig {
        enable_shortcut_tip: bool_field(value, "enable_shortcut_tip", "enableShortcutTip")
            .unwrap_or(false),
        enable_startup_dialog: bool_field(value, "enable_startup_dialog", "enableStartupDialog")
            .unwrap_or(false),
    }
}

/// Maps to CC `getDynamicConfig_CACHED_MAY_BE_STALE('tengu_desktop_upsell', ...)`.
///
/// Cometix resolves the dynamic-config payload from the source-controlled switch
/// table instead of GrowthBook, so it never initializes GrowthBook, logs
/// exposure events, contacts the network, or writes config.
pub fn get_desktop_upsell_config() -> DesktopUpsellConfig {
    let value = crate::utils::feature_flags::feature_enabled(
        crate::utils::feature_flags::FeatureFlag::DesktopUpsell,
    )
    .then(|| serde_json::Value::Object(serde_json::Map::new()));
    desktop_upsell_config_from_value(value.as_ref())
}

/// Snapshot for CC `shouldShowDesktopUpsellStartup()`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DesktopUpsellStartupSnapshot {
    /// Maps to `process.platform`.
    pub platform: String,
    /// Maps to `process.arch`.
    pub arch: String,
    /// Maps to `getDesktopUpsellConfig()`.
    pub config: DesktopUpsellConfig,
    /// Maps to `getGlobalConfig().desktopUpsellDismissed`.
    pub desktop_upsell_dismissed: bool,
    /// Maps to `getGlobalConfig().desktopUpsellSeenCount`.
    pub desktop_upsell_seen_count: u32,
}

/// Maps to CC `isSupportedPlatform()`.
pub fn is_supported_desktop_upsell_platform(platform: &str, arch: &str) -> bool {
    platform == "darwin" || (platform == "win32" && arch == "x64")
}

/// Maps to CC `shouldShowDesktopUpsellStartup()`.
pub fn should_show_desktop_upsell_startup(snapshot: &DesktopUpsellStartupSnapshot) -> bool {
    if !is_supported_desktop_upsell_platform(&snapshot.platform, &snapshot.arch) {
        return false;
    }
    if !snapshot.config.enable_startup_dialog {
        return false;
    }
    if snapshot.desktop_upsell_dismissed {
        return false;
    }
    if snapshot.desktop_upsell_seen_count >= 3 {
        return false;
    }
    true
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DesktopUpsellSelection {
    Try,
    NotNow,
    Never,
}

impl DesktopUpsellSelection {
    fn value(self) -> &'static str {
        match self {
            Self::Try => "try",
            Self::NotNow => "not-now",
            Self::Never => "never",
        }
    }
}

fn selection_from_value(value: &str) -> DesktopUpsellSelection {
    match value {
        "try" => DesktopUpsellSelection::Try,
        "never" => DesktopUpsellSelection::Never,
        _ => DesktopUpsellSelection::NotNow,
    }
}

/// Maps to CC `DesktopUpsellStartup` `options`.
pub fn desktop_upsell_options() -> Vec<SelectOptionData> {
    vec![
        SelectOptionData {
            label: "Open in Claude Code Desktop".to_string(),
            value: DesktopUpsellSelection::Try.value().to_string(),
            ..SelectOptionData::default()
        },
        SelectOptionData {
            label: "Not now".to_string(),
            value: DesktopUpsellSelection::NotNow.value().to_string(),
            ..SelectOptionData::default()
        },
        SelectOptionData {
            label: "Don't ask again".to_string(),
            value: DesktopUpsellSelection::Never.value().to_string(),
            ..SelectOptionData::default()
        },
    ]
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DesktopUpsellDone {
    pub selection: DesktopUpsellSelection,
    /// True when official `handleSelect('never')` would write
    /// `desktopUpsellDismissed: true`.
    pub would_mark_dismissed: bool,
    /// Set when the user chose "try" and the nested `DesktopHandoff` completed.
    pub handoff: Option<DesktopHandoffDone>,
}

#[derive(Default, Props)]
pub struct DesktopUpsellStartupProps<'a> {
    pub on_done: HandlerMut<'a, DesktopUpsellDone>,
    pub handoff_state: DesktopHandoffState,
    pub handoff_error: Option<String>,
    pub handoff_download_message: Option<String>,
}

/// Maps to CC `DesktopUpsellStartup`.
#[component]
pub fn DesktopUpsellStartup<'a>(
    props: &mut DesktopUpsellStartupProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let mut show_handoff = hooks.use_state(|| false);
    let mut focused_index = hooks.use_state(|| 0usize);
    let mut pending_selection = hooks.use_state(|| Option::<DesktopUpsellSelection>::None);
    let mut pending_done = hooks.use_state(|| Option::<DesktopUpsellDone>::None);
    let options = desktop_upsell_options();
    let option_count = options.len().max(1);

    hooks.use_terminal_events({
        let mut focused_index = focused_index;
        let mut pending_selection = pending_selection;
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
                        pending_selection.set(Some(selection_from_value(&option.value)));
                    }
                }
                _ => {}
            }
        }
    });

    let selected = { pending_selection.read().clone() };
    if let Some(selection) = selected {
        pending_selection.set(None);
        match selection {
            DesktopUpsellSelection::Try => show_handoff.set(true),
            DesktopUpsellSelection::NotNow => pending_done.set(Some(DesktopUpsellDone {
                selection,
                would_mark_dismissed: false,
                handoff: None,
            })),
            DesktopUpsellSelection::Never => pending_done.set(Some(DesktopUpsellDone {
                selection,
                would_mark_dismissed: true,
                handoff: None,
            })),
        }
    }

    let done = { pending_done.read().clone() };
    if let Some(done) = done {
        pending_done.set(None);
        (props.on_done)(done);
    }

    if show_handoff.get() {
        let mut pending_done_for_handoff = pending_done;
        return element! {
            DesktopHandoff(
                state: props.handoff_state,
                error: props.handoff_error.clone(),
                download_message: props.handoff_download_message.clone(),
                on_done: move |handoff: DesktopHandoffDone| {
                    pending_done_for_handoff.set(Some(DesktopUpsellDone {
                        selection: DesktopUpsellSelection::Try,
                        would_mark_dismissed: false,
                        handoff: Some(handoff),
                    }));
                },
            )
        }
        .into_any();
    }

    element! {
        Dialog(
            title: "Try Claude Code Desktop".to_string(),
            on_cancel: move |_| pending_selection.set(Some(DesktopUpsellSelection::NotNow)),
        ) {
            View(flex_direction: FlexDirection::Column, padding_x: 2u32, padding_y: 1u32) {
                View(margin_bottom: 1u32) {
                    Text(
                        content: "Same Claude Code with visual diffs, live app preview, parallel sessions, and more.".to_string(),
                        wrap: TextWrap::Wrap,
                    )
                }
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
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};
    use crate::utils::theme;
    use futures::{StreamExt, stream};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn key(code: KeyCode) -> TerminalEvent {
        TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code))
    }

    fn render_dialog() -> String {
        element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                DesktopUpsellStartup
            }
        }
        .render(Some(100))
        .to_string()
    }

    async fn drive(
        events: Vec<TerminalEvent>,
        handoff_state: DesktopHandoffState,
        results: Arc<Mutex<Vec<DesktopUpsellDone>>>,
    ) -> String {
        let results_for_handler = results.clone();
        let mut app = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                DesktopUpsellStartup(
                    handoff_state: handoff_state,
                    handoff_download_message: Some("Claude Desktop is not installed.".to_string()),
                    on_done: move |done: DesktopUpsellDone| results_for_handler.lock().unwrap().push(done),
                )
            }
        };
        let mut render_loop = Box::pin(app.mock_terminal_render_loop(
            MockTerminalConfig::with_events(stream::iter(events)).with_size(100, 24),
        ));
        let mut last = String::new();
        for _ in 0..8 {
            let next = crate::utils::race(render_loop.next(), async {
                futures_timer::Delay::new(Duration::from_millis(100)).await;
                None
            })
            .await;
            let Some(canvas) = next else {
                break;
            };
            last = canvas.to_string();
        }
        last
    }

    #[test]
    fn desktop_upsell_config_parser_accepts_both_official_field_casings() {
        let snake = desktop_upsell_config_from_value(Some(&serde_json::json!({
            "enable_startup_dialog": true,
            "enable_shortcut_tip": false
        })));
        assert!(snake.enable_startup_dialog);
        assert!(!snake.enable_shortcut_tip);

        let camel = desktop_upsell_config_from_value(Some(&serde_json::json!({
            "enableStartupDialog": false,
            "enableShortcutTip": true
        })));
        assert!(!camel.enable_startup_dialog);
        assert!(camel.enable_shortcut_tip);

        assert_eq!(
            desktop_upsell_config_from_value(None),
            DesktopUpsellConfig::default()
        );
    }

    #[test]
    fn desktop_upsell_config_ignores_growthbook_delivery() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let mut config = crate::utils::config::GlobalConfig::default();
        config.cached_growth_book_features = Some(std::collections::HashMap::from([(
            "tengu_desktop_upsell".to_string(),
            serde_json::json!({"enable_startup_dialog": true, "enable_shortcut_tip": true}),
        )]));
        config.growth_book_overrides = Some(std::collections::HashMap::from([(
            "tengu_desktop_upsell".to_string(),
            serde_json::json!({"enable_startup_dialog": true, "enable_shortcut_tip": true}),
        )]));
        crate::utils::config::set_test_global_config(Some(config));
        let _overrides = EnvVarGuard::set(
            "CLAUDE_INTERNAL_FC_OVERRIDES",
            r#"{"tengu_desktop_upsell":{"enableStartupDialog":true}}"#,
        );

        assert_eq!(get_desktop_upsell_config(), DesktopUpsellConfig::default());

        drop(_overrides);
        crate::utils::config::set_test_global_config(None);
    }

    #[test]
    fn desktop_upsell_startup_gate_matches_official_conditions() {
        let base = DesktopUpsellStartupSnapshot {
            platform: "darwin".to_string(),
            arch: "arm64".to_string(),
            config: DesktopUpsellConfig {
                enable_shortcut_tip: false,
                enable_startup_dialog: true,
            },
            desktop_upsell_dismissed: false,
            desktop_upsell_seen_count: 0,
        };
        assert!(should_show_desktop_upsell_startup(&base));
        assert!(is_supported_desktop_upsell_platform("win32", "x64"));
        assert!(!is_supported_desktop_upsell_platform("win32", "arm64"));
        assert!(!is_supported_desktop_upsell_platform("linux", "x64"));

        assert!(!should_show_desktop_upsell_startup(
            &DesktopUpsellStartupSnapshot {
                platform: "linux".to_string(),
                ..base.clone()
            }
        ));
        assert!(!should_show_desktop_upsell_startup(
            &DesktopUpsellStartupSnapshot {
                config: DesktopUpsellConfig::default(),
                ..base.clone()
            }
        ));
        assert!(!should_show_desktop_upsell_startup(
            &DesktopUpsellStartupSnapshot {
                desktop_upsell_dismissed: true,
                ..base.clone()
            }
        ));
        assert!(!should_show_desktop_upsell_startup(
            &DesktopUpsellStartupSnapshot {
                desktop_upsell_seen_count: 3,
                ..base
            }
        ));
    }

    #[test]
    fn desktop_upsell_startup_renders_official_copy_and_options() {
        let text = render_dialog();
        assert!(text.contains("Try Claude Code Desktop"), "canvas=\n{text}");
        assert!(text.contains("visual diffs"), "canvas=\n{text}");
        assert!(
            text.contains("Open in Claude Code Desktop"),
            "canvas=\n{text}"
        );
        assert!(text.contains("Not now"), "canvas=\n{text}");
        assert!(text.contains("Don't ask again"), "canvas=\n{text}");
    }

    #[test]
    fn desktop_upsell_startup_not_now_and_never_emit_safe_done_shapes() {
        let results = Arc::new(Mutex::new(Vec::<DesktopUpsellDone>::new()));
        futures::executor::block_on(drive(
            vec![key(KeyCode::Down), key(KeyCode::Enter)],
            DesktopHandoffState::Checking,
            results.clone(),
        ));
        futures::executor::block_on(drive(
            vec![key(KeyCode::Down), key(KeyCode::Down), key(KeyCode::Enter)],
            DesktopHandoffState::Checking,
            results.clone(),
        ));

        let results = results.lock().unwrap().clone();
        assert_eq!(
            results,
            vec![
                DesktopUpsellDone {
                    selection: DesktopUpsellSelection::NotNow,
                    would_mark_dismissed: false,
                    handoff: None,
                },
                DesktopUpsellDone {
                    selection: DesktopUpsellSelection::Never,
                    would_mark_dismissed: true,
                    handoff: None,
                },
            ]
        );
    }

    #[test]
    fn desktop_upsell_startup_try_switches_to_desktop_handoff() {
        let results = Arc::new(Mutex::new(Vec::<DesktopUpsellDone>::new()));
        let text = futures::executor::block_on(drive(
            vec![key(KeyCode::Enter)],
            DesktopHandoffState::PromptDownload,
            results.clone(),
        ));

        assert!(text.contains("Download now? (y/n)"), "canvas=\n{text}");
        assert!(results.lock().unwrap().is_empty());
    }
}
