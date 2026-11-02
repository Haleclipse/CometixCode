//! Maps to: CC `components/wizard/WizardNavigationFooter.tsx:1-37`.

use crate::components::configurable_shortcut_hint::ConfigurableShortcutHint;
use crate::components::design_system::byline::Byline;
use crate::components::design_system::keyboard_shortcut_hint::{
    KeyboardShortcutHint, KeyboardShortcutHintStyleContext,
};
use iocraft::prelude::*;

#[derive(Default, Props)]
pub struct WizardNavigationFooterProps {
    /// iocraft equivalent of official optional ReactNode instructions.
    pub instructions: Vec<AnyElement<'static>>,
    /// Convenient styled text projection for wizard step implementations.
    pub instruction_text: Option<String>,
}

/// Maps to: CC `WizardNavigationFooter`.
#[component]
pub fn WizardNavigationFooter(
    props: &mut WizardNavigationFooterProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let exit_state = crate::hooks::use_exit::use_exit_on_ctrl_cd_with_keybindings(&mut hooks, true);
    let custom = std::mem::take(&mut props.instructions);
    let instruction_text = props.instruction_text.clone();

    let body: AnyElement<'static> = if let Some(hint) = exit_state.hint() {
        element! { Text(content: hint.to_string(), dim: true, wrap: TextWrap::NoWrap) }.into_any()
    } else if !custom.is_empty() {
        element! { View(flex_direction: FlexDirection::Row) { #(custom) } }.into_any()
    } else if let Some(text) = instruction_text {
        element! { Text(content: text, dim: true, wrap: TextWrap::NoWrap) }.into_any()
    } else {
        element! {
            Byline {
                KeyboardShortcutHint(
                    shortcut: "↑↓".to_string(), action: "navigate".to_string(), dim: true,
                )
                KeyboardShortcutHint(
                    shortcut: "Enter".to_string(), action: "select".to_string(), dim: true,
                )
                ConfigurableShortcutHint(
                    action: "confirm:no".to_string(), context: "Confirmation".to_string(),
                    fallback: "Esc".to_string(), description: "go back".to_string(), dim: true,
                )
            }
        }
        .into_any()
    };

    element! {
        View(margin_left: 3u32, margin_top: 1u32) {
            ContextProvider(value: Context::owned(KeyboardShortcutHintStyleContext {
                dim: true,
                italic: false,
            })) { #(vec![body]) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_footer_matches_official_copy_spacing_and_dim_style() {
        let canvas = element! { WizardNavigationFooter() }.render(Some(100));
        let text = canvas.to_string();
        assert!(text.contains("   ↑↓ to navigate · Enter to select · Esc to go back"));
        let line = text
            .lines()
            .find(|line| line.contains("↑↓"))
            .expect("footer");
        let y = text
            .lines()
            .position(|candidate| candidate == line)
            .unwrap();
        let x = line.find('↑').unwrap();
        assert!(canvas.resolved_text_style(x, y).unwrap().dim);
    }
}
