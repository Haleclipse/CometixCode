//! Maps to: CC
//! `components/permissions/ExitPlanModePermissionRequest/ExitPlanModePermissionRequest.tsx`.
//!
//! Ports the plan-approval shell, empty-plan branch, feedback input, requested
//! permission preview, configurable external-editor handoff, and rejected-plan
//! clipboard images through model-visible permission content blocks.
//! Analytics/session naming and networked Ultraplan remain outside this component.

use super::permission_dialog::PermissionDialog;
use super::permission_rule_explanation::{PermissionRuleExplanation, PermissionRuleToolType};
use super::worker_badge::WorkerBadgeProps;
use crate::components::custom_select::{
    Select, SelectInputOptionData, SelectLayout, SelectOptionData,
};
use crate::components::markdown::Markdown;
use crate::components::prompt_input::input_paste::PastedContent;
use crate::types::permissions::{
    PermissionContentBlock, PermissionMode, PermissionPromptChoice, PermissionPromptResponse,
    PermissionRequest as PermissionRequestData, PermissionRuleValue,
};
use crate::utils::prompt_editor::{EditorResult, ExternalEditorRuntime};
use crate::utils::theme::Theme;
use iocraft::prelude::*;
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitPlanModePermissionOptionValue {
    YesAcceptEditsKeepContext,
    YesDefaultKeepContext,
    No,
}

impl ExitPlanModePermissionOptionValue {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::YesAcceptEditsKeepContext => "yes-accept-edits-keep-context",
            Self::YesDefaultKeepContext => "yes-default-keep-context",
            Self::No => "no",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExitPlanModePermissionOption {
    pub label: String,
    pub value: ExitPlanModePermissionOptionValue,
    pub description: Option<String>,
}

impl ExitPlanModePermissionOption {
    fn select(label: impl Into<String>, value: ExitPlanModePermissionOptionValue) -> Self {
        Self {
            label: label.into(),
            value,
            description: None,
        }
    }

    fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn to_select_option(&self) -> SelectOptionData {
        SelectOptionData {
            label: self.label.clone(),
            description: self.description.clone(),
            dim_description: true,
            value: self.value.as_str().to_string(),
            disabled: false,
            input: None,
        }
    }
}

#[derive(Default, Props)]
pub struct ExitPlanModePermissionRequestProps<'a> {
    pub request: Option<PermissionRequestData>,
    pub worker_badge: Option<WorkerBadgeProps>,
    pub on_select: HandlerMut<'a, ExitPlanModePermissionOptionValue>,
    pub on_select_detail: HandlerMut<'a, ExitPlanModePermissionSelection>,
    /// Deterministic adapter seam for permission image-paste tests.
    pub clipboard_image_override: Option<crate::utils::image_paste::ClipboardImage>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExitPlanModePermissionSelection {
    pub value: ExitPlanModePermissionOptionValue,
    pub plan: String,
    pub feedback: Option<String>,
    pub content_blocks: Vec<PermissionContentBlock>,
}

fn default_request() -> PermissionRequestData {
    PermissionRequestData {
        permission_result: None,
        id: String::new(),
        tool_use_id: String::new(),
        tool_name: "ExitPlanMode".to_string(),
        mcp_info: None,
        decision_reason: None,
        description: String::new(),
        message: String::new(),
        input_summary: String::new(),
        input: serde_json::Value::Null,
        call_input: None,
        rule: PermissionRuleValue::new("ExitPlanMode", None),
        suggestions: Vec::new(),
        blocked_path: None,
        metadata: None,
        is_compound_command: false,
        mode: PermissionMode::Plan,
    }
}

/// Maps to: CC `buildPlanApprovalOptions(...)` for the currently ported
/// non-clear-context, non-Ultraplan subset.
pub fn build_plan_approval_options() -> Vec<ExitPlanModePermissionOption> {
    vec![
        ExitPlanModePermissionOption::select(
            "Yes, auto-accept edits",
            ExitPlanModePermissionOptionValue::YesAcceptEditsKeepContext,
        ),
        ExitPlanModePermissionOption::select(
            "Yes, manually approve edits",
            ExitPlanModePermissionOptionValue::YesDefaultKeepContext,
        ),
        ExitPlanModePermissionOption::select(
            "No, keep planning",
            ExitPlanModePermissionOptionValue::No,
        )
        .with_description("shift+tab to approve with this feedback"),
    ]
}

pub fn empty_plan_options() -> Vec<ExitPlanModePermissionOption> {
    vec![
        ExitPlanModePermissionOption::select(
            "Yes",
            ExitPlanModePermissionOptionValue::YesDefaultKeepContext,
        ),
        ExitPlanModePermissionOption::select("No", ExitPlanModePermissionOptionValue::No),
    ]
}

pub fn exit_plan_mode_permission_option_to_prompt_choice(
    value: ExitPlanModePermissionOptionValue,
) -> PermissionPromptChoice {
    match value {
        ExitPlanModePermissionOptionValue::YesAcceptEditsKeepContext => {
            PermissionPromptChoice::AlwaysAllow
        }
        ExitPlanModePermissionOptionValue::YesDefaultKeepContext => {
            PermissionPromptChoice::AllowOnce
        }
        ExitPlanModePermissionOptionValue::No => PermissionPromptChoice::Deny,
    }
}

/// Maps to: CC `components/permissions/ExitPlanModePermissionRequest/ExitPlanModePermissionRequest.tsx:390-680`.
pub fn exit_plan_mode_selection_to_prompt_response(
    selection: ExitPlanModePermissionSelection,
) -> PermissionPromptResponse {
    let choice = exit_plan_mode_permission_option_to_prompt_choice(selection.value);
    let approved = matches!(
        choice,
        PermissionPromptChoice::AllowOnce | PermissionPromptChoice::AlwaysAllow
    );
    let mut response = PermissionPromptResponse::new(choice);
    if approved {
        response.updated_input = Some(serde_json::json!({ "plan": selection.plan }));
        if let Some(feedback) = selection.feedback {
            response = response.with_feedback(feedback);
        }
    } else {
        let feedback = selection.feedback.or_else(|| {
            (!selection.content_blocks.is_empty()).then(|| "(See attached image)".to_string())
        });
        if let Some(feedback) = feedback {
            response = response.with_feedback(feedback);
        }
        response = response.with_content_blocks(selection.content_blocks);
    }
    response
}

fn permission_image_blocks(
    pasted_contents: &BTreeMap<usize, PastedContent>,
) -> Vec<PermissionContentBlock> {
    pasted_contents
        .values()
        .filter_map(|content| match content {
            PastedContent::Image {
                media_type,
                data: Some(data),
                ..
            } => Some(PermissionContentBlock::image_base64(
                media_type
                    .clone()
                    .unwrap_or_else(|| "image/png".to_string()),
                data.clone(),
            )),
            _ => None,
        })
        .collect()
}

/// Maps to: CC `components/permissions/ExitPlanModePermissionRequest/ExitPlanModePermissionRequest.tsx:255-274`.
fn cache_and_store_permission_image(id: usize, image: &crate::utils::image_paste::ClipboardImage) {
    let stored = crate::utils::image_store::PastedImageContent {
        id: id as u64,
        media_type: Some(image.media_type.clone()),
        data: Some(image.base64.clone()),
    };
    crate::utils::image_store::cache_image_path(&stored);
    std::thread::spawn(move || {
        let _ = crate::utils::image_store::store_image(&stored);
    });
}

pub fn plan_content_from_request(request: &PermissionRequestData) -> String {
    request
        .input
        .get("plan")
        .and_then(serde_json::Value::as_str)
        .filter(|plan| !plan.trim().is_empty())
        .map(str::to_string)
        .or_else(|| {
            (!request.input_summary.trim().is_empty()).then(|| request.input_summary.clone())
        })
        .unwrap_or_default()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AllowedPromptPreview {
    pub tool: String,
    pub prompt: String,
}

/// Maps to: CC `allowedPrompts` requested-permissions preview.
pub fn allowed_prompts_from_request(request: &PermissionRequestData) -> Vec<AllowedPromptPreview> {
    request
        .input
        .get("allowedPrompts")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let tool = item.get("tool")?.as_str()?.trim();
                    let prompt = item.get("prompt")?.as_str()?.trim();
                    if tool.is_empty() || prompt.is_empty() {
                        None
                    } else {
                        Some(AllowedPromptPreview {
                            tool: tool.to_string(),
                            prompt: prompt.to_string(),
                        })
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Maps to: CC `ExitPlanModePermissionRequest` render path.
#[component]
pub fn ExitPlanModePermissionRequest<'a>(
    props: &mut ExitPlanModePermissionRequestProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let request = props.request.clone().unwrap_or_else(default_request);
    let initial_plan = plan_content_from_request(&request);
    let mut current_plan = hooks.use_state(|| initial_plan.clone());
    let mut plan_feedback = hooks.use_state(String::new);
    // Maps to: CC `components/permissions/ExitPlanModePermissionRequest/ExitPlanModePermissionRequest.tsx:210-217`.
    let mut pasted_contents = hooks.use_state(BTreeMap::<usize, PastedContent>::new);
    let mut next_paste_id = hooks.use_state(|| 0usize);
    let mut editor_error = hooks.use_state(|| Option::<String>::None);
    let mut show_save_message = hooks.use_state(|| false);
    let mut editor_result = hooks.use_state(|| Option::<EditorResult>::None);
    let editor_runtime = hooks
        .try_use_context::<ExternalEditorRuntime>()
        .map(|runtime| *runtime);
    let external_editor_available = editor_runtime.is_some()
        && crate::utils::prompt_editor::external_editor_command().is_some();
    let editor_channel = hooks.use_const(|| Arc::new(async_channel::unbounded::<String>()));
    let editor_receiver = editor_channel.1.clone();
    hooks.use_future(async move {
        while let Ok(plan) = editor_receiver.recv().await {
            let result = match editor_runtime {
                Some(runtime) => runtime.edit_prompt(&plan).await,
                None => EditorResult {
                    content: None,
                    error: Some("External editor is unavailable".to_string()),
                },
            };
            editor_result.set(Some(result));
        }
    });
    let keybinding_runtime = hooks
        .try_use_context::<crate::keybindings::keybinding_context::KeybindingRuntime>()
        .map(|runtime| runtime.clone());
    let editor_action_active = external_editor_available && !current_plan.read().trim().is_empty();
    let editor_action_plan = current_plan.read().clone();
    let editor_sender_for_action = editor_channel.0.clone();
    crate::keybindings::use_keybinding::use_keybinding(
        &mut hooks,
        keybinding_runtime,
        "chat:externalEditor",
        crate::keybindings::types::ContextName::Chat,
        move || editor_action_active,
        move || {
            let _ = editor_sender_for_action.try_send(editor_action_plan.clone());
            true
        },
    );
    let completed_editor = { editor_result.read().clone() };
    if let Some(result) = completed_editor {
        editor_result.set(None);
        if let Some(error) = result.error {
            editor_error.set(Some(error));
        } else if let Some(content) = result.content {
            if content != *current_plan.read() {
                current_plan.set(content);
                show_save_message.set(true);
            }
            editor_error.set(None);
        }
    }
    let plan = current_plan.read().clone();
    let feedback = plan_feedback.read().clone();
    let is_empty = plan.trim().is_empty();
    let allowed_prompts = allowed_prompts_from_request(&request);
    let mut options = if is_empty {
        empty_plan_options()
    } else {
        build_plan_approval_options()
    };
    if !is_empty {
        if let Some(no) = options
            .iter_mut()
            .find(|option| option.value == ExitPlanModePermissionOptionValue::No)
        {
            no.description = None;
        }
    }
    let option_count = options.len();
    let mut focused_index = hooks.use_state(|| 0usize);
    // The bool distinguishes Select.onChange (feedback/images included) from
    // Select.onCancel (plain rejection), matching CC's separate callbacks.
    let mut pending_select =
        hooks.use_state(|| Option::<(ExitPlanModePermissionOptionValue, bool)>::None);
    let pasted_contents_snapshot = pasted_contents.read().clone();
    let content_blocks = permission_image_blocks(&pasted_contents_snapshot);
    let image_count = content_blocks.len();

    hooks.use_terminal_events({
        let mut focused_index = focused_index;
        let mut pending_select = pending_select;
        let mut plan_feedback = plan_feedback;
        let options = options.clone();
        move |event| {
            let TerminalEvent::Key(KeyEvent {
                code,
                kind,
                modifiers,
                ..
            }) = event
            else {
                return;
            };
            if kind == KeyEventKind::Release {
                return;
            }
            let current = focused_index.get().min(options.len().saturating_sub(1));
            let feedback_focused = options
                .get(current)
                .is_some_and(|option| option.value == ExitPlanModePermissionOptionValue::No)
                && !is_empty;
            match code {
                KeyCode::BackTab => {
                    if let Some(option) = options.iter().find(|option| {
                        option.value == ExitPlanModePermissionOptionValue::YesAcceptEditsKeepContext
                    }) {
                        pending_select.set(Some((option.value, true)));
                    }
                }
                KeyCode::Up | KeyCode::Char('k')
                    if !feedback_focused || matches!(code, KeyCode::Up) =>
                {
                    focused_index.set(focused_index.get().saturating_sub(1));
                }
                KeyCode::Down | KeyCode::Char('j')
                    if !feedback_focused || matches!(code, KeyCode::Down) =>
                {
                    focused_index
                        .set((focused_index.get() + 1).min(options.len().saturating_sub(1)));
                }
                KeyCode::Tab => {
                    focused_index
                        .set((focused_index.get() + 1).min(options.len().saturating_sub(1)));
                }
                KeyCode::Backspace if feedback_focused => {
                    let mut feedback = plan_feedback.read().clone();
                    feedback.pop();
                    plan_feedback.set(feedback);
                }
                KeyCode::Char(c)
                    if feedback_focused
                        && !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    let mut feedback = plan_feedback.read().clone();
                    feedback.push(c);
                    plan_feedback.set(feedback);
                }
                KeyCode::Enter => {
                    if let Some(option) = options.get(focused_index.get()) {
                        if option.value == ExitPlanModePermissionOptionValue::No
                            && plan_feedback.read().trim().is_empty()
                            && image_count == 0
                        {
                            return;
                        }
                        pending_select.set(Some((option.value, true)));
                    }
                }
                KeyCode::Esc => {
                    // CC Select.onCancel rejects without feedback/images.
                    pending_select.set(Some((ExitPlanModePermissionOptionValue::No, false)));
                }
                // No Ctrl+C here: CC's is PermissionRequest's app:interrupt,
                // and Cometix binds Ctrl+C to app:exit (2.0.x semantics).
                _ => {}
            }
        }
    });

    let selected = { pending_select.read().clone() };
    if let Some((value, include_details)) = selected {
        pending_select.set(None);
        (props.on_select)(value);
        (props.on_select_detail)(ExitPlanModePermissionSelection {
            value,
            plan: current_plan.read().clone(),
            feedback: (include_details && !plan_feedback.read().trim().is_empty())
                .then(|| plan_feedback.read().trim().to_string()),
            content_blocks: include_details
                .then(|| content_blocks.clone())
                .unwrap_or_default(),
        });
    }

    let focused = focused_index.get().min(option_count.saturating_sub(1));
    let select_options = options
        .iter()
        .map(|option| {
            let mut select_option = option.to_select_option();
            if !is_empty && option.value == ExitPlanModePermissionOptionValue::No {
                select_option.input = Some(SelectInputOptionData {
                    placeholder: Some("Tell Claude what to change".to_string()),
                    value: feedback.clone(),
                    show_label_with_value: true,
                    label_value_separator: Some(", ".to_string()),
                });
            }
            select_option
        })
        .collect::<Vec<_>>();
    let on_image_paste = Handler::from(move |image: crate::utils::image_paste::ClipboardImage| {
        let mut next_paste_id = next_paste_id;
        let mut pasted_contents = pasted_contents;
        let id = next_paste_id.get();
        next_paste_id.set(id + 1);
        cache_and_store_permission_image(id, &image);
        let mut next = pasted_contents.read().clone();
        next.insert(
            id,
            PastedContent::Image {
                id,
                media_type: Some(image.media_type),
                data: Some(image.base64),
                filename: Some("Pasted image".to_string()),
                dimensions: image.dimensions,
                source_path: None,
            },
        );
        pasted_contents.set(next);
    });
    let on_remove_image = Handler::from(move |id: usize| {
        let mut pasted_contents = pasted_contents;
        let mut next = pasted_contents.read().clone();
        next.remove(&id);
        pasted_contents.set(next);
    });

    if is_empty {
        return element! {
            PermissionDialog(
                color: Some(theme.plan_mode),
                title: "Exit plan mode?".to_string(),
                worker_badge: props.worker_badge.clone(),
            ) {
                View(flex_direction: FlexDirection::Column, padding_left: 1u32, padding_right: 1u32, margin_top: 1u32) {
                    Text(content: "Claude wants to exit plan mode".to_string(), wrap: TextWrap::Wrap)
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
        .into_any();
    }

    element! {
        View(flex_direction: FlexDirection::Column) {
            PermissionDialog(
                color: Some(theme.plan_mode),
                title: "Ready to code?".to_string(),
                inner_padding_x: Some(0u32),
                worker_badge: props.worker_badge.clone(),
            ) {
                View(flex_direction: FlexDirection::Column, margin_top: 1u32) {
                    View(padding_left: 1u32, padding_right: 1u32, flex_direction: FlexDirection::Column) {
                        Text(content: "Here is Claude's plan:".to_string(), wrap: TextWrap::NoWrap)
                    }
                    View(
                        border_style: BorderStyle::Dashed,
                        border_color: theme.subtle,
                        border_left: false,
                        border_right: false,
                        flex_direction: FlexDirection::Column,
                        padding_left: 1u32,
                        padding_right: 1u32,
                        margin_bottom: 1u32,
                    ) {
                        Markdown(content: plan.clone())
                    }
                    View(flex_direction: FlexDirection::Column, padding_left: 1u32, padding_right: 1u32) {
                        PermissionRuleExplanation(
                            decision_reason: request.decision_reason.clone(),
                            tool_type: PermissionRuleToolType::Tool,
                            permission_mode: request.mode,
                        )
                        #(if allowed_prompts.is_empty() {
                            None
                        } else {
                            Some(element! {
                                View(flex_direction: FlexDirection::Column, margin_bottom: 1u32) {
                                    Text(content: "Requested permissions:".to_string(), weight: Weight::Bold, wrap: TextWrap::NoWrap)
                                    #(allowed_prompts.iter().map(|prompt| element! {
                                        Text(
                                            content: format!("  · {}(prompt {})", prompt.tool, prompt.prompt),
                                            color: theme.inactive,
                                            wrap: TextWrap::Wrap,
                                        )
                                    }))
                                }
                            })
                        })
                        Text(content: "Claude has written up a plan and is ready to execute. Would you like to proceed?".to_string(), color: theme.inactive, wrap: TextWrap::Wrap)
                        View(margin_top: 1u32) {
                            Select(
                                options: select_options,
                                focused_index: focused,
                                visible_option_count: option_count,
                                layout: SelectLayout::Compact,
                                hide_indexes: true,
                                pasted_contents: pasted_contents_snapshot,
                                on_remove_image: on_remove_image,
                                on_image_paste: on_image_paste,
                                clipboard_image_override: props.clipboard_image_override.clone(),
                            )
                        }
                    }
                }
            }
            #(external_editor_available.then(|| element! {
                View(flex_direction: FlexDirection::Row, column_gap: 1u32, padding_left: 1u32, padding_right: 1u32, margin_top: 1u32) {
                    Text(content: "ctrl-g to edit in external editor".to_string(), dim: true, wrap: TextWrap::NoWrap)
                    #(show_save_message.get().then(|| element! {
                        Text(content: " · ✓ Plan saved!".to_string(), color: theme.success, wrap: TextWrap::NoWrap)
                    }))
                }
            }))
            #(editor_error.read().clone().map(|error| element! {
                View(padding_left: 1u32, padding_right: 1u32) {
                    Text(content: error, color: theme.warning, wrap: TextWrap::Wrap)
                }
            }))
        }
    }
    .into_any()
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

    fn modified_key(code: KeyCode, modifiers: KeyModifiers) -> TerminalEvent {
        let mut event = KeyEvent::new(KeyEventKind::Press, code);
        event.modifiers = modifiers;
        TerminalEvent::Key(event)
    }

    fn exit_request(plan: &str) -> PermissionRequestData {
        PermissionRequestData {
            permission_result: None,
            id: "req".to_string(),
            tool_use_id: "toolu".to_string(),
            tool_name: "ExitPlanMode".to_string(),
            mcp_info: None,
            decision_reason: None,
            description: String::new(),
            message: String::new(),
            input_summary: String::new(),
            input: serde_json::json!({
                "plan": plan,
                "allowedPrompts": [{"tool":"Bash", "prompt":"run tests"}]
            }),
            call_input: None,
            rule: PermissionRuleValue::new("ExitPlanMode", Some("plan".to_string())),
            suggestions: Vec::new(),
            blocked_path: None,
            metadata: None,
            is_compound_command: false,
            mode: PermissionMode::Plan,
        }
    }

    #[test]
    fn build_plan_approval_options_matches_ported_official_order() {
        let options = build_plan_approval_options();
        assert_eq!(options[0].label, "Yes, auto-accept edits");
        assert_eq!(options[1].label, "Yes, manually approve edits");
        assert_eq!(options[2].label, "No, keep planning");
    }

    #[test]
    fn exit_plan_mode_permission_request_renders_plan_and_requested_permissions() {
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                ExitPlanModePermissionRequest(request: Some(exit_request("## Plan\n- edit files")))
            }
        }
        .render(Some(120))
        .to_string();

        assert!(text.contains("Ready to code?"), "canvas=\n{text}");
        assert!(text.contains("Here is Claude's plan:"), "canvas=\n{text}");
        assert!(text.contains("edit files"), "canvas=\n{text}");
        assert!(text.contains("Requested permissions:"), "canvas=\n{text}");
        assert!(text.contains("Bash(prompt run tests)"), "canvas=\n{text}");
        assert!(text.contains("Yes, auto-accept edits"), "canvas=\n{text}");
    }

    #[test]
    fn exit_plan_mode_permission_request_renders_empty_plan_branch() {
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                ExitPlanModePermissionRequest(request: Some(exit_request("")))
            }
        }
        .render(Some(100))
        .to_string();

        assert!(text.contains("Exit plan mode?"), "canvas=\n{text}");
        assert!(
            text.contains("Claude wants to exit plan mode"),
            "canvas=\n{text}"
        );
        assert!(text.contains("Yes"), "canvas=\n{text}");
        assert!(text.contains("No"), "canvas=\n{text}");
    }

    #[test]
    fn plan_approval_response_carries_updated_plan_and_accept_feedback() {
        let response =
            exit_plan_mode_selection_to_prompt_response(ExitPlanModePermissionSelection {
                value: ExitPlanModePermissionOptionValue::YesDefaultKeepContext,
                plan: "Edited plan".to_string(),
                feedback: Some("also update the README".to_string()),
                // Maps to: CC `ExitPlanModePermissionRequest.tsx:604-634` (approval omits image blocks).
                content_blocks: vec![PermissionContentBlock::image_base64("image/png", "AAAA")],
            });

        assert_eq!(response.choice, PermissionPromptChoice::AllowOnce);
        assert_eq!(
            response.updated_input.as_ref().unwrap()["plan"],
            "Edited plan"
        );
        assert_eq!(response.feedback.as_deref(), Some("also update the README"));
        assert!(response.content_blocks.is_empty());
    }

    #[tokio::test]
    async fn exit_plan_mode_escape_dispatches_no() {
        let selected = Arc::new(Mutex::new(Vec::new()));
        let selected_clone = selected.clone();
        let mut app = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                ExitPlanModePermissionRequest(
                    request: Some(exit_request("Plan body")),
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
            &[ExitPlanModePermissionOptionValue::No]
        );
    }

    #[tokio::test]
    async fn plan_rejection_detail_carries_feedback_and_current_plan() {
        let details = Arc::new(Mutex::new(Vec::<ExitPlanModePermissionSelection>::new()));
        let details_for_handler = Arc::clone(&details);
        let mut app = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                ExitPlanModePermissionRequest(
                    request: Some(exit_request("Plan body")),
                    on_select_detail: move |detail| details_for_handler.lock().unwrap().push(detail),
                )
            }
        };
        let events = vec![
            key(KeyCode::Down),
            key(KeyCode::Down),
            key(KeyCode::Char('r')),
            key(KeyCode::Char('e')),
            key(KeyCode::Char('v')),
            key(KeyCode::Enter),
        ];
        let mut render_loop = Box::pin(app.mock_terminal_render_loop(
            MockTerminalConfig::with_events(stream::iter(events)).with_size(120, 30),
        ));
        for _ in 0..14 {
            let next = crate::utils::race(render_loop.next(), async {
                futures_timer::Delay::new(Duration::from_millis(100)).await;
                None
            })
            .await;
            if next.is_none() {
                break;
            }
        }
        let details = details.lock().unwrap();
        assert_eq!(details.len(), 1);
        assert_eq!(details[0].value, ExitPlanModePermissionOptionValue::No);
        assert_eq!(details[0].plan, "Plan body");
        assert_eq!(details[0].feedback.as_deref(), Some("rev"));
        assert!(details[0].content_blocks.is_empty());
    }

    #[tokio::test]
    async fn plan_rejection_image_paste_reaches_permission_response() {
        let details = Arc::new(Mutex::new(Vec::<ExitPlanModePermissionSelection>::new()));
        let details_for_handler = Arc::clone(&details);
        let details_for_loop = Arc::clone(&details);
        let child = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                ExitPlanModePermissionRequest(
                    request: Some(exit_request("Plan body")),
                    clipboard_image_override: Some(crate::utils::image_paste::ClipboardImage {
                        base64: "BBBB".to_string(),
                        media_type: "image/jpeg".to_string(),
                        dimensions: None,
                    }),
                    on_select_detail: move |detail| details_for_handler.lock().unwrap().push(detail),
                )
            }
        }
        .into_any();
        let mut app = crate::keybindings::keybinding_provider_setup::test_keybinding_root(child);
        let events = vec![
            key(KeyCode::Down),
            key(KeyCode::Down),
            modified_key(KeyCode::Char('v'), KeyModifiers::CONTROL),
            key(KeyCode::Enter),
        ];
        let paced = stream::unfold(events.into_iter(), |mut events| async move {
            let event = std::iter::Iterator::next(&mut events)?;
            futures_timer::Delay::new(Duration::from_millis(50)).await;
            Some((event, events))
        });
        let mut render_loop =
            Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(paced).with_size(120, 30),
            ));
        for _ in 0..24 {
            let next = crate::utils::race(render_loop.next(), async {
                futures_timer::Delay::new(Duration::from_millis(150)).await;
                None
            })
            .await;
            if next.is_none() || !details_for_loop.lock().unwrap().is_empty() {
                break;
            }
        }

        let detail = details.lock().unwrap().first().cloned().expect("selection");
        assert_eq!(detail.value, ExitPlanModePermissionOptionValue::No);
        assert!(matches!(
            detail.content_blocks.as_slice(),
            [PermissionContentBlock::Image { source }]
                if source.media_type == "image/jpeg" && source.data == "BBBB"
        ));
        let response = exit_plan_mode_selection_to_prompt_response(detail);
        assert_eq!(response.choice, PermissionPromptChoice::Deny);
        assert_eq!(response.feedback.as_deref(), Some("(See attached image)"));
        assert_eq!(response.content_blocks.len(), 1);
    }
}
