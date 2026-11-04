//! API-key access for the Anthropic adapter, through the credential owner.
//!
//! @nyan: `utils::provider_auth` (PR 3) owns credential references: shape
//! validation, `env` / `optionalEnv` / `file` reads, the owner-only check on key
//! files, and the rule that `oauth` is not available to Anthropic. This module
//! asks it for exactly one thing, an API key for one provider profile, and adds
//! nothing of its own, so the adapter cannot drift from the owner's rules.

use std::fmt;

use crate::utils::provider_auth::reference::{
    CredentialError, CredentialSource, resolve_env_secret, resolve_file_secret,
};
use crate::utils::provider_auth::resolve::{ResolveError, validate_reference};
use crate::utils::settings::nyan::{CredentialReference, ProviderKind};

/// An API key. `Debug` and `Display` never print the value.
#[derive(Clone, PartialEq, Eq)]
pub struct ApiKey(String);

impl ApiKey {
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Test support: a key without going through a credential reference, for
    /// tests of code that merely carries one.
    #[cfg(test)]
    pub(crate) fn for_test(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKey(<redacted>)")
    }
}

#[derive(Debug)]
pub enum CredentialGateError {
    /// The provider has no `credential` block.
    NotConfigured { provider: String },
    /// The credential owner refused the reference or could not read it.
    Resolve {
        provider: String,
        source: ResolveError,
    },
}

impl fmt::Display for CredentialGateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotConfigured { provider } => write!(
                f,
                "Provider {provider:?} has no credential. Set settings.nyan.providers.{provider}.credential to {{\"env\": \"ANTHROPIC_API_KEY\"}} or another environment variable holding the API key."
            ),
            Self::Resolve { provider, source } => {
                write!(f, "Provider {provider:?} credential: {source}")
            }
        }
    }
}

impl std::error::Error for CredentialGateError {}

/// Resolve the API key a provider profile points at.
///
/// `Ok(None)` means the profile explicitly asks for no key (`none`), which
/// suits an Anthropic-compatible gateway that authenticates some other way.
pub fn resolve_api_key(
    provider: &str,
    credential: Option<&CredentialReference>,
) -> Result<Option<ApiKey>, CredentialGateError> {
    let resolve_error = |source: ResolveError| CredentialGateError::Resolve {
        provider: provider.to_string(),
        source,
    };
    let credential = credential.ok_or_else(|| CredentialGateError::NotConfigured {
        provider: provider.to_string(),
    })?;
    // Anthropic is API-key only: the owner rejects `oauth` for this kind.
    let source = validate_reference(ProviderKind::Anthropic, credential).map_err(resolve_error)?;
    let secret = match source {
        CredentialSource::Env { name, required } => resolve_env_secret(&name, required)
            .map_err(|e| resolve_error(ResolveError::Credential(e)))?,
        CredentialSource::File { path } => Some(
            resolve_file_secret(&path).map_err(|e| resolve_error(ResolveError::Credential(e)))?,
        ),
        CredentialSource::None => None,
        // `validate_reference` already refused this for Anthropic.
        CredentialSource::OAuth { .. } => {
            return Err(resolve_error(ResolveError::Credential(
                CredentialError::AmbiguousSource,
            )));
        }
    };
    Ok(secret.map(|secret| ApiKey(secret.expose().trim().to_string())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    fn reference(configure: impl FnOnce(&mut CredentialReference)) -> CredentialReference {
        let mut reference = CredentialReference::default();
        configure(&mut reference);
        reference
    }

    fn lock() -> crate::utils::test_env::TestEnvGuard<'static> {
        TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[test]
    fn env_reference_returns_the_trimmed_key() {
        let _lock = lock();
        let _key = EnvVarGuard::set("NYAN_CRED_TEST_KEY", "  sk-1 \n");
        let credential = reference(|r| r.env = Some("NYAN_CRED_TEST_KEY".into()));
        let key = resolve_api_key("a", Some(&credential)).unwrap().unwrap();
        assert_eq!(key.expose(), "sk-1");
    }

    #[test]
    fn missing_credential_block_is_a_gate_error() {
        assert!(matches!(
            resolve_api_key("a", None),
            Err(CredentialGateError::NotConfigured { .. })
        ));
        // An empty reference names no source at all; the owner rejects it.
        let error = resolve_api_key("a", Some(&CredentialReference::default())).unwrap_err();
        assert!(
            matches!(error, CredentialGateError::Resolve { .. }),
            "{error}"
        );
    }

    #[test]
    fn unset_or_blank_env_is_rejected_and_names_the_variable() {
        let _lock = lock();
        let _unset = EnvVarGuard::unset("NYAN_CRED_TEST_UNSET");
        let credential = reference(|r| r.env = Some("NYAN_CRED_TEST_UNSET".into()));
        let error = resolve_api_key("a", Some(&credential)).unwrap_err();
        assert!(
            error.to_string().contains("NYAN_CRED_TEST_UNSET"),
            "{error}"
        );
    }

    #[test]
    fn oauth_is_refused_with_the_owners_anthropic_message() {
        let credential = reference(|r| r.oauth = Some("openai-codex".into()));
        let error = resolve_api_key("a", Some(&credential)).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Anthropic supports API keys only"),
            "{error}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn file_reference_requires_an_owner_only_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("nyan-cred-file-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("key");
        std::fs::write(&path, "sk-from-file\n").unwrap();
        let credential = reference(|r| r.file = Some(path.to_string_lossy().into_owned()));

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            resolve_api_key("a", Some(&credential)).is_err(),
            "a world-readable key file must be refused"
        );

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let key = resolve_api_key("a", Some(&credential)).unwrap().unwrap();
        assert_eq!(key.expose(), "sk-from-file");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn none_allows_a_keyless_request() {
        let none = reference(|r| r.none = Some(true));
        assert!(resolve_api_key("a", Some(&none)).unwrap().is_none());
    }

    #[test]
    fn optional_env_is_rejected_for_anthropic() {
        let optional = reference(|r| r.optional_env = Some("NYAN_CRED_TEST_OPTIONAL".into()));
        let error = resolve_api_key("a", Some(&optional)).unwrap_err();
        assert!(
            matches!(error, CredentialGateError::Resolve { .. }),
            "{error}"
        );
        assert!(error.to_string().contains("optionalEnv"), "{error}");
    }

    #[test]
    fn two_sources_at_once_are_rejected() {
        let credential = reference(|r| {
            r.env = Some("K".into());
            r.file = Some("path".into());
        });
        assert!(resolve_api_key("a", Some(&credential)).is_err());
    }

    #[test]
    fn debug_output_never_contains_the_key() {
        let _lock = lock();
        let _key = EnvVarGuard::set("NYAN_CRED_TEST_SECRET", "sk-secret");
        let credential = reference(|r| r.env = Some("NYAN_CRED_TEST_SECRET".into()));
        let key = resolve_api_key("a", Some(&credential)).unwrap().unwrap();
        assert!(!format!("{key:?}").contains("sk-secret"));
    }
}
