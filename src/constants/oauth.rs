//! OAuth constants shared with the MCP client.
//!
//! Anthropic account OAuth is gone: nyan has no claude.ai/Console login, token
//! refresh, or account-scoped credential state, and reaches Anthropic through a
//! provider profile API key only. What remains here is the default-closed
//! side-effect gate that the inherited credential outlets still consult.

/// No CC counterpart. This is the sole user-authorized L2 product switch for
/// OAuth network, process/listener, credential-storage, revocation, and
/// credential-cache side effects. It is intentionally compile-time only.
pub const OAUTH_CREDENTIAL_SIDE_EFFECTS_ENABLED: bool = false;

/// Stable message for the user-authorized default-closed OAuth outlet gate.
pub const OAUTH_CREDENTIAL_SIDE_EFFECTS_UNAVAILABLE_MESSAGE: &str =
    "OAuth credential side effects are unavailable in this product build";

/// Minimal typed L2 failure shared by source-owned OAuth outlet functions.
///
/// This type carries no policy; each canonical source function checks the one
/// constant immediately before its own final side effect.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OAuthCredentialSideEffectsUnavailable;

impl std::fmt::Display for OAuthCredentialSideEffectsUnavailable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(OAUTH_CREDENTIAL_SIDE_EFFECTS_UNAVAILABLE_MESSAGE)
    }
}

impl std::error::Error for OAuthCredentialSideEffectsUnavailable {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oauth_side_effect_switch_is_single_typed_and_default_closed() {
        assert!(!OAUTH_CREDENTIAL_SIDE_EFFECTS_ENABLED);
        let error = anyhow::Error::new(OAuthCredentialSideEffectsUnavailable);
        assert_eq!(
            error.root_cause().to_string(),
            OAUTH_CREDENTIAL_SIDE_EFFECTS_UNAVAILABLE_MESSAGE
        );
        assert!(
            error
                .downcast_ref::<OAuthCredentialSideEffectsUnavailable>()
                .is_some()
        );
    }
}
