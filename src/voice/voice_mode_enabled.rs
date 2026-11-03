//! Maps to: CC `voice/voiceModeEnabled.ts`.
//!
//! The official module combines the build-time `feature('VOICE_MODE')` guard,
//! claude.ai OAuth-token auth, and GrowthBook kill-switch
//! `tengu_amber_quartz_disabled`. Voice authenticated only with the claude.ai
//! OAuth token, which Anthropic API-key-only auth does not have, so voice has no
//! auth source and is never enabled. Nyan keeps visibility decisions only: it
//! does not start voice streams, write settings, query GrowthBook, or log
//! analytics.

use crate::utils::feature_flags::{FeatureFlag, feature_enabled};

/// Maps to: CC `voice/voiceModeEnabled.ts:16-23`
/// `isVoiceGrowthBookEnabled`.
pub fn is_voice_growth_book_enabled() -> bool {
    cfg!(feature = "voice_mode") && !feature_enabled(FeatureFlag::VoiceModeDisabledKillswitch)
}

/// Maps to: CC `voice/voiceModeEnabled.ts:32-44` `hasVoiceAuth`.
pub fn has_voice_auth() -> bool {
    false
}

/// Maps to: CC `voice/voiceModeEnabled.ts:52-54` `isVoiceModeEnabled`.
pub fn is_voice_mode_enabled() -> bool {
    has_voice_auth() && is_voice_growth_book_enabled()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_growthbook_visibility_matches_official_process_build_state() {
        assert_eq!(is_voice_growth_book_enabled(), cfg!(feature = "voice_mode"));
    }

    #[test]
    fn voice_has_no_auth_source_so_it_is_never_enabled() {
        assert!(!has_voice_auth());
        assert!(!is_voice_mode_enabled());
    }
}
