//! Maps to: CC
//! `components/permissions/ComputerUseApproval/ComputerUseApproval.tsx`.
//!
//! Computer-use approval UI and response shaping. The TCC panel mirrors the
//! official `execFileNoThrow('open', ...)` behavior with a no-throw spawn.

use crate::components::custom_select::{Select, SelectLayout, SelectOptionData};
use crate::components::design_system::dialog::Dialog;
use crate::constants::figures::MAIN_SYMBOLS;
use crate::utils::theme::Theme;
use iocraft::prelude::*;
use std::{collections::BTreeSet, process::Command};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CuGrantFlags {
    pub clipboard_read: bool,
    pub clipboard_write: bool,
    pub system_key_combos: bool,
}

/// Maps to: CC `DEFAULT_GRANT_FLAGS` from
/// `@ant/computer-use-mcp/types`.
pub const DEFAULT_GRANT_FLAGS: CuGrantFlags = CuGrantFlags {
    clipboard_read: false,
    clipboard_write: false,
    system_key_combos: false,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CuResolvedApp {
    pub bundle_id: String,
    pub display_name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CuAppRequest {
    pub requested_name: String,
    pub resolved: Option<CuResolvedApp>,
    pub already_granted: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CuTccState {
    pub accessibility: bool,
    pub screen_recording: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CuHiddenApp {
    pub bundle_id: String,
    pub display_name: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CuPermissionRequest {
    pub request_id: String,
    pub reason: Option<String>,
    pub apps: Vec<CuAppRequest>,
    pub requested_flags: CuGrantFlags,
    pub tcc_state: Option<CuTccState>,
    pub will_hide: Vec<CuHiddenApp>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppGrant {
    pub bundle_id: String,
    pub display_name: String,
    pub granted_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppDenial {
    pub bundle_id: String,
    pub reason: AppDenialReason,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppDenialReason {
    UserDenied,
    NotInstalled,
}

impl AppDenialReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UserDenied => "user_denied",
            Self::NotInstalled => "not_installed",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CuPermissionResponse {
    pub granted: Vec<AppGrant>,
    pub denied: Vec<AppDenial>,
    pub flags: CuGrantFlags,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SentinelCategory {
    Shell,
    Filesystem,
    SystemSettings,
}

impl SentinelCategory {
    pub fn warning_text(self) -> &'static str {
        match self {
            Self::Shell => "equivalent to shell access",
            Self::Filesystem => "can read/write any file",
            Self::SystemSettings => "can change system settings",
        }
    }
}

/// Maps to: CC `getSentinelCategory(...)` from
/// `@ant/computer-use-mcp/sentinelApps`.
pub fn get_sentinel_category(bundle_id: &str) -> Option<SentinelCategory> {
    match bundle_id {
        "com.apple.Terminal"
        | "com.googlecode.iterm2"
        | "com.microsoft.VSCode"
        | "dev.warp.Warp-Stable"
        | "com.github.wez.wezterm"
        | "io.alacritty"
        | "net.kovidgoyal.kitty"
        | "com.jetbrains.intellij"
        | "com.jetbrains.pycharm" => Some(SentinelCategory::Shell),
        "com.apple.finder" => Some(SentinelCategory::Filesystem),
        "com.apple.systempreferences" => Some(SentinelCategory::SystemSettings),
        _ => None,
    }
}

pub fn deny_all_computer_use_response() -> CuPermissionResponse {
    CuPermissionResponse {
        granted: Vec::new(),
        denied: Vec::new(),
        flags: DEFAULT_GRANT_FLAGS,
    }
}

fn plural(count: usize, singular: &str) -> String {
    if count == 1 {
        singular.to_string()
    } else {
        format!("{singular}s")
    }
}

/// Maps to: CC initial `checked` set in `ComputerUseAppListPanel`.
pub fn default_checked_bundle_ids(request: &CuPermissionRequest) -> BTreeSet<String> {
    request
        .apps
        .iter()
        .filter(|app| !app.already_granted)
        .filter_map(|app| {
            app.resolved
                .as_ref()
                .map(|resolved| resolved.bundle_id.clone())
        })
        .collect()
}

/// Maps to: CC `requestedFlagKeys` memo.
pub fn requested_flag_keys(flags: CuGrantFlags) -> Vec<&'static str> {
    let mut keys = Vec::new();
    if flags.clipboard_read {
        keys.push("clipboardRead");
    }
    if flags.clipboard_write {
        keys.push("clipboardWrite");
    }
    if flags.system_key_combos {
        keys.push("systemKeyCombos");
    }
    keys
}

/// Maps to: CC `respond(allow)` in `ComputerUseAppListPanel`.
pub fn computer_use_approval_response(
    request: &CuPermissionRequest,
    checked: &BTreeSet<String>,
    allow: bool,
    now_ms: i64,
) -> CuPermissionResponse {
    if !allow {
        return deny_all_computer_use_response();
    }

    let granted = request
        .apps
        .iter()
        .filter_map(|app| {
            let resolved = app.resolved.as_ref()?;
            checked.contains(&resolved.bundle_id).then(|| AppGrant {
                bundle_id: resolved.bundle_id.clone(),
                display_name: resolved.display_name.clone(),
                granted_at: now_ms,
            })
        })
        .collect::<Vec<_>>();
    let denied = request
        .apps
        .iter()
        .filter(|app| {
            app.resolved
                .as_ref()
                .is_none_or(|resolved| !checked.contains(&resolved.bundle_id))
        })
        .map(|app| AppDenial {
            bundle_id: app
                .resolved
                .as_ref()
                .map(|resolved| resolved.bundle_id.clone())
                .unwrap_or_else(|| app.requested_name.clone()),
            reason: if app.resolved.is_some() {
                AppDenialReason::UserDenied
            } else {
                AppDenialReason::NotInstalled
            },
        })
        .collect::<Vec<_>>();

    CuPermissionResponse {
        granted,
        denied,
        flags: CuGrantFlags {
            clipboard_read: request.requested_flags.clipboard_read,
            clipboard_write: request.requested_flags.clipboard_write,
            system_key_combos: request.requested_flags.system_key_combos,
        },
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TccOption {
    OpenAccessibility,
    OpenScreenRecording,
    Retry,
}

impl TccOption {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenAccessibility => "open_accessibility",
            Self::OpenScreenRecording => "open_screen_recording",
            Self::Retry => "retry",
        }
    }
}

/// Maps to: CC TCC options memo in `ComputerUseTccPanel`.
pub fn computer_use_tcc_options(tcc_state: CuTccState) -> Vec<TccOption> {
    let mut options = Vec::new();
    if !tcc_state.accessibility {
        options.push(TccOption::OpenAccessibility);
    }
    if !tcc_state.screen_recording {
        options.push(TccOption::OpenScreenRecording);
    }
    options.push(TccOption::Retry);
    options
}

/// Maps to: CC TCC `execFileNoThrow('open', [...])` target URLs.
pub fn computer_use_tcc_open_url(option: TccOption) -> Option<&'static str> {
    match option {
        TccOption::OpenAccessibility => {
            Some("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
        }
        TccOption::OpenScreenRecording => {
            Some("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture")
        }
        TccOption::Retry => None,
    }
}

/// Maps to: CC `ComputerUseTccPanel.onChange` `execFileNoThrow('open', ...)`.
fn open_computer_use_tcc_settings_no_throw(option: TccOption) {
    let Some(url) = computer_use_tcc_open_url(option) else {
        return;
    };
    let mut command = Command::new("open");
    // CC inherits process.env (execFileNoThrow); the carrier is its counterpart.
    crate::utils::subprocess_env::apply_process_env_std(&mut command);
    let _ = command.arg(url).spawn();
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AppListOption {
    AllowAll,
    Deny,
}

impl AppListOption {
    fn as_str(self) -> &'static str {
        match self {
            Self::AllowAll => "allow_all",
            Self::Deny => "deny",
        }
    }
}

#[derive(Default, Props)]
pub struct ComputerUseApprovalProps {
    pub request: Option<CuPermissionRequest>,
    pub on_done: Handler<CuPermissionResponse>,
}

/// Maps to: CC `ComputerUseApproval` dispatcher.
#[component]
pub fn ComputerUseApproval(
    props: &ComputerUseApprovalProps,
    _hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let request = props.request.clone().unwrap_or_default();
    let on_done = props.on_done.clone();
    if let Some(tcc_state) = request.tcc_state {
        return element! {
            ComputerUseTccPanel(
                tcc_state: tcc_state,
                on_done: on_done,
            )
        }
        .into_any();
    }

    element! {
        ComputerUseAppListPanel(
            request: request,
            on_done: on_done,
        )
    }
    .into_any()
}

#[derive(Default, Props)]
struct ComputerUseTccPanelProps {
    pub tcc_state: CuTccState,
    pub on_done: Handler<CuPermissionResponse>,
}

/// Maps to: CC `ComputerUseTccPanel`.
#[component]
fn ComputerUseTccPanel(
    props: &ComputerUseTccPanelProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let tcc_state = props.tcc_state;
    let on_done = props.on_done.clone();
    let on_done_cancel = props.on_done.clone();
    let options = computer_use_tcc_options(tcc_state);
    let mut focused_index = hooks.use_state(|| 0usize);
    let mut pending_done = hooks.use_state(|| false);

    hooks.use_terminal_events({
        let mut focused_index = focused_index;
        let mut pending_done = pending_done;
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
                    if let Some(option) = options.get(focused_index.get()).copied() {
                        match option {
                            TccOption::Retry => pending_done.set(true),
                            TccOption::OpenAccessibility | TccOption::OpenScreenRecording => {
                                open_computer_use_tcc_settings_no_throw(option);
                            }
                        }
                    }
                }
                KeyCode::Esc => pending_done.set(true),
                _ => {}
            }
        }
    });

    if pending_done.get() {
        pending_done.set(false);
        (on_done)(deny_all_computer_use_response());
    }

    let focused = focused_index.get().min(options.len().saturating_sub(1));
    let select_options = options
        .iter()
        .map(|option| SelectOptionData {
            label: match option {
                TccOption::OpenAccessibility => "Open System Settings → Accessibility".to_string(),
                TccOption::OpenScreenRecording => {
                    "Open System Settings → Screen Recording".to_string()
                }
                TccOption::Retry => "Try again".to_string(),
            },
            description: None,
            dim_description: true,
            value: option.as_str().to_string(),
            disabled: false,
            input: None,
        })
        .collect::<Vec<_>>();

    element! {
        Dialog(
            title: "Computer Use needs macOS permissions".to_string(),
            on_cancel: move |_| {
                (on_done_cancel)(deny_all_computer_use_response());
            },
        ) {
            View(flex_direction: FlexDirection::Column, padding_left: 1u32, padding_right: 1u32, padding_top: 1u32, padding_bottom: 1u32) {
                View(flex_direction: FlexDirection::Column) {
                    Text(content: format!("Accessibility: {} {}", if tcc_state.accessibility { MAIN_SYMBOLS.tick } else { MAIN_SYMBOLS.cross }, if tcc_state.accessibility { "granted" } else { "not granted" }), wrap: TextWrap::NoWrap)
                    Text(content: format!("Screen Recording: {} {}", if tcc_state.screen_recording { MAIN_SYMBOLS.tick } else { MAIN_SYMBOLS.cross }, if tcc_state.screen_recording { "granted" } else { "not granted" }), wrap: TextWrap::NoWrap)
                }
                Text(
                    content: "Grant the missing permissions in System Settings, then select \"Try again\". macOS may require you to restart Claude Code after granting Screen Recording.".to_string(),
                    color: theme.inactive,
                    wrap: TextWrap::Wrap,
                )
                Select(
                    options: select_options,
                    focused_index: focused,
                    selected_value: None,
                    visible_option_count: options.len(),
                    visible_from_index: 0usize,
                    layout: SelectLayout::Expanded,
                    hide_indexes: true,
                    is_disabled: false,
                )
            }
        }
    }
}

#[derive(Default, Props)]
struct ComputerUseAppListPanelProps {
    pub request: CuPermissionRequest,
    pub on_done: Handler<CuPermissionResponse>,
}

/// Maps to: CC `ComputerUseAppListPanel`.
#[component]
fn ComputerUseAppListPanel(
    props: &ComputerUseAppListPanelProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let request = props.request.clone();
    let on_done = props.on_done.clone();
    let on_done_cancel = props.on_done.clone();
    let checked = hooks.use_const({
        let request = request.clone();
        move || default_checked_bundle_ids(&request)
    });
    let mut focused_index = hooks.use_state(|| 0usize);
    let mut pending_allow = hooks.use_state(|| Option::<bool>::None);
    let options = [AppListOption::AllowAll, AppListOption::Deny];

    hooks.use_terminal_events({
        let mut focused_index = focused_index;
        let mut pending_allow = pending_allow;
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
                    focused_index.set((focused_index.get() + 1).min(1));
                }
                KeyCode::Enter => pending_allow.set(Some(focused_index.get() == 0)),
                KeyCode::Esc => pending_allow.set(Some(false)),
                _ => {}
            }
        }
    });

    let pending = { pending_allow.read().clone() };
    if let Some(allow) = pending {
        pending_allow.set(None);
        let now_ms = chrono::Utc::now().timestamp_millis();
        (on_done)(computer_use_approval_response(
            &request, &checked, allow, now_ms,
        ));
    }

    let focused = focused_index.get().min(1);
    let select_options = options
        .iter()
        .map(|option| SelectOptionData {
            label: match option {
                AppListOption::AllowAll => format!(
                    "Allow for this session ({} {})",
                    checked.len(),
                    plural(checked.len(), "app")
                ),
                AppListOption::Deny => {
                    "Deny, and tell Claude what to do differently (esc)".to_string()
                }
            },
            description: None,
            dim_description: true,
            value: option.as_str().to_string(),
            disabled: false,
            input: None,
        })
        .collect::<Vec<_>>();
    let requested_flag_keys = requested_flag_keys(request.requested_flags);

    element! {
        Dialog(
            title: "Computer Use wants to control these apps".to_string(),
            on_cancel: move |_| {
                (on_done_cancel)(deny_all_computer_use_response());
            },
        ) {
            View(flex_direction: FlexDirection::Column, padding_left: 1u32, padding_right: 1u32, padding_top: 1u32, padding_bottom: 1u32) {
                #(request.reason.as_ref().filter(|reason| !reason.trim().is_empty()).map(|reason| element! {
                    Text(content: reason.clone(), dim: true, wrap: TextWrap::Wrap)
                }))
                View(flex_direction: FlexDirection::Column) {
                    #(request.apps.iter().map(|app| {
                        match &app.resolved {
                            None => element! {
                                Text(
                                    content: format!("  {} {} (not installed)", MAIN_SYMBOLS.circle, app.requested_name),
                                    dim: true,
                                    wrap: TextWrap::Wrap,
                                )
                            }.into_any(),
                            Some(resolved) if app.already_granted => element! {
                                Text(
                                    content: format!("  {} {} (already granted)", MAIN_SYMBOLS.tick, resolved.display_name),
                                    dim: true,
                                    wrap: TextWrap::Wrap,
                                )
                            }.into_any(),
                            Some(resolved) => {
                                let is_checked = checked.contains(&resolved.bundle_id);
                                let sentinel = get_sentinel_category(&resolved.bundle_id);
                                element! {
                                    View(flex_direction: FlexDirection::Column) {
                                        Text(
                                            content: format!("  {} {}", if is_checked { MAIN_SYMBOLS.circle_filled } else { MAIN_SYMBOLS.circle }, resolved.display_name),
                                            wrap: TextWrap::Wrap,
                                        )
                                        #(sentinel.map(|category| element! {
                                            Text(
                                                content: format!("    {} {}", MAIN_SYMBOLS.warning, category.warning_text()),
                                                weight: Weight::Bold,
                                                wrap: TextWrap::Wrap,
                                            )
                                        }))
                                    }
                                }.into_any()
                            }
                        }
                    }))
                }
                #(if requested_flag_keys.is_empty() {
                    None
                } else {
                    Some(element! {
                        View(flex_direction: FlexDirection::Column) {
                            Text(content: "Also requested:".to_string(), dim: true, wrap: TextWrap::NoWrap)
                            #(requested_flag_keys.iter().map(|flag| element! {
                                Text(content: format!("  · {flag}"), dim: true, wrap: TextWrap::NoWrap)
                            }))
                        }
                    })
                })
                #(if request.will_hide.is_empty() {
                    None
                } else {
                    Some(element! {
                        Text(
                            content: format!(
                                "{} other {} will be hidden while Claude works.",
                                request.will_hide.len(),
                                plural(request.will_hide.len(), "app"),
                            ),
                            color: theme.inactive,
                            wrap: TextWrap::Wrap,
                        )
                    })
                })
                Select(
                    options: select_options,
                    focused_index: focused,
                    selected_value: None,
                    visible_option_count: options.len(),
                    visible_from_index: 0usize,
                    layout: SelectLayout::Expanded,
                    hide_indexes: true,
                    is_disabled: false,
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;

    fn app_request() -> CuPermissionRequest {
        CuPermissionRequest {
            request_id: "cu-1".to_string(),
            reason: Some("Need to inspect the terminal".to_string()),
            apps: vec![
                CuAppRequest {
                    requested_name: "Terminal".to_string(),
                    resolved: Some(CuResolvedApp {
                        bundle_id: "com.apple.Terminal".to_string(),
                        display_name: "Terminal".to_string(),
                    }),
                    already_granted: false,
                },
                CuAppRequest {
                    requested_name: "MissingApp".to_string(),
                    resolved: None,
                    already_granted: false,
                },
            ],
            requested_flags: CuGrantFlags {
                clipboard_read: true,
                clipboard_write: false,
                system_key_combos: true,
            },
            tcc_state: None,
            will_hide: vec![CuHiddenApp {
                bundle_id: "com.apple.TextEdit".to_string(),
                display_name: "TextEdit".to_string(),
            }],
        }
    }

    #[test]
    fn computer_use_sentinel_categories_match_official_sets() {
        assert_eq!(
            get_sentinel_category("com.apple.Terminal"),
            Some(SentinelCategory::Shell)
        );
        assert_eq!(
            get_sentinel_category("com.apple.finder"),
            Some(SentinelCategory::Filesystem)
        );
        assert_eq!(
            get_sentinel_category("com.apple.systempreferences"),
            Some(SentinelCategory::SystemSettings)
        );
        assert_eq!(get_sentinel_category("com.example.App"), None);
    }

    #[test]
    fn computer_use_approval_response_matches_official_allow_and_deny_shapes() {
        let request = app_request();
        let checked = default_checked_bundle_ids(&request);
        assert!(checked.contains("com.apple.Terminal"));

        let allowed = computer_use_approval_response(&request, &checked, true, 1234);
        assert_eq!(allowed.granted.len(), 1);
        assert_eq!(allowed.granted[0].bundle_id, "com.apple.Terminal");
        assert_eq!(allowed.denied.len(), 1);
        assert_eq!(allowed.denied[0].bundle_id, "MissingApp");
        assert_eq!(allowed.denied[0].reason, AppDenialReason::NotInstalled);
        assert!(allowed.flags.clipboard_read);
        assert!(allowed.flags.system_key_combos);

        let denied = computer_use_approval_response(&request, &checked, false, 1234);
        assert!(denied.granted.is_empty());
        assert!(denied.denied.is_empty());
        assert_eq!(denied.flags, DEFAULT_GRANT_FLAGS);
    }

    #[test]
    fn computer_use_tcc_options_and_urls_match_official_panel() {
        let options = computer_use_tcc_options(CuTccState {
            accessibility: false,
            screen_recording: true,
        });
        assert_eq!(
            options,
            vec![TccOption::OpenAccessibility, TccOption::Retry]
        );
        assert_eq!(
            computer_use_tcc_open_url(TccOption::OpenAccessibility),
            Some("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
        );
        assert_eq!(computer_use_tcc_open_url(TccOption::Retry), None);
    }

    #[test]
    fn computer_use_approval_renders_official_app_list_copy() {
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                ComputerUseApproval(request: Some(app_request()))
            }
        }
        .render(Some(120))
        .to_string();

        assert!(
            text.contains("Computer Use wants to control these apps"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("Need to inspect the terminal"),
            "canvas=\n{text}"
        );
        assert!(text.contains("Terminal"), "canvas=\n{text}");
        assert!(
            text.contains("equivalent to shell access"),
            "canvas=\n{text}"
        );
        assert!(text.contains("MissingApp"), "canvas=\n{text}");
        assert!(text.contains("clipboardRead"), "canvas=\n{text}");
        assert!(text.contains("systemKeyCombos"), "canvas=\n{text}");
        assert!(
            text.contains("1 other app will be hidden"),
            "canvas=\n{text}"
        );
    }

    #[test]
    fn computer_use_approval_renders_official_tcc_copy() {
        let request = CuPermissionRequest {
            tcc_state: Some(CuTccState {
                accessibility: false,
                screen_recording: false,
            }),
            ..CuPermissionRequest::default()
        };
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                ComputerUseApproval(request: Some(request))
            }
        }
        .render(Some(120))
        .to_string();

        assert!(
            text.contains("Computer Use needs macOS permissions"),
            "canvas=\n{text}"
        );
        assert!(text.contains("Accessibility:"), "canvas=\n{text}");
        assert!(text.contains("Screen Recording:"), "canvas=\n{text}");
        assert!(
            text.contains("Open System Settings → Accessibility"),
            "canvas=\n{text}"
        );
        assert!(text.contains("Try again"), "canvas=\n{text}");
    }
}
