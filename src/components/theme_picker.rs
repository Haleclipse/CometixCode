//! Maps to: CC `components/ThemePicker.tsx`.
//!
//! Like CC's, the picker owns its keys (a Select), previews each focused
//! option for the whole app through the root ThemeProvider, saves on Enter,
//! cancels the preview on Esc, and toggles syntax highlighting (Ctrl+T in its
//! own keybinding context). Callers hand in `on_theme_select` / `on_cancel`.

use crate::components::custom_select::{
    Select, SelectInputOptionMeta, SelectLayout, SelectOptionData, UseSelectInputOptions,
    UseSelectStateProps, use_select_input, use_select_state,
};
use crate::components::design_system::byline::Byline;
use crate::components::design_system::keyboard_shortcut_hint::{
    KeyboardShortcutHint, KeyboardShortcutHintStyleContext,
};
use crate::components::design_system::theme_provider::{use_theme, use_theme_setting};
use crate::components::messages::user_tool_result_message::utils::{
    ToolRenderBackground, ToolRenderLine, ToolRenderTone,
};
use crate::components::structured_diff;
use crate::components::structured_diff::color_diff::{
    get_syntax_theme, syntax_highlighting_disabled_by_env,
};
use crate::keybindings::types::ContextName;
use crate::types::message::StructuredDiffHunk;
use crate::utils::theme::{THEME_PICKER_ORDER, Theme, ThemeName, ThemeSetting};
use iocraft::{Color, prelude::*};

/// Maps to: CC `ThemePicker.tsx:28-38` `ThemePickerProps`.
#[derive(Default, Props)]
pub struct ThemePickerProps<'a> {
    /// CC `onThemeSelect(setting)`, after the preview is saved.
    pub on_theme_select: HandlerMut<'a, ThemeSetting>,
    pub show_intro_text: bool,
    /// CC `helpText = ''`.
    pub help_text: String,
    pub show_help_text_below: bool,
    pub hide_esc_to_cancel: bool,
    /// CC `skipExitHandling`: the caller already owns Ctrl+C/Ctrl+D, and Esc
    /// calls `on_cancel` instead of shutting down.
    pub skip_exit_handling: bool,
    /// CC `onCancel?`, after the preview is cancelled.
    pub on_cancel: HandlerMut<'a, ()>,
}

/// Maps to: CC `ThemePicker.tsx:90-112` `themeOptions`. The Auto option sits
/// behind `feature('AUTO_THEME')`, off until its terminal watcher is ported.
pub fn theme_picker_options(auto_theme_enabled: bool) -> Vec<SelectOptionData> {
    let mut options = Vec::new();
    if auto_theme_enabled {
        options.push(SelectOptionData {
            label: "Auto (match terminal)".to_string(),
            value: "auto".to_string(),
            ..SelectOptionData::default()
        });
    }
    options.extend(
        THEME_PICKER_ORDER
            .iter()
            .map(|theme_name| SelectOptionData {
                label: theme_name.display_label().to_string(),
                value: theme_name.setting_value().to_string(),
                ..SelectOptionData::default()
            }),
    );
    options
}

/// Maps to: CC `ThemePicker.tsx:184-193` — the line under the preview, with
/// the toggle's shortcut as `useShortcutDisplay` resolves it. CC's fourth
/// branch (no syntax theme while the colour module is unavailable for another
/// reason) has no counterpart: the port's highlighter is always built in.
pub fn theme_picker_syntax_footer(
    active_theme_name: ThemeName,
    syntax_highlighting_disabled: bool,
    syntax_disabled_env_value: Option<&str>,
    shortcut: &str,
) -> String {
    if let Some(value) = syntax_disabled_env_value {
        return format!("Syntax highlighting disabled (via CLAUDE_CODE_SYNTAX_HIGHLIGHT={value})");
    }
    if syntax_highlighting_disabled {
        return format!("Syntax highlighting disabled ({shortcut} to enable)");
    }

    let syntax_theme = get_syntax_theme(active_theme_name.setting_value());
    if let Some(source) = syntax_theme.source {
        format!(
            "Syntax theme: {} (from {source}) ({shortcut} to disable)",
            syntax_theme.theme
        )
    } else {
        format!("Syntax theme: {} ({shortcut} to disable)", syntax_theme.theme)
    }
}

