//! Maps to: CC `components/agents/new-agent-creation/CreateAgentWizard.tsx:1-68`.

use super::wizard_steps::{
    ColorStep, ConfirmStepWrapper, DescriptionStep, GenerateStep, LocationStep, MemoryStep,
    MethodStep, ModelStep, PromptStep, ToolsStep, TypeStep,
};
use crate::components::agents::tool_selector::AgentToolOption;
use crate::components::wizard::{WizardData, WizardProvider, WizardStep};
use crate::tools::agent_tool::load_agents_dir::AgentDefinition;
use iocraft::prelude::*;
use std::sync::Arc;

#[derive(Default, Props)]
pub struct CreateAgentWizardProps<'a> {
    pub tools: Vec<AgentToolOption>,
    pub existing_agents: Vec<AgentDefinition>,
    pub on_complete: HandlerMut<'a, String>,
    pub on_cancel: HandlerMut<'a, ()>,
}

#[component]
pub fn CreateAgentWizard<'a>(
    props: &mut CreateAgentWizardProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let auto_memory = hooks.use_const(crate::memdir::paths::is_auto_memory_enabled);
    let completion_channel = hooks.use_const(|| Arc::new(async_channel::unbounded::<String>()));
    let receiver = completion_channel.1.clone();
    let mut pending_complete = hooks.use_state(|| None::<String>);
    hooks.use_future(async move {
        while let Ok(message) = receiver.recv().await {
            pending_complete.set(Some(message));
        }
    });
    let message = { pending_complete.read().clone() };
    if let Some(message) = message {
        pending_complete.set(None);
        (props.on_complete)(message);
    }
    let mut pending_cancel = hooks.use_state(|| false);
    if pending_cancel.get() {
        pending_cancel.set(false);
        (props.on_cancel)(());
    }

    let tools_for_step = props.tools.clone();
    let agents_for_type = props.existing_agents.clone();
    let tools_for_confirm = props.tools.clone();
    let agents_for_confirm = props.existing_agents.clone();
    let sender = completion_channel.0.clone();
    let mut steps = vec![
        WizardStep::new(|| element! { LocationStep }.into_any()),
        WizardStep::new(|| element! { MethodStep }.into_any()),
        WizardStep::new(|| element! { GenerateStep }.into_any()),
        WizardStep::new(move || {
            element! { TypeStep(existing_agents: agents_for_type.clone()) }.into_any()
        }),
        WizardStep::new(|| element! { PromptStep }.into_any()),
        WizardStep::new(|| element! { DescriptionStep }.into_any()),
        WizardStep::new(move || element! { ToolsStep(tools: tools_for_step.clone()) }.into_any()),
        WizardStep::new(|| element! { ModelStep }.into_any()),
        WizardStep::new(|| element! { ColorStep }.into_any()),
    ];
    if auto_memory {
        steps.push(WizardStep::new(|| element! { MemoryStep }.into_any()));
    }
    steps.push(WizardStep::new(move || {
        let sender = sender.clone();
        element! {
            ConfirmStepWrapper(
                tools: tools_for_confirm.clone(), existing_agents: agents_for_confirm.clone(),
                on_complete: move |message| { let _ = sender.try_send(message); },
            )
        }
        .into_any()
    }));
    let mut cancel = pending_cancel;
    element! {
        WizardProvider(
            steps: steps, initial_data: WizardData::new(), title: Some("Create new agent".to_string()),
            show_step_counter: Some(false), on_cancel: move |_| cancel.set(true),
        )
    }
}
