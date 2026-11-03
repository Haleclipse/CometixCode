//! Resolves a `credential` reference from provider settings to a secret.
//!
//! `env`, `optionalEnv` and `file` read a value here. `oauth` returns its
//! provider name; the token itself belongs to that provider's owner module.
//! Errors name the source, never the value.

use std::fmt;

use crate::utils::settings::nyan::CredentialReference;

/// A secret value. `Debug` and `Display` never print the contents.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// The validated shape of a `credential` reference. Exactly one source is set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialSource {
    Env { name: String, required: bool },
    File { path: String },
    OAuth { provider: String },
    None,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialError {
    /// Zero or more than one source is set. The settings schema rejects this,
    /// but the resolver checks again because it also receives in-memory values.
    AmbiguousSource,
    /// A required env variable is unset or empty. Carries the name, not a value.
    MissingEnv(String),
    /// The `file` source could not be read safely. Carries the path and the
    /// reason, never file contents.
    File { path: String, reason: String },
}

impl fmt::Display for CredentialError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AmbiguousSource => write!(
                f,
                "credential must name exactly one of env, file, optionalEnv, oauth, or none"
            ),
            Self::MissingEnv(name) => write!(
                f,
                "credential environment variable {name} is not set; set it or use a credential source that exists"
            ),
            Self::File { path, reason } => write!(
                f,
                "credential file {path} cannot be used: {reason}; use an owner-only file or an env variable"
            ),
        }
    }
}

impl std::error::Error for CredentialError {}

/// Turns a settings reference into exactly one source. Rejects zero or several.
pub fn credential_source(
    reference: &CredentialReference,
) -> Result<CredentialSource, CredentialError> {
    let mut sources = Vec::new();
    if let Some(name) = &reference.env {
        sources.push(CredentialSource::Env {
            name: name.clone(),
            required: true,
        });
    }
    if let Some(path) = &reference.file {
        sources.push(CredentialSource::File { path: path.clone() });
    }
    if let Some(name) = &reference.optional_env {
        sources.push(CredentialSource::Env {
            name: name.clone(),
            required: false,
        });
    }
    if let Some(provider) = &reference.oauth {
        sources.push(CredentialSource::OAuth {
            provider: provider.clone(),
        });
    }
    if reference.none == Some(true) {
        sources.push(CredentialSource::None);
    }
    match <[CredentialSource; 1]>::try_from(sources) {
        Ok([source]) => Ok(source),
        Err(_) => Err(CredentialError::AmbiguousSource),
    }
}

/// Reads an env-sourced secret through the process env carrier. An empty
/// value counts as unset, matching the JS truthiness used across nyan.
pub fn resolve_env_secret(name: &str, required: bool) -> Result<Option<Secret>, CredentialError> {
    match crate::utils::process_env::var(name).filter(|value| !value.is_empty()) {
        Some(value) => Ok(Some(Secret(value))),
        None if required => Err(CredentialError::MissingEnv(name.to_string())),
        None => Ok(None),
    }
}

/// Expands a leading `~/` against the home directory. Other paths are used as
/// written.
fn expand_credential_path(path: &str) -> std::path::PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => crate::utils::node_os::homedir().join(rest),
        None => std::path::PathBuf::from(path),
    }
}

