//! Maps to: CC components/Settings/Config.tsx
//! Config tab — scrollable setting items list.
//! CC layout (line 2153-2230):
//!   - Label column: width=44, selected row has "❯" pointer + suggestion color
//!   - Value column: default fg, suggestion color when selected
//!   - Viewport: slice(scrollOffset, scrollOffset + maxVisible)
//!   - Scroll hints: "↑ N more above" / "↓ N more below"

use super::items::{self, SettingItem, SettingValue};
use crate::components::channel_downgrade_dialog::{ChannelDowngradeChoice, ChannelDowngradeDialog};
use crate::components::custom_select::{
    Select, SelectInputOptionMeta, SelectLayout, SelectOptionData, UseSelectInputOptions,
    UseSelectStateProps, use_select_input, use_select_state,
};
use crate::components::design_system::dialog::Dialog;
use crate::components::design_system::theme_provider::use_theme;
use crate::utils::theme::ThemeSetting;
use crate::components::language_picker::LanguagePicker;
use crate::components::model_picker as model;
use crate::components::model_picker::{ModelPicker, ModelPickerSelection};
use crate::components::output_style_picker as output_style;
use crate::components::output_style_picker::OutputStylePicker;
use crate::components::theme_picker::ThemePicker;
use crate::constants::product;
use crate::context::notifications::use_notifications;
use crate::context::notifications::{
    Notification, NotificationColor, NotificationPriority, NotificationsWriter,
};
use crate::hooks::use_search_input::{SearchInput, use_search_input};
use crate::utils::theme;
use iocraft::prelude::*;

const POINTER: &str = "❯";
const LABEL_WIDTH: u32 = 44;
const PLACEHOLDER: &str = "Search settings…";

fn item_matches_query(item: &SettingItem, query_lower: &str) -> bool {
    if !item.visible {
        return false;
    }
    query_lower.is_empty()
        || item.label.to_lowercase().contains(query_lower)
        || item.search_text.to_lowercase().contains(query_lower)
}

/// Maps to: CC `Config.tsx` `showSubmenu`'s values (TeammateModel and
/// ExternalIncludes are not ported).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingsSubmenu {
    Theme,
    Model,
    OutputStyle,
    Language,
    EnableAutoUpdates,
    ChannelDowngrade,
}

impl SettingsSubmenu {
    fn setting_id(self) -> &'static str {
        match self {
            Self::Theme => "theme",
            Self::Model => "model",
            Self::OutputStyle => "outputStyle",
            Self::Language => "language",
            Self::EnableAutoUpdates | Self::ChannelDowngrade => "autoUpdatesChannel",
        }
    }
}

fn submenu_for_setting(id: &str) -> Option<SettingsSubmenu> {
    match id {
        "theme" => Some(SettingsSubmenu::Theme),
        "model" => Some(SettingsSubmenu::Model),
        "outputStyle" => Some(SettingsSubmenu::OutputStyle),
        "language" => Some(SettingsSubmenu::Language),
        _ => None,
    }
}

/// Maps to: CC `Config.tsx`:890-896 — the language row shows
/// `currentLanguage ?? 'Default (English)'`.
fn language_display_value(language: Option<&str>) -> String {
    language.unwrap_or("Default (English)").to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AutoUpdatesAction {
    Open(SettingsSubmenu),
    SetLatest,
}

fn auto_updates_action_for_item(item: &SettingItem) -> Option<AutoUpdatesAction> {
    if item.id != "autoUpdatesChannel" {
        return None;
    }
    let value = item.display_value();
    if value.eq_ignore_ascii_case("disabled") {
        Some(AutoUpdatesAction::Open(SettingsSubmenu::EnableAutoUpdates))
    } else if value.eq_ignore_ascii_case("latest") {
        Some(AutoUpdatesAction::Open(SettingsSubmenu::ChannelDowngrade))
    } else {
        Some(AutoUpdatesAction::SetLatest)
    }
}

fn submenu_for_item(item: &SettingItem) -> Option<SettingsSubmenu> {
    match auto_updates_action_for_item(item) {
        Some(AutoUpdatesAction::Open(menu)) => Some(menu),
        Some(AutoUpdatesAction::SetLatest) => None,
        None => submenu_for_setting(item.id),
    }
}

fn apply_runtime_display_settings(items: &mut [SettingItem], prefers_reduced_motion: bool) {
    if let Some(item) = items
        .iter_mut()
        .find(|item| item.id == "prefersReducedMotion")
    {
        item.value = SettingValue::Bool(prefers_reduced_motion);
    }
}

fn set_theme_row_value(items: &mut [SettingItem], setting: ThemeSetting) {
    if let Some(item) = items.iter_mut().find(|item| item.id == "theme") {
        item.value =
            SettingValue::Display(theme::theme_display_label(Some(setting.setting_value())).to_string());
    }
}

fn items_with_runtime_context(
    settings_snapshot: Option<&crate::utils::settings::SettingsJson>,
    prefers_reduced_motion: bool,
) -> Vec<SettingItem> {
    // Maps to: CC Config.tsx reading `useAppState(s => s.settings)` plus
    // `getGlobalConfig()` cached direct reads for the initial item values.
    let mut items = settings_snapshot
        .map(|snapshot| items::build_items(&crate::utils::config::load_global_config(), snapshot))
        .unwrap_or_else(items::default_items);
    apply_runtime_display_settings(&mut items, prefers_reduced_motion);
    items
}

fn preview_runtime_display_setting(
    store: Option<crate::state::store::AppStore>,
    item: &SettingItem,
) {
    if item.id != "prefersReducedMotion" {
        return;
    }
    let SettingValue::Bool(value) = &item.value else {
        return;
    };
    // In-memory preview writes the AppState settings snapshot (CC
    // setAppState); the disk watcher never sees it, matching UI-only preview
    // semantics.
    if let Some(store) = store {
        store.replace_with(|state| {
            let mut settings = (*state.settings).clone();
            settings.prefers_reduced_motion = Some(*value);
            state.settings = std::sync::Arc::new(settings);
        });
    }
}

fn preview_streaming_text_display_setting(
    store: Option<crate::state::store::AppStore>,
    item: &SettingItem,
) {
    if item.id != "streamingTextDisplay" {
        return;
    }
    let value = item.display_value();
    // Same in-memory preview route as `prefersReducedMotion`: REPL reads the
    // AppState settings snapshot, so the next stream uses the new mode.
    if let Some(store) = store {
        store.replace_with(|state| {
            let mut settings = (*state.settings).clone();
            settings.streaming_text_display = Some(value.clone());
            state.settings = std::sync::Arc::new(settings);
        });
    }
}

fn preview_verbose_setting(store: Option<crate::state::store::AppStore>, item: &SettingItem) {
    if item.id != "verbose" {
        return;
    }
    let SettingValue::Bool(value) = &item.value else {
        return;
    };
    // Maps to: CC Config preview via `setAppState` — verbose lives in AppState,
    // not in a separate footer-context source of truth.
    if let Some(store) = store {
        store.replace_with(|state| state.verbose = *value);
    }
}

fn preview_expand_display_setting(
    store: Option<crate::state::store::AppStore>,
    item: &SettingItem,
) {
    let SettingValue::Bool(value) = &item.value else {
        return;
    };
    let Some(store) = store else {
        return;
    };
    match item.id {
        "expandThinking" => store.replace_with(|state| state.expand_thinking = *value),
        "expandCollapsedReadSearch" => {
            store.replace_with(|state| state.expand_collapsed_read_search = *value)
        }
        _ => {}
    }
}

fn preview_prompt_suggestion_setting(
    store: Option<crate::state::store::AppStore>,
    item: &SettingItem,
) {
    if item.id != "promptSuggestionEnabled" {
        return;
    }
    let SettingValue::Bool(value) = &item.value else {
        return;
    };
    // Maps to: CC Config `setAppState({ promptSuggestionEnabled })`.
    if let Some(store) = store {
        store.replace_with(|state| {
            state.prompt_suggestion_enabled = *value;
            let mut settings = (*state.settings).clone();
            settings.prompt_suggestion_enabled = Some(*value);
            state.settings = std::sync::Arc::new(settings);
        });
    }
}

fn notification_preview_for_value(value: &str) -> Option<Notification> {
    if matches!(
        value.trim(),
        "Disabled" | "disabled" | "notifications_disabled"
    ) {
        return None;
    }
    Some(
        Notification::text(
            "config-notification-preview",
            format!("Notifications preview: {value}"),
            NotificationPriority::Immediate,
        )
        .with_color(NotificationColor::Suggestion)
        .with_timeout_ms(5_000),
    )
}

fn permission_mode_display(value: &str) -> String {
    match value {
        "default" => "Default".to_string(),
        "plan" => "Plan Mode".to_string(),
        "acceptEdits" => "Accept edits".to_string(),
        "dontAsk" => "Don't Ask".to_string(),
        "bypassPermissions" => "Bypass Permissions".to_string(),
        "auto" => "Auto mode".to_string(),
        other => other.to_string(),
    }
}

fn item_display_value(item: &SettingItem) -> String {
    if item.id == "notifChannel" {
        items::notification_channel_display(&item.display_value())
    } else if item.id == "defaultPermissionMode" {
        permission_mode_display(&item.display_value())
    } else {
        item.display_value()
    }
}

fn preview_notification_setting_selection(mut context: NotificationsWriter, item: &SettingItem) {
    if item.id != "notifChannel" {
        return;
    }
    match notification_preview_for_value(&item_display_value(item)) {
        Some(notification) => context.add_notification(notification),
        None => context.remove_notification("config-notification-preview"),
    }
}

/// Maps to: CC `Config.tsx:2041-2050`, the EnableAutoUpdates Select.
fn enable_auto_updates_options() -> Vec<SelectOptionData> {
    [("Enable with latest channel", "latest"), ("Enable with stable channel", "stable")]
        .into_iter()
        .map(|(label, value)| SelectOptionData {
            label: label.to_string(),
            value: value.to_string(),
            ..SelectOptionData::default()
        })
        .collect()
}

#[derive(Default, Props)]
struct EnableAutoUpdatesSelectProps<'a> {
    /// CC `onChange(channel)` (:2051).
    on_change: HandlerMut<'a, String>,
}

/// L1 (inline Select state carrier): CC writes this `<Select>` inline in
/// Config (`Config.tsx:2040-2076`), and CC's Select is a stateful component
/// that mounts and unmounts with the EnableAutoUpdates submenu. The port's
/// Select only renders, so the state it would own lives here, mounted in its
/// place; this component adds nothing else.
#[component]
fn EnableAutoUpdatesSelect<'a>(
    props: &mut EnableAutoUpdatesSelectProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let options = enable_auto_updates_options();
    // No default value, no focus value and no cancel: the default five-row
    // viewport, focus on the first option, Esc left to the Dialog.
    let state = use_select_state(
        &mut hooks,
        UseSelectStateProps {
            visible_option_count: Some(5),
            values: options.iter().map(|option| option.value.clone()).collect(),
            default_value: None,
            focus_value: None,
        },
    );
    let events = use_select_input(
        &mut hooks,
        state,
        UseSelectInputOptions {
            option_metas: options
                .iter()
                .map(|option| SelectInputOptionMeta {
                    value: option.value.clone(),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        },
    );
    if let Some(channel) = events.take_accepted() {
        (props.on_change)(channel);
    }
    let navigation = state.navigation.snapshot();
    element! {
        Select(
            is_disabled: false,
            hide_indexes: false,
            visible_option_count: navigation.visible_option_count,
            options: options,
            focused_index: navigation.focused_index().unwrap_or(0),
            visible_from_index: navigation.visible_from_index,
            layout: SelectLayout::Compact,
        )
    }
}

fn auto_updates_disabled_env_var() -> Option<&'static str> {
    if crate::utils::env_utils::is_env_truthy(crate::utils::process_env::var("DISABLE_AUTOUPDATER").as_deref())
    {
        Some("DISABLE_AUTOUPDATER")
    } else if crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_DISABLE_AUTOUPDATER")
            .as_deref(),
    ) {
        Some("CLAUDE_CODE_DISABLE_AUTOUPDATER")
    } else {
        // CC `config.ts:1742` `getEssentialTrafficOnlyReason()`: a plain
        // truthiness test, not `isEnvTruthy`.
        crate::utils::privacy_level::get_essential_traffic_only_reason()
    }
}

fn auto_updates_disabled_reason_label() -> &'static str {
    match auto_updates_disabled_env_var() {
        Some(env_var) => match env_var {
            "DISABLE_AUTOUPDATER" => "DISABLE_AUTOUPDATER set",
            "CLAUDE_CODE_DISABLE_AUTOUPDATER" => "CLAUDE_CODE_DISABLE_AUTOUPDATER set",
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC" => {
                "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC set"
            }
            _ => "environment",
        },
        None => "config",
    }
}

fn model_value_for_display(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.eq_ignore_ascii_case("Default (recommended)")
        || trimmed.eq_ignore_ascii_case("default")
    {
        model::MODEL_NO_PREFERENCE.to_string()
    } else {
        trimmed.to_string()
    }
}


fn apply_settings_submenu_selection(
    items: &mut [SettingItem],
    menu: SettingsSubmenu,
    value: &str,
) -> bool {
    let Some(item) = items.iter_mut().find(|item| item.id == menu.setting_id()) else {
        return false;
    };
    item.value = SettingValue::Display(value.to_string());
    true
}

fn set_auto_updates_channel_latest(items: &mut [SettingItem], real_idx: usize) -> bool {
    let Some(item) = items.get_mut(real_idx) else {
        return false;
    };
    if item.id != "autoUpdatesChannel" {
        return false;
    }
    item.value = SettingValue::Display("latest".to_string());
    true
}

fn config_summary_label(item: &SettingItem) -> &'static str {
    match item.id {
        "autoCompactEnabled" => "auto-compact",
        "spinnerTipsEnabled" => "tips",
        "prefersReducedMotion" => "reduce motion",
        "streamingTextDisplay" => "streaming text",
        "thinkingEnabled" => "thinking mode",
        "fastMode" => "fast mode",
        "promptSuggestionEnabled" => "prompt suggestions",
        "fileCheckpointingEnabled" => "rewind code (checkpoints)",
        "verbose" => "verbose",
        "expandThinking" => "expand thinking blocks",
        "expandCollapsedReadSearch" => "expand read/search groups",
        "terminalProgressBarEnabled" => "terminal progress bar",
        "showStatusInTerminalTab" => "terminal tab status",
        "showTurnDuration" => "turn duration",
        "defaultPermissionMode" => "default permission mode",
        "useAutoModeDuringPlan" => "auto mode in plan mode",
        "respectGitignore" => "respect .gitignore in file picker",
        "copyFullResponse" => "always copy full response",
        "copyOnSelect" => "copy on select",
        "autoUpdatesChannel" => "auto-update channel",
        "theme" => "theme",
        "model" => "model",
        "notifChannel" => "notifications",
        "outputStyle" => "output style",
        "defaultView" => "default view",
        "language" => "response language",
        "editorMode" => "editor mode",
        "prStatusFooterEnabled" => "PR status footer",
        "diffTool" => "diff tool",
        "autoConnectIde" => "auto-connect to IDE",
        "autoInstallIdeExtension" => "auto-install IDE extension",
        "remoteControl" => "Remote Control for all sessions",
        _ => item.label,
    }
}

fn format_config_change_line(item: &SettingItem, value: &str) -> String {
    let label = config_summary_label(item);
    match &item.value {
        SettingValue::Bool(true) => format!("Enabled {label}"),
        SettingValue::Bool(false) => format!("Disabled {label}"),
        _ => format!("Set {label} to {value}"),
    }
}

fn format_config_change_summary(
    initial: &[SettingItem],
    current: &[SettingItem],
) -> Option<String> {
    let mut lines = Vec::new();
    for current_item in current.iter().filter(|item| item.visible) {
        let Some(initial_item) = initial.iter().find(|item| item.id == current_item.id) else {
            continue;
        };
        if current_item.display_value() == initial_item.display_value() {
            continue;
        }
        lines.push(format_config_change_line(
            current_item,
            &item_display_value(current_item),
        ));
    }
    if lines.is_empty() {
        return None;
    }
    lines.push("(UI-only preview; settings were not written)".to_string());
    Some(lines.join("\n"))
}

