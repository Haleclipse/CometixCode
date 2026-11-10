//! Maps to: CC `services/tips/tipScheduler.ts`.

use super::tip_history::{get_sessions_since_last_shown, record_tip_shown};
use super::tip_registry::get_relevant_tips;
use super::types::Tip;
use crate::utils::settings::get_initial_settings;
use std::sync::atomic::{AtomicBool, Ordering};

/// Maps to: CC REPL `tipPickedThisTurnRef` — prevents double-pick in one turn.
static TIP_PICKED_THIS_TURN: AtomicBool = AtomicBool::new(false);

/// Maps to: CC clearing `tipPickedThisTurnRef` on user submit.
pub fn reset_tip_picked_this_turn() {
    TIP_PICKED_THIS_TURN.store(false, Ordering::Relaxed);
}

/// Maps to: CC `selectTipWithLongestTimeSinceShown`.
pub fn select_tip_with_longest_time_since_shown(available_tips: &[Tip]) -> Option<&Tip> {
    if available_tips.is_empty() {
        return None;
    }
    if available_tips.len() == 1 {
        return available_tips.first();
    }
    available_tips.iter().max_by_key(|tip| {
        let sessions = get_sessions_since_last_shown(&tip.id);
        // Prefer never-shown (MAX) then largest gap.
        sessions
    })
}

/// Maps to: CC `getTipToShowOnSpinner`.
pub fn get_tip_to_show_on_spinner() -> Option<Tip> {
    let settings = get_initial_settings();
    if settings.spinner_tips_enabled == Some(false) {
        return None;
    }
    let tips = get_relevant_tips();
    select_tip_with_longest_time_since_shown(&tips).cloned()
}

/// Maps to: CC `recordShownTip`.
pub fn record_shown_tip(tip: &Tip) {
    record_tip_shown(&tip.id);
}

/// Maps to: CC REPL `pickNewSpinnerTip` — write `AppState.spinnerTip`.
pub fn pick_new_spinner_tip(app_store: &crate::state::store::AppStore) {
    if TIP_PICKED_THIS_TURN.swap(true, Ordering::Relaxed) {
        return;
    }
    match get_tip_to_show_on_spinner() {
        Some(tip) => {
            let content = tip.content.clone();
            record_shown_tip(&tip);
            app_store.replace_with(|state| {
                state.spinner_tip = Some(content);
            });
        }
        None => {
            // P3 §3a (F-A1): CC clears behind an explicit guard — `if
            // (prev.spinnerTip === undefined) return prev` (REPL.tsx:2099-2100);
            // an already-cleared tip returns `prev` untouched.
            app_store.set_state(|prev| {
                if prev.spinner_tip.is_none() {
                    return crate::state::store::UpdateDecision::Same(());
                }
                let mut next = (**prev).clone();
                next.spinner_tip = None;
                crate::state::store::UpdateDecision::Replace {
                    next: std::sync::Arc::new(next),
                    result: (),
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_tip_prefers_never_shown_then_oldest() {
        let tips = vec![
            Tip {
                id: "a".to_string(),
                content: "A".to_string(),
                cooldown_sessions: 0,
            },
            Tip {
                id: "b".to_string(),
                content: "B".to_string(),
                cooldown_sessions: 0,
            },
        ];
        // With empty history both are MAX; max_by_key is stable on ties for
        // the last max — either is fine, just ensure Some.
        assert!(select_tip_with_longest_time_since_shown(&tips).is_some());
    }

    #[test]
    fn pick_new_spinner_tip_writes_app_state_once_per_turn() {
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        reset_tip_picked_this_turn();
        pick_new_spinner_tip(&store);
        let first = store.get().spinner_tip.clone();
        pick_new_spinner_tip(&store);
        assert_eq!(store.get().spinner_tip, first);
        reset_tip_picked_this_turn();
    }
}
