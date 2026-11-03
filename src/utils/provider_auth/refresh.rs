//! Serialized token refresh for one provider account.
//!
//! This is not a token broker: it knows no endpoint and no token format. A
//! provider module passes the call that exchanges its own refresh token. The
//! helper only guarantees the ordering that keeps rotating refresh tokens
//! alive: take the provider lock, re-read the file, and refresh only if the
//! stored token is still stale. A second process or task that waited on the
//! lock therefore finds the winner's token and does not spend the (possibly
//! single-use) refresh token a second time.

use std::future::Future;

use super::store::{AuthStore, AuthStoreError, OAuthCredential};

/// Why a token could not be produced.
#[derive(Debug)]
pub enum TokenError {
    /// No signed-in account matches. Carries the provider and, if the caller
    /// named one, the account.
    NotSignedIn {
        provider: String,
        account: Option<String>,
    },
    /// The provider rejected the refresh token; only a new sign-in recovers.
    SignedOut {
        provider: String,
    },
    /// The provider's token endpoint failed in a way that may be transient.
    Refresh {
        provider: String,
        reason: String,
    },
    Store(AuthStoreError),
}

impl std::fmt::Display for TokenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotSignedIn { provider, account } => match account {
                Some(account) => write!(f, "{provider} account {account:?} is not signed in"),
                None => write!(
                    f,
                    "{provider} is not signed in (or has several accounts and no default)"
                ),
            },
            Self::SignedOut { provider } => {
                write!(
                    f,
                    "{provider} sign-in expired or was revoked; sign in again"
                )
            }
            Self::Refresh { provider, reason } => {
                write!(f, "{provider} token refresh failed: {reason}")
            }
            Self::Store(error) => std::fmt::Display::fmt(error, f),
        }
    }
}

impl std::error::Error for TokenError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Store(error) => Some(error),
            _ => None,
        }
    }
}

impl From<AuthStoreError> for TokenError {
    fn from(error: AuthStoreError) -> Self {
        Self::Store(error)
    }
}