fn revert_runtime_previews(
    store: Option<crate::state::store::AppStore>,
    notifications_context: NotificationsWriter,
    initial_items: &[SettingItem],
) {
    if let Some(item) = initial_items
        .iter()
        .find(|item| item.id == "prefersReducedMotion")
    {
        preview_runtime_display_setting(store.clone(), item);
    }
    if let Some(item) = initial_items
        .iter()
        .find(|item| item.id == "streamingTextDisplay")
    {
        preview_streaming_text_display_setting(store.clone(), item);
    }
    if let Some(item) = initial_items.iter().find(|item| item.id == "verbose") {
        preview_verbose_setting(store.clone(), item);
    }
    for id in ["expandThinking", "expandCollapsedReadSearch"] {
        if let Some(item) = initial_items.iter().find(|item| item.id == id) {
            preview_expand_display_setting(store.clone(), item);
        }
    }
    if let Some(item) = initial_items
        .iter()
        .find(|item| item.id == "promptSuggestionEnabled")
    {
        preview_prompt_suggestion_setting(store, item);
    }
    let mut notifications_context = notifications_context;
    notifications_context.remove_notification("config-notification-preview");
}

fn activate_focused_config_item(
    mut items: State<Vec<SettingItem>>,
    selected: State<usize>,
    search: SearchInput,
    mut submenu: State<Option<SettingsSubmenu>>,
    mut tabs_hidden_request: State<Option<bool>>,
    runtime_display_context: Option<crate::state::store::AppStore>,
    runtime_notifications_context: NotificationsWriter,
) {
    let query_lower = search.text().to_lowercase();
    let visible_indices = items
        .read()
        .iter()
        .enumerate()
        .filter(|(_, item)| item_matches_query(item, &query_lower))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let Some(&real_idx) = visible_indices.get(selected.get()) else {
        return;
    };
    let current_item = items.read()[real_idx].clone();
    if matches!(
        auto_updates_action_for_item(&current_item),
        Some(AutoUpdatesAction::SetLatest)
    ) {
        let mut all = items.read().clone();
        set_auto_updates_channel_latest(&mut all, real_idx);
        items.set(all);
        return;
    }
    // CC :1604-1653: a submenu row opens its submenu and hides the tabs; the
    // submenu seeds its own state from the current value.
    if let Some(menu) = submenu_for_item(&current_item) {
        tabs_hidden_request.set(Some(true));
        submenu.set(Some(menu));
        return;
    }

    let mut all = items.read().clone();
    all[real_idx].toggle();
    preview_runtime_display_setting(runtime_display_context.clone(), &all[real_idx]);
    preview_streaming_text_display_setting(runtime_display_context.clone(), &all[real_idx]);
    preview_verbose_setting(runtime_display_context.clone(), &all[real_idx]);
    preview_expand_display_setting(runtime_display_context.clone(), &all[real_idx]);
    preview_prompt_suggestion_setting(runtime_display_context, &all[real_idx]);
    preview_notification_setting_selection(runtime_notifications_context, &all[real_idx]);
    items.set(all);
}

#[derive(Default, Props)]
pub struct ConfigProps<'a> {
    /// Maps to: CC paneCap - 10, passed from Settings
    pub max_visible: Option<u32>,
    /// Maps to: Tabs.tsx/useTabHeaderFocus headerFocused.
    /// When true, Config content is visually unfocused and must not consume
    /// list/search navigation keys; the tab row owns them.
    pub header_focused: bool,
    /// Native transport for useTabHeaderFocus().focusHeader in the source.
    pub on_focus_header: Handler<()>,
    /// Maps to official Config `setTabsHidden(true/false)` around submenus.
    pub on_tabs_hidden_change: HandlerMut<'a, bool>,
    /// Close the enclosing local command UI without a custom output row.
    pub on_close: HandlerMut<'a, ()>,
    /// Close with an official-shaped local command output summary.
    pub on_result: HandlerMut<'a, String>,
    /// Maps to official Settings `onIsSearchModeChange`: while Config search
    /// owns Esc, Settings must not close the local command UI first.
    pub on_is_search_mode_change: HandlerMut<'a, bool>,
}

