//! Maps to: CC `components/CtrlOToExpand.tsx`.

use crate::components::configurable_shortcut_hint::ConfigurableShortcutHint;
use crate::keybindings::shortcut_format::get_shortcut_display_for_context_name;
use iocraft::components::InVirtualListContext;
use iocraft::prelude::*;

/// Maps to CC's private `SubAgentContext`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SubAgentContext;

#[derive(Default, Props)]
pub struct SubAgentProviderProps {
    pub children: Vec<AnyElement<'static>>,
}

/// Suppresses nested expand hints in sub-agent output.
#[component]
pub fn SubAgentProvider(props: &mut SubAgentProviderProps) -> impl Into<AnyElement<'static>> {
    let children = std::mem::take(&mut props.children);
    element! {
        ContextProvider(value: Context::owned(SubAgentContext)) {
            #(children)
        }
    }
}

/// Maps to CC `CtrlOToExpand`: hide repeated hints in sub-agent and virtual
/// list subtrees, otherwise render the configured transcript-toggle shortcut.
#[component]
pub fn CtrlOToExpand(hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let in_sub_agent = hooks.try_use_context::<SubAgentContext>().is_some();
    let in_virtual_list = hooks.try_use_context::<InVirtualListContext>().is_some();
    if in_sub_agent || in_virtual_list {
        return element! { View {} }.into_any();
    }

    element! {
        ConfigurableShortcutHint(
            action: "app:toggleTranscript".to_string(),
            context: "Global".to_string(),
            fallback: "ctrl+o".to_string(),
            description: "expand".to_string(),
            parens: true,
            dim: true,
        )
    }
    .into_any()
}

/// Maps to CC `ctrlOToExpand()` for string-based renderers.
pub fn ctrl_o_to_expand_hint() -> String {
    let shortcut =
        get_shortcut_display_for_context_name("app:toggleTranscript", "Global", "ctrl+o");
    format!("({shortcut} to expand)")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_helper_uses_configured_shortcut_boundary() {
        assert_eq!(ctrl_o_to_expand_hint(), "(ctrl+o to expand)");
    }

    #[test]
    fn component_renders_outside_suppression_contexts() {
        let canvas = element!(CtrlOToExpand).render(Some(60));
        assert_eq!(canvas.to_string().trim_end(), "(ctrl+o to expand)");
        assert_eq!(
            canvas.resolved_text_style(0, 0).expect("dim style").weight,
            Weight::Light
        );
    }

    #[test]
    fn sub_agent_provider_suppresses_hint() {
        let text = element! {
            SubAgentProvider {
                CtrlOToExpand
            }
        }
        .render(Some(60))
        .to_string();
        assert!(text.trim().is_empty(), "canvas=\n{text}");
    }

    #[test]
    fn virtual_list_context_suppresses_hint() {
        let text = element! {
            ContextProvider(value: Context::owned(InVirtualListContext)) {
                CtrlOToExpand
            }
        }
        .render(Some(60))
        .to_string();
        assert!(text.trim().is_empty(), "canvas=\n{text}");
    }
}