/// Returns a usable credential for `account` (or the default account),
/// refreshing it under the provider lock when it expires within `margin_ms`.
///
/// `refresh` receives the stored credential and returns the replacement. It
/// runs while the lock is held, so one provider's refreshes never overlap;
/// other providers are unaffected. The new credential is persisted before it is
/// returned.
pub async fn fresh_credential<F, Fut>(
    store: &AuthStore,
    provider: &str,
    account: Option<&str>,
    now_ms: i64,
    margin_ms: i64,
    refresh: F,
) -> Result<OAuthCredential, TokenError>
where
    F: FnOnce(OAuthCredential) -> Fut,
    Fut: Future<Output = Result<OAuthCredential, TokenError>>,
{
    let not_signed_in = || TokenError::NotSignedIn {
        provider: provider.to_string(),
        account: account.map(str::to_string),
    };

    // Fast path: no lock, no write. Files are replaced atomically, so this read
    // sees a whole file.
    let current = store.load(provider)?.ok_or_else(not_signed_in)?;
    let (_, stored) = current.account(account).ok_or_else(not_signed_in)?;
    if !stored.is_expired(now_ms, margin_ms) {
        return Ok(stored.clone());
    }

    let lock = store.lock_async(provider).await?;
    let mut auth = lock.load()?.ok_or_else(not_signed_in)?;
    let (name, stored) = auth.account(account).ok_or_else(not_signed_in)?;
    let name = name.to_string();
    if !stored.is_expired(now_ms, margin_ms) {
        // Someone else refreshed while this call waited for the lock.
        return Ok(stored.clone());
    }
    let refreshed = refresh(stored.clone()).await?;
    auth.set_account(&name, refreshed.clone());
    lock.save(&auth)?;
    Ok(refreshed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::provider_auth::store::ProviderAuth;
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Scratch(std::path::PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn store(name: &str) -> (Scratch, AuthStore) {
        let root = crate::utils::node_os::tmpdir().join(format!(
            "nyan-pr3-refresh-{name}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        (Scratch(root.clone()), AuthStore::at(root.join("auth")))
    }

    fn credential(access: &str, refresh: &str, expires_at_ms: i64) -> OAuthCredential {
        OAuthCredential {
            access: access.to_string(),
            refresh: refresh.to_string(),
            expires_at_ms,
            account_id: None,
            metadata: BTreeMap::new(),
        }
    }

    fn seed(store: &AuthStore, provider: &str, account: &str, credential: OAuthCredential) {
        let mut auth = ProviderAuth::default();
        auth.set_account(account, credential);
        store.lock(provider).unwrap().save(&auth).unwrap();
    }

    #[tokio::test]
    async fn valid_token_is_returned_without_refreshing() {
        let (_scratch, store) = store("valid");
        seed(&store, "opencode", "a", credential("live", "r1", 10_000));
        let token = fresh_credential(&store, "opencode", None, 1_000, 500, |_| async {
            panic!("must not refresh a valid token")
        })
        .await
        .unwrap();
        assert_eq!(token.access, "live");
    }

    #[tokio::test]
    async fn expired_token_is_refreshed_and_persisted() {
        let (_scratch, store) = store("expired");
        seed(&store, "opencode", "a", credential("old", "r1", 1_000));
        let token = fresh_credential(&store, "opencode", Some("a"), 2_000, 0, |old| async move {
            assert_eq!(old.refresh, "r1");
            Ok(credential("new", "r2", 99_000))
        })
        .await
        .unwrap();
        assert_eq!(token.access, "new");
        let stored = store.load("opencode").unwrap().unwrap();
        assert_eq!(stored.account(Some("a")).unwrap().1.refresh, "r2");
    }

    #[tokio::test]
    async fn safety_margin_counts_as_expired() {
        let (_scratch, store) = store("margin");
        seed(&store, "opencode", "a", credential("old", "r1", 10_000));
        let token = fresh_credential(&store, "opencode", None, 9_500, 1_000, |_| async {
            Ok(credential("new", "r2", 99_000))
        })
        .await
        .unwrap();
        assert_eq!(token.access, "new");
    }

    #[tokio::test]
    async fn concurrent_callers_spend_the_refresh_token_once() {
        let (_scratch, store) = store("race");
        seed(&store, "opencode", "a", credential("old", "r1", 1_000));
        let calls = Arc::new(AtomicUsize::new(0));
        let mut tasks = Vec::new();
        for _ in 0..8 {
            let store = store.clone();
            let calls = Arc::clone(&calls);
            tasks.push(tokio::spawn(async move {
                fresh_credential(&store, "opencode", None, 2_000, 0, |_| async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    Ok(credential("new", "r2", 99_000))
                })
                .await
                .unwrap()
                .access
            }));
        }
        for task in tasks {
            assert_eq!(task.await.unwrap(), "new");
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn providers_and_accounts_do_not_share_tokens() {
        let (_scratch, store) = store("iso");
        seed(
            &store,
            "opencode",
            "a",
            credential("oc-token", "oc-r", 1_000),
        );
        seed(
            &store,
            "openai-codex",
            "a",
            credential("cx-token", "cx-r", 99_000),
        );
        let refreshed = fresh_credential(&store, "opencode", None, 2_000, 0, |_| async {
            Ok(credential("oc-new", "oc-r2", 99_000))
        })
        .await
        .unwrap();
        assert_eq!(refreshed.access, "oc-new");
        let other = store.load("openai-codex").unwrap().unwrap();
        assert_eq!(other.account(None).unwrap().1.access, "cx-token");
        assert_eq!(other.account(None).unwrap().1.refresh, "cx-r");
    }

    #[tokio::test]
    async fn failed_refresh_leaves_the_stored_token_untouched() {
        let (_scratch, store) = store("fail");
        seed(&store, "opencode", "a", credential("old", "r1", 1_000));
        let error = fresh_credential(&store, "opencode", None, 2_000, 0, |_| async {
            Err(TokenError::SignedOut {
                provider: "opencode".into(),
            })
        })
        .await
        .unwrap_err();
        assert!(matches!(error, TokenError::SignedOut { .. }));
        let stored = store.load("opencode").unwrap().unwrap();
        assert_eq!(stored.account(None).unwrap().1.refresh, "r1");
    }

    #[tokio::test]
    async fn missing_or_ambiguous_account_is_not_signed_in() {
        let (_scratch, store) = store("missing");
        let never = |_| async { panic!("no refresh") };
        assert!(matches!(
            fresh_credential(&store, "opencode", None, 0, 0, never).await,
            Err(TokenError::NotSignedIn { .. })
        ));
        seed(&store, "opencode", "a", credential("t", "r", 10));
        assert!(matches!(
            fresh_credential(&store, "opencode", Some("b"), 0, 0, never).await,
            Err(TokenError::NotSignedIn { .. })
        ));
    }
}
