//! Maps to: CC `hooks/useBlink.ts`.
//!
//! Synchronized blink derived from the shared animation clock. The clock only
//! ticks while enabled **and** the terminal is focused; when either gate is
//! off, callers see a permanently-visible phase (same as official
//! `if (!enabled || !focused) return [ref, true]`).

use iocraft::prelude::*;
use std::time::Duration;

/// Official `BLINK_INTERVAL_MS`.
pub const BLINK_INTERVAL_MS: u64 = 600;

/// Maps to: CC `useBlink(enabled, intervalMs?)` → `isVisible`.
///
/// Official also returns a viewport `ref` for Ink; iocraft's
/// `use_animation_frame` already pauses when the component is offscreen via
/// `use_terminal_viewport`, so only the visibility bool is exposed.
pub fn use_blink(hooks: &mut Hooks, enabled: bool) -> bool {
    use_blink_with_interval(hooks, enabled, BLINK_INTERVAL_MS)
}

/// Maps to: CC `useBlink(enabled, intervalMs)`.
pub fn use_blink_with_interval(hooks: &mut Hooks, enabled: bool, interval_ms: u64) -> bool {
    let focused = hooks.use_terminal_focus();
    // Official: `useAnimationFrame(enabled && focused ? intervalMs : null)`.
    let interval = if enabled && focused {
        Some(Duration::from_millis(interval_ms))
    } else {
        None
    };
    let frame = hooks.use_animation_frame(interval);
    blink_is_visible(enabled, focused, frame.time_ms, interval_ms)
}

/// Pure blink-phase helper (shared with unit tests).
pub(crate) fn blink_is_visible(
    enabled: bool,
    focused: bool,
    time_ms: u128,
    interval_ms: u64,
) -> bool {
    if !enabled || !focused {
        return true;
    }
    let interval = interval_ms.max(1) as u128;
    (time_ms / interval).is_multiple_of(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn use_blink_stays_visible_when_disabled_or_unfocused() {
        assert!(blink_is_visible(false, true, 600, BLINK_INTERVAL_MS));
        assert!(blink_is_visible(true, false, 600, BLINK_INTERVAL_MS));
        assert!(blink_is_visible(false, false, 1_200, BLINK_INTERVAL_MS));
    }

    #[test]
    fn use_blink_phase_matches_official_shared_clock() {
        assert!(blink_is_visible(true, true, 0, BLINK_INTERVAL_MS));
        assert!(blink_is_visible(true, true, 599, BLINK_INTERVAL_MS));
        assert!(!blink_is_visible(true, true, 600, BLINK_INTERVAL_MS));
        assert!(!blink_is_visible(true, true, 1_199, BLINK_INTERVAL_MS));
        assert!(blink_is_visible(true, true, 1_200, BLINK_INTERVAL_MS));
    }
}
