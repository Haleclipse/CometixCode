//! Settings-sourced environment application.
//!
//! Maps to: CC `utils/managedEnv.ts` (strip filters + safe/full application
//! passes) and `utils/managedEnvConstants.ts` (constant sets).
//!
//! All writes go through the `process_env` carrier, one [`EnvUpdate`]
//! transaction per application pass, so environment readers see one complete
//! publication. The non-reentrant update lock covers only pure filtering,
//! staging, and publication—never I/O, callbacks, effects, awaits, or joins.
//! Filters that read `process.env` at filter time (`managedEnv.ts:27`, `:49`)
//! read the transaction's own snapshot, which includes earlier writes of the
//! same pass — matching CC, where each `Object.assign` is visible to the next
//! filter call. Later settings effects are linearized by the approved
//! StoreTurn and consume the committed snapshot/prebuilt resources.

use std::collections::HashSet;

use indexmap::IndexMap;
use std::sync::RwLock;

use crate::utils::process_env::{self, EnvSnapshot, EnvUpdate};
use crate::utils::settings::{
    SettingSource, get_initial_settings, get_settings_for_source, is_setting_source_enabled,
};

/// Maps to CC `utils/managedEnvConstants.ts#DANGEROUS_SHELL_SETTINGS`.
pub const DANGEROUS_SHELL_SETTINGS: &[&str] = &[
    "apiKeyHelper",
    "awsAuthRefresh",
    "awsCredentialExport",
    "gcpAuthRefresh",
    "otelHeadersHelper",
    "statusLine",
];

/// Maps to CC `utils/managedEnvConstants.ts#SAFE_ENV_VARS`.
pub const SAFE_ENV_VARS: &[&str] = &[
    "BASH_DEFAULT_TIMEOUT_MS",
    "BASH_MAX_OUTPUT_LENGTH",
    "BASH_MAX_TIMEOUT_MS",
    "CLAUDE_BASH_MAINTAIN_PROJECT_WORKING_DIR",
    "CLAUDE_CODE_API_KEY_HELPER_TTL_MS",
    "CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS",
    "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
    "CLAUDE_CODE_DISABLE_TERMINAL_TITLE",
    "CLAUDE_CODE_ENABLE_TELEMETRY",
    "CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS",
    "CLAUDE_CODE_IDE_SKIP_AUTO_INSTALL",
    "CLAUDE_CODE_MAX_OUTPUT_TOKENS",
    "DISABLE_AUTOUPDATER",
    "DISABLE_BUG_COMMAND",
    "DISABLE_COST_WARNINGS",
    "DISABLE_ERROR_REPORTING",
    "DISABLE_FEEDBACK_COMMAND",
    "DISABLE_TELEMETRY",
    "ENABLE_TOOL_SEARCH",
    "MAX_MCP_OUTPUT_TOKENS",
    "MAX_THINKING_TOKENS",
    "MCP_TIMEOUT",
    "MCP_TOOL_TIMEOUT",
    "OTEL_EXPORTER_OTLP_HEADERS",
    "OTEL_EXPORTER_OTLP_LOGS_HEADERS",
    "OTEL_EXPORTER_OTLP_LOGS_PROTOCOL",
    "OTEL_EXPORTER_OTLP_METRICS_CLIENT_CERTIFICATE",
    "OTEL_EXPORTER_OTLP_METRICS_CLIENT_KEY",
    "OTEL_EXPORTER_OTLP_METRICS_HEADERS",
    "OTEL_EXPORTER_OTLP_METRICS_PROTOCOL",
    "OTEL_EXPORTER_OTLP_PROTOCOL",
    "OTEL_EXPORTER_OTLP_TRACES_HEADERS",
    "OTEL_LOG_TOOL_DETAILS",
    "OTEL_LOG_USER_PROMPTS",
    "OTEL_LOGS_EXPORT_INTERVAL",
    "OTEL_LOGS_EXPORTER",
    "OTEL_METRIC_EXPORT_INTERVAL",
    "OTEL_METRICS_EXPORTER",
    "OTEL_METRICS_INCLUDE_ACCOUNT_UUID",
    "OTEL_METRICS_INCLUDE_SESSION_ID",
    "OTEL_METRICS_INCLUDE_VERSION",
    "OTEL_RESOURCE_ATTRIBUTES",
    "USE_BUILTIN_RIPGREP",
];

