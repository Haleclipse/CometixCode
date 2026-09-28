//! Maps to: CC
//! `components/permissions/PowerShellPermissionRequest/PowerShellPermissionRequest.tsx`.
//!
//! Ports the PowerShell-specific permission dialog boundary: command rendering,
//! option construction, destructive-warning slot, feedback editing, debug
//! decision view, and footer hints. Runtime analytics, explainer requests, and
//! AST prefix refinement remain service-owned snapshots.

pub mod powershell_tool_use_options;

use self::powershell_tool_use_options::{
    PowerShellPermissionOption, PowerShellPermissionOptionKind, PowerShellToolUseOptionValue,
    PowerShellToolUseOptionsInput, powershell_tool_use_options,
};
use super::permission_decision_debug_info::PermissionDecisionDebugInfo;
use super::permission_dialog::PermissionDialog;
use super::permission_explanation::{
    PermissionExplainerContent, PermissionExplanationLoadState, permission_explainer_footer_hint,
    use_permission_explanation,
};
use super::permission_rule_explanation::{PermissionRuleExplanation, PermissionRuleToolType};
use super::use_shell_permission_feedback::{
    ShellPermissionFeedbackState, handle_shell_focus, handle_shell_input_mode_toggle,
};
use super::worker_badge::WorkerBadgeProps;
use crate::components::design_system::list_item::ListItem;
use crate::components::text_input::TextInput;
use crate::keybindings::types::ContextName;
use crate::keybindings::use_keybinding::use_keybinding;
use crate::types::message::Message;
use crate::types::permissions::{
    PermissionBehavior, PermissionPromptChoice, PermissionPromptResponse,
    PermissionRequest as PermissionRequestData, PermissionUpdate, PermissionUpdateDestination,
};
use crate::utils::theme::Theme;
use iocraft::prelude::*;
use std::sync::Arc;

#[derive(Default, Props)]
pub struct PowerShellPermissionRequestProps<'a> {
    pub request: Option<PermissionRequestData>,
    pub worker_badge: Option<WorkerBadgeProps>,
    pub suggestions: Vec<PermissionUpdate>,
    pub runtime_permission_result:
        Option<crate::utils::permissions::permission_result::PermissionDecision>,
    pub permission_context: Option<crate::tool::ToolPermissionContext>,
    pub debug_option_enabled: bool,
    pub destructive_warning: Option<String>,
    pub explainer_visible: bool,
    pub explainer_enabled: bool,
    pub explainer_load_state: PermissionExplanationLoadState,
    pub messages: Arc<Vec<Message>>,
    pub show_always_allow_options: bool,
    pub editable_prefix: Option<String>,
    pub editable_prefix_input_enabled: bool,
    pub yes_input_mode: bool,
    pub no_input_mode: bool,
    pub current_cwd: Option<String>,
    pub on_select: HandlerMut<'a, PowerShellPermissionSelection>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PowerShellPermissionSelection {
    pub value: PowerShellToolUseOptionValue,
    pub accept_feedback: Option<String>,
    pub reject_feedback: Option<String>,
    pub editable_prefix: Option<String>,
}

impl PowerShellPermissionSelection {
    pub fn new(value: PowerShellToolUseOptionValue) -> Self {
        Self {
            value,
            accept_feedback: None,
            reject_feedback: None,
            editable_prefix: None,
        }
    }
}

fn default_request() -> PermissionRequestData {
    PermissionRequestData {
        permission_result: None,
        id: String::new(),
        tool_use_id: String::new(),
        tool_name: "PowerShell".to_string(),
        mcp_info: None,
        decision_reason: None,
        description: String::new(),
        message: String::new(),
        input_summary: String::new(),
        input: serde_json::Value::Null,
        call_input: None,
        rule: crate::types::permissions::PermissionRuleValue::new("PowerShell", None),
        suggestions: Vec::new(),
        blocked_path: None,
        metadata: None,
        is_compound_command: false,
        mode: crate::types::permissions::PermissionMode::Default,
    }
}

