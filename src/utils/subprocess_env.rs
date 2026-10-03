//! Subprocess environment helpers.
//!
//! Maps to: CC `utils/subprocessEnv.ts`.
//!
//! Node children of CC inherit `process.env` — the live, runtime-mutated
//! object — as their base environment. Rust children must not inherit the
//! frozen real OS environment, or they would miss every dynamic update
//! (settings env, StructuredIO token refresh, session-id refresh); the base
//! is instead the latest `process_env` carrier snapshot, installed with
//! `env_clear()` + `envs(...)`.
//!
//! Layer order per spawn, matching CC's object spreads
//! (`subprocessEnv.ts:79-99`, `Shell.ts:317-328`):
//!
//! ```text
//! carrier snapshot → (upstream-proxy env) → GHA scrub → explicit overrides
//! ```
//!
//! Explicit per-command/per-server overrides are applied by the caller AFTER
//! these helpers, so they survive the scrub — `{ ...subprocessEnv(),
//! ...overrides }` semantics.

/// Maps to CC `GHA_SUBPROCESS_SCRUB` in `utils/subprocessEnv.ts`.
const GHA_SUBPROCESS_SCRUB: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_FOUNDRY_API_KEY",
    "ANTHROPIC_CUSTOM_HEADERS",
    "OTEL_EXPORTER_OTLP_HEADERS",
    "OTEL_EXPORTER_OTLP_LOGS_HEADERS",
    "OTEL_EXPORTER_OTLP_METRICS_HEADERS",
    "OTEL_EXPORTER_OTLP_TRACES_HEADERS",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "AWS_BEARER_TOKEN_BEDROCK",
    "GOOGLE_APPLICATION_CREDENTIALS",
    "AZURE_CLIENT_SECRET",
    "AZURE_CLIENT_CERTIFICATE_PATH",
    "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
    "ACTIONS_ID_TOKEN_REQUEST_URL",
    "ACTIONS_RUNTIME_TOKEN",
    "ACTIONS_RUNTIME_URL",
    "ALL_INPUTS",
    "OVERRIDE_GITHUB_TOKEN",
    "DEFAULT_WORKFLOW_TOKEN",
    "SSH_SIGNING_KEY",
];

/// Installs the effective `process.env` as the child's base environment.
///
/// CC shape ≙ Rust shape: Node `spawn` inheriting the runtime-mutated
/// `process.env` ≙ `env_clear()` + latest carrier snapshot. Every dynamic
/// update (settings env, StructuredIO, session id) is visible to children
/// spawned afterwards; a `delete process.env[key]` stops being inherited.
pub fn apply_process_env_std(command: &mut std::process::Command) {
    let env = crate::utils::process_env::snapshot();
    command.env_clear();
    command.envs(env.iter());
}

/// Tokio counterpart of [`apply_process_env_std`].
pub fn apply_process_env(command: &mut tokio::process::Command) {
    let env = crate::utils::process_env::snapshot();
    command.env_clear();
    command.envs(env.iter());
}

fn scrub_enabled(env: &crate::utils::process_env::EnvSnapshot) -> bool {
    crate::utils::env_utils::is_env_truthy(env.var("CLAUDE_CODE_SUBPROCESS_ENV_SCRUB"))
}

/// Maps to CC `subprocessEnv()` (`utils/subprocessEnv.ts:79-99`): the
/// effective process env, plus the upstream-proxy overlay, minus the GHA
/// secret scrub when `CLAUDE_CODE_SUBPROCESS_ENV_SCRUB` is truthy.
///
/// Per-command/per-server env overrides must be applied by the caller AFTER
/// this function so explicit entries survive the scrub, matching
/// `{ ...subprocessEnv(), ...overrides }`.
pub fn apply_subprocess_env(command: &mut tokio::process::Command) {
    let env = crate::utils::process_env::snapshot();
    command.env_clear();
    command.envs(env.iter());
    // CC merges `_getUpstreamProxyEnv?.() ?? {}` over process.env here
    // (`subprocessEnv.ts:84-91`) so curl/gh/python in CCR containers route
    // through the local relay. The upstreamproxy module is not ported
    // (MODULE_MAP: missing); when it lands, its env belongs exactly here —
    // after the base, before the scrub.
    if scrub_enabled(&env) {
        for key in GHA_SUBPROCESS_SCRUB {
            command.env_remove(key);
            command.env_remove(format!("INPUT_{key}"));
        }
    }
}

/// `std::process::Command` counterpart of [`apply_subprocess_env`], used by
/// `Shell.ts` and shell-snapshot owners.
pub fn apply_subprocess_env_std(command: &mut std::process::Command) {
    let env = crate::utils::process_env::snapshot();
    apply_subprocess_env_std_snapshot(command, &env);
}

