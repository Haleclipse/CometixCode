//! Maps to: CC `components/hooks/ViewHookMode.tsx`.

use crate::components::design_system::dialog::Dialog;
use crate::utils::hooks::hooks_settings::{
    IndividualHookConfig, hook_source_description_display_string,
};
use crate::utils::theme::Theme;
use iocraft::prelude::*;

#[derive(Default, Props)]
pub struct ViewHookModeProps<'a> {
    pub selected_hook: Option<IndividualHookConfig>,
    pub event_supports_matcher: bool,
    pub on_cancel: HandlerMut<'a, ()>,
}

/// Maps to: CC `ViewHookMode` plus its `getContentFieldLabel` and
/// `getContentFieldValue` helpers.
#[component]
pub fn ViewHookMode<'a>(
    props: &mut ViewHookModeProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let Some(selected_hook) = props.selected_hook.clone() else {
        return element! { View }.into_any();
    };
    let mut pending_cancel = hooks.use_state(|| false);
    if pending_cancel.get() {
        pending_cancel.set(false);
        (props.on_cancel)(());
    }
    let mut pending_cancel_for_dialog = pending_cancel;
    let status_message = selected_hook.config.status_message.clone();
    let plugin_name = selected_hook.plugin_name.clone();

    element! {
        Dialog(
            title: "Hook details".to_string(),
            input_guide: Some("Esc to go back".to_string()),
            on_cancel: move |_| pending_cancel_for_dialog.set(true),
        ) {
            View(flex_direction: FlexDirection::Column, gap: 1u32) {
                View(flex_direction: FlexDirection::Column) {
                    View(flex_direction: FlexDirection::Row) {
                        Text(content: "Event: ".to_string(), wrap: TextWrap::NoWrap)
                        Text(content: selected_hook.event.as_str().to_string(), weight: Weight::Bold, wrap: TextWrap::NoWrap)
                    }
                    #(if props.event_supports_matcher {
                        Some(element! {
                            View(flex_direction: FlexDirection::Row) {
                                Text(content: "Matcher: ".to_string(), wrap: TextWrap::NoWrap)
                                Text(
                                    content: selected_hook.matcher.clone().filter(|matcher| !matcher.is_empty()).unwrap_or_else(|| "(all)".to_string()),
                                    weight: Weight::Bold,
                                    wrap: TextWrap::NoWrap,
                                )
                            }
                        })
                    } else { None })
                    View(flex_direction: FlexDirection::Row) {
                        Text(content: "Type: ".to_string(), wrap: TextWrap::NoWrap)
                        Text(content: selected_hook.config.kind.as_str().to_string(), weight: Weight::Bold, wrap: TextWrap::NoWrap)
                    }
                    View(flex_direction: FlexDirection::Row) {
                        Text(content: "Source: ".to_string(), wrap: TextWrap::NoWrap)
                        Text(
                            content: hook_source_description_display_string(selected_hook.source).to_string(),
                            dim: true,
                            wrap: TextWrap::Wrap,
                        )
                    }
                    #(plugin_name.map(|plugin_name| element! {
                        View(flex_direction: FlexDirection::Row) {
                            Text(content: "Plugin: ".to_string(), wrap: TextWrap::NoWrap)
                            Text(content: plugin_name, dim: true, wrap: TextWrap::NoWrap)
                        }
                    }))
                }
                View(flex_direction: FlexDirection::Column) {
                    Text(content: format!("{}:", selected_hook.config.content_label()), dim: true, wrap: TextWrap::NoWrap)
                    View(
                        border_style: BorderStyle::Round,
                        border_color: theme.inactive,
                        padding_left: 1u32,
                        padding_right: 1u32,
                    ) {
                        Text(content: selected_hook.config.content_value().to_string(), wrap: TextWrap::Wrap)
                    }
                }
                #(status_message.map(|status_message| element! {
                    View(flex_direction: FlexDirection::Row) {
                        Text(content: "Status message: ".to_string(), wrap: TextWrap::NoWrap)
                        Text(content: status_message, dim: true, wrap: TextWrap::Wrap)
                    }
                }))
                Text(
                    content: "To modify or remove this hook, edit settings.json directly or ask nyan to help.".to_string(),
                    dim: true,
                    wrap: TextWrap::Wrap,
                )
            }
        }
    }.into_any()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::hooks::HookEvent;
    use crate::utils::hooks::hooks_settings::{HookConfigKind, HookDisplayConfig, HookSource};
    use crate::utils::theme;

    #[test]
    fn detail_view_renders_official_http_fields_and_status() {
        let selected_hook = IndividualHookConfig {
            event: HookEvent::PostToolUse,
            config: HookDisplayConfig {
                kind: HookConfigKind::Http,
                url: Some("https://example.test/hook".to_string()),
                status_message: Some("Sending event".to_string()),
                ..HookDisplayConfig::default()
            },
            matcher: Some("Bash".to_string()),
            source: HookSource::ProjectSettings,
            plugin_name: None,
        };
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                ViewHookMode(selected_hook: Some(selected_hook), event_supports_matcher: true)
            }
        }
        .render(Some(110))
        .to_string();

        assert!(text.contains("Hook details"), "canvas=\n{text}");
        assert!(text.contains("Matcher: Bash"), "canvas=\n{text}");
        assert!(text.contains("Type: http"), "canvas=\n{text}");
        assert!(text.contains("URL:"), "canvas=\n{text}");
        assert!(
            text.contains("https://example.test/hook"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("Status message: Sending event"),
            "canvas=\n{text}"
        );
    }
}
