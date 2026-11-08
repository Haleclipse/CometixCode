//! @nyan: what the configured providers' credentials resolve to, without a
//! request: whether the session's profile has one, and which values must never
//! leave the process in error text.

use crate::utils::provider_auth::reference::Secret;
use crate::utils::provider_auth::resolve::offline_secrets;
use crate::utils::provider_auth::store::AuthStore;
use crate::utils::settings::SettingsJson;
use crate::utils::settings::nyan::ProviderSettings;

/// Shorter values are left alone: replacing them would garble unrelated text.
const MIN_REDACTED_LEN: usize = 8;

/// Variables an inherited Anthropic code path may still read a token from.
const AMBIENT_SECRET_VARS: [&str; 3] = [
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
];

/// The secrets `provider` would send, or `None` when its credential cannot be
/// resolved the way a request resolves it.
fn provider_secrets(provider: &ProviderSettings, store: &AuthStore) -> Option<Vec<Secret>> {
    let kind = provider.kind?;
    provider
        .credential
        .as_ref()
        .and_then(|reference| offline_secrets(kind, reference, store).ok())
}

/// The provider of `profile` (`None`: `selection.main`) when its credential
/// cannot be resolved; `None` when it can, or when no profile resolves (that
/// is reported where the profile is used).
pub fn missing_credential(
    settings: &SettingsJson,
    profile: Option<&str>,
    store: &AuthStore,
) -> Option<String> {
    let resolved = crate::utils::model::profile::resolve_request_profile(settings, profile).ok()?;
    let provider = settings
        .nyan
        .as_deref()?
        .providers
        .as_ref()?
        .get(&resolved.provider)?;
    provider_secrets(provider, store)
        .is_none()
        .then_some(resolved.provider)
}

