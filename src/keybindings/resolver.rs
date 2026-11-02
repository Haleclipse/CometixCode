//! Maps to: CC `keybindings/resolver.ts` — pure binding resolution with the
//! chord state machine. All state (pending chord, active contexts) lives in
//! the caller; this module is pure functions.

use std::collections::{HashMap, HashSet};

use super::parser::chord_to_string;
use super::types::{ChordResolveResult, ContextName, ParsedBinding, ParsedKeystroke};

/// Maps to: CC `keystrokesEqual` — alt and meta are collapsed into one
/// logical modifier (legacy terminals can't distinguish them); super
/// (cmd/win) stays distinct (kitty keyboard protocol only).
pub fn keystrokes_equal(a: &ParsedKeystroke, b: &ParsedKeystroke) -> bool {
    a.key == b.key
        && a.ctrl == b.ctrl
        && a.shift == b.shift
        && (a.alt || a.meta) == (b.alt || b.meta)
        && a.super_key == b.super_key
}

/// Maps to: CC `chordPrefixMatches`.
fn chord_prefix_matches(prefix: &[ParsedKeystroke], binding: &ParsedBinding) -> bool {
    if prefix.len() >= binding.chord.len() {
        return false;
    }
    prefix
        .iter()
        .zip(binding.chord.iter())
        .all(|(a, b)| keystrokes_equal(a, b))
}

/// Maps to: CC `chordExactlyMatches`.
fn chord_exactly_matches(chord: &[ParsedKeystroke], binding: &ParsedBinding) -> bool {
    chord.len() == binding.chord.len()
        && chord
            .iter()
            .zip(binding.chord.iter())
            .all(|(a, b)| keystrokes_equal(a, b))
}

/// Maps to: CC `resolver.ts#getBindingDisplayText`.
pub fn get_binding_display_text(
    action: &str,
    context: &ContextName,
    bindings: &[ParsedBinding],
) -> Option<String> {
    bindings
        .iter()
        .rev()
        .find(|binding| binding.action.as_deref() == Some(action) && &binding.context == context)
        .map(|binding| chord_to_string(&binding.chord))
}