/// Maps to: CC Config.tsx
#[component]
pub fn Config<'a>(props: &mut ConfigProps<'a>, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let settings_snapshot =
        crate::state::app_state::use_app_state(&mut hooks, |state| state.settings.clone());
    let runtime_display_context = hooks
        .try_use_context::<crate::state::store::AppStore>()
        .map(|store| store.clone());
    let initial_prefers_reduced_motion = settings_snapshot.prefers_reduced_motion.unwrap_or(false);
    let runtime_notifications_context = use_notifications(&mut hooks);
    // Maps to: CC `Config.tsx:180-181` `useTheme()` / `useThemeSetting()`, and
    // ThemePicker's `usePreviewTheme()` (`ThemePicker.tsx:55`): the Theme
    // submenu previews, saves and cancels through the root ThemeProvider, so
    // the whole app previews. A save persists through `saveGlobalConfig`,
    // which is a dry run unless config writes are enabled.
    let (_, theme_control) = use_theme(&hooks);
    let mut initial_items_seed = items_with_runtime_context(
        Some(settings_snapshot.as_ref()),
        initial_prefers_reduced_motion,
    );
    // CC's Theme row shows the provider's `themeSetting` (`Config.tsx:759-761`).
    set_theme_row_value(&mut initial_items_seed, theme_control.theme_setting());
    let initial_items_for_state = initial_items_seed.clone();
    let initial_items = hooks.use_state(move || initial_items_for_state);
    let mut items = hooks.use_state(move || initial_items_seed);
    let mut selected = hooks.use_state(|| 0usize);
    let mut scroll_offset = hooks.use_state(|| 0usize);
    // Maps to: CC Config.tsx useSearchInput — query + cursor offset + nav
    let search = use_search_input(&mut hooks, "");
    // Maps to: CC Config.tsx isSearchMode — default true (search box focused)
    let mut is_search_mode = hooks.use_state(|| true);
    // Maps to: CC Config.tsx:197 `useTerminalFocus()`, handed to SearchBox so
    // the cursor cell disappears while the terminal is blurred.
    let is_terminal_focused = hooks.use_terminal_focus();
    // Maps to: CC `Config.tsx:179` `useIsInsideModal()`, which drops the
    // list container's `marginY` inside the fullscreen modal slot (:2121).
    let inside_modal = crate::context::modal_context::use_is_inside_modal(&hooks);
    // Maps to: CC Config.tsx `showSubmenu`. Submenus are UI-only previews:
    // selecting an option updates this in-memory Config item list only.
    let mut submenu = hooks.use_state(|| None::<SettingsSubmenu>);
    // Maps to: CC `Config.tsx:190-193` `currentLanguage` and its
    // `initialLanguage` ref. The language row shows `currentLanguage ??
    // 'Default (English)'`; LanguagePicker starts from `currentLanguage`
    // itself, never from that display text. (The row's change summary still
    // comes from the shared display-value diff, so a language typed as the
    // literal "Default (English)" produces no summary line — seam.)
    let initial_language = hooks.use_state({
        let language = settings_snapshot.language.clone();
        move || language
    });
    let mut current_language = hooks.use_state(move || initial_language.read().clone());
    // CC `Config.tsx:245` `initialThemeSetting = useRef(themeSetting)`, which
    // Escape's `revertChanges` restores (`:1477-1479`).
    let initial_theme_setting = hooks.use_state({
        let setting = theme_control.theme_setting();
        move || setting
    });
    // CC `:242-244` `initialUserSettings` and the `settings` of
    // `initialAppState` (`:248-262`): ThemePicker's Ctrl+T writes into both,
    // and `revertChanges` restores them (`:1508-1510`, `:1533`).
    let initial_user_syntax_disabled = hooks.use_state(|| {
        crate::utils::settings::get_settings_for_source(crate::utils::settings::SettingSource::User)
            .and_then(|settings| settings.syntax_highlighting_disabled)
    });
    let initial_app_settings = hooks.use_state({
        let settings = settings_snapshot.clone();
        move || settings
    });
    let mut tabs_hidden_request = hooks.use_state(|| None::<bool>);
    let mut should_close = hooks.use_state(|| false);
    let mut pending_result = hooks.use_state(|| None::<String>);
    let mut last_reported_search_owns_esc = hooks.use_state(|| None::<bool>);

    let max_vis = props.max_visible.unwrap_or(12) as usize;
    let header_focused = props.header_focused;
    let focus_header = props.on_focus_header.clone();

    if let Some(hidden) = tabs_hidden_request.get() {
        tabs_hidden_request.set(None);
        (props.on_tabs_hidden_change)(hidden);
    }
    let result_to_emit = pending_result.read().clone();
    if let Some(result) = result_to_emit {
        pending_result.set(None);
        (props.on_result)(result);
    }
    if should_close.get() {
        should_close.set(false);
        (props.on_close)(());
    }
    let search_owns_esc = is_search_mode.get() && !header_focused && submenu.get().is_none();
    if last_reported_search_owns_esc.get() != Some(search_owns_esc) {
        last_reported_search_owns_esc.set(Some(search_owns_esc));
        (props.on_is_search_mode_change)(search_owns_esc);
    }

    // Maps to: CC Config.tsx search-mode branch. Register this before the
    // action hooks so a queued Enter followed by an action key observes the
    // focus transition even when iocraft polls multiple events in one frame.
    hooks.use_propagated_terminal_events({
        move |event| {
            if submenu.get().is_some() || header_focused || !is_search_mode.get() {
                return;
            }
            // CC useSearchInput's InputEvent bridge inserts the entire paste.
            if let TerminalEvent::Paste(text) = event.event() {
                let mut search = search;
                search.reset_key_state(&KeyCode::Char(' '), &KeyModifiers::empty());
                search.insert_text(text);
                selected.set(0);
                scroll_offset.set(0);
                event.stop_propagation();
                return;
            }
            let TerminalEvent::Key(KeyEvent {
                code,
                kind,
                modifiers,
                ..
            }) = event.event()
            else {
                return;
            };
            if *kind == KeyEventKind::Release {
                return;
            }
            // CC Config.tsx:278-285 excludes these before the hook resets
            // ring state; Settings' exit binding owns them.
            if modifiers.contains(KeyModifiers::CONTROL)
                && matches!(code, KeyCode::Char('c' | 'C' | 'd' | 'D'))
            {
                return;
            }
            let mut search = search;
            search.reset_key_state(code, modifiers);
            match code {
                KeyCode::Up => {
                    focus_header(());
                    event.stop_propagation();
                }
                KeyCode::Backspace
                    if search.is_empty() && !modifiers.contains(KeyModifiers::ALT) =>
                {
                    is_search_mode.set(false);
                    event.stop_propagation();
                }
                KeyCode::Char('h' | 'H')
                    if search.is_empty() && modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    is_search_mode.set(false);
                    event.stop_propagation();
                }

                KeyCode::Esc => {
                    if !search.is_empty() {
                        search.clear();
                        selected.set(0);
                        scroll_offset.set(0);
                    } else {
                        is_search_mode.set(false);
                    }
                    event.stop_propagation();
                }
                KeyCode::Enter | KeyCode::Down => {
                    is_search_mode.set(false);
                    selected.set(0);
                    scroll_offset.set(0);
                    event.stop_propagation();
                }
                _ => {
                    let before = search.text();
                    if search.handle_edit_key(code, modifiers) {
                        if search.text() != before {
                            selected.set(0);
                            scroll_offset.set(0);
                        }
                        event.stop_propagation();
                    }
                }
            }
        }
    });

    let keybinding_runtime = hooks
        .try_use_context::<crate::keybindings::keybinding_context::KeybindingRuntime>()
        .map(|runtime| runtime.clone());
    // CC `ConfigurableShortcutHint action="confirm:no" context="Settings"
    // fallback="Esc"` in the Language submenu footer (`Config.tsx:2003-2012`).
    let settings_cancel_shortcut = {
        let bindings = keybinding_runtime
            .as_ref()
            .map(|runtime| runtime.bindings())
            .unwrap_or_else(|| {
                std::sync::Arc::new(crate::keybindings::default_bindings::default_bindings())
            });
        crate::keybindings::shortcut_format::get_shortcut_display_from_bindings(
            "confirm:no",
            &crate::keybindings::types::ContextName::Settings,
            "Esc",
            bindings.as_slice(),
        )
    };
    // CC's Theme submenu footer resolves `confirm:no` in Confirmation
    // (`Config.tsx:1816-1821`), unlike the Language footer's Settings.
    let confirmation_cancel_shortcut = crate::keybindings::shortcut_format::get_shortcut_display_from_bindings(
        "confirm:no",
        &crate::keybindings::types::ContextName::Confirmation,
        "Esc",
        keybinding_runtime
            .as_ref()
            .map(|runtime| runtime.bindings())
            .unwrap_or_else(|| {
                std::sync::Arc::new(crate::keybindings::default_bindings::default_bindings())
            })
            .as_slice(),
    );
    crate::keybindings::use_keybinding::use_keybinding(
        &mut hooks,
        keybinding_runtime.clone(),
        "confirm:no",
        crate::keybindings::types::ContextName::Settings,
        move || submenu.get().is_none() && !is_search_mode.get() && !header_focused,
        {
            let runtime_display_context = runtime_display_context.clone();
            let runtime_notifications_context = runtime_notifications_context.clone();
            let theme_control = theme_control.clone();
            move || {
                // CC `Config.tsx:1477-1479`: the theme is restored first, and
                // only when it changed.
                if theme_control.theme_setting() != initial_theme_setting.get() {
                    theme_control.set_theme_setting(initial_theme_setting.get());
                }
                let initial = initial_items.read().clone();
                revert_runtime_previews(
                    runtime_display_context.clone(),
                    runtime_notifications_context.clone(),
                    &initial,
                );
                // CC `:1508-1510`: the user-settings key Ctrl+T writes. The
                // port reverts on every Escape (it has no `isDirty`), so it
                // writes only a changed value.
                let initial_disabled = initial_user_syntax_disabled.get();
                let current_disabled = crate::utils::settings::get_settings_for_source(
                    crate::utils::settings::SettingSource::User,
                )
                .and_then(|settings| settings.syntax_highlighting_disabled);
                if current_disabled != initial_disabled {
                    let _ = crate::utils::settings::update_settings_for_source(
                        crate::utils::settings::SettingSource::User,
                        &serde_json::Map::from_iter([(
                            "syntaxHighlightingDisabled".to_string(),
                            initial_disabled.map_or(serde_json::Value::Null, serde_json::Value::Bool),
                        )]),
                    );
                }
                // CC `:1533` `settings: ia.settings`.
                if let Some(store) = runtime_display_context.as_ref() {
                    let initial_settings = initial_app_settings.read().clone();
                    if !std::sync::Arc::ptr_eq(&store.get().settings, &initial_settings) {
                        store.replace_with(|state| state.settings = initial_settings.clone());
                    }
                }
                items.set(initial);
                current_language.set(initial_language.read().clone());
                should_close.set(true);
                true
            }
        },
    );
    crate::keybindings::use_keybinding::use_keybinding(
        &mut hooks,
        keybinding_runtime.clone(),
        "settings:close",
        crate::keybindings::types::ContextName::Settings,
        move || submenu.get().is_none() && !is_search_mode.get() && !header_focused,
        move || {
            let initial = initial_items.read();
            let current = items.read();
            if let Some(summary) = format_config_change_summary(&initial, &current) {
                pending_result.set(Some(summary));
            } else {
                should_close.set(true);
            }
            true
        },
    );
    crate::keybindings::use_keybinding::use_keybinding(
        &mut hooks,
        keybinding_runtime.clone(),
        "settings:search",
        crate::keybindings::types::ContextName::Settings,
        move || submenu.get().is_none() && !is_search_mode.get() && !header_focused,
        move || {
            let mut search = search;
            is_search_mode.set(true);
            search.clear();
            true
        },
    );
    crate::keybindings::use_keybinding::use_keybinding(
        &mut hooks,
        keybinding_runtime.clone(),
        "select:previous",
        crate::keybindings::types::ContextName::Settings,
        move || submenu.get().is_none() && !is_search_mode.get() && !header_focused,
        move || {
            let selected_index = selected.get();
            if selected_index == 0 {
                is_search_mode.set(true);
                scroll_offset.set(0);
            } else {
                let next = selected_index - 1;
                selected.set(next);
                if next < scroll_offset.get() {
                    scroll_offset.set(next);
                }
            }
            true
        },
    );
    crate::keybindings::use_keybinding::use_keybinding(
        &mut hooks,
        keybinding_runtime.clone(),
        "select:next",
        crate::keybindings::types::ContextName::Settings,
        move || submenu.get().is_none() && !is_search_mode.get() && !header_focused,
        move || {
            let query = search.text().to_lowercase();
            let count = items
                .read()
                .iter()
                .filter(|item| item_matches_query(item, &query))
                .count();
            let current = selected.get();
            if current + 1 < count {
                let next = current + 1;
                selected.set(next);
                if next >= scroll_offset.get() + max_vis {
                    scroll_offset.set(next - max_vis + 1);
                }
            }
            true
        },
    );
    crate::keybindings::use_keybinding::use_keybinding(
        &mut hooks,
        keybinding_runtime.clone(),
        "select:accept",
        crate::keybindings::types::ContextName::Settings,
        move || submenu.get().is_none() && !is_search_mode.get() && !header_focused,
        {
            let runtime_display_context = runtime_display_context.clone();
            let runtime_notifications_context = runtime_notifications_context.clone();
            move || {
                activate_focused_config_item(
                    items,
                    selected,
                    search,
                    submenu,
                    tabs_hidden_request,
                    runtime_display_context.clone(),
                    runtime_notifications_context.clone(),
                );
                true
            }
        },
    );
    hooks.use_propagated_terminal_events({
        move |event| match event.event() {
            TerminalEvent::Key(KeyEvent {
                code,
                kind,
                modifiers,
                ..
            }) if *kind != KeyEventKind::Release => {
                // CC :1737 `if (showSubmenu !== null) return`: every submenu
                // owns its keys.
                if submenu.get().is_some() {
                    return;
                }

                if header_focused {
                    // The Tabs header row owns navigation while focused.
                    return;
                }

                // Search mode is owned by the earlier propagation hook so it
                // commits focus changes before configurable actions run.
                if is_search_mode.get() {
                    return;
                }

                // List mode
                match code {
                    // CC :1761-1765: Left/Right/Tab run `toggleSetting()`,
                    // the same function as select:accept (Space). Shift+Tab
                    // is `tab` to Ink (parse-keypress.ts:405 `[Z`), BackTab
                    // to crossterm.
                    KeyCode::Tab | KeyCode::BackTab | KeyCode::Right | KeyCode::Left => {
                        activate_focused_config_item(
                            items,
                            selected,
                            search,
                            submenu,
                            tabs_hidden_request,
                            runtime_display_context.clone(),
                            runtime_notifications_context.clone(),
                        );
                        event.stop_propagation();
                    }
                    // CC :1770 `if (e.ctrl || e.meta) return`, leaving the
                    // key to other handlers; Ink's meta is Alt, as in
                    // iocraft's use_input.
                    _ if modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {}
                    // Maps to: CC line 1772-1775 — printable char enters search
                    KeyCode::Char(c) if *c != ' ' && *c != 'j' && *c != 'k' && *c != '/' => {
                        let mut search = search;
                        is_search_mode.set(true);
                        search.set(c.to_string());
                        event.stop_propagation();
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    });

    // The provider's palette, which a Theme submenu preview has already
    // switched for the whole app.
    let theme = *hooks.use_context::<theme::Theme>();

    // Rebuild visible list — filtered by search query
    // Maps to: CC Config.tsx filteredSettingsItems (line 1250-1258)
    let search_query = search.text();
    let query = search_query.to_lowercase();
    let visible: Vec<(usize, SettingItem)> = items
        .read()
        .iter()
        .enumerate()
        .filter(|(_, item)| item_matches_query(item, &query))
        .map(|(i, item)| (i, item.clone()))
        .collect();
    let count = visible.len();
    let ofs = scroll_offset.get();
    let end = (ofs + max_vis).min(count);
    let has_above = ofs > 0;
    let has_below = end < count;

    let is_search_focused = is_search_mode.get() && !header_focused;

    let content = if let Some(active_submenu) = submenu.get() {
        // The Model and OutputStyle footers (CC :1848-1858, :1964-1974):
        // `<KeyboardShortcutHint shortcut="Enter" action="confirm" />` and
        // confirm:no in the Confirmation context.
        let confirm_footer = format!("Enter to confirm · {confirmation_cancel_shortcut} to cancel");
        if active_submenu == SettingsSubmenu::Theme {
            // Maps to: CC `Config.tsx:1796-1825`. The picker owns its keys and
            // the preview; selecting saves through the provider (`setTheme`).
            // The footer is `<Text dimColor italic><Byline>…` as one inactive
            // line, like the Language footer.
            let theme_select = theme_control.clone();
            let footer = format!("Enter to select · {confirmation_cancel_shortcut} to cancel");
            element! {
                View(flex_direction: FlexDirection::Column) {
                    ThemePicker(
                        on_theme_select: move |setting: ThemeSetting| {
                            theme_select.set_theme_setting(setting);
                            let mut all = items.read().clone();
                            apply_settings_submenu_selection(
                                &mut all,
                                SettingsSubmenu::Theme,
                                theme::theme_display_label(Some(setting.setting_value())),
                            );
                            items.set(all);
                            submenu.set(None);
                            tabs_hidden_request.set(Some(false));
                        },
                        on_cancel: move |_| {
                            submenu.set(None);
                            tabs_hidden_request.set(Some(false));
                        },
                        hide_esc_to_cancel: true,
                        skip_exit_handling: true,
                    )
                    View {
                        Text(content: footer, color: theme.inactive, italic: true)
                    }
                }
            }
            .into_any()
        } else if active_submenu == SettingsSubmenu::Model {
            let initial = items
                .read()
                .iter()
                .find(|item| item.id == active_submenu.setting_id())
                .map(|item| model_value_for_display(&item.display_value()));
            element! {
                ContextProvider(value: Context::owned(theme)) {
                    View(flex_direction: FlexDirection::Column) {
                        ModelPicker(
                            initial: initial,
                            header_text: None,
                            session_model_label: None,
                            is_standalone_command: false,
                            skip_settings_write: true,
                            show_fast_mode_notice: false,
                            show_fast_mode_available_hint: false,
                            fast_mode_is_on: false,
                            exit_pending: false,
                            exit_key_name: None,
                            on_select: move |selection: ModelPickerSelection| {
                                let mut all = items.read().clone();
                                apply_settings_submenu_selection(
                                    &mut all,
                                    SettingsSubmenu::Model,
                                    selection.display_label(),
                                );
                                items.set(all);
                                submenu.set(None);
                                tabs_hidden_request.set(Some(false));
                            },
                            on_cancel: move |_| {
                                submenu.set(None);
                                tabs_hidden_request.set(Some(false));
                            },
                        )
                        // CC :1848-1858, one inactive line like the Language
                        // footer.
                        Text(content: confirm_footer, color: theme.inactive)
                    }
                }
            }
            .into_any()
        } else if active_submenu == SettingsSubmenu::OutputStyle {
            // Maps to: CC `Config.tsx:1935-1975`. The picker owns its keys;
            // completing updates the row (the in-memory preview above). The
            // row holds `currentOutputStyle` itself, the style value.
            let initial_style = items
                .read()
                .iter()
                .find(|item| item.id == active_submenu.setting_id())
                .map(SettingItem::display_value)
                .unwrap_or_else(|| {
                    crate::constants::output_styles::DEFAULT_OUTPUT_STYLE_NAME.to_string()
                });
            element! {
                View(flex_direction: FlexDirection::Column) {
                    OutputStylePicker(
                        initial_style: initial_style,
                        on_complete: move |style: String| {
                            let mut all = items.read().clone();
                            apply_settings_submenu_selection(
                                &mut all,
                                SettingsSubmenu::OutputStyle,
                                &style,
                            );
                            items.set(all);
                            submenu.set(None);
                            tabs_hidden_request.set(Some(false));
                        },
                        on_cancel: move |_| {
                            submenu.set(None);
                            tabs_hidden_request.set(Some(false));
                        },
                    )
                    Text(content: confirm_footer, color: theme.inactive)
                }
            }
            .into_any()
        } else if active_submenu == SettingsSubmenu::Language {
            // Maps to: CC `Config.tsx`:1976-2014.
            let initial_language = current_language.read().clone();
            // CC :2003-2012 `<Text dimColor><Byline><KeyboardShortcutHint
            // shortcut="Enter" action="confirm" /><ConfigurableShortcutHint
            // action="confirm:no" context="Settings" …/></Byline></Text>`,
            // as one ThemedText: the design-system hints render an inherited
            // dimColor as SGR dim rather than the `inactive` foreground.
            let footer = format!("Enter to confirm · {settings_cancel_shortcut} to cancel");

            element! {
                ContextProvider(value: Context::owned(theme)) {
                    View(flex_direction: FlexDirection::Column) {
                        LanguagePicker(
                            initial_language: initial_language,
                            on_complete: move |language: Option<String>| {
                                current_language.set(language.clone());
                                let mut all = items.read().clone();
                                apply_settings_submenu_selection(
                                    &mut all,
                                    SettingsSubmenu::Language,
                                    &language_display_value(language.as_deref()),
                                );
                                items.set(all);
                                submenu.set(None);
                                tabs_hidden_request.set(Some(false));
                            },
                            on_cancel: move |_| {
                                submenu.set(None);
                                tabs_hidden_request.set(Some(false));
                            },
                        )
                        Text(content: footer, color: theme.inactive)
                    }
                }
            }
            .into_any()
        } else if active_submenu == SettingsSubmenu::ChannelDowngrade {
            // Maps to: CC `Config.tsx:2079-2116`. Either choice moves the row
            // to stable. CC's `stay` also pins `minimumVersion` to the
            // current version (:2100-2103) in the settings write; the port
            // drops it with that write (the in-memory preview above), so the
            // write, when ported, must carry it.
            element! {
                ChannelDowngradeDialog(
                    current_version: product::VERSION.to_string(),
                    on_choice: move |choice: ChannelDowngradeChoice| {
                        submenu.set(None);
                        tabs_hidden_request.set(Some(false));
                        if choice == ChannelDowngradeChoice::Cancel {
                            return;
                        }
                        let mut all = items.read().clone();
                        apply_settings_submenu_selection(
                            &mut all,
                            SettingsSubmenu::ChannelDowngrade,
                            "stable",
                        );
                        items.set(all);
                    },
                )
            }
            .into_any()
        } else {
            // Maps to: CC `Config.tsx:2015-2078` (EnableAutoUpdates). An
            // environment variable leaves only its message (:2025-2038);
            // otherwise the Select re-enables updates on the chosen channel.
            // The port has no `development` reason here (see
            // `auto_updates_disabled_env_var`).
            let body = match auto_updates_disabled_env_var() {
                Some(env_var) => vec![
                    element! {
                        Text(
                            content: "Auto-updates are controlled by an environment variable and cannot be changed here.".to_string(),
                            wrap: TextWrap::Wrap,
                        )
                    }
                    .into_any(),
                    element! {
                        Text(
                            content: format!("Unset {env_var} to re-enable auto-updates."),
                            color: theme.inactive,
                            wrap: TextWrap::Wrap,
                        )
                    }
                    .into_any(),
                ],
                None => vec![
                    element! {
                        EnableAutoUpdatesSelect(on_change: move |channel: String| {
                            submenu.set(None);
                            tabs_hidden_request.set(Some(false));
                            let mut all = items.read().clone();
                            apply_settings_submenu_selection(
                                &mut all,
                                SettingsSubmenu::EnableAutoUpdates,
                                &channel,
                            );
                            items.set(all);
                        })
                    }
                    .into_any(),
                ],
            };
            element! {
                Dialog(
                    title: "Enable Auto-Updates".to_string(),
                    on_cancel: move |_| {
                        submenu.set(None);
                        tabs_hidden_request.set(Some(false));
                    },
                    hide_border: true,
                    hide_input_guide: true,
                ) {
                    #(body)
                }
            }
            .into_any()
        }
    } else {
        // CC :2118-2122 `<Box flexDirection="column" gap={1}
        // marginY={insideModal ? undefined : 1}>`.
        let margin_y = if inside_modal { 0u32 } else { 1u32 };
        element! {
            View(flex_direction: FlexDirection::Column, gap: 1, margin_top: margin_y, margin_bottom: margin_y) {
            // Maps to: CC Config.tsx:2123-2129 — `<SearchBox query isFocused
            // isTerminalFocused cursorOffset placeholder="Search settings…" />`,
            // the same component LogSelector and the plugin menus mount. A
            // direct child of the column, it stretches to Config's full width.
            crate::components::search_box::SearchBox(
                query: search_query.clone(),
                placeholder: Some(PLACEHOLDER.to_string()),
                is_focused: is_search_focused,
                is_terminal_focused: is_terminal_focused,
                cursor_offset: Some(search.offset()),
            )

            // CC :2130-2238 `<Box flexDirection="column">`: the empty-result
            // line, or the scroll hints around the visible rows.
            View(flex_direction: FlexDirection::Column) {
            // Maps to: CC "↑ N more above"
            #(if has_above {
                Some(element! {
                    Text(
                        content: format!("  ↑ {} more above", ofs),
                        color: theme.inactive,
                    )
                })
            } else {
                None
            })

            // Maps to official empty search result copy.
            #(if count == 0 {
                Some(element! {
                    Text(
                        content: format!("No settings match \"{}\"", search_query),
                        color: theme.inactive,
                        italic: true,
                    )
                })
            } else {
                None
            })

            // Visible items — slice(scrollOffset, scrollOffset + maxVisible)
            #(visible[ofs..end].iter().enumerate().map(|(vi, (_, item))| {
                let t = theme;
                let actual_idx = ofs + vi;
                // Maps to: CC line 2146-2149 — isSelected requires
                // !headerFocused && !isSearchMode.
                let is_sel = actual_idx == selected.get() && !header_focused && !is_search_mode.get();
                let row_color = if is_sel { Some(t.suggestion) } else { None };
                let pointer = if is_sel { POINTER } else { " " };
                let val_text = item_display_value(item);
                let val_color = row_color;
                let disabled_reason = (item.id == "autoUpdatesChannel" && val_text == "disabled")
                    .then(|| format!("({})", auto_updates_disabled_reason_label()));

                // Maps to: CC Config.tsx line 2153-2227
                let label_col = LABEL_WIDTH as usize - 2; // minus pointer prefix
                let padded = format!("{} {:<width$}", pointer, item.label, width = label_col);
                element! {
                    View(flex_direction: FlexDirection::Row) {
                        View(width: LABEL_WIDTH) {
                            Text(content: padded, color: row_color)
                        }
                        View(flex_direction: FlexDirection::Column) {
                            Text(content: val_text, color: val_color)
                            #(disabled_reason.map(|reason| element! {
                                Text(content: reason, color: t.inactive)
                            }))
                        }
                    }
                }
            }))

            // Maps to: CC "↓ N more below"
            #(if has_below {
                Some(element! {
                    Text(
                        content: format!("  ↓ {} more below", count - end),
                        color: theme.inactive,
                    )
                })
            } else {
                None
            })
            }

            // Maps to official Config footer by focus mode (CC :2239-2294).
            Text(
                content: if header_focused {
                    "←/→ tab switch · ↓ return · Esc close"
                } else if is_search_mode.get() {
                    "Type to filter · Enter/↓ select · ↑ tabs · Esc clear"
                } else {
                    "Space change · Enter save · / search · Esc cancel"
                },
                color: theme.inactive,
            )
        }
        }
        .into_any()
    };

    // CC :1789-1795: the root Box is `width="100%"`. CC's Tab is a row Box
    // measured at most the pane's content width, and CC's yoga resolves the
    // percentage against that available width (native-ts/yoga-layout
    // index.ts:1220 `let width = availableWidth`, :1352-1356 `ownerW`), so
    // Config — and the search box stretched across it — spans the pane,
    // where the permission tabs, with no percentage, follow their content.
    // The port's Settings tab container is a column, which stretches Config
    // anyway; the root keeps the width wherever the parent is a row or
    // shrinks to its content.
    element! {
        View(flex_direction: FlexDirection::Column, width: 100pct) {
            #(content)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::config::GlobalConfig;
    use crate::utils::settings::types::SettingsJson;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK, TestEnvLock};
    use crate::utils::theme::{self, ThemeName};
    use futures::{StreamExt, stream};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{Duration, SystemTime};

    #[test]
    fn config_change_summary_uses_official_save_close_shape_with_readonly_note() {
        let initial = items::default_items();
        let mut current = initial.clone();
        let auto_compact = current
            .iter_mut()
            .find(|item| item.id == "autoCompactEnabled")
            .expect("default config should include auto-compact");
        auto_compact.toggle();

        let summary = format_config_change_summary(&initial, &current)
            .expect("changed setting should produce a close summary");

        assert!(summary.contains("Disabled auto-compact"));
        assert!(summary.contains("UI-only preview; settings were not written"));
    }

    #[test]
    fn config_change_summary_uses_current_official_setting_ids() {
        let initial = items::default_items();
        let mut current = initial.clone();
        for id in ["terminalProgressBarEnabled", "showStatusInTerminalTab"] {
            let item = current
                .iter_mut()
                .find(|item| item.id == id)
                .expect("default config should include summary target");
            item.visible = true;
            item.toggle();
        }

        let summary = format_config_change_summary(&initial, &current)
            .expect("changed settings should produce a close summary");

        assert!(summary.contains("Enabled terminal progress bar"));
        assert!(summary.contains("Enabled terminal tab status"));
        assert!(!summary.contains("terminalProgressBarEnabled"));
        assert!(!summary.contains("showStatusInTerminalTab"));
    }

    #[test]
    fn config_default_permission_mode_uses_official_display_titles() {
        let mut item = items::default_items()
            .into_iter()
            .find(|item| item.id == "defaultPermissionMode")
            .expect("default permission mode row should exist");

        assert_eq!(item_display_value(&item), "Default");
        item.toggle();
        assert_eq!(item_display_value(&item), "Plan Mode");
        item.toggle();
        assert_eq!(item_display_value(&item), "Accept edits");
        item.toggle();
        assert_eq!(item_display_value(&item), "Don't Ask");
    }

    #[component]
    fn ConfigHarness(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        use crate::components::design_system::theme_provider::{ThemeProvider, ThemeSaveHandler};
        let keybinding_runtime =
            crate::keybindings::keybinding_provider_setup::use_keybinding_setup(
                &mut hooks,
                crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings(),
            );
        element! {
            ContextProvider(value: Context::owned(keybinding_runtime)) {

                // Config always renders under the root ThemeProvider, whose
                // preview the Theme submenu drives.
                ThemeProvider(
                    initial_state: Some(ThemeSetting::Named(ThemeName::Dark)),
                    on_theme_save: Some(std::sync::Arc::new(|_| {}) as ThemeSaveHandler),
                ) {
                    crate::state::app_state::AppStateProvider(
                        children: crate::state::app_state::ProviderChildren::new(|| element! {
                            Config(max_visible: Some(12u32), header_focused: false)
                        }.into_any()),
                    )
                }
            }
        }
    }

    #[component]
    fn ConfigRemappedAcceptHarness(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let current_theme = *theme::current();
        let mut bindings = crate::keybindings::default_bindings::default_bindings();
        bindings.push(crate::keybindings::types::ParsedBinding {
            chord: crate::keybindings::parser::parse_chord("space"),
            action: None,
            context: crate::keybindings::types::ContextName::Settings,
        });
        bindings.push(crate::keybindings::types::ParsedBinding {
            chord: crate::keybindings::parser::parse_chord("f4"),
            action: Some("select:accept".to_string()),
            context: crate::keybindings::types::ContextName::Settings,
        });
        let keybinding_runtime =
            crate::keybindings::keybinding_provider_setup::use_keybinding_setup(
                &mut hooks,
                crate::keybindings::keybinding_context::KeybindingRuntime::new(bindings),
            );
        element! {
            ContextProvider(value: Context::owned(keybinding_runtime)) {

                ContextProvider(value: Context::owned(current_theme)) {
                    crate::state::app_state::AppStateProvider(
                        children: crate::state::app_state::ProviderChildren::new(|| element! {
                            Config(max_visible: Some(12u32), header_focused: false)
                        }.into_any()),
                    )
                }
            }
        }
    }

    #[component]
    fn ConfigRuntimeDisplayHarness(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let current_theme = *theme::current();
        let keybinding_runtime =
            crate::keybindings::keybinding_provider_setup::use_keybinding_setup(
                &mut hooks,
                crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings(),
            );
        let test_store = hooks.use_const(|| {
            let mut settings = SettingsJson::default();
            settings.prefers_reduced_motion = Some(true);
            let mut initial = crate::state::app_state_store::AppState::default();
            initial.settings = Arc::new(settings);
            crate::state::store::AppStore::new(initial, None)
        });

        element! {
            ContextProvider(value: Context::owned(keybinding_runtime)) {

                ContextProvider(value: Context::owned(current_theme)) {
                    crate::state::app_state::AppStateProvider(
                        prebuilt_store: Some(test_store.clone()),
                        children: crate::state::app_state::ProviderChildren::new(|| element! {
                            Config(max_visible: Some(12u32), header_focused: false)
                        }.into_any()),
                    )
                }
            }
        }
    }

    #[component]
    fn ConfigRuntimeDisplaySpinnerHarness(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let current_theme = *theme::current();
        let keybinding_runtime =
            crate::keybindings::keybinding_provider_setup::use_keybinding_setup(
                &mut hooks,
                crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings(),
            );
        let test_store = hooks.use_const(|| {
            let mut settings = SettingsJson::default();
            settings.prefers_reduced_motion = Some(false);
            let mut initial = crate::state::app_state_store::AppState::default();
            initial.settings = Arc::new(settings);
            crate::state::store::AppStore::new(initial, None)
        });

        element! {
            ContextProvider(value: Context::owned(keybinding_runtime)) {

                ContextProvider(value: Context::owned(current_theme)) {
                    crate::state::app_state::AppStateProvider(
                        prebuilt_store: Some(test_store.clone()),
                        children: crate::state::app_state::ProviderChildren::new(|| element! {
                            View(flex_direction: FlexDirection::Column) {
                                Config(max_visible: Some(4u32), header_focused: false)
                                crate::components::spinner::SpinnerWithVerb(message: "Thinking".to_string())
                            }
                        }.into_any()),
                    )
                }
            }
        }
    }

    #[component]
    fn ConfigPromptFooterHarness(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let current_theme = *theme::current();
        let keybinding_runtime =
            crate::keybindings::keybinding_provider_setup::use_keybinding_setup(
                &mut hooks,
                crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings(),
            );
        let test_store = hooks.use_const(|| {
            let mut initial = crate::state::app_state_store::AppState::default();
            initial.verbose = false;
            crate::state::store::AppStore::new(initial, None)
        });
        // CC: Notifications derives tokenUsage from its messages prop
        // (Notifications.tsx:80-83), not from AppState.
        let messages = hooks.use_const(|| {
            Arc::new(vec![crate::types::message::Message::Assistant(
                crate::types::message::AssistantMessage {
                    uuid: uuid::Uuid::new_v4().to_string(),
                    timestamp: chrono::Utc::now(),
                    content: vec![crate::types::message::AssistantContent::Text("hi".into())],
                    model: Some("claude".into()),
                    stop_reason: None,
                    usage: Some(crate::types::message::TokenUsage {
                        input_tokens: 40,
                        output_tokens: 2,
                        ..Default::default()
                    }),
                },
            )])
        });

        element! {
            ContextProvider(value: Context::owned(keybinding_runtime)) {

                ContextProvider(value: Context::owned(current_theme)) {
                    crate::state::app_state::AppStateProvider(
                        prebuilt_store: Some(test_store.clone()),
                        children: crate::state::app_state::ProviderChildren::new(move || element! {
                            View(flex_direction: FlexDirection::Column) {
                                Config(max_visible: Some(8u32), header_focused: false)
                                crate::components::prompt_input::notifications::Notifications(
                                    messages: messages.clone(),
                                    suppressed: false,
                                    inline: true,
                                )
                            }
                        }.into_any()),
                    )
                }
            }
        }
    }

    #[component]
    fn ConfigSettingsOverrideHarness(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let current_theme = *theme::current();
        let keybinding_runtime =
            crate::keybindings::keybinding_provider_setup::use_keybinding_setup(
                &mut hooks,
                crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings(),
            );
        // Maps to: CC TEST_GLOBAL_CONFIG_FOR_TESTING injection under
        // NODE_ENV=test — getGlobalConfig()-equivalent reads see this override.
        let mut global_config = GlobalConfig::default();
        global_config.auto_compact_enabled = Some(false);
        crate::utils::config::set_test_global_config(Some(global_config));
        let mut settings = SettingsJson::default();
        settings.spinner_tips_enabled = Some(false);
        settings.prefers_reduced_motion = Some(true);
        let test_store = hooks.use_const(move || {
            let mut initial = crate::state::app_state_store::AppState::default();
            initial.settings = Arc::new(settings);
            crate::state::store::AppStore::new(initial, None)
        });

        element! {
            ContextProvider(value: Context::owned(keybinding_runtime)) {

                ContextProvider(value: Context::owned(current_theme)) {
                    crate::state::app_state::AppStateProvider(
                        prebuilt_store: Some(test_store.clone()),
                        children: crate::state::app_state::ProviderChildren::new(|| element! {
                            Config(max_visible: Some(12u32), header_focused: false)
                        }.into_any()),
                    )
                }
            }
        }
    }

    /// Counts key presses (a bypass listener, so consumption does not hide
    /// one) so each key produces a frame for `drive_harness`.
    #[component]
    fn KeyCountEcho(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut keys = hooks.use_state(|| 0usize);
        hooks.use_terminal_events(move |event| {
            if matches!(event, TerminalEvent::Key(key) if key.kind == KeyEventKind::Press) {
                keys.set(keys.get() + 1);
            }
        });
        // Text after the count: `canvas_lines` trims trailing spaces, and
        // `drive_harness` matches `keys=<count> `.
        element! { Text(content: format!("keys={} pressed", keys.get())) }
    }

    #[derive(Default, Props)]
    struct ConfigAutoUpdatesDisabledHarnessProps {
        /// Adds a `KeyCountEcho` below Config.
        echo: bool,
    }

    #[component]
    fn ConfigAutoUpdatesDisabledHarness(
        props: &ConfigAutoUpdatesDisabledHarnessProps,
        mut hooks: Hooks,
    ) -> impl Into<AnyElement<'static>> {
        let current_theme = *theme::current();
        let keybinding_runtime =
            crate::keybindings::keybinding_provider_setup::use_keybinding_setup(
                &mut hooks,
                crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings(),
            );
        let mut global_config = GlobalConfig::default();
        global_config.auto_updates = Some(false);
        crate::utils::config::set_test_global_config(Some(global_config));
        let settings = SettingsJson::default();
        let test_store = hooks.use_const(move || {
            let mut initial = crate::state::app_state_store::AppState::default();
            initial.settings = Arc::new(settings);
            crate::state::store::AppStore::new(initial, None)
        });
        let echo = props.echo;

        element! {
            ContextProvider(value: Context::owned(keybinding_runtime)) {

                ContextProvider(value: Context::owned(current_theme)) {
                    crate::state::app_state::AppStateProvider(
                        prebuilt_store: Some(test_store.clone()),
                        children: crate::state::app_state::ProviderChildren::new(move || element! {
                            View(flex_direction: FlexDirection::Column) {
                                Config(max_visible: Some(12u32), header_focused: false)
                                #(echo.then(|| element! { KeyCountEcho }))
                            }
                        }.into_any()),
                    )
                }
            }
        }
    }

    fn canvas_lines(canvas: &Canvas) -> Vec<String> {
        (0..canvas.height())
            .map(|y| {
                let mut line = String::new();
                for x in 0..canvas.width() {
                    if let Some(text) = canvas.cell(x, y).and_then(|cell| cell.text()) {
                        line.push_str(text);
                    } else {
                        line.push(' ');
                    }
                }
                line.trim_end().to_string()
            })
            .collect()
    }

    fn find_text(canvas: &Canvas, needle: &str) -> Option<(usize, usize)> {
        canvas_lines(canvas)
            .into_iter()
            .enumerate()
            .find_map(|(y, line)| line.find(needle).map(|x| (x, y)))
    }

    fn render_with_config(config: MockTerminalConfig) -> Vec<Canvas> {
        futures::executor::block_on(async {
            let mut app = element!(ConfigHarness);
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(config));
            let mut canvases = Vec::new();
            loop {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(100)).await;
                    None
                })
                .await;
                let Some(canvas) = next else {
                    break;
                };
                canvases.push(canvas);
                if canvases.len() >= 20 {
                    break;
                }
            }
            canvases
        })
    }

    fn render_with_runtime_spinner(config: MockTerminalConfig) -> Vec<Canvas> {
        futures::executor::block_on(async {
            let mut app = element!(ConfigRuntimeDisplaySpinnerHarness);
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(config));
            let mut canvases = Vec::new();
            loop {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(100)).await;
                    None
                })
                .await;
                let Some(canvas) = next else {
                    break;
                };
                canvases.push(canvas);
                if canvases.len() >= 20 {
                    break;
                }
            }
            canvases
        })
    }

    fn render_with_prompt_footer(config: MockTerminalConfig) -> Vec<Canvas> {
        futures::executor::block_on(async {
            let mut app = element!(ConfigPromptFooterHarness);
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(config));
            let mut canvases = Vec::new();
            loop {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(100)).await;
                    None
                })
                .await;
                let Some(canvas) = next else {
                    break;
                };
                canvases.push(canvas);
                if canvases.len() >= 20 {
                    break;
                }
            }
            canvases
        })
    }

    fn render_with_settings_override(config: MockTerminalConfig) -> Vec<Canvas> {
        futures::executor::block_on(async {
            let mut app = element!(ConfigSettingsOverrideHarness);
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(config));
            let mut canvases = Vec::new();
            loop {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(100)).await;
                    None
                })
                .await;
                let Some(canvas) = next else {
                    break;
                };
                canvases.push(canvas);
                if canvases.len() >= 20 {
                    break;
                }
            }
            canvases
        })
    }

    fn render_with_auto_updates_disabled_config(config: MockTerminalConfig) -> Vec<Canvas> {
        futures::executor::block_on(async {
            let mut app = element!(ConfigAutoUpdatesDisabledHarness);
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(config));
            let mut canvases = Vec::new();
            loop {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(100)).await;
                    None
                })
                .await;
                let Some(canvas) = next else {
                    break;
                };
                canvases.push(canvas);
                if canvases.len() >= 20 {
                    break;
                }
            }
            canvases
        })
    }

    fn press(code: KeyCode) -> TerminalEvent {
        TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code))
    }

    fn ctrl_char(ch: char) -> TerminalEvent {
        let mut event = KeyEvent::new(KeyEventKind::Press, KeyCode::Char(ch));
        event.modifiers = KeyModifiers::CONTROL;
        TerminalEvent::Key(event)
    }

    fn text_events(text: &str) -> Vec<TerminalEvent> {
        text.chars().map(|ch| press(KeyCode::Char(ch))).collect()
    }

    fn terminal_timed_events(
        events: Vec<TerminalEvent>,
    ) -> impl futures::Stream<Item = TerminalEvent> {
        stream::unfold(events.into_iter(), |mut events| async move {
            let event = events.next()?;
            futures_timer::Delay::new(Duration::from_millis(1)).await;
            Some((event, events))
        })
    }

    fn render_text_with_events(events: Vec<TerminalEvent>) -> String {
        // Keybinding actions and raw search/picker input are separate React-
        // style consumers. Deliver one event per update, matching a terminal,
        // rather than draining a synthetic stream before retained state can
        // commit between events.
        let events = terminal_timed_events(events);
        let canvases =
            render_with_config(MockTerminalConfig::with_events(events).with_size(100, 24));
        canvas_lines(
            canvases
                .last()
                .expect("mock render should produce a canvas"),
        )
        .join("\n")
    }

    fn render_remapped_accept_text(events: Vec<TerminalEvent>) -> String {
        futures::executor::block_on(async {
            let mut app = element!(ConfigRemappedAcceptHarness);
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(terminal_timed_events(events)).with_size(100, 24),
            ));
            let mut last = None;
            for _ in 0..20 {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(100)).await;
                    None
                })
                .await;
                let Some(canvas) = next else {
                    break;
                };
                last = Some(canvas);
            }
            canvas_lines(&last.expect("mock render should produce a canvas")).join("\n")
        })
    }

    fn render_text_with_timed_events(events: Vec<TerminalEvent>) -> String {
        let events = stream::unfold(events.into_iter(), |mut events| async move {
            let event = events.next()?;
            futures_timer::Delay::new(Duration::from_millis(10)).await;
            Some((event, events))
        });
        let canvases =
            render_with_config(MockTerminalConfig::with_events(events).with_size(100, 24));
        canvas_lines(
            canvases
                .last()
                .expect("mock render should produce a canvas"),
        )
        .join("\n")
    }

    fn render_auto_updates_disabled_text_with_events(events: Vec<TerminalEvent>) -> String {
        let canvases = render_with_auto_updates_disabled_config(
            MockTerminalConfig::with_events(stream::iter(events)).with_size(100, 24),
        );
        canvas_lines(
            canvases
                .last()
                .expect("mock render should produce a canvas"),
        )
        .join("\n")
    }

    fn env_lock() -> &'static TestEnvLock {
        // Share the crate-wide env test lock: settings/items tests mutate the
        // same auto-update env variables and must serialize across modules.
        &TEST_ENV_LOCK
    }

    struct CurrentDirGuard {
        previous: PathBuf,
    }

    impl CurrentDirGuard {
        fn set(path: &PathBuf) -> Self {
            let previous = std::env::current_dir().unwrap();
            std::env::set_current_dir(path).unwrap();
            Self { previous }
        }
    }

    impl Drop for CurrentDirGuard {
        fn drop(&mut self) {
            std::env::set_current_dir(&self.previous).unwrap();
        }
    }

    #[derive(Debug)]
    struct PathSnapshot {
        path: PathBuf,
        existed: bool,
        len: Option<u64>,
        modified: Option<SystemTime>,
    }

    impl PathSnapshot {
        fn capture(path: PathBuf) -> Self {
            let metadata = fs::metadata(&path).ok();
            Self {
                path,
                existed: metadata.is_some(),
                len: metadata.as_ref().map(|metadata| metadata.len()),
                modified: metadata.and_then(|metadata| metadata.modified().ok()),
            }
        }

        fn assert_unchanged(&self) {
            let metadata = fs::metadata(&self.path).ok();
            if !self.existed {
                if metadata.is_some() {
                    if self.path.is_dir() {
                        let _ = fs::remove_dir_all(&self.path);
                    } else {
                        let _ = fs::remove_file(&self.path);
                    }
                    panic!("Settings preview should not create {}", self.path.display());
                }
                return;
            }

            let metadata = metadata.unwrap_or_else(|| {
                panic!(
                    "Settings preview should not remove existing {}",
                    self.path.display()
                )
            });
            assert_eq!(
                metadata.len(),
                self.len
                    .expect("existing path should have a length snapshot"),
                "Settings preview should not rewrite {}",
                self.path.display()
            );
            if let Some(modified) = self.modified {
                assert_eq!(
                    metadata.modified().ok(),
                    Some(modified),
                    "Settings preview should not touch {}",
                    self.path.display()
                );
            }
        }
    }

    #[test]
    fn config_unselected_values_use_default_color_like_official() {
        let canvases = render_with_config(MockTerminalConfig::default().with_size(100, 24));
        let canvas = canvases
            .last()
            .expect("mock render should produce a canvas");
        let (label_x, label_y) = find_text(canvas, "Auto-compact")
            .expect("default settings should render Auto-compact row");
        let (value_x, value_y) =
            find_text(canvas, "true").expect("Auto-compact true value should render");

        assert_eq!(
            canvas
                .cell(label_x, label_y)
                .and_then(|cell| cell.text_style())
                .and_then(|style| style.color),
            None,
            "official Config leaves unselected labels at terminal default fg"
        );
        assert_eq!(
            canvas
                .cell(value_x, value_y)
                .and_then(|cell| cell.text_style())
                .and_then(|style| style.color),
            None,
            "official Config does not color true boolean values green when unselected"
        );
    }

    #[test]
    fn config_auto_updates_disabled_row_uses_readonly_submenu() {
        let _lock = env_lock().lock().expect("env tests should serialize");
        let _disable_guard = EnvVarGuard::unset("DISABLE_AUTOUPDATER");
        let _claude_disable_guard = EnvVarGuard::unset("CLAUDE_CODE_DISABLE_AUTOUPDATER");
        let _traffic_guard = EnvVarGuard::unset("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC");
        let item = SettingItem {
            id: "autoUpdatesChannel",
            label: "Auto-update channel",
            value: SettingValue::Display("disabled".to_string()),
            search_text: "auto update channel version",
            visible: true,
        };

        assert_eq!(
            submenu_for_item(&item),
            Some(SettingsSubmenu::EnableAutoUpdates)
        );
        let options = enable_auto_updates_options();
        assert_eq!(
            options
                .iter()
                .map(|option| option.label.as_str())
                .collect::<Vec<_>>(),
            vec!["Enable with latest channel", "Enable with stable channel"]
        );
        assert_eq!(
            options
                .iter()
                .map(|option| option.value.as_str())
                .collect::<Vec<_>>(),
            vec!["latest", "stable"]
        );
        assert!(
            options.iter().all(|option| option.description.is_none()),
            "official EnableAutoUpdates Select options do not render extra descriptions"
        );
    }

    #[test]
    fn config_auto_updates_disabled_submenu_matches_official_dialog_shape() {
        let _lock = env_lock().lock().expect("env tests should serialize");
        let _disable_guard = EnvVarGuard::unset("DISABLE_AUTOUPDATER");
        let _claude_disable_guard = EnvVarGuard::unset("CLAUDE_CODE_DISABLE_AUTOUPDATER");
        let _traffic_guard = EnvVarGuard::unset("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC");
        let mut events = text_events("version");
        events.push(press(KeyCode::Enter));
        events.push(press(KeyCode::Char(' ')));
        let text = render_auto_updates_disabled_text_with_events(events);

        assert!(text.contains("Enable Auto-Updates"), "canvas=\n{text}");
        assert!(
            text.contains("Enable with latest channel"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("Enable with stable channel"),
            "canvas=\n{text}"
        );
        assert!(
            !text.contains("Choose an auto-update channel preview"),
            "official Dialog does not render extra Cometix guidance; canvas=\n{text}"
        );
        assert!(
            !text.contains("Preview enabling auto-updates"),
            "official Select rows do not render preview descriptions; canvas=\n{text}"
        );
        assert!(
            !text.contains("Enter preview") && !text.contains("Esc cancel"),
            "official Dialog hideInputGuide omits a separate footer; canvas=\n{text}"
        );
    }

    /// Search "version", leave the search box and open the auto-update
    /// channel row's submenu.
    fn open_auto_updates_submenu() -> Vec<TerminalEvent> {
        let mut open = text_events("version");
        open.push(press(KeyCode::Enter));
        open.push(press(KeyCode::Char(' ')));
        open
    }

    /// `ConfigAutoUpdatesDisabledHarness` driven one key per frame; returns
    /// the text of the frame the last key produced.
    fn drive_auto_updates_disabled_keys_text(events: Vec<TerminalEvent>) -> String {
        let canvases = drive_harness(
            element!(ConfigAutoUpdatesDisabledHarness(echo: true)).into_any(),
            events.into_iter().map(|event| vec![event]).collect(),
        );
        canvas_lines(canvases.last().unwrap()).join("\n")
    }

    #[test]
    fn config_auto_updates_disabled_by_env_stays_message_only() {
        // CC Config.tsx:2025-2038: an env reason shows its message, no Select.
        let _lock = env_lock().lock().expect("env tests should serialize");
        let _disable_guard = EnvVarGuard::unset("CLAUDE_CODE_DISABLE_AUTOUPDATER");
        let _traffic_guard = EnvVarGuard::unset("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC");
        let _env_guard = EnvVarGuard::set("DISABLE_AUTOUPDATER", "1");

        let text = drive_auto_updates_disabled_keys_text(open_auto_updates_submenu());
        assert!(text.contains("Enable Auto-Updates"), "canvas=\n{text}");
        assert!(
            text.contains(
                "Auto-updates are controlled by an environment variable and cannot be changed here."
            ),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("Unset DISABLE_AUTOUPDATER to re-enable auto-updates."),
            "canvas=\n{text}"
        );
        assert!(!text.contains("Enable with latest channel"), "canvas=\n{text}");
    }

    #[test]
    fn config_enable_auto_updates_select_sets_the_channel_and_closes() {
        // CC Config.tsx:2051-2075: the chosen channel becomes the row value.
        let _lock = env_lock().lock().expect("env tests should serialize");
        let _disable_guard = EnvVarGuard::unset("DISABLE_AUTOUPDATER");
        let _claude_disable_guard = EnvVarGuard::unset("CLAUDE_CODE_DISABLE_AUTOUPDATER");
        let _traffic_guard = EnvVarGuard::unset("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC");
        // F12 is a key nothing binds: its frame shows the settled choice.
        let mut keys = open_auto_updates_submenu();
        keys.push(press(KeyCode::Down));
        keys.push(press(KeyCode::Enter));
        keys.push(press(KeyCode::F(12)));

        let text = drive_auto_updates_disabled_keys_text(keys);
        let row = text
            .lines()
            .find(|line| line.contains("Auto-update channel"))
            .unwrap_or_else(|| panic!("the list should be back; canvas=\n{text}"));
        assert!(row.contains("stable"), "canvas=\n{text}");
        assert!(!text.contains("Enable Auto-Updates"), "canvas=\n{text}");
    }

    #[test]
    fn config_enable_auto_updates_escape_and_n_close_the_dialog() {
        // CC Config.tsx:2016-2021 `<Dialog onCancel>`: confirm:no is Esc and
        // n; the row keeps its value.
        let _lock = env_lock().lock().expect("env tests should serialize");
        let _disable_guard = EnvVarGuard::unset("DISABLE_AUTOUPDATER");
        let _claude_disable_guard = EnvVarGuard::unset("CLAUDE_CODE_DISABLE_AUTOUPDATER");
        let _traffic_guard = EnvVarGuard::unset("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC");
        // F12 is a key nothing binds: its frame shows the settled cancel.
        for key in [KeyCode::Esc, KeyCode::Char('n')] {
            let mut keys = open_auto_updates_submenu();
            keys.push(press(key));
            keys.push(press(KeyCode::F(12)));
            let text = drive_auto_updates_disabled_keys_text(keys);
            let row = text
                .lines()
                .find(|line| line.contains("Auto-update channel"))
                .unwrap_or_else(|| panic!("{key:?}: the list should be back; canvas=\n{text}"));
            assert!(row.contains("disabled"), "{key:?}: canvas=\n{text}");
            assert!(!text.contains("Enable Auto-Updates"), "{key:?}: canvas=\n{text}");
        }
    }

    #[test]
    fn config_auto_updates_disabled_row_renders_official_reason_line() {
        let _lock = env_lock().lock().expect("env tests should serialize");
        let _disable_guard = EnvVarGuard::unset("DISABLE_AUTOUPDATER");
        let _claude_disable_guard = EnvVarGuard::unset("CLAUDE_CODE_DISABLE_AUTOUPDATER");
        let _traffic_guard = EnvVarGuard::unset("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC");
        let mut events = text_events("version");
        events.push(press(KeyCode::Enter));
        let text = render_auto_updates_disabled_text_with_events(events);

        assert!(
            text.contains("Auto-update channel") && text.contains("disabled"),
            "disabled auto-update row should remain visible; canvas=\n{text}"
        );
        assert!(
            text.contains("(config)"),
            "official Config renders disabled reason on a dim second line; canvas=\n{text}"
        );
    }

    #[test]
    fn config_auto_updates_latest_opens_official_channel_downgrade_dialog() {
        let _guard = env_lock().lock().expect("env lock should not be poisoned");
        let _guards = latest_channel_env_guards();
        let mut events = text_events("version");
        events.push(press(KeyCode::Enter));
        events.push(press(KeyCode::Char(' ')));
        let text = render_text_with_events(events);

        assert!(
            text.contains("Switch to Stable Channel"),
            "latest channel should open the official downgrade dialog; canvas=\n{text}"
        );
        assert!(
            text.contains(&format!("currently running ({})", product::VERSION)),
            "downgrade dialog should include the current version; canvas=\n{text}"
        );
        assert!(
            text.contains("Allow possible downgrade to stable version"),
            "downgrade dialog should include the official downgrade choice; canvas=\n{text}"
        );
        assert!(
            text.contains(&format!(
                "Stay on current version ({}) until stable catches up",
                product::VERSION
            )),
            "downgrade dialog should include the official stay choice; canvas=\n{text}"
        );
        assert!(
            !text.contains("Enter select · Esc cancel"),
            "official ChannelDowngradeDialog hideInputGuide omits a separate footer; canvas=\n{text}"
        );
    }

    /// Clears every variable that disables auto-updates
    /// (`items::auto_updates_are_disabled`), so the row reads the default
    /// `latest` channel whatever the host exports.
    fn latest_channel_env_guards() -> [EnvVarGuard; 3] {
        [
            EnvVarGuard::unset("DISABLE_AUTOUPDATER"),
            EnvVarGuard::unset("CLAUDE_CODE_DISABLE_AUTOUPDATER"),
            EnvVarGuard::unset("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC"),
        ]
    }

    /// Opens the latest channel's ChannelDowngrade dialog and sends `keys`,
    /// one key per frame; returns the Auto-update channel row, or panics when
    /// the list is not back.
    fn channel_downgrade_row_after(keys: Vec<TerminalEvent>) -> String {
        let mut events = text_events("version");
        events.push(press(KeyCode::Enter));
        events.push(press(KeyCode::Char(' ')));
        events.extend(keys);
        let text = drive_config_keys_text(events);
        assert!(!text.contains("Switch to Stable Channel"), "canvas=\n{text}");
        text.lines()
            .find(|line| line.contains("Auto-update channel"))
            .unwrap_or_else(|| panic!("the list should be back; canvas=\n{text}"))
            .to_string()
    }

    #[test]
    fn config_channel_downgrade_choice_moves_the_row_to_stable() {
        // CC Config.tsx:2082-2109: `downgrade` or `stay` switch to stable.
        let _guard = env_lock().lock().expect("env lock should not be poisoned");
        let _guards = latest_channel_env_guards();
        // F12 is a key nothing binds: its frame shows the settled choice.
        for choice in [Vec::new(), vec![press(KeyCode::Down)]] {
            let mut keys = choice;
            keys.push(press(KeyCode::Enter));
            keys.push(press(KeyCode::F(12)));
            let row = channel_downgrade_row_after(keys);
            assert!(row.contains("stable"), "{row}");
        }
    }

    #[test]
    fn config_channel_downgrade_escape_and_n_keep_the_channel() {
        // CC ChannelDowngradeDialog.tsx:25-27 cancels through the Dialog's
        // confirm:no (Esc and n); Config.tsx:2086-2089 then changes nothing.
        // F12 is a key nothing binds: its frame shows the settled cancel.
        let _guard = env_lock().lock().expect("env lock should not be poisoned");
        let _guards = latest_channel_env_guards();
        for key in [KeyCode::Esc, KeyCode::Char('n')] {
            let row = channel_downgrade_row_after(vec![press(key), press(KeyCode::F(12))]);
            assert!(row.contains("latest"), "{key:?}: {row}");
        }
    }

    #[test]
    fn config_auto_updates_stable_switches_to_latest_without_picker() {
        let item = SettingItem {
            id: "autoUpdatesChannel",
            label: "Auto-update channel",
            value: SettingValue::Display("stable".to_string()),
            search_text: "auto update channel version",
            visible: true,
        };

        assert_eq!(
            auto_updates_action_for_item(&item),
            Some(AutoUpdatesAction::SetLatest),
            "official Config switches stable back to latest directly"
        );
        assert_eq!(
            submenu_for_item(&item),
            None,
            "stable channel should not open the old generic auto-update picker"
        );

        let mut items = vec![item];
        assert!(set_auto_updates_channel_latest(&mut items, 0));
        assert_eq!(items[0].display_value(), "latest");
    }

    #[test]
    fn config_selected_row_uses_suggestion_color_for_label_and_value() {
        let events = stream::iter([press(KeyCode::Enter)]);
        let canvases =
            render_with_config(MockTerminalConfig::with_events(events).with_size(100, 24));
        let canvas = canvases
            .last()
            .expect("mock render should produce a canvas");
        let expected = theme::current().suggestion;
        let (label_x, label_y) = find_text(canvas, "Auto-compact")
            .expect("selected settings should render Auto-compact row");
        let (value_x, value_y) =
            find_text(canvas, "true").expect("selected true value should render");

        assert_eq!(
            canvas
                .cell(label_x, label_y)
                .and_then(|cell| cell.text_style())
                .and_then(|style| style.color),
            Some(expected)
        );
        assert_eq!(
            canvas
                .cell(value_x, value_y)
                .and_then(|cell| cell.text_style())
                .and_then(|style| style.color),
            Some(expected)
        );
    }

    #[test]
    fn config_footer_matches_official_search_and_list_modes() {
        let search_text = render_text_with_events(Vec::new());
        assert!(
            search_text.contains("Type to filter · Enter/↓ select · ↑ tabs · Esc clear"),
            "Config search footer should match official key ownership; canvas=\n{search_text}"
        );

        let list_text = render_text_with_events(vec![press(KeyCode::Enter)]);
        assert!(
            list_text.contains("Space change · Enter save · / search · Esc cancel"),
            "Config list footer should advertise Space change and Enter save; canvas=\n{list_text}"
        );
    }

    #[test]
    fn config_slash_key_enters_search_mode_from_list_like_official_settings_search() {
        let text = render_text_with_timed_events(vec![
            press(KeyCode::Enter),
            press(KeyCode::Down),
            press(KeyCode::Char('/')),
        ]);

        assert!(
            text.contains("Search settings"),
            "Slash from list mode should focus the empty search box; canvas=\n{text}"
        );
        assert!(
            text.contains("Type to filter · Enter/↓ select · ↑ tabs · Esc clear"),
            "Slash should restore Config search footer instead of doing nothing; canvas=\n{text}"
        );
    }

    #[test]
    fn config_select_accept_honors_null_unbind_and_live_remap() {
        let unbound =
            render_remapped_accept_text(vec![press(KeyCode::Enter), press(KeyCode::Char(' '))]);
        let remapped =
            render_remapped_accept_text(vec![press(KeyCode::Enter), press(KeyCode::F(4))]);

        assert!(
            unbound
                .lines()
                .any(|line| line.contains("Auto-compact") && line.contains("true")),
            "null-unbound Space must not toggle the row; canvas=\n{unbound}"
        );
        assert!(
            remapped
                .lines()
                .any(|line| line.contains("Auto-compact") && line.contains("false")),
            "remapped F4 must execute select:accept; canvas=\n{remapped}"
        );
    }

    #[test]
    fn config_left_and_right_keys_accept_like_official_toggle_setting() {
        let mut left_events = text_events("editor");
        left_events.push(press(KeyCode::Enter));
        left_events.push(press(KeyCode::Left));
        let left_text = render_text_with_events(left_events);
        assert!(
            left_text.contains("Editor mode") && left_text.contains("vim"),
            "Left should accept/cycle the selected enum like official toggleSetting; canvas=\n{left_text}"
        );

        let mut right_events = text_events("editor");
        right_events.push(press(KeyCode::Enter));
        right_events.push(press(KeyCode::Right));
        let right_text = render_text_with_events(right_events);
        assert!(
            right_text.contains("Editor mode") && right_text.contains("vim"),
            "Right should accept/cycle the selected enum like official toggleSetting; canvas=\n{right_text}"
        );
    }

    #[test]
    fn config_reduce_motion_row_reads_runtime_display_context() {
        let canvas = element!(ConfigRuntimeDisplayHarness).render(Some(100));
        let row = canvas_lines(&canvas)
            .into_iter()
            .find(|line| line.contains("Reduce motion"))
            .expect("runtime Settings view should render Reduce motion row");

        assert!(
            row.contains("true"),
            "Reduce motion row should reflect the runtime display context; row={row:?}"
        );
    }

    #[test]
    fn config_reads_injected_config_and_settings_without_writes() {
        let canvases =
            render_with_settings_override(MockTerminalConfig::default().with_size(100, 24));
        let text = canvas_lines(
            canvases
                .last()
                .expect("mock render should produce a canvas"),
        )
        .join("\n");

        let auto_compact_row = text
            .lines()
            .find(|line| line.contains("Auto-compact"))
            .expect("Config should render Auto-compact from runtime config");
        let show_tips_row = text
            .lines()
            .find(|line| line.contains("Show tips"))
            .expect("Config should render Show tips from runtime settings");
        let reduce_motion_row = text
            .lines()
            .find(|line| line.contains("Reduce motion"))
            .expect("Config should render Reduce motion from runtime settings");

        assert!(
            auto_compact_row.contains("false"),
            "Global config value should populate Config row; canvas=\n{text}"
        );
        assert!(
            show_tips_row.contains("false"),
            "Merged settings value should populate Config row; canvas=\n{text}"
        );
        assert!(
            reduce_motion_row.contains("true"),
            "Runtime display settings should preserve loaded reduced-motion value; canvas=\n{text}"
        );
        assert!(
            !crate::utils::session_storage::is_session_write_enabled(),
            "reading runtime config for Settings must not imply session writes"
        );
    }

    #[test]
    fn config_notifications_preview_uses_runtime_notification_shape() {
        let notification = notification_preview_for_value("Kitty (OSC 99)")
            .expect("enabled notification setting should build a preview row");
        assert_eq!(notification.key, "config-notification-preview");
        assert_eq!(notification.text, "Notifications preview: Kitty (OSC 99)");
        assert_eq!(notification.priority, NotificationPriority::Immediate);
        assert_eq!(notification.color, Some(NotificationColor::Suggestion));
        assert_eq!(notification.timeout_ms, Some(5_000));
        assert!(
            notification_preview_for_value("notifications_disabled").is_none(),
            "Disabled should clear the preview notification instead of enqueueing one"
        );
    }

    #[test]
    fn config_notification_channel_cycles_official_enum_values_without_submenu() {
        let mut config = GlobalConfig::default();
        config.preferred_notif_channel = Some("iterm2".to_string());
        let settings = SettingsJson::default();
        let mut all = items::build_items(&config, &settings);
        let notifications_idx = all
            .iter()
            .position(|item| item.id == "notifChannel")
            .expect("Config should include Notifications row");

        assert_eq!(submenu_for_item(&all[notifications_idx]), None);
        assert_eq!(
            item_display_value(&all[notifications_idx]),
            "iTerm2 (OSC 9)"
        );

        all[notifications_idx].toggle();
        assert_eq!(all[notifications_idx].display_value(), "terminal_bell");
        assert_eq!(
            item_display_value(&all[notifications_idx]),
            "Terminal Bell (\\a)"
        );
    }

    #[test]
    fn config_reduce_motion_preview_updates_live_spinner_context() {
        let events = stream::iter([
            press(KeyCode::Enter),
            press(KeyCode::Down),
            press(KeyCode::Down),
            press(KeyCode::Char(' ')),
        ]);
        let canvases =
            render_with_runtime_spinner(MockTerminalConfig::with_events(events).with_size(100, 30));
        let text = canvas_lines(
            canvases
                .last()
                .expect("mock render should produce a canvas"),
        )
        .join("\n");

        assert!(
            text.contains("Reduce motion") && text.contains("true"),
            "Config should preview Reduce motion as enabled in memory; canvas=\n{text}"
        );
        assert!(
            text.contains("● Thinking…"),
            "live spinner rows should consume Settings reduced-motion preview; canvas=\n{text}"
        );
    }

    #[test]
    fn config_verbose_preview_updates_app_state_for_footer() {
        // Six Downs from the top reached "Verbose output" before the
        // Cometix-only "Streaming text" row was added above it.
        let events = stream::iter([
            press(KeyCode::Enter),
            press(KeyCode::Down),
            press(KeyCode::Down),
            press(KeyCode::Down),
            press(KeyCode::Down),
            press(KeyCode::Down),
            press(KeyCode::Down),
            press(KeyCode::Down),
            press(KeyCode::Char(' ')),
        ]);
        let canvases =
            render_with_prompt_footer(MockTerminalConfig::with_events(events).with_size(100, 32));
        let text = canvas_lines(
            canvases
                .last()
                .expect("mock render should produce a canvas"),
        )
        .join("\n");

        assert!(
            text.contains("Verbose output") && text.contains("true"),
            "Config should preview Verbose output as enabled in memory; canvas=\n{text}"
        );
        assert!(
            text.contains("42 tokens"),
            "Prompt footer should consume the in-memory verbose preview; canvas=\n{text}"
        );
    }

    #[test]
    fn config_search_escape_clears_query_before_list_or_settings_close() {
        let mut events = text_events("theme");
        events.push(press(KeyCode::Esc));
        let text = render_text_with_events(events);

        assert!(
            text.contains("Search settings"),
            "Esc in search mode should clear the query and keep Config open; canvas=\n{text}"
        );
        assert!(
            text.contains("Auto-compact"),
            "clearing search should restore the full settings list; canvas=\n{text}"
        );
    }

    #[test]
    fn config_empty_search_result_matches_official_copy() {
        let text = render_text_with_events(text_events("zzzznomatch"));

        assert!(
            text.contains("No settings match \"zzzznomatch\""),
            "Config empty-search copy should match official Config.tsx; canvas=\n{text}"
        );
    }

    #[test]
    fn config_managed_theme_setting_opens_submenu_and_esc_returns_to_list() {
        // Frame-driven: Esc goes to the picker once it is on screen.
        let canvases =
            drive_config_with_provider(vec![open_theme_submenu(), vec![press(KeyCode::Esc)]]);
        let submenu_text = canvas_lines(&canvases[1]).join("\n");
        assert!(
            submenu_text.contains("Choose the text style that looks best"),
            "managed Theme row should open the ThemePicker; canvas=\n{submenu_text}"
        );
        assert!(
            submenu_text.contains("Dark mode"),
            "ThemePicker should render official-shaped options; canvas=\n{submenu_text}"
        );

        let closed_text = canvas_lines(&canvases[2]).join("\n");
        assert!(
            closed_text.contains("Theme") && !closed_text.contains("Choose the text style"),
            "Esc in the picker should close it back to the Config list; canvas=\n{closed_text}"
        );
    }

    #[test]
    fn config_submenu_selection_updates_preview_value_in_memory_only() {
        let mut items = items::default_items();
        assert!(apply_settings_submenu_selection(
            &mut items,
            SettingsSubmenu::Theme,
            "Light mode"
        ));
        let theme_item = items
            .iter()
            .find(|item| item.id == "theme")
            .expect("default settings include theme");
        assert_eq!(theme_item.display_value(), "Light mode");
        let auto_update_item = items
            .iter_mut()
            .find(|item| item.id == "autoUpdatesChannel")
            .expect("default settings include auto-update channel");
        auto_update_item.value = SettingValue::Display("disabled".to_string());
        assert!(apply_settings_submenu_selection(
            &mut items,
            SettingsSubmenu::EnableAutoUpdates,
            "stable"
        ));
        let auto_update_item = items
            .iter()
            .find(|item| item.id == "autoUpdatesChannel")
            .expect("default settings include auto-update channel");
        assert_eq!(auto_update_item.display_value(), "stable");
        assert!(
            !crate::utils::session_storage::is_session_write_enabled(),
            "settings picker previews must not imply session writes"
        );
    }

    #[test]
    fn config_picker_preview_events_do_not_write_settings_or_session_files() {
        crate::utils::process_runtime::initialize_test_process_runtime();
        let _guard = env_lock().lock().expect("env lock should not be poisoned");
        // Neutralize machine-level auto-update kill-switches so the
        // auto-updates picker rows behave identically on every host.
        let _disable_guard = EnvVarGuard::unset("DISABLE_AUTOUPDATER");
        let _claude_disable_guard = EnvVarGuard::unset("CLAUDE_CODE_DISABLE_AUTOUPDATER");
        let _traffic_guard = EnvVarGuard::unset("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC");
        let temp_root = std::env::temp_dir().join(format!(
            "cometix-settings-readonly-{}",
            uuid::Uuid::new_v4()
        ));
        let config_home = temp_root.join("config-home");
        fs::create_dir_all(&config_home).expect("temp config home should be created");
        let _config_guard = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config_home);
        let _projects_guard = crate::utils::session_storage::set_test_projects_dir_override(
            temp_root.join("session-projects"),
        );
        let _write_guard = EnvVarGuard::unset("COMETIX_WRITE_ENABLED");

        let cwd = std::env::current_dir().expect("test should know current directory");
        let snapshots = vec![
            PathSnapshot::capture(config_home.join(".claude.json")),
            PathSnapshot::capture(config_home.join(".config.json")),
            PathSnapshot::capture(config_home.join("settings.json")),
            PathSnapshot::capture(config_home.join("projects")),
            PathSnapshot::capture(cwd.join(".claude/settings.json")),
            PathSnapshot::capture(cwd.join(".claude/settings.local.json")),
        ];

        // Frame-driven throughout: a timed stream loses the tail of a key
        // sequence under load, once the reader's idle cutoff fires before
        // the next key lands. A theme save goes through
        // the provider's save handler — a no-op in this harness; in
        // production `saveGlobalConfig`, a dry run unless writes are enabled —
        // so this part checks the flow, not the file snapshots below.
        let theme_canvases = drive_config_with_provider(vec![
            open_theme_submenu(),
            vec![press(KeyCode::Down)],
            vec![press(KeyCode::Enter)],
        ]);
        let theme_text = canvas_lines(theme_canvases.last().unwrap()).join("\n");
        assert!(
            theme_text.contains("Light mode") && !theme_text.contains("Choose the text style"),
            "Theme selection should close the picker onto the new row value; canvas=\n{theme_text}"
        );

        let output_style_text =
            drive_output_style_submenu_text(vec![
                press(KeyCode::Down),
                press(KeyCode::Enter),
                press(KeyCode::F(12)),
            ]);
        assert!(
            !output_style_text.contains("Preferred output style")
                && output_style_text
                    .lines()
                    .any(|line| line.contains("Output style") && line.contains("Explanatory")),
            "Output style selection should update only the in-memory preview; canvas=\n{output_style_text}"
        );

        let mut language_events = text_events("language");
        language_events.push(press(KeyCode::Enter));
        language_events.push(press(KeyCode::Char(' ')));
        language_events.extend("Korean".chars().map(|ch| press(KeyCode::Char(ch))));
        language_events.push(press(KeyCode::Enter));
        let language_text = drive_config_keys_text(language_events);
        assert!(
            language_text.contains("Korean"),
            "Language submit should update only the in-memory preview; canvas=\n{language_text}"
        );

        let mut model_events = text_events("model");
        model_events.push(press(KeyCode::Enter));
        model_events.push(press(KeyCode::Char(' ')));
        model_events.push(press(KeyCode::Down));
        model_events.push(press(KeyCode::Enter));
        let model_text = drive_config_keys_text(model_events);
        assert!(
            model_text.contains("Model") && model_text.contains("Sonnet"),
            "Model selection should update only the in-memory preview; canvas=\n{model_text}"
        );

        let reduce_motion_events = vec![
            press(KeyCode::Enter),
            press(KeyCode::Down),
            press(KeyCode::Down),
            press(KeyCode::Char(' ')),
        ];
        let reduce_motion_text = drive_config_keys_text(reduce_motion_events);
        assert!(
            reduce_motion_text.contains("Reduce motion") && reduce_motion_text.contains("true"),
            "Reduce motion toggle should update only the in-memory preview; canvas=\n{reduce_motion_text}"
        );

        let mut notification_events = text_events("notification");
        notification_events.push(press(KeyCode::Enter));
        notification_events.push(press(KeyCode::Char(' ')));
        let notification_text = drive_config_keys_text(notification_events);
        assert!(
            notification_text.contains("Notifications")
                && notification_text.contains("iTerm2 (OSC 9)"),
            "Notification channel should cycle as an in-memory enum preview; canvas=\n{notification_text}"
        );
        assert!(
            !notification_text.contains("Enter select · Esc cancel"),
            "official notifChannel is an enum row, not a submenu picker; canvas=\n{notification_text}"
        );

        for snapshot in &snapshots {
            snapshot.assert_unchanged();
        }
        assert!(
            !crate::utils::session_storage::is_session_write_enabled(),
            "Settings previews must keep session writes disabled"
        );

        let _ = fs::remove_dir_all(temp_root);
    }

    #[test]
    fn config_theme_submenu_enter_applies_preview_and_closes_submenu() {
        // Frame-driven: Down and Enter go to the picker once it is on screen.
        let canvases = drive_config_with_provider(vec![
            open_theme_submenu(),
            vec![press(KeyCode::Down)],
            vec![press(KeyCode::Enter)],
        ]);
        let text = canvas_lines(canvases.last().unwrap()).join("\n");

        assert!(
            text.contains("Light mode"),
            "selecting a Theme submenu option should update the displayed value; canvas=\n{text}"
        );
        assert!(
            !text.contains("Choose the text style") && !text.contains("Enter to select · Esc to cancel"),
            "submenu should close after selecting a value; canvas=\n{text}"
        );
    }

    /// What the rest of the app sees of the provider while Config drives it,
    /// with a count of every key press (a bypass listener, so Config's
    /// consumption does not hide one) that makes each key produce a frame.
    #[component]
    fn ProviderThemeEcho(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let (current, value) = use_theme(&hooks);
        let streaming = crate::state::app_state::use_app_state(&mut hooks, |state| {
            state.settings.streaming_text_display.clone()
        });
        let mut keys = hooks.use_state(|| 0usize);
        hooks.use_terminal_events(move |event| {
            if matches!(event, TerminalEvent::Key(key) if key.kind == KeyEventKind::Press) {
                keys.set(keys.get() + 1);
            }
        });
        element! {
            Text(content: format!(
                "keys={} provider setting={} current={} streaming={}",
                keys.get(),
                value.theme_setting().setting_value(),
                current.setting_value(),
                streaming.as_deref().unwrap_or("unset"),
            ))
        }
    }

    #[component]
    fn ConfigWithProviderEchoHarness(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        use crate::components::design_system::theme_provider::{ThemeProvider, ThemeSaveHandler};
        let keybinding_runtime =
            crate::keybindings::keybinding_provider_setup::use_keybinding_setup(
                &mut hooks,
                crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings(),
            );
        element! {
            ContextProvider(value: Context::owned(keybinding_runtime)) {
                ThemeProvider(
                    initial_state: Some(ThemeSetting::Named(ThemeName::Dark)),
                    on_theme_save: Some(std::sync::Arc::new(|_| {}) as ThemeSaveHandler),
                ) {
                    crate::state::app_state::AppStateProvider(
                        children: crate::state::app_state::ProviderChildren::new(|| element! {
                            View(flex_direction: FlexDirection::Column) {
                                Config(max_visible: Some(12u32), header_focused: false)
                                ProviderThemeEcho
                            }
                        }.into_any()),
                    )
                }
            }
        }
    }

    /// Search "theme", leave the search box and open its submenu.
    fn open_theme_submenu() -> Vec<TerminalEvent> {
        let mut open = text_events("theme");
        open.push(press(KeyCode::Enter));
        open.push(press(KeyCode::Char(' ')));
        open
    }

    /// Drives `ConfigWithProviderEchoHarness` one key batch at a time and
    /// returns the canvas each batch's last key produced, after the mount
    /// canvas. The echo's key count makes every key produce a frame.
    fn drive_config_with_provider(batches: Vec<Vec<TerminalEvent>>) -> Vec<Canvas> {
        drive_harness(element!(ConfigWithProviderEchoHarness).into_any(), batches)
    }

    /// Drives a harness that renders a `keys=<count> ` echo one key batch at
    /// a time; returns the mount canvas and the canvas each batch's last key
    /// produced.
    fn drive_harness(
        app: AnyElement<'static>,
        batches: Vec<Vec<TerminalEvent>>,
    ) -> Vec<Canvas> {
        drive_harness_waiting(app, batches.into_iter().map(|batch| (None, batch)).collect())
    }

    /// `drive_harness` where a batch paired with a marker also waits for a
    /// frame showing it — for a submenu that loads after it opens (CC's
    /// OutputStylePicker takes no keys until its styles load). An empty batch
    /// sends nothing, so no frame follows it: it ends the run.
    fn drive_harness_waiting(
        mut app: AnyElement<'static>,
        batches: Vec<(Option<&'static str>, Vec<TerminalEvent>)>,
    ) -> Vec<Canvas> {
        futures::executor::block_on(async move {
            let (keys, events) = async_channel::unbounded();
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(110, 40),
            ));
            let mut sent = 0;
            let mut batch = 0;
            let mut canvases = Vec::new();
            let mut last_text = String::new();
            loop {
                // Frames only follow a change: a frame awaited for this long
                // (a key count or marker that never renders) fails the test
                // instead of stalling it until the runner's timeout.
                let next_frame = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_secs(10)).await;
                    None
                })
                .await;
                let Some(canvas) = next_frame else {
                    panic!(
                        "no frame with keys={sent} and batch {batch}'s marker; last frame:\n{last_text}"
                    );
                };
                let text = canvas_lines(&canvas).join("\n");
                assert!(text.contains("keys="), "the echo stopped rendering:\n{text}");
                last_text = text.clone();
                if !text.contains(&format!("keys={sent} ")) {
                    continue;
                }
                if let Some((Some(marker), _)) = batches.get(batch) {
                    if !text.contains(marker) {
                        continue;
                    }
                }
                canvases.push(canvas);
                let Some((_, next)) = batches.get(batch) else {
                    break;
                };
                if next.is_empty() {
                    assert_eq!(batch + 1, batches.len(), "an empty batch must come last");
                    break;
                }
                sent += next.len();
                for event in next.iter().cloned() {
                    keys.send(event).await.unwrap();
                }
                batch += 1;
            }
            canvases
        })
    }

    /// `drive_config_with_provider` with one key per frame, as a terminal
    /// delivers them; returns the text of the frame the last key produced.
    fn drive_config_keys_text(events: Vec<TerminalEvent>) -> String {
        let canvases =
            drive_config_with_provider(events.into_iter().map(|event| vec![event]).collect());
        canvas_lines(canvases.last().unwrap()).join("\n")
    }

    /// Search "style", leave the search box and open the Output style
    /// submenu; then, once its styles have loaded, send `after_load` one key
    /// per frame. Returns the text of the frame the last key produced.
    fn drive_output_style_submenu_text(after_load: Vec<TerminalEvent>) -> String {
        let mut open = text_events("style");
        open.push(press(KeyCode::Enter));
        open.push(press(KeyCode::Char(' ')));
        let mut batches: Vec<_> = open.into_iter().map(|event| (None, vec![event])).collect();
        let mut after_load = after_load.into_iter();
        // The first key after opening waits for the Explanatory row; with
        // no keys, an empty batch waits for it and ends the run.
        batches.push((Some("Explanatory"), after_load.next().into_iter().collect()));
        batches.extend(after_load.map(|event| (None, vec![event])));
        let canvases = drive_harness_waiting(
            element!(ConfigWithProviderEchoHarness).into_any(),
            batches,
        );
        canvas_lines(canvases.last().unwrap()).join("\n")
    }

    #[test]
    fn config_theme_submenu_drives_the_provider_and_escape_reverts_it() {
        // CC ThemePicker.tsx:132-149 previews and saves through the
        // ThemeProvider, so the whole app follows; Config's Escape
        // `revertChanges` restores the mount-time theme (Config.tsx:1477-1479).
        // Frame-driven: each key batch is followed by the frame its last key
        // counts to, and the provider state is read off that frame.
        let steps = futures::executor::block_on(async {
            let (keys, events) = async_channel::unbounded();
            let mut app = element!(ConfigWithProviderEchoHarness);
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(110, 40),
            ));
            let mut open = text_events("theme");
            open.push(press(KeyCode::Enter));
            open.push(press(KeyCode::Char(' ')));
            let batches = [
                open,
                vec![press(KeyCode::Down)],
                vec![press(KeyCode::Enter)],
                vec![press(KeyCode::Esc)],
            ];
            let mut sent = 0;
            let mut batch = 0;
            let mut steps = Vec::new();
            while let Some(canvas) = render_loop.next().await {
                let text = canvas_lines(&canvas).join("\n");
                assert!(text.contains("provider setting="), "the echo stopped rendering:\n{text}");
                if !text.contains(&format!("keys={sent} ")) {
                    continue;
                }
                steps.push(text);
                let Some(next) = batches.get(batch) else {
                    break;
                };
                sent += next.len();
                for event in next.iter().cloned() {
                    keys.send(event).await.unwrap();
                }
                batch += 1;
            }
            steps
        });
        let expected = [
            (None, "provider setting=dark current=dark"),
            // Opening previews the focused (current) theme.
            (Some("Choose the text style"), "provider setting=dark current=dark"),
            // Down previews the next option for the whole app.
            (Some("Choose the text style"), "provider setting=dark current=light"),
            // Enter saves it and closes the submenu.
            (None, "provider setting=light current=light"),
            // Escape closes /config and restores the mount-time theme.
            (None, "provider setting=dark current=dark"),
        ];
        assert_eq!(steps.len(), expected.len(), "steps={steps:#?}");
        for (step, (picker, provider)) in steps.iter().zip(expected) {
            assert!(step.contains(provider), "expected {provider:?} in\n{step}");
            if let Some(picker) = picker {
                assert!(step.contains(picker), "expected {picker:?} in\n{step}");
            }
        }
        assert!(!steps[3].contains("Choose the text style"), "{}", steps[3]);
    }

    #[test]
    fn config_theme_picker_renders_official_preview_and_syntax_footer() {
        let mut events = text_events("theme");
        events.push(press(KeyCode::Enter));
        events.push(press(KeyCode::Char(' ')));
        let text = render_text_with_events(events);

        assert!(text.contains("Theme"), "canvas=\n{text}");
        assert!(
            text.contains("Choose the text style that looks best with your terminal"),
            "canvas=\n{text}"
        );
        // StructuredDiff's filePath only picks the language: no header row.
        assert!(!text.contains("demo.js"), "canvas=\n{text}");
        assert!(text.contains("Hello, World!"), "canvas=\n{text}");
        assert!(text.contains("Hello, Claude!"), "canvas=\n{text}");
        assert!(
            text.contains("Syntax theme: Monokai Extended (ctrl+t to disable)"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("Enter to select · Esc to cancel"),
            "canvas=\n{text}"
        );
        assert!(
            !text.contains("Default dark color palette"),
            "ThemePicker options should not use the old invented descriptions; canvas=\n{text}"
        );
    }

    #[test]
    fn config_theme_picker_focus_previews_selected_palette_colors() {
        // Frame-driven: Down goes out once the picker is on screen, since the
        // picker, not Config, owns the submenu's keys.
        let canvases =
            drive_config_with_provider(vec![open_theme_submenu(), vec![press(KeyCode::Down)]]);
        let canvas = canvases
            .last()
            .expect("mock render should produce a canvas");
        let (title_x, title_y) = find_text(canvas, "Theme").expect("Theme title should render");
        let (added_x, added_y) =
            find_text(canvas, "Hello, Claude!").expect("diff preview should render added line");
        let (added_word_x, added_word_y) =
            find_text(canvas, "Claude").expect("word-level added highlight should render");
        let (removed_word_x, removed_word_y) =
            find_text(canvas, "World").expect("word-level removed highlight should render");

        assert_eq!(
            canvas
                .cell(title_x, title_y)
                .and_then(|cell| cell.text_style())
                .and_then(|style| style.color),
            Some(theme::LIGHT.permission)
        );
        assert_eq!(
            canvas
                .cell(added_x, added_y)
                .and_then(|cell| cell.background_color),
            Some(theme::LIGHT.diff_added)
        );
        assert_eq!(
            canvas
                .cell(added_word_x, added_word_y)
                .and_then(|cell| cell.background_color),
            Some(theme::LIGHT.diff_added_word)
        );
        assert_eq!(
            canvas
                .cell(removed_word_x, removed_word_y)
                .and_then(|cell| cell.background_color),
            Some(theme::LIGHT.diff_removed_word)
        );
    }

    #[test]
    fn config_theme_picker_escape_cancels_preview_palette() {
        // Down previews Light (checked, so the cancel is not vacuous); the
        // frame Esc produces must be back on Dark. It is read directly: a
        // reopened picker would preview the saved theme on mount and hide a
        // missing cancel.
        let canvases = drive_config_with_provider(vec![
            open_theme_submenu(),
            vec![press(KeyCode::Down)],
            vec![press(KeyCode::Esc)],
        ]);
        let (x, y) = find_text(&canvases[2], "Theme").expect("Theme title should render");
        assert_eq!(
            canvases[2]
                .cell(x, y)
                .and_then(|cell| cell.text_style())
                .and_then(|style| style.color),
            Some(theme::LIGHT.permission)
        );
        let after_escape = canvas_lines(&canvases[3]).join("\n");
        assert!(
            after_escape.contains("provider setting=dark current=dark")
                && !after_escape.contains("Choose the text style"),
            "{after_escape}"
        );
    }

    #[test]
    fn config_theme_submenu_ctrl_t_reaches_the_picker_binding() {
        // CC ThemePicker.tsx:61-83: inside the Theme submenu ctrl+t is the
        // picker's `theme:toggleSyntaxHighlighting` (its context outranks
        // Global's `app:toggleTodos`), and the toggle writes user settings,
        // so the config dir is a throwaway one.
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = std::env::temp_dir().join(format!(
            "cometix-config-theme-{}",
            uuid::Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&dir).unwrap();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &dir);
        let _syntax = EnvVarGuard::unset("CLAUDE_CODE_SYNTAX_HIGHLIGHT");
        // Frame-driven: ctrl+t goes out once the picker registered its context.
        let canvases = drive_config_with_provider(vec![open_theme_submenu(), vec![ctrl_char('t')]]);
        let text = canvas_lines(canvases.last().unwrap()).join("\n");
        let _ = fs::remove_dir_all(&dir);

        assert!(
            text.contains("Syntax highlighting disabled (ctrl+t to enable)"),
            "canvas=\n{text}"
        );
    }

    #[test]
    fn config_model_picker_renders_official_modelpicker_shape() {
        let mut events = text_events("model");
        events.push(press(KeyCode::Enter));
        events.push(press(KeyCode::Char(' ')));
        let text = render_text_with_events(events);

        assert!(text.contains("Select model"), "canvas=\n{text}");
        assert!(
            text.contains("Switch between Claude models."),
            "ModelPicker header copy should match official; canvas=\n{text}"
        );
        assert!(
            text.contains("specify with --model."),
            "ModelPicker header should include official --model hint; canvas=\n{text}"
        );
        assert!(
            text.lines().any(|line| {
                line.contains("Default (recommended)")
                    && line.contains("Use the default model (currently Sonnet 4.6)")
            }),
            "ModelPicker should use official compact rows with inline descriptions; canvas=\n{text}"
        );
        assert!(
            text.contains("● High effort (default)"),
            "ModelPicker should render the effort indicator; canvas=\n{text}"
        );
        assert!(
            text.contains("← → to adjust"),
            "ModelPicker should render effort adjustment hint; canvas=\n{text}"
        );
        assert!(
            text.contains("Enter to confirm · Esc to cancel"),
            "Config-owned ModelPicker footer should match official KeyboardShortcutHint copy; canvas=\n{text}"
        );
        assert!(
            !text.contains("Enter confirm · Esc cancel"),
            "ModelPicker footer should not use the old non-official terse copy; canvas=\n{text}"
        );
    }

    #[test]
    fn config_model_picker_effort_cycle_updates_visual_preview_only() {
        let mut events = text_events("model");
        events.push(press(KeyCode::Enter));
        events.push(press(KeyCode::Char(' ')));
        events.push(press(KeyCode::Down));
        events.push(press(KeyCode::Right));
        let text = render_text_with_timed_events(events);

        assert!(
            text.contains("◈ Max effort"),
            "Right should walk High → Max without confirming the submenu; canvas=\n{text}"
        );
        assert!(
            !crate::utils::session_storage::is_session_write_enabled(),
            "ModelPicker effort previews must not imply session writes"
        );
    }

    #[test]
    fn config_model_selection_updates_preview_in_memory_only() {
        let mut events = text_events("model");
        events.push(press(KeyCode::Enter));
        events.push(press(KeyCode::Char(' ')));
        events.push(press(KeyCode::Down));
        events.push(press(KeyCode::Enter));
        let text = render_text_with_events(events);

        assert!(
            text.contains("Model") && text.contains("Sonnet"),
            "selecting a ModelPicker option should update only the in-memory row value; canvas=\n{text}"
        );
        assert!(
            !text.contains("Select model"),
            "ModelPicker should close after selection; canvas=\n{text}"
        );
        assert!(
            !crate::utils::session_storage::is_session_write_enabled(),
            "ModelPicker selections must not imply session writes"
        );
    }

    #[test]
    fn config_output_style_picker_renders_official_built_in_options() {
        let _lock = env_lock().lock().unwrap();
        let temp_root = std::env::temp_dir().join(format!(
            "cometix-config-output-style-{}",
            uuid::Uuid::new_v4()
        ));
        let cwd = temp_root.join("repo");
        let config_home = temp_root.join("config");
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(&config_home).unwrap();
        let _config_guard = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config_home);
        let _cwd_guard = CurrentDirGuard::set(&cwd);
        let text = drive_output_style_submenu_text(Vec::new());
        let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let _ = fs::remove_dir_all(&temp_root);

        assert!(
            normalized.contains("Preferred output style"),
            "OutputStylePicker title should render; canvas=\n{text}"
        );
        assert!(
            normalized.contains("This changes how Claude Code communicates with you"),
            "official OutputStylePicker copy should render; canvas=\n{text}"
        );
        assert!(
            normalized.contains(
                "Claude completes coding tasks efficiently and provides concise responses"
            ),
            "Default output style description should match official fallback copy; canvas=\n{text}"
        );
        assert!(
            normalized.contains("Claude explains its implementation choices and codebase patterns"),
            "Explanatory output style description should match official copy; canvas=\n{text}"
        );
        assert!(
            normalized.contains(
                "Claude pauses and asks you to write small pieces of code for hands-on practice"
            ),
            "Learning output style description should match official copy; canvas=\n{text}"
        );
        assert!(
            text.lines().any(|line| {
                line.contains("Default")
                    && line.contains(
                        "Claude completes coding tasks efficiently and provides concise responses",
                    )
            }),
            "OutputStylePicker should use official compact two-column rows, not expanded description rows; canvas=\n{text}"
        );
        assert!(
            text.contains("Enter to confirm · Esc to cancel"),
            "Config-owned footer should render outside the hideInputGuide picker; canvas=\n{text}"
        );
        assert!(
            !text.contains("Concise"),
            "invented OutputStyle option should be removed; canvas=\n{text}"
        );
        assert!(
            !text.contains("Completes coding tasks efficiently and concisely"),
            "old non-official description should not render; canvas=\n{text}"
        );
    }

    #[test]
    fn config_output_style_escape_and_n_close_the_picker() {
        // CC OutputStylePicker.tsx:72 `<Dialog onCancel={onCancel}>` and
        // Config.tsx:1959-1962. The port's Dialog had a no-op cancel that
        // swallowed both keys. F12 is a key nothing binds: its frame shows
        // the settled cancel.
        for key in [KeyCode::Esc, KeyCode::Char('n')] {
            let text = drive_output_style_submenu_text(vec![press(key), press(KeyCode::F(12))]);
            assert!(!text.contains("Preferred output style"), "{key:?}: canvas=\n{text}");
            assert!(
                text.lines()
                    .any(|line| line.contains("Output style") && line.contains("default")),
                "{key:?}: the row keeps its value; canvas=\n{text}"
            );
        }
    }

    #[test]
    fn config_root_takes_the_full_width_under_a_row_parent() {
        // CC Config.tsx:1789-1791 `width="100%"`: under a row parent — CC's
        // Tab (Tabs.tsx:308), or this harness's root — Config would shrink to
        // its widest row without it. The search box stretches across Config.
        let canvases = render_with_config(
            MockTerminalConfig::with_events(stream::iter(Vec::<TerminalEvent>::new()))
                .with_size(100, 24),
        );
        let lines = canvas_lines(canvases.last().unwrap());
        let top = lines
            .iter()
            .find(|line| line.contains('╭'))
            .unwrap_or_else(|| panic!("no search box; canvas=\n{}", lines.join("\n")));
        assert_eq!(top.trim_end().chars().count(), 100, "{top:?}");
        assert!(top.starts_with('╭') && top.trim_end().ends_with('╮'), "{top:?}");
    }

    #[test]
    fn config_list_ignores_ctrl_and_alt_letters_instead_of_searching() {
        // CC Config.tsx:1770 `if (e.ctrl || e.meta) return`: a modified
        // letter neither starts a search nor is consumed. Ctrl+W and Alt+Q
        // are bound nowhere by default.
        for modifier in [KeyModifiers::CONTROL, KeyModifiers::ALT] {
            let letter = if modifier == KeyModifiers::CONTROL { 'w' } else { 'q' };
            let mut modified = KeyEvent::new(KeyEventKind::Press, KeyCode::Char(letter));
            modified.modifiers = modifier;
            let text = drive_config_keys_text(vec![
                press(KeyCode::Enter),
                TerminalEvent::Key(modified),
            ]);
            assert!(!text.contains(&format!("⌕ {letter}")), "{modifier:?}: canvas=\n{text}");
            assert!(text.contains("Search settings"), "{modifier:?}: canvas=\n{text}");
        }
    }

    #[test]
    fn config_tab_right_and_left_preview_streaming_text_like_space() {
        // CC Config.tsx:1761-1765: Left/Right/Tab run `toggleSetting()`, the
        // function select:accept (Space in Settings) runs, so each previews
        // the Streaming text mode into AppState as Space does. Shift+Tab is
        // `tab` to Ink and BackTab to crossterm.
        for key in [
            KeyCode::Tab,
            KeyCode::BackTab,
            KeyCode::Right,
            KeyCode::Left,
            KeyCode::Char(' '),
        ] {
            let mut keys = text_events("streaming");
            keys.push(press(KeyCode::Enter));
            keys.push(press(key));
            let text = drive_config_keys_text(keys);
            assert!(text.contains("streaming=line"), "{key:?}: canvas=\n{text}");
        }
    }

    #[test]
    fn config_output_style_selection_updates_preview_in_memory_only() {
        let text =
            drive_output_style_submenu_text(vec![
                press(KeyCode::Down),
                press(KeyCode::Enter),
                press(KeyCode::F(12)),
            ]);

        assert!(
            text.contains("Output style") && text.contains("Explanatory"),
            "selecting OutputStylePicker option should update only the in-memory row value; canvas=\n{text}"
        );
        assert!(
            !text.contains("Preferred output style"),
            "OutputStylePicker should close after selection; canvas=\n{text}"
        );
        assert!(
            !crate::utils::session_storage::is_session_write_enabled(),
            "OutputStylePicker previews must not imply session writes"
        );
    }

    #[test]
    fn config_language_submenu_uses_official_free_text_input() {
        let mut events = text_events("language");
        events.push(press(KeyCode::Enter));
        events.push(press(KeyCode::Char(' ')));
        let text = render_text_with_events(events);

        assert!(
            text.contains("Enter your preferred response and voice language:"),
            "Language submenu should render the official free-text prompt; canvas=\n{text}"
        );
        assert!(
            text.contains("e.g., Japanese") && text.contains("Español"),
            "Language submenu should show the official free-text placeholder; canvas=\n{text}"
        );
        assert!(
            !text.contains("Respond in Japanese"),
            "Language submenu should no longer render the old preset list seam; canvas=\n{text}"
        );
        assert!(
            text.contains("Enter to confirm · Esc to cancel"),
            "Config embeds the official Byline footer around LanguagePicker; canvas=\n{text}"
        );
        assert!(
            !text.contains("Enter confirm · Esc cancel"),
            "LanguagePicker footer copy should use official KeyboardShortcutHint text; canvas=\n{text}"
        );
    }

    #[test]
    fn config_language_submenu_styles_match_official_text_defaults() {
        let mut events = text_events("language");
        events.push(press(KeyCode::Enter));
        events.push(press(KeyCode::Char(' ')));
        let canvases = render_with_config(
            MockTerminalConfig::with_events(stream::iter(events)).with_size(100, 24),
        );
        let canvas = canvases
            .last()
            .expect("mock render should produce a canvas");
        let text = canvas_lines(canvas).join("\n");

        let (pointer_x, pointer_y) = find_text(canvas, POINTER).expect("pointer should render");
        let pointer_style = canvas
            .resolved_text_style(pointer_x, pointer_y)
            .expect("pointer style should resolve");
        assert_eq!(
            pointer_style.color, None,
            "Official LanguagePicker pointer uses terminal/default foreground; canvas=\n{text}"
        );

        let (footer_x, footer_y) = find_text(canvas, "Leave empty for default (English)")
            .expect("dim footer should render");
        let footer_style = canvas
            .resolved_text_style(footer_x, footer_y)
            .expect("footer style should resolve");
        assert_eq!(
            footer_style.color,
            Some(theme::current().inactive),
            "Official LanguagePicker dimColor maps to the inactive foreground; canvas=\n{text}"
        );

        let (byline_x, byline_y) = find_text(canvas, "Enter to confirm · Esc to cancel")
            .expect("Config-owned LanguagePicker byline should render");
        let byline_style = canvas
            .resolved_text_style(byline_x, byline_y)
            .expect("byline style should resolve");
        assert_eq!(
            byline_style.color,
            Some(theme::current().inactive),
            "Config-owned LanguagePicker footer uses official dimColor styling; canvas=\n{text}"
        );
    }

    #[test]
    fn config_language_free_text_submit_updates_preview_in_memory_only() {
        let mut events = text_events("language");
        events.push(press(KeyCode::Enter));
        events.push(press(KeyCode::Char(' ')));
        events.extend("Korean".chars().map(|ch| press(KeyCode::Char(ch))));
        events.push(press(KeyCode::Enter));
        let text = render_text_with_events(events);

        assert!(
            text.contains("Korean"),
            "Language free-text submit should update the in-memory displayed value; canvas=\n{text}"
        );
        assert!(
            !text.contains("Enter to confirm · Esc to cancel"),
            "Language submenu should close after submit; canvas=\n{text}"
        );
        assert!(
            !crate::utils::session_storage::is_session_write_enabled(),
            "Language picker previews must not imply session writes"
        );
    }

    #[test]
    fn config_language_picker_types_settings_keys_as_text() {
        // CC LanguagePicker.tsx:23-25: the Settings context keeps `n` (and
        // the list's j / k / space) as text while the picker is open.
        let mut events = text_events("language");
        events.push(press(KeyCode::Enter));
        events.push(press(KeyCode::Char(' ')));
        events.extend("nj k".chars().map(|ch| press(KeyCode::Char(ch))));
        events.push(press(KeyCode::Enter));
        let text = render_text_with_events(events);

        assert!(
            text.contains("nj k"),
            "keys bound in the Settings list must be typed into the picker; canvas=\n{text}"
        );
        assert!(
            !text.contains("Enter to confirm · Esc to cancel"),
            "Enter should submit and close the picker; canvas=\n{text}"
        );
    }

    #[test]
    fn config_language_picker_escape_cancels_without_applying() {
        // TextInput's Escape arms the "Esc again to clear" notification
        // timer before confirm:no cancels (CC useTextInput.ts:320-329).
        crate::utils::process_runtime::initialize_test_process_runtime();
        let mut events = text_events("language");
        events.push(press(KeyCode::Enter));
        events.push(press(KeyCode::Char(' ')));
        events.extend("Korean".chars().map(|ch| press(KeyCode::Char(ch))));
        events.push(press(KeyCode::Esc));
        let text = render_text_with_events(events);

        assert!(
            !text.contains("Enter your preferred response and voice language:"),
            "confirm:no should close the picker; canvas=\n{text}"
        );
        assert!(
            !text.contains("Korean") && text.contains("Default (English)"),
            "a cancelled picker leaves the language row unchanged; canvas=\n{text}"
        );
    }
    #[component]
    fn ConfigSearchPeerFrame(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let header = hooks.use_state(|| false);
        let mut forwarded = hooks.use_state(String::new);
        let mut owns_escape = hooks.use_state(|| false);
        // Observe events only after the real Config child has had its turn.
        // No app:interrupt handler is registered: source Settings owns C/D.
        hooks.use_propagated_terminal_events(move |event| {
            if let TerminalEvent::Key(key) = event.event() {
                if key.kind != KeyEventKind::Release
                    && key.modifiers.contains(KeyModifiers::CONTROL)
                {
                    if let KeyCode::Char(c @ ('c' | 'd')) = key.code {
                        let mut next = forwarded.read().clone();
                        next.push(c);
                        forwarded.set(next);
                        event.stop_propagation();
                    }
                }
            }
        });
        element! {
            View(flex_direction: FlexDirection::Column) {
                Text(content: format!("peer-header={} forwarded={} owns-escape={}", header.get(), forwarded.read().as_str(), owns_escape.get()))
                Config(
                    max_visible: Some(8u32),
                    header_focused: header.get(),
                    on_focus_header: move |_| { let mut header = header; header.set(true); },
                    on_is_search_mode_change: move |value| owns_escape.set(value),
                )
            }
        }
    }

    #[component]
    fn ConfigSearchPeerHarness(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let runtime = crate::keybindings::keybinding_provider_setup::use_keybinding_setup(
            &mut hooks,
            crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings(),
        );
        let store =
            hooks.use_const(|| crate::state::store::AppStore::new(Default::default(), None));
        element! {
            ContextProvider(value: Context::owned(runtime)) {

                ContextProvider(value: Context::owned(*theme::current())) {
                    ContextProvider(value: Context::owned(store.clone())) {
                        FocusScope(handle_keys: false) { ConfigSearchPeerFrame }
                    }
                }
            }
        }
    }

    #[tokio::test]
    async fn config_search_matches_official_paste_header_and_ctrl_passthrough() {
        // Actual source Config.tsx:278-285 options + full useSearchInput were
        // executed in consumer-search-peer-oracle.json. Meta+Backspace on an
        // empty query stays active; C/D passthrough leaves query untouched;
        // Up invokes focusHeader without clearing query or committing search.
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let (sender, receiver) = async_channel::unbounded();
        let mut app = element! { ConfigSearchPeerHarness };
        let mut frames = Box::pin(app.mock_terminal_render_loop(
            MockTerminalConfig::with_events(receiver).with_size(100, 24),
        ));
        let mut step = 0usize;
        let mut last = String::new();
        let deadline = futures_timer::Delay::new(Duration::from_secs(5));
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                _ = &mut deadline => panic!("Config source flow stalled at {step}:\n{last}"),
                canvas = frames.next() => {
                    let canvas = canvas.expect("Config terminal must stay mounted");
                    last = canvas_lines(&canvas).join("\n");
                    match step {
                        0 if last.contains("Search settings") && last.contains("owns-escape=true") => {
                            let mut key = KeyEvent::new(KeyEventKind::Press, KeyCode::Backspace);
                            key.modifiers = KeyModifiers::ALT;
                            sender.send(TerminalEvent::Key(key)).await.unwrap();
                            sender.send(TerminalEvent::Paste("zz-search🙂".to_string())).await.unwrap();
                            step = 1;
                        }
                        1 if last.contains("zz-search🙂") => {
                            assert!(last.contains("peer-header=false forwarded= owns-escape=true"), "{last}");
                            let mut key = KeyEvent::new(KeyEventKind::Press, KeyCode::Char('c'));
                            key.modifiers = KeyModifiers::CONTROL;
                            sender.send(TerminalEvent::Key(key)).await.unwrap();
                            step = 2;
                        }
                        2 if last.contains("forwarded=c owns-escape=true") => {
                            assert!(last.contains("zz-search🙂"), "Ctrl+C must not clear search: {last}");
                            let mut key = KeyEvent::new(KeyEventKind::Press, KeyCode::Char('d'));
                            key.modifiers = KeyModifiers::CONTROL;
                            sender.send(TerminalEvent::Key(key)).await.unwrap();
                            step = 3;
                        }
                        3 if last.contains("forwarded=cd owns-escape=true") => {
                            assert!(last.contains("zz-search🙂"), "Ctrl+D must not delete search: {last}");
                            sender.send(TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Up))).await.unwrap();
                            step = 4;
                        }
                        4 if last.contains("peer-header=true forwarded=cd owns-escape=false") => {
                            assert!(last.contains("zz-search🙂"), "Up must retain query: {last}");
                            break;
                        }
                        _ => {}
                    }
                }
            }
        }
        assert_eq!(step, 4);
    }
}
