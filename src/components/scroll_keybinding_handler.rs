//! Maps to: CC `components/ScrollKeybindingHandler.tsx`.
//!
//! Wires scroll actions and modal pager input to an imperative
//! `ScrollBoxHandle`, alongside the official selection and wheel-acceleration
//! decision helpers.

use crate::keybindings::types::ContextName;
use crate::keybindings::use_keybinding::use_keybinding;
use crate::utils::process_env::JsTruthy;
use iocraft::prelude::*;

const WHEEL_ACCEL_WINDOW_MS: f64 = 40.0;
const WHEEL_ACCEL_STEP: f64 = 0.3;
const WHEEL_ACCEL_MAX: f64 = 6.0;
const WHEEL_BOUNCE_GAP_MAX_MS: f64 = 200.0;
const WHEEL_MODE_STEP: f64 = 15.0;
const WHEEL_MODE_CAP: f64 = 15.0;
const WHEEL_MODE_RAMP: f64 = 3.0;
const WHEEL_MODE_IDLE_DISENGAGE_MS: f64 = 1500.0;
const WHEEL_DECAY_HALFLIFE_MS: f64 = 150.0;
const WHEEL_DECAY_STEP: f64 = 5.0;
const WHEEL_BURST_MS: f64 = 5.0;
const WHEEL_DECAY_GAP_MS: f64 = 80.0;
const WHEEL_DECAY_CAP_SLOW: f64 = 3.0;
const WHEEL_DECAY_CAP_FAST: f64 = 6.0;
const WHEEL_DECAY_IDLE_MS: f64 = 500.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScrollKey {
    pub wheel_up: bool,
    pub wheel_down: bool,
    pub left_arrow: bool,
    pub right_arrow: bool,
    pub up_arrow: bool,
    pub down_arrow: bool,
    pub home: bool,
    pub end: bool,
    pub page_up: bool,
    pub page_down: bool,
    pub shift: bool,
    pub meta: bool,
    pub super_key: bool,
    pub ctrl: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusMove {
    Left,
    Right,
    Up,
    Down,
    LineStart,
    LineEnd,
}

impl FocusMove {
    pub fn as_official_str(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
            Self::Up => "up",
            Self::Down => "down",
            Self::LineStart => "lineStart",
            Self::LineEnd => "lineEnd",
        }
    }
}

/// Maps to: CC `ScrollKeybindingHandler.tsx#shouldClearSelectionOnKey`.
pub fn should_clear_selection_on_key(key: ScrollKey) -> bool {
    if key.wheel_up || key.wheel_down {
        return false;
    }
    let is_nav = key.left_arrow
        || key.right_arrow
        || key.up_arrow
        || key.down_arrow
        || key.home
        || key.end
        || key.page_up
        || key.page_down;
    if is_nav && (key.shift || key.meta || key.super_key) {
        return false;
    }
    true
}

