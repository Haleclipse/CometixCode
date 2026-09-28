//! Maps to: CC `keybindings/defaultBindings.ts` — the full default binding
//! table, loaded first; user keybindings.json entries are appended after
//! (last wins).
//!
//! Platform notes:
//! - IMAGE_PASTE_KEY: `alt+v` on Windows (ctrl+v is system paste), `ctrl+v`
//!   elsewhere — mirrored via `cfg!(windows)`.
//! - MODE_CYCLE_KEY: CC falls back to `meta+m` on Windows terminals without
//!   VT mode (Node/Bun version gates). crossterm always enables VT
//!   processing on Windows, so Cometix uses `shift+tab` unconditionally
//!   (documented divergence — the fallback path cannot occur here).
//!
//! Feature gates: CC's `KAIROS`, `TERMINAL_PANEL`, and `MESSAGE_ACTIONS`
//! blocks remain external-only exclusions. QUICK_SEARCH is available in this
//! tree and keeps its official cross-platform defaults. `VOICE_MODE` maps to
//! the `voice_mode` cargo feature.

use super::parser::parse_bindings;
use super::types::{ContextName, KeybindingBlock, ParsedBinding};

fn push_block(
    context: ContextName,
    entries: &[(&str, &str)],
    include_in_template: bool,
    out: &mut Vec<KeybindingBlock>,
) {
    out.push(KeybindingBlock {
        context,
        bindings: entries
            .iter()
            .map(|(keys, action)| ((*keys).to_string(), Some((*action).to_string())))
            .collect(),
        include_in_template,
    });
}

fn block(context: ContextName, entries: &[(&str, &str)], out: &mut Vec<KeybindingBlock>) {
    push_block(context, entries, true, out);
}

fn runtime_only_block(
    context: ContextName,
    entries: &[(&str, &str)],
    out: &mut Vec<KeybindingBlock>,
) {
    push_block(context, entries, false, out);
}

