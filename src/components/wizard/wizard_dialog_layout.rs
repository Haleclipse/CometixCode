//! Maps to: CC `components/wizard/WizardDialogLayout.tsx:1-48`.

use super::use_wizard::use_wizard;
use super::wizard_navigation_footer::WizardNavigationFooter;
use crate::components::design_system::dialog::Dialog;
use iocraft::prelude::*;

#[derive(Default, Props)]
pub struct WizardDialogLayoutProps {
    pub title: Option<String>,
    pub color: Option<Color>,
    pub subtitle: Option<String>,
    pub footer_instructions: Vec<AnyElement<'static>>,
    pub footer_text: Option<String>,
    pub children: Vec<AnyElement<'static>>,
}

/// Shared dialog shell for wizard steps.
#[component]
pub fn WizardDialogLayout(
    props: &mut WizardDialogLayoutProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let wizard = use_wizard(&mut hooks);
    let title = props
        .title
        .clone()
        .or_else(|| wizard.title.clone())
        .unwrap_or_else(|| "Wizard".to_string());
    let title = if wizard.show_step_counter {
        format!(
            "{title} ({}/{})",
            wizard.current_step_index + 1,
            wizard.total_steps
        )
    } else {
        title
    };
    let color = props.color.or_else(|| {
        hooks
            .try_use_context::<crate::utils::theme::Theme>()
            .map(|theme| theme.suggestion)
            .or_else(|| Some(crate::utils::theme::current().suggestion))
    });
    let subtitle = props.subtitle.clone();
    let body = std::mem::take(&mut props.children);
    let footer_instructions = std::mem::take(&mut props.footer_instructions);
    let footer_text = props.footer_text.clone();
    let wizard_for_back = wizard.clone();

    element! {
        View(flex_direction: FlexDirection::Column) {
            Dialog(
                title: title,
                subtitle: subtitle,
                color: color,
                hide_input_guide: true,
                is_cancel_active: Some(false),
                on_cancel: move |_| wizard_for_back.go_back(),
            ) { #(body) }
            WizardNavigationFooter(
                instructions: footer_instructions,
                instruction_text: footer_text,
            )
        }
    }
}