/// Maps to CC `utils/managedEnvConstants.ts#SAFE_ENV_VARS.has(...)`.
pub fn is_safe_managed_env_var(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    SAFE_ENV_VARS.contains(&upper.as_str())
}

/// Maps to CC `managedEnv.ts:69` `ccdSpawnEnvKeys`.
///
/// Outer `Option`: not yet captured (CC `undefined`). Inner `Option`: captured
/// but the entrypoint was not claude-desktop (CC `null`). `RwLock` instead of
/// `OnceLock` only so tests can reset the latch; production captures once.
static CCD_SPAWN_ENV_KEYS: RwLock<Option<Option<HashSet<String>>>> = RwLock::new(None);

/// Maps to CC `managedEnv.ts:126-131` — capture the spawn-env key snapshot on
/// the first `applySafeConfigEnvironmentVariables()` call, before any
/// settings.env is applied.
fn capture_ccd_spawn_env_keys(env: &EnvSnapshot) {
    let mut slot = CCD_SPAWN_ENV_KEYS
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if slot.is_none() {
        *slot = Some(
            (env.var("CLAUDE_CODE_ENTRYPOINT") == Some("claude-desktop")).then(|| {
                env.keys()
                    .filter_map(|key| key.to_str().map(str::to_owned))
                    .collect()
            }),
        );
    }
}

fn ccd_spawn_env_keys() -> Option<HashSet<String>> {
    CCD_SPAWN_ENV_KEYS
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
        .flatten()
}

#[cfg(test)]
pub(crate) fn reset_ccd_spawn_env_keys_for_tests() {
    *CCD_SPAWN_ENV_KEYS
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
}

/// Maps to CC `managedEnv.ts:85-91` `filterSettingsEnv` — the composed strip
/// filters applied to every settings-sourced env object.
///
/// @nyan: CC's SSH-tunnel and host-managed-provider filters are gone with the
/// provider environment itself.
fn filter_settings_env(env: Option<&IndexMap<String, String>>) -> Vec<(String, String)> {
    // CC `withoutCcdSpawnEnvKeys` (:71-80) is a no-op until applySafe captured
    // the snapshot (`!ccdSpawnEnvKeys`); an un-latched read reproduces that.
    let ccd_keys = ccd_spawn_env_keys();

    env.into_iter()
        .flat_map(|env| process_env::ecmascript_object_entries(env.iter()))
        .filter(|(key, _)| !ccd_keys.as_ref().is_some_and(|keys| keys.contains(*key)))
        .map(|(key, value)| (key.to_owned(), value.clone()))
        .collect()
}

/// One `Object.assign(process.env, filterSettingsEnv(...))` step.
fn apply_filtered(update: &mut EnvUpdate<'_>, env: Option<&IndexMap<String, String>>) {
    let filtered = filter_settings_env(env);
    update.apply(filtered);
}

/// Apply environment variables from trusted sources before the trust dialog.
///
/// Trusted sources (user settings, `--settings` flag) apply
/// ALL their env vars; project-scoped sources contribute only the
/// [`SAFE_ENV_VARS`] allowlist, so a repository-controlled
/// `.nyan/settings.json` cannot inject arbitrary environment variables
/// before trust is established.
///
/// Maps to: CC `utils/managedEnv.ts:124-178` `applySafeConfigEnvironmentVariables`.
pub fn apply_safe_config_environment_variables() {
    // Collect cache/disk-backed inputs before opening the non-reentrant carrier
    // turn; only source-ordered filtering, staging, and publication belong
    // under its lock.
    let global = crate::utils::config::load_global_config();
    let trusted = [SettingSource::User, SettingSource::Flag]
        .into_iter()
        .filter(|source| is_setting_source_enabled(*source))
        .filter_map(get_settings_for_source)
        .collect::<Vec<_>>();
    let merged = get_initial_settings();

    let mut update = process_env::begin_update();

    // Capture CCD spawn-env keys before any settings.env is applied (:126-131).
    capture_ccd_spawn_env_keys(&update.snapshot());

    // Global config (state.json) env, user-controlled (:133-136).
    apply_filtered(&mut update, global.env.as_ref());

    // Trusted setting sources (:138-149). The isSettingSourceEnabled gate only
    // ever filters userSettings (SDK settingSources isolation, gh#217) — flag
    // is always enabled.
    for settings in &trusted {
        apply_filtered(&mut update, settings.env.as_deref());
    }

    // @nyan: CC's remote-managed-settings eligibility check (:151-157) and
    // policySettings env (:159-162) are gone with the managed policy layer.

    // Safe allowlisted vars from the fully-merged settings, which include
    // project-scoped sources (:164-178).
    let safe = filter_settings_env(merged.env.as_deref())
        .into_iter()
        .filter(|(key, _)| is_safe_managed_env_var(key));
    update.apply(safe);
    update.commit();
}

