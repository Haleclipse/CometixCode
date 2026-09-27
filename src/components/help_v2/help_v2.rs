//! Maps to: CC `components/HelpV2/HelpV2.tsx`.
//!
//! HelpV2 owns only what CC's does: `help:dismiss`, the Ctrl+C/D exit hook
//! and the footer. The tab row is the design-system `Tabs` (tab/←/→ while
//! it has focus, ↓ into an opted-in tab), and each command tab's `Commands`
//! owns its list's keys, handing focus back to the tab row on ↑ from the
//! first item.
//!
//! Listener order differs for Esc on a list. CC registers `help:dismiss`
//! when HelpV2 mounts and a tab's Select only when that tab is shown, so
//! `help:dismiss` takes Esc first; iocraft polls the list (a descendant)
//! first, so the Select's cancel takes it. Both close once with the default
//! bindings; they differ only when Esc is unbound in Select or Help. No
//! single placement of `help:dismiss` gives CC's whole order (Tabs' tab
//! keys, then `help:dismiss`, then the Select, then Tabs' ↓): Tabs
//! registers its ↓ ahead of its tab keys so that ↓ and a tab key read
//! together behave as in CC, and moving `help:dismiss` ahead of the list
//! would put it ahead of the tab keys too.
//!
//! Not ported: CC's height cap (`height={insideModal ? undefined :
//! maxHeight}`, :107) relies on Yoga shrinking children below their content,
//! which taffy does not do; and the internal build's `/help` title and
//! ant-only tab (:47-55, :92-104), which wait on `INTERNAL_ONLY_COMMANDS`.

use std::sync::Arc;

use crate::commands::Command;
use crate::components::design_system::pane::Pane;
use crate::components::design_system::tabs::{Tab, Tabs};
use crate::components::help_v2::commands::Commands;
use crate::components::help_v2::general::General;
use crate::constants::product;
use crate::keybindings::keybinding_context::KeybindingRuntime;
use crate::keybindings::types::ContextName;
use crate::utils::theme::Theme;
use iocraft::prelude::*;

const HELP_DISPLAY_NAME: &str = "Cometix Code";
const DOCS_URL: &str = "https://code.claude.com/docs/en/overview";

#[derive(Default, Props)]
pub struct HelpV2Props<'a> {
    pub on_close: HandlerMut<'a, ()>,
    pub commands: Option<Arc<Vec<Command>>>,
}

