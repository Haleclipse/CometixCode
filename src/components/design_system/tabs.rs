//! Maps to: CC `components/design-system/Tabs.tsx` header/body chrome.
//!
//! Canonical Tabs/Tab own selection, keybindings, header focus with its
//! `use_tab_header_focus` opt-in, and modal body layout for source-shaped
//! consumers. Existing screens that assemble `TabsHeader` themselves keep
//! their own focus state until they are migrated individually.

use crate::keybindings::keybinding_context::KeybindingRuntime;
use crate::keybindings::types::ContextName;
use crate::keybindings::use_keybinding::{KeybindingHandlers, use_keybindings};
use crate::utils::theme::Theme;
use iocraft::prelude::*;
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TabItem {
    pub id: String,
    pub title: String,
}

impl TabItem {
    pub fn new(id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
        }
    }
}

#[allow(dead_code)]
pub fn selected_tab_index(
    tabs: &[TabItem],
    selected_id: Option<&str>,
    default_index: usize,
) -> usize {
    if tabs.is_empty() {
        return 0;
    }

    selected_id
        .and_then(|id| tabs.iter().position(|tab| tab.id == id))
        .unwrap_or(default_index)
        .min(tabs.len() - 1)
}

#[allow(dead_code)]
pub fn next_tab_index(index: usize, tab_count: usize) -> usize {
    if tab_count == 0 {
        0
    } else {
        (index + 1) % tab_count
    }
}

#[allow(dead_code)]
pub fn previous_tab_index(index: usize, tab_count: usize) -> usize {
    if tab_count == 0 {
        0
    } else if index == 0 {
        tab_count - 1
    } else {
        index - 1
    }
}

/// Maps to: CC `Tabs`' two `useKeybindings` registrations. Screens retain
/// their controlled selected-tab state and provide the state transitions.
pub fn use_tabs_keybindings<Next, Previous>(
    hooks: &mut Hooks,
    is_active: bool,
    mut on_next: Next,
    mut on_previous: Previous,
) where
    Next: FnMut() + Send + 'static,
    Previous: FnMut() + Send + 'static,
{
    let runtime = hooks
        .try_use_context::<KeybindingRuntime>()
        .map(|runtime| runtime.clone());
    let handlers: KeybindingHandlers = vec![
        (
            "tabs:next".to_string(),
            Box::new(move || {
                on_next();
                true
            }),
        ),
        (
            "tabs:previous".to_string(),
            Box::new(move || {
                on_previous();
                true
            }),
        ),
    ];
    use_keybindings(hooks, runtime, handlers, ContextName::Tabs, move || {
        is_active
    });
}

#[derive(Default, Props)]
pub struct TabsHeaderProps {
    pub title: Option<String>,
    pub color: Option<Color>,
    pub tabs: Vec<TabItem>,
    pub selected_index: usize,
    pub header_focused: bool,
    pub hidden: bool,
    pub use_full_width: bool,
}

/// Official tab header shape:
/// title is bold + colored; current tab is bold. When the header owns focus and
/// a color is provided, the current tab uses colored background + inverse text.
/// When content owns focus, the current tab falls back to inverse video only.
#[component]
pub fn TabsHeader(props: &TabsHeaderProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let (terminal_width, _) = hooks.use_terminal_size();
    let selected_index = props.selected_index.min(props.tabs.len().saturating_sub(1));
    let title_width = props
        .title
        .as_deref()
        .filter(|title| !title.is_empty())
        .map(|title| UnicodeWidthStr::width(title) + 1)
        .unwrap_or(0);
    let tabs_width = props
        .tabs
        .iter()
        .map(|tab| UnicodeWidthStr::width(tab.title.as_str()) + 3)
        .sum::<usize>();
    let spacer_width = if props.use_full_width {
        usize::from(terminal_width).saturating_sub(title_width + tabs_width)
    } else {
        0
    };

    // Maps to Tabs.tsx:229-260: the row's gap=1 lies between actual
    // children, outside each Text's style and its highlighted padding.
    // Native empty layout cells do not reset all SGR attributes. Materialize
    // only these gap cells through existing neutral Text; no renderer changes.
    let mut children = Vec::new();
    if let Some(title) = &props.title {
        children.push(
            element! {
                View {
                    Text(content: title.clone(), color: props.color,
                        weight: Weight::Bold, wrap: TextWrap::NoWrap)
                }
            }
            .into_any(),
        );
    }
    for (index, tab) in props.tabs.iter().enumerate() {
        let is_current = index == selected_index;
        let has_color_cursor = props.color.is_some() && is_current && props.header_focused;
        children.push(
            element! {
                View {
                    Text(
                        content: format!(" {} ", tab.title),
                        background_color: if has_color_cursor { props.color } else { None },
                        color: if has_color_cursor { Some(theme.inverse_text) } else { None },
                        weight: if is_current { Weight::Bold } else { Weight::Normal },
                        inverse: is_current && !has_color_cursor,
                        wrap: TextWrap::NoWrap,
                    )
                }
            }
            .into_any(),
        );
    }
    if spacer_width > 0 {
        children.push(
            element! {
                Text(content: " ".repeat(spacer_width), wrap: TextWrap::NoWrap)
            }
            .into_any(),
        );
    }
    let children = children.into_iter().enumerate().flat_map(|(index, child)| {
        let gap = (index > 0).then(|| {
            element! {
                // Like Yoga gap, this one column cannot shrink with the labels.
                View(width: 1u32, flex_shrink: 0.0f32) {
                    Text(content: " ", wrap: TextWrap::NoWrap)
                }
            }
            .into_any()
        });
        gap.into_iter().chain(std::iter::once(child))
    });
    element! {
        Fragment {
            #((!props.hidden).then(|| element! {
                View(flex_direction: FlexDirection::Row, flex_shrink: 0.0f32) {
                    #(children)
                }
            }))
        }
    }
}

