//! Implementation of `/effort`.
//!
//! Per design §2:
//! - `/effort` only changes the current session.
//! - Supported levels are the four unified levels: `off < medium < high < max`.
//! - Level changes are recorded as session events and restored on resume.
//! - Global effortLevel setting, CLAUDE_CODE_EFFORT_LEVEL env, launch pinning,
//!   and ultracode are removed.
//!
//! @nyan: `/effort` with no argument prints the current effort and the valid
//! values; CC opens an effort picker. The model picker's ←/→ is the
//! interactive control.

use crate::constants::xml::COMMON_HELP_ARGS;
use crate::tool::ToolUseContext;
use crate::utils::effort::{Effort, get_effort_description};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffortCommandResult {
    pub message: String,
    pub effort_update: Option<Effort>,
}

impl EffortCommandResult {
    pub fn message(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            effort_update: None,
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

/// The current effort line: the session's, else the profile's configured
/// effort, else the model default.
pub fn show_current_effort(current: Option<Effort>) -> EffortCommandResult {
    let current = match current {
        Some(level) => format!(
            "Current effort: {} ({})",
            level.as_str(),
            get_effort_description(level)
        ),
        None => "Current effort: model default".to_string(),
    };
    EffortCommandResult::message(format!("{current}\n\n{}", usage_text()))
}

pub fn execute_effort(args: &str) -> EffortCommandResult {
    let trimmed = args.trim();
    let Some(effort) = Effort::from_str_loose(trimmed) else {
        return EffortCommandResult::message(format!(
            "Invalid argument: {trimmed}. Valid options are: off, medium, high, max\n\n{}",
            usage_text()
        ));
    };

    let _ = crate::utils::session_storage::save_session_effort(effort);

    EffortCommandResult {
        message: format!(
            "Effort set to '{}' for this session: {}",
            effort.as_str(),
            get_effort_description(effort)
        ),
        effort_update: Some(effort),
    }
}

fn current_effort(context: &ToolUseContext) -> Option<Effort> {
    let (session, profile) = match context.get_app_state() {
        Some(state) => (state.effort_value, state.main_loop_profile.clone()),
        None => (context.effort_value, context.main_loop_profile.clone()),
    };
    session.or_else(|| {
        crate::utils::model::profile::profile_default_effort(
            &crate::utils::settings::get_initial_settings(),
            profile.as_deref(),
        )
    })
}

pub fn call(args: &str, context: &ToolUseContext) -> EffortCommandResult {
    let args = args.trim();
    if COMMON_HELP_ARGS.contains(&args) {
        return EffortCommandResult::message(usage_text());
    }
    if args.is_empty() || matches!(args, "current" | "status") {
        return show_current_effort(current_effort(context));
    }
    let result = execute_effort(args);
    if let Some(update) = result.effort_update {
        context.set_app_state(|state| {
            state.effort_value = Some(update);
        });
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_argument_prints_the_current_effort_and_the_valid_values() {
        let context = ToolUseContext::default().with_effort_value(Some(Effort::High));
        let message = call("", &context).message;
        assert!(message.starts_with("Current effort: high"), "{message}");
        for level in ["off", "medium", "high", "max"] {
            assert!(message.contains(&format!("- {level}: ")), "{message}");
        }
        assert_eq!(call("", &context).effort_update, None);
    }

    #[test]
    fn a_level_sets_the_session_effort_and_anything_else_is_refused() {
        let context = ToolUseContext::default();
        assert_eq!(call("max", &context).effort_update, Some(Effort::Max));
        let invalid = call("low", &context);
        assert_eq!(invalid.effort_update, None);
        assert!(invalid.message.starts_with("Invalid argument: low."));
    }
}