/// Apply ALL settings env vars.
/// Applies potentially dangerous variables such as `LD_PRELOAD` and `PATH`;
/// call only after trust is established.
///
/// Maps to: CC `utils/managedEnv.ts:187-199` `applyConfigEnvironmentVariables`.
pub fn apply_config_environment_variables() {
    let global = crate::utils::config::load_global_config();
    let merged = get_initial_settings();

    let mut update = process_env::begin_update();
    apply_filtered(&mut update, global.env.as_ref());
    apply_filtered(&mut update, merged.env.as_deref());

    update.commit();

    // (:192-198) Clear the caches so the agents are rebuilt from the new
    // environment, then reconfigure them. They read files and the committed
    // environment, so this runs after the commit, outside the EnvUpdate lock.
    crate::utils::ca_certs::clear_ca_certs_cache();
    crate::utils::mtls::clear_mtls_cache();
    crate::utils::proxy::clear_proxy_cache();
    crate::utils::proxy::configure_global_agents();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    /// CC `utils/managedEnv.ts#applyConfigEnvironmentVariables` applies each
    /// settings object with `Object.assign`, so own-key projection precedes
    /// Node `process.env` assignment normalization.
    #[test]
    fn settings_application_matches_official_object_assign_then_normalize_order() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _numeric = EnvVarGuard::unset("1");
        reset_ccd_spawn_env_keys_for_tests();
        let env = IndexMap::from([
            ("1\0tail".to_string(), "malformed".to_string()),
            ("1".to_string(), "exact".to_string()),
        ]);

        let mut update = process_env::begin_update();
        apply_filtered(&mut update, Some(&env));
        update.commit();

        assert_eq!(process_env::var("1").as_deref(), Some("malformed"));
    }

    #[test]
    fn ccd_spawn_keys_latch_once_and_shield_spawn_env() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        reset_ccd_spawn_env_keys_for_tests();
        let _entry = EnvVarGuard::set("CLAUDE_CODE_ENTRYPOINT", "claude-desktop");
        let _spawn_var = EnvVarGuard::set("OTEL_LOGS_EXPORTER", "otlp");

        capture_ccd_spawn_env_keys(&process_env::snapshot());

        let env: IndexMap<String, String> = [
            ("OTEL_LOGS_EXPORTER", "console"),
            ("BRAND_NEW_VAR", "applies"),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect();
        let filtered = filter_settings_env(Some(&env));
        // Keys present in the spawn env are shielded; later additions apply.
        assert!(!filtered.iter().any(|(key, _)| key == "OTEL_LOGS_EXPORTER"));
        assert!(filtered.iter().any(|(key, _)| key == "BRAND_NEW_VAR"));

        reset_ccd_spawn_env_keys_for_tests();
    }

    #[test]
    fn ccd_capture_is_null_outside_claude_desktop() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        reset_ccd_spawn_env_keys_for_tests();
        let _entry = EnvVarGuard::set("CLAUDE_CODE_ENTRYPOINT", "cli");

        capture_ccd_spawn_env_keys(&process_env::snapshot());
        assert_eq!(ccd_spawn_env_keys(), None);

        reset_ccd_spawn_env_keys_for_tests();
    }
}
