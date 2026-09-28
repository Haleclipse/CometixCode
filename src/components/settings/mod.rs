//! Maps to: CC components/Settings/Settings.tsx (line 116-135)
//! CC Settings structure:
//!   <Pane color="permission">
//!     <Tabs title="Settings" color="permission"
//!           initialHeaderFocused={defaultTab !== 'Config'}
//!           selectedTab={selectedTab} onTabChange={...}>
//!       <Tab title="Status">...</Tab>
//!       <Tab title="Config">...</Tab>
//!       <Tab title="Usage">...</Tab>
//!     </Tabs>
//!   </Pane>
//! Tab rendering (CC design-system/Tabs.tsx line 243-258):
//!   selected + headerFocused + color → backgroundColor=color, color=inverseText
//!   selected + !headerFocused       → inverse=true, bold=true (white bg, black text)
//!   unselected                      → default color (dimColor implied)

pub mod config;
pub mod items;
pub mod status;
pub mod usage;

use crate::components::design_system::pane::Pane;
use crate::components::design_system::tabs::{TabItem, TabsHeader};
use crate::utils::theme::Theme;
use iocraft::prelude::*;

/// Maps to: CC Settings.tsx tab enum
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsTab {
    Status,
    Config,
    Usage,
}

impl SettingsTab {
    pub fn label(self) -> &'static str {
        match self {
            Self::Status => "Status",
            Self::Config => "Config",
            Self::Usage => "Usage",
        }
    }

    pub fn all() -> &'static [SettingsTab] {
        &[Self::Status, Self::Config, Self::Usage]
    }

    pub fn index(self) -> usize {
        match self {
            Self::Status => 0,
            Self::Config => 1,
            Self::Usage => 2,
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Status => Self::Config,
            Self::Config => Self::Usage,
            Self::Usage => Self::Status,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            Self::Status => Self::Usage,
            Self::Config => Self::Status,
            Self::Usage => Self::Config,
        }
    }
}

#[derive(Default, Props)]
pub struct SettingsProps<'a> {
    pub on_close: HandlerMut<'a, ()>,
    /// Official Config `onClose(result)` path for save-and-close summaries.
    pub on_result: HandlerMut<'a, String>,
    /// Maps to official Settings `defaultTab` prop used by `/usage`.
    pub default_tab: Option<SettingsTab>,
    /// Readonly session id for the Status tab. This may come from a restored
    /// transcript; no session file is created when it is absent.
    pub session_id: Option<String>,
    /// Readonly current session title for the Status tab.
    pub session_name: Option<String>,
}

/// Native placement carrier for CC `Settings.tsx:62`
/// `useExitOnCtrlCDWithKeybindings()`, like `PermissionRuleListCancelBinding`.
/// Ink keeps listener registration order, so the pane's hook, mounted before
/// any submenu, takes Ctrl+C/Ctrl+D ahead of what a submenu mounts later — a
/// ThemePicker's no-op exit hook, a LanguagePicker's TextInput. iocraft polls
/// descendants first; a zero-size first child of the pane restores that order.
#[component]
fn SettingsExitBinding(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let _ = crate::hooks::use_exit::use_exit_on_ctrl_cd_with_keybindings(&mut hooks, true);
    element! { View(width: 0u32, height: 0u32) }
}