fn theme_picker_demo_diff_lines(
    width: usize,
    theme_name: ThemeName,
    syntax_highlighting_enabled: bool,
) -> Vec<ToolRenderLine> {
    let hunk = StructuredDiffHunk {
        old_start: 1,
        new_start: 1,
        old_lines: 3,
        new_lines: 3,
        lines: vec![
            " function greet() {".to_string(),
            "-  console.log(\"Hello, World!\");".to_string(),
            "+  console.log(\"Hello, Claude!\");".to_string(),
            " }".to_string(),
        ],
    };

    if syntax_highlighting_enabled {
        structured_diff::render_hunks_with_syntax(
            &[hunk],
            false,
            width.max(1),
            // CC :178-180: `filePath` only picks the language (no header
            // row), and `firstLine` is null.
            structured_diff::SyntaxHighlightOptions {
                file_path: Some("demo.js".to_string()),
                first_line: None,
                theme: get_syntax_theme(theme_name.setting_value()).highlight_theme(),
                prefix_content: None,
            },
        )
    } else {
        structured_diff::render_hunks(&[hunk], false, width.max(1))
    }
}

fn tool_background_color(theme: Theme, background: ToolRenderBackground) -> Color {
    match background {
        ToolRenderBackground::DiffAdded => theme.diff_added,
        ToolRenderBackground::DiffRemoved => theme.diff_removed,
        ToolRenderBackground::DiffAddedWord => theme.diff_added_word,
        ToolRenderBackground::DiffRemovedWord => theme.diff_removed_word,
    }
}

fn tool_tone_color(theme: Theme, tone: ToolRenderTone) -> Option<Color> {
    match tone {
        ToolRenderTone::Normal => None,
        ToolRenderTone::Success => Some(theme.success),
        ToolRenderTone::Warning => Some(theme.warning),
        ToolRenderTone::Error => Some(theme.error),
        ToolRenderTone::Inactive => Some(theme.inactive),
    }
}

