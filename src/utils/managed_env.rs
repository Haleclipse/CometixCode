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

use crate::utils::process_env::{self, EnvSnapshot, EnvUpdate, JsTruthy};
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

/// Maps to CC `utils/managedEnvConstants.ts#PROVIDER_MANAGED_ENV_VARS`.
const PROVIDER_MANAGED_ENV_VARS: &[&str] = &[
    // The flag itself — settings can't unset it once the host set it
    "CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST",
    // Provider selection
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
    // Endpoint config (base URLs, project/resource identifiers)
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_BEDROCK_BASE_URL",
    "ANTHROPIC_VERTEX_BASE_URL",
    "ANTHROPIC_FOUNDRY_BASE_URL",
    "ANTHROPIC_FOUNDRY_RESOURCE",
    "ANTHROPIC_VERTEX_PROJECT_ID",
    // Region routing (per-model VERTEX_REGION_CLAUDE_* handled by prefix below)
    "CLOUD_ML_REGION",
    // Auth
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "AWS_BEARER_TOKEN_BEDROCK",
    "ANTHROPIC_FOUNDRY_API_KEY",
    "CLAUDE_CODE_SKIP_BEDROCK_AUTH",
    "CLAUDE_CODE_SKIP_VERTEX_AUTH",
    "CLAUDE_CODE_SKIP_FOUNDRY_AUTH",
    // Model defaults — often set to provider-specific ID formats
    "ANTHROPIC_MODEL",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL_DESCRIPTION",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL_NAME",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL_SUPPORTED_CAPABILITIES",
    "ANTHROPIC_DEFAULT_OPUS_MODEL",
    "ANTHROPIC_DEFAULT_OPUS_MODEL_DESCRIPTION",
    "ANTHROPIC_DEFAULT_OPUS_MODEL_NAME",
    "ANTHROPIC_DEFAULT_OPUS_MODEL_SUPPORTED_CAPABILITIES",
    "ANTHROPIC_DEFAULT_SONNET_MODEL",
    "ANTHROPIC_DEFAULT_SONNET_MODEL_DESCRIPTION",
    "ANTHROPIC_DEFAULT_SONNET_MODEL_NAME",
    "ANTHROPIC_DEFAULT_SONNET_MODEL_SUPPORTED_CAPABILITIES",
    "ANTHROPIC_SMALL_FAST_MODEL",
    "ANTHROPIC_SMALL_FAST_MODEL_AWS_REGION",
    "CLAUDE_CODE_SUBAGENT_MODEL",
];

/// Maps to CC `utils/managedEnvConstants.ts#PROVIDER_MANAGED_ENV_PREFIXES`.
const PROVIDER_MANAGED_ENV_PREFIXES: &[&str] = &["VERTEX_REGION_CLAUDE_"];

/// Maps to CC `utils/managedEnvConstants.ts#isProviderManagedEnvVar`.
pub fn is_provider_managed_env_var(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    PROVIDER_MANAGED_ENV_VARS.contains(&upper.as_str())
        || PROVIDER_MANAGED_ENV_PREFIXES
            .iter()
            .any(|prefix| upper.starts_with(prefix))
}