/// Raw, ordered owner for CC `DEFAULT_BINDINGS`.
pub fn default_binding_blocks() -> Vec<KeybindingBlock> {
    let image_paste_key = if cfg!(windows) { "alt+v" } else { "ctrl+v" };
    let mode_cycle_key = "shift+tab"; // see module docs: crossterm always has VT

    let mut bindings = Vec::new();

    block(
        ContextName::Global,
        &[
            // ctrl+c / ctrl+d use special double-press handling; defined so
            // the resolver can find them but non-rebindable (reserved_shortcuts.rs).
            // Cometix-specific deviation (product requirement — skip in parity
            // audits): CC 2.1.88 binds Ctrl+C to app:interrupt, which cancels
            // a running turn and rejects a permission dialog. Cometix keeps
            // 2.0.x's Ctrl+C: it only exits, on a double press, and Esc is
            // what interrupts. app:interrupt keeps its consumers for a key a
            // user binds to it.
            ("ctrl+c", "app:exit"),
            ("ctrl+d", "app:exit"),
            ("ctrl+l", "app:redraw"),
            ("ctrl+t", "app:toggleTodos"),
            ("ctrl+o", "app:toggleTranscript"),
            ("ctrl+shift+o", "app:toggleTeammatePreview"),
            ("ctrl+r", "history:search"),
            ("ctrl+shift+f", "app:globalSearch"),
            ("cmd+shift+f", "app:globalSearch"),
            ("ctrl+shift+p", "app:quickOpen"),
            ("cmd+shift+p", "app:quickOpen"),
        ],
        &mut bindings,
    );

    let chat_entries: Vec<(&str, &str)> = vec![
        ("escape", "chat:cancel"),
        // ctrl+x chord prefix avoids shadowing readline editing keys.
        ("ctrl+x ctrl+k", "chat:killAgents"),
        (mode_cycle_key, "chat:cycleMode"),
        ("meta+p", "chat:modelPicker"),
        ("meta+o", "chat:fastMode"),
        ("meta+t", "chat:thinkingToggle"),
        ("enter", "chat:submit"),
        ("up", "history:previous"),
        ("down", "history:next"),
        // Undo dual-binding: ctrl+_ (legacy \x1f) + ctrl+shift+- (kitty).
        ("ctrl+_", "chat:undo"),
        ("ctrl+shift+-", "chat:undo"),
        ("ctrl+x ctrl+e", "chat:externalEditor"),
        ("ctrl+g", "chat:externalEditor"),
        ("ctrl+s", "chat:stash"),
        (image_paste_key, "chat:imagePaste"),
        #[cfg(feature = "voice_mode")]
        ("space", "voice:pushToTalk"),
    ];
    block(ContextName::Chat, &chat_entries, &mut bindings);

    block(
        ContextName::Autocomplete,
        &[
            ("tab", "autocomplete:accept"),
            ("escape", "autocomplete:dismiss"),
            ("up", "autocomplete:previous"),
            ("down", "autocomplete:next"),
        ],
        &mut bindings,
    );

    block(
        ContextName::Settings,
        &[
            ("escape", "confirm:no"),
            ("up", "select:previous"),
            ("down", "select:next"),
            ("k", "select:previous"),
            ("j", "select:next"),
            ("ctrl+p", "select:previous"),
            ("ctrl+n", "select:next"),
            ("space", "select:accept"),
            ("enter", "settings:close"),
            ("/", "settings:search"),
            ("r", "settings:retry"),
        ],
        &mut bindings,
    );

    block(
        ContextName::Confirmation,
        &[
            ("y", "confirm:yes"),
            ("n", "confirm:no"),
            ("enter", "confirm:yes"),
            ("escape", "confirm:no"),
            ("up", "confirm:previous"),
            ("down", "confirm:next"),
            ("tab", "confirm:nextField"),
            ("space", "confirm:toggle"),
            ("shift+tab", "confirm:cycleMode"),
            ("ctrl+e", "confirm:toggleExplanation"),
            ("ctrl+d", "permission:toggleDebug"),
        ],
        &mut bindings,
    );
    // Agent confirmation actions project CC's component-local `s`/`e`
    // handlers through the runtime registry, but are not DEFAULT_BINDINGS.
    runtime_only_block(
        ContextName::Confirmation,
        &[("s", "agent:save"), ("e", "agent:saveAndEdit")],
        &mut bindings,
    );

    block(
        ContextName::Tabs,
        &[
            ("tab", "tabs:next"),
            ("shift+tab", "tabs:previous"),
            ("right", "tabs:next"),
            ("left", "tabs:previous"),
        ],
        &mut bindings,
    );

    block(
        ContextName::Transcript,
        &[
            ("ctrl+e", "transcript:toggleShowAll"),
            ("ctrl+c", "transcript:exit"),
            ("escape", "transcript:exit"),
            // q — pager convention (less, tmux copy-mode).
            ("q", "transcript:exit"),
        ],
        &mut bindings,
    );

    block(
        ContextName::HistorySearch,
        &[
            ("ctrl+r", "historySearch:next"),
            ("escape", "historySearch:accept"),
            ("tab", "historySearch:accept"),
            ("ctrl+c", "historySearch:cancel"),
            ("enter", "historySearch:execute"),
        ],
        &mut bindings,
    );

    block(
        ContextName::Task,
        &[
            ("ctrl+b", "task:background"),
            ("space", "task:close"),
            ("left", "task:back"),
            ("x", "task:stop"),
            ("f", "task:foreground"),
        ],
        &mut bindings,
    );

    block(
        ContextName::Custom("Teams".to_string()),
        &[
            ("up", "teams:previous"),
            ("down", "teams:next"),
            ("enter", "teams:accept"),
            ("left", "teams:back"),
            ("k", "teams:kill"),
            ("s", "teams:shutdown"),
            ("h", "teams:toggleVisibility"),
            ("shift+h", "teams:toggleAllVisibility"),
            ("p", "teams:promptOrPrune"),
        ],
        &mut bindings,
    );

    block(
        ContextName::ThemePicker,
        &[("ctrl+t", "theme:toggleSyntaxHighlighting")],
        &mut bindings,
    );

    block(
        ContextName::Scroll,
        &[
            ("pageup", "scroll:pageUp"),
            ("pagedown", "scroll:pageDown"),
            ("wheelup", "scroll:lineUp"),
            ("wheeldown", "scroll:lineDown"),
            ("ctrl+home", "scroll:top"),
            ("ctrl+end", "scroll:bottom"),
            ("ctrl+shift+c", "selection:copy"),
            ("cmd+c", "selection:copy"),
        ],
        &mut bindings,
    );

    block(
        ContextName::Help,
        &[("escape", "help:dismiss")],
        &mut bindings,
    );

    block(
        ContextName::Attachments,
        &[
            ("right", "attachments:next"),
            ("left", "attachments:previous"),
            ("backspace", "attachments:remove"),
            ("delete", "attachments:remove"),
            ("down", "attachments:exit"),
            ("escape", "attachments:exit"),
        ],
        &mut bindings,
    );

    block(
        ContextName::Footer,
        &[
            ("up", "footer:up"),
            ("ctrl+p", "footer:up"),
            ("down", "footer:down"),
            ("ctrl+n", "footer:down"),
            ("right", "footer:next"),
            ("left", "footer:previous"),
            ("enter", "footer:openSelected"),
            ("escape", "footer:clearSelection"),
        ],
        &mut bindings,
    );

    block(
        ContextName::MessageSelector,
        &[
            ("up", "messageSelector:up"),
            ("down", "messageSelector:down"),
            ("k", "messageSelector:up"),
            ("j", "messageSelector:down"),
            ("ctrl+p", "messageSelector:up"),
            ("ctrl+n", "messageSelector:down"),
            ("ctrl+up", "messageSelector:top"),
            ("shift+up", "messageSelector:top"),
            ("meta+up", "messageSelector:top"),
            ("shift+k", "messageSelector:top"),
            ("ctrl+down", "messageSelector:bottom"),
            ("shift+down", "messageSelector:bottom"),
            ("meta+down", "messageSelector:bottom"),
            ("shift+j", "messageSelector:bottom"),
            ("enter", "messageSelector:select"),
        ],
        &mut bindings,
    );

    block(
        ContextName::DiffDialog,
        &[
            ("escape", "diff:dismiss"),
            ("left", "diff:previousSource"),
            ("right", "diff:nextSource"),
            ("up", "diff:previousFile"),
            ("down", "diff:nextFile"),
            ("enter", "diff:viewDetails"),
        ],
        &mut bindings,
    );

    block(
        ContextName::ModelPicker,
        &[
            ("left", "modelPicker:decreaseEffort"),
            ("right", "modelPicker:increaseEffort"),
        ],
        &mut bindings,
    );

    block(
        ContextName::Select,
        &[
            ("up", "select:previous"),
            ("down", "select:next"),
            ("j", "select:next"),
            ("k", "select:previous"),
            ("ctrl+n", "select:next"),
            ("ctrl+p", "select:previous"),
            ("enter", "select:accept"),
            ("escape", "select:cancel"),
        ],
        &mut bindings,
    );
    // Native projections of component-local page/digit/tree handlers.
    runtime_only_block(
        ContextName::Select,
        &[
            ("pageup", "select:previousPage"),
            ("pagedown", "select:nextPage"),
            ("1", "select:index1"),
            ("2", "select:index2"),
            ("3", "select:index3"),
            ("4", "select:index4"),
            ("5", "select:index5"),
            ("6", "select:index6"),
            ("7", "select:index7"),
            ("8", "select:index8"),
            ("9", "select:index9"),
            ("right", "select:expand"),
            ("left", "select:collapse"),
        ],
        &mut bindings,
    );

    // L1 projection of `commands/copy/copy.tsx`'s component-local
    // `onKeyDown(e.key === 'w')`; production components remain action-driven.
    runtime_only_block(
        ContextName::Custom("CopyPicker".to_string()),
        &[("w", "copy:write")],
        &mut bindings,
    );

    block(
        ContextName::Plugin,
        &[("space", "plugin:toggle"), ("i", "plugin:install")],
        &mut bindings,
    );

    bindings
}

