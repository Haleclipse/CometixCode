//! Maps to: CC `components/EffortIndicator.ts`.

use crate::constants::figures::{EFFORT_HIGH, EFFORT_LOW, EFFORT_MAX, EFFORT_MEDIUM};
use crate::utils::effort::{Effort, get_displayed_effort_level};

/// Maps to: CC `components/EffortIndicator.ts#effortLevelToSymbol`.
pub fn effort_level_to_symbol(level: &str) -> &'static str {
    match level {
        "off" => EFFORT_LOW,
        "medium" => EFFORT_MEDIUM,
        "high" => EFFORT_HIGH,
        "max" => EFFORT_MAX,
        _ => EFFORT_HIGH,
    }
}

/// Maps to: CC `components/EffortIndicator.ts#getEffortNotificationText`.
pub fn get_effort_notification_text(effort_value: Option<&Effort>, model: &str) -> String {
    let level = get_displayed_effort_level(model, effort_value);
    format!("{} {level} · /effort", effort_level_to_symbol(level))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effort_level_to_symbol_matches_the_four_unified_levels() {
        assert_eq!(effort_level_to_symbol("off"), "○");
        assert_eq!(effort_level_to_symbol("medium"), "◐");
        assert_eq!(effort_level_to_symbol("high"), "●");
        assert_eq!(effort_level_to_symbol("max"), "◈");
        assert_eq!(effort_level_to_symbol("unexpected"), EFFORT_HIGH);
    }

    #[test]
    fn effort_notification_text_uses_the_session_value_without_global_overrides() {
        assert_eq!(
            get_effort_notification_text(Some(&Effort::Medium), "any-model"),
            format!("{EFFORT_MEDIUM} medium · /effort")
        );
        assert_eq!(
            get_effort_notification_text(None, "any-model"),
            format!("{EFFORT_MEDIUM} medium · /effort")
        );
        assert_eq!(
            get_effort_notification_text(Some(&Effort::Off), "any-model"),
            format!("{EFFORT_LOW} off · /effort")
        );
    }
}
