//! Maps to: CC `components/hooks/HooksConfigMenu.tsx`.
//! The menu is intentionally read-only and preserves the official
//! event → matcher → hook → detail responsibility boundaries.

use super::{SelectEventMode, SelectHookMode, SelectMatcherMode, ViewHookMode};
use crate::components::design_system::dialog::Dialog;
use crate::services::hooks::HookEvent;
use crate::utils::hooks::hooks_config_manager::{
    get_hook_event_metadata, get_hooks_for_matcher, get_matcher_metadata,
    get_sorted_matchers_for_event, group_hooks_by_event_and_matcher,
    group_individual_hooks_by_event_and_matcher,
};
use crate::utils::hooks::hooks_settings::IndividualHookConfig;
use crate::utils::settings;
use crate::utils::string_utils::plural;
use crate::utils::worktree::CommandResultDisplay;
use iocraft::prelude::*;
use std::collections::HashMap;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HooksConfigMenuDone {
    pub result: String,
    pub display: CommandResultDisplay,
}

#[derive(Clone, Debug, Default)]
pub struct HooksConfigMenuOverride {
    pub hooks: Vec<IndividualHookConfig>,
    pub hooks_disabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ModeState {
    SelectEvent,
    SelectMatcher {
        event: HookEvent,
    },
    SelectHook {
        event: HookEvent,
        matcher: String,
    },
    ViewHook {
        event: HookEvent,
        hook: IndividualHookConfig,
    },
}

#[derive(Default, Props)]
pub struct HooksConfigMenuProps<'a> {
    pub tool_names: Vec<String>,
    pub on_exit: HandlerMut<'a, HooksConfigMenuDone>,
}

fn load_hooks_disabled() -> bool {
    settings::get_initial_settings().disable_all_hooks == Some(true)
}