/// Maps to: CC `components/design-system/Tabs.tsx:294-298#TabProps`.
#[derive(Default, Props)]
pub struct TabProps {
    pub title: String,
    pub id: Option<String>,
    pub children: Vec<AnyElement<'static>>,
}
/// Maps to: CC `components/design-system/Tabs.tsx:300-312#Tab`.
#[component]
pub fn Tab<'a>(props: &'a mut TabProps, hooks: Hooks) -> impl Into<AnyElement<'a>> {
    let context = hooks
        .try_use_context::<TabsContextValue>()
        .map(|value| value.clone())
        .unwrap_or_default();
    if context.selected_tab.as_deref() != Some(props.id.as_deref().unwrap_or(&props.title)) {
        return element! {View}.into_any();
    }
    let inside_modal = crate::context::modal_context::use_is_inside_modal(&hooks);
    element!{View(width:context.width.map(|w|Size::Length(w.into())).unwrap_or(Size::Auto),flex_shrink:if inside_modal{0.0f32}else{1.0f32}){#(props.children.iter_mut())}}.into_any()
}
/// Maps to: CC `components/design-system/Tabs.tsx:58-76#TabsContextValue`.
/// The default, outside a Tabs, is CC's: content has focus and the header
/// callbacks do nothing. Screens that assemble their own header (Settings,
/// /permissions) still keep their own focus state.
#[derive(Clone, Default)]
struct TabsContextValue {
    selected_tab: Option<String>,
    width: Option<u16>,
    /// CC `headerFocused`.
    header_focused: bool,
    /// The Tabs' header-focus state behind CC `focusHeader`/`blurHeader`.
    header: Option<State<bool>>,
    /// The Tabs' opt-in count behind CC `registerOptIn`.
    opt_ins: Option<State<usize>>,
}

/// Maps to: CC `Tabs.tsx:330-334`, what `useTabHeaderFocus()` returns.
#[derive(Clone, Copy)]
pub struct TabHeaderFocus {
    pub header_focused: bool,
    header: Option<State<bool>>,
}

impl TabHeaderFocus {
    /// CC `focusHeader`.
    pub fn focus_header(&self) {
        if let Some(mut header) = self.header {
            header.set(true);
        }
    }

    /// CC `blurHeader`.
    pub fn blur_header(&self) {
        if let Some(mut header) = self.header {
            header.set(false);
        }
    }
}

/// CC `registerOptIn` and the cleanup it returns: counted while the guard
/// lives. iocraft exposes no unmount effect, so the caller keeps the guard
/// in `use_state` and its drop is the unmount (the `OverlayRegistration`
/// pattern). A Tabs that unmounts first leaves the write a no-op.
struct TabHeaderOptIn(Option<State<usize>>);

impl TabHeaderOptIn {
    fn register(mut opt_ins: Option<State<usize>>) -> Self {
        if let Some(mut count) = opt_ins.as_mut().and_then(State::try_write) {
            *count += 1;
        }
        Self(opt_ins)
    }
}

impl Drop for TabHeaderOptIn {
    fn drop(&mut self) {
        if let Some(mut count) = self.0.as_mut().and_then(State::try_write) {
            *count = count.saturating_sub(1);
        }
    }
}

/// Maps to: CC `Tabs.tsx:319-339#useTabHeaderFocus`. Opts the calling
/// component into header-focus gating for as long as it is mounted: the
/// Tabs' ↓ then hands focus to the content. For a Select, disable it while
/// `header_focused` and call `focus_header` on up-from-first-item. As in CC,
/// don't call it above an early return that renders static text — split the
/// component so it only runs where the Select renders.
pub fn use_tab_header_focus(hooks: &mut Hooks) -> TabHeaderFocus {
    let context = hooks
        .try_use_context::<TabsContextValue>()
        .map(|context| context.clone())
        .unwrap_or_default();
    let opt_ins = context.opt_ins;
    hooks.use_state(move || TabHeaderOptIn::register(opt_ins));
    TabHeaderFocus {
        header_focused: context.header_focused,
        header: context.header,
    }
}
/// Maps to: CC `components/design-system/Tabs.tsx:314-317#useTabsWidth`.
pub fn use_tabs_width(hooks: &Hooks) -> Option<u16> {
    hooks
        .try_use_context::<TabsContextValue>()
        .and_then(|context| context.width)
}

/// Maps to: CC `components/design-system/Tabs.tsx:20-57#TabsProps`.
/// Typed Tab children preserve React's child.props metadata without duplicating
/// a separate header list in callers. Focus opt-in props are not used by /plugin.
#[derive(Default, Props)]
pub struct TabsProps {
    pub children: Vec<Element<'static, Tab>>,
    pub title: Option<String>,
    pub color: Option<Color>,
    pub default_tab: Option<String>,
    pub hidden: bool,
    pub use_full_width: bool,
    pub selected_tab: Option<String>,
    pub on_tab_change: Handler<String>,
    pub banner: Vec<AnyElement<'static>>,
    pub disable_navigation: bool,
    /// CC `initialHeaderFocused`, default true (`None`).
    pub initial_header_focused: Option<bool>,
    pub content_height: Option<u16>,
    /// CC `navFromContent`.
    pub nav_from_content: bool,
}
/// Maps to: CC `components/design-system/Tabs.tsx:78-292#Tabs`.
/// Controlled/uncontrolled selection, header focus (`initialHeaderFocused`,
/// the ↓ hand-off to opted-in content, `navFromContent`) and canonical modal
/// ScrollBox placement.
#[component]
pub fn Tabs<'a>(props: &'a mut TabsProps, mut hooks: Hooks) -> impl Into<AnyElement<'a>> {
    let (terminal_width, _) = hooks.use_terminal_size();
    let tabs = props
        .children
        .iter()
        .map(|child| {
            TabItem::new(
                child
                    .props
                    .id
                    .as_ref()
                    .unwrap_or(&child.props.title)
                    .clone(),
                child.props.title.clone(),
            )
        })
        .collect::<Vec<_>>();
    let default_index = props
        .default_tab
        .as_ref()
        .and_then(|id| tabs.iter().position(|tab| &tab.id == id))
        .unwrap_or(0);
    let internal_selected = hooks.use_state(move || default_index);
    let selected_index = props
        .selected_tab
        .as_ref()
        .map(|id| tabs.iter().position(|tab| &tab.id == id).unwrap_or(0))
        .unwrap_or(internal_selected.get());
    let controlled = props.selected_tab.is_some();
    let on_change = props.on_tab_change.clone();
    let callback_present = !on_change.is_default();
    // CC :125-136: the header row starts focused unless the caller says
    // otherwise; children that call `use_tab_header_focus` are counted.
    let initial_header_focused = props.initial_header_focused.unwrap_or(true);
    let mut header_focused = hooks.use_state(move || initial_header_focused);
    let opt_ins = hooks.use_state(|| 0usize);
    let opted_in = opt_ins.get() > 0;
    // CC :138-150 `handleTabChange`: switching is a header action, so the
    // header keeps focus; the new tab can take it back itself.
    let change_tab = {
        let tabs = tabs.clone();
        move |forward: bool| {
            let (mut internal_selected, mut header_focused) = (internal_selected, header_focused);
            if tabs.is_empty() {
                return;
            }
            let offset = if forward { 1 } else { tabs.len() - 1 };
            let index = (selected_index + offset) % tabs.len();
            if controlled && callback_present && !tabs[index].id.is_empty() {
                on_change(tabs[index].id.clone());
            } else {
                internal_selected.set(index);
            }
            header_focused.set(true);
        }
    };
    let navigable = !props.hidden && !props.disable_navigation;
    // CC :163-172 `handleKeyDown`: while the header is focused, ↓ hands focus
    // to the content, but only when a child has opted in — a legacy tab has
    // no way to give it back. Without an opt-in (as in /plugin) it does not
    // subscribe at all. `prevent_default` is CC's `e.preventDefault()`: it
    // marks the key, it does not hide it from later handlers.
    //
    // Registered ahead of the tab keybindings: iocraft drains each event
    // hook's queue before the next hook runs, so with ↓ and a tab key read
    // in one chunk this order handles ↓ first, as Ink's key-by-key dispatch
    // does, and the tab switch that follows focuses the header again.
    // (The other order would blur the header on the new tab, counting the
    // old tab's opt-in, and a static tab could never take focus back.)
    let hidden = props.hidden;
    hooks.use_propagated_terminal_events_for(
        if opted_in && !hidden {
            TerminalEventInterest::KEY
        } else {
            TerminalEventInterest::NONE
        },
        move |event| {
            let TerminalEvent::Key(KeyEvent { code, kind, .. }) = event.event() else {
                return;
            };
            if *kind == KeyEventKind::Release || *code != KeyCode::Down {
                return;
            }
            if header_focused.get() && opt_ins.get() > 0 && !hidden {
                header_focused.set(false);
                event.prevent_default();
            }
        },
    );
    // CC :152-161: tab/←/→ switch tabs while the header is focused.
    use_tabs_keybindings(
        &mut hooks,
        navigable && header_focused.get(),
        {
            let change_tab = change_tab.clone();
            move || change_tab(true)
        },
        {
            let change_tab = change_tab.clone();
            move || change_tab(false)
        },
    );
    // CC :174-196: with `navFromContent`, the same keys switch tabs from
    // opted-in content too, and focus the header.
    use_tabs_keybindings(
        &mut hooks,
        props.nav_from_content && !header_focused.get() && opted_in && navigable,
        {
            let change_tab = change_tab.clone();
            move || change_tab(true)
        },
        move || change_tab(false),
    );
    let modal_scroll_ref = crate::context::modal_context::use_modal_scroll_ref(&hooks);
    let width = props.use_full_width.then_some(terminal_width);
    let context = TabsContextValue {
        selected_tab: tabs.get(selected_index).map(|tab| tab.id.clone()),
        width,
        header_focused: header_focused.get(),
        header: Some(header_focused),
        opt_ins: Some(opt_ins),
    };
    // Borrow source children: a local selection/resize render must not consume
    // props and erase the tab metadata or the previously mounted body.
    let children = props
        .children
        .iter_mut()
        .map(AnyElement::from)
        .collect::<Vec<_>>();
    let body = if let Some(handle) = modal_scroll_ref {
        element!{View(width:width.map(|w|Size::Length(w.into())).unwrap_or(Size::Auto),margin_top:if props.hidden{0u32}else{1u32},flex_shrink:0.0f32){ScrollBox(key:selected_index,handle:Some(handle),keyboard_scroll:Some(false)){#(children)}}}.into_any()
    } else {
        element!{View(width:width.map(|w|Size::Length(w.into())).unwrap_or(Size::Auto),margin_top:if props.hidden{0u32}else{1u32},height:props.content_height.map(|h|Size::Length(h.into())).unwrap_or(Size::Auto),overflow:if props.content_height.is_some(){Overflow::Hidden}else{Overflow::Visible}){#(children)}}.into_any()
    };
    // AnyElement's existing From<&mut AnyElement<'static>> keeps its inner
    // lifetime. First select that implementation, then shorten the resulting
    // value to this update borrow; mutable input references cannot be coerced.
    let banner = props.banner.iter_mut().map(|child| -> AnyElement<'a> {
        let borrowed: AnyElement<'static> = AnyElement::from(child);
        borrowed
    });
    element!{ContextProvider(value:Context::owned(context)){View(flex_direction:FlexDirection::Column,tab_index:Some(0),auto_focus:true,flex_shrink:if modal_scroll_ref.is_some(){0.0f32}else{1.0f32}){
        TabsHeader(title:props.title.clone(),color:props.color,tabs:tabs,selected_index:selected_index,header_focused:header_focused.get(),hidden:props.hidden,use_full_width:props.use_full_width)
        #(banner)
        #(std::iter::once(body))
    }}}.into_any()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;

    #[tokio::test]
    async fn mounted_tabs_selection_matches_official_bun() {
        use futures::{Stream, StreamExt};
        use std::{
            pin::Pin,
            sync::{
                Arc, Mutex,
                atomic::{AtomicUsize, Ordering},
            },
            task::Poll,
            time::Duration,
        };
        // The mock renderer pushes every repaint into a separate canvas queue.
        // Polling it once is not an event completion barrier: focus/resize may
        // leave an older frame queued. Observe the source outcome, retaining all
        // intermediate frames for a bounded, diagnostic failure.
        async fn observe<S: Stream<Item = Canvas> + Unpin>(
            frames: &mut S,
            last: &mut (String, usize),
            history: &mut Vec<(String, usize)>,
            ready: impl Fn(&str, usize) -> bool,
        ) {
            let result = tokio::time::timeout(
                Duration::from_secs(2),
                std::future::poll_fn(|cx| {
                    loop {
                        match Pin::new(&mut *frames).poll_next(cx) {
                            Poll::Ready(Some(canvas)) => {
                                *last = (canvas.to_string(), canvas.width());
                                history.push(last.clone());
                                if ready(&last.0, last.1) {
                                    return Poll::Ready(());
                                }
                            }
                            Poll::Ready(None) => panic!("renderer ended: {history:?}"),
                            Poll::Pending => {
                                // Controlled/disabled Tabs need not repaint after
                                // Tab; source oracle records an empty after frame.
                                return if ready(&last.0, last.1) {
                                    Poll::Ready(())
                                } else {
                                    Poll::Pending
                                };
                            }
                        }
                    }
                }),
            )
            .await;
            assert!(
                result.is_ok(),
                "Tabs did not reach source outcome; frames={history:?}"
            );
        }
        let oracle:serde_json::Value=serde_json::from_str(include_str!("../../../tests/fixtures/oracles/plugin-ui-complete-0914/tabs-oracle.json")).unwrap();
        for case in oracle.as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            let events = Arc::new(Mutex::new(Vec::<String>::new()));
            let sink = events.clone();
            let runtime = KeybindingRuntime::with_default_bindings();
            let mut app = element! {ContextProvider(value:Context::owned(*theme::current())){ContextProvider(value:Context::owned(runtime)){FocusScope(handle_keys:false){
                Tabs(title:Some("Plugins".into()),selected_tab:if name=="controlled"{Some("b".into())}else{None},disable_navigation:name=="disabled",on_tab_change:Handler::from(move|id|sink.lock().unwrap().push(id))){
                    Tab(id:Some("a".into()),title:"Discover"){Text(content:"body A")}
                    Tab(id:Some("b".into()),title:"Installed"){Text(content:"body B")}
                }
            }}}};
            let (keys, input) = async_channel::unbounded();
            let delivered_tabs = Arc::new(AtomicUsize::new(0));
            let delivered = delivered_tabs.clone();
            let input = input.inspect(move |event| {
                if matches!(
                    event,
                    TerminalEvent::Key(KeyEvent {
                        code: KeyCode::Tab,
                        ..
                    })
                ) {
                    delivered.fetch_add(1, Ordering::SeqCst);
                }
            });
            let mut frames = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(input).with_size(60, 20),
            ));
            let mut last = (String::new(), 0);
            let mut history = Vec::new();
            let initial_body = if name == "controlled" {
                "body B"
            } else {
                "body A"
            };
            observe(&mut frames, &mut last, &mut history, |text, _| {
                text.contains(initial_body)
            })
            .await;
            let first = last
                .0
                .lines()
                .map(str::trim_end)
                .collect::<Vec<_>>()
                .join("\n");
            assert!(
                first.contains("Discover") && first.contains("Installed"),
                "{name}: {first}"
            );
            assert!(
                case["initial"].as_str().unwrap().contains(first.trim()),
                "{name}: {first}"
            );

            // First verify Tab alone. For controlled mode, the callback is the
            // acknowledgement; the fixed selected prop intentionally stays B.
            keys.send(TerminalEvent::Key(KeyEvent::new(
                KeyEventKind::Press,
                KeyCode::Tab,
            )))
            .await
            .unwrap();
            let expected = if name == "disabled" {
                "body A"
            } else {
                "body B"
            };
            observe(&mut frames, &mut last, &mut history, |text, _| {
                delivered_tabs.load(Ordering::SeqCst) >= 1
                    && text.contains(expected)
                    && serde_json::json!(*events.lock().unwrap()) == case["events"]
            })
            .await;
            assert!(
                last.0.contains("Discover") && last.0.contains("Installed"),
                "{name}: {:?}",
                history
            );
            assert!(
                !last.0.contains(if expected == "body A" {
                    "body B"
                } else {
                    "body A"
                }),
                "{name}: {:?}",
                history
            );
            assert_eq!(serde_json::json!(*events.lock().unwrap()), case["events"]);

            // Only then resize. Canvas width comes from the native root's
            // terminal-column layout, so an older frame cannot satisfy this.
            for width in [59u16, 58] {
                keys.send(TerminalEvent::Resize(width, 20)).await.unwrap();
                observe(&mut frames, &mut last, &mut history, |text, columns| {
                    columns == usize::from(width)
                        && text.contains(expected)
                        && text.contains("Installed")
                })
                .await;
            }
            if name == "uncontrolled" {
                keys.send(TerminalEvent::Key(KeyEvent::new(
                    KeyEventKind::Press,
                    KeyCode::Tab,
                )))
                .await
                .unwrap();
                observe(&mut frames, &mut last, &mut history, |text, _| {
                    delivered_tabs.load(Ordering::SeqCst) >= 2
                        && text.contains("body A")
                        && !text.contains("body B")
                })
                .await;
                assert!(last.0.contains("Discover"), "{:?}", history);
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

    #[component]
    fn TabsBindingHarness(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut selected = hooks.use_state(|| 0usize);
        use_tabs_keybindings(
            &mut hooks,
            true,
            move || selected.set((selected.get() + 1) % 2),
            move || selected.set((selected.get() + 1) % 2),
        );
        element! { Text(content: format!("selected={}", selected.get())) }
    }

    fn find_text_cell(canvas: &Canvas, needle: &str) -> Option<(usize, usize)> {
        canvas_lines(canvas)
            .iter()
            .enumerate()
            .find_map(|(row, line)| line.find(needle).map(|column| (column, row)))
    }

    #[test]
    fn tabs_actions_follow_live_user_binding_instead_of_raw_arrow_keys() {
        use futures::{StreamExt, stream};
        use std::time::Duration;

        let runtime = KeybindingRuntime::new(vec![crate::keybindings::types::ParsedBinding {
            chord: crate::keybindings::parser::parse_chord("f2"),
            action: Some("tabs:next".to_string()),
            context: ContextName::Tabs,
        }]);
        let text = futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(runtime)) {
                    TabsBindingHarness
                }
            };
            let mut render_loop = Box::pin(
                app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(stream::iter(vec![TerminalEvent::Key(
                        KeyEvent::new(KeyEventKind::Press, KeyCode::F(2)),
                    )]))
                    .with_size(24, 4),
                ),
            );
            let mut last = String::new();
            for _ in 0..12 {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(100)).await;
                    None
                })
                .await;
                let Some(canvas) = next else {
                    break;
                };
                last = canvas.to_string();
                if last.contains("selected=1") {
                    break;
                }
            }
            last
        });
        assert!(text.contains("selected=1"), "canvas=\n{text}");
    }

    #[test]
    fn tabs_header_matches_official_focused_color_cursor() {
        let current_theme = *theme::current();
        let color = current_theme.professional_blue;
        let canvas = element! {
            ContextProvider(value: Context::owned(current_theme)) {
                TabsHeader(
                    title: Some("Help".to_string()),
                    color: Some(color),
                    tabs: vec![
                        TabItem::new("general", "general"),
                        TabItem::new("commands", "commands"),
                    ],
                    selected_index: 1usize,
                    header_focused: true,
                )
            }
        }
        .render(Some(80));
        let text = canvas_lines(&canvas).join("\n");
        let (column, row) = find_text_cell(&canvas, "commands").expect("commands tab");
        let cell = canvas.cell(column, row).unwrap();

        assert!(text.contains("Help  general   commands"), "canvas=\n{text}");
        assert_eq!(cell.background_color, Some(color), "canvas=\n{text}");
        let style = cell.text_style().expect("commands tab style");
        assert_eq!(
            style.color,
            Some(current_theme.inverse_text),
            "canvas=\n{text}"
        );
        assert!(!style.invert, "canvas=\n{text}");
    }

    #[test]
    fn tabs_header_blurred_current_tab_uses_inverse_without_color_background() {
        let current_theme = *theme::current();
        let color = current_theme.professional_blue;
        let canvas = element! {
            ContextProvider(value: Context::owned(current_theme)) {
                TabsHeader(
                    title: Some("Help".to_string()),
                    color: Some(color),
                    tabs: vec![
                        TabItem::new("general", "general"),
                        TabItem::new("commands", "commands"),
                    ],
                    selected_index: 1usize,
                    header_focused: false,
                )
            }
        }
        .render(Some(80));
        let text = canvas_lines(&canvas).join("\n");
        let (column, row) = find_text_cell(&canvas, "commands").expect("commands tab");
        let cell = canvas.cell(column, row).unwrap();

        assert_eq!(cell.background_color, None, "canvas=\n{text}");
        assert!(
            cell.text_style().is_some_and(|style| style.invert),
            "canvas=\n{text}"
        );
    }

    #[test]
    fn tabs_header_index_helpers_wrap_like_official_tabs() {
        let tabs = vec![
            TabItem::new("general", "general"),
            TabItem::new("commands", "commands"),
        ];

        assert_eq!(selected_tab_index(&tabs, Some("commands"), 0), 1);
        assert_eq!(selected_tab_index(&tabs, Some("missing"), 7), 1);
        assert_eq!(next_tab_index(1, tabs.len()), 0);
        assert_eq!(previous_tab_index(0, tabs.len()), 1);
    }
    #[test]
    fn tabs_header_serialized_space_styles_match_bun() {
        // Source oracle mounts the actual Tabs component under Bun. Compare
        // resolved SGR cells, not escape spellings or blank Canvas metadata:
        // an undrawn layout gap previously looked neutral but leaked SGR.
        let cases: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/oracles/tabs-style-0915/oracle.json"
        )).unwrap();
        for case in cases.as_array().unwrap() {
            let width = case["width"].as_u64().unwrap() as usize;
            // At 28 columns Yoga shrinks/repositions the labels. That existing
            // layout difference is separately recorded, not a style assertion.
            if width < 30 {
                continue;
            }
            let title = case["title"].as_str().map(str::to_owned);
            let colored = case["color"].is_string();
            let current_theme = *theme::current();
            let canvas = element! {
                ContextProvider(value: Context::owned(current_theme)) {
                    TabsHeader(title: title, color: colored.then_some(Color::Rgb{r:106,g:155,b:204}),
                        tabs: vec![TabItem::new("a", "Discover"), TabItem::new("b", "Installed")],
                        selected_index: if case["selectedTab"] == "a" {0usize} else {1usize},
                        header_focused: true)
                }
            }.render(Some(width));
            let mut bytes = Vec::new();
            canvas.write_ansi(&mut bytes).unwrap();
            let emitted = String::from_utf8(bytes).unwrap();
            let actual = element! { RawAnsi(lines: vec![emitted.lines().next().unwrap().to_owned()], width: width) }.render(Some(width));
            let expected = element! { RawAnsi(lines: vec![case["initial"].as_str().unwrap().lines().next().unwrap().to_owned()], width: width) }.render(Some(width));
            let expected_text = canvas_lines(&expected)[0].trim_end().to_owned();
            assert_eq!(canvas_lines(&actual)[0].trim_end(), expected_text, "{case}");
            for col in 0..expected_text.len() {
                let actual = actual.cell(col, 0).unwrap();
                let expected = expected.cell(col, 0).unwrap();
                assert_eq!(
                    actual.text_style(),
                    expected.text_style(),
                    "column {col}: {case}"
                );
                assert_eq!(
                    actual.background_color, expected.background_color,
                    "column {col}: {case}"
                );
            }
        }
    }

    #[derive(Default, Props)]
    struct OptInBodyProps {
        label: String,
    }

    /// A tab body that opts into header focus, shows what it sees, and hands
    /// focus back to the header on `u` (a Select's up-from-first-item).
    #[component]
    fn OptInBody(props: &OptInBodyProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let focus = use_tab_header_focus(&mut hooks);
        hooks.use_terminal_events(move |event| {
            if let TerminalEvent::Key(KeyEvent { code: KeyCode::Char('u'), kind, .. }) = event {
                if kind == KeyEventKind::Press && !focus.header_focused {
                    focus.focus_header();
                }
            }
        });
        element! { Text(content: format!("{} header={}", props.label, focus.header_focused)) }
    }

    #[component]
    fn KeyCountEcho(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut keys = hooks.use_state(|| 0usize);
        hooks.use_terminal_events(move |event| {
            if matches!(event, TerminalEvent::Key(key) if key.kind == KeyEventKind::Press) {
                keys.set(keys.get() + 1);
            }
        });
        element! { Text(content: format!("keys={} pressed", keys.get())) }
    }

    #[derive(Default, Props)]
    struct FocusHarnessProps {
        nav_from_content: bool,
    }

    /// Tab A opts in; tab B is static text that does not.
    #[component]
    fn FocusHarness(props: &FocusHarnessProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let runtime = crate::keybindings::keybinding_provider_setup::use_keybinding_setup(
            &mut hooks,
            KeybindingRuntime::with_default_bindings(),
        );
        element! {
            ContextProvider(value: Context::owned(runtime)) {
                ContextProvider(value: Context::owned(*theme::current())) {
                    View(flex_direction: FlexDirection::Column) {
                        Tabs(title: Some("T".to_string()), nav_from_content: props.nav_from_content) {
                            Tab(id: Some("a".to_string()), title: "A") {
                                OptInBody(label: "body A")
                            }
                            Tab(id: Some("b".to_string()), title: "B") {
                                Text(content: "body B static")
                            }
                        }
                        KeyCountEcho
                    }
                }
            }
        }
    }

    /// Sends one key per frame; returns the body line of the mount frame and
    /// of the frame after each key.
    fn drive_focus(nav_from_content: bool, keys_in_order: Vec<KeyCode>) -> Vec<String> {
        drive_focus_batches(nav_from_content, keys_in_order.into_iter().map(|key| vec![key]).collect())
    }

    /// Sends each batch of keys back to back, so a batch reaches the app as
    /// one input chunk; returns the body line of the mount frame and of the
    /// frame after each batch.
    fn drive_focus_batches(nav_from_content: bool, batches: Vec<Vec<KeyCode>>) -> Vec<String> {
        use futures::StreamExt;
        futures::executor::block_on(async move {
            let (keys, events) = async_channel::unbounded();
            let mut app = element!(FocusHarness(nav_from_content: nav_from_content));
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(60, 12),
            ));
            let mut bodies = Vec::new();
            let mut sent = 0;
            while let Some(canvas) = render_loop.next().await {
                let text = canvas.to_string();
                if !text.contains(&format!("keys={sent} ")) {
                    continue;
                }
                let body = text
                    .lines()
                    .find(|line| line.contains("body"))
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                bodies.push(body);
                let Some(batch) = batches.get(bodies.len() - 1) else {
                    break;
                };
                for code in batch {
                    keys.send(TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, *code)))
                        .await
                        .unwrap();
                }
                sent += batch.len();
            }
            bodies
        })
    }

    #[test]
    fn tabs_down_and_a_tab_key_in_one_chunk_leave_the_header_focused() {
        // Ink dispatches ↓ then → key by key: ↓ blurs the header (Tabs.tsx
        // :163-172), → switches tabs and focuses it (:149). Read as one chunk
        // the port must end the same way, or the static tab B would strand
        // the panel with no focused header. The next → proves it is focused.
        let bodies = drive_focus_batches(
            false,
            vec![vec![KeyCode::Down, KeyCode::Right], vec![KeyCode::Right]],
        );
        assert_eq!(bodies, vec!["body A header=true", "body B static", "body A header=true"]);
    }

    #[test]
    fn tabs_hand_focus_to_opted_in_content_on_down_and_take_it_back() {
        // CC Tabs.tsx:125 the header starts focused; :163-172 ↓ hands focus
        // to opted-in content; :152-161 tab/←/→ only switch tabs while the
        // header has focus; the content's focusHeader gives it back.
        let bodies = drive_focus(
            false,
            vec![KeyCode::Down, KeyCode::Right, KeyCode::Char('u'), KeyCode::Right],
        );
        assert_eq!(
            bodies,
            vec![
                "body A header=true",
                "body A header=false",
                "body A header=false",
                "body A header=true",
                "body B static",
            ]
        );
    }

    #[test]
    fn tabs_keep_the_header_focused_on_a_tab_that_did_not_opt_in() {
        // CC Tabs.tsx:128-136: only mounted opt-ins count, so once tab A
        // unmounts ↓ on B leaves the header focused and → still switches.
        let bodies = drive_focus(false, vec![KeyCode::Right, KeyCode::Down, KeyCode::Right]);
        assert_eq!(
            bodies,
            vec!["body A header=true", "body B static", "body B static", "body A header=true"]
        );
    }

    #[test]
    fn tabs_nav_from_content_switches_tabs_and_focuses_the_header() {
        // CC Tabs.tsx:174-196: with navFromContent, → from focused content
        // switches tabs and focuses the header, so ← then switches back.
        let bodies = drive_focus(true, vec![KeyCode::Down, KeyCode::Right, KeyCode::Left]);
        assert_eq!(
            bodies,
            vec![
                "body A header=true",
                "body A header=false",
                "body B static",
                "body A header=true",
            ]
        );
    }

    #[test]
    fn tabs_header_separator_is_real_neutral_text_outside_selected_padding() {
        for focused in [false, true] {
            for selected in [0usize, 1] {
                let canvas = element! {ContextProvider(value: Context::owned(*theme::current())) {
                    TabsHeader(title: Some("Plugins".to_owned()), color: Some(Color::Blue),
                        tabs: vec![TabItem::new("a", "Discover"), TabItem::new("b", "Installed")],
                        selected_index: selected, header_focused: focused)
                }}
                .render(Some(60));
                for col in [7, 18] {
                    let gap = canvas.cell(col, 0).unwrap();
                    assert_eq!(gap.text(), Some(" "), "column {col}");
                    assert_eq!(gap.text_style(), Some(&Default::default()), "column {col}");
                    assert_eq!(gap.background_color, None);
                }
                for col in if selected == 0 { [8, 17] } else { [19, 29] } {
                    let padding = canvas.cell(col, 0).unwrap();
                    assert_eq!(padding.text_style().unwrap().weight, Weight::Bold);
                    assert_eq!(padding.text_style().unwrap().invert, !focused);
                    assert_eq!(padding.background_color, focused.then_some(Color::Blue));
                }
            }
        }
    }
}
