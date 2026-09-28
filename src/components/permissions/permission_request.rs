//! Maps to: CC `components/permissions/PermissionRequest.tsx`.

use super::ask_user_question_permission_request::AskUserQuestionPermissionRequest;
use super::bash_permission_request::{
    BashPermissionRequest, bash_command_from_request, bash_tool_use_selection_to_prompt_response,
};
use super::enter_plan_mode_permission_request::{
    EnterPlanModePermissionRequest, enter_plan_mode_permission_option_to_prompt_choice,
};
use super::exit_plan_mode_permission_request::{
    ExitPlanModePermissionRequest, exit_plan_mode_selection_to_prompt_response,
};
use super::fallback_permission_request::FallbackPermissionRequest;
use super::file_edit_permission_request::FileEditPermissionRequest;
use super::file_write_permission_request::FileWritePermissionRequest;
use super::filesystem_permission_request::FilesystemPermissionRequest;
use super::notebook_edit_permission_request::NotebookEditPermissionRequest;
use super::powershell_permission_request::{
    PowerShellPermissionRequest, powershell_tool_use_selection_to_prompt_response,
};
use super::sed_edit_permission_request::SedEditPermissionRequest;
use super::skill_permission_request::SkillPermissionRequest;
use super::web_fetch_permission_request::{
    WebFetchPermissionRequest, web_fetch_permission_option_to_prompt_response,
};
use super::worker_badge::WorkerBadgeProps;
use crate::tool::ToolPermissionContext;
use crate::tools::bash_tool::sed_edit_parser::parse_sed_edit_command;
use crate::types::message::Message;
use crate::types::permissions::{
    PermissionBehavior, PermissionPromptChoice, PermissionPromptResponse,
    PermissionRequest as PermissionRequestData, PermissionUpdate, PermissionUpdateDestination,
};
use iocraft::prelude::*;
use std::sync::Arc;

#[derive(Default, Props)]
pub struct PermissionRequestProps {
    pub request: Option<PermissionRequestData>,
    pub on_select: Handler<PermissionPromptChoice>,
    /// Maps to CC `ToolUseConfirm.onAllow(updatedInput, ...)` for interactive
    /// permission UIs that update the tool input before execution.
    ///
    /// The answer carries the `toolUseID` of the row this dialog was handed
    /// (stamped in [`PermissionRequest`], not by the leaf dialogs). CC needs no
    /// such field: its dialogs call `toolUseConfirm.onAllow(...)` on the ROW
    /// object they were given (`PermissionRequest.tsx:196-231` forwards
    /// `toolUseConfirm` to the concrete component; ast-grep
    /// `toolUseConfirm.onAllow($$$)` over `components/permissions/` — every
    /// answer site is a method on the row), so a queue that changed since the
    /// render cannot mis-address it. This port's answer is a value travelling
    /// back to the REPL, so it names its row explicitly.
    pub on_select_response: Handler<PermissionPromptResponse>,
    /// Maps to CC `PermissionRequestProps.toolUseContext.toolPermissionContext`.
    /// File permission dialogs use it to render/generate the official
    /// session-scope updates for `Yes, during this session`.
    pub tool_permission_context: Option<ToolPermissionContext>,
    /// Maps to CC `ToolUseConfirm.workerBadge` forwarded to concrete
    /// permission request UIs.
    pub worker_badge: Option<WorkerBadgeProps>,
    /// Maps to: CC `PermissionRequestProps.verbose` (`PermissionRequest.tsx:121`).
    /// Its producer is the REPL: `useAppState(s => s.verbose)` (`REPL.tsx:973`)
    /// passed at `:6045`, and `PermissionRequest` forwards it unchanged to
    /// whichever concrete dialog it selected (`:227`).
    ///
    /// Only three CC dialogs read it — `FilesystemPermissionRequest.tsx:65-68`
    /// (`renderToolUseMessage(input, { theme, verbose })`),
    /// `NotebookEditPermissionRequest.tsx:67-68` (`verbose` + the 120/80 diff
    /// width) and `WebFetchPermissionRequest.tsx:127`. Bash/PowerShell/Fallback
    /// hardcode `verbose: true` at their own render sites
    /// (`BashPermissionRequest.tsx:522`, `PowerShellPermissionRequest.tsx:250`,
    /// `FallbackPermissionRequest.tsx:170`) and ignore the prop.
    pub verbose: bool,
    /// Maps to: CC `toolUseContext.messages` consumed by permission explainer.
    pub messages: Arc<Vec<Message>>,
    /// Maps to CC `PermissionRequestProps.onReject` reaching
    /// `toolUseConfirm.onReject()` — the `app:interrupt` keybinding at
    /// `PermissionRequest.tsx:206-214` (CC's Ctrl-C; Cometix binds Ctrl-C to
    /// `app:exit`, so here it is a key a user binds to it). CC's Esc leg reaches the same row method
    /// through `PermissionPrompt.tsx:219-231` → `FallbackPermissionRequest.tsx:108-121`;
    /// in this port Esc is a `Deny` on
    /// [`on_select_response`](PermissionRequestProps::on_select_response)
    /// instead (`fallback_permission_request.rs:272-275`), so both legs now
    /// name their row.
    ///
    /// Carries the `toolUseID` of the row this dialog was handed, for the same
    /// reason `on_select_response` does.
    pub on_cancel: Handler<String>,
}

fn emit_prompt_choice(
    on_select: &Handler<PermissionPromptChoice>,
    on_select_response: &Handler<PermissionPromptResponse>,
    choice: PermissionPromptChoice,
) {
    (on_select)(choice);
    (on_select_response)(PermissionPromptResponse::new(choice));
}

fn emit_prompt_response(
    on_select: &Handler<PermissionPromptChoice>,
    on_select_response: &Handler<PermissionPromptResponse>,
    response: PermissionPromptResponse,
) {
    (on_select)(response.choice);
    (on_select_response)(response);
}

fn tool_permission_context_for_request(
    request: &PermissionRequestData,
    context: &Option<ToolPermissionContext>,
) -> ToolPermissionContext {
    context.clone().unwrap_or_else(|| ToolPermissionContext {
        mode: request.mode,
        ..ToolPermissionContext::default()
    })
}