/// Maps to: CC `DEFAULT_BINDINGS` parsed by `parseBindings(...)`.
pub fn default_bindings() -> Vec<ParsedBinding> {
    parse_bindings(&default_binding_blocks())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keybindings::parser::parse_keystroke;
    use crate::keybindings::resolver::resolve_key_with_chord_state;
    use crate::keybindings::types::ChordResolveResult;
    use std::collections::HashSet;

    #[test]
    fn escape_resolves_chat_cancel_in_chat_context() {
        let bindings = default_bindings();
        let contexts: HashSet<_> = [ContextName::Chat, ContextName::Global].into();
        let result = resolve_key_with_chord_state(
            Some(&parse_keystroke("escape")),
            true,
            &contexts,
            &bindings,
            None,
        );
        assert_eq!(
            result,
            ChordResolveResult::Match {
                action: "chat:cancel".into()
            }
        );
    }

    #[test]
    fn ctrl_c_resolves_app_exit_globally() {
        // Cometix-specific deviation: CC 2.1.88 resolves app:interrupt here.
        let bindings = default_bindings();
        let contexts: HashSet<_> = [ContextName::Chat, ContextName::Global].into();
        let result = resolve_key_with_chord_state(
            Some(&parse_keystroke("ctrl+c")),
            false,
            &contexts,
            &bindings,
            None,
        );
        assert_eq!(
            result,
            ChordResolveResult::Match {
                action: "app:exit".into()
            }
        );
    }

    #[test]
    fn shift_tab_cycles_mode_in_chat() {
        let bindings = default_bindings();
        let contexts: HashSet<_> = [ContextName::Chat, ContextName::Global].into();
        let result = resolve_key_with_chord_state(
            Some(&parse_keystroke("shift+tab")),
            false,
            &contexts,
            &bindings,
            None,
        );
        assert_eq!(
            result,
            ChordResolveResult::Match {
                action: "chat:cycleMode".into()
            }
        );
    }

    #[test]
    fn ctrl_s_resolves_prompt_stash_in_chat() {
        let bindings = default_bindings();
        let contexts: HashSet<_> = [ContextName::Chat, ContextName::Global].into();
        let result = resolve_key_with_chord_state(
            Some(&parse_keystroke("ctrl+s")),
            false,
            &contexts,
            &bindings,
            None,
        );
        assert_eq!(
            result,
            ChordResolveResult::Match {
                action: "chat:stash".into()
            }
        );
    }

    #[test]
    fn ctrl_x_enters_chord_wait_for_kill_agents_and_external_editor() {
        let bindings = default_bindings();
        let contexts: HashSet<_> = [ContextName::Chat, ContextName::Global].into();
        let step1 = resolve_key_with_chord_state(
            Some(&parse_keystroke("ctrl+x")),
            false,
            &contexts,
            &bindings,
            None,
        );
        let ChordResolveResult::ChordStarted { pending } = step1 else {
            panic!("expected chord wait, got {step1:?}");
        };
        let step2 = resolve_key_with_chord_state(
            Some(&parse_keystroke("ctrl+k")),
            false,
            &contexts,
            &bindings,
            Some(&pending),
        );
        assert_eq!(
            step2,
            ChordResolveResult::Match {
                action: "chat:killAgents".into()
            }
        );
    }

    #[test]
    fn copy_picker_write_shortcut_projects_official_local_handler() {
        let bindings = default_bindings();
        let contexts: HashSet<_> = [
            ContextName::Custom("CopyPicker".to_string()),
            ContextName::Select,
            ContextName::Global,
        ]
        .into();
        let result = resolve_key_with_chord_state(
            Some(&parse_keystroke("w")),
            false,
            &contexts,
            &bindings,
            None,
        );
        assert_eq!(
            result,
            ChordResolveResult::Match {
                action: "copy:write".into()
            }
        );
    }

    #[test]
    fn transcript_context_escape_exits_instead_of_cancelling_chat() {
        let bindings = default_bindings();
        let contexts: HashSet<_> = [ContextName::Transcript, ContextName::Global].into();
        let result = resolve_key_with_chord_state(
            Some(&parse_keystroke("escape")),
            true,
            &contexts,
            &bindings,
            None,
        );
        assert_eq!(
            result,
            ChordResolveResult::Match {
                action: "transcript:exit".into()
            }
        );
    }
}
