//! Maps to: CC `utils/mtls.ts`: the client certificate and the TLS options
//! every agent carries.
//!
//! Node agents take `cert`, `key`, `passphrase` and `ca` separately. reqwest
//! has no agent apart from its client, so [`get_mtls_agent`] holds those
//! options parsed and [`MTLSAgent::apply`] sets them on a client builder.
//!
//! Cometix-specific deviations:
//! - An encrypted key cannot be used: rustls reads only plain PKCS#8, PKCS#1
//!   and SEC1 keys, and Android's native-tls only plain PKCS#8. It is
//!   logged, and no client certificate is sent;
//!   `CLAUDE_CODE_CLIENT_KEY_PASSPHRASE` has no effect.
//! - A certificate without a key, or a key without a certificate, is logged
//!   and not used. Node accepts either and may fail the handshake later.
//!
//! CC's `getWebSocketTLSOptions` (`:100-112`) waits for the WebSocket
//! transports. `getTLSFetchOptions` (`:117-152`) is the direct client
//! [`crate::utils::proxy::get_proxy_fetch_options`] builds.

use std::sync::{Arc, LazyLock, RwLock};

use crate::utils::debug::{DebugLogLevel, log_for_debugging, log_for_debugging_with_level};
use crate::utils::process_env::JsTruthy;

/// Maps to: CC `utils/mtls.ts:10-14` `MTLSConfig`: the file contents, not
/// the paths.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MTLSConfig {
    pub cert: Option<String>,
    pub key: Option<String>,
    pub passphrase: Option<String>,
}

/// The options a Node HTTPS agent built from [`MTLSConfig`] and the CA
/// certificates would carry. `keepAlive: true` is reqwest's default.
#[derive(Clone)]
pub struct MTLSAgent {
    identity: Option<reqwest::Identity>,
    ca: Option<crate::utils::ca_certs::CaCertificates>,
}

impl MTLSAgent {
    /// Set this agent's options on `builder`: the CA certificates are added
    /// to the platform's, and the client certificate is presented to servers
    /// and HTTPS proxies alike.
    pub fn apply(&self, builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
        let builder = match &self.ca {
            Some(ca) => builder.tls_certs_merge(ca.iter().cloned()),
            None => builder,
        };
        match &self.identity {
            Some(identity) => builder.identity(identity.clone()),
            None => builder,
        }
    }
}

// CC's lodash memoize slots. The outer `None` is "not computed yet". The
// agent's computation takes the config's slot, so locks go agent, then config.
static MTLS_CONFIG: LazyLock<RwLock<Option<Option<Arc<MTLSConfig>>>>> =
    LazyLock::new(|| RwLock::new(None));
static MTLS_AGENT: LazyLock<RwLock<Option<Option<Arc<MTLSAgent>>>>> =
    LazyLock::new(|| RwLock::new(None));

/// Computed under the write lock, so a clear cannot be overtaken by a
/// computation that read the environment before it.
fn memoized<T: Clone>(slot: &RwLock<Option<T>>, compute: impl FnOnce() -> T) -> T {
    if let Some(cached) = slot
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
    {
        return cached;
    }
    let mut slot = slot.write().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(cached) = slot.clone() {
        return cached;
    }
    let value = compute();
    *slot = Some(value.clone());
    value
}

/// Maps to: CC `utils/mtls.ts:23-73` `getMTLSConfig`, memoized. Each
/// variable is tested for JS truthiness; a file that cannot be read is logged
/// and left out.
pub fn get_mtls_config() -> Option<Arc<MTLSConfig>> {
    memoized(&MTLS_CONFIG, load_mtls_config)
}

fn read_file(path: &str) -> std::io::Result<String> {
    crate::utils::fs_operations::get_fs_implementation()
        .read_file_sync(
            std::path::Path::new(path),
            crate::utils::fs_operations::BufferEncoding::Utf8,
        )
        .map(|text| text.to_string_lossy())
}