#[component]
pub fn PermissionRequest(
    props: &PermissionRequestProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    // Maps to: CC `isPermissionExplainerEnabled()` (permissionExplainer.ts:139-141),
    // a config-only gate CC reads leaf-local in `usePermissionExplainerUI`
    // (PermissionExplanation.tsx:101). Re-derived from the live global config on
    // every render so `/config` flips apply mid-session (no AppState snapshot);
    // moving the read fully into `use_permission_explanation` (dropping the
    // prop threading) is a deferred stricter-shape follow-up.
    let permission_explainer_enabled =
        crate::utils::permissions::permission_explainer::is_permission_explainer_enabled(
            &crate::utils::config::load_global_config(),
        );
    let request = props.request.clone().unwrap_or(PermissionRequestData {
        permission_result: None,
        id: String::new(),
        tool_use_id: String::new(),
        tool_name: "Tool".to_string(),
        mcp_info: None,
        decision_reason: None,
        description: String::new(),
        message: String::new(),
        input_summary: String::new(),
        input: serde_json::Value::Null,
        call_input: None,
        rule: crate::types::permissions::PermissionRuleValue::new("Tool", None),
        suggestions: Vec::new(),
        blocked_path: None,
        metadata: None,
        is_compound_command: false,
        mode: crate::types::permissions::PermissionMode::Default,
    });

    // Maps to: CC `PermissionRequest.tsx:196-231` — the dispatcher hands the
    // concrete dialog the ROW (`toolUseConfirm`), and every answer site is a
    // method on that row (`toolUseConfirm.onAllow(...)`, `.onReject()`), so an
    // answer can only ever reach the entry the user was actually shown. This
    // port's leaf dialogs emit a `PermissionPromptResponse` value instead, and
    // the REPL used to apply it to `queue[0]` — which is a DIFFERENT row once a
    // teammate abort or a recheck sweep withdrew the head between the render
    // and the keypress. Stamping the rendered row's `toolUseID` here is that
    // structural addressing, made explicit: this is the one place that knows
    // which row is on screen.
    //
    // An empty id is the props-default request (no row at all), which no
    // production caller renders; it stays `None` so the REPL keeps its
    // head-of-queue fallback rather than looking up `""`.
    let rendered_tool_use_id =
        (!request.tool_use_id.is_empty()).then(|| request.tool_use_id.clone());
    let props_on_select_response = props.on_select_response.clone();
    let stamp_tool_use_id = rendered_tool_use_id.clone();
    let on_select_response =
        Handler::<PermissionPromptResponse>::from(move |mut response: PermissionPromptResponse| {
            response.tool_use_id = stamp_tool_use_id.clone();
            (props_on_select_response)(response);
        });
    // CC `PermissionRequest.tsx:206-214` `toolUseConfirm.onReject()` — the
    // cancel leg addresses the same row, so it carries the same id. The leaf
    // dialogs keep CC's `onReject()` arity (no argument).
    let props_on_cancel = props.on_cancel.clone();
    let cancel_tool_use_id = rendered_tool_use_id.unwrap_or_default();
    let on_cancel = Handler::<()>::from(move |_: ()| {
        (props_on_cancel)(cancel_tool_use_id.clone());
    });
    // CC :206-214: app:interrupt (CC's Ctrl+C; not Cometix's, which binds
    // Ctrl+C to app:exit) rejects whichever dialog shows. It runs after the
    // dialog's own listeners, as a parent's do; the REPL's cancel handler
    // leaves it the key while the dialog's list registers an overlay.
    let runtime = hooks
        .try_use_context::<crate::keybindings::keybinding_context::KeybindingRuntime>()
        .map(|runtime| runtime.clone());
    crate::keybindings::use_keybinding::use_keybinding(
        &mut hooks,
        runtime,
        "app:interrupt",
        crate::keybindings::types::ContextName::Confirmation,
        || true,
        {
            let on_cancel = on_cancel.clone();
            move || {
                on_cancel(());
                true
            }
        },
    );

    if request.tool_name.eq_ignore_ascii_case("bash") {
        let on_select = props.on_select.clone();
        let on_select_response = on_select_response.clone();
        let on_cancel = on_cancel.clone();
        let command = bash_command_from_request(&request);
        // Maps to CC `BashPermissionRequest` top-level sed delegation. Rust
        // performs the branch at the permission dispatch seam so the official
        // `_simulatedSedEdit` updated-input payload can be emitted through
        // `PermissionPromptResponse` before the generic Bash option adapter.
        if let Some(sed_info) = parse_sed_edit_command(&command) {
            return element! {
                SedEditPermissionRequest(
                    request: Some(request.clone()),
                    sed_info: Some(sed_info),
                    tool_permission_context: Some(tool_permission_context_for_request(&request, &props.tool_permission_context)),
                    worker_badge: props.worker_badge.clone(),
                    on_select: move |response| {
                        emit_prompt_response(&on_select, &on_select_response, response);
                    },
                    on_cancel: move |_| {
                        (on_cancel)(());
                    },
                )
            }
            .into_any();
        }

        let sandbox_settings = crate::utils::settings::get_initial_settings();
        let sandboxing_enabled =
            crate::utils::sandbox::sandbox_adapter::get_sandbox_enabled_setting(&sandbox_settings);
        let is_sandboxed = sandboxing_enabled
            && crate::tools::bash_tool::should_use_sandbox::should_use_sandbox(
                &crate::tools::bash_tool::should_use_sandbox::SandboxInput {
                    command: Some(command.clone()),
                    dangerously_disable_sandbox: request
                        .input
                        .get("dangerouslyDisableSandbox")
                        .or_else(|| request.input.get("dangerously_disable_sandbox"))
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                },
            );
        let destructive_warning = crate::utils::feature_flags::feature_enabled(
            crate::utils::feature_flags::FeatureFlag::DestructiveCommandWarning,
        )
        .then(|| {
            crate::tools::bash_tool::destructive_command_warning::get_destructive_command_warning(
                &command,
            )
            .map(str::to_string)
        })
        .flatten();
        let suggestions = request.suggestions.clone();
        // Maps to BashPermissionRequest's lazy editable-prefix initializer.
        // Compound commands retain the backend's complete suggestion set;
        // only a single Bash rule can seed the editable field.
        let editable_prefix = if request.is_compound_command {
            let bash_rules = suggestions
                .iter()
                .filter_map(|update| match update {
                    PermissionUpdate::AddRules { rules, .. }
                    | PermissionUpdate::ReplaceRules { rules, .. } => Some(rules),
                    _ => None,
                })
                .flatten()
                .filter(|rule| {
                    rule.tool_name == crate::tools::bash_tool::tool_name::BASH_TOOL_NAME
                        && rule
                            .rule_content
                            .as_ref()
                            .is_some_and(|value| !value.is_empty())
                })
                .collect::<Vec<_>>();
            (bash_rules.len() == 1)
                .then(|| bash_rules[0].rule_content.clone())
                .flatten()
        } else {
            crate::tools::bash_tool::bash_permissions::get_simple_command_prefix(&command)
                .or_else(|| {
                    crate::tools::bash_tool::bash_permissions::get_first_word_prefix(&command)
                })
                .map(|prefix| format!("{prefix}:*"))
                .or_else(|| Some(command.clone()))
        };
        let response_request = request.clone();
        let response_suggestions = suggestions.clone();

        return element! {
            BashPermissionRequest(
                request: Some(request),
                suggestions: suggestions,
                worker_badge: props.worker_badge.clone(),
                show_always_allow_options: true,
                sandboxing_enabled: sandboxing_enabled,
                is_sandboxed: is_sandboxed,
                destructive_warning: destructive_warning,
                editable_prefix: editable_prefix,
                editable_prefix_input_enabled: true,
                explainer_enabled: permission_explainer_enabled,
                messages: Arc::clone(&props.messages),
                on_select: move |selection| {
                    emit_prompt_response(
                        &on_select,
                        &on_select_response,
                        bash_tool_use_selection_to_prompt_response(
                            &selection,
                            &response_request,
                            &response_suggestions,
                        ),
                    );
                },
            )
        }
        .into_any();
    }
    if request.tool_name.eq_ignore_ascii_case("powershell") {
        let on_select = props.on_select.clone();
        let on_select_response = on_select_response.clone();
        let suggestions = request
            .rule
            .rule_content
            .as_ref()
            .filter(|content| !content.trim().is_empty())
            .map(|_| PermissionUpdate::AddRules {
                destination: PermissionUpdateDestination::LocalSettings,
                behavior: PermissionBehavior::Allow,
                rules: vec![request.rule.clone()],
            })
            .into_iter()
            .collect::<Vec<_>>();
        let editable_prefix = request.rule.rule_content.clone();
        // Maps to: CC `PowerShellPermissionRequest.tsx:65-70` — the same
        // `tengu_destructive_command_warning` gate the Bash branch uses, but
        // reading the PowerShell pattern table
        // (`tools/PowerShellTool/destructiveCommandWarning.ts`), NOT Bash's.
        // The prop existed on the component and was never supplied, so this
        // dialog's warning row could not render at all.
        let destructive_warning = crate::utils::feature_flags::feature_enabled(
            crate::utils::feature_flags::FeatureFlag::DestructiveCommandWarning,
        )
        .then(|| {
            crate::tools::powershell_tool::destructive_command_warning::get_destructive_command_warning(
                request
                    .input
                    .get("command")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default(),
            )
            .map(str::to_string)
        })
        .flatten();
        let response_request = request.clone();
        let response_suggestions = suggestions.clone();

        return element! {
            PowerShellPermissionRequest(
                request: Some(request),
                suggestions: suggestions,
                worker_badge: props.worker_badge.clone(),
                show_always_allow_options: true,
                destructive_warning: destructive_warning,
                editable_prefix: editable_prefix,
                editable_prefix_input_enabled: true,
                explainer_enabled: permission_explainer_enabled,
                messages: Arc::clone(&props.messages),
                on_select: move |selection| {
                    emit_prompt_response(
                        &on_select,
                        &on_select_response,
                        powershell_tool_use_selection_to_prompt_response(
                            &selection,
                            &response_request,
                            &response_suggestions,
                        ),
                    );
                },
            )
        }
        .into_any();
    }
    if request.tool_name == "AskUserQuestion" {
        let on_select = props.on_select.clone();
        let on_select_response = on_select_response.clone();

        return element! {
            AskUserQuestionPermissionRequest(
                request: Some(request),
                worker_badge: props.worker_badge.clone(),
                on_select: move |response| {
                    emit_prompt_response(&on_select, &on_select_response, response);
                },
            )
        }
        .into_any();
    }
    if request.tool_name.eq_ignore_ascii_case("webfetch") {
        let on_select = props.on_select.clone();
        let on_select_response = on_select_response.clone();

        let response_request = request.clone();

        return element! {
            WebFetchPermissionRequest(
                request: Some(request),
                show_always_allow_options: true,
                worker_badge: props.worker_badge.clone(),
                // CC `PermissionRequest.tsx:227` forwards the prop unchanged.
                verbose: props.verbose,
                on_select: move |value| {
                    emit_prompt_response(
                        &on_select,
                        &on_select_response,
                        web_fetch_permission_option_to_prompt_response(value, &response_request),
                    );
                },
            )
        }
        .into_any();
    }
    if request.tool_name == "EnterPlanMode" {
        let on_select = props.on_select.clone();
        let on_select_response = on_select_response.clone();

        return element! {
            EnterPlanModePermissionRequest(
                request: Some(request),
                worker_badge: props.worker_badge.clone(),
                on_select: move |value| {
                    emit_prompt_choice(&on_select, &on_select_response, enter_plan_mode_permission_option_to_prompt_choice(value));
                },
            )
        }
        .into_any();
    }
    if request.tool_name == "ExitPlanMode" {
        let on_select = props.on_select.clone();
        let on_select_response = on_select_response.clone();

        return element! {
            ExitPlanModePermissionRequest(
                request: Some(request),
                worker_badge: props.worker_badge.clone(),
                // Maps to: CC `hooks/toolPermission/PermissionContext.ts:269-316` single callback resolution.
                on_select_detail: move |selection| {
                    emit_prompt_response(
                        &on_select,
                        &on_select_response,
                        exit_plan_mode_selection_to_prompt_response(selection),
                    );
                },
            )
        }
        .into_any();
    }
    if matches!(request.tool_name.as_str(), "Read" | "Glob" | "Grep") {
        let on_select = props.on_select.clone();
        let on_select_response = on_select_response.clone();
        let on_cancel = on_cancel.clone();

        return element! {
            FilesystemPermissionRequest(
                request: Some(request.clone()),
                tool_permission_context: Some(tool_permission_context_for_request(&request, &props.tool_permission_context)),
                worker_badge: props.worker_badge.clone(),
                // CC `PermissionRequest.tsx:227` forwards the prop unchanged.
                verbose: props.verbose,
                on_select: on_select,
                on_select_response: on_select_response,
                on_cancel: move |_| {
                    (on_cancel)(());
                },
            )
        }
        .into_any();
    }
    if matches!(
        request.tool_name.as_str(),
        "Edit" | "FileEdit" | "MultiEdit"
    ) {
        let on_select = props.on_select.clone();
        let on_select_response = on_select_response.clone();
        let on_cancel = on_cancel.clone();

        return element! {
            FileEditPermissionRequest(
                request: Some(request.clone()),
                tool_permission_context: Some(tool_permission_context_for_request(&request, &props.tool_permission_context)),
                worker_badge: props.worker_badge.clone(),
                on_select: on_select,
                on_select_response: on_select_response,
                on_cancel: move |_| {
                    (on_cancel)(());
                },
            )
        }
        .into_any();
    }
    if matches!(request.tool_name.as_str(), "Write" | "FileWrite") {
        let on_select = props.on_select.clone();
        let on_select_response = on_select_response.clone();
        let on_cancel = on_cancel.clone();

        return element! {
            FileWritePermissionRequest(
                request: Some(request.clone()),
                tool_permission_context: Some(tool_permission_context_for_request(&request, &props.tool_permission_context)),
                worker_badge: props.worker_badge.clone(),
                on_select: on_select,
                on_select_response: on_select_response,
                on_cancel: move |_| {
                    (on_cancel)(());
                },
            )
        }
        .into_any();
    }
    if request.tool_name == "NotebookEdit" {
        let on_select = props.on_select.clone();
        let on_select_response = on_select_response.clone();
        let on_cancel = on_cancel.clone();

        return element! {
            NotebookEditPermissionRequest(
                request: Some(request.clone()),
                tool_permission_context: Some(tool_permission_context_for_request(&request, &props.tool_permission_context)),
                // Re-derived 2026-08-26 (#134): CC
                // `NotebookEditPermissionRequest.tsx:67-68` passes
                // `verbose={props.verbose}` (and picks the 120/80 diff width from
                // it), not a literal `true`. The port hardcoded `true` only
                // because no producer existed above this component.
                verbose: props.verbose,
                worker_badge: props.worker_badge.clone(),
                on_select: on_select,
                on_select_response: on_select_response,
                on_cancel: move |_| {
                    (on_cancel)(());
                },
            )
        }
        .into_any();
    }
    if request.tool_name == "Skill" {
        let on_select = props.on_select.clone();
        let on_select_response = on_select_response.clone();

        return element! {
            SkillPermissionRequest(
                request: Some(request),
                worker_badge: props.worker_badge.clone(),
                on_select: Handler::from(move |response| {
                    emit_prompt_response(&on_select, &on_select_response, response);
                }),
            )
        }
        .into_any();
    }

    let on_select = props.on_select.clone();

    element! {
        FallbackPermissionRequest(
            request: Some(request),
            worker_badge: props.worker_badge.clone(),
            on_select: Handler::from(move |response| {
                emit_prompt_response(&on_select, &on_select_response, response);
            }),
        )
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::messages_list::Messages;
    use crate::components::prompt_input::PromptInput;
    use crate::types::message::{RenderableMessage, RenderableMessageKind, ToolUseStatus};
    use crate::types::permissions::{PermissionMode, PermissionRuleValue};
    use crate::utils::theme;
    use futures::StreamExt;
    use std::sync::Mutex;

    fn bash_request() -> PermissionRequestData {
        PermissionRequestData {
            permission_result: None,
            id: "req".to_string(),
            tool_use_id: "toolu".to_string(),
            tool_name: "Bash".to_string(),
            mcp_info: None,
            decision_reason: None,
            description: String::new(),
            message: String::new(),
            input_summary: "echo permission-gated".to_string(),
            input: serde_json::json!({ "command": "echo permission-gated" }),
            call_input: None,
            rule: PermissionRuleValue::new("Bash", Some("echo permission-gated:*".to_string())),
            suggestions: vec![PermissionUpdate::AddRules {
                destination: PermissionUpdateDestination::LocalSettings,
                behavior: PermissionBehavior::Allow,
                rules: vec![PermissionRuleValue::new(
                    "Bash",
                    Some("echo permission-gated:*".to_string()),
                )],
            }],
            blocked_path: None,
            metadata: None,
            is_compound_command: false,
            mode: PermissionMode::Default,
        }
    }

    fn powershell_request() -> PermissionRequestData {
        PermissionRequestData {
            permission_result: None,
            id: "req".to_string(),
            tool_use_id: "toolu".to_string(),
            tool_name: "PowerShell".to_string(),
            mcp_info: None,
            decision_reason: None,
            description: String::new(),
            message: String::new(),
            input_summary: "Get-ChildItem".to_string(),
            input: serde_json::json!({ "command": "Get-ChildItem" }),
            call_input: None,
            rule: PermissionRuleValue::new("PowerShell", Some("Get-ChildItem".to_string())),
            suggestions: Vec::new(),
            blocked_path: None,
            metadata: None,
            is_compound_command: false,
            mode: PermissionMode::Default,
        }
    }

    fn web_fetch_request() -> PermissionRequestData {
        PermissionRequestData {
            permission_result: None,
            id: "req".to_string(),
            tool_use_id: "toolu".to_string(),
            tool_name: "WebFetch".to_string(),
            mcp_info: None,
            decision_reason: None,
            description: "Fetch https://example.com/docs".to_string(),
            message: String::new(),
            input_summary: "https://example.com/docs".to_string(),
            input: serde_json::json!({
                "url": "https://example.com/docs",
                "prompt": "summarize"
            }),
            call_input: None,
            rule: PermissionRuleValue::new("WebFetch", Some("domain:example.com".to_string())),
            suggestions: Vec::new(),
            blocked_path: None,
            metadata: None,
            is_compound_command: false,
            mode: PermissionMode::Default,
        }
    }

    fn ask_user_question_request() -> PermissionRequestData {
        PermissionRequestData {
            permission_result: None,
            id: "req".to_string(),
            tool_use_id: "toolu_ask".to_string(),
            tool_name: "AskUserQuestion".to_string(),
            mcp_info: None,
            decision_reason: None,
            description: String::new(),
            message: String::new(),
            input_summary: String::new(),
            input: serde_json::json!({
                "questions": [{
                    "question": "Proceed?",
                    "header": "Proceed",
                    "options": [
                        {"label": "Yes", "description": "Continue"},
                        {"label": "No", "description": "Stop"}
                    ]
                }]
            }),
            call_input: None,
            rule: PermissionRuleValue::new("AskUserQuestion", None),
            suggestions: Vec::new(),
            blocked_path: None,
            metadata: None,
            is_compound_command: false,
            mode: PermissionMode::Default,
        }
    }

    fn read_request() -> PermissionRequestData {
        PermissionRequestData {
            permission_result: None,
            id: "req".to_string(),
            tool_use_id: "toolu".to_string(),
            tool_name: "Read".to_string(),
            mcp_info: None,
            decision_reason: None,
            description: String::new(),
            message: String::new(),
            input_summary: "/repo/src/main.rs".to_string(),
            input: serde_json::json!({ "file_path": "/repo/src/main.rs" }),
            call_input: None,
            rule: PermissionRuleValue::new("Read", Some("/repo/src/main.rs".to_string())),
            suggestions: Vec::new(),
            blocked_path: None,
            metadata: None,
            is_compound_command: false,
            mode: PermissionMode::Default,
        }
    }

    fn edit_request() -> PermissionRequestData {
        PermissionRequestData {
            permission_result: None,
            id: "req".to_string(),
            tool_use_id: "toolu".to_string(),
            tool_name: "Edit".to_string(),
            mcp_info: None,
            decision_reason: None,
            description: String::new(),
            message: String::new(),
            input_summary: "/repo/src/lib.rs".to_string(),
            input: serde_json::json!({
                "file_path": "/repo/src/lib.rs",
                "old_string": "old()",
                "new_string": "new()"
            }),
            call_input: None,
            rule: PermissionRuleValue::new("Edit", Some("/repo/src/lib.rs".to_string())),
            suggestions: Vec::new(),
            blocked_path: None,
            metadata: None,
            is_compound_command: false,
            mode: PermissionMode::Default,
        }
    }

    fn write_request() -> PermissionRequestData {
        PermissionRequestData {
            permission_result: None,
            id: "req".to_string(),
            tool_use_id: "toolu".to_string(),
            tool_name: "Write".to_string(),
            mcp_info: None,
            decision_reason: None,
            description: String::new(),
            message: String::new(),
            input_summary: "/repo/src/new.rs".to_string(),
            input: serde_json::json!({
                "file_path": "/repo/src/new.rs",
                "content": "fn main() {}"
            }),
            call_input: None,
            rule: PermissionRuleValue::new("Write", Some("/repo/src/new.rs".to_string())),
            suggestions: Vec::new(),
            blocked_path: None,
            metadata: None,
            is_compound_command: false,
            mode: PermissionMode::Default,
        }
    }

    fn notebook_request() -> PermissionRequestData {
        PermissionRequestData {
            permission_result: None,
            id: "req".to_string(),
            tool_use_id: "toolu".to_string(),
            tool_name: "NotebookEdit".to_string(),
            mcp_info: None,
            decision_reason: None,
            description: String::new(),
            message: String::new(),
            input_summary: "/repo/notebooks/demo.ipynb".to_string(),
            input: serde_json::json!({
                "notebook_path": "/repo/notebooks/demo.ipynb",
                "cell_id": "abc123",
                "new_source": "print('hello')",
                "cell_type": "code",
                "edit_mode": "replace"
            }),
            call_input: None,
            rule: PermissionRuleValue::new(
                "NotebookEdit",
                Some("/repo/notebooks/demo.ipynb".to_string()),
            ),
            suggestions: Vec::new(),
            blocked_path: None,
            metadata: None,
            is_compound_command: false,
            mode: PermissionMode::Default,
        }
    }

    fn enter_plan_request() -> PermissionRequestData {
        PermissionRequestData {
            permission_result: None,
            id: "req".to_string(),
            tool_use_id: "toolu".to_string(),
            tool_name: "EnterPlanMode".to_string(),
            mcp_info: None,
            decision_reason: None,
            description: String::new(),
            message: String::new(),
            input_summary: String::new(),
            input: serde_json::json!({}),
            call_input: None,
            rule: PermissionRuleValue::new("EnterPlanMode", None),
            suggestions: Vec::new(),
            blocked_path: None,
            metadata: None,
            is_compound_command: false,
            mode: PermissionMode::Default,
        }
    }

    fn exit_plan_request() -> PermissionRequestData {
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
            input: serde_json::json!({ "plan": "## Plan\n- make the change" }),
            call_input: None,
            rule: PermissionRuleValue::new("ExitPlanMode", Some("plan".to_string())),
            suggestions: Vec::new(),
            blocked_path: None,
            metadata: None,
            is_compound_command: false,
            mode: PermissionMode::Plan,
        }
    }

    fn custom_tool_request() -> PermissionRequestData {
        PermissionRequestData {
            permission_result: None,
            id: "req".to_string(),
            tool_use_id: "toolu".to_string(),
            tool_name: "CustomTool".to_string(),
            mcp_info: None,
            decision_reason: None,
            description: "custom description".to_string(),
            message: String::new(),
            input_summary: "arg".to_string(),
            // `renderToolUseMessage` for a tool with no bespoke renderer reads
            // the first of description/command/file_path/path — the raw input is
            // never stringified into the dialog.
            input: serde_json::json!({ "description": "arg" }),
            call_input: None,
            rule: PermissionRuleValue::new("CustomTool", Some("arg".to_string())),
            suggestions: Vec::new(),
            blocked_path: None,
            metadata: None,
            is_compound_command: false,
            mode: PermissionMode::Default,
        }
    }

    fn key(code: KeyCode) -> TerminalEvent {
        TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code))
    }

    fn modified_key(code: KeyCode, modifiers: KeyModifiers) -> TerminalEvent {
        let mut event = KeyEvent::new(KeyEventKind::Press, code);
        event.modifiers = modifiers;
        TerminalEvent::Key(event)
    }

    /// Drive a dialog element with `events` until it settles, so the answer
    /// callbacks have fired.
    async fn drive_dialog(app: AnyElement<'static>, events: Vec<TerminalEvent>) {
        drive_dialog_with(
            app,
            events,
            crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings(),
        )
        .await;
    }

    /// The default bindings plus F6 for `app:interrupt`: Cometix binds Ctrl+C
    /// to `app:exit`, so the dispatcher's interrupt is reached through a key a
    /// user binds to it.
    fn runtime_with_interrupt_key() -> crate::keybindings::keybinding_context::KeybindingRuntime {
        let mut bindings = crate::keybindings::default_bindings::default_bindings();
        bindings.push(crate::keybindings::types::ParsedBinding {
            chord: crate::keybindings::parser::parse_chord("f6"),
            action: Some("app:interrupt".to_string()),
            context: crate::keybindings::types::ContextName::Global,
        });
        crate::keybindings::keybinding_context::KeybindingRuntime::new(bindings)
    }

    async fn drive_dialog_with(
        app: AnyElement<'static>,
        events: Vec<TerminalEvent>,
        runtime: crate::keybindings::keybinding_context::KeybindingRuntime,
    ) {
        // The dialogs answer through keybindings (their Select's, and the
        // dispatcher's app:interrupt), so they run under a runtime.
        let mut app = element! {
            ContextProvider(value: Context::owned(runtime)) {
                #(vec![app])
            }
        }
        .into_any();
        // `ignore_ctrl_c`, as production does (CC `exitOnCtrlC: false`): a
        // Ctrl+C no dialog takes must leave the loop running, not end it.
        let mut render_loop = Box::pin(
            app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(futures::stream::iter(events))
                    .with_size(120, 30)
                    .with_ignore_ctrl_c(true),
            ),
        );
        for _ in 0..10 {
            let next = crate::utils::race(render_loop.next(), async {
                futures_timer::Delay::new(std::time::Duration::from_millis(100)).await;
                None
            })
            .await;
            if next.is_none() {
                break;
            }
        }
    }

    /// Maps to: CC `components/permissions/PermissionRequest.tsx:196-231` — the
    /// dispatcher hands the concrete dialog the ROW, and the dialog answers by
    /// calling a method ON that row (`toolUseConfirm.onAllow(...)` /
    /// `.onReject()`; ast-grep `toolUseConfirm.onAllow($$$)` over
    /// `components/permissions/` shows every answer site is such a method).
    /// The answer therefore cannot reach any entry but the one rendered.
    ///
    /// This port's answer is a value the REPL applies to its queue, so it names
    /// the rendered row instead. OLD SHAPE: nothing named it, and
    /// `screens/repl.rs#on_permission_response` applied it to `queue[0]` — the
    /// assertion below (`Some("toolu")`) failed with `None`, and a queue whose
    /// head had been withdrawn between render and keypress took the answer on
    /// the successor's behalf.
    #[tokio::test]
    async fn a_dialog_answer_names_the_row_the_dialog_rendered() {
        let answers = Arc::new(Mutex::new(Vec::<PermissionPromptResponse>::new()));
        let recorder = answers.clone();
        drive_dialog(
            element! {
                ContextProvider(value: Context::owned(*theme::current())) {
                    PermissionRequest(
                        request: Some(custom_tool_request()),
                        on_select_response: move |response| recorder.lock().unwrap().push(response),
                        on_cancel: move |_| {},
                    )
                }
            }
            .into_any(),
            vec![key(KeyCode::Enter)],
        )
        .await;

        let answers = answers.lock().unwrap();
        let answer = answers.first().expect("Enter answers the dialog");
        assert_eq!(answer.choice, PermissionPromptChoice::AllowOnce);
        assert_eq!(answer.tool_use_id.as_deref(), Some("toolu"));
    }

    /// Maps to: CC `PermissionRequest.tsx:206-214` — the `app:interrupt`
    /// keybinding rejects through `toolUseConfirm.onReject()`, i.e. the
    /// rendered row's own callback. The cancel leg needs the same address as the
    /// allow leg, or an interrupt that arrives after its row was withdrawn drops
    /// the successor instead. (Cometix binds Ctrl+C to `app:exit`; F6 stands in
    /// for a key a user binds to `app:interrupt`.)
    ///
    /// OLD SHAPE: `on_cancel` was `Handler<()>` and carried nothing, so
    /// `screens/repl.rs#on_permission_cancel` denied `queue[0]`.
    #[tokio::test]
    async fn a_dialog_cancel_names_the_row_the_dialog_rendered() {
        let cancelled = Arc::new(Mutex::new(Vec::<String>::new()));
        let recorder = cancelled.clone();
        drive_dialog_with(
            element! {
                ContextProvider(value: Context::owned(*theme::current())) {
                    PermissionRequest(
                        request: Some(custom_tool_request()),
                        on_cancel: move |tool_use_id| recorder.lock().unwrap().push(tool_use_id),
                    )
                }
            }
            .into_any(),
            vec![key(KeyCode::F(6))],
            runtime_with_interrupt_key(),
        )
        .await;

        assert_eq!(cancelled.lock().unwrap().as_slice(), &["toolu".to_string()]);
    }

    fn skill_request() -> PermissionRequestData {
        PermissionRequestData {
            tool_name: "Skill".to_string(),
            input: serde_json::json!({ "skill": "deploy" }),
            rule: PermissionRuleValue::new("Skill", Some("deploy".to_string())),
            ..custom_tool_request()
        }
    }

    /// Maps to: CC `PermissionRequest.tsx:206-214` over
    /// `PermissionPrompt.tsx:219-231`: `app:interrupt` is the dispatcher's
    /// (the row's `onReject`), Esc the prompt's own cancel (a rejection
    /// answer); the prompt keeps no interrupt of its own, so each key reaches
    /// exactly one of them. Ctrl+C reaches neither: Cometix binds it to
    /// `app:exit` (2.0.x semantics), which this dialog does not take.
    #[tokio::test]
    async fn a_prompt_dialog_leaves_the_interrupt_to_the_dispatcher_and_answers_esc() {
        for (event, cancels, answers) in [
            (key(KeyCode::F(6)), vec!["toolu".to_string()], vec![]),
            (key(KeyCode::Esc), vec![], vec![PermissionPromptChoice::Deny]),
            (modified_key(KeyCode::Char('c'), KeyModifiers::CONTROL), vec![], vec![]),
        ] {
            let cancelled = Arc::new(Mutex::new(Vec::<String>::new()));
            let answered = Arc::new(Mutex::new(Vec::<PermissionPromptChoice>::new()));
            let (cancel_recorder, answer_recorder) = (cancelled.clone(), answered.clone());
            drive_dialog_with(
                element! {
                    ContextProvider(value: Context::owned(*theme::current())) {
                        PermissionRequest(
                            request: Some(skill_request()),
                            on_select_response: move |response: PermissionPromptResponse| {
                                answer_recorder.lock().unwrap().push(response.choice)
                            },
                            on_cancel: move |tool_use_id| cancel_recorder.lock().unwrap().push(tool_use_id),
                        )
                    }
                }
                .into_any(),
                vec![event.clone()],
                runtime_with_interrupt_key(),
            )
            .await;
            assert_eq!(*cancelled.lock().unwrap(), cancels, "{event:?}");
            assert_eq!(*answered.lock().unwrap(), answers, "{event:?}");
        }
    }

    /// The dialogs that kept a Ctrl+C reject of their own, which CC's never
    /// had: under Cometix's binding (Ctrl+C → `app:exit`, which no permission
    /// dialog takes) Ctrl+C now answers nothing in them, while a key bound to
    /// `app:interrupt` still reaches the dispatcher (CC :206-214).
    #[tokio::test]
    async fn ctrl_c_answers_nothing_in_the_tool_dialogs() {
        for request in [
            bash_request(),
            powershell_request(),
            web_fetch_request(),
            enter_plan_request(),
            exit_plan_request(),
        ] {
            let tool = request.tool_name.clone();
            for (event, cancels) in [
                (modified_key(KeyCode::Char('c'), KeyModifiers::CONTROL), 0),
                (key(KeyCode::F(6)), 1),
            ] {
                let cancelled = Arc::new(Mutex::new(0usize));
                let answered = Arc::new(Mutex::new(Vec::<PermissionPromptChoice>::new()));
                let (cancel_recorder, answer_recorder) = (cancelled.clone(), answered.clone());
                drive_dialog_with(
                    element! {
                        ContextProvider(value: Context::owned(*theme::current())) {
                            PermissionRequest(
                                request: Some(request.clone()),
                                on_select_response: move |response: PermissionPromptResponse| {
                                    answer_recorder.lock().unwrap().push(response.choice)
                                },
                                on_cancel: move |_| *cancel_recorder.lock().unwrap() += 1,
                            )
                        }
                    }
                    .into_any(),
                    vec![event.clone()],
                    runtime_with_interrupt_key(),
                )
                .await;
                assert_eq!(*cancelled.lock().unwrap(), cancels, "{tool} {event:?}");
                assert!(answered.lock().unwrap().is_empty(), "{tool} {event:?}");
            }
        }
    }

    fn transcript_fixture_messages(count: usize) -> Vec<RenderableMessage> {
        let mut messages = vec![
            RenderableMessage::user("fixture-user", "please inspect permissions"),
            RenderableMessage::assistant_block(
                "fixture-assistant",
                crate::types::message::AssistantContent::Text(
                    "I will check this safely.".to_string(),
                ),
            ),
            RenderableMessage::assistant_block(
                "fixture-tool",
                crate::types::message::AssistantContent::ToolUse(
                    crate::types::message::ToolUseBlock {
                        id: crate::types::ids::ToolUseId("toolu".to_string()),
                        name: "Bash".to_string(),
                        input: serde_json::json!({"command": "echo permission-gated"}),
                    },
                ),
            ),
        ];
        while messages.len() < count {
            let idx = messages.len();
            messages.push(RenderableMessage::assistant_block(
                format!("fixture-text-{idx}"),
                crate::types::message::AssistantContent::Text(format!(
                    "fixture transcript row {idx}"
                )),
            ));
        }
        messages
    }

    #[test]
    fn permission_request_uses_natural_height() {
        let current_theme = *theme::current();
        let canvas = element! {
            ContextProvider(value: Context::owned(current_theme)) {
                PermissionRequest(
                    request: Some(bash_request()),
                    on_select: move |_| {},
                    on_cancel: move |_| {},
                )
            }
        }
        .render(Some(125));
        assert!(
            canvas.height() < 40,
            "permission request should stay natural-height"
        );
    }

    #[test]
    fn permission_explainer_gate_follows_global_config_mid_session() {
        use crate::utils::config::{GlobalConfig, replace_test_global_config};

        let render = || {
            element! {
                ContextProvider(value: Context::owned(*theme::current())) {
                    PermissionRequest(
                        request: Some(bash_request()),
                        on_select: move |_| {},
                        on_cancel: move |_| {},
                    )
                }
            }
            .render(Some(125))
            .to_string()
        };

        // Startup-equivalent config: explainer enabled by default.
        let previous = replace_test_global_config(Some(GlobalConfig::default()));
        let enabled_canvas = render();

        // Mid-session `/config` flip: the gate must re-read the live config
        // (the old AppState snapshot stayed frozen until restart).
        replace_test_global_config(Some(GlobalConfig {
            permission_explainer_enabled: Some(false),
            ..GlobalConfig::default()
        }));
        let disabled_canvas = render();
        replace_test_global_config(previous);

        assert!(
            enabled_canvas.contains("ctrl+e to explain"),
            "default config keeps the explainer gate open; canvas=\n{enabled_canvas}"
        );
        assert!(
            !disabled_canvas.contains("ctrl+e"),
            "config opt-out must gate the explainer without a restart; canvas=\n{disabled_canvas}"
        );
    }

    #[test]
    fn permission_request_forwards_worker_badge_to_concrete_tool_ui() {
        let current_theme = *theme::current();
        let text = element! {
            ContextProvider(value: Context::owned(current_theme)) {
                PermissionRequest(
                    request: Some(bash_request()),
                    worker_badge: Some(WorkerBadgeProps { name: "reviewer".to_string(), color: None }),
                    on_select: move |_| {},
                    on_cancel: move |_| {},
                )
            }
        }
        .render(Some(125))
        .to_string();

        assert!(text.contains("@reviewer"), "canvas=\n{text}");
    }

    #[test]
    fn permission_request_stretches_to_wide_main_screen_parent() {
        let current_theme = *theme::current();
        let canvas = element! {
            ContextProvider(value: Context::owned(current_theme)) {
                View(flex_direction: FlexDirection::Column, width: 240u32) {
                    PermissionRequest(
                        request: Some(bash_request()),
                        on_select: move |_| {},
                        on_cancel: move |_| {},
                    )
                }
            }
        }
        .render(Some(240));

        assert_eq!(canvas.width(), 240);
    }

    #[test]
    fn transcript_plus_permission_request_uses_terminal_width_root() {
        let current_theme = *theme::current();
        let messages = transcript_fixture_messages(3);

        let messages = Arc::new(messages);
        let canvas = element! {
            ContextProvider(value: Context::owned(current_theme)) {
                // `Messages` reads app state for the display preferences —
                // strict since P7.
                crate::state::app_state::AppStateProvider(
                    children: crate::state::app_state::ProviderChildren::new(move || element! {
                        View(flex_direction: FlexDirection::Column, width: 100pct) {
                            Messages(messages: Arc::clone(&messages), is_loading: false)
                            PermissionRequest(
                                request: Some(bash_request()),
                                on_select: move |_| {},
                                on_cancel: move |_| {},
                            )
                        }
                    }.into_any()),
                )
            }
        }
        .render(Some(125));
        assert!(
            canvas.height() < 120,
            "combined overlay should stay natural-height"
        );
    }

    #[test]
    fn transcript_plus_prompt_input_uses_terminal_width_root() {
        let current_theme = *theme::current();
        let messages = transcript_fixture_messages(12);

        let canvas = element! {
            ContextProvider(value: Context::owned(current_theme)) {
                // PromptInput reads AppState and raises notifications, both
                // strict since P6 G2/G3a. This is a layout assertion, so
                // default state is the fixture.
                crate::state::app_state::AppStateProvider(
                    children: crate::state::app_state::ProviderChildren::new({
                        let messages = Arc::new(messages);
                        move || element! {
                            View(flex_direction: FlexDirection::Column, width: 100pct) {
                                Messages(messages: Arc::clone(&messages), is_loading: false)
                                PromptInput(
                                    on_submit: move |_| {},
                                    on_exit: move |_| {},
                                )
                            }
                        }.into_any()
                    }),
                )
            }
        }
        .render(Some(125));
        assert!(
            canvas.height() < 220,
            "prompt input should not add blank pages below transcript"
        );
    }

    #[test]
    fn transcript_loading_plus_prompt_input_uses_terminal_width_root() {
        let current_theme = *theme::current();
        let messages = transcript_fixture_messages(36);

        let canvas = element! {
            ContextProvider(value: Context::owned(current_theme)) {
                crate::state::app_state::AppStateProvider(
                    children: crate::state::app_state::ProviderChildren::new({
                        let messages = Arc::new(messages);
                        move || element! {
                            View(flex_direction: FlexDirection::Column, width: 100pct) {
                                Messages(messages: Arc::clone(&messages), is_loading: true)
                                PromptInput(
                                    on_submit: move |_| {},
                                    on_exit: move |_| {},
                                )
                            }
                        }.into_any()
                    }),
                )
            }
        }
        .render(Some(125));
        assert!(
            canvas.height() < 160,
            "loading prompt should not add blank pages below transcript"
        );
    }

    #[component]
    fn PromptLayoutProbe(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let current_theme = *theme::current();
        let messages = transcript_fixture_messages(12);
        let (cols, rows) = hooks.use_terminal_size();
        if cols == 125 && rows == 41 {
            system.exit();
        }
        let layout_width = cols.max(1) as u32;

        element! {
            ContextProvider(value: Context::owned(current_theme)) {
                crate::state::app_state::AppStateProvider(
                    children: crate::state::app_state::ProviderChildren::new({
                        let messages = Arc::new(messages);
                        move || element! {
                            View(flex_direction: FlexDirection::Column, width: layout_width) {
                                Messages(messages: Arc::clone(&messages), is_loading: false)
                                PromptInput(
                                    on_submit: move |_| {},
                                    on_exit: move |_| {},
                                )
                            }
                        }.into_any()
                    }),
                )
            }
        }
    }

    #[test]
    fn transcript_plus_prompt_input_mock_terminal_size_no_blank_pages() {
        let canvases = futures::executor::block_on(
            element!(PromptLayoutProbe)
                .mock_terminal_render_loop(MockTerminalConfig::default().with_size(125, 41))
                .collect::<Vec<_>>(),
        );
        let last = canvases
            .last()
            .expect("mock render should produce a canvas");
        assert!(
            last.height() < 220,
            "mock terminal prompt should not add blank pages below transcript"
        );
    }

    #[test]
    fn permission_request_delegates_bash_to_official_bash_permission_dialog() {
        let current_theme = *theme::current();
        let text = element! {
            ContextProvider(value: Context::owned(current_theme)) {
                PermissionRequest(
                    request: Some(bash_request()),
                    on_select: move |_| {},
                    on_cancel: move |_| {},
                )
            }
        }
        .render(Some(125))
        .to_string();

        assert!(text.contains("Bash command"), "canvas=\n{text}");
        assert!(text.contains("echo permission-gated"), "canvas=\n{text}");
        assert!(
            text.contains("Yes, and don’t ask again for"),
            "Bash requests should use official BashPermissionRequest option copy; canvas=\n{text}"
        );
        assert!(
            text.contains("Yes, and don’t ask again for: echo permission-gated:*"),
            "Bash editable-prefix input should render its current value; canvas=\n{text}"
        );
        assert!(
            !text.contains("Yes, and don't ask again for echo permission-gated commands in"),
            "generic PermissionPrompt shell copy should not replace BashPermissionRequest; canvas=\n{text}"
        );
    }

    #[test]
    fn permission_request_projects_explicit_unsandboxed_bash_metadata() {
        struct ConfigGuard {
            config_dir: Option<crate::utils::env_utils::EnvVarGuard>,
            previous_original_cwd: std::path::PathBuf,
            root: std::path::PathBuf,
        }
        impl Drop for ConfigGuard {
            fn drop(&mut self) {
                drop(self.config_dir.take());
                crate::bootstrap::state::set_original_cwd(&self.previous_original_cwd);
                crate::utils::settings::settings_cache::reset_settings_cache();
                let _ = std::fs::remove_dir_all(&self.root);
            }
        }

        let _lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-bash-permission-sandbox-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("settings.json"),
            r#"{"sandbox":{"enabled":true,"allowUnsandboxedCommands":true}}"#,
        )
        .unwrap();
        let _config = ConfigGuard {
            config_dir: Some(crate::utils::env_utils::EnvVarGuard::set(
                "CLAUDE_CONFIG_DIR",
                &root,
            )),
            previous_original_cwd: crate::bootstrap::state::get_original_cwd(),
            root: root.clone(),
        };
        crate::bootstrap::state::set_original_cwd(&root);
        // The sandbox metadata reads merged settings, cached process-wide
        // (`settings_cache.rs:10-12`), so an earlier reader pins the snapshot
        // taken before the `settings.json` written above existed.
        crate::utils::settings::settings_cache::reset_settings_cache();
        let mut request = bash_request();
        request.input["dangerouslyDisableSandbox"] = serde_json::Value::Bool(true);

        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                PermissionRequest(
                    request: Some(request),
                    on_select: move |_| {},
                    on_cancel: move |_| {},
                )
            }
        }
        .render(Some(125))
        .to_string();
        assert!(
            text.contains("Bash command (unsandboxed)"),
            "canvas=\n{text}"
        );
    }

    #[tokio::test]
    async fn permission_request_delegates_bash_sed_to_official_sed_edit_dialog() {
        let path = std::env::temp_dir().join(format!(
            "cometix-permission-sed-{}.txt",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::write(&path, "hello old\n").unwrap();
        let command = format!("sed -i 's/old/new/' {}", path.display());
        let request = PermissionRequestData {
            permission_result: None,
            id: "req".to_string(),
            tool_use_id: "toolu".to_string(),
            tool_name: "Bash".to_string(),
            mcp_info: None,
            decision_reason: None,
            description: String::new(),
            message: String::new(),
            input_summary: command.clone(),
            input: serde_json::json!({ "command": command }),
            call_input: None,
            rule: PermissionRuleValue::new("Bash", None),
            suggestions: Vec::new(),
            blocked_path: None,
            metadata: None,
            is_compound_command: false,
            mode: PermissionMode::Default,
        };
        let current_theme = *theme::current();
        let mut app = element! {
            ContextProvider(value: Context::owned(current_theme)) {
                PermissionRequest(
                    request: Some(request),
                    on_select: move |_| {},
                    on_cancel: move |_| {},
                )
            }
        };
        // The Sed child matches CC Suspense: its initial frame is empty while
        // readFile is pending. Mount and await the actual completed preview.
        use futures::StreamExt;
        let mut frames = Box::pin(app.mock_terminal_render_loop(
            MockTerminalConfig::with_events(futures::stream::pending()).with_size(140, 40),
        ));
        let text = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while let Some(frame) = frames.next().await {
                let text = frame.to_string();
                if text.contains("hello new") {
                    return text;
                }
            }
            panic!("Sed permission delegation ended before the async preview completed");
        })
        .await
        .expect("delegated Sed preview must complete");
        let _ = std::fs::remove_file(&path);

        assert!(text.contains("Edit file"), "canvas=\n{text}");
        assert!(
            text.contains("Do you want to make this edit"),
            "canvas=\n{text}"
        );
        assert!(text.contains("hello old"), "canvas=\n{text}");
        assert!(text.contains("hello new"), "canvas=\n{text}");
        assert!(
            !text.contains("Bash command"),
            "sed branch should delegate away from generic Bash permission dialog; canvas=\n{text}"
        );
    }

    #[test]
    fn permission_request_delegates_powershell_to_official_powershell_permission_dialog() {
        let current_theme = *theme::current();
        let text = element! {
            ContextProvider(value: Context::owned(current_theme)) {
                PermissionRequest(
                    request: Some(powershell_request()),
                    on_select: move |_| {},
                    on_cancel: move |_| {},
                )
            }
        }
        .render(Some(125))
        .to_string();

        assert!(text.contains("PowerShell command"), "canvas=\n{text}");
        assert!(text.contains("Get-ChildItem"), "canvas=\n{text}");
        assert!(
            text.contains("Yes, and don’t ask again for"),
            "PowerShell requests should use official PowerShellPermissionRequest option copy; canvas=\n{text}"
        );
        assert!(
            text.contains("Yes, and don’t ask again for: Get-ChildItem"),
            "PowerShell editable-prefix input should render its current value; canvas=\n{text}"
        );
        assert!(
            !text.contains("Yes, and don't ask again for Get-ChildItem commands in"),
            "generic PermissionPrompt shell copy should not replace PowerShellPermissionRequest; canvas=\n{text}"
        );
    }

    #[test]
    fn permission_request_delegates_webfetch_to_official_webfetch_permission_dialog() {
        let current_theme = *theme::current();
        let render = |verbose: bool| {
            element! {
                ContextProvider(value: Context::owned(current_theme)) {
                    PermissionRequest(
                        request: Some(web_fetch_request()),
                        verbose: verbose,
                        on_select: move |_| {},
                        on_cancel: move |_| {},
                    )
                }
            }
            .render(Some(125))
            .to_string()
        };
        let text = render(false);

        assert!(text.contains("Fetch"), "canvas=\n{text}");
        // Re-derived 2026-08-26 (#134): `WebFetchPermissionRequest.tsx:120-129`
        // renders `WebFetchTool.renderToolUseMessage(input, { theme, verbose })`
        // with the forwarded PROP (`PermissionRequest.tsx:227`), and
        // `WebFetchTool/UI.tsx:14-20` returns the bare url unless verbose. The
        // old assertion encoded the port's hardcoded `verbose: true`.
        assert!(
            !text.contains("url: \"https://example.com/docs\""),
            "canvas=\n{text}"
        );
        let verbose_text = render(true);
        assert!(
            verbose_text.contains("url: \"https://example.com/docs\", prompt: \"summarize\""),
            "canvas=\n{verbose_text}"
        );
        assert!(
            text.contains("Do you want to allow Claude to fetch this content?"),
            "WebFetch requests should use official WebFetchPermissionRequest copy; canvas=\n{text}"
        );
        assert!(
            text.contains("Yes, and don't ask again for example.com"),
            "WebFetch domain option should be present; canvas=\n{text}"
        );
        assert!(
            !text.contains("Do you want to proceed?"),
            "generic PermissionPrompt should not replace WebFetchPermissionRequest; canvas=\n{text}"
        );
    }

    #[test]
    fn permission_request_delegates_ask_user_question_to_official_question_dialog() {
        let current_theme = *theme::current();
        let text = element! {
            ContextProvider(value: Context::owned(current_theme)) {
                PermissionRequest(
                    request: Some(ask_user_question_request()),
                    on_select: move |_| {},
                    on_cancel: move |_| {},
                )
            }
        }
        .render(Some(125))
        .to_string();

        assert!(text.contains("Proceed?"), "canvas=\n{text}");
        assert!(text.contains("Yes"), "canvas=\n{text}");
        // CC select-input-option.tsx:341-345: the unfocused Other shows its
        // placeholder.
        assert!(text.contains("Type something."), "canvas=\n{text}");
        assert!(text.contains("Chat about this"), "canvas=\n{text}");
        assert!(
            !text.contains("Tool use"),
            "AskUserQuestion should not use fallback permission prompt; canvas=\n{text}"
        );
    }

    #[test]
    fn permission_request_delegates_file_tools_to_official_file_permission_dialogs() {
        let current_theme = *theme::current();
        let cases = vec![
            (read_request(), "Read file", "Read(/repo/src/main.rs"),
            (
                edit_request(),
                "Edit file",
                "Do you want to make this edit to lib.rs?",
            ),
            (
                write_request(),
                "Create file",
                "Do you want to create new.rs?",
            ),
            (
                notebook_request(),
                "Edit notebook",
                // Notebook diff mirrors CC Suspense `fallback={null}`; the
                // first retained frame still owns the title/question/footer.
                "Do you want to make this edit to demo.ipynb?",
            ),
        ];

        for (request, title, needle) in cases {
            // Only the FILE cases need the provider — write/notebook delegate to
            // dialogs that render the diff components, which read
            // `settings.syntax_highlighting_disabled`. Mounting it for all four
            // keeps the loop uniform; defaults are the fixture since every
            // assertion here is on title/question/footer copy.
            let text = element! {
                ContextProvider(value: Context::owned(current_theme)) {
                    crate::state::app_state::AppStateProvider(
                        children: crate::state::app_state::ProviderChildren::new(move || element! {
                            PermissionRequest(
                                request: Some(request.clone()),
                                on_select: move |_| {},
                                on_cancel: move |_| {},
                            )
                        }.into_any()),
                    )
                }
            }
            .render(Some(125))
            .to_string();
            assert!(text.contains(title), "missing {title}; canvas=\n{text}");
            assert!(text.contains(needle), "missing {needle}; canvas=\n{text}");
            assert!(
                text.contains("Esc to cancel"),
                "file permission dialog footer should render; canvas=\n{text}"
            );
        }
    }

    #[test]
    fn permission_request_delegates_plan_mode_tools_to_official_plan_dialogs() {
        let current_theme = *theme::current();
        let enter = element! {
            ContextProvider(value: Context::owned(current_theme)) {
                PermissionRequest(
                    request: Some(enter_plan_request()),
                    on_select: move |_| {},
                    on_cancel: move |_| {},
                )
            }
        }
        .render(Some(125))
        .to_string();
        assert!(enter.contains("Enter plan mode?"), "canvas=\n{enter}");
        assert!(enter.contains("Yes, enter plan mode"), "canvas=\n{enter}");

        let exit = element! {
            ContextProvider(value: Context::owned(current_theme)) {
                PermissionRequest(
                    request: Some(exit_plan_request()),
                    on_select: move |_| {},
                    on_cancel: move |_| {},
                )
            }
        }
        .render(Some(125))
        .to_string();
        assert!(exit.contains("Ready to code?"), "canvas=\n{exit}");
        assert!(exit.contains("Yes, auto-accept edits"), "canvas=\n{exit}");
    }

    #[test]
    fn permission_request_uses_official_fallback_for_unknown_tools() {
        let current_theme = *theme::current();
        let text = element! {
            ContextProvider(value: Context::owned(current_theme)) {
                PermissionRequest(
                    request: Some(custom_tool_request()),
                    on_select: move |_| {},
                    on_cancel: move |_| {},
                )
            }
        }
        .render(Some(125))
        .to_string();

        assert!(text.contains("Tool use"), "canvas=\n{text}");
        assert!(text.contains("CustomTool(arg)"), "canvas=\n{text}");
        // Maps to: CC `PermissionRuleExplanation.tsx:91-97` — this request has
        // no `decisionReason`, so no rule sentence is rendered. It used to
        // quote `request.rule`, i.e. the current input, as if it were a
        // matched rule.
        assert!(!text.contains("Permission rule"), "canvas=\n{text}");
        assert!(
            text.contains("Yes, and don't ask again for CustomTool commands in"),
            "fallback always-allow copy should render; canvas=\n{text}"
        );
    }
}
