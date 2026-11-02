//! Maps to: CC `components/permissions/rules/RecentDenialsTab.tsx`.

use crate::components::custom_select::select::SelectOptionLabel;
use crate::components::custom_select::{
    Select, SelectInputOptionMeta, SelectLayout, SelectOptionData, UseSelectInputOptions,
    UseSelectStateProps, use_select_input, use_select_state,
};
use crate::components::design_system::status_icon::{StatusIconStatus, status_icon_config};
use crate::utils::auto_mode_denials::{AutoModeDenial, get_auto_mode_denials};
use crate::utils::theme::Theme;
use indexmap::IndexSet;
use iocraft::prelude::*;
use std::collections::BTreeMap;
use std::sync::Arc;

/// Typed carrier for CC RecentDenialsTab.tsx:36-37's two insertion-ordered Sets.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecentDenialsState {
    pub approved: IndexSet<usize>,
    pub retry: IndexSet<usize>,
}

/// Maps to: CC `RecentDenialsTab.tsx#Props.onStateChange:16-20`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RecentDenialsSnapshot {
    pub approved: IndexSet<usize>,
    pub retry: IndexSet<usize>,
    pub denials: Arc<Vec<AutoModeDenial>>,
}

pub type RecentDenialsStateChange = Arc<dyn Fn(RecentDenialsSnapshot) + Send + Sync>;
pub type RecentDenialsHeaderFocusChange = Arc<dyn Fn(bool) + Send + Sync>;

#[derive(Default, Props)]
pub struct RecentDenialsTabProps<'a> {
    /// Fixture-only replacement for the mount snapshot. Production leaves
    /// this absent and reads utils/autoModeDenials.ts#getAutoModeDenials.
    pub denials_override: Option<Arc<Vec<AutoModeDenial>>>,
    /// Maps to: CC `useTabHeaderFocus().headerFocused`; Tabs context transport.
    pub header_focused: bool,
    /// Maps to: CC `useTabHeaderFocus().focusHeader`.
    pub on_focus_header: HandlerMut<'a, ()>,
    /// Maps to: CC `Props.onHeaderFocusChange`.
    pub on_header_focus_change: Option<RecentDenialsHeaderFocusChange>,
    /// Maps to: CC `Props.onStateChange`; Arc retains callback identity for
    /// the source effect dependency, and can write the parent's ref directly.
    pub on_state_change: Option<RecentDenialsStateChange>,
}