fn load_mtls_config() -> Option<Arc<MTLSConfig>> {
    let env = crate::utils::process_env::snapshot();
    let mut config = MTLSConfig::default();

    if let Some(path) = env.var("CLAUDE_CODE_CLIENT_CERT").truthy() {
        match read_file(path) {
            Ok(cert) => {
                config.cert = Some(cert);
                log_for_debugging("mTLS: Loaded client certificate from CLAUDE_CODE_CLIENT_CERT");
            }
            Err(error) => log_for_debugging_with_level(
                &format!("mTLS: Failed to load client certificate: {error}"),
                DebugLogLevel::Error,
            ),
        }
    }

    if let Some(path) = env.var("CLAUDE_CODE_CLIENT_KEY").truthy() {
        match read_file(path) {
            Ok(key) => {
                config.key = Some(key);
                log_for_debugging("mTLS: Loaded client key from CLAUDE_CODE_CLIENT_KEY");
            }
            Err(error) => log_for_debugging_with_level(
                &format!("mTLS: Failed to load client key: {error}"),
                DebugLogLevel::Error,
            ),
        }
    }

    if let Some(passphrase) = env.var("CLAUDE_CODE_CLIENT_KEY_PASSPHRASE").truthy() {
        config.passphrase = Some(passphrase.to_owned());
        log_for_debugging("mTLS: Using client key passphrase");
    }

    (config != MTLSConfig::default()).then(|| Arc::new(config))
}

/// Maps to: CC `utils/mtls.ts:78-95` `getMTLSAgent`, memoized: `None` when
/// there is neither a client certificate nor a CA certificate to add.
pub fn get_mtls_agent() -> Option<Arc<MTLSAgent>> {
    memoized(&MTLS_AGENT, || {
        let mtls_config = get_mtls_config();
        let ca_certs = crate::utils::ca_certs::get_ca_certificates();
        if mtls_config.is_none() && ca_certs.is_none() {
            return None;
        }
        log_for_debugging("mTLS: Creating HTTPS agent with custom certificates");
        Some(Arc::new(MTLSAgent {
            identity: mtls_config.as_deref().and_then(build_identity),
            ca: ca_certs,
        }))
    })
}

/// Node's `cert` + `key` + `passphrase` as one reqwest identity. A pair that
/// does not match parses here and fails the client build instead, which then
/// surfaces as a request error, as Node's does.
fn build_identity(config: &MTLSConfig) -> Option<reqwest::Identity> {
    let (Some(cert), Some(key)) = (&config.cert, &config.key) else {
        if config.cert.is_some() || config.key.is_some() {
            log_for_debugging_with_level(
                "mTLS: CLAUDE_CODE_CLIENT_CERT and CLAUDE_CODE_CLIENT_KEY must both be readable; no client certificate is sent",
                DebugLogLevel::Error,
            );
        }
        return None;
    };
    if key.contains("-----BEGIN ENCRYPTED PRIVATE KEY-----") || key.contains("Proc-Type: 4,ENCRYPTED")
    {
        log_for_debugging_with_level(
            "mTLS: Encrypted client keys are not supported; no client certificate is sent",
            DebugLogLevel::Error,
        );
        return None;
    }
    #[cfg(not(target_os = "android"))]
    let identity = reqwest::Identity::from_pem(format!("{cert}\n{key}").as_bytes());
    #[cfg(target_os = "android")]
    let identity = reqwest::Identity::from_pkcs8_pem(cert.as_bytes(), key.as_bytes());
    identity
        .inspect_err(|error| {
            log_for_debugging_with_level(
                &format!("mTLS: Failed to load client certificate and key: {error}"),
                DebugLogLevel::Error,
            );
        })
        .ok()
}

/// Maps to: CC `utils/mtls.ts:157-161` `clearMTLSCache`.
pub fn clear_mtls_cache() {
    let mut agent = MTLS_AGENT
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *MTLS_CONFIG
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    *agent = None;
    drop(agent);
    log_for_debugging("Cleared mTLS configuration cache");
}

