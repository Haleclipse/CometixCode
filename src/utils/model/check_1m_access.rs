//! 1M-context access checks.
//! Maps to CC `utils/model/check1mAccess.ts`.
//!
//! The only input is the `CLAUDE_CODE_DISABLE_1M_CONTEXT` opt-out. The
//! subscription entitlement (cached extra-usage state) was part of the removed
//! claude.ai OAuth path; Anthropic is API-key only.

/// Maps to: CC `utils/model/check1mAccess.ts:46-58` `checkOpus1mAccess`.
pub fn check_opus_1m_access() -> bool {
    !crate::utils::context::is_1m_context_disabled()
}

/// Maps to: CC `utils/model/check1mAccess.ts:60-72` `checkSonnet1mAccess`.
pub fn check_sonnet_1m_access() -> bool {
    !crate::utils::context::is_1m_context_disabled()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::TEST_ENV_LOCK;

    /// The opt-out closes the gate; without it both checks pass.
    #[test]
    fn access_checks_honor_the_opt_out_and_otherwise_pass() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("CLAUDE_CODE_DISABLE_1M_CONTEXT", "1");
        assert!(!check_opus_1m_access());
        assert!(!check_sonnet_1m_access());
        crate::utils::process_env::remove("CLAUDE_CODE_DISABLE_1M_CONTEXT");

        assert!(check_opus_1m_access());
        assert!(check_sonnet_1m_access());
    }
}
