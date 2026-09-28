//! Maps to: CC
//! `components/permissions/EnterPlanModePermissionRequest/EnterPlanModePermissionRequest.tsx`.
//!
//! Ports the official enter-plan-mode confirmation dialog. The UI dispatches
//! through the existing Rust permission choice seam; applying the mode update is
//! handled in `utils/permissions/permissions.rs` for this tool.

use super::permission_dialog::PermissionDialog;
use super::worker_badge::WorkerBadgeProps;
use crate::components::custom_select::{Select, SelectLayout, SelectOptionData};
use crate::types::permissions::{
    PermissionMode, PermissionPromptChoice, PermissionRequest as PermissionRequestData,
    PermissionRuleValue,
};
use crate::utils::theme::Theme;
use iocraft::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnterPlanModePermissionOptionValue {
    Yes,
    No,
}

impl EnterPlanModePermissionOptionValue {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Yes => "yes",
            Self::No => "no",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnterPlanModePermissionOption {
    pub label: String,
    pub value: EnterPlanModePermissionOptionValue,
}

impl EnterPlanModePermissionOption {
    fn select(label: impl Into<String>, value: EnterPlanModePermissionOptionValue) -> Self {
        Self {
            label: label.into(),
            value,
        }
    }

    pub fn to_select_option(&self) -> SelectOptionData {
        SelectOptionData {
            label: self.label.clone(),
            description: None,
            dim_description: true,
            value: self.value.as_str().to_string(),
            disabled: false,
            input: None,
        }
    }
}

#[derive(Default, Props)]
pub struct EnterPlanModePermissionRequestProps<'a> {
    pub request: Option<PermissionRequestData>,
    pub worker_badge: Option<WorkerBadgeProps>,
    pub on_select: HandlerMut<'a, EnterPlanModePermissionOptionValue>,
}

fn _default_request() -> PermissionRequestData {
    PermissionRequestData {
        permission_result: None,
        id: String::new(),
        tool_use_id: String::new(),
        tool_name: "EnterPlanMode".to_string(),
        mcp_info: None,
        decision_reason: None,
        description: String::new(),
        message: String::new(),
        input_summary: String::new(),
        input: serde_json::Value::Null,
        call_input: None,
        rule: PermissionRuleValue::new("EnterPlanMode", None),
        suggestions: Vec::new(),
        blocked_path: None,
        metadata: None,
        is_compound_command: false,
        mode: PermissionMode::Default,
    }
}

/// Maps to: CC `<Select options={[...]}>` in EnterPlanModePermissionRequest.
pub fn enter_plan_mode_permission_options() -> Vec<EnterPlanModePermissionOption> {
    vec![
        EnterPlanModePermissionOption::select(
            "Yes, enter plan mode",
            EnterPlanModePermissionOptionValue::Yes,
        ),
        EnterPlanModePermissionOption::select(
            "No, start implementing now",
            EnterPlanModePermissionOptionValue::No,
        ),
    ]
}

pub fn enter_plan_mode_permission_option_to_prompt_choice(
    value: EnterPlanModePermissionOptionValue,
) -> PermissionPromptChoice {
    match value {
        EnterPlanModePermissionOptionValue::Yes => PermissionPromptChoice::AllowOnce,
        EnterPlanModePermissionOptionValue::No => PermissionPromptChoice::Deny,
    }
}

