//! Anthropic API-key source helpers (`getAnthropicApiKeyWithSource` and
//! `getApiKeyFromApiKeyHelper`).
//!
//! Anthropic is API-key only: there is no claude.ai/Console OAuth, token
//! refresh or subscription state here. OAuth for other providers lives in
//! `utils/provider_auth`.

use crate::utils::auth_file_descriptor::get_api_key_from_file_descriptor;
use crate::utils::config::{GlobalConfig, normalize_api_key_for_config};
use crate::utils::env_utils::is_running_on_homespace;
use crate::utils::process_env::JsTruthy;
use crate::utils::settings::constants::SettingSource;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

/// Credential shape returned by CC `utils/auth.ts#refreshAndGetAwsCredentials`.
#[derive(Clone, Debug)]
pub struct AwsCredentials {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: Option<String>,
}

/// Maps to: CC `utils/auth.ts#refreshAndGetAwsCredentials`.
///
/// The official owner returns credentials only from its configured
/// auth-refresh/export flow. That lifecycle is not yet ported; callers must
/// continue to delegate the ordinary credential chain to the provider SDK.
pub async fn refresh_and_get_aws_credentials() -> Option<AwsCredentials> {
    None
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AuthIoOperation {
    TokenFileRead,
    KeychainSubprocess,
    ApiKeyHelperSubprocess,
    SettingsFileRead,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct AuthIoProbeSnapshot {
    pub token_file_reads: usize,
    pub keychain_subprocesses: usize,
    pub api_key_helper_subprocesses: usize,
    pub settings_file_reads: usize,
}

#[cfg(test)]
thread_local! {
    static AUTH_IO_PROBE: std::cell::Cell<AuthIoProbeSnapshot> =
        const { std::cell::Cell::new(AuthIoProbeSnapshot {
            token_file_reads: 0,
            keychain_subprocesses: 0,
            api_key_helper_subprocesses: 0,
            settings_file_reads: 0,
        }) };
}

pub(crate) fn record_auth_io(operation: AuthIoOperation) {
    #[cfg(test)]
    AUTH_IO_PROBE.with(|probe| {
        let mut snapshot = probe.get();
        match operation {
            AuthIoOperation::TokenFileRead => snapshot.token_file_reads += 1,
            AuthIoOperation::KeychainSubprocess => snapshot.keychain_subprocesses += 1,
            AuthIoOperation::ApiKeyHelperSubprocess => snapshot.api_key_helper_subprocesses += 1,
            AuthIoOperation::SettingsFileRead => snapshot.settings_file_reads += 1,
        }
        probe.set(snapshot);
    });

    #[cfg(not(test))]
    let _ = operation;
}

#[cfg(test)]
pub(crate) fn reset_auth_io_probe() {
    AUTH_IO_PROBE.with(|probe| probe.set(AuthIoProbeSnapshot::default()));
}

#[cfg(test)]
pub(crate) fn auth_io_probe_snapshot() -> AuthIoProbeSnapshot {
    AUTH_IO_PROBE.with(std::cell::Cell::get)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApiKeySource {
    None,
    AnthropicApiKey,
    ApiKeyHelper,
    LoginManagedKey,
}

impl ApiKeySource {
    pub fn status_label(self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::AnthropicApiKey => Some("ANTHROPIC_API_KEY"),
            Self::ApiKeyHelper => Some("apiKeyHelper"),
            Self::LoginManagedKey => Some("/login managed key"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnthropicApiKeyWithSource {
    pub key: Option<String>,
    pub source: ApiKeySource,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GetAnthropicApiKeyOptions {
    pub skip_retrieving_key_from_api_key_helper: bool,
}

fn env_value(get_env: &impl Fn(&str) -> Option<String>, key: &str) -> Option<String> {
    // JavaScript `process.env[key]` truthiness rejects only the empty string;
    // callers that parse booleans or numbers apply their own source-defined
    // whitespace handling. Credential bytes must remain unmodified.
    get_env(key).truthy()
}

/// Maps to: CC `utils/auth.ts:355-363` `getConfiguredApiKeyHelper`.
pub fn get_configured_api_key_helper() -> Option<String> {
    record_auth_io(AuthIoOperation::SettingsFileRead);
    let helper = if crate::utils::env_utils::is_bare_mode() {
        crate::utils::settings::get_settings_for_source(SettingSource::Flag)
            .and_then(|settings| settings.api_key_helper)
    } else {
        crate::utils::settings::get_initial_settings().api_key_helper
    };
    helper.filter(|helper| !helper.is_empty())
}

/// Maps to: CC `utils/auth.ts:366-379` `isApiKeyHelperFromProjectOrLocalSettings`.
fn is_api_key_helper_from_project_or_local_settings() -> bool {
    let Some(helper) = get_configured_api_key_helper() else {
        return false;
    };
    let project_helper = {
        record_auth_io(AuthIoOperation::SettingsFileRead);
        crate::utils::settings::get_settings_for_source(SettingSource::Project)
            .and_then(|settings| settings.api_key_helper)
    };
    let local_helper = {
        record_auth_io(AuthIoOperation::SettingsFileRead);
        crate::utils::settings::get_settings_for_source(SettingSource::Local)
            .and_then(|settings| settings.api_key_helper)
    };
    project_helper.as_deref() == Some(helper.as_str())
        || local_helper.as_deref() == Some(helper.as_str())
}

const DEFAULT_API_KEY_HELPER_TTL_MS: u64 = 5 * 60 * 1000;
const API_KEY_HELPER_TIMEOUT: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Default)]
struct ApiKeyHelperState {
    cache: Option<ApiKeyHelperCache>,
    inflight: bool,
    started_at: Option<Instant>,
    epoch: u64,
}

#[derive(Debug, Clone)]
struct ApiKeyHelperCache {
    value: String,
    timestamp: Instant,
}

static API_KEY_HELPER_STATE: LazyLock<Mutex<ApiKeyHelperState>> =
    LazyLock::new(|| Mutex::new(ApiKeyHelperState::default()));

/// Maps to: CC `utils/auth.ts:435-452` `calculateApiKeyHelperTTL`.
pub fn calculate_api_key_helper_ttl() -> u64 {
    if let Some(raw) = crate::utils::process_env::var("CLAUDE_CODE_API_KEY_HELPER_TTL_MS").truthy()
    {
        if let Ok(parsed) = raw.parse::<u64>() {
            return parsed;
        }
        eprintln!(
            "Found CLAUDE_CODE_API_KEY_HELPER_TTL_MS env var, but it was not a valid number. Got {raw}"
        );
    }
    DEFAULT_API_KEY_HELPER_TTL_MS
}

fn execute_api_key_helper_command(
    command: &str,
    from_project_or_local_settings: bool,
    is_non_interactive_session: bool,
) -> anyhow::Result<Option<String>> {
    // Maps to: CC `_executeApiKeyHelper()`: block project/local helpers before
    // workspace trust in interactive sessions, execute configured command via
    // the shell, wait up to 10 minutes, trim stdout, and treat empty output as
    // an error.
    if from_project_or_local_settings
        && !crate::utils::config::check_has_trust_dialog_accepted()
        && !is_non_interactive_session
    {
        eprintln!(
            "Security: apiKeyHelper executed before workspace trust is confirmed. If you see this message, report this nyan bug."
        );
        return Ok(None);
    }
    record_auth_io(AuthIoOperation::ApiKeyHelperSubprocess);
    // CC execa `shell: true` (auth.ts:558-559): `/bin/sh` off Windows.
    let (shell, shell_flag) = if cfg!(target_os = "windows") {
        ("cmd", "/C")
    } else {
        ("/bin/sh", "-c")
    };
    let mut helper = std::process::Command::new(shell);
    // CC inherits process.env (execa default env); the carrier is its counterpart.
    crate::utils::subprocess_env::apply_process_env_std(&mut helper);
    let mut child = helper
        .args([shell_flag, command])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            let output = child.wait_with_output()?;
            if !status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                let why = status
                    .code()
                    .map(|code| format!("exited {code}"))
                    .unwrap_or_else(|| "terminated".to_string());
                anyhow::bail!(
                    "{}",
                    if stderr.is_empty() {
                        why
                    } else {
                        format!("{why}: {stderr}")
                    }
                );
            }
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if stdout.is_empty() {
                anyhow::bail!("did not return a value");
            }
            return Ok(Some(stdout));
        }
        if started.elapsed() >= API_KEY_HELPER_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("timed out");
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn run_and_cache_api_key_helper(
    helper: &str,
    from_project_or_local_settings: bool,
    is_non_interactive_session: bool,
    is_cold: bool,
    epoch: u64,
) -> Option<String> {
    // Maps to: CC `_runAndCache()`.
    let result = execute_api_key_helper_command(
        helper,
        from_project_or_local_settings,
        is_non_interactive_session,
    );
    let value = match result {
        Ok(Some(value)) => value,
        Ok(None) => {
            let mut state = API_KEY_HELPER_STATE.lock().unwrap();
            if epoch == state.epoch {
                state.inflight = false;
                state.started_at = None;
            }
            return None;
        }
        Err(error) => {
            eprintln!("apiKeyHelper failed: {error}");
            let mut state = API_KEY_HELPER_STATE.lock().unwrap();
            if epoch != state.epoch {
                state.inflight = false;
                state.started_at = None;
                return Some(" ".to_string());
            }
            if !is_cold && state.cache.as_ref().is_some_and(|cache| cache.value != " ") {
                let stale = state.cache.as_ref().unwrap().value.clone();
                if let Some(cache) = state.cache.as_mut() {
                    cache.timestamp = Instant::now();
                }
                state.inflight = false;
                state.started_at = None;
                return Some(stale);
            }
            let sentinel = " ".to_string();
            state.cache = Some(ApiKeyHelperCache {
                value: sentinel.clone(),
                timestamp: Instant::now(),
            });
            state.inflight = false;
            state.started_at = None;
            return Some(sentinel);
        }
    };
    let mut state = API_KEY_HELPER_STATE.lock().unwrap();
    if epoch == state.epoch {
        state.cache = Some(ApiKeyHelperCache {
            value: value.clone(),
            timestamp: Instant::now(),
        });
        state.inflight = false;
        state.started_at = None;
    }
    Some(value)
}

/// Maps to: CC `utils/auth.ts#getApiKeyHelperElapsedMs`.
pub fn get_api_key_helper_elapsed_ms() -> u64 {
    API_KEY_HELPER_STATE
        .lock()
        .unwrap()
        .started_at
        .map(|started| started.elapsed().as_millis() as u64)
        .unwrap_or(0)
}

/// Maps to: CC `utils/auth.ts#getApiKeyFromApiKeyHelper`.
pub fn get_api_key_from_api_key_helper(is_non_interactive_session: bool) -> Option<String> {
    let helper = get_configured_api_key_helper()?;
    let from_project_or_local_settings = is_api_key_helper_from_project_or_local_settings();
    let ttl = Duration::from_millis(calculate_api_key_helper_ttl());
    {
        let mut state = API_KEY_HELPER_STATE.lock().unwrap();
        if let Some(cache) = &state.cache {
            let cached_value = cache.value.clone();
            if cache.timestamp.elapsed() < ttl {
                return Some(cached_value);
            }
            if !state.inflight {
                state.inflight = true;
                state.started_at = None;
                let helper_for_thread = helper.clone();
                let epoch = state.epoch;
                std::thread::spawn(move || {
                    let _ = run_and_cache_api_key_helper(
                        &helper_for_thread,
                        from_project_or_local_settings,
                        is_non_interactive_session,
                        false,
                        epoch,
                    );
                });
            }
            return Some(cached_value);
        }
        if state.inflight {
            return None;
        }
        state.inflight = true;
        state.started_at = Some(Instant::now());
    }
    let epoch = API_KEY_HELPER_STATE.lock().unwrap().epoch;
    run_and_cache_api_key_helper(
        &helper,
        from_project_or_local_settings,
        is_non_interactive_session,
        true,
        epoch,
    )
}

/// Maps to: CC `utils/auth.ts#getApiKeyFromApiKeyHelperCached`.
pub fn get_api_key_from_api_key_helper_cached() -> Option<String> {
    API_KEY_HELPER_STATE
        .lock()
        .unwrap()
        .cache
        .as_ref()
        .map(|cache| cache.value.clone())
}

/// Maps to: CC `utils/auth.ts#clearApiKeyHelperCache`.
pub fn clear_api_key_helper_cache() {
    let mut state = API_KEY_HELPER_STATE.lock().unwrap();
    state.epoch = state.epoch.saturating_add(1);
    state.cache = None;
    state.inflight = false;
    state.started_at = None;
}

/// Maps to: CC `utils/auth.ts:591-602` `prefetchApiKeyFromApiKeyHelperIfSafe`.
pub fn prefetch_api_key_from_api_key_helper_if_safe(is_non_interactive_session: bool) {
    if is_api_key_helper_from_project_or_local_settings()
        && !crate::utils::config::check_has_trust_dialog_accepted()
    {
        return;
    }
    std::thread::spawn(move || {
        let _ = get_api_key_from_api_key_helper(is_non_interactive_session);
    });
}

/// Maps to CC `getApiKeyFromConfigOrMacOSKeychain()`.
pub fn get_api_key_from_config_or_macos_keychain() -> Option<AnthropicApiKeyWithSource> {
    if crate::utils::env_utils::is_bare_mode() {
        return None;
    }

    // Maps to: CC `utils/auth.ts:1051-1083` macOS branch. This legacy API-key
    // path remains in the auth owner; only service-name/cache helpers are
    // delegated to their source-shaped secure-storage owners.
    #[cfg(target_os = "macos")]
    let primary_api_key = {
        let username = crate::utils::secure_storage::mac_os_keychain_helpers::get_username();
        crate::utils::secure_storage::mac_os_keychain_helpers::get_mac_os_keychain_storage_service_name("")
            .ok()
            .and_then(|service_name| {
                if let Some(prefetched) = crate::utils::secure_storage::keychain_prefetch::get_legacy_api_key_prefetch_result() {
                    return prefetched;
                }
                #[cfg(not(test))]
                let value = {
                    record_auth_io(AuthIoOperation::KeychainSubprocess);
                    let mut security = std::process::Command::new("security");
                    // CC passes `env: process.env` (execSyncWithDefaults_DEPRECATED); the carrier is its counterpart.
                    crate::utils::subprocess_env::apply_process_env_std(&mut security);
                    security
                        .args([
                            "find-generic-password",
                            "-a",
                            &username,
                            "-w",
                            "-s",
                            &service_name,
                        ])
                        .output()
                        .ok()
                        .filter(|output| output.status.success())
                        .and_then(|output| String::from_utf8(output.stdout).ok())
                        .map(|value| value.trim().to_string())
                        .filter(|value| !value.is_empty())
                };
                #[cfg(test)]
                let value = {
                    #[cfg(test)]
                    record_auth_io(AuthIoOperation::KeychainSubprocess);
                    None
                };
                value
            })
    };
    #[cfg(not(target_os = "macos"))]
    let primary_api_key: Option<String> = None;
    if let Some(primary_api_key) = primary_api_key {
        return Some(AnthropicApiKeyWithSource {
            key: Some(primary_api_key),
            source: ApiKeySource::LoginManagedKey,
        });
    }

    crate::utils::config::load_global_config()
        .primary_api_key
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(|primary_api_key| AnthropicApiKeyWithSource {
            key: Some(primary_api_key.to_string()),
            source: ApiKeySource::LoginManagedKey,
        })
}

fn is_custom_api_key_approved(config: &GlobalConfig, api_key: &str) -> bool {
    let normalized_key = normalize_api_key_for_config(api_key);
    config
        .custom_api_key_responses
        .as_ref()
        .and_then(|responses| responses.approved.as_ref())
        .is_some_and(|approved| approved.iter().any(|key| key == &normalized_key))
}

/// Maps to: CC `utils/auth.ts:221-224` `getAnthropicApiKey`.
pub fn get_anthropic_api_key() -> Option<String> {
    get_anthropic_api_key_with_source(GetAnthropicApiKeyOptions::default()).key
}

/// Maps to: CC `utils/auth.ts#hasAnthropicApiKeyAuth`.
///
/// The helper is intentionally queried with the source's skip flag: command
/// visibility must not execute a configured `apiKeyHelper` process.
pub fn has_anthropic_api_key_auth() -> bool {
    let result = get_anthropic_api_key_with_source(GetAnthropicApiKeyOptions {
        skip_retrieving_key_from_api_key_helper: true,
    });
    result.key.is_some() && result.source != ApiKeySource::None
}

/// Maps to: CC `utils/auth.ts:226-341` `getAnthropicApiKeyWithSource`.
pub fn get_anthropic_api_key_with_source(
    opts: GetAnthropicApiKeyOptions,
) -> AnthropicApiKeyWithSource {
    let config = crate::utils::config::load_global_config();
    let get_env = |key: &str| crate::utils::process_env::var(key);
    if crate::utils::env_utils::is_bare_mode() {
        if let Some(api_key) = env_value(&get_env, "ANTHROPIC_API_KEY") {
            return AnthropicApiKeyWithSource {
                key: Some(api_key),
                source: ApiKeySource::AnthropicApiKey,
            };
        }
        if get_configured_api_key_helper().is_some() {
            return AnthropicApiKeyWithSource {
                key: if opts.skip_retrieving_key_from_api_key_helper {
                    None
                } else {
                    get_api_key_from_api_key_helper(false)
                },
                source: ApiKeySource::ApiKeyHelper,
            };
        }
        return AnthropicApiKeyWithSource {
            key: None,
            source: ApiKeySource::None,
        };
    }

    // Maps to CC `getAnthropicApiKeyWithSource()`: homespace ignores
    // ANTHROPIC_API_KEY; normal interactive sessions only use that env key
    // after the onboarding approval list contains its normalized suffix.
    let api_key_env = if is_running_on_homespace() {
        None
    } else {
        env_value(&get_env, "ANTHROPIC_API_KEY")
    };

    if crate::bootstrap::state::prefer_third_party_authentication()
        && let Some(api_key) = api_key_env.clone()
    {
        return AnthropicApiKeyWithSource {
            key: Some(api_key),
            source: ApiKeySource::AnthropicApiKey,
        };
    }

    if crate::utils::env_utils::is_env_truthy(get_env("CI").as_deref())
        || env_value(&get_env, "NODE_ENV").as_deref() == Some("test")
    {
        if let Some(api_key) = get_api_key_from_file_descriptor() {
            return AnthropicApiKeyWithSource {
                key: Some(api_key),
                source: ApiKeySource::AnthropicApiKey,
            };
        }
        if let Some(api_key) = api_key_env {
            return AnthropicApiKeyWithSource {
                key: Some(api_key),
                source: ApiKeySource::AnthropicApiKey,
            };
        }
        return AnthropicApiKeyWithSource {
            key: None,
            source: ApiKeySource::None,
        };
    }

    if let Some(api_key) = api_key_env.filter(|key| is_custom_api_key_approved(&config, key)) {
        return AnthropicApiKeyWithSource {
            key: Some(api_key),
            source: ApiKeySource::AnthropicApiKey,
        };
    }

    if let Some(api_key) = get_api_key_from_file_descriptor() {
        return AnthropicApiKeyWithSource {
            key: Some(api_key),
            source: ApiKeySource::AnthropicApiKey,
        };
    }

    if get_configured_api_key_helper().is_some() {
        return AnthropicApiKeyWithSource {
            key: if opts.skip_retrieving_key_from_api_key_helper {
                None
            } else {
                get_api_key_from_api_key_helper(false)
            },
            source: ApiKeySource::ApiKeyHelper,
        };
    }

    if let Some(api_key) = get_api_key_from_config_or_macos_keychain() {
        return api_key;
    }

    AnthropicApiKeyWithSource {
        key: None,
        source: ApiKeySource::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    struct CurrentDirGuard {
        previous: std::path::PathBuf,
    }

    impl CurrentDirGuard {
        fn set(path: &std::path::Path) -> Self {
            let previous = std::env::current_dir().unwrap();
            std::env::set_current_dir(path).unwrap();
            Self { previous }
        }
    }

    impl Drop for CurrentDirGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.previous);
        }
    }

    struct OriginalCwdGuard(std::path::PathBuf);

    impl OriginalCwdGuard {
        fn set(path: &std::path::Path) -> Self {
            let previous = crate::bootstrap::state::get_original_cwd();
            crate::bootstrap::state::set_original_cwd(path.to_path_buf());
            Self(previous)
        }
    }

    impl Drop for OriginalCwdGuard {
        fn drop(&mut self) {
            crate::bootstrap::state::set_original_cwd(self.0.clone());
        }
    }

    struct FlagSettingsPathGuard {
        previous: Option<std::path::PathBuf>,
    }

    impl FlagSettingsPathGuard {
        fn set(path: Option<std::path::PathBuf>) -> Self {
            let previous = crate::utils::settings::get_flag_settings_path();
            crate::utils::settings::set_flag_settings_path(path);
            Self { previous }
        }
    }

    impl Drop for FlagSettingsPathGuard {
        fn drop(&mut self) {
            crate::utils::settings::set_flag_settings_path(self.previous.clone());
        }
    }

    fn unique_temp_dir(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "nyan-auth-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn api_key_helper_bare_mode_uses_only_flag_settings_source() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _simple = EnvVarGuard::set("CLAUDE_CODE_SIMPLE", "1");
        clear_api_key_helper_cache();
        let dir = unique_temp_dir("bare-helper");
        std::fs::create_dir_all(&dir).unwrap();
        let flag_path = dir.join("flag-settings.json");
        std::fs::write(&flag_path, r#"{"apiKeyHelper":"flag-helper"}"#).unwrap();
        let _flag_guard = FlagSettingsPathGuard::set(Some(flag_path));

        assert_eq!(
            get_configured_api_key_helper().as_deref(),
            Some("flag-helper")
        );

        drop(_flag_guard);
        assert_eq!(get_configured_api_key_helper(), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn project_api_key_helper_is_blocked_until_trust_in_interactive_sessions() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        clear_api_key_helper_cache();
        let dir = unique_temp_dir("project-helper-trust");
        let config_home = dir.join("config");
        let project = dir.join("project");
        std::fs::create_dir_all(project.join(".claude")).unwrap();
        std::fs::create_dir_all(&config_home).unwrap();
        let command = if cfg!(target_os = "windows") {
            "echo project-key"
        } else {
            "printf project-key"
        };
        std::fs::write(
            project.join(".claude/settings.json"),
            format!(r#"{{"apiKeyHelper":"{command}"}}"#),
        )
        .unwrap();
        let _config_guard = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config_home);
        let _cwd_guard = CurrentDirGuard::set(&project);
        let _original_cwd_guard = OriginalCwdGuard::set(&project);

        let mut config = GlobalConfig::default();
        config.api_key_helper = Some(command.to_string());
        crate::utils::config::set_test_global_config(Some(config));
        assert_eq!(get_api_key_from_api_key_helper(false), None);
        assert_eq!(get_api_key_from_api_key_helper_cached(), None);
        assert_eq!(
            get_api_key_from_api_key_helper(true).as_deref(),
            Some("project-key")
        );
        clear_api_key_helper_cache();
        crate::utils::config::set_test_global_config(None);
        drop(_cwd_guard);
        drop(_config_guard);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn api_key_helper_executes_caches_and_uses_official_failure_sentinel() {
        let _env_guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ttl = EnvVarGuard::set("CLAUDE_CODE_API_KEY_HELPER_TTL_MS", "300000");
        let dir = unique_temp_dir("helper-cache");
        std::fs::create_dir_all(&dir).unwrap();
        let _config_home = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &dir);
        clear_api_key_helper_cache();

        let command = if cfg!(target_os = "windows") {
            "echo nyan-helper-key"
        } else {
            "printf nyan-helper-key"
        };
        std::fs::write(
            dir.join("settings.json"),
            serde_json::json!({"apiKeyHelper": command}).to_string(),
        )
        .unwrap();
        assert_eq!(
            get_api_key_from_api_key_helper(false).as_deref(),
            Some("nyan-helper-key")
        );
        assert_eq!(
            get_api_key_from_api_key_helper_cached().as_deref(),
            Some("nyan-helper-key")
        );

        clear_api_key_helper_cache();
        let failing_command = if cfg!(target_os = "windows") {
            "exit /B 7"
        } else {
            "exit 7"
        };
        std::fs::write(
            dir.join("settings.json"),
            serde_json::json!({"apiKeyHelper": failing_command}).to_string(),
        )
        .unwrap();
        // `clear_api_key_helper_cache` drops the helper RESULT cache, not the
        // merged-settings cache that supplies `apiKeyHelper` itself — the read
        // above memoized the previous command for the session, so without this
        // the failing command is never even reached.
        crate::utils::settings::settings_cache::reset_settings_cache();
        assert_eq!(get_api_key_from_api_key_helper(false).as_deref(), Some(" "));

        clear_api_key_helper_cache();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn api_key_with_source_preserves_official_precedence_without_helper_execution() {
        let _env_guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = unique_temp_dir("api-key-precedence");
        std::fs::create_dir_all(&dir).unwrap();
        let _config_home = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &dir);
        let _api_key = EnvVarGuard::unset("ANTHROPIC_API_KEY");
        let _homespace = EnvVarGuard::unset("COO_RUNNING_ON_HOMESPACE");
        let _ci = EnvVarGuard::unset("CI");
        let _node_env = EnvVarGuard::unset("NODE_ENV");
        std::fs::write(
            dir.join("settings.json"),
            r#"{"apiKeyHelper":"helper --print"}"#,
        )
        .unwrap();

        let mut config = GlobalConfig::default();
        config.primary_api_key = Some("managed".to_string());
        crate::utils::config::set_test_global_config(Some(config.clone()));

        let helper = get_anthropic_api_key_with_source(GetAnthropicApiKeyOptions {
            skip_retrieving_key_from_api_key_helper: true,
        });
        assert_eq!(helper.source, ApiKeySource::ApiKeyHelper);
        assert_eq!(helper.key, None);

        crate::utils::process_env::set("ANTHROPIC_API_KEY", "sk-ant-test");
        let unapproved_env_key = get_anthropic_api_key_with_source(GetAnthropicApiKeyOptions {
            skip_retrieving_key_from_api_key_helper: true,
        });
        assert_eq!(unapproved_env_key.source, ApiKeySource::ApiKeyHelper);
        assert_eq!(unapproved_env_key.key, None);

        config.custom_api_key_responses = Some(crate::utils::config::CustomApiKeyResponses {
            approved: Some(vec![normalize_api_key_for_config("sk-ant-test")]),
            rejected: None,
        });
        crate::utils::config::set_test_global_config(Some(config));
        let approved_env_key =
            get_anthropic_api_key_with_source(GetAnthropicApiKeyOptions::default());
        assert_eq!(approved_env_key.source, ApiKeySource::AnthropicApiKey);
        assert_eq!(approved_env_key.key.as_deref(), Some("sk-ant-test"));

        crate::utils::process_env::set("COO_RUNNING_ON_HOMESPACE", "true");
        let homespace_key = get_anthropic_api_key_with_source(GetAnthropicApiKeyOptions {
            skip_retrieving_key_from_api_key_helper: true,
        });
        if crate::utils::build_profile::build_audience().is_internal() {
            assert_eq!(homespace_key.source, ApiKeySource::ApiKeyHelper);
            assert_eq!(homespace_key.key, None);
        } else {
            assert_eq!(homespace_key.source, ApiKeySource::AnthropicApiKey);
            assert_eq!(homespace_key.key.as_deref(), Some("sk-ant-test"));
        }

        crate::utils::config::set_test_global_config(None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn api_key_file_descriptor_reads_existing_fd_without_placeholder_or_write() {
        use std::os::fd::AsRawFd;

        let _env_guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let path = std::env::temp_dir().join(format!(
            "nyan-auth-fd-{}-{}.txt",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, " sk-ant-fd-readonly\n").unwrap();
        let file = std::fs::File::open(&path).unwrap();
        let fd = file.as_raw_fd().to_string();
        let _fd_guard = EnvVarGuard::set("CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR", &fd);
        crate::bootstrap::state::reset_auth_file_descriptor_caches_for_testing();

        let api_key = get_anthropic_api_key_with_source(GetAnthropicApiKeyOptions::default());

        assert_eq!(api_key.source, ApiKeySource::AnthropicApiKey);
        assert_eq!(api_key.key.as_deref(), Some("sk-ant-fd-readonly"));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            " sk-ant-fd-readonly\n"
        );
        crate::bootstrap::state::reset_auth_file_descriptor_caches_for_testing();
        drop(file);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn bare_mode_limits_api_key_sources_like_official_simple_mode() {
        let _env_guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _simple = EnvVarGuard::set("CLAUDE_CODE_SIMPLE", "1");
        let mut config = GlobalConfig::default();
        config.primary_api_key = Some("managed".to_string());
        crate::utils::config::set_test_global_config(Some(config));
        let _api_key = EnvVarGuard::unset("ANTHROPIC_API_KEY");

        let no_key = get_anthropic_api_key_with_source(GetAnthropicApiKeyOptions::default());
        assert_eq!(no_key.source, ApiKeySource::None);
        assert_eq!(no_key.key, None);

        crate::utils::process_env::set("ANTHROPIC_API_KEY", "sk-ant-test");
        let env_key = get_anthropic_api_key_with_source(GetAnthropicApiKeyOptions::default());
        assert_eq!(env_key.source, ApiKeySource::AnthropicApiKey);
        assert_eq!(env_key.key.as_deref(), Some("sk-ant-test"));
        crate::utils::config::set_test_global_config(None);
    }
}
