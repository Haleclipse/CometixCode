//! Maps to: CC hooks/useExitOnCtrlCD.ts + useExitOnCtrlCDWithKeybindings.ts

use super::use_double_press::{DoublePressState, use_double_press};
use crate::keybindings::keybinding_context::KeybindingRuntime;
use crate::keybindings::types::ContextName;
use crate::keybindings::use_keybinding::{KeybindingHandlers, use_keybindings};
use iocraft::prelude::*;

/// Maps to: CC `hooks/useExitOnCtrlCD.ts` `ExitState`.
#[derive(Clone, Copy)]
pub struct ExitState {
    pub ctrl_c: DoublePressState,
    pub ctrl_d: DoublePressState,
    should_exit: State<bool>,
}

/// Maps to: CC `hooks/useExitOnCtrlCD.ts` `ExitState`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExitKeyState {
    pub pending: bool,
    pub key_name: Option<&'static str>,
}

impl ExitState {
    pub fn hint(self) -> Option<&'static str> {
        exit_hint_from_key_name(self.key_name())
    }

    pub fn pending(self) -> bool {
        self.ctrl_c.is_pending() || self.ctrl_d.is_pending()
    }

    pub fn key_name(self) -> Option<&'static str> {
        if self.ctrl_c.is_pending() {
            Some("Ctrl-C")
        } else if self.ctrl_d.is_pending() {
            Some("Ctrl-D")
        } else {
            None
        }
    }

    pub fn should_exit(self) -> bool {
        self.should_exit.get()
    }

    /// `should_exit`, reset as it is read: CC `useDoublePress` calls its exit
    /// callback once per completed double press, not on every later render.
    pub fn take_should_exit(mut self) -> bool {
        let should_exit = self.should_exit.get();
        if should_exit {
            self.should_exit.set(false);
        }
        should_exit
    }

    pub fn clear(self) {
        self.ctrl_c.clear();
        self.ctrl_d.clear();
    }
}

impl ExitKeyState {
    pub fn hint(self) -> Option<&'static str> {
        exit_hint_from_key_name(self.key_name)
    }
}

pub fn use_exit(hooks: &mut Hooks) -> ExitState {
    let ctrl_c = use_double_press(hooks);
    let ctrl_d = use_double_press(hooks);
    let mut should_exit = hooks.use_state(|| false);

    if ctrl_c.take_triggered() || ctrl_d.take_triggered() {
        should_exit.set(true);
    }

    ExitState {
        ctrl_c,
        ctrl_d,
        should_exit,
    }
}

/// Maps to: CC `hooks/useExitOnCtrlCDWithKeybindings.ts#useExitOnCtrlCDWithKeybindings`.
///
/// This retained adapter registers `app:interrupt` / `app:exit` through the
/// shared runtime and calls `useApp().exit()` on the second press. The default
/// Ctrl+C/Ctrl+D keys remain non-rebindable, while assigning either action to
/// an additional key works like CC.
///
/// Presses are counted per action, as in CC: every key bound to `app:exit`
/// (Ctrl+C and Ctrl+D here, `default_bindings.rs`) presses the same counter,
/// so Ctrl+C then Ctrl+D is a double press.
///
/// Cometix-specific deviation (product requirement — skip in parity audits):
/// the hint names the key that was pressed (`exit_press_target`), where CC
/// names the action's key — "Ctrl-C" for `app:interrupt`, "Ctrl-D" for
/// `app:exit` — which would read "Press Ctrl-D again" after a Ctrl+C.
pub fn use_exit_on_ctrl_cd_with_keybindings(hooks: &mut Hooks, is_active: bool) -> ExitKeyState {
    use_exit_on_ctrl_cd_with_keybindings_on_exit(hooks, is_active, None)
}

/// CC's `onExit` override (`useExitOnCtrlCDWithKeybindings(onExit?)`).
pub type ExitHandler = std::sync::Arc<dyn Fn() + Send + Sync>;

