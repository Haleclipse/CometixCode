//! Agent model selection helpers.
//! Maps to CC `utils/model/agent.ts`.

use crate::types::permissions::PermissionMode;
use crate::utils::model::model::{get_runtime_main_loop_model, parse_user_specified_model};
use crate::utils::model::providers::{ApiProvider, get_api_provider};
use crate::utils::process_env::JsTruthy;

/// Maps to CC `utils/model/agent.ts#AGENT_MODEL_OPTIONS`.
pub const AGENT_MODEL_OPTIONS: &[&str] = &[
    "sonnet",
    "opus",
    "haiku",
    "best",
    "sonnet[1m]",
    "opus[1m]",
    "opusplan",
    "inherit",
];

/// Maps to CC `utils/model/agent.ts#getDefaultSubagentModel`.
pub fn get_default_subagent_model() -> &'static str {
    "inherit"
}

/// Maps to CC `utils/model/agent.ts#getAgentModel`.
pub fn get_agent_model(
    agent_model: Option<&str>,
    parent_model: &str,
    tool_specified_model: Option<&str>,
    permission_mode: Option<PermissionMode>,
) -> String {
    // Maps to CC `agent.ts:42-44`: `if (process.env.CLAUDE_CODE_SUBAGENT_MODEL)`
    // — plain JS truthiness on the env value. A whitespace-only value is
    // truthy in JS and reaches `parseUserSpecifiedModel`; trimming here would
    // have swallowed it.
    if let Some(env_model) = crate::utils::process_env::var("CLAUDE_CODE_SUBAGENT_MODEL").truthy() {
        return parse_user_specified_model(&env_model);
    }

    let parent_region_prefix =
        crate::utils::model::bedrock::get_bedrock_region_prefix(parent_model);
    let apply_parent_region_prefix = |resolved_model: String, original_spec: &str| -> String {
        if let Some(prefix) = parent_region_prefix {
            if get_api_provider() == ApiProvider::Bedrock
                && crate::utils::model::bedrock::get_bedrock_region_prefix(original_spec).is_none()
            {
                return crate::utils::model::bedrock::apply_bedrock_region_prefix(
                    &resolved_model,
                    prefix,
                );
            }
        }
        resolved_model
    };

    // Maps to CC `agent.ts:69` `if (toolSpecifiedModel)` — truthiness, so an
    // empty string falls through to the agent-model branch but a whitespace-only
    // one does not.
    if let Some(model) = tool_specified_model.filter(|value| !value.is_empty()) {
        if alias_matches_parent_tier(model, parent_model) {
            return parent_model.to_string();
        }
        return apply_parent_region_prefix(parse_user_specified_model(model), model);
    }

    // Maps to CC `agent.ts:77` `const agentModelWithExp = agentModel ??
    // getDefaultSubagentModel()`. The operator is NULLISH here while the two
    // guards above are truthiness, so an empty `agent_model` is NOT replaced by
    // the default: it stays `""`, fails the `inherit` and tier-alias checks, and
    // reaches `parse_user_specified_model("")`. Filtering empties here collapsed
    // that distinction and silently rerouted it to `inherit`.
    let agent_model_with_exp: &str = match agent_model {
        Some(value) => value,
        None => get_default_subagent_model(),
    };

    if agent_model_with_exp == "inherit" {
        return get_runtime_main_loop_model(
            permission_mode.unwrap_or_default(),
            parent_model.to_string(),
            false,
        );
    }

    if alias_matches_parent_tier(agent_model_with_exp, parent_model) {
        return parent_model.to_string();
    }

    apply_parent_region_prefix(
        parse_user_specified_model(agent_model_with_exp),
        agent_model_with_exp,
    )
}

/// Maps to CC `utils/model/agent.ts#aliasMatchesParentTier`.
pub fn alias_matches_parent_tier(alias: &str, parent_model: &str) -> bool {
    let canonical = parent_model.to_ascii_lowercase();
    match alias.to_ascii_lowercase().as_str() {
        "opus" => canonical.contains("opus"),
        "sonnet" => canonical.contains("sonnet"),
        "haiku" => canonical.contains("haiku"),
        _ => false,
    }
}

/// Maps to CC `utils/model/agent.ts#getAgentModelDisplay`.
pub fn get_agent_model_display(model: Option<&str>) -> String {
    match model {
        None => "Inherit from parent (default)".to_string(),
        Some("inherit") => "Inherit from parent".to_string(),
        Some(value) => capitalize_ascii(value),
    }
}

