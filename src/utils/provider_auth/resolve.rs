//! One entry point from a provider's `credential` reference to a secret.
//!
//! The settings schema checks the shape of the reference. This module checks
//! what the shape cannot: that an `oauth` reference names a provider module
//! that exists and belongs to the provider's `kind` (decision 16: only OpenAI
//! Codex and OpenCode; Anthropic is API key only), and that `optionalEnv` only
//! pairs with local provider kinds (`vllm`, `ollama`). It does no network I/O
//! for `env`, `optionalEnv` or `file`, and for `oauth` only refreshes an
//! expired token.

use std::fmt;

use super::oauth::{self, openai_codex, opencode};
use super::reference::{
    CredentialError, CredentialSource, Secret, credential_source, resolve_env_secret,
    resolve_file_secret,
};
use super::refresh::{TokenError, fresh_credential};
use super::store::AuthStore;
use crate::utils::settings::nyan::{CredentialReference, ProviderKind};

/// Refresh a token this long before it expires, so a request that starts with a
/// valid token does not finish with an expired one.
const REFRESH_MARGIN_MS: i64 = 60_000;

#[derive(Debug)]
pub enum ResolveError {
    Credential(CredentialError),
    /// `oauth` names a provider nyan has no module for.
    UnknownOAuthProvider(String),
    /// `oauth` names a module that does not belong to this provider `kind`.
    OAuthNotAllowed {
        kind: ProviderKind,
        oauth: String,
    },
    /// `optionalEnv` is only allowed for local provider kinds (`vllm`, `ollama`).
    OptionalEnvNotAllowed {
        kind: ProviderKind,
    },
    Token(TokenError),
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Credential(error) => fmt::Display::fmt(error, f),
            Self::UnknownOAuthProvider(name) => write!(
                f,
                "oauth provider {name:?} is not supported; supported: {}",
                SUPPORTED.join(", ")
            ),
            Self::OAuthNotAllowed { kind, oauth } => write!(
                f,
                "oauth provider {oauth:?} cannot be used with provider kind {kind:?}{}",
                if *kind == ProviderKind::Anthropic {
                    "; Anthropic supports API keys only"
                } else {
                    ""
                }
            ),
            Self::OptionalEnvNotAllowed { kind } => write!(
                f,
                "optionalEnv credential cannot be used with provider kind {kind:?}; optionalEnv is only allowed for local provider kinds (vllm, ollama)"
            ),
            Self::Token(error) => fmt::Display::fmt(error, f),
        }
    }
}

impl std::error::Error for ResolveError {}

impl From<CredentialError> for ResolveError {
    fn from(error: CredentialError) -> Self {
        Self::Credential(error)
    }
}

impl From<TokenError> for ResolveError {
    fn from(error: TokenError) -> Self {
        Self::Token(error)
    }
}

const SUPPORTED: [&str; 2] = [openai_codex::PROVIDER_ID, opencode::PROVIDER_ID];

/// Which OAuth module a provider `kind` may use. OpenCode Console is an
/// aggregating gateway, so it pairs with `gateway`.
fn kind_allows(kind: ProviderKind, oauth: &str) -> Result<(), ResolveError> {
    if !SUPPORTED.contains(&oauth) {
        return Err(ResolveError::UnknownOAuthProvider(oauth.to_string()));
    }
    let allowed = matches!(
        (kind, oauth),
        (ProviderKind::Openai, openai_codex::PROVIDER_ID)
            | (ProviderKind::Gateway, opencode::PROVIDER_ID)
    );
    if allowed {
        Ok(())
    } else {
        Err(ResolveError::OAuthNotAllowed {
            kind,
            oauth: oauth.to_string(),
        })
    }
}

/// Validates a reference against its provider `kind` without reading anything.
pub fn validate_reference(
    kind: ProviderKind,
    reference: &CredentialReference,
) -> Result<CredentialSource, ResolveError> {
    let source = credential_source(reference)?;
    if let CredentialSource::OAuth { provider } = &source {
        kind_allows(kind, provider)?;
    }
    if let CredentialSource::Env {
        required: false, ..
    } = &source
    {
        if !kind.is_local() {
            return Err(ResolveError::OptionalEnvNotAllowed { kind });
        }
    }
    Ok(source)
}

/// What a credential reference yields.
#[derive(Debug)]
pub enum Resolved {
    /// A secret to send as the provider's credential.
    Secret(Secret),
    /// `optionalEnv` was unset, or `none` was configured: send no credential.
    /// A later 401 is still an auth error.
    Absent,
}