/// Maps to: CC `useExitOnCtrlCDWithKeybindings(onExit, …, isActive)` with an
/// `onExit` in place of `useApp().exit()` on the second press. The keys are
/// still taken: a no-op `onExit` (ThemePicker's `skipExitHandling`) arms the
/// "again to exit" hint and then does nothing.
pub fn use_exit_on_ctrl_cd_with_keybindings_on_exit(
    hooks: &mut Hooks,
    is_active: bool,
    on_exit: Option<ExitHandler>,
) -> ExitKeyState {
    let ctrl_c = use_double_press(hooks);
    let ctrl_d = use_double_press(hooks);
    // The name of the key each counter's pending press came from.
    let ctrl_c_key = hooks.use_state(|| "Ctrl-C");
    let ctrl_d_key = hooks.use_state(|| "Ctrl-D");
    let mut app = hooks.use_app();

    if ctrl_c.take_triggered() || ctrl_d.take_triggered() {
        match on_exit {
            Some(on_exit) => on_exit(),
            None => app.exit(),
        }
    }

    let runtime = hooks
        .try_use_context::<KeybindingRuntime>()
        .map(|runtime| runtime.clone());
    let press = move |runtime: Option<KeybindingRuntime>, action_is_exit: bool| {
        move || {
            let keys = runtime.as_ref().and_then(KeybindingRuntime::dispatching_keys);
            let (counter, mut key) = if action_is_exit {
                (ctrl_d, ctrl_d_key)
            } else {
                (ctrl_c, ctrl_c_key)
            };
            key.set(exit_key_name(keys.as_deref(), action_is_exit));
            counter.press();
            true
        }
    };
    let handlers: KeybindingHandlers = vec![
        (
            "app:interrupt".to_string(),
            Box::new(press(runtime.clone(), false)),
        ),
        ("app:exit".to_string(), Box::new(press(runtime.clone(), true))),
    ];
    use_keybindings(hooks, runtime, handlers, ContextName::Global, move || {
        is_active
    });

    let key_name = if ctrl_c.is_pending() {
        Some(ctrl_c_key.get())
    } else if ctrl_d.is_pending() {
        Some(ctrl_d_key.get())
    } else {
        None
    };

    ExitKeyState {
        pending: key_name.is_some(),
        key_name,
    }
}

fn exit_hint_from_key_name(key_name: Option<&'static str>) -> Option<&'static str> {
    match key_name? {
        "Ctrl-C" => Some("Press Ctrl-C again to exit"),
        "Ctrl-D" => Some("Press Ctrl-D again to exit"),
        other => Some(intern(format!("Press {other} again to exit"))),
    }
}

/// The key name a press's hint uses: CC's spelling for Ctrl+C and Ctrl+D,
/// whichever action they are bound to; `chord_to_string`'s for another key.
/// Without the pressed keys (a handler called outside key dispatch) it is
/// CC's name for the action.
fn exit_key_name(
    keys: Option<&[crate::keybindings::types::ParsedKeystroke]>,
    action_is_exit: bool,
) -> &'static str {
    use crate::keybindings::parser::{chord_to_string, parse_keystroke};
    match keys {
        None | Some([]) if action_is_exit => "Ctrl-D",
        None | Some([]) => "Ctrl-C",
        Some([key]) if *key == parse_keystroke("ctrl+c") => "Ctrl-C",
        Some([key]) if *key == parse_keystroke("ctrl+d") => "Ctrl-D",
        Some(keys) => intern(chord_to_string(keys)),
    }
}

