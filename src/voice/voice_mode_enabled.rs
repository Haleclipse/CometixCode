//! Maps to: CC `voice/voiceModeEnabled.ts`.
//!
//! The official module combines the build-time `feature('VOICE_MODE')` guard,
//! OAuth-token auth, and GrowthBook kill-switch `tengu_amber_quartz_disabled`.
//! Cometix keeps the same boundary for visibility decisions only: it does not
//! start voice streams, refresh OAuth tokens, write settings, query GrowthBook,
//! or log analytics.

use crate::utils::feature_flags::{FeatureFlag, feature_enabled};

/// Maps to: CC `voice/voiceModeEnabled.ts:16-23`
/// `isVoiceGrowthBookEnabled`.
pub fn is_voice_growth_book_enabled() -> bool {
    cfg!(feature = "voice_mode") && !feature_enabled(FeatureFlag::VoiceModeDisabledKillswitch)
}

/// Maps to: CC `voice/voiceModeEnabled.ts:32-44` `hasVoiceAuth`.
pub fn has_voice_auth() -> bool {
    if !crate::utils::auth::is_anthropic_auth_enabled() {
        return false;
    }
    crate::utils::auth::get_claude_ai_oauth_tokens()
        .is_some_and(|tokens| !tokens.access_token.is_empty())
}

/// Maps to: CC `voice/voiceModeEnabled.ts:52-54` `isVoiceModeEnabled`.
pub fn is_voice_mode_enabled() -> bool {
    has_voice_auth() && is_voice_growth_book_enabled()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::config::GlobalConfig;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK, TestEnvGuard};

    struct GlobalConfigGuard(Option<GlobalConfig>);

    impl GlobalConfigGuard {
        fn install() -> Self {
            let mut config = GlobalConfig::default();
            // Prevent the process-state auth check from inheriting an API-key
            // helper from the developer's settings fixture.
            config.api_key_helper = Some(String::new());
            Self(crate::utils::config::replace_test_global_config(Some(
                config,
            )))
        }
    }

    impl Drop for GlobalConfigGuard {
        fn drop(&mut self) {
            crate::utils::config::replace_test_global_config(self.0.take());
        }
    }

    struct VoiceProcessStateFixture {
        _env: Vec<EnvVarGuard>,
        _config: GlobalConfigGuard,
        _env_lock: TestEnvGuard<'static>,
    }

    impl VoiceProcessStateFixture {
        fn with_oauth_token() -> Self {
            let env_lock = TEST_ENV_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let env = vec![
                EnvVarGuard::set("NODE_ENV", "test"),
                EnvVarGuard::set("CLAUDE_CODE_OAUTH_TOKEN", "oauth-token"),
                EnvVarGuard::unset("CLAUDE_CODE_SIMPLE"),
                EnvVarGuard::unset("CLAUDE_CODE_USE_BEDROCK"),
                EnvVarGuard::unset("CLAUDE_CODE_USE_VERTEX"),
                EnvVarGuard::unset("CLAUDE_CODE_USE_FOUNDRY"),
                EnvVarGuard::unset("ANTHROPIC_UNIX_SOCKET"),
                EnvVarGuard::unset("ANTHROPIC_AUTH_TOKEN"),
                EnvVarGuard::unset("ANTHROPIC_API_KEY"),
                EnvVarGuard::unset("CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR"),
            ];
            Self {
                _env: env,
                _config: GlobalConfigGuard::install(),
                _env_lock: env_lock,
            }
        }
    }

    #[test]
    fn voice_growthbook_visibility_matches_official_process_build_state() {
        assert_eq!(is_voice_growth_book_enabled(), cfg!(feature = "voice_mode"));
    }

    #[test]
    fn voice_auth_and_mode_enablement_matches_official_process_state() {
        let _fixture = VoiceProcessStateFixture::with_oauth_token();

        assert!(has_voice_auth());
        assert_eq!(is_voice_mode_enabled(), cfg!(feature = "voice_mode"));

        let _bedrock = EnvVarGuard::set("CLAUDE_CODE_USE_BEDROCK", "1");
        assert!(!has_voice_auth());
        assert!(!is_voice_mode_enabled());
    }
}
