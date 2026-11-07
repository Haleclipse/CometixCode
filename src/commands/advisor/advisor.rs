//! Maps to CC `commands/advisor.ts`.

use crate::commands::Command;
use crate::tool::ToolUseContext;
use crate::utils::process_user_input::ProcessUserInputBaseResult;
use crate::utils::process_user_input::process_slash_command::{
    SlashCommandAction, SlashCommandInvocation, local_text_command_input,
};

/// The command callback is asynchronous in CC because `validateModel` may make
/// a side query.  The synchronous slash dispatcher therefore carries this
/// request to the REPL's retained async callback, just like `/reload-plugins`.
pub fn dispatch(
    command: &Command,
    args: &str,
    _uuid: Option<String>,
    context: &ToolUseContext,
) -> ProcessUserInputBaseResult {
    let invocation = SlashCommandInvocation::new(command.name.as_ref(), args);
    let user_message = local_text_command_input(&invocation);
    ProcessUserInputBaseResult {
        messages: Vec::new(),
        should_query: false,
        allowed_tools: None,
        local_action: Some(SlashCommandAction::Advisor {
            args: args.to_string(),
            context: std::sync::Arc::new(context.clone()),
            invocation,
            user_message,
        }),
        query_source: crate::constants::query_source::QuerySource::Prompt,
    }
}

/// Maps to CC `commands/advisor.ts#call`.
pub async fn call(args: &str, context: &ToolUseContext) -> Result<String, String> {
    let arg = args.trim().to_ascii_lowercase();
    let base_model = crate::utils::model::model::profile_model(
        context
            .get_app_state()
            .and_then(|state| state.main_loop_profile.clone())
            .as_deref(),
    );

    if arg.is_empty() {
        let current = context
            .get_app_state()
            .and_then(|state| state.advisor_model.clone());
        let Some(current) = current else {
            return Ok(
                "Advisor: not set\nUse \"/advisor <model>\" to enable (e.g. \"/advisor claude-opus-4-6\")."
                    .to_string(),
            );
        };
        if !crate::utils::advisor::model_supports_advisor(&base_model) {
            return Ok(format!(
                "Advisor: {current} (inactive)\nThe current model ({base_model}) does not support advisors."
            ));
        }
        return Ok(format!(
            "Advisor: {current}\nUse \"/advisor unset\" to disable or \"/advisor <model>\" to change."
        ));
    }

    if matches!(arg.as_str(), "unset" | "off") {
        let previous = context
            .get_app_state()
            .and_then(|state| state.advisor_model.clone());
        context.set_app_state(|state| state.advisor_model = None);
        persist_advisor_setting(None)?;
        return Ok(previous
            .map(|previous| format!("Advisor disabled (was {previous})."))
            .unwrap_or_else(|| "Advisor already unset.".to_string()));
    }

    let normalized_model = crate::utils::model::model::normalize_model_string_for_api(&arg);
    // @nyan: the advisor is a server-side tool parameter; the id is sent as
    // given (no CC model aliases).
    let resolved_model = arg.clone();
    // CC returns validation failures as local command text.  They are not
    // rejected promises, so the REPL renders them in the stdout envelope.
    if let Err(error) = crate::utils::model::validate_model::validate_model(&resolved_model).await {
        return Ok(if error.is_empty() {
            format!("Unknown model: {arg} ({resolved_model})")
        } else {
            format!("Invalid advisor model: {error}")
        });
    }

    if !crate::utils::advisor::is_valid_advisor_model(&resolved_model) {
        return Ok(format!(
            "The model {arg} ({resolved_model}) cannot be used as an advisor"
        ));
    }

    context.set_app_state(|state| state.advisor_model = Some(normalized_model.clone()));
    persist_advisor_setting(Some(&normalized_model))?;

    if !crate::utils::advisor::model_supports_advisor(&base_model) {
        return Ok(format!(
            "Advisor set to {normalized_model}.\nNote: Your current model ({base_model}) does not support advisors. Switch to a supported model to use the advisor."
        ));
    }
    Ok(format!("Advisor set to {normalized_model}."))
}

fn persist_advisor_setting(value: Option<&str>) -> Result<(), String> {
    let mut updates = serde_json::Map::new();
    updates.insert(
        "advisorModel".to_string(),
        value
            .map(|value| serde_json::Value::String(value.to_string()))
            .unwrap_or(serde_json::Value::Null),
    );
    crate::utils::settings::update_settings_for_source(
        crate::utils::settings::SettingSource::User,
        &updates,
    )
    .map_err(|error| error.to_string())
}
