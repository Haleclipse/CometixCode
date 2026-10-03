//! Maps to: CC `components/EffortIndicator.ts`.

use crate::constants::figures::{EFFORT_HIGH, EFFORT_LOW, EFFORT_MAX, EFFORT_MEDIUM, EFFORT_XHIGH};
use crate::utils::effort::{EffortValue, get_displayed_effort_level, model_supports_effort};

/// Maps to: CC `components/EffortIndicator.ts#effortLevelToSymbol`.
pub fn effort_level_to_symbol(level: &str) -> &'static str {
    match level {
        "low" => EFFORT_LOW,
        "medium" => EFFORT_MEDIUM,
        "high" => EFFORT_HIGH,
        "xhigh" => EFFORT_XHIGH,
        "max" => EFFORT_MAX,
        // Defensive parity: remote config can supply unknown values; CC falls
        // back to the high-effort symbol rather than rendering undefined.
        _ => EFFORT_HIGH,
    }
}

/// Maps to: CC `components/EffortIndicator.ts#getEffortNotificationText`.
pub fn get_effort_notification_text(
    effort_value: Option<&EffortValue>,
    model: &str,
) -> Option<String> {
    if !model_supports_effort(model) {
        return None;
    }
    let level = get_displayed_effort_level(model, effort_value);
    Some(format!(
        "{} {level} · /effort",
        effort_level_to_symbol(level)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::TEST_ENV_LOCK;

    #[test]
    fn effort_level_to_symbol_matches_official_symbols_and_fallback() {
        assert_eq!(effort_level_to_symbol("low"), "○");
        assert_eq!(effort_level_to_symbol("medium"), "◐");
        assert_eq!(effort_level_to_symbol("high"), "●");
        assert_eq!(effort_level_to_symbol("xhigh"), "◉");
        assert_eq!(effort_level_to_symbol("max"), "◈");
        assert_eq!(effort_level_to_symbol("unexpected"), EFFORT_HIGH);
    }

    #[test]
    fn effort_notification_text_respects_model_support_gate() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("CLAUDE_CODE_ALWAYS_ENABLE_EFFORT");
        crate::utils::process_env::remove("CLAUDE_CODE_EFFORT_LEVEL");
        crate::utils::process_env::remove("CLAUDE_CODE_USE_BEDROCK");
        crate::utils::process_env::remove("CLAUDE_CODE_USE_VERTEX");
        crate::utils::process_env::remove("CLAUDE_CODE_USE_FOUNDRY");

        assert_eq!(
            get_effort_notification_text(
                Some(&EffortValue::Named("medium".to_string())),
                "claude-sonnet-4-6-20260101",
            ),
            Some(format!("{EFFORT_MEDIUM} medium · /effort"))
        );
        assert_eq!(
            get_effort_notification_text(None, "claude-sonnet-4-6-20260101"),
            Some(format!("{EFFORT_HIGH} high · /effort"))
        );
        assert_eq!(
            get_effort_notification_text(
                Some(&EffortValue::Named("max".to_string())),
                "claude-opus-4-5",
            ),
            Some(format!("{EFFORT_HIGH} high · /effort")),
            "models without max support clamp through getDisplayedEffortLevel"
        );
        assert_eq!(
            get_effort_notification_text(
                Some(&EffortValue::Named("max".to_string())),
                "claude-opus-4-6-20260101",
            ),
            Some(format!("{EFFORT_MAX} max · /effort"))
        );
        assert_eq!(
            get_effort_notification_text(
                Some(&EffortValue::Named("medium".to_string())),
                "claude-sonnet-4-20250514",
            ),
            None
        );
    }

    #[test]
    fn effort_notification_text_honors_env_override_like_displayed_effort() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("CLAUDE_CODE_EFFORT_LEVEL", "low");
        crate::utils::process_env::remove("CLAUDE_CODE_USE_BEDROCK");
        crate::utils::process_env::remove("CLAUDE_CODE_USE_VERTEX");
        crate::utils::process_env::remove("CLAUDE_CODE_USE_FOUNDRY");

        assert_eq!(
            get_effort_notification_text(
                Some(&EffortValue::Named("high".to_string())),
                "claude-opus-4-6-20260101",
            ),
            Some(format!("{EFFORT_LOW} low · /effort"))
        );
        crate::utils::process_env::remove("CLAUDE_CODE_EFFORT_LEVEL");
    }
}