fn capitalize_ascii(value: &str) -> String {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    format!("{}{}", first.to_ascii_uppercase(), chars.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::TEST_ENV_LOCK;

    #[test]
    fn get_agent_model_matches_official_inherit_and_priority_order() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("CLAUDE_CODE_SUBAGENT_MODEL");
        crate::utils::process_env::remove("ANTHROPIC_MODEL");

        assert_eq!(
            get_agent_model(
                None,
                "parent-sonnet-model",
                None,
                Some(PermissionMode::Default)
            ),
            "parent-sonnet-model"
        );
        assert_eq!(
            get_agent_model(
                Some("inherit"),
                "parent-sonnet-model",
                None,
                Some(PermissionMode::Default)
            ),
            "parent-sonnet-model"
        );
        assert!(
            get_agent_model(
                Some("haiku"),
                "parent-sonnet-model",
                Some("opus"),
                Some(PermissionMode::Default)
            )
            .contains("opus")
        );
    }

    #[test]
    fn get_agent_model_preserves_parent_exact_model_for_matching_bare_aliases() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("CLAUDE_CODE_SUBAGENT_MODEL");

        assert_eq!(
            get_agent_model(
                Some("opus"),
                "claude-opus-4-6-custom-provider-id",
                None,
                Some(PermissionMode::Default)
            ),
            "claude-opus-4-6-custom-provider-id"
        );
        assert_eq!(
            get_agent_model(
                None,
                "claude-sonnet-4-6-custom-provider-id",
                Some("sonnet"),
                Some(PermissionMode::Default)
            ),
            "claude-sonnet-4-6-custom-provider-id"
        );
    }

    /// CC's `getAgentModel` uses THREE different guards, and they are not
    /// interchangeable: truthiness on the env value (`agent.ts:42`), truthiness
    /// on `toolSpecifiedModel` (`:69`), and NULLISH coalescing on `agentModel`
    /// (`:77`). A single `.trim().is_empty()` filter at all three sites — what
    /// this port had — collapses them into one behaviour and reroutes every
    /// blank input to `inherit`. Each assertion below is a case where CC does
    /// NOT fall through to the parent model.
    #[test]
    fn get_agent_model_keeps_officials_three_distinct_guards() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("CLAUDE_CODE_SUBAGENT_MODEL");
        crate::utils::process_env::remove("ANTHROPIC_MODEL");
        crate::utils::process_env::remove("CLAUDE_CODE_USE_BEDROCK");

        // `agent.ts:77` is `??`: an empty agent model is NOT replaced by
        // `getDefaultSubagentModel()`, so it never reaches the `inherit` branch.
        assert_eq!(
            get_agent_model(Some(""), "parent-sonnet-model", None, None),
            ""
        );

        // `agent.ts:69` is truthiness: a whitespace-only tool model is truthy in
        // JS, so it takes the tool branch instead of falling through to the
        // agent model.
        assert_eq!(
            get_agent_model(
                Some("inherit"),
                "parent-sonnet-model",
                Some("   "),
                Some(PermissionMode::Default)
            ),
            ""
        );

        // `agent.ts:42` is truthiness on the raw env value, which is truthy for
        // whitespace; the env branch wins over both models below it.
        crate::utils::process_env::set("CLAUDE_CODE_SUBAGENT_MODEL", "   ");
        assert_eq!(
            get_agent_model(
                Some("inherit"),
                "parent-sonnet-model",
                None,
                Some(PermissionMode::Default)
            ),
            ""
        );
        crate::utils::process_env::remove("CLAUDE_CODE_SUBAGENT_MODEL");
    }

    #[test]
    fn get_agent_model_honors_official_subagent_env_override() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("CLAUDE_CODE_SUBAGENT_MODEL", "haiku");
        assert!(
            get_agent_model(
                Some("opus"),
                "parent-sonnet-model",
                Some("sonnet"),
                Some(PermissionMode::Default)
            )
            .contains("haiku")
        );
        crate::utils::process_env::remove("CLAUDE_CODE_SUBAGENT_MODEL");
    }

    #[test]
    fn get_agent_model_inherits_bedrock_region_for_aliases() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("CLAUDE_CODE_SUBAGENT_MODEL");
        crate::utils::process_env::set("CLAUDE_CODE_USE_BEDROCK", "1");
        crate::utils::process_env::set(
            "ANTHROPIC_DEFAULT_HAIKU_MODEL",
            "anthropic.claude-haiku-4-5-20251001-v1:0",
        );

        assert_eq!(
            get_agent_model(
                Some("haiku"),
                "eu.anthropic.claude-sonnet-4-5-20250929-v1:0",
                None,
                Some(PermissionMode::Default)
            ),
            "eu.anthropic.claude-haiku-4-5-20251001-v1:0"
        );

        crate::utils::process_env::remove("ANTHROPIC_DEFAULT_HAIKU_MODEL");
        crate::utils::process_env::remove("CLAUDE_CODE_USE_BEDROCK");
    }

    #[test]
    fn get_agent_model_display_matches_official_copy() {
        assert_eq!(
            get_agent_model_display(None),
            "Inherit from parent (default)"
        );
        assert_eq!(
            get_agent_model_display(Some("inherit")),
            "Inherit from parent"
        );
        assert_eq!(get_agent_model_display(Some("opus")), "Opus");
    }
}