/// Installs a caller-captured version, for operations whose gates and child
/// environment must derive from one coherent `process.env` snapshot.
pub(crate) fn apply_subprocess_env_std_snapshot(
    command: &mut std::process::Command,
    env: &crate::utils::process_env::EnvSnapshot,
) {
    command.env_clear();
    command.envs(env.iter());
    // Upstream-proxy seam — see [`apply_subprocess_env`].
    if scrub_enabled(env) {
        for key in GHA_SUBPROCESS_SCRUB {
            command.env_remove(key);
            command.env_remove(format!("INPUT_{key}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[test]
    fn scrub_list_matches_official_subprocess_sensitive_keys() {
        assert!(super::GHA_SUBPROCESS_SCRUB.contains(&"ANTHROPIC_API_KEY"));
        assert!(super::GHA_SUBPROCESS_SCRUB.contains(&"ACTIONS_ID_TOKEN_REQUEST_TOKEN"));
        assert!(super::GHA_SUBPROCESS_SCRUB.contains(&"GOOGLE_APPLICATION_CREDENTIALS"));
        assert!(!super::GHA_SUBPROCESS_SCRUB.contains(&"GITHUB_TOKEN"));
        assert!(!super::GHA_SUBPROCESS_SCRUB.contains(&"GH_TOKEN"));
    }

    fn child_env(command: &std::process::Command) -> Vec<(String, Option<String>)> {
        command
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.map(|value| value.to_string_lossy().into_owned()),
                )
            })
            .collect()
    }

    /// CC `utils/subprocessEnv.ts#subprocessEnv` returns the current
    /// `process.env` when secret scrubbing is disabled.
    #[test]
    fn base_env_matches_official_current_process_env_inheritance() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _dynamic = EnvVarGuard::set("COMETIX_SUBPROCESS_ENV_TEST_DYNAMIC", "fresh");
        let _scrub_off = EnvVarGuard::unset("CLAUDE_CODE_SUBPROCESS_ENV_SCRUB");

        let mut command = std::process::Command::new("true");
        super::apply_subprocess_env_std(&mut command);

        let env = child_env(&command);
        assert!(env.iter().any(|(key, value)| {
            key == "COMETIX_SUBPROCESS_ENV_TEST_DYNAMIC" && value.as_deref() == Some("fresh")
        }));
    }

    /// CC `utils/subprocessEnv.ts#subprocessEnv` clones the current environment,
    /// then deletes each sensitive key and its `INPUT_` counterpart.
    #[test]
    fn scrub_matches_official_secret_and_deleted_key_filtering() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _scrub = EnvVarGuard::set("CLAUDE_CODE_SUBPROCESS_ENV_SCRUB", "1");
        let _secret = EnvVarGuard::set("ANTHROPIC_API_KEY", "sk-test");
        let _input = EnvVarGuard::set("INPUT_ANTHROPIC_API_KEY", "sk-test");
        let _deleted = EnvVarGuard::unset("COMETIX_SUBPROCESS_ENV_TEST_DELETED");

        let mut command = std::process::Command::new("true");
        super::apply_subprocess_env_std(&mut command);

        let env = child_env(&command);
        // Sanity: the carrier base was installed.
        assert!(
            env.iter()
                .any(|(key, value)| key == "PATH" && value.is_some())
        );
        // After env_clear(), env_remove() drops the key from the explicit map
        // entirely — the child must carry no live value for scrubbed keys.
        assert!(
            !env.iter()
                .any(|(key, value)| key == "ANTHROPIC_API_KEY" && value.is_some())
        );
        assert!(
            !env.iter()
                .any(|(key, value)| key == "INPUT_ANTHROPIC_API_KEY" && value.is_some())
        );
        assert!(
            !env.iter()
                .any(|(key, value)| key == "COMETIX_SUBPROCESS_ENV_TEST_DELETED"
                    && value.is_some())
        );
    }

    /// CC child launch sites spread explicit overrides after `subprocessEnv()`,
    /// so caller values win over the scrubbed base.
    #[test]
    fn explicit_overrides_after_scrub_match_official_spread_precedence() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _scrub = EnvVarGuard::set("CLAUDE_CODE_SUBPROCESS_ENV_SCRUB", "true");

        let mut command = std::process::Command::new("true");
        super::apply_subprocess_env_std(&mut command);
        // Caller-applied override, `{ ...subprocessEnv(), ...overrides }`.
        command.env("ANTHROPIC_API_KEY", "explicit-wins");

        let env = child_env(&command);
        assert!(env.iter().any(|(key, value)| {
            key == "ANTHROPIC_API_KEY" && value.as_deref() == Some("explicit-wins")
        }));
    }
}