/// Maps to: CC `ScrollKeybindingHandler.tsx#selectionFocusMoveForKey`.
pub fn selection_focus_move_for_key(key: ScrollKey) -> Option<FocusMove> {
    if !key.shift || key.meta {
        return None;
    }
    if key.left_arrow {
        return Some(FocusMove::Left);
    }
    if key.right_arrow {
        return Some(FocusMove::Right);
    }
    if key.up_arrow {
        return Some(FocusMove::Up);
    }
    if key.down_arrow {
        return Some(FocusMove::Down);
    }
    if key.home {
        return Some(FocusMove::LineStart);
    }
    if key.end {
        return Some(FocusMove::LineEnd);
    }
    None
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WheelAccelState {
    pub time: f64,
    pub mult: f64,
    pub dir: i8,
    pub xterm_js: bool,
    pub frac: f64,
    pub base: f64,
    pub pending_flip: bool,
    pub wheel_mode: bool,
    pub burst_count: u32,
}

/// Maps to: CC `ScrollKeybindingHandler.tsx#readScrollSpeedBase` parse/clamp.
pub fn read_scroll_speed_base_from_value(raw: Option<&str>) -> f64 {
    let Some(raw) = raw else {
        return 1.0;
    };
    let Ok(n) = raw.parse::<f64>() else {
        return 1.0;
    };
    if n.is_nan() || n <= 0.0 {
        1.0
    } else {
        n.min(20.0)
    }
}

/// Maps to: CC `ScrollKeybindingHandler.tsx#readScrollSpeedBase`.
pub fn read_scroll_speed_base() -> f64 {
    read_scroll_speed_base_from_value(
        crate::utils::process_env::var("CLAUDE_CODE_SCROLL_SPEED")
            .truthy()
            .as_deref(),
    )
}

/// Maps to: CC `ScrollKeybindingHandler.tsx#initWheelAccel`.
pub fn init_wheel_accel(xterm_js: bool, base: f64) -> WheelAccelState {
    WheelAccelState {
        time: 0.0,
        mult: base,
        dir: 0,
        xterm_js,
        frac: 0.0,
        base,
        pending_flip: false,
        wheel_mode: false,
        burst_count: 0,
    }
}

/// Maps to: CC `ScrollKeybindingHandler.tsx#computeWheelStep`.
pub fn compute_wheel_step(state: &mut WheelAccelState, dir: i8, now: f64) -> i64 {
    debug_assert!(dir == 1 || dir == -1);
    if !state.xterm_js {
        if state.wheel_mode && now - state.time > WHEEL_MODE_IDLE_DISENGAGE_MS {
            state.wheel_mode = false;
            state.burst_count = 0;
            state.mult = state.base;
        }

        if state.pending_flip {
            state.pending_flip = false;
            if dir != state.dir || now - state.time > WHEEL_BOUNCE_GAP_MAX_MS {
                state.dir = dir;
                state.time = now;
                state.mult = state.base;
                return state.mult.floor() as i64;
            }
            state.wheel_mode = true;
        }

        let gap = now - state.time;
        if dir != state.dir && state.dir != 0 {
            state.pending_flip = true;
            state.time = now;
            return 0;
        }
        state.dir = dir;
        state.time = now;

        if state.wheel_mode {
            if gap < WHEEL_BURST_MS {
                state.burst_count += 1;
                if state.burst_count >= 5 {
                    state.wheel_mode = false;
                    state.burst_count = 0;
                    state.mult = state.base;
                } else {
                    return 1;
                }
            } else {
                state.burst_count = 0;
            }
        }

        if state.wheel_mode {
            let m = 0.5_f64.powf(gap / WHEEL_DECAY_HALFLIFE_MS);
            let cap = WHEEL_MODE_CAP.max(state.base * 2.0);
            let next = 1.0 + (state.mult - 1.0) * m + WHEEL_MODE_STEP * m;
            state.mult = cap.min(next).min(state.mult + WHEEL_MODE_RAMP);
            return state.mult.floor() as i64;
        }

        if gap > WHEEL_ACCEL_WINDOW_MS {
            state.mult = state.base;
        } else {
            let cap = WHEEL_ACCEL_MAX.max(state.base * 2.0);
            state.mult = cap.min(state.mult + WHEEL_ACCEL_STEP);
        }
        return state.mult.floor() as i64;
    }

    let gap = now - state.time;
    let same_dir = dir == state.dir;
    state.time = now;
    state.dir = dir;
    if same_dir && gap < WHEEL_BURST_MS {
        return 1;
    }
    if !same_dir || gap > WHEEL_DECAY_IDLE_MS {
        state.mult = 2.0;
        state.frac = 0.0;
    } else {
        let m = 0.5_f64.powf(gap / WHEEL_DECAY_HALFLIFE_MS);
        let cap = if gap >= WHEEL_DECAY_GAP_MS {
            WHEEL_DECAY_CAP_SLOW
        } else {
            WHEEL_DECAY_CAP_FAST
        };
        state.mult = cap.min(1.0 + (state.mult - 1.0) * m + WHEEL_DECAY_STEP * m);
    }
    let total = state.mult + state.frac;
    let rows = total.floor();
    state.frac = total - rows;
    rows as i64
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SelectionPoint {
    pub row: i32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SelectionStateSnapshot {
    pub is_dragging: bool,
    pub anchor: Option<SelectionPoint>,
    pub focus: Option<SelectionPoint>,
}

/// Maps to: CC `ScrollKeybindingHandler.tsx#dragScrollDirection`.
pub fn drag_scroll_direction(
    sel: Option<SelectionStateSnapshot>,
    top: i32,
    bottom: i32,
    already_scrolling_dir: i8,
) -> i8 {
    let Some(sel) = sel else {
        return 0;
    };
    if !sel.is_dragging {
        return 0;
    }
    let (Some(anchor), Some(focus)) = (sel.anchor, sel.focus) else {
        return 0;
    };
    let want = if focus.row < top {
        -1
    } else if focus.row > bottom {
        1
    } else {
        0
    };
    if already_scrolling_dir != 0 {
        return if want == already_scrolling_dir {
            want
        } else {
            0
        };
    }
    if anchor.row < top || anchor.row > bottom {
        return 0;
    }
    want
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModalPagerAction {
    LineUp,
    LineDown,
    HalfPageUp,
    HalfPageDown,
    FullPageUp,
    FullPageDown,
    Top,
    Bottom,
}

impl ModalPagerAction {
    pub fn as_official_str(self) -> &'static str {
        match self {
            Self::LineUp => "lineUp",
            Self::LineDown => "lineDown",
            Self::HalfPageUp => "halfPageUp",
            Self::HalfPageDown => "halfPageDown",
            Self::FullPageUp => "fullPageUp",
            Self::FullPageDown => "fullPageDown",
            Self::Top => "top",
            Self::Bottom => "bottom",
        }
    }
}

/// Maps to: CC `ScrollKeybindingHandler.tsx#modalPagerAction`.
pub fn modal_pager_action(input: &str, key: ScrollKey) -> Option<ModalPagerAction> {
    if key.meta {
        return None;
    }
    if !key.ctrl && !key.shift {
        if key.up_arrow {
            return Some(ModalPagerAction::LineUp);
        }
        if key.down_arrow {
            return Some(ModalPagerAction::LineDown);
        }
        if key.home {
            return Some(ModalPagerAction::Top);
        }
        if key.end {
            return Some(ModalPagerAction::Bottom);
        }
    }
    if key.ctrl {
        if key.shift {
            return None;
        }
        return match input {
            "u" => Some(ModalPagerAction::HalfPageUp),
            "d" => Some(ModalPagerAction::HalfPageDown),
            "b" => Some(ModalPagerAction::FullPageUp),
            "f" => Some(ModalPagerAction::FullPageDown),
            "n" => Some(ModalPagerAction::LineDown),
            "p" => Some(ModalPagerAction::LineUp),
            _ => None,
        };
    }
    let Some(c) = input.chars().next() else {
        return None;
    };
    if input.chars().any(|ch| ch != c) {
        return None;
    }
    if c == 'G' || (c == 'g' && key.shift) {
        return Some(ModalPagerAction::Bottom);
    }
    if key.shift {
        return None;
    }
    match c {
        'g' => Some(ModalPagerAction::Top),
        'j' => Some(ModalPagerAction::LineDown),
        'k' => Some(ModalPagerAction::LineUp),
        ' ' => Some(ModalPagerAction::FullPageDown),
        'b' => Some(ModalPagerAction::FullPageUp),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScrollAction {
    PageUp,
    PageDown,
    LineUp,
    LineDown,
    HalfPageUp,
    HalfPageDown,
    FullPageUp,
    FullPageDown,
    Top,
    Bottom,
}

fn perform_scroll_action(
    mut handle: Ref<ScrollBoxHandle>,
    action: ScrollAction,
    wheel: &mut WheelAccelState,
) -> Option<bool> {
    let snapshot = handle.read();
    let viewport = i32::from(snapshot.get_viewport_height()).max(1);
    let content = i32::from(snapshot.get_scroll_height());
    let top = snapshot
        .get_scroll_top()
        .saturating_add(snapshot.get_pending_delta());
    drop(snapshot);
    if matches!(action, ScrollAction::LineUp | ScrollAction::LineDown) && content <= viewport {
        return None;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
        * 1000.0;
    let mut handle = handle.write();
    let sticky = match action {
        ScrollAction::PageUp | ScrollAction::HalfPageUp => {
            handle.scroll_to(top.saturating_sub((viewport / 2).max(1)));
            false
        }
        ScrollAction::PageDown | ScrollAction::HalfPageDown => {
            let target = top.saturating_add((viewport / 2).max(1));
            let max = (content - viewport).max(0);
            if target >= max {
                handle.scroll_to(max);
                handle.scroll_to_bottom();
                true
            } else {
                handle.scroll_to(target);
                false
            }
        }
        ScrollAction::FullPageUp => {
            handle.scroll_to(top.saturating_sub(viewport));
            false
        }
        ScrollAction::FullPageDown => {
            let target = top.saturating_add(viewport);
            let max = (content - viewport).max(0);
            if target >= max {
                handle.scroll_to(max);
                handle.scroll_to_bottom();
                true
            } else {
                handle.scroll_to(target);
                false
            }
        }
        ScrollAction::LineUp => {
            let step = compute_wheel_step(wheel, -1, now).max(1) as i32;
            handle.scroll_by(-step);
            false
        }
        ScrollAction::LineDown => {
            let step = compute_wheel_step(wheel, 1, now).max(1) as i32;
            handle.scroll_by(step);
            let max = (content - viewport).max(0);
            top.saturating_add(step) >= max
        }
        ScrollAction::Top => {
            handle.scroll_to_top();
            false
        }
        ScrollAction::Bottom => {
            handle.scroll_to((content - viewport).max(0));
            handle.scroll_to_bottom();
            true
        }
    };
    Some(sticky)
}

fn scroll_key_from_event(code: &KeyCode, modifiers: KeyModifiers) -> (String, ScrollKey) {
    let input = match code {
        KeyCode::Char(c) => c.to_string(),
        _ => String::new(),
    };
    (
        input,
        ScrollKey {
            left_arrow: matches!(code, KeyCode::Left),
            right_arrow: matches!(code, KeyCode::Right),
            up_arrow: matches!(code, KeyCode::Up),
            down_arrow: matches!(code, KeyCode::Down),
            home: matches!(code, KeyCode::Home),
            end: matches!(code, KeyCode::End),
            page_up: matches!(code, KeyCode::PageUp),
            page_down: matches!(code, KeyCode::PageDown),
            shift: modifiers.contains(KeyModifiers::SHIFT),
            meta: modifiers.contains(KeyModifiers::ALT),
            super_key: modifiers.contains(KeyModifiers::SUPER),
            ctrl: modifiers.contains(KeyModifiers::CONTROL),
            ..ScrollKey::default()
        },
    )
}

#[derive(Default, Props)]
pub struct ScrollKeybindingHandlerProps {
    pub is_active: bool,
    pub is_modal: bool,
    pub scroll_handle: Option<Ref<ScrollBoxHandle>>,
    pub on_scroll: Handler<(bool)>,
}

/// Maps to: CC `components/ScrollKeybindingHandler.tsx:420-535`
/// (`copyAndToast` and the `selection:copy` registration).
///
/// TODO(fullscreen-repl): replace this safe, unregistered placeholder when the
/// fullscreen REPL, selection context, clipboard transport, and notification
/// owner are implemented together. Main-screen native scrollback continues to
/// leave selection and copying to the terminal.
#[must_use]
pub const fn selection_copy_placeholder() -> bool {
    false
}

/// Maps to: CC `components/ScrollKeybindingHandler.tsx#ScrollKeybindingHandler`.
#[component]
pub fn ScrollKeybindingHandler(
    props: &ScrollKeybindingHandlerProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let wheel = hooks.use_state(|| {
        init_wheel_accel(
            crate::utils::process_env::var("TERM_PROGRAM").as_deref() == Some("vscode"),
            read_scroll_speed_base(),
        )
    });
    let runtime = hooks
        .try_use_context::<crate::keybindings::keybinding_context::KeybindingRuntime>()
        .map(|runtime| runtime.clone());
    let handle = props.scroll_handle;
    let bindings = [
        ("scroll:pageUp", ScrollAction::PageUp),
        ("scroll:pageDown", ScrollAction::PageDown),
        ("scroll:lineUp", ScrollAction::LineUp),
        ("scroll:lineDown", ScrollAction::LineDown),
        ("scroll:top", ScrollAction::Top),
        ("scroll:bottom", ScrollAction::Bottom),
        ("scroll:halfPageUp", ScrollAction::HalfPageUp),
        ("scroll:halfPageDown", ScrollAction::HalfPageDown),
        ("scroll:fullPageUp", ScrollAction::FullPageUp),
        ("scroll:fullPageDown", ScrollAction::FullPageDown),
    ];
    for (action_name, action) in bindings {
        let runtime = runtime.clone();
        let on_scroll = props.on_scroll.clone();
        let active = props.is_active && handle.is_some();
        use_keybinding(
            &mut hooks,
            runtime,
            action_name,
            ContextName::Scroll,
            move || active,
            move || {
                let Some(handle) = handle else {
                    return false;
                };
                let mut wheel = wheel;
                let result = {
                    let mut wheel_state = wheel.write();
                    perform_scroll_action(handle, action, &mut wheel_state)
                };
                let Some(sticky) = result else {
                    return false;
                };
                (on_scroll)(sticky);
                true
            },
        );
    }

    // Registered unconditionally, with the modal/handle tests moved inside the
    // callback: iocraft resolves hooks by call index, so subscribing only while
    // modal shifts every later hook when the flag or the handle changes, and
    // the next render panics with "Unexpected hook type!".
    {
        {
            let on_scroll = props.on_scroll.clone();
            let active = props.is_active;
            let is_modal = props.is_modal;
            let handle = props.scroll_handle;
            hooks.use_propagated_terminal_events(move |event| {
                if !active || !is_modal {
                    return;
                }
                let Some(handle) = handle else {
                    return;
                };
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
                let (input, key) = scroll_key_from_event(code, *modifiers);
                let Some(action) = modal_pager_action(&input, key) else {
                    return;
                };
                let action = match action {
                    ModalPagerAction::LineUp => ScrollAction::LineUp,
                    ModalPagerAction::LineDown => ScrollAction::LineDown,
                    ModalPagerAction::HalfPageUp => ScrollAction::HalfPageUp,
                    ModalPagerAction::HalfPageDown => ScrollAction::HalfPageDown,
                    ModalPagerAction::FullPageUp => ScrollAction::FullPageUp,
                    ModalPagerAction::FullPageDown => ScrollAction::FullPageDown,
                    ModalPagerAction::Top => ScrollAction::Top,
                    ModalPagerAction::Bottom => ScrollAction::Bottom,
                };
                let mut wheel_state = wheel;
                let result = {
                    let mut wheel_state_ref = wheel_state.write();
                    perform_scroll_action(handle, action, &mut wheel_state_ref)
                };
                if let Some(sticky) = result {
                    (on_scroll)(sticky);
                    event.stop_propagation();
                }
            });
        }
    }

    element! { View(width: 0u32, height: 0u32) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{StreamExt, stream};
    use std::time::Duration;

    #[component]
    fn ScrollHandlerHarness(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let handle = hooks.use_ref_default::<ScrollBoxHandle>();
        let runtime =
            crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings();
        element! {
            ContextProvider(value: Context::owned(runtime)) {
                View(width: 20u32, height: 6u32, flex_direction: FlexDirection::Column, overflow: Overflow::Hidden) {
                    ScrollKeybindingHandler(
                        is_active: true,
                        is_modal: false,
                        scroll_handle: Some(handle),
                    )
                    ScrollBox(
                        handle: Some(handle),
                        sticky_scroll: true,
                        keyboard_scroll: Some(false),
                    ) {
                        View(flex_direction: FlexDirection::Column) {
                            #((0..20usize).map(|index| element! {
                                Text(content: format!("row-{index:02}"))
                            }))
                        }
                    }
                }
            }
        }
    }

    fn canvas_text(canvas: &Canvas) -> String {
        (0..canvas.height())
            .map(|y| {
                (0..canvas.width())
                    .map(|x| {
                        canvas
                            .cell(x, y)
                            .and_then(|cell| cell.text())
                            .unwrap_or(" ")
                    })
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn render_scroll_handler(events: Vec<TerminalEvent>) -> Vec<String> {
        let canvases = futures::executor::block_on(async move {
            let mut app = element!(ScrollHandlerHarness);
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(stream::iter(events)).with_size(20, 6),
            ));
            let mut canvases = Vec::new();
            loop {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(100)).await;
                    None
                })
                .await;
                let Some(canvas) = next else { break };
                canvases.push(canvas);
                if canvases.len() >= 8 {
                    break;
                }
            }
            canvases
        });
        canvases.iter().map(canvas_text).collect()
    }

    #[test]
    fn scroll_key_selection_clear_and_focus_move_match_official() {
        assert!(!should_clear_selection_on_key(ScrollKey {
            wheel_up: true,
            ..ScrollKey::default()
        }));
        assert!(!should_clear_selection_on_key(ScrollKey {
            left_arrow: true,
            shift: true,
            ..ScrollKey::default()
        }));
        assert!(!should_clear_selection_on_key(ScrollKey {
            home: true,
            meta: true,
            ..ScrollKey::default()
        }));
        assert!(should_clear_selection_on_key(ScrollKey {
            left_arrow: true,
            ..ScrollKey::default()
        }));
        assert!(should_clear_selection_on_key(ScrollKey::default()));

        assert_eq!(
            selection_focus_move_for_key(ScrollKey {
                left_arrow: true,
                shift: true,
                ..ScrollKey::default()
            }),
            Some(FocusMove::Left)
        );
        assert_eq!(
            selection_focus_move_for_key(ScrollKey {
                home: true,
                shift: true,
                ..ScrollKey::default()
            }),
            Some(FocusMove::LineStart)
        );
        assert_eq!(
            selection_focus_move_for_key(ScrollKey {
                left_arrow: true,
                shift: true,
                meta: true,
                ..ScrollKey::default()
            }),
            None
        );
    }

    #[test]
    fn scroll_key_wheel_base_parse_and_init_match_official() {
        assert_eq!(read_scroll_speed_base_from_value(None), 1.0);
        assert_eq!(read_scroll_speed_base_from_value(Some("nope")), 1.0);
        assert_eq!(read_scroll_speed_base_from_value(Some("0")), 1.0);
        assert_eq!(read_scroll_speed_base_from_value(Some("30")), 20.0);
        assert_eq!(read_scroll_speed_base_from_value(Some("2.5")), 2.5);

        let state = init_wheel_accel(true, 3.0);
        assert!(state.xterm_js);
        assert_eq!(state.mult, 3.0);
        assert_eq!(state.base, 3.0);
        assert_eq!(state.dir, 0);
    }

    #[test]
    fn scroll_key_compute_wheel_step_native_tracks_bounce_and_modes() {
        let mut state = init_wheel_accel(false, 1.0);
        assert_eq!(compute_wheel_step(&mut state, 1, 100.0), 1);
        assert_eq!(compute_wheel_step(&mut state, 1, 120.0), 1);
        assert_eq!(compute_wheel_step(&mut state, -1, 130.0), 0);
        assert_eq!(state.pending_flip, true);
        let bounce_step = compute_wheel_step(&mut state, 1, 140.0);
        assert!(state.wheel_mode);
        assert!(
            bounce_step >= 4,
            "bounce_step={bounce_step} state={state:?}"
        );
        let next_step = compute_wheel_step(&mut state, 1, 260.0);
        assert!(
            next_step >= bounce_step,
            "next_step={next_step} bounce_step={bounce_step} state={state:?}"
        );
    }

    #[test]
    fn scroll_key_compute_wheel_step_xterm_uses_decay_and_fraction() {
        let mut state = init_wheel_accel(true, 1.0);
        assert_eq!(compute_wheel_step(&mut state, 1, 1000.0), 2);
        assert_eq!(compute_wheel_step(&mut state, 1, 1001.0), 1);
        let rows = compute_wheel_step(&mut state, 1, 1040.0);
        assert!((3..=6).contains(&rows), "rows={rows} state={state:?}");
    }

    #[test]
    fn scroll_key_drag_direction_matches_official_anchor_guards() {
        let sel = SelectionStateSnapshot {
            is_dragging: true,
            anchor: Some(SelectionPoint { row: 5 }),
            focus: Some(SelectionPoint { row: 1 }),
        };
        assert_eq!(drag_scroll_direction(Some(sel), 2, 8, 0), -1);
        let outside_anchor = SelectionStateSnapshot {
            anchor: Some(SelectionPoint { row: 0 }),
            ..sel
        };
        assert_eq!(drag_scroll_direction(Some(outside_anchor), 2, 8, 0), 0);
        assert_eq!(drag_scroll_direction(Some(outside_anchor), 2, 8, -1), -1);
        assert_eq!(drag_scroll_direction(Some(outside_anchor), 2, 8, 1), 0);
    }

    #[test]
    fn scroll_key_runtime_page_up_moves_the_retained_scroll_box() {
        let frames = render_scroll_handler(vec![TerminalEvent::Key(KeyEvent::new(
            KeyEventKind::Press,
            KeyCode::PageUp,
        ))]);
        let first = frames.first().expect("initial frame");
        let last = frames.last().expect("post-key frame");
        assert!(first.contains("row-19"), "first=\n{first}");
        assert!(!last.contains("row-19"), "last=\n{last}");
        assert!(last.contains("row-00"), "last=\n{last}");
    }

    #[test]
    fn scroll_key_modal_pager_action_matches_official_bindings() {
        assert_eq!(
            modal_pager_action(
                "",
                ScrollKey {
                    up_arrow: true,
                    ..ScrollKey::default()
                }
            ),
            Some(ModalPagerAction::LineUp)
        );
        assert_eq!(
            modal_pager_action(
                "u",
                ScrollKey {
                    ctrl: true,
                    ..ScrollKey::default()
                }
            ),
            Some(ModalPagerAction::HalfPageUp)
        );
        assert_eq!(
            modal_pager_action(
                "d",
                ScrollKey {
                    ctrl: true,
                    ..ScrollKey::default()
                }
            ),
            Some(ModalPagerAction::HalfPageDown)
        );
        assert_eq!(
            modal_pager_action("g", ScrollKey::default()),
            Some(ModalPagerAction::Top)
        );
        assert_eq!(
            modal_pager_action("ggg", ScrollKey::default()),
            Some(ModalPagerAction::Top)
        );
        assert_eq!(modal_pager_action("gG", ScrollKey::default()), None);
        assert_eq!(
            modal_pager_action(
                "g",
                ScrollKey {
                    shift: true,
                    ..ScrollKey::default()
                }
            ),
            Some(ModalPagerAction::Bottom)
        );
        assert_eq!(
            modal_pager_action("b", ScrollKey::default()),
            Some(ModalPagerAction::FullPageUp)
        );
        assert_eq!(
            modal_pager_action(
                "j",
                ScrollKey {
                    meta: true,
                    ..ScrollKey::default()
                }
            ),
            None
        );
    }
}
