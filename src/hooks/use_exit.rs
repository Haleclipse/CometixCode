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
    let handlers: KeybindingHandlers = vec![
        (
            "app:interrupt".to_string(),
            Box::new(move || {
                ctrl_c.press();
                true
            }),
        ),
        (
            "app:exit".to_string(),
            Box::new(move || {
                ctrl_d.press();
                true
            }),
        ),
    ];
    use_keybindings(hooks, runtime, handlers, ContextName::Global, move || {
        is_active
    });

    let key_name = if ctrl_c.is_pending() {
        Some("Ctrl-C")
    } else if ctrl_d.is_pending() {
        Some("Ctrl-D")
    } else {
        None
    };

    ExitKeyState {
        pending: key_name.is_some(),
        key_name,
    }
}

fn exit_hint_from_key_name(key_name: Option<&'static str>) -> Option<&'static str> {
    match key_name {
        Some("Ctrl-C") => Some("Press Ctrl-C again to exit"),
        Some("Ctrl-D") => Some("Press Ctrl-D again to exit"),
        _ => None,
    }
}