/// Maps to: CC components/Settings/Settings.tsx
#[component]
pub fn Settings<'a>(
    props: &mut SettingsProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let initial_tab = props.default_tab.unwrap_or(SettingsTab::Config);
    let mut selected_tab = hooks.use_state(move || initial_tab);
    let (_, rows) = hooks.use_terminal_size();
    // Maps to: CC contentHeight = max(15, min(floor(rows * 0.8), 30))
    let content_height = (rows as f32 * 0.8).min(30.0).max(15.0) as u32;
    let theme = hooks.use_context::<Theme>();

    let mut should_close = hooks.use_state(|| false);
    // Maps to: CC Tabs.tsx headerFocused. /config starts with content
    // focused (`initialHeaderFocused={defaultTab !== 'Config'}`), and ↑ from
    // the Config search box moves focus to the tab row.
    let mut header_focused = hooks.use_state(move || initial_tab != SettingsTab::Config);
    // Maps to: official Settings `tabsHidden`, which hides the tab header
    // while Config-owned picker submenus are active.
    let mut tabs_hidden = hooks.use_state(|| false);
    // Maps to official `configOwnsEsc`: Config search clears/exits before
    // Settings closes the local command UI.
    let mut config_owns_esc = hooks.use_state(|| false);
    let mut pending_config_result = hooks.use_state(|| None::<String>);

    // CC `Settings.tsx:62` `useExitOnCtrlCDWithKeybindings()` lives in
    // `SettingsExitBinding`, the pane's first child: see there.

    let keybinding_runtime = hooks
        .try_use_context::<crate::keybindings::keybinding_context::KeybindingRuntime>()
        .map(|runtime| runtime.clone());
    crate::keybindings::use_keybinding::use_keybinding(
        &mut hooks,
        keybinding_runtime,
        "confirm:no",
        crate::keybindings::types::ContextName::Settings,
        move || {
            !tabs_hidden.get()
                && !(selected_tab.get() == SettingsTab::Config && config_owns_esc.get())
        },
        move || {
            should_close.set(true);
            true
        },
    );

    crate::components::design_system::tabs::use_tabs_keybindings(
        &mut hooks,
        !tabs_hidden.get() && header_focused.get(),
        {
            let mut selected_tab = selected_tab;
            let mut header_focused = header_focused;
            move || {
                selected_tab.set(selected_tab.get().next());
                header_focused.set(true);
            }
        },
        {
            let mut selected_tab = selected_tab;
            let mut header_focused = header_focused;
            move || {
                selected_tab.set(selected_tab.get().prev());
                header_focused.set(true);
            }
        },
    );

    hooks.use_propagated_terminal_events({
        move |event| match event.event() {
            TerminalEvent::Key(KeyEvent { code, kind, .. }) if *kind != KeyEventKind::Release => {
                if tabs_hidden.get() {
                    return;
                }
                let tab = selected_tab.get();
                if header_focused.get() {
                    // Tabs.tsx handleKeyDown: ↓ returns focus to opted-in
                    // content. Config is the interactive/opted-in tab here.
                    if matches!(code, KeyCode::Down) && tab == SettingsTab::Config {
                        header_focused.set(false);
                        event.stop_propagation();
                    }
                } else if tab == SettingsTab::Config && matches!(code, KeyCode::Up) {
                    // Config's useSearchInput({ onExitUp: focusHeader }) path.
                    // In list mode Config consumes ↑ itself; this only fires from
                    // search mode, where Config intentionally lets ↑ bubble.
                    header_focused.set(true);
                    event.stop_propagation();
                }
            }
            _ => {}
        }
    });

    let config_result = pending_config_result.read().clone();
    if let Some(result) = config_result {
        pending_config_result.set(None);
        (props.on_result)(result);
    }

    if should_close.get() {
        should_close.set(false);
        (props.on_close)(());
    }

    let tab = selected_tab.get();
    let settings_tabs = SettingsTab::all()
        .iter()
        .map(|tab| TabItem::new(tab.label(), tab.label()))
        .collect::<Vec<_>>();
    let are_tabs_hidden = tabs_hidden.get();
    let is_header_focused = header_focused.get() && !are_tabs_hidden;

    element! {
        Pane(color: theme.permission) {
            SettingsExitBinding
            #(if !are_tabs_hidden {
                Some(element! {
                    View(margin_bottom: 1u32) {
                        TabsHeader(
                            title: Some("Settings".to_string()),
                            color: Some(theme.permission),
                            tabs: settings_tabs,
                            selected_index: tab.index(),
                            header_focused: is_header_focused,
                        )
                    }
                })
            } else {
                None
            })

            // Tab content
            View(flex_direction: FlexDirection::Column) {
                #(match tab {
                    SettingsTab::Status => element! {
                        status::Status(
                            session_id: props.session_id.clone(),
                            session_name: props.session_name.clone(),
                        )
                    }.into_any(),
                    SettingsTab::Config => {
                        // Maps to: CC paneCap - 10 chrome
                        let max_vis = content_height.saturating_sub(10).max(5);
                        element! {
                            config::Config(
                                max_visible: max_vis,
                                header_focused: is_header_focused,
                                on_focus_header: move |_| { let mut header = header_focused; header.set(true); },
                                on_tabs_hidden_change: move |hidden| tabs_hidden.set(hidden),
                                on_close: move |_| should_close.set(true),
                                on_result: move |result| pending_config_result.set(Some(result)),
                                on_is_search_mode_change: move |owns| config_owns_esc.set(owns),
                            )
                        }.into_any()
                    },
                    SettingsTab::Usage => element! { usage::Usage }.into_any(),
                })
            }

            #(if !are_tabs_hidden && tab != SettingsTab::Config {
                Some(element! {
                    // Footer hint — mirrors the Tabs header-focus model. Config owns
                    // its official search/list footer inside Config.tsx.
                    View(margin_top: 1u32) {
                        Text(
                            content: if is_header_focused {
                                "Tab/←/→ switch tabs · ↓ return · Esc close"
                            } else {
                                "Esc close"
                            },
                            color: theme.subtle,
                        )
                    }
                })
            } else {
                None
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;
    use futures::{StreamExt, stream};
    use std::time::Duration;

    #[component]
    fn SettingsHarness(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let current_theme = *theme::current();
        let runtime = crate::keybindings::keybinding_provider_setup::use_keybinding_setup(
            &mut hooks,
            crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings(),
        );
        element! {
            ContextProvider(value: Context::owned(runtime)) {

                ContextProvider(value: Context::owned(current_theme)) {
                    // The Config tab reads AppState and raises preview
                    // notifications; both are strict at the source.
                    crate::state::app_state::AppStateProvider(
                        children: crate::state::app_state::ProviderChildren::new(|| element! {
                            Settings(on_close: move |_| {}, default_tab: Some(SettingsTab::Config))
                        }.into_any()),
                    )
                }
            }
        }
    }

    #[component]
    fn SettingsCloseProbe(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let current_theme = *theme::current();
        let runtime = crate::keybindings::keybinding_provider_setup::use_keybinding_setup(
            &mut hooks,
            crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings(),
        );
        let mut closed = hooks.use_state(|| false);
        element! {
            ContextProvider(value: Context::owned(runtime)) {

                ContextProvider(value: Context::owned(current_theme)) {
                crate::state::app_state::AppStateProvider(
                    children: crate::state::app_state::ProviderChildren::new(move || if closed.get() {
                        element! { Text(content: "settings closed") }.into_any()
                    } else {
                        element! {
                            Settings(
                                on_close: move |_| {
                                    let mut closed = closed;
                                    closed.set(true);
                                },
                                default_tab: Some(SettingsTab::Config),
                            )
                        }.into_any()
                    }),
                )
                }
            }
        }
    }

    #[component]
    fn SettingsStatusHarness(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let current_theme = *theme::current();
        let runtime = crate::keybindings::keybinding_provider_setup::use_keybinding_setup(
            &mut hooks,
            crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings(),
        );
        element! {
            ContextProvider(value: Context::owned(runtime)) {

                ContextProvider(value: Context::owned(current_theme)) {
                    // The Status tab reads `state.settings` for its model row.
                    // Defaults are the fixture: this harness's tests assert on
                    // tab selection and footer copy.
                    crate::state::app_state::AppStateProvider(
                        children: crate::state::app_state::ProviderChildren::new(|| element! {
                            Settings(on_close: move |_| {}, default_tab: Some(SettingsTab::Status))
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

    fn press(code: KeyCode) -> TerminalEvent {
        TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code))
    }

    fn text_events(text: &str) -> Vec<TerminalEvent> {
        text.chars().map(|ch| press(KeyCode::Char(ch))).collect()
    }

    fn render_settings_text(events: Vec<TerminalEvent>) -> String {
        render_settings_probe_text(element!(SettingsHarness), events)
    }

    fn render_settings_close_probe_text(events: Vec<TerminalEvent>) -> String {
        render_settings_probe_text(element!(SettingsCloseProbe), events)
    }

    fn render_settings_status_text(events: Vec<TerminalEvent>) -> String {
        render_settings_probe_text(element!(SettingsStatusHarness), events)
    }

    fn render_settings_probe_text<T>(
        mut app: Element<'static, T>,
        events: Vec<TerminalEvent>,
    ) -> String
    where
        T: Component + 'static,
    {
        futures::executor::block_on(async {
            let events = stream::unfold(events.into_iter(), |mut events| async move {
                let event = events.next()?;
                futures_timer::Delay::new(Duration::from_millis(1)).await;
                Some((event, events))
            });
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(110, 30),
            ));
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
                if canvases.len() >= 30 {
                    break;
                }
            }
            canvas_lines(
                canvases
                    .last()
                    .expect("mock render should produce a canvas"),
            )
            .join("\n")
        })
    }

    #[test]
    fn settings_config_search_box_spans_the_pane() {
        // CC Config.tsx:1789-1791 roots Config at `width="100%"`, which in
        // CC's Tab resolves to the pane's content width; the search box, a
        // direct child of the gap column (:2118-2123), stretches across it.
        // The permission tabs, with no percentage, follow their content
        // instead. (Here the tab container is a column, so this catches a
        // wrapper that stops the stretch; the root's width is checked under
        // a row parent in config.rs.)
        // The 110-column pane's content box, inside Pane's `paddingX={2}`
        // (Pane.tsx:52), is 106 columns from column 2.
        let text = render_settings_text(Vec::new());
        let top = text
            .lines()
            .find(|line| line.contains('╭'))
            .unwrap_or_else(|| panic!("no search box; canvas=\n{text}"));
        let border = top.trim();
        assert!(top.starts_with("  ╭"), "{top:?}\ncanvas=\n{text}");
        assert!(border.ends_with('╮'), "{top:?}");
        assert_eq!(border.chars().count(), 106, "{top:?}\ncanvas=\n{text}");
    }

    #[test]
    fn settings_exit_hook_takes_ctrl_c_ahead_of_the_language_input() {
        // CC `Settings.tsx:62` registers the exit hook (app:interrupt and
        // app:exit, Ctrl+C's action here) with the pane, before
        // LanguagePicker's TextInput mounts, so Ctrl+C arms the exit double
        // press and never clears the typed language. Frame-driven: each step
        // waits for the frame the previous one produced; the trailing `z`
        // guarantees a frame either way.
        crate::utils::process_runtime::initialize_test_process_runtime();
        let last = futures::executor::block_on(async {
            let (keys, events) = async_channel::unbounded();
            let mut app = element!(SettingsHarness);
            let mut frames = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(110, 30),
            ));
            let mut stage = 0;
            let mut last = String::new();
            while let Some(canvas) = frames.next().await {
                last = canvas_lines(&canvas).join("\n");
                match stage {
                    0 => {
                        let mut open = text_events("language");
                        open.push(press(KeyCode::Enter));
                        open.push(press(KeyCode::Char(' ')));
                        for event in open {
                            keys.send(event).await.unwrap();
                        }
                        stage = 1;
                    }
                    1 if last.contains("Enter your preferred response and voice language:") => {
                        let mut ctrl_c = KeyEvent::new(KeyEventKind::Press, KeyCode::Char('c'));
                        ctrl_c.modifiers = KeyModifiers::CONTROL;
                        let mut typed = text_events("Korean");
                        typed.push(TerminalEvent::Key(ctrl_c));
                        typed.push(press(KeyCode::Char('z')));
                        for event in typed {
                            keys.send(event).await.unwrap();
                        }
                        stage = 2;
                    }
                    2 if last.contains('z') => break,
                    _ => {}
                }
            }
            last
        });
        assert!(last.contains("Koreanz"), "canvas=\n{last}");
    }

    #[test]
    fn settings_exit_hook_exits_on_double_ctrl_d_with_an_empty_language_input() {
        // CC: `app:exit` belongs to Settings' useExitOnCtrlCDWithKeybindings
        // (Settings.tsx:62), registered before LanguagePicker's TextInput,
        // whose useTextInput registers no keybinding. So a double Ctrl+D in
        // the empty field exits. Frame-driven and hang-free: the render loop
        // ends on exit (the `z` sent in the same batch may still paint first);
        // if it does not, the typed `z` then `q` produce frames and the test
        // fails on "zq".
        crate::utils::process_runtime::initialize_test_process_runtime();
        let (exited, last) = futures::executor::block_on(async {
            let (keys, events) = async_channel::unbounded();
            let mut app = element!(SettingsHarness);
            let mut frames = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(110, 30),
            ));
            let mut stage = 0;
            let mut last = String::new();
            while let Some(canvas) = frames.next().await {
                last = canvas_lines(&canvas).join("\n");
                match stage {
                    0 => {
                        let mut open = text_events("language");
                        open.push(press(KeyCode::Enter));
                        open.push(press(KeyCode::Char(' ')));
                        for event in open {
                            keys.send(event).await.unwrap();
                        }
                        stage = 1;
                    }
                    1 if last.contains("Enter your preferred response and voice language:") => {
                        let mut ctrl_d = KeyEvent::new(KeyEventKind::Press, KeyCode::Char('d'));
                        ctrl_d.modifiers = KeyModifiers::CONTROL;
                        for event in [
                            TerminalEvent::Key(ctrl_d.clone()),
                            TerminalEvent::Key(ctrl_d),
                            press(KeyCode::Char('z')),
                        ] {
                            keys.send(event).await.unwrap();
                        }
                        stage = 2;
                    }
                    2 if last.contains("❯ z") => {
                        if keys.send(press(KeyCode::Char('q'))).await.is_err() {
                            return (true, last);
                        }
                        stage = 3;
                    }
                    3 if last.contains("❯ zq") => return (false, last),
                    _ => {}
                }
            }
            (true, last)
        });
        assert!(
            exited,
            "a double Ctrl+D in the empty language field must exit; canvas=\n{last}"
        );
    }

    #[test]
    fn settings_exit_hook_exits_on_double_ctrl_c_inside_the_theme_submenu() {
        // CC: Settings' exit hook (Settings.tsx:62) registered before the
        // ThemePicker mounts, so its listener takes Ctrl+C ahead of the
        // picker's no-op exit (ThemePicker.tsx:85-88) and a double press
        // exits. `SettingsExitBinding` keeps that order. Frame-driven: the
        // loop ends on exit; otherwise `z` and `q` produce frames.
        crate::utils::process_runtime::initialize_test_process_runtime();
        let (exited, last) = futures::executor::block_on(async {
            let (keys, events) = async_channel::unbounded();
            let mut app = element!(SettingsHarness);
            let mut frames = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(110, 30),
            ));
            let mut stage = 0;
            let mut last = String::new();
            while let Some(canvas) = frames.next().await {
                last = canvas_lines(&canvas).join("\n");
                match stage {
                    0 => {
                        let mut open = text_events("theme");
                        open.push(press(KeyCode::Enter));
                        open.push(press(KeyCode::Char(' ')));
                        for event in open {
                            keys.send(event).await.unwrap();
                        }
                        stage = 1;
                    }
                    1 if last.contains("Choose the text style") => {
                        let mut ctrl_c = KeyEvent::new(KeyEventKind::Press, KeyCode::Char('c'));
                        ctrl_c.modifiers = KeyModifiers::CONTROL;
                        for event in [TerminalEvent::Key(ctrl_c.clone()), TerminalEvent::Key(ctrl_c)] {
                            keys.send(event).await.unwrap();
                        }
                        stage = 2;
                    }
                    // The picker's own count of presses shows in its footer
                    // only if it, not Settings, took the keys.
                    2 => {
                        if keys.send(press(KeyCode::Down)).await.is_err() {
                            return (true, last);
                        }
                        stage = 3;
                    }
                    3 => return (false, last),
                    _ => {}
                }
            }
            (true, last)
        });
        assert!(
            exited,
            "a double Ctrl+C in the Theme submenu must exit through Settings; canvas=\n{last}"
        );
    }

    #[test]
    fn settings_default_status_tab_starts_with_header_focused_like_official() {
        let text = render_settings_status_text(Vec::new());

        assert!(
            text.contains("Status"),
            "Status tab should be selected for the /status entry path; canvas=\n{text}"
        );
        assert!(
            text.contains("Tab/←/→ switch tabs · ↓ return · Esc close"),
            "non-Config Settings entry should start with the tab header focused; canvas=\n{text}"
        );
    }

    #[test]
    fn settings_hides_tab_header_while_config_submenu_is_active() {
        let mut events = text_events("theme");
        events.push(press(KeyCode::Enter));
        events.push(press(KeyCode::Char(' ')));
        let text = render_settings_text(events);

        assert!(
            text.contains("Choose the text style that looks best"),
            "Theme submenu should be active; canvas=\n{text}"
        );
        assert!(
            !text.contains("Settings"),
            "official tabsHidden hides the Settings tab title while submenu is active; canvas=\n{text}"
        );
        assert!(
            !text.contains("Status") && !text.contains("Config") && !text.contains("Usage"),
            "official tabsHidden hides tab labels while submenu is active; canvas=\n{text}"
        );
        assert!(
            !text.contains("↑ tabs"),
            "Settings footer should not advertise tab focus while tabs are hidden; canvas=\n{text}"
        );
    }

    #[test]
    fn settings_cedes_escape_to_config_search_before_closing() {
        let text_after_first_escape = render_settings_close_probe_text(vec![press(KeyCode::Esc)]);
        assert!(
            text_after_first_escape.contains("Settings"),
            "first Esc in Config search mode should stay inside Settings; canvas=\n{text_after_first_escape}"
        );
        assert!(
            !text_after_first_escape.contains("settings closed"),
            "Settings parent must not close while Config search owns Esc; canvas=\n{text_after_first_escape}"
        );

        let text_after_second_escape =
            render_settings_close_probe_text(vec![press(KeyCode::Esc), press(KeyCode::Esc)]);
        assert!(
            text_after_second_escape.contains("settings closed"),
            "second Esc after Config exits search mode should close Settings; canvas=\n{text_after_second_escape}"
        );
    }

    #[test]
    fn settings_restores_tab_header_after_submenu_escape() {
        // Frame-driven: Esc goes out once the ThemePicker, which owns the
        // submenu's keys, is on screen; the frame its close produces is read.
        crate::utils::process_runtime::initialize_test_process_runtime();
        let text = futures::executor::block_on(async {
            let (keys, events) = async_channel::unbounded();
            let mut app = element!(SettingsHarness);
            let mut frames = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(110, 30),
            ));
            let mut stage = 0;
            let mut last = String::new();
            while let Some(canvas) = frames.next().await {
                last = canvas_lines(&canvas).join("\n");
                match stage {
                    0 => {
                        let mut open = text_events("theme");
                        open.push(press(KeyCode::Enter));
                        open.push(press(KeyCode::Char(' ')));
                        for event in open {
                            keys.send(event).await.unwrap();
                        }
                        stage = 1;
                    }
                    1 if last.contains("Choose the text style") => {
                        keys.send(press(KeyCode::Esc)).await.unwrap();
                        stage = 2;
                    }
                    2 if !last.contains("Choose the text style") => break,
                    _ => {}
                }
            }
            last
        });

        assert!(
            text.contains("Settings"),
            "Esc in submenu should return to the Settings tab shell; canvas=\n{text}"
        );
        assert!(
            text.contains("Config"),
            "Config tab label should return after submenu Esc; canvas=\n{text}"
        );
        assert!(
            !text.contains("Enter to select · Esc to cancel"),
            "the Theme submenu footer should disappear after its Esc; canvas=\n{text}"
        );
    }
}