/// Maps to: CC `HooksConfigMenu`.
#[component]
pub fn HooksConfigMenu<'a>(
    props: &mut HooksConfigMenuProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let mode_state = hooks.use_state(|| ModeState::SelectEvent);
    let mut pending_done = hooks.use_state(|| Option::<HooksConfigMenuDone>::None);
    // Official useSettingsChange invalidates cached policy/settings data while
    // the dialog is open. Until that shared hook is ported, a short read-only
    // poll gives this browser the same live-refresh behavior without executing
    // hooks or mutating settings.
    let mut refresh_revision = hooks.use_state(|| 0u64);
    hooks.use_interval(
        move || refresh_revision.set(refresh_revision.get().wrapping_add(1)),
        Some(Duration::from_millis(500)),
    );

    let done = { pending_done.read().clone() };
    if let Some(done) = done {
        pending_done.set(None);
        (props.on_exit)(done);
    }

    let mut combined_tool_names = props.tool_names.clone();
    if let Some(store) = hooks.try_use_context::<crate::state::store::AppStore>() {
        combined_tool_names.extend(store.get().mcp.tools.iter().map(|tool| tool.name.clone()));
    }

    let metadata = get_hook_event_metadata(&combined_tool_names);
    let override_data = hooks
        .try_use_context::<HooksConfigMenuOverride>()
        .map(|data| data.clone());
    let hooks_disabled = override_data
        .as_ref()
        .map_or_else(load_hooks_disabled, |data| data.hooks_disabled);
    let hooks_by_event_and_matcher = if let Some(data) = override_data {
        group_individual_hooks_by_event_and_matcher(data.hooks, &combined_tool_names)
    } else {
        group_hooks_by_event_and_matcher(
            &crate::bootstrap::state::get_session_id(),
            &combined_tool_names,
        )
    };

    let mut hooks_by_event = HashMap::new();
    let mut total_hooks_count = 0usize;
    for (event, matchers) in &hooks_by_event_and_matcher {
        let count = matchers.values().map(Vec::len).sum::<usize>();
        hooks_by_event.insert(*event, count);
        total_hooks_count += count;
    }

    let mode = mode_state.read().clone();
    if hooks_disabled {
        let mut pending_done_for_cancel = pending_done;
        let configured_word = plural(total_hooks_count, "hook", None);
        let verb = plural(total_hooks_count, "is", Some("are"));
        return element! {
            Dialog(
                title: "Hook Configuration - Disabled".to_string(),
                input_guide: Some("Esc to close".to_string()),
                on_cancel: move |_| pending_done_for_cancel.set(Some(HooksConfigMenuDone {
                    result: "Hooks dialog dismissed".to_string(),
                    display: CommandResultDisplay::System,
                })),
            ) {
                View(flex_direction: FlexDirection::Column, gap: 1u32) {
                    View(flex_direction: FlexDirection::Column) {
                        Text(
                            content: format!(
                                "All hooks are currently disabled. You have {total_hooks_count} configured {configured_word} that {verb} not running."
                            ),
                            wrap: TextWrap::Wrap,
                        )
                        View(margin_top: 1u32) {
                            Text(content: "When hooks are disabled:".to_string(), dim: true, wrap: TextWrap::NoWrap)
                        }
                        Text(content: "· No hook commands will execute".to_string(), dim: true, wrap: TextWrap::Wrap)
                        Text(content: "· StatusLine will not be displayed".to_string(), dim: true, wrap: TextWrap::Wrap)
                        Text(content: "· Tool operations will proceed without hook validation".to_string(), dim: true, wrap: TextWrap::Wrap)
                    }
                    Text(
                        content: "To re-enable hooks, remove \"disableAllHooks\" from settings.json or ask nyan.".to_string(),
                        dim: true,
                        wrap: TextWrap::Wrap,
                    )
                }
            }
        }.into_any();
    }

    match mode {
        ModeState::SelectEvent => {
            let mut mode_state_for_select = mode_state;
            let mut pending_done_for_cancel = pending_done;
            element! {
                SelectEventMode(
                    hook_event_metadata: metadata,
                    hooks_by_event: hooks_by_event,
                    total_hooks_count: total_hooks_count,
                    on_select_event: move |event| {
                        if get_matcher_metadata(event, &combined_tool_names).is_some() {
                            mode_state_for_select.set(ModeState::SelectMatcher { event });
                        } else {
                            mode_state_for_select.set(ModeState::SelectHook {
                                event,
                                matcher: String::new(),
                            });
                        }
                    },
                    on_cancel: move |_| pending_done_for_cancel.set(Some(HooksConfigMenuDone {
                        result: "Hooks dialog dismissed".to_string(),
                        display: CommandResultDisplay::System,
                    })),
                )
            }
            .into_any()
        }
        ModeState::SelectMatcher { event } => {
            let matchers = get_sorted_matchers_for_event(&hooks_by_event_and_matcher, event);
            let description = metadata
                .get(&event)
                .map(|metadata| metadata.description)
                .unwrap_or("")
                .to_string();
            let mut mode_state_for_select = mode_state;
            let mut mode_state_for_cancel = mode_state;
            element! {
                SelectMatcherMode(
                    selected_event: event,
                    matchers_for_selected_event: matchers,
                    hooks_by_event_and_matcher: hooks_by_event_and_matcher,
                    event_description: description,
                    on_select: move |matcher| mode_state_for_select.set(ModeState::SelectHook { event, matcher }),
                    on_cancel: move |_| mode_state_for_cancel.set(ModeState::SelectEvent),
                )
            }.into_any()
        }
        ModeState::SelectHook { event, matcher } => {
            let selected_hooks =
                get_hooks_for_matcher(&hooks_by_event_and_matcher, event, Some(&matcher));
            let event_metadata = metadata.get(&event).cloned();
            let event_supports_matcher =
                get_matcher_metadata(event, &combined_tool_names).is_some();
            let mut mode_state_for_select = mode_state;
            let mut mode_state_for_cancel = mode_state;
            element! {
                SelectHookMode(
                    selected_event: event,
                    selected_matcher: Some(matcher),
                    hooks_for_selected_matcher: selected_hooks,
                    hook_event_metadata: event_metadata,
                    on_select: move |hook| mode_state_for_select.set(ModeState::ViewHook { event, hook }),
                    on_cancel: move |_| {
                        if event_supports_matcher {
                            mode_state_for_cancel.set(ModeState::SelectMatcher { event });
                        } else {
                            mode_state_for_cancel.set(ModeState::SelectEvent);
                        }
                    },
                )
            }.into_any()
        }
        ModeState::ViewHook { event, hook } => {
            let event_supports_matcher =
                get_matcher_metadata(event, &combined_tool_names).is_some();
            let matcher = hook.matcher.clone().unwrap_or_default();
            let mut mode_state_for_cancel = mode_state;
            element! {
                ViewHookMode(
                    selected_hook: Some(hook),
                    event_supports_matcher: event_supports_matcher,
                    on_cancel: move |_| mode_state_for_cancel.set(ModeState::SelectHook { event, matcher: matcher.clone() }),
                )
            }.into_any()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::hooks::hooks_settings::{HookConfigKind, HookDisplayConfig, HookSource};
    use crate::utils::theme;
    use futures::{StreamExt, stream};
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    fn configured_hook() -> IndividualHookConfig {
        IndividualHookConfig {
            event: HookEvent::PreToolUse,
            config: HookDisplayConfig {
                kind: HookConfigKind::Prompt,
                prompt: Some("Review the response".to_string()),
                ..HookDisplayConfig::default()
            },
            matcher: None,
            source: HookSource::ProjectSettings,
            plugin_name: None,
        }
    }

    #[test]
    fn menu_uses_typed_override_and_renders_event_counts() {
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                ContextProvider(value: Context::owned(HooksConfigMenuOverride {
                    hooks: vec![configured_hook()],
                    ..HooksConfigMenuOverride::default()
                })) {
                    HooksConfigMenu(tool_names: vec!["Bash".to_string()])
                }
            }
        }
        .render(Some(110))
        .to_string();
        assert!(text.contains("1 hook configured"), "canvas=\n{text}");
        assert!(text.contains("PreToolUse (1)"), "canvas=\n{text}");
    }

    #[test]
    fn disabled_menu_matches_official_copy() {
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                ContextProvider(value: Context::owned(HooksConfigMenuOverride {
                    hooks: vec![configured_hook()],
                    hooks_disabled: true,
                })) {
                    HooksConfigMenu
                }
            }
        }
        .render(Some(110))
        .to_string();
        assert!(
            text.contains("Hook Configuration - Disabled"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("All hooks are currently disabled. You have"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("1 configured hook that is not running"),
            "canvas=\n{text}"
        );
        assert!(text.contains("To re-enable hooks"), "canvas=\n{text}");
    }

    #[test]
    fn menu_navigation_drills_down_backs_out_and_dismisses() {
        let mut hook = configured_hook();
        hook.matcher = Some("Bash".to_string());
        let done = Arc::new(Mutex::new(Vec::<HooksConfigMenuDone>::new()));
        let done_for_handler = Arc::clone(&done);
        let done_for_wait = Arc::clone(&done);
        let events = VecDeque::from(vec![
            KeyCode::Enter,
            KeyCode::Enter,
            KeyCode::Enter,
            KeyCode::Esc,
            KeyCode::Esc,
            KeyCode::Esc,
            KeyCode::Esc,
        ]);
        let event_stream = stream::unfold(events, |mut events| async move {
            let code = events.pop_front()?;
            futures_timer::Delay::new(Duration::from_millis(35)).await;
            Some((
                TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code)),
                events,
            ))
        });

        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(
                    crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings()
                )) {
                    ContextProvider(value: Context::owned(*theme::current())) {
                        ContextProvider(value: Context::owned(HooksConfigMenuOverride {
                            hooks: vec![hook],
                            ..HooksConfigMenuOverride::default()
                        })) {
                            HooksConfigMenu(
                                tool_names: vec!["Bash".to_string()],
                                on_exit: move |result| done_for_handler.lock().expect("done mutex").push(result),
                            )
                        }
                    }
                }
            };
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(event_stream).with_size(110, 30),
            ));
            for _ in 0..80 {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(80)).await;
                    None
                })
                .await;
                if next.is_none() || !done_for_wait.lock().expect("done mutex").is_empty() {
                    break;
                }
            }
        });

        assert_eq!(
            *done.lock().expect("done mutex"),
            vec![HooksConfigMenuDone {
                result: "Hooks dialog dismissed".to_string(),
                display: CommandResultDisplay::System,
            }]
        );
    }
}