/// Maps to CC `utils/managedEnvConstants.ts#SAFE_ENV_VARS`.
pub const SAFE_ENV_VARS: &[&str] = &[
    "ANTHROPIC_CUSTOM_HEADERS",
    "ANTHROPIC_CUSTOM_MODEL_OPTION",
    "ANTHROPIC_CUSTOM_MODEL_OPTION_DESCRIPTION",
    "ANTHROPIC_CUSTOM_MODEL_OPTION_NAME",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL_DESCRIPTION",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL_NAME",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL_SUPPORTED_CAPABILITIES",
    "ANTHROPIC_DEFAULT_OPUS_MODEL",
    "ANTHROPIC_DEFAULT_OPUS_MODEL_DESCRIPTION",
    "ANTHROPIC_DEFAULT_OPUS_MODEL_NAME",
    "ANTHROPIC_DEFAULT_OPUS_MODEL_SUPPORTED_CAPABILITIES",
    "ANTHROPIC_DEFAULT_SONNET_MODEL",
    "ANTHROPIC_DEFAULT_SONNET_MODEL_DESCRIPTION",
    "ANTHROPIC_DEFAULT_SONNET_MODEL_NAME",
    "ANTHROPIC_DEFAULT_SONNET_MODEL_SUPPORTED_CAPABILITIES",
    "ANTHROPIC_FOUNDRY_API_KEY",
    "ANTHROPIC_MODEL",
    "ANTHROPIC_SMALL_FAST_MODEL_AWS_REGION",
    "ANTHROPIC_SMALL_FAST_MODEL",
    "AWS_DEFAULT_REGION",
    "AWS_PROFILE",
    "AWS_REGION",
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
    "CLAUDE_CODE_SKIP_BEDROCK_AUTH",
    "CLAUDE_CODE_SKIP_FOUNDRY_AUTH",
    "CLAUDE_CODE_SKIP_VERTEX_AUTH",
    "CLAUDE_CODE_SUBAGENT_MODEL",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_FOUNDRY",
    "CLAUDE_CODE_USE_VERTEX",
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
    "VERTEX_REGION_CLAUDE_3_5_HAIKU",
    "VERTEX_REGION_CLAUDE_3_5_SONNET",
    "VERTEX_REGION_CLAUDE_3_7_SONNET",
    "VERTEX_REGION_CLAUDE_4_0_OPUS",
    "VERTEX_REGION_CLAUDE_4_0_SONNET",
    "VERTEX_REGION_CLAUDE_4_1_OPUS",
    "VERTEX_REGION_CLAUDE_4_5_SONNET",
    "VERTEX_REGION_CLAUDE_4_6_SONNET",
    "VERTEX_REGION_CLAUDE_HAIKU_4_5",
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

/// Maps to CC `managedEnv.ts:23-36` `withoutSSHTunnelVars` key set.
fn is_ssh_tunnel_var(key: &str) -> bool {
    matches!(
        key,
        "ANTHROPIC_UNIX_SOCKET"
            | "ANTHROPIC_BASE_URL"
            | "ANTHROPIC_API_KEY"
            | "ANTHROPIC_AUTH_TOKEN"
            | "CLAUDE_CODE_OAUTH_TOKEN"
    )
}

/// Maps to CC `managedEnv.ts:85-91` `filterSettingsEnv` — the composed strip
/// filters applied to every settings-sourced env object.
///
/// `process` is the calling transaction's own snapshot ([`EnvUpdate::snapshot`]):
/// CC reads `process.env` at filter time (`:27`, `:49`), which mid-pass means
/// the partially-applied state.
fn filter_settings_env(
    env: Option<&IndexMap<String, String>>,
    process: &EnvSnapshot,
) -> Vec<(String, String)> {
    // JS truthiness: an empty ANTHROPIC_UNIX_SOCKET disables the filter (:27).
    let ssh_tunnel = process.var("ANTHROPIC_UNIX_SOCKET").truthy().is_some();
    let host_managed =
        crate::utils::env_utils::is_env_truthy(process.var("CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST"));
    // CC `withoutCcdSpawnEnvKeys` (:71-80) is a no-op until applySafe captured
    // the snapshot (`!ccdSpawnEnvKeys`); an un-latched read reproduces that.
    let ccd_keys = ccd_spawn_env_keys();

    env.into_iter()
        .flat_map(|env| process_env::ecmascript_object_entries(env.iter()))
        .filter(|(key, _)| !(ssh_tunnel && is_ssh_tunnel_var(key)))
        .filter(|(key, _)| !(host_managed && is_provider_managed_env_var(key)))
        .filter(|(key, _)| !ccd_keys.as_ref().is_some_and(|keys| keys.contains(*key)))
        .map(|(key, value)| (key.to_owned(), value.clone()))
        .collect()
}

/// One `Object.assign(process.env, filterSettingsEnv(...))` step.
fn apply_filtered(update: &mut EnvUpdate<'_>, env: Option<&IndexMap<String, String>>) {
    let filtered = filter_settings_env(env, &update.snapshot());
    update.apply(filtered);
}

/// Apply environment variables from trusted sources before the trust dialog.
///
/// Trusted sources (user settings, `--settings` flag, managed policy) apply
/// ALL their env vars; project-scoped sources contribute only the
/// [`SAFE_ENV_VARS`] allowlist, so a repository-controlled
/// `.claude/settings.json` cannot redirect provider routing (e.g.
/// `ANTHROPIC_BASE_URL`) before trust is established.
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
    let policy = get_settings_for_source(SettingSource::Policy);
    let merged = get_initial_settings();

    let mut update = process_env::begin_update();

    // Capture CCD spawn-env keys before any settings.env is applied (:126-131).
    capture_ccd_spawn_env_keys(&update.snapshot());

    // Global config (~/.claude.json) env, user-controlled (:133-136).
    apply_filtered(&mut update, global.env.as_ref());

    // Trusted setting sources; policySettings is deferred below (:138-149).
    // The isSettingSourceEnabled gate only ever filters userSettings (SDK
    // settingSources isolation, gh#217) — flag/policy are always enabled.
    for settings in &trusted {
        apply_filtered(&mut update, settings.env.as_deref());
    }

    // (:151-157) CC computes remote-managed-settings eligibility exactly here
    // — after user/flag env (eligibility reads CLAUDE_CODE_USE_BEDROCK and
    // ANTHROPIC_BASE_URL, both settable via settings.env) and before policy env.
    // `services/remoteManagedSettings` is not ported (MODULE_MAP: missing);
    // when it lands, prebuild its cache-backed input before this turn and use
    // `update.snapshot()` only for this pure eligibility decision.

    // Policy env, applied last among trusted sources (:159-162).
    apply_filtered(
        &mut update,
        policy.as_ref().and_then(|settings| settings.env.as_deref()),
    );

    // Safe allowlisted vars from the fully-merged settings, which include
    // project-scoped sources (:164-178).
    let safe = filter_settings_env(merged.env.as_deref(), &update.snapshot())
        .into_iter()
        .filter(|(key, _)| is_safe_managed_env_var(key));
    update.apply(safe);
    update.commit();
}