/// Maps to: CC `resolveKeyWithChordState` — the production resolver.
///
/// Differences in signature only: CC receives Ink's `(input, key)` pair and
/// builds the keystroke internally; the Rust caller passes an
/// already-normalized `ParsedKeystroke` (see `matcher.rs` for the
/// crossterm → keystroke bridge), or `None` when the event isn't a
/// resolvable key.
pub fn resolve_key_with_chord_state(
    current: Option<&ParsedKeystroke>,
    is_escape: bool,
    active_contexts: &HashSet<ContextName>,
    bindings: &[ParsedBinding],
    pending: Option<&[ParsedKeystroke]>,
) -> ChordResolveResult {
    // Cancel chord on escape.
    if is_escape && pending.is_some() {
        return ChordResolveResult::ChordCancelled;
    }

    let Some(current) = current else {
        if pending.is_some() {
            return ChordResolveResult::ChordCancelled;
        }
        return ChordResolveResult::None;
    };

    // Build the full chord sequence to test.
    let mut test_chord: Vec<ParsedKeystroke> = pending.map(|p| p.to_vec()).unwrap_or_default();
    test_chord.push(current.clone());

    // Filter bindings by active contexts.
    let context_bindings: Vec<&ParsedBinding> = bindings
        .iter()
        .filter(|binding| active_contexts.contains(&binding.context))
        .collect();

    // Prefix check: group by chord string so a later null-override shadows
    // the default it unbinds — otherwise null-unbinding `ctrl+x ctrl+k`
    // still makes `ctrl+x` enter chord-wait and the single-key binding on
    // the prefix never fires.
    let mut chord_winners: HashMap<String, Option<&str>> = HashMap::new();
    for binding in &context_bindings {
        if binding.chord.len() > test_chord.len() && chord_prefix_matches(&test_chord, binding) {
            chord_winners.insert(chord_to_string(&binding.chord), binding.action.as_deref());
        }
    }
    let has_longer_chords = chord_winners.values().any(|action| action.is_some());

    // If this keystroke could start a longer chord, prefer that (even if
    // there's an exact single-key match).
    if has_longer_chords {
        return ChordResolveResult::ChordStarted {
            pending: test_chord,
        };
    }

    // Exact match: last one wins (user bindings are appended after
    // defaults, so Vec order is the override order).
    let exact_match = context_bindings
        .iter()
        .rfind(|binding| chord_exactly_matches(&test_chord, binding));

    if let Some(binding) = exact_match {
        return match &binding.action {
            None => ChordResolveResult::Unbound,
            Some(action) => ChordResolveResult::Match {
                action: action.clone(),
            },
        };
    }

    if pending.is_some() {
        return ChordResolveResult::ChordCancelled;
    }
    ChordResolveResult::None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keybindings::parser::{parse_chord, parse_keystroke};

    fn binding(chord: &str, action: Option<&str>, context: ContextName) -> ParsedBinding {
        ParsedBinding {
            chord: parse_chord(chord),
            action: action.map(String::from),
            context,
        }
    }

    fn contexts(list: &[ContextName]) -> HashSet<ContextName> {
        list.iter().cloned().collect()
    }

    #[test]
    fn get_binding_display_text_uses_last_matching_binding() {
        let bindings = vec![
            binding("ctrl+t", Some("app:toggleTodos"), ContextName::Global),
            binding("ctrl+shift+t", Some("app:toggleTodos"), ContextName::Global),
            binding(
                "ctrl+t",
                Some("theme:toggleSyntaxHighlighting"),
                ContextName::ThemePicker,
            ),
        ];

        assert_eq!(
            get_binding_display_text("app:toggleTodos", &ContextName::Global, &bindings).as_deref(),
            Some("ctrl+shift+t")
        );
        assert_eq!(
            get_binding_display_text(
                "theme:toggleSyntaxHighlighting",
                &ContextName::ThemePicker,
                &bindings,
            )
            .as_deref(),
            Some("ctrl+t")
        );
        assert_eq!(
            get_binding_display_text("missing", &ContextName::Global, &bindings),
            None
        );
    }

    #[test]
    fn single_key_match_resolves_action_in_active_context() {
        let bindings = vec![binding("escape", Some("chat:cancel"), ContextName::Chat)];
        let result = resolve_key_with_chord_state(
            Some(&parse_keystroke("escape")),
            true,
            &contexts(&[ContextName::Chat, ContextName::Global]),
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
    fn inactive_context_bindings_do_not_match() {
        let bindings = vec![binding("escape", Some("help:dismiss"), ContextName::Help)];
        let result = resolve_key_with_chord_state(
            Some(&parse_keystroke("escape")),
            true,
            &contexts(&[ContextName::Chat, ContextName::Global]),
            &bindings,
            None,
        );
        assert_eq!(result, ChordResolveResult::None);
    }

    #[test]
    fn last_binding_wins_like_cc_user_override_order() {
        let bindings = vec![
            binding("ctrl+t", Some("app:toggleTodos"), ContextName::Global),
            binding("ctrl+t", Some("app:toggleTranscript"), ContextName::Global),
        ];
        let result = resolve_key_with_chord_state(
            Some(&parse_keystroke("ctrl+t")),
            false,
            &contexts(&[ContextName::Global]),
            &bindings,
            None,
        );
        assert_eq!(
            result,
            ChordResolveResult::Match {
                action: "app:toggleTranscript".into()
            }
        );
    }

    #[test]
    fn chord_sequence_walks_started_then_match() {
        let bindings = vec![binding(
            "ctrl+x ctrl+k",
            Some("chat:killAgents"),
            ContextName::Chat,
        )];
        let ctx = contexts(&[ContextName::Chat]);

        let step1 = resolve_key_with_chord_state(
            Some(&parse_keystroke("ctrl+x")),
            false,
            &ctx,
            &bindings,
            None,
        );
        let ChordResolveResult::ChordStarted { pending } = step1 else {
            panic!("expected chord_started, got {step1:?}");
        };

        let step2 = resolve_key_with_chord_state(
            Some(&parse_keystroke("ctrl+k")),
            false,
            &ctx,
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
    fn chord_prefers_longer_chord_over_exact_single_key_match() {
        let bindings = vec![
            binding("ctrl+x", Some("single:x"), ContextName::Global),
            binding("ctrl+x ctrl+k", Some("chord:xk"), ContextName::Global),
        ];
        let result = resolve_key_with_chord_state(
            Some(&parse_keystroke("ctrl+x")),
            false,
            &contexts(&[ContextName::Global]),
            &bindings,
            None,
        );
        assert!(matches!(result, ChordResolveResult::ChordStarted { .. }));
    }

    #[test]
    fn null_override_shadows_chord_so_prefix_single_key_fires() {
        // CC: null-unbinding `ctrl+x ctrl+k` must not leave `ctrl+x` in
        // ghost chord-wait; the single-key binding fires instead.
        let bindings = vec![
            binding("ctrl+x", Some("single:x"), ContextName::Global),
            binding("ctrl+x ctrl+k", Some("chord:xk"), ContextName::Global),
            binding("ctrl+x ctrl+k", None, ContextName::Global), // user unbind
        ];
        let result = resolve_key_with_chord_state(
            Some(&parse_keystroke("ctrl+x")),
            false,
            &contexts(&[ContextName::Global]),
            &bindings,
            None,
        );
        assert_eq!(
            result,
            ChordResolveResult::Match {
                action: "single:x".into()
            }
        );
    }

    #[test]
    fn escape_cancels_pending_chord() {
        let bindings = vec![binding(
            "ctrl+x ctrl+k",
            Some("chord:xk"),
            ContextName::Global,
        )];
        let pending = vec![parse_keystroke("ctrl+x")];
        let result = resolve_key_with_chord_state(
            Some(&parse_keystroke("escape")),
            true,
            &contexts(&[ContextName::Global]),
            &bindings,
            Some(&pending),
        );
        assert_eq!(result, ChordResolveResult::ChordCancelled);
    }

    #[test]
    fn invalid_key_mid_chord_cancels() {
        let bindings = vec![binding(
            "ctrl+x ctrl+k",
            Some("chord:xk"),
            ContextName::Global,
        )];
        let pending = vec![parse_keystroke("ctrl+x")];
        let result = resolve_key_with_chord_state(
            Some(&parse_keystroke("q")),
            false,
            &contexts(&[ContextName::Global]),
            &bindings,
            Some(&pending),
        );
        assert_eq!(result, ChordResolveResult::ChordCancelled);
    }

    #[test]
    fn alt_and_meta_collapse_but_super_stays_distinct() {
        let alt = parse_keystroke("alt+k");
        let meta = parse_keystroke("meta+k");
        let cmd = parse_keystroke("cmd+k");
        assert!(keystrokes_equal(&alt, &meta));
        assert!(!keystrokes_equal(&alt, &cmd));
    }

    #[test]
    fn explicit_unbind_swallows_event() {
        let bindings = vec![
            binding("ctrl+t", Some("app:toggleTodos"), ContextName::Global),
            binding("ctrl+t", None, ContextName::Global),
        ];
        let result = resolve_key_with_chord_state(
            Some(&parse_keystroke("ctrl+t")),
            false,
            &contexts(&[ContextName::Global]),
            &bindings,
            None,
        );
        assert_eq!(result, ChordResolveResult::Unbound);
    }
}