/// Maps to CC `components/HelpV2/HelpV2.tsx:26-138#HelpV2`.
#[component]
pub fn HelpV2<'a>(props: &mut HelpV2Props<'a>, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme = *hooks.use_context::<Theme>();
    let (columns, rows) = hooks.use_terminal_size();
    let max_height = rows / 2;

    // CC :35 `close`. The handlers below cannot hold `on_close`; they set
    // this, and the next render calls it.
    let mut should_close = hooks.use_state(|| false);
    let close = move || {
        let mut should_close = should_close;
        should_close.set(true);
    };
    let runtime = hooks
        .try_use_context::<KeybindingRuntime>()
        .map(|runtime| runtime.clone());
    // CC :36-37.
    crate::keybindings::use_keybinding::use_keybinding(
        &mut hooks,
        runtime.clone(),
        "help:dismiss",
        ContextName::Help,
        || true,
        move || {
            close();
            true
        },
    );
    let exit_state = crate::hooks::use_exit::use_exit_on_ctrl_cd_with_keybindings_on_exit(
        &mut hooks,
        true,
        Some(Arc::new(close)),
    );
    // CC :38 `useShortcutDisplay('help:dismiss', 'Help', 'esc')`.
    let dismiss_shortcut = runtime.as_ref().map_or_else(
        || "esc".to_string(),
        |runtime| {
            crate::keybindings::shortcut_format::get_shortcut_display_from_bindings(
                "help:dismiss",
                &ContextName::Help,
                "esc",
                runtime.bindings().as_slice(),
            )
        },
    );

    if should_close.get() {
        should_close.set(false);
        (props.on_close)(());
    }

    let commands = hooks.use_const({
        let provided = props.commands.clone();
        move || {
            provided.unwrap_or_else(|| {
                #[cfg(test)]
                {
                    Arc::new(crate::commands::declared_commands_for_tests())
                }
                #[cfg(not(test))]
                {
                    Arc::new(crate::commands::get_commands(
                        &crate::bootstrap::state::get_original_cwd(),
                    ))
                }
            })
        }
    });
    // CC :40-60: built-in by name (aliases included), hidden ones dropped.
    let builtin_names = crate::commands::built_in_command_names();
    let builtin_commands = Arc::new(
        commands
            .iter()
            .filter(|command| builtin_names.contains(command.name.as_ref()))
            .filter(|command| !crate::commands::is_command_hidden(command))
            .cloned()
            .collect::<Vec<_>>(),
    );
    let custom_commands = Arc::new(
        commands
            .iter()
            .filter(|command| !builtin_names.contains(command.name.as_ref()))
            .filter(|command| !crate::commands::is_command_hidden(command))
            .cloned()
            .collect::<Vec<_>>(),
    );

    // CC :127-133 `<Text dimColor>`; only the dismiss hint is italic.
    let footer = if exit_state.pending {
        format!(
            "Press {} again to exit",
            exit_state.key_name.unwrap_or("Ctrl-C")
        )
    } else {
        format!("{dismiss_shortcut} to cancel")
    };

    // CC :106-137.
    element! {
        Pane(color: theme.professional_blue) {
            Tabs(
                title: Some(format!("{HELP_DISPLAY_NAME} v{}", product::VERSION)),
                color: Some(theme.professional_blue),
                default_tab: Some("general".to_string()),
            ) {
                Tab(title: "general".to_string()) {
                    General
                }
                Tab(title: "commands".to_string()) {
                    Commands(
                        commands: builtin_commands,
                        max_height,
                        columns,
                        title: "Browse default commands:".to_string(),
                        on_cancel: move |_| close(),
                    )
                }
                Tab(title: "custom-commands".to_string()) {
                    Commands(
                        commands: custom_commands,
                        max_height,
                        columns,
                        title: "Browse custom commands:".to_string(),
                        empty_message: Some("No custom commands found".to_string()),
                        on_cancel: move |_| close(),
                    )
                }
            }
            View(margin_top: 1u32) {
                Text(segments: Some(vec![
                    StyledSegment::new("For more help: "),
                    link_segment(DOCS_URL.to_string(), None, None, None),
                ]))
            }
            View(margin_top: 1u32) {
                Text(content: footer, color: theme.inactive, italic: !exit_state.pending)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use std::sync::Mutex;

    #[derive(Default, Props)]
    struct HelpHarnessProps {
        closes: Option<Arc<Mutex<usize>>>,
    }

    /// HelpV2 under the keybinding runtime and theme it mounts with, and a
    /// line counting key presses (so each read produces a frame) and
    /// `onClose` calls.
    #[component]
    fn HelpHarness(props: &HelpHarnessProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let runtime = crate::keybindings::keybinding_provider_setup::use_keybinding_setup(
            &mut hooks,
            KeybindingRuntime::with_default_bindings(),
        );
        let closes = props.closes.clone().unwrap_or_default();
        let counted = closes.clone();
        let mut keys = hooks.use_state(|| 0usize);
        hooks.use_terminal_events(move |event| {
            if matches!(event, TerminalEvent::Key(key) if key.kind == KeyEventKind::Press) {
                keys.set(keys.get() + 1);
            }
        });
        let close_count = *closes.lock().unwrap();
        element! {
            ContextProvider(value: Context::owned(runtime)) {
                ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                    View(flex_direction: FlexDirection::Column, width: 100u32) {
                        HelpV2(on_close: move |_| *counted.lock().unwrap() += 1)
                        Text(content: format!("keys={} closes={close_count}", keys.get()))
                    }
                }
            }
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(KeyEventKind::Press, code)
    }

    fn ctrl(c: char) -> KeyEvent {
        let mut event = KeyEvent::new(KeyEventKind::Press, KeyCode::Char(c));
        event.modifiers = KeyModifiers::CONTROL;
        event
    }

    /// Sends each batch in one go once the previous one has a frame, then
    /// an unbound F12 to settle; returns the last frame and the close count.
    fn drive(batches: Vec<Vec<KeyEvent>>) -> (String, usize) {
        let (frames, closes) = drive_frames(batches);
        (frames.last().cloned().unwrap_or_default(), closes)
    }

    /// As [`drive`], returning the frame each batch settled on (the last is
    /// the F12 one). Panics if a frame takes over ten seconds, or if the
    /// render loop ends (the app exited) before the last batch settled.
    fn drive_frames(batches: Vec<Vec<KeyEvent>>) -> (Vec<String>, usize) {
        let closes = Arc::new(Mutex::new(0usize));
        let counted = closes.clone();
        let frames = futures::executor::block_on(async move {
            let (keys, events) = async_channel::unbounded();
            let mut app = element!(HelpHarness(closes: Some(counted)));
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(100, 32),
            ));
            let mut batches = batches;
            batches.push(vec![key(KeyCode::F(12))]);
            let mut next_batch = 0;
            let mut sent = 0;
            let mut last = String::new();
            let mut settled = Vec::new();
            loop {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(std::time::Duration::from_secs(10)).await;
                    None
                })
                .await;
                let Some(canvas) = next else {
                    panic!("no frame after batch {next_batch} (or the app exited); last:\n{last}");
                };
                last = canvas.to_string();
                if !last.contains(&format!("keys={sent} ")) {
                    continue;
                }
                if next_batch > 0 {
                    settled.push(last.clone());
                }
                let Some(batch) = batches.get(next_batch) else {
                    break;
                };
                for event in batch {
                    keys.send(TerminalEvent::Key(event.clone()))
                        .await
                        .unwrap_or_else(|_| panic!("the app exited before batch {next_batch}; last:\n{last}"));
                }
                sent += batch.len();
                next_batch += 1;
            }
            settled
        });
        let closes = *closes.lock().unwrap();
        (frames, closes)
    }

    fn keys(codes: &[KeyCode]) -> Vec<Vec<KeyEvent>> {
        codes.iter().map(|code| vec![key(*code)]).collect()
    }

    /// The option label under the pointer.
    fn pointed(text: &str) -> Option<String> {
        text.lines()
            .find_map(|line| line.split_once('❯').map(|(_, rest)| rest.trim().to_string()))
    }

    #[test]
    fn help_v2_uses_official_tab_shape() {
        let (text, _) = drive(Vec::new());
        assert!(text.contains("Cometix Code v"), "canvas=\n{text}");
        // CC :62-89: the tab titles.
        assert!(
            text.contains("general   commands   custom-commands"),
            "canvas=\n{text}"
        );
        assert!(text.contains("Shortcuts"), "canvas=\n{text}");
        assert!(text.contains("For more help: https://code.claude.com/docs/en/overview"), "canvas=\n{text}");
        assert!(text.contains("Esc to cancel"), "canvas=\n{text}");
    }

    #[test]
    fn help_v2_commands_tab_lists_default_commands() {
        let (text, _) = drive(keys(&[KeyCode::Tab]));
        assert!(text.contains("Browse default commands:"), "canvas=\n{text}");
        assert!(text.contains("/add-dir"), "canvas=\n{text}");
        assert!(text.contains("Add a new working directory"), "canvas=\n{text}");
        // CC Commands.tsx:28: `(maxHeight - 10) / 2` rows of the half-height
        // cap; 32 rows give 3.
        let listed = text
            .lines()
            .filter(|line| {
                line.trim_start_matches([' ', '❯', '↑', '↓'])
                    .starts_with('/')
            })
            .count();
        assert_eq!(listed, 3, "canvas=\n{text}");
    }

    #[test]
    fn help_v2_list_takes_focus_from_the_tab_row_and_moves_with_select_keys() {
        // ↓ from the tab row focuses the list (Tabs.tsx:163-172); the list
        // then moves with the Select bindings, j included.
        let (text, _) = drive(keys(&[KeyCode::Tab, KeyCode::Down]));
        assert_eq!(pointed(&text).as_deref(), Some("/add-dir"), "canvas=\n{text}");
        let (text, _) = drive(keys(&[KeyCode::Tab, KeyCode::Down, KeyCode::Char('j')]));
        let second = pointed(&text).unwrap_or_default();
        assert!(second.starts_with('/') && second != "/add-dir", "canvas=\n{text}");
    }

    #[test]
    fn help_v2_up_from_the_first_command_hands_focus_back_to_the_tab_row() {
        // CC Commands.tsx:63 `onUpFromFirstItem={focusHeader}`: the tab key
        // then switches tabs again.
        let (text, _) = drive(keys(&[KeyCode::Tab, KeyCode::Down, KeyCode::Up, KeyCode::Tab]));
        assert!(text.contains("No custom commands found"), "canvas=\n{text}");
        // While the list has focus the tab key does not switch.
        let (text, _) = drive(keys(&[KeyCode::Tab, KeyCode::Down, KeyCode::Tab]));
        assert!(text.contains("Browse default commands:"), "canvas=\n{text}");
    }

    #[test]
    fn help_v2_escape_closes_once_from_the_list_and_from_the_tab_row() {
        // CC's help:dismiss (HelpV2.tsx:36) takes Esc from both; from the
        // list the port's Select cancel (Commands.tsx:59) takes it first
        // (module doc), with the same single close.
        let (_, closes) = drive(keys(&[KeyCode::Tab, KeyCode::Down, KeyCode::Esc]));
        assert_eq!(closes, 1);
        let (_, closes) = drive(keys(&[KeyCode::Esc]));
        assert_eq!(closes, 1);
    }

    #[test]
    fn help_v2_first_ctrl_c_shows_the_exit_hint() {
        // CC :127-133: the exit hook's pending state replaces the hint. The
        // frame right after the key: the hint lapses after the double-press
        // window.
        let (frames, closes) = drive_frames(vec![vec![ctrl('c')]]);
        let text = &frames[0];
        assert!(text.contains("Press Ctrl-C again to exit"), "canvas=\n{text}");
        assert!(!text.contains("to cancel"), "canvas=\n{text}");
        assert_eq!(closes, 0);
    }

    #[test]
    fn help_v2_second_ctrl_c_closes_help_not_the_app() {
        // CC :37 `useExitOnCtrlCDWithKeybindings(close)`: the second press
        // runs `close`, and the app keeps running (drive_frames panics if
        // the render loop ends).
        let (_, closes) = drive(vec![vec![ctrl('c')], vec![ctrl('c')]]);
        assert_eq!(closes, 1);
    }

    #[test]
    fn help_v2_empty_tab_still_takes_focus_off_the_tab_row() {
        // CC Commands.tsx:26 calls useTabHeaderFocus above its empty-message
        // branch: ↓ blurs the tab row with no list to hand it back, so the
        // tab key no longer switches.
        let (text, _) = drive(keys(&[KeyCode::Tab, KeyCode::Tab, KeyCode::Down, KeyCode::Tab]));
        assert!(text.contains("No custom commands found"), "canvas=\n{text}");
    }
}