/// Maps to: CC `RecentDenialsTab.tsx#RecentDenialsTab:23-118`.
#[component]
pub fn RecentDenialsTab<'a>(
    props: &mut RecentDenialsTabProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = *hooks.use_context::<Theme>();
    // The deferred initialization reads actual mounted props, rather than
    // allowing a framework default-props construction to seed a fixture.
    // Once retained, concurrent store prepends cannot change these indices.
    let mut mounted_denials = hooks.use_state(|| None::<Arc<Vec<AutoModeDenial>>>);
    let denials = mounted_denials.read().clone().unwrap_or_else(|| {
        props
            .denials_override
            .clone()
            .unwrap_or_else(get_auto_mode_denials)
    });
    hooks.use_effect(
        {
            let denials = denials.clone();
            move || mounted_denials.set(Some(denials))
        },
        (),
    );
    let mut denial_state = hooks.use_state(|| Arc::new(RecentDenialsState::default()));
    let mut focused_idx = hooks.use_state(|| 0usize);

    // CC :59-77 raw useInput remains active even while the tab header owns
    // Select focus; only a non-empty denial snapshot gates the r shortcut.
    hooks.use_terminal_events({
        let mut denial_state = denial_state;
        let focused_idx = focused_idx;
        let is_active = !denials.is_empty();
        move |event| {
            // InputEvent.parseKey retains the character for Ctrl/Alt+r;
            // source deliberately ignores _key. Pasted input is one string.
            let is_retry = match event {
                TerminalEvent::Key(KeyEvent {
                    code: KeyCode::Char('r'),
                    kind,
                    ..
                }) => kind != KeyEventKind::Release,
                TerminalEvent::Paste(input) => input == "r",
                _ => false,
            };
            if !is_retry || !is_active {
                return;
            }
            let mut next = (**denial_state.read()).clone();
            let idx = focused_idx.get();
            if !next.retry.shift_remove(&idx) {
                next.retry.insert(idx);
            }
            // Every r press implies approval, including removal from retry.
            next.approved.insert(idx);
            denial_state.set(Arc::new(next));
        }
    });

    // CC :107-114 uses the canonical Select hooks and no onCancel callback.
    let values = (0..denials.len())
        .map(|idx| idx.to_string())
        .collect::<Vec<_>>();
    let state = use_select_state(
        &mut hooks,
        UseSelectStateProps {
            visible_option_count: Some(10.min(values.len())),
            values: values.clone(),
            default_value: None,
            focus_value: None,
        },
    );
    let events = use_select_input(
        &mut hooks,
        state,
        UseSelectInputOptions {
            is_disabled: props.header_focused || denials.is_empty(),
            has_on_up_from_first_item: true,
            option_metas: values
                .iter()
                .map(|value| SelectInputOptionMeta {
                    value: value.clone(),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        },
    );
    // Maps to: CC RecentDenialsTab.tsx:44-52#handleSelect.
    if let Some(value) = events.take_accepted()
        && let Ok(idx) = value.parse::<usize>()
    {
        let mut next = (**denial_state.read()).clone();
        if !next.approved.shift_remove(&idx) {
            next.approved.insert(idx);
        }
        denial_state.set(Arc::new(next));
    }
    // Maps to: CC RecentDenialsTab.tsx:54-56#handleFocus.
    if let Some(value) = state.navigation.take_focus_change()
        && let Ok(idx) = value.parse::<usize>()
    {
        focused_idx.set(idx);
    }
    if events.take_up_from_first_item() {
        (props.on_focus_header)(());
    }

    let header_callback = props.on_header_focus_change.clone();
    let header_callback_id = header_callback
        .as_ref()
        .map(|callback| Arc::as_ptr(callback) as *const () as usize);
    let header_focused = props.header_focused;
    // Maps to: CC :28-30 useEffect([headerFocused, onHeaderFocusChange]).
    hooks.use_effect(
        move || {
            if let Some(callback) = header_callback {
                callback(header_focused);
            }
        },
        (header_focused, header_callback_id),
    );
    let current = denial_state.read().clone();
    let on_state_change = props.on_state_change.clone();
    let callback_id = on_state_change
        .as_ref()
        .map(|callback| Arc::as_ptr(callback) as *const () as usize);
    let snapshot = RecentDenialsSnapshot {
        approved: current.approved.clone(),
        retry: current.retry.clone(),
        denials: denials.clone(),
    };
    // Maps to: CC :40-42 useEffect. Retain the Set carrier's allocation in
    // the effect closure so pointer reuse cannot erase a later state change.
    hooks.use_effect(
        move || {
            let _current = current;
            if let Some(callback) = on_state_change {
                callback(snapshot);
            }
        },
        (
            Arc::as_ptr(&denial_state.read()) as usize,
            Arc::as_ptr(&denials) as usize,
            callback_id,
        ),
    );

    if denials.is_empty() {
        return element! {
            Text(content: "No recent denials. Commands denied by the auto mode classifier will appear here.", color: theme.inactive, wrap: TextWrap::Wrap)
        }.into_any();
    }

    let current = denial_state.read();
    let mut option_labels = BTreeMap::<String, SelectOptionLabel>::new();
    let options = denials
        .iter()
        .enumerate()
        .map(|(idx, denial)| {
            let status = if current.approved.contains(&idx) {
                StatusIconStatus::Success
            } else {
                StatusIconStatus::Error
            };
            let suffix = if current.retry.contains(&idx) {
                " (retry)"
            } else {
                ""
            };
            let display = denial.display.clone();
            // CC :92-98 nested Text is one wrapping text flow. The canonical
            // StatusIcon mapping owns the glyph/color; child-local segments retain
            // its color and the retry suffix's ThemedText inactive color through
            // the Select root (ThemedText dimColor is not Ink's ANSI dim).
            let (icon, icon_color) = status_icon_config(status, &theme);
            let inactive_color = theme.inactive;
            option_labels.insert(idx.to_string(), {
                let mut icon_segment = StyledSegment::new(format!("{icon} "));
                icon_segment.styles.color = Some(icon_color.unwrap_or(inactive_color));
                let mut retry_segment = StyledSegment::new(suffix);
                retry_segment.styles.color = Some(inactive_color);
                vec![
                    icon_segment,
                    StyledSegment::new(display.clone()),
                    retry_segment,
                ]
            });
            SelectOptionData {
                // CC getTextContent visits nested Text children, but does not
                // invoke the StatusIcon function component during measurement.
                label: format!("{}{suffix}", denial.display),
                value: idx.to_string(),
                ..Default::default()
            }
        })
        .collect::<Vec<_>>();
    let navigation = state.navigation.snapshot();
    element! {
        View(flex_direction: FlexDirection::Column) {
            Text(content: "Commands recently denied by the auto mode classifier.", wrap: TextWrap::Wrap)
            View(margin_top: 1u32, flex_direction: FlexDirection::Row) {
                Select(
                    options,
                    option_labels,
                    focused_index: navigation.focused_index().unwrap_or(0),
                    selected_value: state.committed_value(),
                    visible_option_count: navigation.visible_option_count,
                    visible_from_index: navigation.visible_from_index,
                    is_disabled: props.header_focused,
                    layout: SelectLayout::Compact,
                )
            }
        }
    }.into_any()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::auto_mode_denials::record_auto_mode_denial;
    use crate::utils::theme;
    use futures::StreamExt;
    use std::sync::Mutex;
    use std::time::Duration;

    fn denial(display: &str) -> AutoModeDenial {
        AutoModeDenial {
            tool_name: "Bash".into(),
            display: display.into(),
            reason: "classifier reason".into(),
            timestamp: 1.0,
        }
    }

    #[derive(Default, Props)]
    struct DenialsHarnessProps {
        callback: Option<RecentDenialsStateChange>,
        header_callback: Option<RecentDenialsHeaderFocusChange>,
        denials_override: Option<Arc<Vec<AutoModeDenial>>>,
    }

    #[component]
    fn DenialsHarness(
        props: &DenialsHarnessProps,
        mut hooks: Hooks,
    ) -> impl Into<AnyElement<'static>> {
        let mut header_focused = hooks.use_state(|| false);
        let mut generation = hooks.use_state(|| 0usize);
        hooks.use_terminal_events(move |event| {
            if let TerminalEvent::Key(KeyEvent { code, kind, .. }) = event {
                if kind == KeyEventKind::Release {
                    return;
                }
                match code {
                    KeyCode::F(2) => generation.set(generation.get() + 1),
                    KeyCode::F(3) => header_focused.set(!header_focused.get()),
                    _ => {}
                }
            }
        });
        let callback = hooks
            .use_memo(
                {
                    let callback = props.callback.clone();
                    move || {
                        callback.map(|callback| {
                            Arc::new(move |snapshot| callback(snapshot)) as RecentDenialsStateChange
                        })
                    }
                },
                (
                    generation.get(),
                    props
                        .callback
                        .as_ref()
                        .map(|callback| Arc::as_ptr(callback) as *const () as usize),
                ),
            )
            .clone();
        element! {
            View(flex_direction: FlexDirection::Column) {
                Text(content: format!("generation={} header={}", generation.get(), header_focused.get()))
                RecentDenialsTab(
                    denials_override: props.denials_override.clone(),
                    header_focused: header_focused.get(),
                    on_focus_header: move |_| header_focused.set(true),
                    on_header_focus_change: props.header_callback.clone(),
                    on_state_change: callback,
                )
            }
        }
    }

    #[test]
    fn recent_denials_matches_official_empty_copy_and_status_icon_color() {
        // CC RecentDenialsTab.tsx:79-98: ThemedText dimColor uses inactive;
        // the original Kitty ANSI oracle has no SGR dim. A denial's
        // icon uses StatusIcon(error), overriding the focused Select color.
        // Actual figures :201/204 values are ✔/✘, not the doc-comment ✓/✗.
        let current_theme = *theme::current();
        let empty = element! {
            ContextProvider(value: Context::owned(current_theme)) { RecentDenialsTab() }
        }
        .render(Some(100));
        assert!(empty.to_string().contains("No recent denials."));
        assert_eq!(
            empty.resolved_text_style(0, 0).unwrap().color,
            Some(current_theme.inactive)
        );
        assert_eq!(
            empty.resolved_text_style(0, 0).unwrap().weight,
            Weight::Normal
        );
        assert!(!empty.resolved_text_style(0, 0).unwrap().dim);
        let canvas = element! {
            ContextProvider(value: Context::owned(current_theme)) {
                RecentDenialsTab(denials_override: Some(Arc::new(vec![denial("command-zero")])) )
            }
        }
        .render(Some(100));
        let text = canvas.to_string();
        assert!(text.contains("❯ 1. ✘ command-zero"), "canvas=\n{text}");
        let row = text
            .lines()
            .position(|line| line.contains("command-zero"))
            .unwrap();
        assert_eq!(
            canvas.resolved_text_style(5, row).unwrap().color,
            Some(current_theme.error)
        );
        assert_eq!(
            canvas.resolved_text_style(7, row).unwrap().color,
            Some(current_theme.suggestion)
        );
    }

    // Long labels still differ between measured height and rendered wrapping
    // under narrow constraints. User deferred research on 2026-09-13;
    // keep complete label, status-color and retry assertions for later study.
    #[test]
    #[ignore = "Deferred RecentDenials measurement/draw clipping; see research/layout-regressions-deferred-0913.md"]
    fn recent_denials_matches_official_wrapped_status_display_and_retry() {
        // CC :92-98 and Select :863: nested Text is one flow, including the
        // status icon and inactive retry suffix; long command labels must not clip.
        let display = "first-token second-token third-token fourth-token fifth-token";
        let current_theme = *theme::current();
        futures::executor::block_on(async {
            let (sender, receiver) = async_channel::unbounded::<TerminalEvent>();
            let mut app = element! {
                ContextProvider(value: Context::owned(current_theme)) {
                    RecentDenialsTab(denials_override: Some(Arc::new(vec![denial(display)])))
                }
            };
            let mut render = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(receiver).with_size(26, 20),
            ));
            let mut last_text = String::new();
            let drive = async {
                let mut sent_retry = false;
                while let Some(canvas) = render.next().await {
                    last_text = canvas.to_string();
                    let joined = last_text.split_whitespace().collect::<String>();
                    if !joined.contains(&display.split_whitespace().collect::<String>()) {
                        continue;
                    }
                    if !sent_retry {
                        let row = last_text
                            .lines()
                            .position(|line| line.contains('✘'))
                            .expect("error icon");
                        assert_eq!(
                            canvas.resolved_text_style(5, row).unwrap().color,
                            Some(current_theme.error)
                        );
                        sender
                            .send(TerminalEvent::Key(KeyEvent::new(
                                KeyEventKind::Press,
                                KeyCode::Char('r'),
                            )))
                            .await
                            .unwrap();
                        sent_retry = true;
                        continue;
                    }
                    if !joined.contains("(retry)") {
                        continue;
                    }
                    let row = last_text
                        .lines()
                        .position(|line| line.contains('✔'))
                        .expect("success icon");
                    assert_eq!(
                        canvas.resolved_text_style(5, row).unwrap().color,
                        Some(current_theme.success)
                    );
                    let mut retry_cells = Vec::new();
                    for y in row..canvas.height() {
                        for x in 0..canvas.width() {
                            if let Some(text) = canvas.cell(x, y).and_then(|cell| cell.text())
                                && canvas.resolved_text_style(x, y).is_some_and(|style| {
                                    style.color == Some(current_theme.inactive)
                                        && !style.dim
                                        && style.weight == Weight::Normal
                                })
                            {
                                retry_cells.push(text.to_owned());
                            }
                        }
                    }
                    assert!(
                        retry_cells.concat().contains("(retry)"),
                        "retry must use ThemedText inactive color without ANSI dim: {last_text}"
                    );
                    assert!(
                        last_text
                            .lines()
                            .filter(|line| line.contains("token"))
                            .count()
                            > 1,
                        "must wrap: {last_text}"
                    );
                    return true;
                }
                false
            };
            let completed = crate::utils::race(drive, async {
                futures_timer::Delay::new(Duration::from_secs(5)).await;
                false
            })
            .await;
            assert!(
                completed,
                "wrapped denial test timeout, canvas=\n{last_text}"
            );
        });
    }

    #[test]
    fn recent_denials_matches_official_set_order_retry_and_header_effects() {
        // CC :36-77: Sets preserve click order; retry always approves,
        // approval removal does not clear retry, and r remains active at header.
        let snapshots = Arc::new(Mutex::new(Vec::<RecentDenialsSnapshot>::new()));
        let headers = Arc::new(Mutex::new(Vec::<bool>::new()));
        let callback: RecentDenialsStateChange = {
            let snapshots = snapshots.clone();
            Arc::new(move |snapshot| snapshots.lock().unwrap().push(snapshot))
        };
        let header_callback: RecentDenialsHeaderFocusChange = {
            let headers = headers.clone();
            Arc::new(move |focused| headers.lock().unwrap().push(focused))
        };
        let cases = [
            (KeyCode::Char('2'), vec![1], vec![]),
            (KeyCode::Char('1'), vec![1, 0], vec![]),
            (KeyCode::Char('2'), vec![0], vec![]),
            (KeyCode::Char('2'), vec![0, 1], vec![]),
            // CC use-select-input.ts:255-285 numbers only onChange; they
            // do not focus the chosen row. Retry/Enter still target row 0.
            (KeyCode::Char('r'), vec![0, 1], vec![0]),
            (KeyCode::Enter, vec![1], vec![0]),
            (KeyCode::Char('r'), vec![1, 0], vec![]),
            (KeyCode::Char('r'), vec![1, 0], vec![0]),
        ];
        futures::executor::block_on(async {
            let (sender, receiver) = async_channel::unbounded::<TerminalEvent>();
            let mut app = element! {
                ContextProvider(value: Context::owned(*theme::current())) {
                    ContextProvider(value: Context::owned(crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings())) {
                    FocusScope(handle_keys: false) {
                    DenialsHarness(
                        callback: Some(callback),
                        header_callback: Some(header_callback),
                        denials_override: Some(Arc::new(vec![denial("command-zero"), denial("command-one")])),
                    )
                    }
                    }
                }
            };
            let mut render = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(receiver).with_size(100, 20),
            ));
            let mut last_text = String::new();
            let mut stage = "initial mount".to_string();
            let drive = async {
                while snapshots.lock().unwrap().is_empty() {
                    last_text = render.next().await.expect("initial canvas").to_string();
                }
                assert!(snapshots.lock().unwrap()[0].approved.is_empty());
                assert!(headers.lock().unwrap().contains(&false));
                for (key, approved, retry) in cases {
                    stage = format!("submit {key:?}, expect approved={approved:?} retry={retry:?}");
                    let count = snapshots.lock().unwrap().len();
                    sender
                        .send(TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, key)))
                        .await
                        .unwrap();
                    while snapshots.lock().unwrap().len() == count {
                        last_text = render.next().await.expect("changed canvas").to_string();
                    }
                    let snapshot = snapshots.lock().unwrap().last().unwrap().clone();
                    assert_eq!(
                        snapshot.approved.iter().copied().collect::<Vec<_>>(),
                        approved,
                        "key={key:?}"
                    );
                    assert_eq!(
                        snapshot.retry.iter().copied().collect::<Vec<_>>(),
                        retry,
                        "key={key:?}"
                    );
                }
                // A new callback alone retriggers the source effect.
                stage = "replace callback via F2".to_string();
                let count = snapshots.lock().unwrap().len();
                sender
                    .send(TerminalEvent::Key(KeyEvent::new(
                        KeyEventKind::Press,
                        KeyCode::F(2),
                    )))
                    .await
                    .unwrap();
                while snapshots.lock().unwrap().len() == count {
                    last_text = render
                        .next()
                        .await
                        .expect("callback-change canvas")
                        .to_string();
                }
                sender
                    .send(TerminalEvent::Key(KeyEvent::new(
                        KeyEventKind::Press,
                        KeyCode::F(3),
                    )))
                    .await
                    .unwrap();
                stage = "focus header via F3".to_string();
                while !headers.lock().unwrap().last().copied().unwrap_or(false) {
                    last_text = render.next().await.expect("header canvas").to_string();
                }
                stage = "retry while header focused".to_string();
                let count = snapshots.lock().unwrap().len();
                sender
                    .send(TerminalEvent::Key(KeyEvent::new(
                        KeyEventKind::Press,
                        KeyCode::Char('r'),
                    )))
                    .await
                    .unwrap();
                while snapshots.lock().unwrap().len() == count {
                    last_text = render
                        .next()
                        .await
                        .expect("header retry canvas")
                        .to_string();
                }
                assert!(snapshots.lock().unwrap().last().unwrap().retry.is_empty());
                // Return from the header, then navigate using a real arrow.
                // Unlike numeric selection, this must update source onFocus.
                sender
                    .send(TerminalEvent::Key(KeyEvent::new(
                        KeyEventKind::Press,
                        KeyCode::F(3),
                    )))
                    .await
                    .unwrap();
                stage = "return to content via F3".to_string();
                while headers.lock().unwrap().last().copied().unwrap_or(true) {
                    last_text = render
                        .next()
                        .await
                        .expect("content focus canvas")
                        .to_string();
                }
                sender
                    .send(TerminalEvent::Key(KeyEvent::new(
                        KeyEventKind::Press,
                        KeyCode::Down,
                    )))
                    .await
                    .unwrap();
                stage = "Down focus second row".to_string();
                while !last_text
                    .lines()
                    .any(|line| line.contains("❯ 2.") && line.contains("command-one"))
                {
                    last_text = render
                        .next()
                        .await
                        .expect("second-row focus canvas")
                        .to_string();
                }
                let count = snapshots.lock().unwrap().len();
                sender
                    .send(TerminalEvent::Key(KeyEvent::new(
                        KeyEventKind::Press,
                        KeyCode::Char('r'),
                    )))
                    .await
                    .unwrap();
                stage = "retry second focused row".to_string();
                while snapshots.lock().unwrap().len() == count {
                    last_text = render
                        .next()
                        .await
                        .expect("second-row retry canvas")
                        .to_string();
                }
                let snapshot = snapshots.lock().unwrap().last().unwrap().clone();
                assert_eq!(
                    snapshot.approved.iter().copied().collect::<Vec<_>>(),
                    vec![1, 0]
                );
                assert_eq!(snapshot.retry.iter().copied().collect::<Vec<_>>(), vec![1]);
                true
            };
            let completed = crate::utils::race(drive, async {
                futures_timer::Delay::new(Duration::from_secs(5)).await;
                false
            })
            .await;
            assert!(
                completed,
                "denials key test timeout at {stage}, canvas=\n{last_text}"
            );
        });
    }

    #[test]
    fn recent_denials_matches_official_mount_snapshot_after_live_prepend() {
        // CC :32-34: a concurrent record must not shift index zero while editing.
        record_auto_mode_denial(denial("mounted-command"));
        let mounted = get_auto_mode_denials();
        let snapshots = Arc::new(Mutex::new(Vec::<RecentDenialsSnapshot>::new()));
        let callback: RecentDenialsStateChange = {
            let snapshots = snapshots.clone();
            Arc::new(move |snapshot| snapshots.lock().unwrap().push(snapshot))
        };
        futures::executor::block_on(async {
            let (sender, receiver) = async_channel::unbounded::<TerminalEvent>();
            let mut app = element! {
                ContextProvider(value: Context::owned(*theme::current())) {
                    RecentDenialsTab(on_state_change: Some(callback))
                }
            };
            let mut render = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(receiver).with_size(100, 20),
            ));
            let mut last_text = String::new();
            let drive = async {
                while snapshots.lock().unwrap().is_empty() {
                    last_text = render.next().await.expect("initial canvas").to_string();
                }
                record_auto_mode_denial(denial("concurrent-command"));
                let count = snapshots.lock().unwrap().len();
                sender
                    .send(TerminalEvent::Key(KeyEvent::new(
                        KeyEventKind::Press,
                        KeyCode::Char('r'),
                    )))
                    .await
                    .unwrap();
                while snapshots.lock().unwrap().len() == count {
                    last_text = render.next().await.expect("retry canvas").to_string();
                }
                let snapshot = snapshots.lock().unwrap().last().unwrap().clone();
                assert!(Arc::ptr_eq(&mounted, &snapshot.denials));
                assert_eq!(snapshot.denials[0].display, "mounted-command");
                assert_eq!(get_auto_mode_denials()[0].display, "concurrent-command");
                assert!(snapshot.approved.contains(&0));
                assert!(snapshot.retry.contains(&0));
                assert!(!last_text.contains("concurrent-command"));
                true
            };
            let completed = crate::utils::race(drive, async {
                futures_timer::Delay::new(Duration::from_secs(5)).await;
                false
            })
            .await;
            assert!(
                completed,
                "mount snapshot test timeout, canvas=\n{last_text}"
            );
        });
    }
}
