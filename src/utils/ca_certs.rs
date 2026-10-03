//! Maps to: CC `utils/caCerts.ts`: the CA certificates every TLS agent trusts.
//!
//! Setting `ca` on a Node agent replaces the runtime's store, so CC's
//! `getCACertificates` returns a whole store: the bundled Mozilla roots (or
//! the system roots under `--use-system-ca`/`--use-openssl-ca`) plus the
//! `NODE_EXTRA_CA_CERTS` file. reqwest's `tls_certs_merge` appends to a base it
//! keeps, the platform verifier, which already trusts the system store. So
//! this returns only the extra certificates.
//!
//! Cometix-specific deviation: the base store is always the platform's, as in
//! CC under `--use-system-ca`. CC's default base is its bundled Mozilla roots.
//!
//! Reads only `NODE_EXTRA_CA_CERTS`; `ca_certs_config.rs` populates it from
//! settings at startup.

use std::sync::{Arc, LazyLock, RwLock};

use crate::utils::debug::{DebugLogLevel, log_for_debugging, log_for_debugging_with_level};
use crate::utils::process_env::JsTruthy;

/// The extra certificates, parsed once for every client.
pub type CaCertificates = Arc<Vec<reqwest::Certificate>>;

/// CC's lodash memoize slot. The outer `None` is "not computed yet".
static CA_CERTIFICATES: LazyLock<RwLock<Option<Option<CaCertificates>>>> =
    LazyLock::new(|| RwLock::new(None));

/// Maps to: CC `utils/caCerts.ts:28-105` `getCACertificates`, memoized.
///
/// `None` leaves the runtime's trust store alone: `NODE_EXTRA_CA_CERTS` is
/// unset, or its file could not be read. `--use-system-ca` alone needs
/// nothing, since the base is already the system store.
///
/// A file that holds no certificate, or one a TLS client rejects, is logged
/// and skipped. reqwest would otherwise fail every client build on it; Node
/// would skip a bad `NODE_EXTRA_CA_CERTS` file with a warning.
///
/// Computed under the write lock, so a [`clear_ca_certs_cache`] cannot be
/// overtaken by a computation that read the environment before it.
pub fn get_ca_certificates() -> Option<CaCertificates> {
    if let Some(cached) = CA_CERTIFICATES
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
    {
        return cached;
    }
    let mut slot = CA_CERTIFICATES
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(cached) = slot.clone() {
        return cached;
    }
    let certificates = load_ca_certificates();
    *slot = Some(certificates.clone());
    certificates
}

fn load_ca_certificates() -> Option<CaCertificates> {
    use crate::utils::env_utils::has_node_option;
    let use_system_ca = has_node_option("--use-system-ca") || has_node_option("--use-openssl-ca");
    let extra_certs_path = crate::utils::process_env::var("NODE_EXTRA_CA_CERTS");
    log_for_debugging(&format!(
        "CA certs: useSystemCA={use_system_ca}, extraCertsPath={}",
        extra_certs_path.as_deref().unwrap_or("undefined"),
    ));
    let extra_certs_path = extra_certs_path.truthy()?;

    let extra_cert = match crate::utils::fs_operations::get_fs_implementation().read_file_sync(
        std::path::Path::new(&extra_certs_path),
        crate::utils::fs_operations::BufferEncoding::Utf8,
    ) {
        Ok(text) => text.to_string_lossy(),
        Err(error) => {
            log_for_debugging_with_level(
                &format!(
                    "CA certs: Failed to read NODE_EXTRA_CA_CERTS file ({extra_certs_path}): {error}"
                ),
                DebugLogLevel::Error,
            );
            return None;
        }
    };
    match reqwest::Certificate::from_pem_bundle(extra_cert.as_bytes()) {
        Ok(certificates) if !certificates.is_empty() => {
            // rustls only decodes the PEM here and checks the certificates
            // when a client is built, so build one now.
            crate::utils::tls_provider::install_crypto_provider();
            if let Err(error) = reqwest::Client::builder()
                .no_proxy()
                .tls_certs_merge(certificates.iter().cloned())
                .build()
            {
                log_for_debugging_with_level(
                    &format!(
                        "CA certs: Invalid certificate in NODE_EXTRA_CA_CERTS file ({extra_certs_path}): {error:?}"
                    ),
                    DebugLogLevel::Error,
                );
                return None;
            }
            log_for_debugging(&format!(
                "CA certs: Appended extra certificates from NODE_EXTRA_CA_CERTS ({extra_certs_path})"
            ));
            Some(Arc::new(certificates))
        }
        Ok(_) => {
            log_for_debugging_with_level(
                &format!("CA certs: No certificate in NODE_EXTRA_CA_CERTS file ({extra_certs_path})"),
                DebugLogLevel::Error,
            );
            None
        }
        Err(error) => {
            log_for_debugging_with_level(
                &format!(
                    "CA certs: Failed to parse NODE_EXTRA_CA_CERTS file ({extra_certs_path}): {error}"
                ),
                DebugLogLevel::Error,
            );
            None
        }
    }
}

/// Maps to: CC `utils/caCerts.ts:112-115` `clearCACertsCache`.
pub fn clear_ca_certs_cache() {
    *CA_CERTIFICATES
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    log_for_debugging("Cleared CA certificates cache");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    fn fixture(name: &str) -> String {
        format!("{}/tests/fixtures/tls/{name}", env!("CARGO_MANIFEST_DIR"))
    }

    #[test]
    fn extra_certificates_come_from_node_extra_ca_certs_and_are_memoized() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _options = EnvVarGuard::unset("NODE_OPTIONS");
        let _path = EnvVarGuard::set("NODE_EXTRA_CA_CERTS", fixture("ca.pem"));
        clear_ca_certs_cache();
        assert_eq!(get_ca_certificates().map(|certs| certs.len()), Some(1));

        // Memoized until cleared, as `managedEnv.ts:193` relies on.
        crate::utils::process_env::remove("NODE_EXTRA_CA_CERTS");
        assert!(get_ca_certificates().is_some());
        clear_ca_certs_cache();
        assert!(get_ca_certificates().is_none());
    }

    #[test]
    fn unreadable_or_certificate_free_files_leave_the_store_alone() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _missing = EnvVarGuard::set("NODE_EXTRA_CA_CERTS", fixture("missing.pem"));
        clear_ca_certs_cache();
        assert!(get_ca_certificates().is_none());

        // A private key is PEM but holds no certificate.
        let _key = EnvVarGuard::set("NODE_EXTRA_CA_CERTS", fixture("client.key"));
        clear_ca_certs_cache();
        assert!(get_ca_certificates().is_none());

        // A well-formed PEM block whose DER is not a certificate.
        let bogus = std::env::temp_dir().join(format!("cometix-bogus-ca-{}.pem", uuid::Uuid::new_v4()));
        std::fs::write(
            &bogus,
            "-----BEGIN CERTIFICATE-----\nAAECAwQFBgcICQ==\n-----END CERTIFICATE-----\n",
        )
        .unwrap();
        let _bogus = EnvVarGuard::set("NODE_EXTRA_CA_CERTS", &bogus);
        clear_ca_certs_cache();
        assert!(get_ca_certificates().is_none());
        let _ = std::fs::remove_file(&bogus);

        // `!extraCertsPath`: an empty value is unset.
        let _empty = EnvVarGuard::set("NODE_EXTRA_CA_CERTS", "");
        clear_ca_certs_cache();
        assert!(get_ca_certificates().is_none());
        clear_ca_certs_cache();
    }
}