/// Maps to: CC `ThemePicker.tsx:40-229` `ThemePicker`.
#[component]
pub fn ThemePicker<'a>(
    props: &mut ThemePickerProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    // CC :49-51 — the resolved theme (a preview included), the saved setting
    // and the width. The palette is the provider's, so a preview repaints the
    // whole app, this picker with it.
    let palette = *hooks.use_context::<Theme>();
    let (theme, theme_control) = use_theme(&hooks);
    let theme_setting = use_theme_setting(&hooks);
    let (columns, _) = hooks.use_terminal_size();
    // CC :52-54 `getColorModuleUnavailableReason() === 'env'`.
    let syntax_disabled_env_value = syntax_highlighting_disabled_by_env();
    // CC :56-58.
    let syntax_highlighting_disabled = crate::state::app_state::use_app_state(&mut hooks, |state| {
        state.settings.syntax_highlighting_disabled
    })
    .unwrap_or(false);
    let store = hooks
        .try_use_context::<crate::state::store::AppStore>()
        .map(|store| store.clone());
    let runtime = hooks
        .try_use_context::<crate::keybindings::keybinding_context::KeybindingRuntime>()
        .map(|runtime| runtime.clone());
    // CC :61 — ThemePicker bindings take precedence over Global ones (ctrl+t
    // is also `app:toggleTodos`).
    crate::keybindings::use_keybinding::use_register_keybinding_context(
        &mut hooks,
        runtime.clone(),
        ContextName::ThemePicker,
        true,
    );
    // CC :63-67 `useShortcutDisplay`.
    let syntax_toggle_shortcut = runtime.as_ref().map_or_else(
        || "ctrl+t".to_string(),
        |runtime| {
            crate::keybindings::shortcut_format::get_shortcut_display_from_bindings(
                "theme:toggleSyntaxHighlighting",
                &ContextName::ThemePicker,
                "ctrl+t",
                runtime.bindings().as_slice(),
            )
        },
    );
    // CC :69-84.
    crate::keybindings::use_keybinding::use_keybinding(
        &mut hooks,
        runtime,
        "theme:toggleSyntaxHighlighting",
        ContextName::ThemePicker,
        || true,
        {
            let color_module_unavailable = syntax_disabled_env_value.is_some();
            move || {
                if !color_module_unavailable {
                    let new_value = !syntax_highlighting_disabled;
                    let _ = crate::utils::settings::update_settings_for_source(
                        crate::utils::settings::SettingSource::User,
                        &serde_json::Map::from_iter([(
                            "syntaxHighlightingDisabled".to_string(),
                            serde_json::Value::Bool(new_value),
                        )]),
                    );
                    if let Some(store) = store.as_ref() {
                        store.replace_with(|state| {
                            std::sync::Arc::make_mut(&mut state.settings)
                                .syntax_highlighting_disabled = Some(new_value);
                        });
                    }
                }
                true
            }
        },
    );
    // CC :85-88: the picker always takes Ctrl+C/Ctrl+D; under
    // `skipExitHandling` its exit is a no-op. Whether a caller's own exit
    // hook sees the keys first is listener order: Settings (/config) and
    // Onboarding register theirs ahead of the picker (the port places them in
    // leading siblings), while /theme has no other owner — the prompt is
    // unmounted and REPL's cancel is idle — so there the second press does
    // nothing, as in CC.
    let exit_state = crate::hooks::use_exit::use_exit_on_ctrl_cd_with_keybindings_on_exit(
        &mut hooks,
        true,
        props
            .skip_exit_handling
            .then(|| std::sync::Arc::new(|| {}) as crate::hooks::use_exit::ExitHandler),
    );
    let mut app = hooks.use_app();

    // CC :130-153 `<Select>`.
    let options = theme_picker_options(false);
    let setting_value = theme_setting.setting_value().to_string();
    let state = use_select_state(
        &mut hooks,
        UseSelectStateProps {
            visible_option_count: Some(options.len()),
            values: options.iter().map(|option| option.value.clone()).collect(),
            default_value: Some(setting_value.clone()),
            focus_value: Some(setting_value),
        },
    );
    let events = use_select_input(
        &mut hooks,
        state,
        UseSelectInputOptions {
            has_on_cancel: true,
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
    // CC :132-134 `onFocus` — Select also fires it for the default focus.
    if let Some(value) = state.navigation.take_focus_change() {
        if let Some(setting) = ThemeSetting::from_setting_value(&value) {
            theme_control.set_preview_theme(setting);
        }
    }
    // CC :135-138 `onChange`.
    if let Some(value) = events.take_accepted() {
        theme_control.save_preview();
        if let Some(setting) = ThemeSetting::from_setting_value(&value) {
            (props.on_theme_select)(setting);
        }
    }
    // CC :139-149 `onCancel`.
    if events.take_cancelled() {
        theme_control.cancel_preview();
        if props.skip_exit_handling {
            (props.on_cancel)(());
        } else {
            // CC `gracefulShutdown(0)`.
            app.exit();
        }
    }

    let navigation = state.navigation.snapshot();
    let focused_index = navigation.focused_index().unwrap_or(0);
    let syntax_footer = theme_picker_syntax_footer(
        theme,
        syntax_highlighting_disabled,
        syntax_disabled_env_value.as_deref(),
        &syntax_toggle_shortcut,
    );
    // CC :181: the preview spans the terminal width.
    let diff_lines = theme_picker_demo_diff_lines(
        usize::from(columns),
        theme,
        syntax_disabled_env_value.is_none() && !syntax_highlighting_disabled,
    );
    let help_text = props.help_text.clone();

    // CC :114-196 `content`.
    let content = element! {
        View(flex_direction: FlexDirection::Column, row_gap: 1u32) {
            View(flex_direction: FlexDirection::Column, row_gap: 1u32) {
                #(if props.show_intro_text {
                    element! { Text(content: "Let's get started.".to_string()) }.into_any()
                } else {
                    element! { Text(content: "Theme".to_string(), color: palette.permission, weight: Weight::Bold) }.into_any()
                })
                View(flex_direction: FlexDirection::Column) {
                    Text(
                        content: "Choose the text style that looks best with your terminal".to_string(),
                        weight: Weight::Bold,
                    )
                    #(if !help_text.is_empty() && !props.show_help_text_below {
                        Some(element! { Text(content: help_text.clone(), color: palette.inactive) })
                    } else {
                        None
                    })
                }
                Select(
                    is_disabled: false,
                    hide_indexes: false,
                    visible_option_count: options.len(),
                    options: options,
                    focused_index: focused_index,
                    selected_value: state.committed_value(),
                    visible_from_index: navigation.visible_from_index,
                    layout: SelectLayout::Compact,
                )
            }
            View(flex_direction: FlexDirection::Column, width: 100pct) {
                View(
                    flex_direction: FlexDirection::Column,
                    border_style: BorderStyle::Dashed,
                    border_top: true,
                    border_bottom: true,
                    border_left: false,
                    border_right: false,
                    border_color: palette.subtle,
                ) {
                    #(diff_lines.into_iter().map(|line| {
                        let line_color = tool_tone_color(palette, line.tone);
                        let background_color = line.background.map(|background| tool_background_color(palette, background));
                        let dim_text = line.dim;
                        if !line.segments.is_empty() {
                            element! {
                                View(flex_direction: FlexDirection::Row) {
                                    #(line.segments.into_iter().map(|segment| {
                                        let segment_background = segment
                                            .background
                                            .map(|background| tool_background_color(palette, background))
                                            .or(background_color);
                                        let segment_color = segment.foreground.or(line_color);
                                        let segment_dim = dim_text || segment.dim;
                                        element! {
                                            Text(
                                                content: segment.text,
                                                color: segment_color,
                                                background_color: segment_background,
                                                dim: segment_dim,
                                                wrap: TextWrap::NoWrap,
                                            )
                                        }
                                    }))
                                }
                            }.into_any()
                        } else {
                            element! {
                                Text(
                                    content: line.text,
                                    color: line_color,
                                    background_color: background_color,
                                    dim: dim_text,
                                    wrap: TextWrap::NoWrap,
                                )
                            }.into_any()
                        }
                    }))
                }
                Text(content: format!(" {syntax_footer}"), color: palette.inactive)
            }
        }
    }
    .into_any();

    // CC :228: onboarding's intro form is the content alone.
    if props.show_intro_text {
        return content;
    }

    // CC :199-226. The footer Box keeps Ink's default row direction, so the
    // help text below and the Esc hint share a row.
    let show_help_below = props.show_help_text_below && !help_text.is_empty();
    let footer = (!props.hide_esc_to_cancel).then(|| {
        if exit_state.pending {
            element! {
                View {
                    Text(
                        content: format!("Press {} again to exit", exit_state.key_name.unwrap_or("Ctrl-C")),
                        color: palette.inactive,
                        italic: true,
                    )
                }
            }
            .into_any()
        } else {
            element! {
                View {
                    ContextProvider(value: Context::owned(KeyboardShortcutHintStyleContext {
                        dim: true,
                        italic: true,
                    })) {
                        Byline {
                            KeyboardShortcutHint(shortcut: "Enter".to_string(), action: "select".to_string())
                            KeyboardShortcutHint(shortcut: "Esc".to_string(), action: "cancel".to_string())
                        }
                    }
                }
            }
            .into_any()
        }
    });
    element! {
        Fragment {
            View(flex_direction: FlexDirection::Column) {
                #(content)
            }
            View(margin_top: 1u32) {
                #(show_help_below.then(|| element! {
                    View(margin_left: 3u32) {
                        Text(content: help_text.clone(), color: palette.inactive)
                    }
                }))
                #(footer)
            }
        }
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::design_system::theme_provider::{ThemeProvider, ThemeSaveHandler};
    use futures::StreamExt;
    use std::sync::{Arc, Mutex};

    #[test]
    fn theme_picker_options_match_official_external_order() {
        let options = theme_picker_options(false);
        assert_eq!(
            options
                .iter()
                .map(|option| option.label.as_str())
                .collect::<Vec<_>>(),
            vec![
                "Dark mode",
                "Light mode",
                "Dark mode (colorblind-friendly)",
                "Light mode (colorblind-friendly)",
                "Dark mode (ANSI colors only)",
                "Light mode (ANSI colors only)",
            ]
        );
        assert_eq!(theme_picker_options(true)[0].label, "Auto (match terminal)");
    }

    #[test]
    fn theme_picker_syntax_footer_matches_official_branches() {
        assert_eq!(
            theme_picker_syntax_footer(ThemeName::Dark, false, Some("0"), "ctrl+t"),
            "Syntax highlighting disabled (via CLAUDE_CODE_SYNTAX_HIGHLIGHT=0)"
        );
        assert_eq!(
            theme_picker_syntax_footer(ThemeName::Dark, true, None, "ctrl+t"),
            "Syntax highlighting disabled (ctrl+t to enable)"
        );
        assert_eq!(
            theme_picker_syntax_footer(ThemeName::Dark, false, None, "ctrl+t"),
            "Syntax theme: Monokai Extended (ctrl+t to disable)"
        );
    }

    #[derive(Default, Props)]
    struct PickerHarnessProps {
        show_intro_text: bool,
        skip_exit_handling: bool,
        events: Option<Arc<Mutex<Vec<String>>>>,
    }

    /// The picker under the providers it reads in production, with a line
    /// showing what the provider resolves and what the callbacks received.
    #[component]
    fn PickerHarness(props: &PickerHarnessProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let runtime = crate::keybindings::keybinding_provider_setup::use_keybinding_setup(
            &mut hooks,
            crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings(),
        );
        let events = props.events.clone().unwrap_or_default();
        let show_intro_text = props.show_intro_text;
        let skip_exit_handling = props.skip_exit_handling;
        element! {
            ContextProvider(value: Context::owned(runtime)) {
                ThemeProvider(
                    initial_state: Some(ThemeSetting::Named(ThemeName::Dark)),
                    on_theme_save: Some(Arc::new(|_| {}) as ThemeSaveHandler),
                ) {
                    crate::state::app_state::AppStateProvider(
                        children: crate::state::app_state::ProviderChildren::new(move || {
                            let selected = events.clone();
                            let cancelled = events.clone();
                            let echo = events.clone();
                            element! {
                                View(flex_direction: FlexDirection::Column) {
                                    ThemePicker(
                                        show_intro_text: show_intro_text,
                                        help_text: "To change this later, run /theme".to_string(),
                                        skip_exit_handling: skip_exit_handling,
                                        on_theme_select: move |setting: ThemeSetting| {
                                            selected.lock().unwrap().push(format!("select {}", setting.setting_value()));
                                        },
                                        on_cancel: move |_| cancelled.lock().unwrap().push("cancel".to_string()),
                                    )
                                    PickerEcho(events: Some(echo))
                                }
                            }
                            .into_any()
                        }),
                    )
                }
            }
        }
    }

    #[derive(Default, Props)]
    struct PickerEchoProps {
        events: Option<Arc<Mutex<Vec<String>>>>,
    }

    /// Counts every key press (a bypass listener, so the picker consuming a
    /// key does not hide it) so each key is followed by a frame.
    #[component]
    fn PickerEcho(props: &PickerEchoProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let (current, value) = use_theme(&hooks);
        let mut keys = hooks.use_state(|| 0usize);
        hooks.use_terminal_events(move |event| {
            if matches!(event, TerminalEvent::Key(key) if key.kind == KeyEventKind::Press) {
                keys.set(keys.get() + 1);
            }
        });
        let events = props.events.clone().unwrap_or_default();
        let events = events.lock().unwrap().join(",");
        element! {
            Text(content: format!(
                "keys={} provider setting={} current={} events=[{events}]",
                keys.get(),
                value.theme_setting().setting_value(),
                current.setting_value(),
            ))
        }
    }

    fn render_harness(show_intro_text: bool) -> String {
        element!(PickerHarness(show_intro_text: show_intro_text, skip_exit_handling: true))
            .render(Some(100))
            .to_string()
    }

    #[test]
    fn theme_picker_renders_official_title_preview_and_footer() {
        let text = render_harness(false);
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
        assert!(text.contains("Enter to select · Esc to cancel"), "canvas=\n{text}");
    }

    #[test]
    fn theme_picker_intro_mode_matches_onboarding_shape_without_footer() {
        let text = render_harness(true);
        assert!(text.contains("Let's get started."), "canvas=\n{text}");
        assert!(text.contains("To change this later, run /theme"), "canvas=\n{text}");
        assert!(!text.contains("Enter to select"), "canvas=\n{text}");
        assert!(!text.contains("Theme\n"), "canvas=\n{text}");
    }

    /// Drives the harness one key batch at a time, reading the frame each
    /// batch's last key produces.
    fn drive(batches: Vec<Vec<TerminalEvent>>) -> Vec<String> {
        let events = Arc::new(Mutex::new(Vec::new()));
        futures::executor::block_on(async move {
            let (keys, stream) = async_channel::unbounded();
            let mut app = element!(PickerHarness(skip_exit_handling: true, events: Some(events)));
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(stream).with_size(100, 40),
            ));
            let mut sent = 0;
            let mut batch = 0;
            let mut frames = Vec::new();
            while let Some(canvas) = render_loop.next().await {
                let text = canvas.to_string();
                assert!(text.contains("keys="), "the harness stopped rendering:\n{text}");
                if !text.contains(&format!("keys={sent} ")) {
                    continue;
                }
                frames.push(text);
                let Some(next) = batches.get(batch) else {
                    break;
                };
                sent += next.len();
                for event in next.iter().cloned() {
                    keys.send(event).await.unwrap();
                }
                batch += 1;
            }
            frames
        })
    }

    fn press(code: KeyCode) -> TerminalEvent {
        TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code))
    }

    #[test]
    fn theme_picker_previews_on_focus_saves_on_enter_and_cancels_on_escape() {
        // CC ThemePicker.tsx:132-149.
        let frames = drive(vec![
            vec![press(KeyCode::Down)],
            vec![press(KeyCode::Enter)],
            vec![press(KeyCode::Down)],
            vec![press(KeyCode::Esc)],
        ]);
        let expected = [
            // Mounting focuses the saved setting and previews it.
            "provider setting=dark current=dark events=[]",
            // Down previews Light for the whole app.
            "provider setting=dark current=light events=[]",
            // Enter saves the preview, then hands the setting over.
            "provider setting=light current=light events=[select light]",
            // Down previews the next option again.
            "provider setting=light current=dark-daltonized events=[select light]",
            // Esc cancels that preview and calls on_cancel.
            "provider setting=light current=light events=[select light,cancel]",
        ];
        assert_eq!(frames.len(), expected.len(), "frames={frames:#?}");
        for (frame, expected) in frames.iter().zip(expected) {
            assert!(frame.contains(expected), "expected {expected:?} in\n{frame}");
        }
    }

    #[test]
    fn theme_picker_ctrl_t_toggles_syntax_highlighting_in_its_own_context() {
        // CC ThemePicker.tsx:61-83: the ThemePicker context outranks Global's
        // ctrl+t (`app:toggleTodos`), and the toggle writes user settings and
        // AppState, which the footer then reads.
        let _lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = std::env::temp_dir().join(format!(
            "cometix-theme-picker-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let _config = crate::utils::env_utils::EnvVarGuard::set("CLAUDE_CONFIG_DIR", &dir);
        let _syntax = crate::utils::env_utils::EnvVarGuard::unset("CLAUDE_CODE_SYNTAX_HIGHLIGHT");
        let mut ctrl_t = KeyEvent::new(KeyEventKind::Press, KeyCode::Char('t'));
        ctrl_t.modifiers = KeyModifiers::CONTROL;
        let frames = drive(vec![vec![TerminalEvent::Key(ctrl_t)]]);
        assert_eq!(frames.len(), 2, "frames={frames:#?}");
        assert!(
            frames[0].contains("Syntax theme: Monokai Extended (ctrl+t to disable)"),
            "{}",
            frames[0]
        );
        assert!(
            frames[1].contains("Syntax highlighting disabled (ctrl+t to enable)"),
            "{}",
            frames[1]
        );
        let written = std::fs::read_to_string(dir.join("settings.json")).unwrap_or_default();
        assert!(written.contains("\"syntaxHighlightingDisabled\": true"), "{written}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
