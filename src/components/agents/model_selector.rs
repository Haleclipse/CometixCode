//! Maps to: CC `components/agents/ModelSelector.tsx:1-52`.

use crate::components::custom_select::{Select, SelectLayout, SelectOptionData};
use crate::keybindings::keybinding_context::KeybindingRuntime;
use crate::keybindings::types::ContextName;
use crate::keybindings::use_keybinding::use_keybinding;
use iocraft::prelude::*;

/// @nyan: an agent's `model:` is a profile ref — inherit, a tier or a
/// configured profile name (sorted). Effort is not chosen here; write
/// `:effort` in the agent file when needed.
fn model_options(
    nyan: Option<&crate::utils::settings::nyan::NyanSettings>,
    initial_model: Option<&str>,
) -> Vec<SelectOptionData> {
    let option = |value: &str, label: &str, description: &str| SelectOptionData {
        value: value.to_string(),
        label: label.to_string(),
        description: Some(description.to_string()),
        dim_description: true,
        ..Default::default()
    };
    let mut options = vec![
        option(
            "inherit",
            "Inherit from parent",
            "Use the same model as the main conversation",
        ),
        option("@light", "@light", "The light tier (selection.light)"),
        option("@heavy", "@heavy", "The heavy tier (selection.heavy)"),
        option("@main", "@main", "The main tier (selection.main)"),
    ];
    let mut profiles = crate::utils::model::profile::profile_names(nyan).collect::<Vec<_>>();
    profiles.sort_unstable();
    options.extend(
        profiles
            .into_iter()
            .map(|name| option(name, name, "Model profile")),
    );
    if let Some(model) = initial_model
        && !options.iter().any(|option| option.value == model)
    {
        options.insert(0, option(model, model, "Current model"));
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
    let settings = crate::utils::settings::get_initial_settings();
    let options = model_options(settings.nyan.as_deref(), props.initial_model.as_deref());
    let default_model = props.initial_model.as_deref().unwrap_or("inherit");
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
    fn options_are_inherit_tiers_then_sorted_profiles() {
        let nyan: crate::utils::settings::nyan::NyanSettings =
            serde_json::from_value(serde_json::json!({
                "models": {"zeta": {"model": "z"}, "alpha": {"model": "a"}}
            }))
            .unwrap();
        let values = model_options(Some(&nyan), None)
            .into_iter()
            .map(|option| option.value)
            .collect::<Vec<_>>();
        assert_eq!(
            values,
            ["inherit", "@light", "@heavy", "@main", "alpha", "zeta"]
        );
        for alias in ["sonnet", "opus", "haiku"] {
            assert!(!values.iter().any(|value| value == alias));
        }
        assert_eq!(model_options(None, None).len(), 4);
        // A value already in the file that is not an option is shown first.
        assert_eq!(
            model_options(Some(&nyan), Some("alpha:high"))[0].value,
            "alpha:high"
        );
        assert_eq!(model_options(Some(&nyan), Some("alpha")).len(), 6);
    }

    #[test]
    fn selector_renders_explanation_and_tiers() {
        let text = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                ModelSelector(initial_model: Some("inherit".to_string()))
            }
        }
        .render(Some(110))
        .to_string();
        assert!(text.contains("Model determines the agent's reasoning capabilities and speed."));
        assert!(text.contains("Inherit from parent"));
        assert!(text.contains("@light"));
        assert!(!text.contains("Sonnet"));
    }
}
