//! Maps to: CC `components/hooks/SelectHookMode.tsx`.

use super::{use_select_bindings, visible_from_index};
use crate::components::custom_select::{Select, SelectOptionData};
use crate::components::design_system::dialog::Dialog;
use crate::services::hooks::HookEvent;
use crate::utils::hooks::hooks_config_manager::HookEventMetadata;
use crate::utils::hooks::hooks_settings::{
    HookSource, IndividualHookConfig, get_hook_display_text, hook_source_header_display_string,
};
use iocraft::prelude::*;

/// Maps to the plugin-name suffix branch in the official option projection.
pub fn hook_source_description(hook: &IndividualHookConfig) -> String {
    if hook.source == HookSource::PluginHook
        && let Some(plugin_name) = hook.plugin_name.as_deref()
    {
        return format!(
            "{} ({plugin_name})",
            hook_source_header_display_string(hook.source)
        );
    }
    hook_source_header_display_string(hook.source).to_string()
}

#[derive(Props)]
pub struct SelectHookModeProps<'a> {
    pub selected_event: HookEvent,
    pub selected_matcher: Option<String>,
    pub hooks_for_selected_matcher: Vec<IndividualHookConfig>,
    pub hook_event_metadata: Option<HookEventMetadata>,
    pub on_select: HandlerMut<'a, IndividualHookConfig>,
    pub on_cancel: HandlerMut<'a, ()>,
}

impl Default for SelectHookModeProps<'_> {
    fn default() -> Self {
        Self {
            selected_event: HookEvent::PreToolUse,
            selected_matcher: None,
            hooks_for_selected_matcher: Vec::new(),
            hook_event_metadata: None,
            on_select: HandlerMut::default(),
            on_cancel: HandlerMut::default(),
        }
    }
}

/// Maps to: CC `SelectHookMode`.
#[component]
pub fn SelectHookMode<'a>(
    props: &mut SelectHookModeProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let metadata = props
        .hook_event_metadata
        .clone()
        .unwrap_or(HookEventMetadata {
            summary: "",
            description: "",
            matcher_metadata: None,
        });
    let mut focused_index = hooks.use_state(|| 0usize);
    let mut pending_selection = hooks.use_state(|| Option::<usize>::None);
    let mut pending_cancel = hooks.use_state(|| false);
    let option_count = props.hooks_for_selected_matcher.len();

    if focused_index.get() >= option_count {
        focused_index.set(option_count.saturating_sub(1));
    }
    let selected_index = { *pending_selection.read() };
    if let Some(index) = selected_index {
        pending_selection.set(None);
        if let Some(hook) = props.hooks_for_selected_matcher.get(index).cloned() {
            (props.on_select)(hook);
        }
    }
    if pending_cancel.get() {
        pending_cancel.set(false);
        (props.on_cancel)(());
    }

    use_select_bindings(&mut hooks, option_count, focused_index, pending_selection);

    let title = if metadata.matcher_metadata.is_some() {
        format!(
            "{} - Matcher: {}",
            props.selected_event.as_str(),
            props
                .selected_matcher
                .as_deref()
                .filter(|matcher| !matcher.is_empty())
                .unwrap_or("(all)")
        )
    } else {
        props.selected_event.as_str().to_string()
    };
    let options = props
        .hooks_for_selected_matcher
        .iter()
        .enumerate()
        .map(|(index, hook)| SelectOptionData {
            label: format!(
                "[{}] {}",
                hook.config.kind.as_str(),
                get_hook_display_text(&hook.config)
            ),
            value: index.to_string(),
            description: Some(hook_source_description(hook)),
            ..SelectOptionData::default()
        })
        .collect::<Vec<_>>();
    let mut pending_cancel_for_dialog = pending_cancel;

    element! {
        Dialog(
            title: title,
            subtitle: Some(metadata.description.to_string()),
            input_guide: (option_count == 0).then(|| "Esc to go back".to_string()),
            on_cancel: move |_| pending_cancel_for_dialog.set(true),
        ) {
            #(if option_count == 0 {
                vec![element! {
                    View(flex_direction: FlexDirection::Column, gap: 1u32) {
                        Text(content: "No hooks configured for this event.".to_string(), dim: true, wrap: TextWrap::Wrap)
                        Text(content: "To add hooks, edit settings.json directly or ask Claude.".to_string(), dim: true, wrap: TextWrap::Wrap)
                    }
                }.into_any()]
            } else {
                vec![element! {
                    View(flex_direction: FlexDirection::Column) {
                        Select(
                            options: options,
                            focused_index: focused_index.get(),
                            visible_option_count: 5usize,
                            visible_from_index: visible_from_index(focused_index.get(), option_count),
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

    fn plugin_prompt() -> IndividualHookConfig {
        IndividualHookConfig {
            event: HookEvent::Stop,
            config: HookDisplayConfig {
                kind: HookConfigKind::Prompt,
                prompt: Some("Review the answer".to_string()),
                ..HookDisplayConfig::default()
            },
            matcher: None,
            source: HookSource::PluginHook,
            plugin_name: Some("reviewer".to_string()),
        }
    }

    #[test]
    fn hook_mode_renders_type_content_and_plugin_source() {
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                SelectHookMode(
                    selected_event: HookEvent::Stop,
                    hooks_for_selected_matcher: vec![plugin_prompt()],
                    hook_event_metadata: Some(HookEventMetadata {
                        summary: "stop",
                        description: "Before Claude stops",
                        matcher_metadata: None,
                    }),
                )
            }
        }
        .render(Some(100))
        .to_string();
        assert!(
            text.contains("[prompt] Review the answer"),
            "canvas=\n{text}"
        );
        assert!(text.contains("Plugin Hooks (reviewer)"), "canvas=\n{text}");
    }
}
