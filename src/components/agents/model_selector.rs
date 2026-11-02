//! Maps to: CC `components/agents/ModelSelector.tsx:1-52`.

use crate::components::custom_select::{Select, SelectLayout, SelectOptionData};
use crate::keybindings::keybinding_context::KeybindingRuntime;
use crate::keybindings::types::ContextName;
use crate::keybindings::use_keybinding::use_keybinding;
use iocraft::prelude::*;

fn model_options(initial_model: Option<&str>) -> Vec<SelectOptionData> {
    let mut options = vec![
        SelectOptionData {
            value: "sonnet".to_string(),
            label: "Sonnet".to_string(),
            description: Some("Balanced performance - best for most agents".to_string()),
            dim_description: true,
            ..Default::default()
        },
        SelectOptionData {
            value: "opus".to_string(),
            label: "Opus".to_string(),
            description: Some("Most capable for complex reasoning tasks".to_string()),
            dim_description: true,
            ..Default::default()
        },
        SelectOptionData {
            value: "haiku".to_string(),
            label: "Haiku".to_string(),
            description: Some("Fast and efficient for simple tasks".to_string()),
            dim_description: true,
            ..Default::default()
        },
        SelectOptionData {
            value: "inherit".to_string(),
            label: "Inherit from parent".to_string(),
            description: Some("Use the same model as the main conversation".to_string()),
            dim_description: true,
            ..Default::default()
        },
    ];
    if let Some(model) = initial_model
        && !options.iter().any(|option| option.value == model)
    {
        options.insert(
            0,
            SelectOptionData {
                value: model.to_string(),
                label: model.to_string(),
                description: Some("Current model (custom ID)".to_string()),
                dim_description: true,
                ..Default::default()
            },
        );
    }
    options
}

#[derive(Clone, Copy, Debug)]
enum ModelAction {
    Previous,
    Next,
    Accept,
    Cancel,
}

#[derive(Default, Props)]
pub struct ModelSelectorProps<'a> {
    pub initial_model: Option<String>,
    pub on_complete: HandlerMut<'a, Option<String>>,
    pub on_cancel: HandlerMut<'a, ()>,
}

#[component]
pub fn ModelSelector<'a>(
    props: &mut ModelSelectorProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let options = model_options(props.initial_model.as_deref());
    let default_model = props.initial_model.as_deref().unwrap_or("sonnet");
    let initial_index = options
        .iter()
        .position(|option| option.value == default_model)
        .unwrap_or(0);
    let mut focused = hooks.use_state(move || initial_index);
    let mut pending = hooks.use_state(|| None::<ModelAction>);
    let mut pending_complete = hooks.use_state(|| None::<Option<String>>);
    let mut pending_cancel = hooks.use_state(|| false);

    let completion = {
        let value = pending_complete.read();
        value.clone()
    };
    if let Some(value) = completion {
        pending_complete.set(None);
        (props.on_complete)(value);
    }
    if pending_cancel.get() {
        pending_cancel.set(false);
        if props.on_cancel.is_default() {
            (props.on_complete)(None);
        } else {
            (props.on_cancel)(());
        }
    }
    let action = {
        let value = pending.read();
        *value
    };
    if let Some(action) = action {
        pending.set(None);
        match action {
            ModelAction::Previous => focused.set(if focused.get() == 0 {
                options.len() - 1
            } else {
                focused.get() - 1
            }),
            ModelAction::Next => focused.set((focused.get() + 1) % options.len()),
            ModelAction::Accept => {
                pending_complete.set(Some(Some(options[focused.get()].value.clone())))
            }
            ModelAction::Cancel => pending_cancel.set(true),
        }
    }

    let runtime = hooks
        .try_use_context::<KeybindingRuntime>()
        .map(|runtime| runtime.clone());
    for (name, action) in [
        ("select:previous", ModelAction::Previous),
        ("select:next", ModelAction::Next),
        ("select:accept", ModelAction::Accept),
        ("select:cancel", ModelAction::Cancel),
    ] {
        let mut pending = pending;
        use_keybinding(
            &mut hooks,
            runtime.clone(),
            name,
            ContextName::Select,
            || true,
            move || {
                pending.set(Some(action));
                true
            },
        );
    }

    element! {
        View(flex_direction: FlexDirection::Column) {
            View(margin_bottom: 1u32) {
                Text(content: "Model determines the agent's reasoning capabilities and speed.".to_string(), dim: true)
            }
            Select(
                options: options,
                focused_index: focused.get(),
                visible_option_count: 5usize,
                visible_from_index: focused.get().saturating_add(1).saturating_sub(5),
                selected_value: Some(default_model.to_string()),
                layout: SelectLayout::Compact,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_model_is_injected_before_aliases_and_round_trips() {
        let options = model_options(Some("claude-opus-4-5"));
        assert_eq!(options[0].value, "claude-opus-4-5");
        assert_eq!(
            options[0].description.as_deref(),
            Some("Current model (custom ID)")
        );
        assert_eq!(model_options(Some("sonnet")).len(), 4);
    }

    #[test]
    fn selector_renders_official_explanation_and_aliases() {
        let text = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                ModelSelector(initial_model: Some("inherit".to_string()))
            }
        }
        .render(Some(110))
        .to_string();
        assert!(text.contains("Model determines the agent's reasoning capabilities and speed."));
        assert!(text.contains("Sonnet"));
        assert!(text.contains("Inherit from parent"));
    }
}