/// Maps to: CC `EnterPlanModePermissionRequest` render path.
#[component]
pub fn EnterPlanModePermissionRequest<'a>(
    props: &mut EnterPlanModePermissionRequestProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let options = enter_plan_mode_permission_options();
    let option_count = options.len();
    let mut focused_index = hooks.use_state(|| 0usize);
    let mut pending_select = hooks.use_state(|| Option::<EnterPlanModePermissionOptionValue>::None);

    hooks.use_terminal_events({
        let mut focused_index = focused_index;
        let mut pending_select = pending_select;
        let options = options.clone();
        move |event| {
            let TerminalEvent::Key(KeyEvent {
                code,
                kind,
                ..
            }) = event
            else {
                return;
            };
            if kind == KeyEventKind::Release {
                return;
            }
            match code {
                KeyCode::Up | KeyCode::Char('k') => {
                    focused_index.set(focused_index.get().saturating_sub(1));
                }
                KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                    focused_index
                        .set((focused_index.get() + 1).min(options.len().saturating_sub(1)));
                }
                KeyCode::Enter => {
                    if let Some(option) = options.get(focused_index.get()) {
                        pending_select.set(Some(option.value));
                    }
                }
                KeyCode::Esc => {
                    // CC Select.onCancel handles this as the negative response.
                    pending_select.set(Some(EnterPlanModePermissionOptionValue::No));
                }
                // No Ctrl+C here: CC's is PermissionRequest's app:interrupt,
                // and Cometix binds Ctrl+C to app:exit (2.0.x semantics).
                _ => {}
            }
        }
    });

    let selected = { pending_select.read().clone() };
    if let Some(value) = selected {
        pending_select.set(None);
        (props.on_select)(value);
    }

    let focused = focused_index.get().min(option_count.saturating_sub(1));
    let select_options = options
        .iter()
        .map(EnterPlanModePermissionOption::to_select_option)
        .collect::<Vec<_>>();

    element! {
        PermissionDialog(
            color: Some(theme.plan_mode),
            title: "Enter plan mode?".to_string(),
            worker_badge: props.worker_badge.clone(),
        ) {
            View(flex_direction: FlexDirection::Column, margin_top: 1u32, padding_left: 1u32, padding_right: 1u32) {
                Text(content: "Claude wants to enter plan mode to explore and design an implementation approach.".to_string(), wrap: TextWrap::Wrap)
                View(margin_top: 1u32, flex_direction: FlexDirection::Column) {
                    Text(content: "In plan mode, Claude will:".to_string(), color: theme.inactive, wrap: TextWrap::NoWrap)
                    Text(content: " · Explore the codebase thoroughly".to_string(), color: theme.inactive, wrap: TextWrap::NoWrap)
                    Text(content: " · Identify existing patterns".to_string(), color: theme.inactive, wrap: TextWrap::NoWrap)
                    Text(content: " · Design an implementation strategy".to_string(), color: theme.inactive, wrap: TextWrap::NoWrap)
                    Text(content: " · Present a plan for your approval".to_string(), color: theme.inactive, wrap: TextWrap::NoWrap)
                }
                View(margin_top: 1u32) {
                    Text(content: "No code changes will be made until you approve the plan.".to_string(), color: theme.inactive, wrap: TextWrap::Wrap)
                }
                View(margin_top: 1u32) {
                    Select(
                        options: select_options,
                        focused_index: focused,
                        visible_option_count: option_count,
                        layout: SelectLayout::Compact,
                        hide_indexes: true,
                    )
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;
    use futures::{StreamExt, stream};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn key(code: KeyCode) -> TerminalEvent {
        TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code))
    }

    #[test]
    fn enter_plan_mode_permission_request_renders_official_copy() {
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                EnterPlanModePermissionRequest()
            }
        }
        .render(Some(120))
        .to_string();

        assert!(text.contains("Enter plan mode?"), "canvas=\n{text}");
        assert!(
            text.contains("Explore the codebase thoroughly"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("No code changes will be made"),
            "canvas=\n{text}"
        );
        assert!(text.contains("Yes, enter plan mode"), "canvas=\n{text}");
        assert!(
            text.contains("No, start implementing now"),
            "canvas=\n{text}"
        );
    }

    #[tokio::test]
    async fn enter_plan_mode_escape_dispatches_no() {
        let selected = Arc::new(Mutex::new(Vec::new()));
        let selected_clone = selected.clone();
        let mut app = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                EnterPlanModePermissionRequest(
                    on_select: move |value| selected_clone.lock().unwrap().push(value),
                )
            }
        };
        let mut render_loop = Box::pin(
            app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(stream::iter(vec![key(KeyCode::Esc)]))
                    .with_size(120, 30),
            ),
        );
        for _ in 0..10 {
            let next = crate::utils::race(render_loop.next(), async {
                futures_timer::Delay::new(Duration::from_millis(100)).await;
                None
            })
            .await;
            if next.is_none() {
                break;
            }
        }
        assert_eq!(
            selected.lock().unwrap().as_slice(),
            &[EnterPlanModePermissionOptionValue::No]
        );
    }
}