/// A `'static` copy of `text`, allocated once per distinct text. The texts
/// are the names of keys bound to `app:interrupt` / `app:exit` and their
/// hints, so the set stays as small as the user's bindings.
fn intern(text: String) -> &'static str {
    use std::collections::HashSet;
    use std::sync::{LazyLock, Mutex};
    static TEXTS: LazyLock<Mutex<HashSet<&'static str>>> =
        LazyLock::new(|| Mutex::new(HashSet::new()));
    let mut texts = TEXTS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(existing) = texts.get(text.as_str()) {
        return existing;
    }
    let leaked: &'static str = Box::leak(text.into_boxed_str());
    texts.insert(leaked);
    leaked
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keybindings::parser::{parse_chord, parse_keystroke};
    use futures::StreamExt;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn exit_key_name_follows_the_pressed_key() {
        let ctrl_c = [parse_keystroke("ctrl+c")];
        let ctrl_d = [parse_keystroke("ctrl+d")];
        let f6 = [parse_keystroke("f6")];
        // Ctrl+C and Ctrl+D keep CC's spelling whichever action they fire.
        assert_eq!(exit_key_name(Some(&ctrl_c), true), "Ctrl-C");
        assert_eq!(exit_key_name(Some(&ctrl_c), false), "Ctrl-C");
        assert_eq!(exit_key_name(Some(&ctrl_d), true), "Ctrl-D");
        // Another key is named as bound.
        assert_eq!(exit_key_name(Some(&f6), true), "f6");
        // Outside key dispatch: CC's name for the action.
        assert_eq!(exit_key_name(None, true), "Ctrl-D");
        assert_eq!(exit_key_name(None, false), "Ctrl-C");
        assert_eq!(exit_hint_from_key_name(Some("f6")), Some("Press f6 again to exit"));
    }

    #[derive(Default, Props)]
    struct ExitProbeProps {
        exits: Option<Arc<AtomicUsize>>,
    }

    #[component]
    fn ExitProbe(props: &ExitProbeProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let exits = props.exits.clone().unwrap_or_default();
        let on_exit: ExitHandler = {
            let exits = exits.clone();
            Arc::new(move || {
                exits.fetch_add(1, Ordering::SeqCst);
            })
        };
        let state = use_exit_on_ctrl_cd_with_keybindings_on_exit(&mut hooks, true, Some(on_exit));
        let mut keys = hooks.use_state(|| 0usize);
        hooks.use_terminal_events(move |event| {
            if matches!(event, TerminalEvent::Key(key) if key.kind == KeyEventKind::Press) {
                keys.set(keys.get() + 1);
            }
        });
        element! {
            Text(content: format!(
                "keys={} exits={} hint={}",
                keys.get(),
                exits.load(Ordering::SeqCst),
                state.hint().unwrap_or("none"),
            ))
        }
    }

    /// One key per frame under the default bindings plus F6 for `app:exit`;
    /// returns the frame after the last key.
    fn settle(keys_in_order: Vec<KeyEvent>) -> String {
        let mut bindings = crate::keybindings::default_bindings::default_bindings();
        bindings.push(crate::keybindings::types::ParsedBinding {
            chord: parse_chord("f6"),
            action: Some("app:exit".to_string()),
            context: ContextName::Global,
        });
        futures::executor::block_on(async move {
            let (keys, events) = async_channel::unbounded();
            let mut app = element! {
                ContextProvider(value: Context::owned(KeybindingRuntime::new(bindings))) {
                    ExitProbe(exits: Some(Arc::new(AtomicUsize::new(0))))
                }
            };
            let mut frames = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events)
                    .with_size(80, 3)
                    .with_ignore_ctrl_c(true),
            ));
            let mut sent = 0;
            let mut last = String::new();
            loop {
                let next = crate::utils::race(frames.next(), async {
                    futures_timer::Delay::new(std::time::Duration::from_secs(10)).await;
                    None
                })
                .await;
                let Some(canvas) = next else {
                    panic!("no frame after key {sent}; last:\n{last}");
                };
                last = canvas.to_string();
                if !last.contains(&format!("keys={sent} ")) {
                    continue;
                }
                let Some(key) = keys_in_order.get(sent) else {
                    break last;
                };
                keys.send(TerminalEvent::Key(key.clone())).await.unwrap();
                sent += 1;
            }
        })
    }

    #[test]
    fn the_exit_hint_names_the_key_that_was_pressed() {
        let ctrl = |c| {
            let mut key = KeyEvent::new(KeyEventKind::Press, KeyCode::Char(c));
            key.modifiers = KeyModifiers::CONTROL;
            key
        };
        let f6 = || KeyEvent::new(KeyEventKind::Press, KeyCode::F(6));
        // Ctrl+C is bound to app:exit and still reads Ctrl-C.
        assert!(settle(vec![ctrl('c')]).contains("exits=0 hint=Press Ctrl-C again to exit"));
        assert!(settle(vec![ctrl('c'), ctrl('c')]).contains("exits=1 "));
        // Counted per action: every key bound to app:exit completes the
        // double press another one started.
        assert!(settle(vec![ctrl('c'), ctrl('d')]).contains("exits=1 "));
        assert!(settle(vec![ctrl('d'), f6()]).contains("exits=1 "));
        // A key a user binds to app:exit is named as bound.
        assert!(settle(vec![f6()]).contains("exits=0 hint=Press f6 again to exit"));
    }
}
