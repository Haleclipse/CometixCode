//! Privacy-level network gates.
//!
//! Maps to: CC `utils/privacyLevel.ts:1-52`.

use crate::utils::process_env::JsTruthy;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PrivacyLevel {
    #[default]
    Default,
    NoTelemetry,
    EssentialTraffic,
}

pub fn get_privacy_level_with(get_env: &impl Fn(&str) -> Option<String>) -> PrivacyLevel {
    if get_env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC").truthy().is_some() {
        PrivacyLevel::EssentialTraffic
    } else if get_env("DISABLE_TELEMETRY").truthy().is_some() {
        PrivacyLevel::NoTelemetry
    } else {
        PrivacyLevel::Default
    }
}

pub fn get_privacy_level() -> PrivacyLevel {
    get_privacy_level_with(&|key| crate::utils::process_env::var(key))
}

pub fn is_essential_traffic_only() -> bool {
    get_privacy_level() == PrivacyLevel::EssentialTraffic
}

pub fn is_telemetry_disabled() -> bool {
    get_privacy_level() != PrivacyLevel::Default
}

pub fn get_essential_traffic_only_reason() -> Option<&'static str> {
    crate::utils::process_env::var("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC")
        .truthy()
        .is_some()
        .then_some("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn privacy_level_uses_most_restrictive_official_environment_signal() {
        let env = |key: &str| match key {
            "DISABLE_TELEMETRY" => Some("1".to_string()),
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC" => Some("1".to_string()),
            _ => None,
        };
        assert_eq!(get_privacy_level_with(&env), PrivacyLevel::EssentialTraffic);
        let telemetry_only = |key: &str| (key == "DISABLE_TELEMETRY").then(|| "1".to_string());
        assert_eq!(
            get_privacy_level_with(&telemetry_only),
            PrivacyLevel::NoTelemetry
        );
    }
}