/// Maps to: CC `PowerShellPermissionRequest.tsx:35-37`
/// `const { command, description } = PowerShellTool.inputSchema.parse(toolUseConfirm.input)`.
///
/// Same shape as the Bash dialog: CC destructures the parsed input with no
/// fallback. The removed `request.input_summary` fallback substituted the
/// permission-rule-content projection for a display string.
pub fn powershell_command_from_request(request: &PermissionRequestData) -> String {
    request
        .input
        .get("command")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Maps to: CC optional `description` from `PowerShellTool.inputSchema`.
pub fn powershell_description_from_request(request: &PermissionRequestData) -> Option<String> {
    request
        .input
        .get("description")
        .and_then(serde_json::Value::as_str)
        .filter(|description| !description.trim().is_empty())
        .map(str::to_string)
}

/// Maps to: CC `onSelect(...)` branches: yes allows once, persistent yes
/// variants add allow rules/descriptions, and no rejects.
pub fn powershell_tool_use_option_to_prompt_choice(
    value: PowerShellToolUseOptionValue,
) -> PermissionPromptChoice {
    match value {
        PowerShellToolUseOptionValue::Yes => PermissionPromptChoice::AllowOnce,
        PowerShellToolUseOptionValue::YesApplySuggestions
        | PowerShellToolUseOptionValue::YesPrefixEdited => PermissionPromptChoice::AlwaysAllow,
        PowerShellToolUseOptionValue::No => PermissionPromptChoice::Deny,
    }
}

fn trimmed_option(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// Maps to CC `PowerShellPermissionRequest.tsx#onSelect` calling
/// `toolUseConfirm.onAllow(toolUseConfirm.input, permissionUpdates, feedback)`
/// or `toolUseConfirm.onReject(feedback)`.
pub fn powershell_tool_use_selection_to_prompt_response(
    selection: &PowerShellPermissionSelection,
    request: &PermissionRequestData,
    suggestions: &[PermissionUpdate],
) -> PermissionPromptResponse {
    let choice = powershell_tool_use_option_to_prompt_choice(selection.value);
    let mut response = PermissionPromptResponse::new(choice);
    match selection.value {
        PowerShellToolUseOptionValue::Yes => {
            response = response.with_permission_updates(Vec::new());
            if let Some(feedback) = trimmed_option(selection.accept_feedback.as_deref()) {
                response = response.with_feedback(feedback);
            }
        }
        PowerShellToolUseOptionValue::YesApplySuggestions => {
            response = response.with_permission_updates(suggestions.to_vec());
        }
        PowerShellToolUseOptionValue::YesPrefixEdited => {
            response = response.with_permission_updates(
                selection
                    .editable_prefix
                    .as_deref()
                    .map(str::trim)
                    .filter(|prefix| !prefix.is_empty())
                    .map(|prefix| {
                        vec![PermissionUpdate::AddRules {
                            destination: PermissionUpdateDestination::LocalSettings,
                            behavior: PermissionBehavior::Allow,
                            rules: vec![crate::types::permissions::PermissionRuleValue::new(
                                request.tool_name.clone(),
                                Some(prefix.to_string()),
                            )],
                        }]
                    })
                    .unwrap_or_default(),
            );
        }
        PowerShellToolUseOptionValue::No => {
            if let Some(feedback) = trimmed_option(selection.reject_feedback.as_deref()) {
                response = response.with_feedback(feedback);
            }
        }
    }
    response
}

/// Backwards-compatible value adapter for tests and callers that do not collect
/// shell feedback yet.
pub fn powershell_tool_use_option_to_prompt_response(
    value: PowerShellToolUseOptionValue,
    request: &PermissionRequestData,
    suggestions: &[PermissionUpdate],
    editable_prefix: Option<&str>,
) -> PermissionPromptResponse {
    let mut selection = PowerShellPermissionSelection::new(value);
    selection.editable_prefix = editable_prefix.map(str::to_string);
    powershell_tool_use_selection_to_prompt_response(&selection, request, suggestions)
}

fn current_cwd(props: &PowerShellPermissionRequestProps<'_>) -> String {
    props.current_cwd.clone().unwrap_or_else(|| {
        std::env::current_dir()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| ".".to_string())
    })
}

fn build_options(
    props: &PowerShellPermissionRequestProps<'_>,
    yes_input_mode: bool,
    no_input_mode: bool,
    editable_prefix: Option<String>,
) -> Vec<PowerShellPermissionOption> {
    powershell_tool_use_options(PowerShellToolUseOptionsInput {
        suggestions: props.suggestions.clone(),
        yes_input_mode,
        no_input_mode,
        editable_prefix,
        editable_prefix_input_enabled: props.editable_prefix_input_enabled,
        show_always_allow_options: props.show_always_allow_options,
        current_cwd: current_cwd(props),
    })
}

fn sync_feedback_text(
    state: &mut ShellPermissionFeedbackState,
    accept_feedback: &str,
    reject_feedback: &str,
) {
    state.accept_feedback = accept_feedback.to_string();
    state.reject_feedback = reject_feedback.to_string();
}

/// Maps to: CC `PowerShellPermissionRequest` render path.
#[component]
pub fn PowerShellPermissionRequest<'a>(
    props: &mut PowerShellPermissionRequestProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let mut show_permission_debug = hooks.use_state(|| false);
    let runtime = hooks
        .try_use_context::<crate::keybindings::keybinding_context::KeybindingRuntime>()
        .map(|runtime| runtime.clone());
    use_keybinding(
        &mut hooks,
        runtime.clone(),
        "permission:toggleDebug",
        ContextName::Confirmation,
        || true,
        move || {
            show_permission_debug.set(!show_permission_debug.get());
            true
        },
    );
    let request = props.request.clone().unwrap_or_else(default_request);
    let explanation = use_permission_explanation(
        &mut hooks,
        props.explainer_enabled,
        props.explainer_visible,
        props.explainer_load_state.clone(),
        request.tool_name.clone(),
        request.input.clone(),
        (!request.description.is_empty()).then(|| request.description.clone()),
        Arc::clone(&props.messages),
    );
    props.explainer_visible = explanation.visible();
    props.explainer_load_state = explanation.load_state();
    let command = powershell_command_from_request(&request);
    let input_description = powershell_description_from_request(&request);
    let mut feedback_state = hooks.use_state(ShellPermissionFeedbackState::default);
    let accept_feedback = hooks.use_state(String::new);
    let accept_feedback_cursor = hooks.use_state(|| 0usize);
    let reject_feedback = hooks.use_state(String::new);
    let reject_feedback_cursor = hooks.use_state(|| 0usize);
    let editable_prefix_value =
        hooks.use_state(|| props.editable_prefix.clone().unwrap_or_default());
    let editable_prefix_cursor =
        hooks.use_state(|| props.editable_prefix.as_ref().map(|s| s.len()).unwrap_or(0));

    let feedback_snapshot = feedback_state.read().clone();
    let yes_input_mode = props.yes_input_mode || feedback_snapshot.yes_input_mode;
    let no_input_mode = props.no_input_mode || feedback_snapshot.no_input_mode;
    let editable_prefix_for_options = props
        .editable_prefix
        .as_ref()
        .map(|_| editable_prefix_value.read().clone());
    let options = build_options(
        props,
        yes_input_mode,
        no_input_mode,
        editable_prefix_for_options,
    );
    let option_count = options.len().max(1);
    let mut focused_index = hooks.use_state(|| 0usize);
    let mut pending_select = hooks.use_state(|| Option::<PowerShellToolUseOptionValue>::None);

    for (action, direction) in [("select:previous", -1isize), ("select:next", 1isize)] {
        let active_options = options.clone();
        let handler_options = options.clone();
        let mut feedback_state_for_handler = feedback_state;
        crate::keybindings::use_keybinding::use_keybinding(
            &mut hooks,
            runtime.clone(),
            action,
            ContextName::Select,
            move || {
                active_options
                    .get(
                        focused_index
                            .get()
                            .min(active_options.len().saturating_sub(1)),
                    )
                    .is_some_and(|option| {
                        !matches!(option.kind, PowerShellPermissionOptionKind::Input { .. })
                    })
            },
            move || {
                let len = handler_options.len();
                if len == 0 {
                    return true;
                }
                let current = focused_index.get().min(len - 1);
                let next = if direction < 0 {
                    if current == 0 { len - 1 } else { current - 1 }
                } else {
                    (current + 1) % len
                };
                if let Some(option) = handler_options.get(next) {
                    let mut state = feedback_state_for_handler.read().clone();
                    sync_feedback_text(
                        &mut state,
                        &accept_feedback.read(),
                        &reject_feedback.read(),
                    );
                    handle_shell_focus(&mut state, option.value.as_str());
                    feedback_state_for_handler.set(state);
                }
                focused_index.set(next);
                true
            },
        );
    }
    crate::keybindings::use_keybinding::use_keybinding(
        &mut hooks,
        runtime.clone(),
        "select:accept",
        ContextName::Select,
        {
            let options = options.clone();
            move || {
                options
                    .get(focused_index.get().min(options.len().saturating_sub(1)))
                    .is_some_and(|option| {
                        !matches!(option.kind, PowerShellPermissionOptionKind::Input { .. })
                    })
            }
        },
        {
            let options = options.clone();
            move || {
                if let Some(option) =
                    options.get(focused_index.get().min(options.len().saturating_sub(1)))
                {
                    pending_select.set(Some(option.value));
                }
                true
            }
        },
    );
    crate::keybindings::use_keybinding::use_keybinding(
        &mut hooks,
        runtime,
        "select:cancel",
        ContextName::Select,
        || true,
        move || {
            pending_select.set(Some(PowerShellToolUseOptionValue::No));
            true
        },
    );

    hooks.use_propagated_terminal_events({
        let mut feedback_state = feedback_state;
        let accept_feedback = accept_feedback;
        let reject_feedback = reject_feedback;
        let options = options.clone();
        move |event| {
            let TerminalEvent::Key(KeyEvent {
                code,
                kind,
                ..
            }) = event.event()
            else {
                return;
            };
            if *kind == KeyEventKind::Release {
                return;
            }
            let current = focused_index.get().min(options.len().saturating_sub(1));
            match code {
                KeyCode::Tab => {
                    if let Some(option) = options.get(current) {
                        let mut state = feedback_state.read().clone();
                        sync_feedback_text(
                            &mut state,
                            &accept_feedback.read(),
                            &reject_feedback.read(),
                        );
                        handle_shell_input_mode_toggle(&mut state, option.value.as_str());
                        feedback_state.set(state);
                        event.stop_propagation();
                    }
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
        let mut selection = PowerShellPermissionSelection::new(value);
        selection.accept_feedback = trimmed_option(Some(&accept_feedback.read()));
        selection.reject_feedback = trimmed_option(Some(&reject_feedback.read()));
        selection.editable_prefix = props
            .editable_prefix
            .as_ref()
            .map(|_| editable_prefix_value.read().clone());
        (props.on_select)(selection);
    }

    let focused = focused_index.get().min(option_count - 1);
    let command_display =
        crate::tools::powershell_tool::ui::render_tool_use_message(Some(&command), true)
            .unwrap_or_else(|| command.clone());
    let description_to_show = if props.explainer_visible {
        None
    } else {
        request
            .description
            .trim()
            .is_empty()
            .then(|| input_description.clone())
            .unwrap_or_else(|| Some(request.description.clone()))
            .filter(|description| !description.trim().is_empty())
    };
    let show_tab_to_amend = options
        .get(focused)
        .is_some_and(|option| match option.value {
            PowerShellToolUseOptionValue::Yes => !yes_input_mode,
            PowerShellToolUseOptionValue::No => !no_input_mode,
            _ => false,
        });
    let mut footer_parts = vec!["Esc to cancel".to_string()];
    if show_tab_to_amend {
        footer_parts.push("Tab to amend".to_string());
    }
    if let Some(explainer_hint) =
        permission_explainer_footer_hint(props.explainer_enabled, props.explainer_visible)
    {
        footer_parts.push(explainer_hint);
    }
    let footer_text = footer_parts.join(" · ");

    if show_permission_debug.get() {
        return element! {
            PermissionDialog(
                title: "PowerShell command".to_string(),
                worker_badge: props.worker_badge.clone(),
            ) {
                View(flex_direction: FlexDirection::Column, padding_left: 2u32, padding_right: 2u32, padding_top: 1u32, padding_bottom: 1u32) {
                    Text(content: command_display.clone(), color: theme.inactive, wrap: TextWrap::Wrap)
                    #(description_to_show.clone().map(|description| element! {
                        Text(content: description, color: theme.inactive, wrap: TextWrap::Wrap)
                    }))
                    PermissionExplainerContent(
                        visible: props.explainer_visible,
                        load_state: props.explainer_load_state.clone(),
                    )
                }
                PermissionDecisionDebugInfo(
                    runtime_permission_result: props.runtime_permission_result.clone(),
                    tool_name: Some("PowerShell".to_string()),
                    permission_context: props.permission_context.clone(),
                )
                #(props.debug_option_enabled.then(|| element! {
                    View(justify_content: JustifyContent::FLEX_END, margin_top: 1u32) {
                        Text(content: "Ctrl-D to hide debug info".to_string(), color: theme.inactive)
                    }
                }))
            }
        }
        .into_any();
    }

    element! {
        PermissionDialog(
            title: "PowerShell command".to_string(),
            worker_badge: props.worker_badge.clone(),
        ) {
            View(flex_direction: FlexDirection::Column, padding_left: 2u32, padding_right: 2u32, padding_top: 1u32, padding_bottom: 1u32) {
                Text(content: command_display, color: theme.inactive, wrap: TextWrap::Wrap)
                #(description_to_show.map(|description| element! {
                    Text(content: description, color: theme.inactive, wrap: TextWrap::Wrap)
                }))
                PermissionExplainerContent(
                    visible: props.explainer_visible,
                    load_state: props.explainer_load_state.clone(),
                )
            }
            View(flex_direction: FlexDirection::Column) {
                // Maps to: CC `PowerShellPermissionRequest.tsx:276-279` —
                // mounted inside the same `flexDirection="column"` Box,
                // immediately before the `destructiveWarning` block.
                PermissionRuleExplanation(
                    decision_reason: request.decision_reason.clone(),
                    tool_type: PermissionRuleToolType::Command,
                    permission_mode: request.mode,
                )
                #(props.destructive_warning.as_ref().filter(|warning| !warning.trim().is_empty()).map(|warning| element! {
                    View(margin_bottom: 1u32) {
                        Text(content: warning.clone(), color: theme.warning, wrap: TextWrap::Wrap)
                    }
                }))
                Text(content: "Do you want to proceed?".to_string(), wrap: TextWrap::NoWrap)
                View(flex_direction: FlexDirection::Column) {
                    #(options.iter().enumerate().map(|(idx, option)| {
                        let is_focused = idx == focused;
                        let option_color = if is_focused { Some(theme.suggestion) } else { None };
                        match &option.kind {
                            PowerShellPermissionOptionKind::Select => element! {
                                ListItem(
                                    is_focused: is_focused,
                                    styled: Some(false),
                                ) {
                                    Text(content: option.label.clone(), color: option_color, wrap: TextWrap::NoWrap)
                                }
                            }.into_any(),
                            PowerShellPermissionOptionKind::Input {
                                placeholder,
                                initial_value: _,
                                show_label_with_value: _,
                                label_value_separator,
                            } => {
                                let (value_state, cursor_state) = match option.value {
                                    PowerShellToolUseOptionValue::Yes => (accept_feedback, accept_feedback_cursor),
                                    PowerShellToolUseOptionValue::No => (reject_feedback, reject_feedback_cursor),
                                    _ => (editable_prefix_value, editable_prefix_cursor),
                                };
                                let value_for_static = value_state.read().clone();
                                let separator = label_value_separator
                                    .clone()
                                    .unwrap_or_else(|| ", ".to_string());
                                let option_value = option.value;
                                let mut pending_select_for_submit = pending_select;
                                let mut pending_select_for_exit = pending_select;
                                element! {
                                    ListItem(
                                        is_focused: is_focused,
                                        styled: Some(false),
                                        declare_cursor: Some(false),
                                    ) {
                                        View(flex_direction: FlexDirection::Row, flex_shrink: 0.0f32) {
                                            Text(content: option.label.clone(), color: option_color, wrap: TextWrap::NoWrap)
                                            #(if is_focused {
                                                Some(element! {
                                                    View(flex_direction: FlexDirection::Row) {
                                                        Text(content: separator.clone(), color: option_color, wrap: TextWrap::NoWrap)
                                                        TextInput(
                                                            value: Some(value_state),
                                                            cursor_offset: Some(cursor_state),
                                                            focus: true,
                                                            multiline: true,
                                                            columns: 80usize,
                                                            show_cursor: true,
                                                            placeholder: Some(placeholder.clone()),
                                                            on_submit: move |_| {
                                                                pending_select_for_submit.set(Some(option_value));
                                                            },
                                                            on_exit: move |_| {
                                                                pending_select_for_exit.set(Some(PowerShellToolUseOptionValue::No));
                                                            },
                                                        )
                                                    }
                                                }.into_any())
                                            } else if !value_for_static.is_empty() {
                                                Some(element! {
                                                    Text(content: format!("{}{}", separator, value_for_static), color: option_color, wrap: TextWrap::NoWrap)
                                                }.into_any())
                                            } else {
                                                None
                                            })
                                        }
                                    }
                                }.into_any()
                            }
                        }
                    }).collect::<Vec<_>>())
                }
            }
            View(justify_content: JustifyContent::SPACE_BETWEEN, margin_top: 1u32) {
                Text(
                    content: footer_text,
                    color: theme.inactive,
                    wrap: TextWrap::NoWrap,
                )
            }
        }
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::permissions::{
        PermissionBehavior, PermissionMode, PermissionPromptChoice, PermissionRuleSource,
        PermissionRuleValue, PermissionUpdateDestination,
    };
    use crate::utils::permissions::permission_explainer::{PermissionExplanation, RiskLevel};
    use crate::utils::theme;
    use futures::{StreamExt, stream};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn key(code: KeyCode) -> TerminalEvent {
        TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code))
    }

    fn powershell_request(command: &str, description: &str) -> PermissionRequestData {
        PermissionRequestData {
            permission_result: None,
            id: "req".to_string(),
            tool_use_id: "toolu".to_string(),
            tool_name: "PowerShell".to_string(),
            mcp_info: None,
            decision_reason: None,
            description: description.to_string(),
            message: String::new(),
            input_summary: command.to_string(),
            input: serde_json::json!({ "command": command, "description": description }),
            call_input: None,
            rule: PermissionRuleValue::new("PowerShell", Some(command.to_string())),
            suggestions: Vec::new(),
            blocked_path: None,
            metadata: None,
            is_compound_command: false,
            mode: PermissionMode::Default,
        }
    }

    fn render_powershell(props: PowerShellPermissionRequestProps<'static>) -> String {
        element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                PowerShellPermissionRequest(
                    request: props.request,
                    worker_badge: props.worker_badge,
                    suggestions: props.suggestions,
                    destructive_warning: props.destructive_warning,
                    explainer_visible: props.explainer_visible,
                    explainer_enabled: props.explainer_enabled,
                    explainer_load_state: props.explainer_load_state,
                    show_always_allow_options: props.show_always_allow_options,
                    editable_prefix: props.editable_prefix,
                    editable_prefix_input_enabled: props.editable_prefix_input_enabled,
                    yes_input_mode: props.yes_input_mode,
                    no_input_mode: props.no_input_mode,
                    current_cwd: props.current_cwd,
                )
            }
        }
        .render(Some(120))
        .to_string()
    }

    #[test]
    fn powershell_permission_helpers_extract_command_and_choice_mapping() {
        let request = powershell_request("Get-Process", "List processes");
        assert_eq!(powershell_command_from_request(&request), "Get-Process");
        assert_eq!(
            powershell_description_from_request(&request).as_deref(),
            Some("List processes")
        );
        assert_eq!(
            powershell_tool_use_option_to_prompt_choice(PowerShellToolUseOptionValue::Yes),
            PermissionPromptChoice::AllowOnce
        );
        assert_eq!(
            powershell_tool_use_option_to_prompt_choice(
                PowerShellToolUseOptionValue::YesPrefixEdited
            ),
            PermissionPromptChoice::AlwaysAllow
        );
        assert_eq!(
            powershell_tool_use_option_to_prompt_choice(PowerShellToolUseOptionValue::No),
            PermissionPromptChoice::Deny
        );
    }

    #[test]
    fn powershell_permission_request_renders_official_dialog_copy_options_and_footer() {
        let text = render_powershell(PowerShellPermissionRequestProps {
            request: Some(powershell_request("Get-ChildItem -Recurse", "List files")),
            worker_badge: Some(WorkerBadgeProps {
                name: "builder".to_string(),
                color: None,
            }),
            show_always_allow_options: true,
            current_cwd: Some("C:/repo".to_string()),
            suggestions: vec![PermissionUpdate::AddRules {
                destination: PermissionUpdateDestination::LocalSettings,
                behavior: PermissionBehavior::Allow,
                rules: vec![PermissionRuleValue::new(
                    "PowerShell",
                    Some("Get-ChildItem:*".to_string()),
                )],
            }],
            ..PowerShellPermissionRequestProps::default()
        });

        assert!(text.contains("PowerShell command"), "canvas=\n{text}");
        assert!(text.contains("· @builder"), "canvas=\n{text}");
        assert!(text.contains("Get-ChildItem -Recurse"), "canvas=\n{text}");
        assert!(text.contains("List files"), "canvas=\n{text}");
        assert!(text.contains("Do you want to proceed?"), "canvas=\n{text}");
        assert!(text.contains("Yes"), "canvas=\n{text}");
        assert!(
            text.contains("Yes, and don't ask again for Get-ChildItem commands in C:/repo"),
            "canvas=\n{text}"
        );
        assert!(text.contains("No"), "canvas=\n{text}");
        assert!(
            text.contains("Esc to cancel · Tab to amend"),
            "canvas=\n{text}"
        );
        assert!(
            !text.contains("Enter to confirm · Esc to cancel"),
            "PowerShellPermissionRequest uses PermissionDialog, not generic Dialog guide; canvas=\n{text}"
        );
    }

    /// Maps to: CC `PowerShellPermissionRequest.tsx:275-284` —
    /// `<PermissionRuleExplanation permissionResult={toolUseConfirm.permissionResult}
    /// toolType="command" />` is the first child of the `flexDirection="column"`
    /// Box, immediately before the `destructiveWarning` block, with `toolType`
    /// `"command"` at `:278`. `PermissionRuleExplanation.tsx:95-97` renders
    /// nothing when there is no reason.
    #[test]
    fn powershell_permission_request_matches_official_rule_explanation_mount() {
        let mut request = powershell_request("Remove-Item -Recurse", "Delete a tree");
        request.decision_reason = Some(
            crate::utils::permissions::permission_result::PermissionDecisionReason::Rule {
                rule: crate::types::permissions::PermissionRule {
                    source: PermissionRuleSource::LocalSettings,
                    rule_behavior: PermissionBehavior::Ask,
                    rule_value: PermissionRuleValue::new(
                        "PowerShell",
                        Some("Remove-Item:*".to_string()),
                    ),
                },
            },
        );

        let with_reason = render_powershell(PowerShellPermissionRequestProps {
            request: Some(request),
            destructive_warning: Some("This command may be destructive".to_string()),
            ..PowerShellPermissionRequestProps::default()
        });

        assert!(
            with_reason
                .contains("Permission rule PowerShell(Remove-Item:*) requires confirmation for"),
            "canvas=\n{with_reason}"
        );
        assert!(
            with_reason.contains("command."),
            "toolType is \"command\" at CC :278; canvas=\n{with_reason}"
        );
        // CC :276-279 sits ABOVE the :280-284 destructiveWarning block.
        let explanation_at = with_reason
            .find("Permission rule PowerShell(Remove-Item:*)")
            .expect("explanation rendered");
        let warning_at = with_reason
            .find("This command may be destructive")
            .expect("destructive warning rendered");
        assert!(
            explanation_at < warning_at,
            "explanation must precede the destructive warning; canvas=\n{with_reason}"
        );

        let without_reason = render_powershell(PowerShellPermissionRequestProps {
            request: Some(powershell_request("Remove-Item -Recurse", "Delete a tree")),
            ..PowerShellPermissionRequestProps::default()
        });
        assert!(
            !without_reason.contains("Permission rule"),
            "canvas=\n{without_reason}"
        );
        assert!(
            !without_reason.contains("/permissions to update rules"),
            "canvas=\n{without_reason}"
        );
    }

    #[test]
    fn powershell_permission_request_renders_explainer_content_and_footer_hint() {
        let text = render_powershell(PowerShellPermissionRequestProps {
            request: Some(powershell_request("Get-ChildItem", "List files")),
            explainer_visible: true,
            explainer_enabled: true,
            explainer_load_state: PermissionExplanationLoadState::Ready(PermissionExplanation {
                risk_level: RiskLevel::Medium,
                explanation: "Lists directory entries.".to_string(),
                reasoning: "I need to inspect available files.".to_string(),
                risk: "Could reveal filenames".to_string(),
            }),
            ..PowerShellPermissionRequestProps::default()
        });

        assert!(text.contains("Get-ChildItem"), "canvas=\n{text}");
        assert!(
            !text.contains("List files"),
            "official hides the request description when explainer is visible; canvas=\n{text}"
        );
        assert!(text.contains("Lists directory entries."), "canvas=\n{text}");
        assert!(
            text.contains("I need to inspect available files."),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("Med risk: Could reveal filenames"),
            "canvas=\n{text}"
        );
        assert!(text.contains("ctrl+e to hide"), "canvas=\n{text}");
    }

    #[test]
    fn powershell_permission_response_preserves_shell_feedback() {
        let request = powershell_request("Get-Process", "List processes");
        let mut accept_selection =
            PowerShellPermissionSelection::new(PowerShellToolUseOptionValue::Yes);
        accept_selection.accept_feedback = Some("  inspect the process list  ".to_string());
        let accept_response =
            powershell_tool_use_selection_to_prompt_response(&accept_selection, &request, &[]);
        assert_eq!(accept_response.choice, PermissionPromptChoice::AllowOnce);
        assert_eq!(
            accept_response.feedback.as_deref(),
            Some("inspect the process list")
        );
        assert!(accept_response.permission_updates_explicit);

        let mut reject_selection =
            PowerShellPermissionSelection::new(PowerShellToolUseOptionValue::No);
        reject_selection.reject_feedback = Some("  use Get-Service instead  ".to_string());
        let reject_response =
            powershell_tool_use_selection_to_prompt_response(&reject_selection, &request, &[]);
        assert_eq!(reject_response.choice, PermissionPromptChoice::Deny);
        assert_eq!(
            reject_response.feedback.as_deref(),
            Some("use Get-Service instead")
        );
    }

    #[test]
    fn powershell_permission_request_renders_destructive_warning() {
        let text = render_powershell(PowerShellPermissionRequestProps {
            request: Some(powershell_request(
                "Remove-Item -Recurse target",
                "Remove output",
            )),
            destructive_warning: Some("This command may be destructive".to_string()),
            ..PowerShellPermissionRequestProps::default()
        });

        assert!(text.contains("PowerShell command"), "canvas=\n{text}");
        assert!(
            text.contains("This command may be destructive"),
            "canvas=\n{text}"
        );
    }

    #[test]
    fn powershell_permission_request_enter_and_escape_dispatch_official_option_values() {
        let selected = Arc::new(Mutex::new(Vec::new()));
        let selected_for_handler = Arc::clone(&selected);

        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(
                    crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings()
                )) {
                    ContextProvider(value: Context::owned(*theme::current())) {
                        PowerShellPermissionRequest(
                        request: Some(powershell_request("Get-Process", "List processes")),
                        on_select: move |value| {
                            selected_for_handler.lock().expect("selected mutex").push(value);
                        },
                        )
                    }
                }
            };
            let mut render_loop = Box::pin(
                app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(stream::iter(vec![
                        key(KeyCode::Down),
                        key(KeyCode::Enter),
                        key(KeyCode::Esc),
                    ]))
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
        });

        assert_eq!(
            selected
                .lock()
                .expect("selected mutex")
                .iter()
                .map(|selection| selection.value)
                .collect::<Vec<_>>(),
            vec![PowerShellToolUseOptionValue::No]
        );
    }
}
