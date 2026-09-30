//! Process-wide TLS runtime behavior: the crypto provider, and what Node's
//! TLS layer does on its own.
//!
//! Desktop builds use `rustls-no-provider`, so the application owns the
//! provider choice. Android uses `native-tls` and has no rustls provider to
//! install. Keeping this at one owner gives direct library and test entry
//! points the same initialization contract as the normal launcher.

use std::sync::atomic::{AtomicBool, Ordering};

/// Install the selected desktop rustls crypto provider.
///
/// The process-wide slot is naturally idempotent: subsequent calls report
/// that a provider is already installed, which is the expected result for
/// direct library/test entry points and is intentionally ignored here.
#[inline]
pub fn install_crypto_provider() {
    #[cfg(not(target_os = "android"))]
    {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
}

static WARNED_ON_ALLOW_UNAUTHORIZED: AtomicBool = AtomicBool::new(false);

/// Node's `getAllowUnauthorized()`, which `tls.connect` consults: exactly
/// `NODE_TLS_REJECT_UNAUTHORIZED=0` turns certificate verification off. CC
/// never reads the variable; its runtime does, for every TLS connection.
/// Here each client reads it when built, and the cached clients are rebuilt
/// whenever the settings environment is applied.
///
/// Node emits a process warning the first time. CC's `warningHandler.ts`
/// (`:62-111`) removes Node's printer, so users see nothing, and the warning
/// reaches the debug log only when `CLAUDE_DEBUG` is truthy. That handler is
/// not ported (MODULE_MAP: missing); its visible effect for this one warning
/// is written here, and its `tengu_node_warning` event is not logged.
pub fn get_allow_unauthorized() -> bool {
    let env = crate::utils::process_env::snapshot();
    let allow_unauthorized = env.var("NODE_TLS_REJECT_UNAUTHORIZED") == Some("0");
    if allow_unauthorized
        && !WARNED_ON_ALLOW_UNAUTHORIZED.swap(true, Ordering::Relaxed)
        && crate::utils::env_utils::is_env_truthy(env.var("CLAUDE_DEBUG"))
    {
        crate::utils::debug::log_for_debugging_with_level(
            "[Warning] Warning: Setting the NODE_TLS_REJECT_UNAUTHORIZED environment variable to '0' makes TLS connections and HTTPS requests insecure by disabling certificate verification.",
            crate::utils::debug::DebugLogLevel::Warn,
        );
    }
    allow_unauthorized
}
