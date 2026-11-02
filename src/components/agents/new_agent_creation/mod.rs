//! Maps to: CC `components/agents/new-agent-creation/**`.

pub mod create_agent_wizard;
pub mod types;
pub mod wizard_steps;

pub use create_agent_wizard::CreateAgentWizard;

#[cfg(test)]
mod tests {
    use super::create_agent_wizard::CreateAgentWizard;
    use super::wizard_steps::{
        ConfirmStep, LocationStep, MemoryStep, MethodStep, PromptStep, TypeStep,
    };
    use crate::components::wizard::{WizardData, WizardProvider, WizardStep};
    use futures::{StreamExt, stream};
    use iocraft::prelude::*;
    use serde_json::json;
    use std::time::Duration;

    #[component]
    fn Marker() -> impl Into<AnyElement<'static>> {
        element! { Text(content: "NEXT STEP".to_string()) }
    }

    fn provider(step: WizardStep, data: WizardData) -> AnyElement<'static> {
        // CC useTextInput.ts:105-106 calls useNotifications even before input;
        // notifications.tsx:50-51 requires the production AppState context.
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        element! {
            ContextProvider(value: Context::owned(store)) {
                ContextProvider(value: Context::owned(
                        crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings()
                    )) {
                    ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                        WizardProvider(
                            steps: vec![step, WizardStep::new(|| element! { Marker }.into_any())],
                            initial_data: data,
                            title: Some("Create new agent".to_string()),
                            show_step_counter: Some(false),
                        )
                    }
                }
            }
        }
        .into_any()
    }

    #[test]
    fn location_and_text_steps_render_official_copy() {
        let location = provider(
            WizardStep::new(|| element! { LocationStep }.into_any()),
            WizardData::new(),
        )
        .render(Some(100))
        .to_string();
        assert!(location.contains("Choose location"));
        assert!(location.contains("Project (.claude/agents/)"));
        assert!(location.contains("Personal (~/.claude/agents/)"));
        let kind = provider(
            WizardStep::new(|| element! { TypeStep }.into_any()),
            WizardData::new(),
        )
        .render(Some(100))
        .to_string();
        assert!(kind.contains("Agent type (identifier)"));
        assert!(kind.contains("Enter a unique identifier"));
        let prompt = provider(
            WizardStep::new(|| element! { PromptStep }.into_any()),
            WizardData::new(),
        )
        .render(Some(100))
        .to_string();
        assert!(prompt.contains("Be comprehensive for best results"));
    }

    #[test]
    fn memory_recommendation_tracks_agent_location() {
        let mut user = WizardData::new();
        user.insert("location".to_string(), json!("userSettings"));
        let text = provider(WizardStep::new(|| element! { MemoryStep }.into_any()), user)
            .render(Some(100))
            .to_string();
        let user_position = text.find("User scope").unwrap();
        let none_position = text.find("None (no persistent memory)").unwrap();
        assert!(user_position < none_position);

        let mut project = WizardData::new();
        project.insert("location".to_string(), json!("projectSettings"));
        let text = provider(
            WizardStep::new(|| element! { MemoryStep }.into_any()),
            project,
        )
        .render(Some(100))
        .to_string();
        assert!(
            text.find("Project scope").unwrap() < text.find("None (no persistent memory)").unwrap()
        );
    }

    #[test]
    fn location_accept_updates_data_and_advances() {
        let events = stream::iter(vec![KeyCode::Enter])
            .then(|code| async move {
                futures_timer::Delay::new(Duration::from_millis(45)).await;
                TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code))
            })
            .chain(stream::pending());
        futures::executor::block_on(async move {
            let mut app = provider(
                WizardStep::new(|| element! { LocationStep }.into_any()),
                WizardData::new(),
            );
            let mut loop_ = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(100, 24),
            ));
            let mut reached = false;
            for _ in 0..25 {
                if let Some(frame) = crate::utils::race(loop_.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(80)).await;
                    None
                })
                .await
                    && frame.to_string().contains("NEXT STEP")
                {
                    reached = true;
                    break;
                }
            }
            assert!(reached);
        });
    }

    #[test]
    fn method_copy_exposes_generate_and_manual_paths() {
        let text = provider(
            WizardStep::new(|| element! { MethodStep }.into_any()),
            WizardData::new(),
        )
        .render(Some(100))
        .to_string();
        assert!(text.contains("Generate with Claude (recommended)"));
        assert!(text.contains("Manual configuration"));
    }

    #[test]
    fn create_wizard_starts_at_location_without_executing_generation_or_file_writes() {
        let text = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                CreateAgentWizard()
            }
        }
        .render(Some(100))
        .to_string();
        assert!(text.contains("Create new agent"));
        assert!(text.contains("Choose location"));
        assert!(text.contains("Project (.claude/agents/)"));
    }

    #[test]
    fn confirm_step_projects_final_agent_validation_and_preview() {
        let mut data = WizardData::new();
        data.insert("finalAgent".to_string(), json!({
            "agentType": "reviewer", "whenToUse": "Use this agent when reviewing code",
            "systemPrompt": "You are a careful code reviewer with detailed instructions.",
            "source": "projectSettings", "tools": ["Read", "Missing"], "model": "opus", "memory": "project"
        }));
        let text = provider(
            WizardStep::new(|| element! { ConfirmStep(tools: vec![crate::components::agents::tool_selector::AgentToolOption::new("Read")]) }.into_any()),
            data,
        ).render(Some(120)).to_string();
        assert!(text.contains("Confirm and save"));
        assert!(text.contains("Name: reviewer"));
        assert!(text.contains("Location: .claude/agents/reviewer.md"));
        assert!(text.contains("Tools: Read and Missing"));
        assert!(text.contains("Invalid tools: Missing"));
        assert!(text.contains("Press s or Enter to save"));
    }
}