/// Apply ALL settings env vars (only provider-routing vars are stripped when
/// `CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST` is set — see [`filter_settings_env`]).
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

    #[test]
    fn provider_managed_matches_set_and_prefix_case_insensitively() {
        assert!(is_provider_managed_env_var("ANTHROPIC_BASE_URL"));
        assert!(is_provider_managed_env_var("anthropic_base_url"));
        assert!(is_provider_managed_env_var("VERTEX_REGION_CLAUDE_9_0_OPUS"));
        assert!(is_provider_managed_env_var(
            "CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST"
        ));
        assert!(!is_provider_managed_env_var("ANTHROPIC_CUSTOM_HEADERS"));
        assert!(!is_provider_managed_env_var("PATH"));
    }

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
    fn ssh_tunnel_filter_gates_on_socket_presence() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        reset_ccd_spawn_env_keys_for_tests();
        let env: IndexMap<String, String> = [
            ("ANTHROPIC_API_KEY", "from-settings"),
            ("OTHER_VAR", "kept"),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect();

        {
            let _socket = EnvVarGuard::set("ANTHROPIC_UNIX_SOCKET", "/tmp/sock");
            let filtered = filter_settings_env(Some(&env), &process_env::snapshot());
            assert!(!filtered.iter().any(|(key, _)| key == "ANTHROPIC_API_KEY"));
            assert!(filtered.iter().any(|(key, _)| key == "OTHER_VAR"));
        }
        {
            let _socket = EnvVarGuard::unset("ANTHROPIC_UNIX_SOCKET");
            let filtered = filter_settings_env(Some(&env), &process_env::snapshot());
            assert!(filtered.iter().any(|(key, _)| key == "ANTHROPIC_API_KEY"));
        }
        // JS truthiness: an empty socket value disables the filter (:27).
        {
            let _socket = EnvVarGuard::set("ANTHROPIC_UNIX_SOCKET", "");
            let filtered = filter_settings_env(Some(&env), &process_env::snapshot());
            assert!(filtered.iter().any(|(key, _)| key == "ANTHROPIC_API_KEY"));
        }
    }

    #[test]
    fn host_managed_filter_strips_provider_vars_only_when_truthy() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        reset_ccd_spawn_env_keys_for_tests();
        let env: IndexMap<String, String> =
            [("ANTHROPIC_MODEL", "custom"), ("MAX_THINKING_TOKENS", "1")]
                .into_iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect();

        let _no_socket = EnvVarGuard::unset("ANTHROPIC_UNIX_SOCKET");
        {
            let _host = EnvVarGuard::set("CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST", "1");
            let filtered = filter_settings_env(Some(&env), &process_env::snapshot());
            assert!(!filtered.iter().any(|(key, _)| key == "ANTHROPIC_MODEL"));
            assert!(filtered.iter().any(|(key, _)| key == "MAX_THINKING_TOKENS"));
        }
        {
            let _host = EnvVarGuard::set("CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST", "false");
            let filtered = filter_settings_env(Some(&env), &process_env::snapshot());
            assert!(filtered.iter().any(|(key, _)| key == "ANTHROPIC_MODEL"));
        }
    }

    #[test]
    fn ccd_spawn_keys_latch_once_and_shield_spawn_env() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        reset_ccd_spawn_env_keys_for_tests();
        let _no_socket = EnvVarGuard::unset("ANTHROPIC_UNIX_SOCKET");
        let _no_host = EnvVarGuard::unset("CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST");
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
        let filtered = filter_settings_env(Some(&env), &process_env::snapshot());
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
