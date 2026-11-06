//! Implementation of `/effort`.
//!
//! Per design §2:
//! - `/effort` only changes the current session.
//! - Supported levels are the four unified levels: `off < medium < high < max`.
//! - Level changes are recorded as session events and restored on resume.
//! - Global effortLevel setting, CLAUDE_CODE_EFFORT_LEVEL env, launch pinning,
//!   and ultracode are removed.

use crate::constants::xml::COMMON_HELP_ARGS;
use crate::state::store::AppStore;
use crate::tool::ToolUseContext;
use crate::utils::effort::{Effort, get_effort_description};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffortCommandResult {
    pub message: String,
    pub effort_update: Option<Option<Effort>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EffortCall {
    OpenPicker,
    Result(EffortCommandResult),
}

impl EffortCall {
    pub fn into_result(self) -> Option<EffortCommandResult> {
        match self {
            Self::Result(result) => Some(result),
            Self::OpenPicker => None,
        }
    }
}

impl EffortCommandResult {
    pub fn message(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            effort_update: None,
        }
    }

    pub fn with_update(message: impl Into<String>, value: Option<Effort>) -> Self {
        Self {
            message: message.into(),
            effort_update: Some(value),
        }
    }
}

pub fn usage_text() -> String {
    let mut text = "Usage: /effort [off|medium|high|max]\n\nEffort levels:\n".to_string();
    for level in Effort::ALL {
        text.push_str(&format!(
            "- {}: {}\n",
            level.as_str(),
            get_effort_description(level)
        ));
    }
    text
}

pub fn handle_cancel_output() -> String {
    "Cancelled".to_string()
}

pub fn show_current_effort(current: Option<&Effort>) -> EffortCommandResult {
    let level = current.copied().unwrap_or_default();
    EffortCommandResult::message(format!(
        "Current session effort: {} ({})",
        level.as_str(),
        get_effort_description(level)
    ))
}

pub fn execute_effort(args: &str) -> EffortCommandResult {
    let trimmed = args.trim();
    if trimmed.is_empty() {
        return EffortCommandResult::message(usage_text());
    }
    let Some(effort) = Effort::from_str_loose(trimmed) else {
        return EffortCommandResult::message(format!(
            "Invalid argument: {trimmed}. Valid options are: off, medium, high, max\n\n{}",
            usage_text()
        ));
    };

    let _ = crate::utils::session_storage::save_session_effort(effort);

    EffortCommandResult::with_update(
        format!(
            "Effort set to '{}' for this session: {}",
            effort.as_str(),
            get_effort_description(effort)
        ),
        Some(effort),
    )
}

pub fn execute_effort_for_model(args: &str, _model: &str) -> EffortCommandResult {
    execute_effort(args)
}

fn current_effort_and_model(context: &ToolUseContext) -> (Option<Effort>, String) {
    if let Some(state) = context.get_app_state() {
        let model = crate::hooks::use_main_loop_model::use_main_loop_model(
            state.main_loop_model.as_deref(),
            state.main_loop_model_for_session.as_deref(),
        );
        return (state.effort_value, model);
    }
    let model = context
        .main_loop_model
        .clone()
        .unwrap_or_else(|| crate::hooks::use_main_loop_model::use_main_loop_model(None, None));
    (context.effort_value, model)
}

fn apply_effort_and_close(
    result: EffortCommandResult,
    context: &ToolUseContext,
) -> EffortCommandResult {
    if let Some(update) = result.effort_update {
        context.set_app_state(|state| {
            state.effort_value = update;
        });
    }
    result
}

pub fn apply_effort_command(args: &str, store: &AppStore) -> String {
    let result = execute_effort(args);
    if let Some(update) = result.effort_update {
        store.replace_with(|state| {
            state.effort_value = update;
        });
    }
    result.message
}

pub fn normalize_effort_picker_arg(args: &str, _model: &str) -> Option<Option<Effort>> {
    let trimmed = args.trim();
    if matches!(trimmed.to_ascii_lowercase().as_str(), "auto" | "unset") {
        Some(None)
    } else {
        Effort::from_str_loose(trimmed).map(|e| Some(e))
    }
}

pub fn call(args: &str, context: &ToolUseContext) -> EffortCall {
    let args = args.trim();
    if COMMON_HELP_ARGS.contains(&args) {
        return EffortCall::Result(EffortCommandResult::message(usage_text()));
    }
    if matches!(args, "current" | "status") {
        let (effort, _) = current_effort_and_model(context);
        return EffortCall::Result(show_current_effort(effort.as_ref()));
    }
    if args.is_empty() {
        return EffortCall::OpenPicker;
    }
    let result = execute_effort(args);
    EffortCall::Result(apply_effort_and_close(result, context))
}