/// Resolves credentials for one process: where OAuth state lives and how to
/// reach the token endpoints.
pub struct Resolver {
    store: AuthStore,
    client: reqwest::Client,
    codex: openai_codex::Endpoints,
}

impl Resolver {
    pub fn new(store: AuthStore) -> anyhow::Result<Self> {
        Ok(Self {
            store,
            client: oauth::http_client()?,
            codex: openai_codex::Endpoints::default(),
        })
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        store: AuthStore,
        client: reqwest::Client,
        codex: openai_codex::Endpoints,
    ) -> Self {
        Self {
            store,
            client,
            codex,
        }
    }

    pub async fn resolve(
        &self,
        kind: ProviderKind,
        reference: &CredentialReference,
        account: Option<&str>,
    ) -> Result<Resolved, ResolveError> {
        match validate_reference(kind, reference)? {
            CredentialSource::Env { name, required } => {
                Ok(match resolve_env_secret(&name, required)? {
                    Some(secret) => Resolved::Secret(secret),
                    None => Resolved::Absent,
                })
            }
            CredentialSource::File { path } => Ok(Resolved::Secret(resolve_file_secret(&path)?)),
            CredentialSource::None => Ok(Resolved::Absent),
            CredentialSource::OAuth { provider } => {
                let now = oauth::now_ms();
                let credential = match provider.as_str() {
                    openai_codex::PROVIDER_ID => {
                        fresh_credential(
                            &self.store,
                            &provider,
                            account,
                            now,
                            REFRESH_MARGIN_MS,
                            |stored| openai_codex::refresh(&self.client, &self.codex, stored, now),
                        )
                        .await?
                    }
                    opencode::PROVIDER_ID => {
                        fresh_credential(
                            &self.store,
                            &provider,
                            account,
                            now,
                            REFRESH_MARGIN_MS,
                            |stored| opencode::refresh(&self.client, stored, now),
                        )
                        .await?
                    }
                    other => return Err(ResolveError::UnknownOAuthProvider(other.to_string())),
                };
                Ok(Resolved::Secret(credential.access_secret()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::web::test_support::{Route, serve};
    use crate::utils::provider_auth::store::{OAuthCredential, ProviderAuth};
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};
    use std::collections::BTreeMap;

    struct Scratch(std::path::PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(name: &str) -> (Scratch, AuthStore) {
        let root = crate::utils::node_os::tmpdir().join(format!(
            "nyan-pr3-resolve-{name}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        (Scratch(root.clone()), AuthStore::at(root.join("auth")))
    }

    fn reference(json: serde_json::Value) -> CredentialReference {
        serde_json::from_value(json).unwrap()
    }

    fn resolver(store: AuthStore, codex_base: &str) -> Resolver {
        let builder = crate::utils::proxy::client_builder().no_proxy();
        Resolver::for_test(
            store,
            oauth::build_client(builder).unwrap(),
            openai_codex::Endpoints::at(codex_base),
        )
    }

    fn seed(store: &AuthStore, provider: &str, account: &str, credential: OAuthCredential) {
        let mut auth = ProviderAuth::default();
        auth.set_account(account, credential);
        store.lock(provider).unwrap().save(&auth).unwrap();
    }

    fn live(access: &str) -> OAuthCredential {
        OAuthCredential {
            access: access.into(),
            refresh: "r".into(),
            expires_at_ms: i64::MAX / 2,
            account_id: None,
            metadata: BTreeMap::new(),
        }
    }

    fn secret_of(resolved: Resolved) -> String {
        match resolved {
            Resolved::Secret(secret) => secret.expose().to_string(),
            Resolved::Absent => panic!("expected a secret"),
        }
    }

    /// The new credential path must not reach the inherited Anthropic OAuth
    /// code or its compile-time gate; those stay untouched until the Anthropic
    /// adapter replaces them.
    #[test]
    fn provider_auth_does_not_depend_on_the_legacy_anthropic_oauth_path() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/utils/provider_auth");
        let forbidden = [
            "OAUTH_CREDENTIAL_SIDE_EFFECTS",
            "crate::services::oauth",
            "crate::utils::auth",
            "claude_ai_oauth",
            "ClaudeAiOAuth",
        ];
        let mut stack = vec![root];
        let mut scanned = 0;
        while let Some(path) = stack.pop() {
            for entry in std::fs::read_dir(&path).unwrap() {
                let entry = entry.unwrap().path();
                if entry.is_dir() {
                    stack.push(entry);
                } else if entry.extension().is_some_and(|ext| ext == "rs") {
                    let text = std::fs::read_to_string(&entry).unwrap();
                    // Production code only: the test modules name the patterns.
                    let code = text.split("#[cfg(test)]\nmod tests").next().unwrap();
                    for pattern in forbidden {
                        let hit = code
                            .lines()
                            .filter(|line| !line.trim_start().starts_with("//"))
                            .any(|line| line.contains(pattern));
                        assert!(!hit, "{} references {pattern}", entry.display());
                    }
                    scanned += 1;
                }
            }
        }
        assert!(scanned >= 8, "scanned only {scanned} files");
    }

    #[test]
    fn anthropic_can_never_use_oauth() {
        for oauth in ["openai-codex", "opencode", "anthropic", "claude-ai"] {
            let error = validate_reference(
                ProviderKind::Anthropic,
                &reference(serde_json::json!({ "oauth": oauth })),
            )
            .unwrap_err();
            assert!(
                matches!(
                    error,
                    ResolveError::OAuthNotAllowed { .. } | ResolveError::UnknownOAuthProvider(_)
                ),
                "{oauth}"
            );
        }
        let text = validate_reference(
            ProviderKind::Anthropic,
            &reference(serde_json::json!({ "oauth": "openai-codex" })),
        )
        .unwrap_err()
        .to_string();
        assert!(text.contains("API keys only"));
    }

    #[test]
    fn oauth_provider_must_match_the_provider_kind() {
        let codex = reference(serde_json::json!({ "oauth": "openai-codex" }));
        let opencode = reference(serde_json::json!({ "oauth": "opencode" }));
        assert!(validate_reference(ProviderKind::Openai, &codex).is_ok());
        assert!(validate_reference(ProviderKind::Gateway, &opencode).is_ok());
        assert!(validate_reference(ProviderKind::Gateway, &codex).is_err());
        assert!(validate_reference(ProviderKind::Openai, &opencode).is_err());
        assert!(validate_reference(ProviderKind::Gemini, &codex).is_err());
        assert!(validate_reference(ProviderKind::Vllm, &opencode).is_err());
        assert!(matches!(
            validate_reference(
                ProviderKind::Openai,
                &reference(serde_json::json!({ "oauth": "../x" }))
            ),
            Err(ResolveError::UnknownOAuthProvider(_))
        ));
    }

    #[test]
    fn non_oauth_sources_validate_for_every_kind() {
        for kind in [
            ProviderKind::Anthropic,
            ProviderKind::Vllm,
            ProviderKind::Gemini,
        ] {
            assert!(validate_reference(kind, &reference(serde_json::json!({"env": "K"}))).is_ok());
            assert!(
                validate_reference(kind, &reference(serde_json::json!({"none": true}))).is_ok()
            );
        }
    }

    #[test]
    fn commercial_kinds_reject_optional_env() {
        for kind in [
            ProviderKind::Anthropic,
            ProviderKind::Openai,
            ProviderKind::Gemini,
            ProviderKind::Openrouter,
            ProviderKind::Gateway,
        ] {
            let error = validate_reference(
                kind,
                &reference(serde_json::json!({"optionalEnv": "OPTIONAL_KEY"})),
            )
            .unwrap_err();
            assert!(
                matches!(error, ResolveError::OptionalEnvNotAllowed { kind: k } if k == kind),
                "{kind:?}"
            );
            assert!(
                error
                    .to_string()
                    .contains("only allowed for local provider kinds")
            );
        }
    }

    #[test]
    fn local_kinds_accept_optional_env() {
        for kind in [ProviderKind::Vllm, ProviderKind::Ollama] {
            assert!(
                validate_reference(
                    kind,
                    &reference(serde_json::json!({"optionalEnv": "OPTIONAL_KEY"})),
                )
                .is_ok(),
                "{kind:?}"
            );
        }
    }

    #[tokio::test]
    async fn env_optional_env_and_none_resolve_without_touching_the_store() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let (_scratch, store) = scratch("env");
        let resolver = resolver(store.clone(), "http://127.0.0.1:1");
        let _set = EnvVarGuard::set("NYAN_PR3_RESOLVE_KEY", "sk-env-1");
        let _unset = EnvVarGuard::unset("NYAN_PR3_RESOLVE_OPTIONAL");

        let required = resolver
            .resolve(
                ProviderKind::Anthropic,
                &reference(serde_json::json!({"env": "NYAN_PR3_RESOLVE_KEY"})),
                None,
            )
            .await
            .unwrap();
        assert_eq!(secret_of(required), "sk-env-1");

        let optional = resolver
            .resolve(
                ProviderKind::Vllm,
                &reference(serde_json::json!({"optionalEnv": "NYAN_PR3_RESOLVE_OPTIONAL"})),
                None,
            )
            .await
            .unwrap();
        assert!(matches!(optional, Resolved::Absent));

        let none = resolver
            .resolve(
                ProviderKind::Ollama,
                &reference(serde_json::json!({"none": true})),
                None,
            )
            .await
            .unwrap();
        assert!(matches!(none, Resolved::Absent));

        let missing = resolver
            .resolve(
                ProviderKind::Anthropic,
                &reference(serde_json::json!({"env": "NYAN_PR3_RESOLVE_OPTIONAL"})),
                None,
            )
            .await
            .unwrap_err();
        assert!(matches!(
            missing,
            ResolveError::Credential(CredentialError::MissingEnv(_))
        ));
        assert!(
            !store.dir().exists(),
            "env sources never create the auth dir"
        );
    }

    #[tokio::test]
    async fn file_source_reads_an_owner_only_key() {
        let (scratch, store) = scratch("file");
        std::fs::create_dir_all(&scratch.0).unwrap();
        let key = scratch.0.join("key");
        crate::utils::secure_storage::plain_text_storage::write_owner_only_file(
            &key,
            "sk-file-9\n",
        )
        .unwrap();
        let resolved = resolver(store, "http://127.0.0.1:1")
            .resolve(
                ProviderKind::Openai,
                &reference(serde_json::json!({"file": key.to_str().unwrap()})),
                None,
            )
            .await
            .unwrap();
        assert_eq!(secret_of(resolved), "sk-file-9");
    }

    #[tokio::test]
    async fn oauth_returns_a_live_token_without_network() {
        let (_scratch, store) = scratch("live");
        seed(&store, "openai-codex", "work", live("access-live"));
        let resolved = resolver(store, "http://127.0.0.1:1")
            .resolve(
                ProviderKind::Openai,
                &reference(serde_json::json!({"oauth": "openai-codex"})),
                None,
            )
            .await
            .unwrap();
        assert_eq!(secret_of(resolved), "access-live");
    }

    #[tokio::test]
    async fn oauth_refreshes_an_expired_token_and_persists_it() {
        let (_scratch, store) = scratch("refresh");
        let mut old = live("access-old");
        old.expires_at_ms = 0;
        old.refresh = "ref-old".into();
        seed(&store, "openai-codex", "work", old);
        let claim = serde_json::json!({
            "https://api.openai.com/auth": { "chatgpt_account_id": "acct_1" }
        });
        let jwt = format!(
            "e30.{}.sig",
            base64::Engine::encode(
                &base64::engine::general_purpose::URL_SAFE_NO_PAD,
                claim.to_string()
            )
        );
        let server = serve(vec![(
            "/oauth/token",
            Route::new(
                200,
                "application/json",
                serde_json::json!({
                    "access_token": jwt, "refresh_token": "ref-new", "expires_in": 3600
                })
                .to_string(),
            ),
        )])
        .await;
        let resolver = resolver(store.clone(), &server.origin);
        let resolved = resolver
            .resolve(
                ProviderKind::Openai,
                &reference(serde_json::json!({"oauth": "openai-codex"})),
                Some("work"),
            )
            .await
            .unwrap();
        assert_eq!(secret_of(resolved), jwt);
        let stored = store.load("openai-codex").unwrap().unwrap();
        assert_eq!(stored.account(Some("work")).unwrap().1.refresh, "ref-new");
        assert_eq!(server.hits("/oauth/token"), 1);
    }

    #[tokio::test]
    async fn oauth_without_a_sign_in_names_the_provider() {
        let (_scratch, store) = scratch("signed-out");
        let error = resolver(store, "http://127.0.0.1:1")
            .resolve(
                ProviderKind::Gateway,
                &reference(serde_json::json!({"oauth": "opencode"})),
                None,
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            ResolveError::Token(TokenError::NotSignedIn { .. })
        ));
        assert!(error.to_string().contains("opencode"));
    }

    #[tokio::test]
    async fn oauth_tokens_do_not_cross_providers() {
        let (_scratch, store) = scratch("cross");
        seed(&store, "openai-codex", "a", live("codex-token"));
        let error = resolver(store, "http://127.0.0.1:1")
            .resolve(
                ProviderKind::Gateway,
                &reference(serde_json::json!({"oauth": "opencode"})),
                None,
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            ResolveError::Token(TokenError::NotSignedIn { .. })
        ));
        assert!(!error.to_string().contains("codex-token"));
    }
}
