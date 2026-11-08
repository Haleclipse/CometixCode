//! Maps to: CC `components/hooks/SelectEventMode.tsx`.

use super::{use_select_bindings, visible_from_index};
use crate::components::custom_select::{Select, SelectOptionData};
use crate::components::design_system::dialog::Dialog;
use crate::constants::figures;
use crate::services::hooks::HookEvent;
use crate::utils::hooks::hooks_config_manager::{HOOK_MENU_EVENTS, HookEventMetadata};
use crate::utils::string_utils::plural;
use crate::utils::theme::Theme;
use iocraft::prelude::*;
use std::collections::HashMap;

#[derive(Props, Default)]
pub struct SelectEventModeProps<'a> {
    pub hook_event_metadata: HashMap<HookEvent, HookEventMetadata>,
    pub hooks_by_event: HashMap<HookEvent, usize>,
    pub total_hooks_count: usize,
    pub on_select_event: HandlerMut<'a, HookEvent>,
    pub on_cancel: HandlerMut<'a, ()>,
}

/// Maps to: CC `SelectEventMode`.
#[component]
pub fn SelectEventMode<'a>(
    props: &mut SelectEventModeProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let mut focused_index = hooks.use_state(|| 0usize);
    let mut pending_selection = hooks.use_state(|| Option::<usize>::None);
    let mut pending_cancel = hooks.use_state(|| false);

    let events = HOOK_MENU_EVENTS
        .iter()
        .copied()
        .filter(|event| props.hook_event_metadata.contains_key(event))
        .collect::<Vec<_>>();
    if focused_index.get() >= events.len() {
        focused_index.set(events.len().saturating_sub(1));
    }

    let selected_index = { *pending_selection.read() };
    if let Some(index) = selected_index {
        pending_selection.set(None);
        if let Some(event) = events.get(index).copied() {
            (props.on_select_event)(event);
        }
    }
    if pending_cancel.get() {
        pending_cancel.set(false);
        (props.on_cancel)(());
    }

    use_select_bindings(&mut hooks, events.len(), focused_index, pending_selection);

    let options = events
        .iter()
        .map(|event| {
            let count = props.hooks_by_event.get(event).copied().unwrap_or(0);
            let name = event.as_str();
            SelectOptionData {
                label: if count > 0 {
                    format!("{name} ({count})")
                } else {
                    name.to_string()
                },
                description: props
                    .hook_event_metadata
                    .get(event)
                    .map(|metadata| metadata.summary.to_string()),
                dim_description: false,
                value: name.to_string(),
                ..SelectOptionData::default()
            }
        })
        .collect::<Vec<_>>();
    let subtitle = format!(
        "{} {} configured",
        props.total_hooks_count,
        plural(props.total_hooks_count, "hook", None)
    );
    let info = figures::get().info.to_string();
    let mut pending_cancel_for_dialog = pending_cancel;

    element! {
        Dialog(
            title: "Hooks".to_string(),
            subtitle: Some(subtitle),
            on_cancel: move |_| pending_cancel_for_dialog.set(true),
        ) {
            View(flex_direction: FlexDirection::Column, gap: 1u32) {
                View(flex_direction: FlexDirection::Column) {
                    View(flex_direction: FlexDirection::Row) {
                        Text(
                            content: format!("{info} This menu is read-only. To add or modify hooks, edit settings.json directly or ask Claude. "),
                            dim: true,
                            wrap: TextWrap::Wrap,
                        )
                        Link(
                            url: "https://code.claude.com/docs/en/hooks".to_string(),
                            label: Some("Learn more".to_string()),
                        )
                    }
                }
                View(flex_direction: FlexDirection::Column) {
                    Select(
                        options: options,
                        focused_index: focused_index.get(),
                        visible_option_count: 5usize,
                        visible_from_index: visible_from_index(focused_index.get(), events.len()),
                    )
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::hooks::hooks_config_manager::get_hook_event_metadata;
    use crate::utils::theme;

    #[test]
    fn event_mode_renders_official_read_only_copy_and_counts() {
        let metadata = get_hook_event_metadata(&["Bash".to_string()]);
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                SelectEventMode(
                    hook_event_metadata: metadata,
                    hooks_by_event: HashMap::from([(HookEvent::PreToolUse, 2usize)]),
                    total_hooks_count: 2usize,
                )
            }
        }
        .render(Some(110))
        .to_string();

        assert!(text.contains("Hooks"), "canvas=\n{text}");
        assert!(text.contains("2 hooks configured"), "canvas=\n{text}");
        assert!(text.contains("This menu is read-only"), "canvas=\n{text}");
        assert!(text.contains("PreToolUse (2)"), "canvas=\n{text}");
        assert!(text.contains("Before tool execution"), "canvas=\n{text}");
    }
}
