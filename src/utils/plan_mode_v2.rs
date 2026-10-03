//! Plan-mode V2 feature and concurrency policy.
//!
//! Maps to CC `utils/planModeV2.ts:1-60`.

use crate::utils::process_env::JsTruthy;

fn env_count(key: &str) -> Option<usize> {
    crate::utils::process_env::var(key)
        .truthy()
        .and_then(|value| {
            // CC parseInt(value, 10) accepts a decimal prefix and JS whitespace.
            let trimmed = value.trim_start_matches(|ch: char| {
                (ch.is_whitespace() && ch != '\u{0085}') || ch == '\u{feff}'
            });
            let trimmed = trimmed.strip_prefix('+').unwrap_or(trimmed);
            let digits = trimmed.bytes().take_while(u8::is_ascii_digit).count();
            trimmed[..digits].parse::<usize>().ok()
        })
        .filter(|value| (1..=10).contains(value))
}

/// Maps to CC `utils/planModeV2.ts:6-27` `getPlanModeV2AgentCount()`.
pub fn get_plan_mode_v2_agent_count() -> usize {
    if let Some(count) = env_count("CLAUDE_CODE_PLAN_V2_AGENT_COUNT") {
        return count;
    }
    let subscription = crate::utils::auth::get_subscription_type();
    let rate_limit_tier =
        crate::utils::auth::get_claude_ai_oauth_tokens().and_then(|tokens| tokens.rate_limit_tier);
    if (subscription.as_deref() == Some("max")
        && rate_limit_tier.as_deref() == Some("default_claude_max_20x"))
        || matches!(subscription.as_deref(), Some("enterprise" | "team"))
    {
        return 3;
    }
    1
}

/// Maps to CC `utils/planModeV2.ts:29-40`
/// `getPlanModeV2ExploreAgentCount()`.
pub fn get_plan_mode_v2_explore_agent_count() -> usize {
    env_count("CLAUDE_CODE_PLAN_V2_EXPLORE_AGENT_COUNT").unwrap_or(3)
}

/// Maps to CC `utils/planModeV2.ts:42-60`
/// `isPlanModeInterviewPhaseEnabled()`.
pub fn is_plan_mode_interview_phase_enabled() -> bool {
    if crate::utils::build_profile::has_internal_capability(
        crate::utils::build_profile::InternalCapability::Prompts,
    ) {
        return true;
    }
    // The `isEnvDefinedFalsy` branch and the GrowthBook fallback both yield
    // `false`: GrowthBook delivery is intentionally absent. Preserve its source
    // default rather than inventing a cohort assignment.
    crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_PLAN_MODE_INTERVIEW_PHASE").as_deref(),
    )
}

/// Maps to: CC `utils/planModeV2.ts:88-95#getPewterLedgerVariant`.
/// GrowthBook delivery is excluded by the existing project boundary; preserve
/// the source's null fallback without inventing a local experiment assignment.
pub fn get_pewter_ledger_variant() -> Option<&'static str> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[test]
    fn plan_counts_match_official_parse_int_prefix_semantics() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        for (input, expected) in [
            ("2x", Some(2)),
            (" +3.5", Some(3)),
            ("0x10", None),
            ("11", None),
            ("-2", None),
        ] {
            let _env = EnvVarGuard::set("CLAUDE_CODE_PLAN_V2_AGENT_COUNT", input);
            assert_eq!(env_count("CLAUDE_CODE_PLAN_V2_AGENT_COUNT"), expected);
        }
    }

    #[test]
    fn external_interview_gate_and_agent_counts_honor_official_env_precedence() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _interview = EnvVarGuard::set("CLAUDE_CODE_PLAN_MODE_INTERVIEW_PHASE", "true");
        let _agents = EnvVarGuard::set("CLAUDE_CODE_PLAN_V2_AGENT_COUNT", "2");
        let _explore = EnvVarGuard::set("CLAUDE_CODE_PLAN_V2_EXPLORE_AGENT_COUNT", "4");
        assert!(is_plan_mode_interview_phase_enabled());
        assert_eq!(get_plan_mode_v2_agent_count(), 2);
        assert_eq!(get_plan_mode_v2_explore_agent_count(), 4);
    }
}