/// Maps to: CC `utils/mtls.ts:166-179` `configureGlobalMTLS`: loads the
/// configuration at startup. Node itself appends `NODE_EXTRA_CA_CERTS`; here
/// [`crate::utils::ca_certs`] does.
pub fn configure_global_mtls() {
    if get_mtls_config().is_none() {
        return;
    }
    if crate::utils::process_env::var("NODE_EXTRA_CA_CERTS")
        .truthy()
        .is_some()
    {
        log_for_debugging("NODE_EXTRA_CA_CERTS detected - appended to the platform CAs");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    fn fixture(name: &str) -> String {
        format!("{}/tests/fixtures/tls/{name}", env!("CARGO_MANIFEST_DIR"))
    }

    fn reset() {
        crate::utils::ca_certs::clear_ca_certs_cache();
        clear_mtls_cache();
    }

    #[test]
    fn config_holds_what_was_read_and_is_none_when_nothing_was() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _cert = EnvVarGuard::set("CLAUDE_CODE_CLIENT_CERT", fixture("client.pem"));
        let _key = EnvVarGuard::set("CLAUDE_CODE_CLIENT_KEY", fixture("missing.key"));
        let _passphrase = EnvVarGuard::set("CLAUDE_CODE_CLIENT_KEY_PASSPHRASE", "secret");
        reset();
        let config = get_mtls_config().unwrap();
        assert!(config.cert.as_deref().unwrap().contains("BEGIN CERTIFICATE"));
        // The unreadable key is logged and left out.
        assert_eq!(config.key, None);
        assert_eq!(config.passphrase.as_deref(), Some("secret"));

        let _cert = EnvVarGuard::set("CLAUDE_CODE_CLIENT_CERT", "");
        let _key = EnvVarGuard::unset("CLAUDE_CODE_CLIENT_KEY");
        let _passphrase = EnvVarGuard::unset("CLAUDE_CODE_CLIENT_KEY_PASSPHRASE");
        reset();
        assert!(get_mtls_config().is_none());
        reset();
    }

    #[test]
    fn agent_carries_the_identity_only_for_a_plain_complete_pair() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        crate::utils::tls_provider::install_crypto_provider();
        let _ca = EnvVarGuard::unset("NODE_EXTRA_CA_CERTS");
        let _cert = EnvVarGuard::set("CLAUDE_CODE_CLIENT_CERT", fixture("client.pem"));
        let _key = EnvVarGuard::set("CLAUDE_CODE_CLIENT_KEY", fixture("client.key"));
        reset();
        let agent = get_mtls_agent().unwrap();
        assert!(agent.identity.is_some());
        assert!(agent.apply(reqwest::Client::builder().no_proxy()).build().is_ok());

        let _key = EnvVarGuard::set("CLAUDE_CODE_CLIENT_KEY", fixture("client-encrypted.key"));
        reset();
        assert!(get_mtls_agent().unwrap().identity.is_none());

        let _key = EnvVarGuard::unset("CLAUDE_CODE_CLIENT_KEY");
        reset();
        assert!(get_mtls_agent().unwrap().identity.is_none());

        // A key that does not match the certificate fails the build.
        let _key = EnvVarGuard::set("CLAUDE_CODE_CLIENT_KEY", fixture("other.key"));
        reset();
        let agent = get_mtls_agent().unwrap();
        assert!(agent.apply(reqwest::Client::builder().no_proxy()).build().is_err());
        reset();
    }

    #[test]
    fn agent_exists_for_ca_certificates_alone() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ca = EnvVarGuard::set("NODE_EXTRA_CA_CERTS", fixture("ca.pem"));
        let _cert = EnvVarGuard::unset("CLAUDE_CODE_CLIENT_CERT");
        let _key = EnvVarGuard::unset("CLAUDE_CODE_CLIENT_KEY");
        let _passphrase = EnvVarGuard::unset("CLAUDE_CODE_CLIENT_KEY_PASSPHRASE");
        reset();
        crate::utils::tls_provider::install_crypto_provider();
        let agent = get_mtls_agent().unwrap();
        assert!(agent.identity.is_none());
        assert_eq!(agent.ca.as_ref().map(|ca| ca.len()), Some(1));
        // The CA reaches the platform verifier: the client builds.
        assert!(agent.apply(reqwest::Client::builder().no_proxy()).build().is_ok());

        let _ca = EnvVarGuard::unset("NODE_EXTRA_CA_CERTS");
        reset();
        assert!(get_mtls_agent().is_none());
        reset();
    }
}
