//! Maps to: CC `components/hooks/SelectMatcherMode.tsx`.

use super::{use_select_bindings, visible_from_index};
use crate::components::custom_select::{Select, SelectOptionData};
use crate::components::design_system::dialog::Dialog;
use crate::services::hooks::HookEvent;
use crate::utils::hooks::hooks_settings::{
    HookSource, IndividualHookConfig, hook_source_inline_display_string,
};
use crate::utils::string_utils::plural;
use iocraft::prelude::*;
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MatcherWithSource {
    pub matcher: String,
    pub sources: Vec<HookSource>,
    pub hook_count: usize,
}

/// Maps to the official component's `useMemo` projection.
pub fn matchers_with_sources(
    selected_event: HookEvent,
    matchers: &[String],
    grouped: &HashMap<HookEvent, HashMap<String, Vec<IndividualHookConfig>>>,
) -> Vec<MatcherWithSource> {
    matchers
        .iter()
        .map(|matcher| {
            let hooks = grouped
                .get(&selected_event)
                .and_then(|by_matcher| by_matcher.get(matcher))
                .map(Vec::as_slice)
                .unwrap_or_default();
            let mut sources = Vec::new();
            for hook in hooks {
                if !sources.contains(&hook.source) {
                    sources.push(hook.source);
                }
            }
            MatcherWithSource {
                matcher: matcher.clone(),
                sources,
                hook_count: hooks.len(),
            }
        })
        .collect()
}

#[derive(Props)]
pub struct SelectMatcherModeProps<'a> {
    pub selected_event: HookEvent,
    pub matchers_for_selected_event: Vec<String>,
    pub hooks_by_event_and_matcher: HashMap<HookEvent, HashMap<String, Vec<IndividualHookConfig>>>,
    pub event_description: String,
    pub on_select: HandlerMut<'a, String>,
    pub on_cancel: HandlerMut<'a, ()>,
}

impl Default for SelectMatcherModeProps<'_> {
    fn default() -> Self {
        Self {
            selected_event: HookEvent::PreToolUse,
            matchers_for_selected_event: Vec::new(),
            hooks_by_event_and_matcher: HashMap::new(),
            event_description: String::new(),
            on_select: HandlerMut::default(),
            on_cancel: HandlerMut::default(),
        }
    }
}

/// Maps to: CC `SelectMatcherMode`.
#[component]
pub fn SelectMatcherMode<'a>(
    props: &mut SelectMatcherModeProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let mut focused_index = hooks.use_state(|| 0usize);
    let mut pending_selection = hooks.use_state(|| Option::<usize>::None);
    let mut pending_cancel = hooks.use_state(|| false);
    let items = matchers_with_sources(
        props.selected_event,
        &props.matchers_for_selected_event,
        &props.hooks_by_event_and_matcher,
    );

    if focused_index.get() >= items.len() {
        focused_index.set(items.len().saturating_sub(1));
    }
    let selected_index = { *pending_selection.read() };
    if let Some(index) = selected_index {
        pending_selection.set(None);
        if let Some(item) = items.get(index) {
            (props.on_select)(item.matcher.clone());
        }
    }
    if pending_cancel.get() {
        pending_cancel.set(false);
        (props.on_cancel)(());
    }

    use_select_bindings(&mut hooks, items.len(), focused_index, pending_selection);

    let options = items
        .iter()
        .map(|item| {
            let source_text = item
                .sources
                .iter()
                .map(|source| hook_source_inline_display_string(*source))
                .collect::<Vec<_>>()
                .join(", ");
            SelectOptionData {
                label: format!(
                    "[{source_text}] {}",
                    if item.matcher.is_empty() {
                        "(all)"
                    } else {
                        &item.matcher
                    }
                ),
                value: item.matcher.clone(),
                description: Some(format!(
                    "{} {}",
                    item.hook_count,
                    plural(item.hook_count, "hook", None)
                )),
                ..SelectOptionData::default()
            }
        })
        .collect::<Vec<_>>();
    let title = format!("{} - Matchers", props.selected_event.as_str());
    let mut pending_cancel_for_dialog = pending_cancel;

    element! {
        Dialog(
            title: title,
            subtitle: Some(props.event_description.clone()),
            input_guide: items.is_empty().then(|| "Esc to go back".to_string()),
            on_cancel: move |_| pending_cancel_for_dialog.set(true),
        ) {
            #(if items.is_empty() {
                vec![element! {
                    View(flex_direction: FlexDirection::Column, gap: 1u32) {
                        Text(content: "No hooks configured for this event.".to_string(), dim: true, wrap: TextWrap::Wrap)
                        Text(content: "To add hooks, edit settings.json directly or ask nyan.".to_string(), dim: true, wrap: TextWrap::Wrap)
                    }
                }.into_any()]
            } else {
                vec![element! {
                    View(flex_direction: FlexDirection::Column) {
                        Select(
                            options: options,
                            focused_index: focused_index.get(),
                            visible_option_count: 5usize,
                            visible_from_index: visible_from_index(focused_index.get(), items.len()),
                        )
                    }
                }.into_any()]
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::hooks::hooks_settings::{HookConfigKind, HookDisplayConfig, HookSource};
    use crate::utils::theme;

    fn hook(source: HookSource) -> IndividualHookConfig {
        IndividualHookConfig {
            event: HookEvent::PreToolUse,
            config: HookDisplayConfig {
                kind: HookConfigKind::Command,
                command: Some("echo hook".to_string()),
                ..HookDisplayConfig::default()
            },
            matcher: Some("Bash".to_string()),
            source,
            plugin_name: None,
        }
    }

    #[test]
    fn matcher_projection_preserves_source_order_and_hook_count() {
        let grouped = HashMap::from([(
            HookEvent::PreToolUse,
            HashMap::from([(
                "Bash".to_string(),
                vec![
                    hook(HookSource::LocalSettings),
                    hook(HookSource::PluginHook),
                ],
            )]),
        )]);
        let projected =
            matchers_with_sources(HookEvent::PreToolUse, &["Bash".to_string()], &grouped);
        assert_eq!(
            projected[0].sources,
            vec![HookSource::LocalSettings, HookSource::PluginHook]
        );
        assert_eq!(projected[0].hook_count, 2);
    }

    #[test]
    fn matcher_mode_renders_official_empty_state() {
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                SelectMatcherMode(
                    selected_event: HookEvent::PreToolUse,
                    event_description: "Before tool execution".to_string(),
                )
            }
        }
        .render(Some(90))
        .to_string();
        assert!(text.contains("PreToolUse - Matchers"), "canvas=\n{text}");
        assert!(
            text.contains("No hooks configured for this event."),
            "canvas=\n{text}"
        );
        assert!(text.contains("Esc to go back"), "canvas=\n{text}");
    }
}