/// Reads a `file` source: the file must be owner-only (Unix 0600, Windows
/// verified owner-only ACL) and holds one non-empty line. Anything else is
/// refused with the path and reason, never the contents.
pub fn resolve_file_secret(path: &str) -> Result<Secret, CredentialError> {
    let fail = |reason: String| CredentialError::File {
        path: path.to_string(),
        reason,
    };
    let resolved = expand_credential_path(path);
    crate::utils::secure_storage::plain_text_storage::verify_owner_only_file(&resolved)
        .map_err(|error| fail(error.to_string()))?;
    let text = std::fs::read_to_string(&resolved).map_err(|error| fail(error.to_string()))?;
    let mut lines = text.lines().map(str::trim).filter(|line| !line.is_empty());
    match (lines.next(), lines.next()) {
        (Some(line), None) => Ok(Secret::new(line)),
        (None, _) => Err(fail("file is empty".to_string())),
        (Some(_), Some(_)) => Err(fail("file must hold a single line".to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::EnvVarGuard;

    fn reference(json: serde_json::Value) -> CredentialReference {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn exactly_one_source_is_accepted() {
        assert_eq!(
            credential_source(&reference(serde_json::json!({"env": "A"}))),
            Ok(CredentialSource::Env {
                name: "A".into(),
                required: true
            })
        );
        assert_eq!(
            credential_source(&reference(serde_json::json!({"optionalEnv": "B"}))),
            Ok(CredentialSource::Env {
                name: "B".into(),
                required: false
            })
        );
        assert_eq!(
            credential_source(&reference(serde_json::json!({"file": "/k"}))),
            Ok(CredentialSource::File { path: "/k".into() })
        );
        assert_eq!(
            credential_source(&reference(serde_json::json!({"oauth": "openai-codex"}))),
            Ok(CredentialSource::OAuth {
                provider: "openai-codex".into()
            })
        );
        assert_eq!(
            credential_source(&reference(serde_json::json!({"none": true}))),
            Ok(CredentialSource::None)
        );
    }

    #[test]
    fn zero_or_several_sources_are_rejected() {
        assert_eq!(
            credential_source(&CredentialReference::default()),
            Err(CredentialError::AmbiguousSource)
        );
        assert_eq!(
            credential_source(&reference(serde_json::json!({"env": "A", "file": "B"}))),
            Err(CredentialError::AmbiguousSource)
        );
    }

    #[test]
    fn required_env_missing_or_empty_is_an_error_without_value() {
        let _unset = EnvVarGuard::unset("NYAN_PR3_TEST_KEY");
        assert_eq!(
            resolve_env_secret("NYAN_PR3_TEST_KEY", true),
            Err(CredentialError::MissingEnv("NYAN_PR3_TEST_KEY".into()))
        );
        let _empty = EnvVarGuard::set("NYAN_PR3_TEST_KEY", "");
        assert_eq!(
            resolve_env_secret("NYAN_PR3_TEST_KEY", true),
            Err(CredentialError::MissingEnv("NYAN_PR3_TEST_KEY".into()))
        );
        assert_eq!(resolve_env_secret("NYAN_PR3_TEST_KEY", false), Ok(None));
    }

    fn scratch_dir(name: &str) -> std::path::PathBuf {
        let dir = crate::utils::node_os::tmpdir().join(format!(
            "nyan-pr3-ref-{name}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn file_secret_reads_one_owner_only_line() {
        let dir = scratch_dir("ok");
        let path = dir.join("key");
        crate::utils::secure_storage::plain_text_storage::write_owner_only_file(
            &path,
            "sk-file-123\n",
        )
        .unwrap();
        let secret = resolve_file_secret(path.to_str().unwrap()).unwrap();
        assert_eq!(secret.expose(), "sk-file-123");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_secret_rejects_empty_multiline_and_missing_without_leaking() {
        let dir = scratch_dir("bad");
        let write = crate::utils::secure_storage::plain_text_storage::write_owner_only_file;
        let empty = dir.join("empty");
        write(&empty, "\n").unwrap();
        let two = dir.join("two");
        write(&two, "sk-first\nsk-second\n").unwrap();
        for path in [&empty, &two, &dir.join("missing")] {
            let error = resolve_file_secret(path.to_str().unwrap()).unwrap_err();
            let message = error.to_string();
            assert!(matches!(error, CredentialError::File { .. }));
            assert!(!message.contains("sk-first") && !message.contains("sk-second"));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Runs on the Windows CI job: a key file with an inherited ACL is refused,
    /// and one written through the owner-only writer is accepted.
    #[cfg(windows)]
    #[test]
    fn windows_smoke_file_secret_requires_owner_only_acl() {
        let dir = scratch_dir("win");
        let loose = dir.join("loose");
        std::fs::write(&loose, "sk-loose\n").unwrap();
        let error = resolve_file_secret(loose.to_str().unwrap()).unwrap_err();
        assert!(matches!(error, CredentialError::File { .. }));
        assert!(!error.to_string().contains("sk-loose"));

        let strict = dir.join("strict");
        crate::utils::secure_storage::plain_text_storage::write_owner_only_file(
            &strict,
            "sk-strict\n",
        )
        .unwrap();
        assert_eq!(
            resolve_file_secret(strict.to_str().unwrap())
                .unwrap()
                .expose(),
            "sk-strict"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn file_secret_rejects_group_readable_file_and_symlink() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = scratch_dir("perm");
        let loose = dir.join("loose");
        std::fs::write(&loose, "sk-loose\n").unwrap();
        std::fs::set_permissions(&loose, std::fs::Permissions::from_mode(0o640)).unwrap();
        let error = resolve_file_secret(loose.to_str().unwrap()).unwrap_err();
        assert!(error.to_string().contains("insecure permissions"));
        assert!(!error.to_string().contains("sk-loose"));

        let strict = dir.join("strict");
        std::fs::write(&strict, "sk-strict\n").unwrap();
        std::fs::set_permissions(&strict, std::fs::Permissions::from_mode(0o600)).unwrap();
        let link = dir.join("link");
        std::os::unix::fs::symlink(&strict, &link).unwrap();
        assert!(resolve_file_secret(link.to_str().unwrap()).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn env_secret_resolves_and_never_prints() {
        let _set = EnvVarGuard::set("NYAN_PR3_TEST_KEY", "sk-live-123");
        let secret = resolve_env_secret("NYAN_PR3_TEST_KEY", true)
            .unwrap()
            .unwrap();
        assert_eq!(secret.expose(), "sk-live-123");
        assert!(!format!("{secret:?}").contains("sk-live-123"));
        assert_eq!(format!("{secret:?}"), "Secret(<redacted>)");
    }
}