/// `text` on one line, with every credential value the configured providers
/// resolve to, and the ambient Anthropic token variables (names and values),
/// replaced by `<redacted>`.
pub fn redact_credentials(text: &str) -> String {
    let settings = crate::utils::settings::get_initial_settings();
    let store = AuthStore::user();
    let mut secrets = settings
        .nyan
        .as_deref()
        .and_then(|nyan| nyan.providers.as_ref())
        .into_iter()
        .flat_map(|providers| providers.values())
        .filter_map(|provider| provider_secrets(provider, &store))
        .flatten()
        .map(|secret| secret.expose().trim().to_string())
        .chain(
            AMBIENT_SECRET_VARS
                .into_iter()
                .filter_map(crate::utils::process_env::var)
                .map(|value| value.trim().to_string()),
        )
        .filter(|secret| secret.len() >= MIN_REDACTED_LEN)
        .collect::<Vec<_>>();
    // Longest first, so a secret that contains another is replaced whole.
    secrets.sort_by_key(|secret| std::cmp::Reverse(secret.len()));
    let mut redacted = text.replace('\n', " ");
    for name in AMBIENT_SECRET_VARS {
        redacted = redacted.replace(name, "<redacted>");
    }
    for secret in &secrets {
        redacted = redacted.replace(secret.as_str(), "<redacted>");
    }
    redacted
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK, TestEnvGuard};

    struct Fixture {
        _lock: TestEnvGuard<'static>,
        _guards: Vec<EnvVarGuard>,
        root: std::path::PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let lock = TEST_ENV_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let root = crate::utils::node_os::tmpdir()
                .join(format!("nyan-provider-credential-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&root).unwrap();
            let guards = AMBIENT_SECRET_VARS
                .into_iter()
                .map(EnvVarGuard::unset)
                .collect();
            Self {
                _lock: lock,
                _guards: guards,
                root,
            }
        }

        fn store(&self) -> AuthStore {
            AuthStore::at(self.root.join("auth"))
        }

        fn key_file(&self, value: &str) -> String {
            let path = self.root.join("key");
            crate::utils::secure_storage::plain_text_storage::write_owner_only_file(
                &path,
                &format!("{value}\n"),
            )
            .unwrap();
            path.to_string_lossy().into_owned()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            crate::utils::settings::settings_cache::reset_settings_cache();
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn settings(providers: serde_json::Value) -> SettingsJson {
        serde_json::from_value(serde_json::json!({"nyan": {
            "providers": providers,
            "models": {
                "main": {"provider": "deepseek", "model": "deepseek-v4",
                    "capabilities": {"contextWindow": 128000, "maxOutputTokens": 8192}},
                "claude": {"provider": "anthropic", "model": "claude-sonnet-4-6",
                    "capabilities": {"contextWindow": 200000, "maxOutputTokens": 64000}}
            },
            "selection": {"main": "main"}
        }}))
        .unwrap()
    }

    fn openai_compatible(credential: serde_json::Value) -> serde_json::Value {
        serde_json::json!({"kind": "openai", "protocol": "openaiChat",
            "baseUrl": "https://api.deepseek.example", "credential": credential})
    }

    #[test]
    fn a_readable_credential_file_is_not_missing() {
        let fixture = Fixture::new();
        let file = fixture.key_file("sk-fake-deepseek-file-0001");
        let settings = settings(serde_json::json!({
            "deepseek": openai_compatible(serde_json::json!({"file": file}))
        }));
        assert_eq!(missing_credential(&settings, None, &fixture.store()), None);
    }

    #[test]
    fn an_absent_credential_names_the_profiles_provider() {
        let fixture = Fixture::new();
        let _unset = EnvVarGuard::unset("NYAN_FAKE_DEEPSEEK_KEY");
        let absent_file = fixture.root.join("no-such-key");
        for credential in [
            serde_json::json!({"file": absent_file.to_string_lossy()}),
            serde_json::json!({"env": "NYAN_FAKE_DEEPSEEK_KEY"}),
            serde_json::json!({"oauth": "openai-codex"}),
        ] {
            let settings = settings(serde_json::json!({
                "deepseek": openai_compatible(credential.clone()),
                "anthropic": {"kind": "anthropic", "protocol": "anthropicMessages",
                    "credential": {"env": "NYAN_FAKE_DEEPSEEK_KEY"}}
            }));
            assert_eq!(
                missing_credential(&settings, None, &fixture.store()).as_deref(),
                Some("deepseek"),
                "{credential}"
            );
            assert_eq!(
                missing_credential(&settings, Some("claude"), &fixture.store()).as_deref(),
                Some("anthropic")
            );
        }
    }

    #[test]
    fn env_none_and_a_signed_in_oauth_account_are_not_missing() {
        let fixture = Fixture::new();
        let _key = EnvVarGuard::set("NYAN_FAKE_DEEPSEEK_KEY", "sk-fake-deepseek-env-0002");
        let store = fixture.store();
        let mut auth = crate::utils::provider_auth::store::ProviderAuth::default();
        auth.set_account(
            "me",
            crate::utils::provider_auth::store::OAuthCredential {
                access: "fake-access-token-0003".into(),
                refresh: "fake-refresh-token-0004".into(),
                // Expired: refreshing it is the request's job, not a missing key.
                expires_at_ms: 0,
                account_id: None,
                metadata: Default::default(),
            },
        );
        store.lock("openai-codex").unwrap().save(&auth).unwrap();
        for credential in [
            serde_json::json!({"env": "NYAN_FAKE_DEEPSEEK_KEY"}),
            serde_json::json!({"none": true}),
            serde_json::json!({"oauth": "openai-codex"}),
        ] {
            let settings = settings(serde_json::json!({
                "deepseek": openai_compatible(credential.clone())
            }));
            assert_eq!(
                missing_credential(&settings, None, &store),
                None,
                "{credential}"
            );
        }
    }

    #[test]
    fn no_resolvable_profile_reports_no_missing_credential() {
        let fixture = Fixture::new();
        assert_eq!(
            missing_credential(&SettingsJson::default(), None, &fixture.store()),
            None
        );
    }

    #[test]
    fn redaction_covers_every_configured_provider_and_spares_short_values() {
        let fixture = Fixture::new();
        let file = fixture.key_file("sk-fake-deepseek-file-0005");
        let _env = EnvVarGuard::set("NYAN_FAKE_GEMINI_KEY", "AIzaFakeGeminiKey0006");
        let _short = EnvVarGuard::set("NYAN_FAKE_SHORT_KEY", "abc");
        let _ambient = EnvVarGuard::set("ANTHROPIC_API_KEY", "sk-ant-fake-ambient-0007");
        crate::utils::settings::settings_cache::install_test_settings(settings(
            serde_json::json!({
                "deepseek": openai_compatible(serde_json::json!({"file": file})),
                "gemini": {"kind": "gemini", "protocol": "gemini",
                    "credential": {"env": "NYAN_FAKE_GEMINI_KEY"}},
                "local": {"kind": "vllm", "protocol": "openaiChat",
                    "baseUrl": "http://127.0.0.1:8000",
                    "credential": {"env": "NYAN_FAKE_SHORT_KEY"}}
            }),
        ));

        let redacted = redact_credentials(
            "401 for sk-fake-deepseek-file-0005\nkey=AIzaFakeGeminiKey0006 \
             ambient sk-ant-fake-ambient-0007 via ANTHROPIC_API_KEY; abc stays",
        );
        assert_eq!(
            redacted,
            "401 for <redacted> key=<redacted> ambient <redacted> via <redacted>; abc stays"
        );
    }

    #[test]
    fn a_secret_containing_another_is_redacted_whole() {
        let _fixture = Fixture::new();
        let _outer = EnvVarGuard::set("NYAN_FAKE_OUTER_KEY", "sk-fake-inner-0008-outer");
        let _inner = EnvVarGuard::set("NYAN_FAKE_INNER_KEY", "sk-fake-inner-0008");
        crate::utils::settings::settings_cache::install_test_settings(settings(
            serde_json::json!({
                "deepseek": openai_compatible(serde_json::json!({"env": "NYAN_FAKE_INNER_KEY"})),
                "anthropic": {"kind": "anthropic", "protocol": "anthropicMessages",
                    "credential": {"env": "NYAN_FAKE_OUTER_KEY"}}
            }),
        ));
        assert_eq!(
            redact_credentials("got sk-fake-inner-0008-outer"),
            "got <redacted>"
        );
    }
}
