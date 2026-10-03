//! Auth source and secure-storage helpers matching official `utils/auth.ts`
//! boundaries.
//!
//! These functions intentionally keep the same decision chain used by official
//! Claude Code (`isAnthropicAuthEnabled`, `isClaudeAISubscriber`,
//! `getAnthropicApiKeyWithSource`, and `getApiKeyFromApiKeyHelper`). Claude.ai
//! refresh/check/401 policy remains here while secure-storage mechanics stay in
//! `utils/secureStorage/` and HTTP refresh transport stays in
//! `services/oauth/client.rs`. Other
//! unported login and profile lifecycles remain unavailable.

use crate::constants::oauth::CLAUDE_AI_INFERENCE_SCOPE;
use crate::utils::auth_file_descriptor::{
    get_api_key_from_file_descriptor, get_oauth_token_from_file_descriptor,
};
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthTokenSource {
    None,
    AnthropicAuthToken,
    ClaudeCodeOauthToken,
    ClaudeCodeOauthTokenFileDescriptor,
    CcrOauthTokenFile,
    ApiKeyHelper,
    ClaudeAi,
}

impl AuthTokenSource {
    pub fn status_label(self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::AnthropicAuthToken => Some("ANTHROPIC_AUTH_TOKEN"),
            Self::ClaudeCodeOauthToken => Some("CLAUDE_CODE_OAUTH_TOKEN"),
            Self::ClaudeCodeOauthTokenFileDescriptor => {
                Some("CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR")
            }
            Self::CcrOauthTokenFile => Some("CCR_OAUTH_TOKEN_FILE"),
            Self::ApiKeyHelper => Some("apiKeyHelper"),
            Self::ClaudeAi => Some("claude.ai"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuthTokenSourceStatus {
    pub source: AuthTokenSource,
    pub has_token: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnthropicApiKeyWithSource {
    pub key: Option<String>,
    pub source: ApiKeySource,
}

/// Maps to: CC `utils/auth.ts:1855-1861` `UserAccountInfo`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UserAccountInfo {
    pub subscription: Option<String>,
    pub token_source: Option<String>,
    pub api_key_source: Option<ApiKeySource>,
    pub organization: Option<String>,
    pub email: Option<String>,
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

/// Maps to: CC `utils/auth.ts:88-96` `isManagedOAuthContext`.
fn is_managed_oauth_context() -> bool {
    crate::utils::env_utils::is_env_truthy(crate::utils::process_env::var("CLAUDE_CODE_REMOTE").as_deref())
        || crate::utils::process_env::var("CLAUDE_CODE_ENTRYPOINT").as_deref() == Some("claude-desktop")
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
            "Security: apiKeyHelper executed before workspace trust is confirmed. If you see this message, report this CometixCode bug."
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaudeAiCredentialsSnapshot {
    /// The optional stored subscription type from official secure storage. The
    /// OAuth access token itself is intentionally not retained in this snapshot.
    pub subscription_type: Option<String>,
}

/// Maps to CC `utils/auth.ts` `getClaudeAIOAuthTokens()` read shape for
/// runtime callers that must attach an existing token to a transport.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaudeAiOAuthTokensSnapshot {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<u64>,
    pub scopes: Vec<String>,
    pub subscription_type: Option<String>,
    pub rate_limit_tier: Option<String>,
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

fn credentials_json_tokens(value: &serde_json::Value) -> Option<ClaudeAiOAuthTokensSnapshot> {
    let oauth = value.get("claudeAiOauth")?;
    let access_token = oauth
        .get("accessToken")
        .and_then(serde_json::Value::as_str)
        .filter(|token| !token.is_empty())?
        .to_string();
    let refresh_token = oauth
        .get("refreshToken")
        .and_then(serde_json::Value::as_str)
        .filter(|token| !token.is_empty())
        .map(ToOwned::to_owned);
    let expires_at = oauth.get("expiresAt").and_then(serde_json::Value::as_u64);
    let scopes = oauth
        .get("scopes")
        .and_then(serde_json::Value::as_array)
        .map(|scopes| {
            scopes
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let subscription_type = oauth
        .get("subscriptionType")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);

    let rate_limit_tier = oauth
        .get("rateLimitTier")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);

    Some(ClaudeAiOAuthTokensSnapshot {
        access_token,
        refresh_token,
        expires_at,
        scopes,
        subscription_type,
        rate_limit_tier,
    })
}

fn credentials_json_snapshot(value: &serde_json::Value) -> Option<ClaudeAiCredentialsSnapshot> {
    let tokens = credentials_json_tokens(value)?;
    if !crate::services::oauth::client::should_use_claude_ai_auth(Some(&tokens.scopes)) {
        return None;
    }

    Some(ClaudeAiCredentialsSnapshot {
        subscription_type: tokens.subscription_type,
    })
}

#[cfg(test)]
static LAST_PREPARED_OAUTH_CREDENTIALS: std::sync::Mutex<Option<serde_json::Value>> =
    std::sync::Mutex::new(None);

/// Maps to: CC `utils/auth.ts:1194-1253` `saveOAuthTokensIfNeeded`.
///
/// The service refresh owner supplies the absolute expiry and refreshed token
/// fields; this function owns Claude.ai eligibility, secure-storage fallback,
/// stored plan metadata fallback, and request-shape cache invalidation. Rust's
/// `Result` projects the source update status; as in CC, request-shape caches
/// are invalidated after an enabled update attempt even when persistence reports
/// a failure. There is no separate memoized OAuth-token cache in this reader.
pub fn save_oauth_tokens_if_needed(tokens: &ClaudeAiOAuthTokensSnapshot) -> anyhow::Result<()> {
    if !crate::services::oauth::client::should_use_claude_ai_auth(Some(&tokens.scopes)) {
        return Ok(());
    }
    let Some(refresh_token) = tokens
        .refresh_token
        .as_deref()
        .filter(|refresh_token| !refresh_token.is_empty())
    else {
        return Ok(());
    };
    let Some(expires_at) = tokens.expires_at.filter(|expires_at| *expires_at != 0) else {
        return Ok(());
    };

    let secure_storage = crate::utils::secure_storage::get_secure_storage();
    let mut credentials = secure_storage
        .read()
        .unwrap_or_else(|| serde_json::json!({}));
    if !credentials.is_object() {
        credentials = serde_json::json!({});
    }
    let root = credentials.as_object_mut().expect("object checked above");
    let existing_oauth = root.get("claudeAiOauth").cloned().unwrap_or_default();
    let subscription_type = tokens.subscription_type.clone().or_else(|| {
        existing_oauth
            .get("subscriptionType")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned)
    });
    let rate_limit_tier = tokens.rate_limit_tier.clone().or_else(|| {
        existing_oauth
            .get("rateLimitTier")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned)
    });
    root.insert(
        "claudeAiOauth".to_string(),
        serde_json::json!({
            "accessToken": tokens.access_token,
            "refreshToken": refresh_token,
            "expiresAt": expires_at,
            "scopes": tokens.scopes,
            "subscriptionType": subscription_type,
            "rateLimitTier": rate_limit_tier,
        }),
    );
    #[cfg(test)]
    {
        *LAST_PREPARED_OAUTH_CREDENTIALS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(credentials.clone());
    }

    // Deviation (L2, user-authorized OAuth safety gate): CC
    // `utils/auth.ts:1237` persists the prepared merge. A blocked write returns
    // before credential bytes or beta/tool-schema caches are mutated.
    if !crate::constants::oauth::OAUTH_CREDENTIAL_SIDE_EFFECTS_ENABLED {
        return Err(crate::constants::oauth::OAuthCredentialSideEffectsUnavailable.into());
    }
    let update_result = secure_storage.update(&credentials).and_then(|status| {
        if status.success {
            Ok(())
        } else {
            anyhow::bail!("Failed to save OAuth tokens")
        }
    });
    crate::utils::betas::clear_betas_caches();
    crate::utils::tool_schema_cache::clear_tool_schema_cache();
    update_result
}

pub fn read_claude_ai_credentials_snapshot(
    _get_env: &impl Fn(&str) -> Option<String>,
) -> Option<ClaudeAiCredentialsSnapshot> {
    crate::utils::secure_storage::get_secure_storage()
        .read()
        .and_then(|value| credentials_json_snapshot(&value))
}

/// Maps to: CC `utils/auth.ts:1256-1297` `getClaudeAIOAuthTokens`.
pub fn get_claude_ai_oauth_tokens() -> Option<ClaudeAiOAuthTokensSnapshot> {
    if crate::utils::env_utils::is_bare_mode() {
        return None;
    }
    if let Some(access_token) = crate::utils::process_env::var("CLAUDE_CODE_OAUTH_TOKEN").truthy() {
        return Some(ClaudeAiOAuthTokensSnapshot {
            access_token,
            refresh_token: None,
            expires_at: None,
            scopes: vec![CLAUDE_AI_INFERENCE_SCOPE.to_string()],
            subscription_type: None,
            rate_limit_tier: None,
        });
    }
    if let Some(access_token) = get_oauth_token_from_file_descriptor() {
        return Some(ClaudeAiOAuthTokensSnapshot {
            access_token,
            refresh_token: None,
            expires_at: None,
            scopes: vec![CLAUDE_AI_INFERENCE_SCOPE.to_string()],
            subscription_type: None,
            rate_limit_tier: None,
        });
    }
    crate::utils::secure_storage::get_secure_storage()
        .read()
        .and_then(|value| credentials_json_tokens(&value))
}

/// Maps to: CC `utils/auth.ts:1360-1371` `handleOAuth401Error`.
///
/// The live Rust reader has no process-long memoized token snapshot, so each
/// call re-reads the current secure-storage value before comparing it with the
/// exact token rejected by the proxy. Partial seam: the source's per-token
/// in-flight promise map and async keychain cache invalidation are unavailable.
pub async fn handle_oauth_401_error(failed_access_token: &str) -> anyhow::Result<bool> {
    handle_oauth_401_error_impl(failed_access_token).await
}

/// Maps to: CC `utils/auth.ts:1373-1391` `handleOAuth401ErrorImpl`.
async fn handle_oauth_401_error_impl(failed_access_token: &str) -> anyhow::Result<bool> {
    let Some(current_tokens) = get_claude_ai_oauth_tokens() else {
        return Ok(false);
    };
    if current_tokens.refresh_token.is_none() {
        return Ok(false);
    }
    if current_tokens.access_token != failed_access_token {
        return Ok(true);
    }
    check_and_refresh_oauth_token_if_needed(true).await
}

/// Maps to: CC `utils/auth.ts:1427-1445` `checkAndRefreshOAuthTokenIfNeeded`.
///
/// `force` is the Rust projection of the source's optional force argument. The
/// current live path preserves expiry gating, Claude.ai-scope gating, canonical
/// refresh transport, secure-storage save, and cache invalidation.
///
/// Partial seam: credentials-file mtime invalidation, async keychain re-read,
/// process lock acquisition/retry/race recovery, and in-flight refresh dedup are
/// not available in the current Rust auth lifecycle. This path performs one
/// refresh attempt and propagates transport/storage errors to its consumer.
pub async fn check_and_refresh_oauth_token_if_needed(force: bool) -> anyhow::Result<bool> {
    check_and_refresh_oauth_token_if_needed_impl(force).await
}

/// Maps to: CC `utils/auth.ts:1447-1564` `checkAndRefreshOAuthTokenIfNeededImpl`.
async fn check_and_refresh_oauth_token_if_needed_impl(force: bool) -> anyhow::Result<bool> {
    let Some(tokens) = get_claude_ai_oauth_tokens() else {
        return Ok(false);
    };
    if !force
        && (tokens.refresh_token.is_none()
            || !crate::services::oauth::client::is_oauth_token_expired(tokens.expires_at))
    {
        return Ok(false);
    }
    if tokens.refresh_token.is_none()
        || !crate::services::oauth::client::should_use_claude_ai_auth(Some(&tokens.scopes))
    {
        return Ok(false);
    }

    let refreshed_tokens =
        crate::services::oauth::client::refresh_oauth_token(&tokens, None).await?;
    save_oauth_tokens_if_needed(&refreshed_tokens)?;
    Ok(true)
}

fn secure_storage_credentials_has_claude_ai_scope(
    get_env: &impl Fn(&str) -> Option<String>,
) -> bool {
    read_claude_ai_credentials_snapshot(get_env).is_some()
}

pub fn claude_ai_subscription_name(subscription_type: Option<&str>) -> &'static str {
    match subscription_type
        .map(str::trim)
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "enterprise" => "Claude Enterprise",
        "team" => "Claude Team",
        "max" => "Claude Max",
        "pro" => "Claude Pro",
        _ => "Claude API",
    }
}

/// Maps to: CC `utils/auth.ts:1714-1728` `getSubscriptionName`.
pub fn get_subscription_name() -> &'static str {
    match get_subscription_type().as_deref() {
        Some("enterprise") => "Claude Enterprise",
        Some("team") => "Claude Team",
        Some("max") => "Claude Max",
        Some("pro") => "Claude Pro",
        _ => "Claude API",
    }
}

/// Maps to: CC `utils/auth.ts:1863-1905` `getAccountInformation`.
pub fn get_account_information() -> Option<UserAccountInfo> {
    if crate::utils::model::providers::get_api_provider()
        != crate::utils::model::providers::ApiProvider::FirstParty
    {
        return None;
    }

    let auth_token_source = get_auth_token_source().source;
    let mut account_info = UserAccountInfo::default();
    if matches!(
        auth_token_source,
        AuthTokenSource::ClaudeCodeOauthToken | AuthTokenSource::ClaudeCodeOauthTokenFileDescriptor
    ) {
        account_info.token_source = auth_token_source.status_label().map(ToOwned::to_owned);
    } else if is_claude_ai_subscriber() {
        account_info.subscription = Some(get_subscription_name().to_string());
    } else {
        account_info.token_source = Some(
            auth_token_source
                .status_label()
                .unwrap_or("none")
                .to_string(),
        );
    }

    let api_key = get_anthropic_api_key_with_source(GetAnthropicApiKeyOptions::default());
    if api_key.key.is_some() {
        account_info.api_key_source = Some(api_key.source);
    }

    if matches!(auth_token_source, AuthTokenSource::ClaudeAi)
        || api_key.source == ApiKeySource::LoginManagedKey
    {
        let oauth_account = crate::utils::config::load_global_config().oauth_account;
        account_info.organization = oauth_account
            .as_ref()
            .and_then(|account| account.organization_name.clone())
            .filter(|organization| !organization.is_empty());
        account_info.email = oauth_account
            .and_then(|account| account.email_address)
            .filter(|email| !email.is_empty());
    }

    Some(account_info)
}

/// Maps to: CC `utils/auth.ts:153-210` `getAuthTokenSource`.
pub fn get_auth_token_source() -> AuthTokenSourceStatus {
    if crate::utils::env_utils::is_bare_mode() {
        if get_configured_api_key_helper().is_some() {
            return AuthTokenSourceStatus {
                source: AuthTokenSource::ApiKeyHelper,
                has_token: true,
            };
        }
        return AuthTokenSourceStatus {
            source: AuthTokenSource::None,
            has_token: false,
        };
    }

    if crate::utils::process_env::var("ANTHROPIC_AUTH_TOKEN")
        .truthy()
        .is_some()
        && !is_managed_oauth_context()
    {
        return AuthTokenSourceStatus {
            source: AuthTokenSource::AnthropicAuthToken,
            has_token: true,
        };
    }

    if crate::utils::process_env::var("CLAUDE_CODE_OAUTH_TOKEN")
        .truthy()
        .is_some()
    {
        return AuthTokenSourceStatus {
            source: AuthTokenSource::ClaudeCodeOauthToken,
            has_token: true,
        };
    }

    if get_oauth_token_from_file_descriptor().is_some() {
        return AuthTokenSourceStatus {
            source: if crate::utils::process_env::var_os("CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR")
                .as_deref()
                .truthy()
                .is_some()
            {
                AuthTokenSource::ClaudeCodeOauthTokenFileDescriptor
            } else {
                AuthTokenSource::CcrOauthTokenFile
            },
            has_token: true,
        };
    }

    if get_configured_api_key_helper().is_some() && !is_managed_oauth_context() {
        return AuthTokenSourceStatus {
            source: AuthTokenSource::ApiKeyHelper,
            has_token: true,
        };
    }

    if get_claude_ai_oauth_tokens().is_some_and(|tokens| {
        !tokens.access_token.is_empty()
            && crate::services::oauth::client::should_use_claude_ai_auth(Some(&tokens.scopes))
    }) {
        return AuthTokenSourceStatus {
            source: AuthTokenSource::ClaudeAi,
            has_token: true,
        };
    }

    AuthTokenSourceStatus {
        source: AuthTokenSource::None,
        has_token: false,
    }
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

    if crate::bootstrap::state::prefer_third_party_authentication() {
        if let Some(api_key) = api_key_env.clone() {
            return AnthropicApiKeyWithSource {
                key: Some(api_key),
                source: ApiKeySource::AnthropicApiKey,
            };
        }
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

/// Maps to: CC `utils/auth.ts:100-141` `isAnthropicAuthEnabled`.
pub fn is_anthropic_auth_enabled() -> bool {
    if crate::utils::env_utils::is_bare_mode() {
        return false;
    }

    if crate::utils::process_env::var("ANTHROPIC_UNIX_SOCKET")
        .truthy()
        .is_some()
    {
        return crate::utils::process_env::var("CLAUDE_CODE_OAUTH_TOKEN")
            .truthy()
            .is_some();
    }

    let is_3p = crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_USE_BEDROCK").as_deref(),
    ) || crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_USE_VERTEX").as_deref(),
    ) || crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_USE_FOUNDRY").as_deref(),
    );
    let has_external_auth_token = crate::utils::process_env::var("ANTHROPIC_AUTH_TOKEN")
        .truthy()
        .is_some()
        || get_configured_api_key_helper().is_some()
        || crate::utils::process_env::var("CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR")
            .truthy()
            .is_some();
    let api_key_source = get_anthropic_api_key_with_source(GetAnthropicApiKeyOptions {
        skip_retrieving_key_from_api_key_helper: true,
    })
    .source;
    let has_external_api_key = matches!(
        api_key_source,
        ApiKeySource::AnthropicApiKey | ApiKeySource::ApiKeyHelper
    );
    let should_disable_auth =
        is_3p || ((has_external_auth_token || has_external_api_key) && !is_managed_oauth_context());

    !should_disable_auth
}

/// Maps to: CC `utils/auth.ts:1611-1617` `getOauthAccountInfo`.
///
/// Gets OAuth account information when Anthropic auth is enabled. Returns
/// `None` when using external API keys or third-party services.
pub fn get_oauth_account_info() -> Option<crate::utils::config::AccountInfo> {
    if !is_anthropic_auth_enabled() {
        return None;
    }
    crate::utils::config::load_global_config().oauth_account
}

/// Maps to: CC `utils/auth.ts:1564-1570` `isClaudeAISubscriber`.
pub fn is_claude_ai_subscriber() -> bool {
    if !is_anthropic_auth_enabled() {
        return false;
    }

    get_claude_ai_oauth_tokens().is_some_and(|tokens| {
        crate::services::oauth::client::should_use_claude_ai_auth(Some(&tokens.scopes))
    })
}

/// Maps to: CC `utils/auth.ts:1662-1678` `getSubscriptionType`.
pub fn get_subscription_type() -> Option<String> {
    if crate::services::mock_rate_limits::should_use_mock_subscription() {
        return crate::services::mock_rate_limits::get_mock_subscription_type();
    }
    if !is_anthropic_auth_enabled() {
        return None;
    }
    get_claude_ai_oauth_tokens().and_then(|tokens| tokens.subscription_type)
}

/// Maps to: CC `utils/auth.ts:1702-1712` `getRateLimitTier`.
pub fn get_rate_limit_tier() -> Option<String> {
    if !is_anthropic_auth_enabled() {
        return None;
    }
    get_claude_ai_oauth_tokens().and_then(|tokens| tokens.rate_limit_tier)
}

/// Maps to: CC `utils/auth.ts:1679-1681` `isMaxSubscriber`.
pub fn is_max_subscriber() -> bool {
    get_subscription_type().as_deref() == Some("max")
}

/// Maps to: CC `utils/auth.ts:1687-1692` `isTeamPremiumSubscriber`.
pub fn is_team_premium_subscriber() -> bool {
    get_subscription_type().as_deref() == Some("team")
        && get_rate_limit_tier().as_deref() == Some("default_claude_max_5x")
}

/// Maps to: CC `utils/auth.ts:1698-1700` `isProSubscriber`.
pub fn is_pro_subscriber() -> bool {
    get_subscription_type().as_deref() == Some("pro")
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
            "cometix-auth-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn anthropic_auth_enabled_follows_official_external_source_gate() {
        let _env_guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = unique_temp_dir("auth-enabled");
        std::fs::create_dir_all(&dir).unwrap();
        let _config_home = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &dir);
        let _api_key = EnvVarGuard::unset("ANTHROPIC_API_KEY");
        let _bedrock = EnvVarGuard::unset("CLAUDE_CODE_USE_BEDROCK");
        let _vertex = EnvVarGuard::unset("CLAUDE_CODE_USE_VERTEX");
        let _foundry = EnvVarGuard::unset("CLAUDE_CODE_USE_FOUNDRY");
        let _remote = EnvVarGuard::unset("CLAUDE_CODE_REMOTE");
        let _ci = EnvVarGuard::unset("CI");
        let _node_env = EnvVarGuard::unset("NODE_ENV");

        crate::utils::config::set_test_global_config(Some(GlobalConfig::default()));
        assert!(is_anthropic_auth_enabled());

        crate::utils::process_env::set("ANTHROPIC_API_KEY", "sk-ant-test");
        assert!(is_anthropic_auth_enabled());

        let mut approved_config = GlobalConfig::default();
        approved_config.custom_api_key_responses =
            Some(crate::utils::config::CustomApiKeyResponses {
                approved: Some(vec![normalize_api_key_for_config("sk-ant-test")]),
                rejected: None,
            });
        crate::utils::config::set_test_global_config(Some(approved_config));
        assert!(!is_anthropic_auth_enabled());

        crate::utils::process_env::set("CLAUDE_CODE_USE_BEDROCK", "1");
        assert!(!is_anthropic_auth_enabled());
        crate::utils::process_env::remove("CLAUDE_CODE_USE_BEDROCK");

        crate::utils::process_env::set("CLAUDE_CODE_REMOTE", "1");
        assert!(is_anthropic_auth_enabled());

        crate::utils::config::set_test_global_config(None);
        let _ = std::fs::remove_dir_all(dir);
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

        let source = get_auth_token_source();
        assert_eq!(source.source, AuthTokenSource::ApiKeyHelper);

        drop(_flag_guard);
        let no_flag_source = get_auth_token_source();
        assert_eq!(no_flag_source.source, AuthTokenSource::None);
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
            "echo cometix-helper-key"
        } else {
            "printf cometix-helper-key"
        };
        std::fs::write(
            dir.join("settings.json"),
            serde_json::json!({"apiKeyHelper": command}).to_string(),
        )
        .unwrap();
        assert_eq!(
            get_api_key_from_api_key_helper(false).as_deref(),
            Some("cometix-helper-key")
        );
        assert_eq!(
            get_api_key_from_api_key_helper_cached().as_deref(),
            Some("cometix-helper-key")
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
            "cometix-auth-fd-{}-{}.txt",
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

    #[cfg(unix)]
    #[test]
    fn oauth_file_descriptor_requires_readable_token_like_official() {
        use std::os::fd::AsRawFd;

        let _env_guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        crate::utils::config::set_test_global_config(Some(GlobalConfig::default()));
        // This case asserts the NOT-a-subscriber outcome, so it has to own that
        // premise: `is_claude_ai_subscriber` falls back to `.credentials.json`
        // under `get_claude_config_home_dir()`, and the test harness seeds a logged-in
        // identity there. Point the config home at an empty scratch dir for the
        // duration instead of relying on the developer being logged out.
        let scratch_home = std::env::temp_dir().join(format!(
            "cometix-oauth-fd-home-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&scratch_home).unwrap();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &scratch_home);
        {
            let _missing_guard =
                EnvVarGuard::set("CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR", "999999");
            crate::bootstrap::state::reset_auth_file_descriptor_caches_for_testing();
            assert!(!is_claude_ai_subscriber());
        }
        let _ = std::fs::remove_dir_all(&scratch_home);

        let path = std::env::temp_dir().join(format!(
            "cometix-oauth-fd-{}-{}.txt",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, " oauth-token\n").unwrap();
        let file = std::fs::File::open(&path).unwrap();
        let fd = file.as_raw_fd().to_string();
        let _fd_guard = EnvVarGuard::set("CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR", &fd);
        crate::bootstrap::state::reset_auth_file_descriptor_caches_for_testing();

        assert!(is_claude_ai_subscriber());

        crate::bootstrap::state::reset_auth_file_descriptor_caches_for_testing();
        crate::utils::config::set_test_global_config(None);
        drop(file);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn plaintext_credentials_file_is_readonly_subscriber_source() {
        let _env_guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = std::env::temp_dir().join(format!(
            "cometix-credentials-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let credentials_path = dir.join(".credentials.json");
        std::fs::write(
            &credentials_path,
            serde_json::json!({
                "claudeAiOauth": {
                    "accessToken": "oauth-token",
                    "scopes": ["user:inference"],
                    "subscriptionType": "team"
                }
            })
            .to_string(),
        )
        .unwrap();

        let _config_guard = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &dir);
        crate::utils::config::set_test_global_config(Some(GlobalConfig::default()));
        let get_env = |key: &str| crate::utils::process_env::var(key);
        assert!(is_claude_ai_subscriber());
        assert_eq!(
            read_claude_ai_credentials_snapshot(&get_env)
                .and_then(|snapshot| snapshot.subscription_type),
            Some("team".to_string())
        );
        assert_eq!(
            get_auth_token_source(),
            AuthTokenSourceStatus {
                source: AuthTokenSource::ClaudeAi,
                has_token: true,
            }
        );
        assert!(credentials_path.exists());

        crate::utils::config::set_test_global_config(None);
        let _ = std::fs::remove_file(credentials_path);
        let _ = std::fs::remove_dir(dir);
    }

    #[test]
    fn plaintext_credentials_scope_parser_requires_access_token_and_inference_scope() {
        let valid = serde_json::json!({
            "claudeAiOauth": {
                "accessToken": "oauth-token",
                "refreshToken": "refresh-token",
                "expiresAt": 123456_u64,
                "scopes": ["user:inference", "user:profile"],
                "subscriptionType": "pro",
                "rateLimitTier": "pro"
            }
        });
        assert_eq!(
            credentials_json_snapshot(&valid).and_then(|snapshot| snapshot.subscription_type),
            Some("pro".to_string())
        );
        let tokens = credentials_json_tokens(&valid).expect("tokens should parse");
        assert_eq!(tokens.access_token, "oauth-token");
        assert_eq!(tokens.refresh_token.as_deref(), Some("refresh-token"));

        let raw_bytes = serde_json::json!({
            "claudeAiOauth": {
                "accessToken": "  access-token\n",
                "refreshToken": "\trefresh-token  ",
                "scopes": ["user:inference"]
            }
        });
        let raw_tokens = credentials_json_tokens(&raw_bytes).expect("raw tokens should parse");
        assert_eq!(raw_tokens.access_token, "  access-token\n");
        assert_eq!(
            raw_tokens.refresh_token.as_deref(),
            Some("\trefresh-token  ")
        );
        assert_eq!(tokens.expires_at, Some(123456));
        assert_eq!(tokens.rate_limit_tier.as_deref(), Some("pro"));

        let api_only = serde_json::json!({
            "claudeAiOauth": {
                "accessToken": "oauth-token",
                "scopes": ["user:profile"]
            }
        });
        assert!(credentials_json_snapshot(&api_only).is_none());

        let missing_token = serde_json::json!({
            "claudeAiOauth": {
                "scopes": ["user:inference"]
            }
        });
        assert!(credentials_json_snapshot(&missing_token).is_none());
    }

    #[test]
    fn save_oauth_tokens_if_needed_prepares_official_merge_before_the_outlet_gate() {
        let _env_guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = unique_temp_dir("prepare-oauth-save");
        std::fs::create_dir_all(&dir).unwrap();
        let _config_guard = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &dir);
        std::fs::write(
            dir.join(".credentials.json"),
            serde_json::json!({
                "claudeAiOauth": {
                    "accessToken": "old-access",
                    "refreshToken": "old-refresh",
                    "expiresAt": 1,
                    "scopes": ["user:inference"],
                    "subscriptionType": "pro",
                    "rateLimitTier": "default_claude_pro"
                },
                "mcpOAuth": {"keep": {"accessToken": "mcp-token"}}
            })
            .to_string(),
        )
        .unwrap();
        *LAST_PREPARED_OAUTH_CREDENTIALS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;

        save_oauth_tokens_if_needed(&ClaudeAiOAuthTokensSnapshot {
            access_token: "new-access".to_string(),
            refresh_token: Some("new-refresh".to_string()),
            expires_at: Some(1_900_000_000_000),
            scopes: vec!["user:inference".to_string(), "user:profile".to_string()],
            subscription_type: None,
            rate_limit_tier: None,
        })
        .expect_err("the final credential update outlet is default-closed");
        let prepared = LAST_PREPARED_OAUTH_CREDENTIALS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .expect("source-shaped merge must finish before the gate");

        assert_eq!(prepared["claudeAiOauth"]["accessToken"], "new-access");
        assert_eq!(prepared["claudeAiOauth"]["refreshToken"], "new-refresh");
        assert_eq!(
            prepared["claudeAiOauth"]["expiresAt"].as_u64(),
            Some(1_900_000_000_000)
        );
        assert_eq!(prepared["claudeAiOauth"]["subscriptionType"], "pro");
        assert_eq!(
            prepared["claudeAiOauth"]["rateLimitTier"],
            "default_claude_pro"
        );
        assert_eq!(
            prepared["mcpOAuth"]["keep"]["accessToken"].as_str(),
            Some("mcp-token")
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn save_oauth_tokens_if_needed_skips_non_claude_and_inference_only_tokens_like_official() {
        let _env_guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = unique_temp_dir("skip-oauth-save");
        std::fs::create_dir_all(&dir).unwrap();
        let _config_guard = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &dir);
        let credentials_path = dir.join(".credentials.json");
        let original = serde_json::json!({
            "claudeAiOauth": {
                "accessToken": "old-access",
                "refreshToken": "old-refresh",
                "expiresAt": 10,
                "scopes": ["user:inference"]
            }
        });
        std::fs::write(&credentials_path, original.to_string()).unwrap();

        save_oauth_tokens_if_needed(&ClaudeAiOAuthTokensSnapshot {
            access_token: "api-only".to_string(),
            refresh_token: Some("refresh".to_string()),
            expires_at: Some(20),
            scopes: vec!["user:profile".to_string()],
            subscription_type: None,
            rate_limit_tier: None,
        })
        .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(
                &std::fs::read_to_string(&credentials_path).unwrap()
            )
            .unwrap(),
            original
        );

        save_oauth_tokens_if_needed(&ClaudeAiOAuthTokensSnapshot {
            access_token: "inference-only".to_string(),
            refresh_token: None,
            expires_at: None,
            scopes: vec!["user:inference".to_string()],
            subscription_type: None,
            rate_limit_tier: None,
        })
        .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(
                &std::fs::read_to_string(&credentials_path).unwrap()
            )
            .unwrap(),
            original
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn blocked_oauth_save_preserves_credential_bytes_and_request_shape_caches() {
        let _env_guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = unique_temp_dir("blocked-oauth-save");
        std::fs::create_dir_all(&dir).unwrap();
        let _config_guard = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &dir);
        let _betas_guard = EnvVarGuard::set("ANTHROPIC_BETAS", "oauth-gate-cache-sentinel");
        let credentials_path = dir.join(".credentials.json");
        let original = br#"{"claudeAiOauth":{"accessToken":"old-access","refreshToken":"old-refresh","expiresAt":1,"scopes":["user:inference"]}}"#;
        std::fs::write(&credentials_path, original).unwrap();

        crate::utils::betas::clear_betas_caches();
        let raw_model = format!("claude-sonnet-4-6-oauth-gate-{}", std::process::id());
        assert!(
            crate::utils::betas::get_all_model_betas(&raw_model)
                .iter()
                .any(|beta| beta == "oauth-gate-cache-sentinel")
        );
        crate::utils::process_env::set("ANTHROPIC_BETAS", "");
        crate::utils::tool_schema_cache::clear_tool_schema_cache();
        crate::utils::tool_schema_cache::get_tool_schema_cache().insert(
            "Read".to_string(),
            crate::utils::tool_schema_cache::CachedSchema {
                name: "Read".to_string(),
                description: "Read a file".to_string(),
                input_schema: serde_json::json!({"type": "object"}),
                strict: Some(true),
                eager_input_streaming: None,
            },
        );

        let error = save_oauth_tokens_if_needed(&ClaudeAiOAuthTokensSnapshot {
            access_token: "new-access".to_string(),
            refresh_token: Some("new-refresh".to_string()),
            expires_at: Some(1_900_000_000_000),
            scopes: vec!["user:inference".to_string()],
            subscription_type: None,
            rate_limit_tier: None,
        })
        .expect_err("OAuth credential persistence must be default-closed");
        assert!(
            error
                .downcast_ref::<crate::constants::oauth::OAuthCredentialSideEffectsUnavailable>()
                .is_some()
        );
        assert_eq!(std::fs::read(&credentials_path).unwrap(), original);
        assert!(
            crate::utils::betas::get_all_model_betas(&raw_model)
                .iter()
                .any(|beta| beta == "oauth-gate-cache-sentinel")
        );
        assert!(crate::utils::tool_schema_cache::get_tool_schema_cache().contains_key("Read"));

        crate::utils::betas::clear_betas_caches();
        crate::utils::tool_schema_cache::clear_tool_schema_cache();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn check_and_refresh_oauth_token_if_needed_matches_official_no_refresh_gates() {
        let _env_guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = unique_temp_dir("oauth-check-gates");
        std::fs::create_dir_all(&dir).unwrap();
        let _config_guard = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &dir);
        let credentials_path = dir.join(".credentials.json");
        std::fs::write(
            &credentials_path,
            serde_json::json!({
                "claudeAiOauth": {
                    "accessToken": "fresh-access",
                    "refreshToken": "refresh-token",
                    "expiresAt": u64::MAX,
                    "scopes": ["user:inference"]
                }
            })
            .to_string(),
        )
        .unwrap();

        let refreshed = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(check_and_refresh_oauth_token_if_needed(false))
            .unwrap();
        assert!(!refreshed);

        std::fs::write(
            &credentials_path,
            serde_json::json!({
                "claudeAiOauth": {
                    "accessToken": "expired-api-access",
                    "refreshToken": "refresh-token",
                    "expiresAt": 0,
                    "scopes": ["user:profile"]
                }
            })
            .to_string(),
        )
        .unwrap();
        let refreshed = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(check_and_refresh_oauth_token_if_needed(false))
            .unwrap();
        assert!(!refreshed);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn handle_oauth_401_error_matches_official_failed_token_comparison() {
        let _env_guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = unique_temp_dir("oauth-401-compare");
        std::fs::create_dir_all(&dir).unwrap();
        let _config_guard = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &dir);
        let credentials_path = dir.join(".credentials.json");
        std::fs::write(
            &credentials_path,
            serde_json::json!({
                "claudeAiOauth": {
                    "accessToken": "new-access",
                    "refreshToken": "refresh-token",
                    "expiresAt": 0,
                    "scopes": ["user:inference"]
                }
            })
            .to_string(),
        )
        .unwrap();
        let recovered = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(handle_oauth_401_error("failed-access"))
            .unwrap();
        assert!(recovered);

        std::fs::write(
            &credentials_path,
            serde_json::json!({
                "claudeAiOauth": {
                    "accessToken": "new-access",
                    "expiresAt": 0,
                    "scopes": ["user:inference"]
                }
            })
            .to_string(),
        )
        .unwrap();
        let recovered = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(handle_oauth_401_error("new-access"))
            .unwrap();
        assert!(!recovered);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn claude_ai_oauth_tokens_read_path_prefers_env_inference_token_like_official() {
        let _env_guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _oauth = EnvVarGuard::set("CLAUDE_CODE_OAUTH_TOKEN", "  env-oauth-token\n");
        let tokens =
            get_claude_ai_oauth_tokens().expect("env token should produce inference-only snapshot");
        assert_eq!(tokens.access_token, "  env-oauth-token\n");
        assert_eq!(tokens.refresh_token, None);
        assert_eq!(tokens.expires_at, None);
        assert_eq!(tokens.scopes, vec!["user:inference".to_string()]);
    }

    #[test]
    fn bare_mode_disables_claude_ai_oauth_token_read_path_like_official() {
        let _env_guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _simple = EnvVarGuard::set("CLAUDE_CODE_SIMPLE", "1");
        let _oauth = EnvVarGuard::set("CLAUDE_CODE_OAUTH_TOKEN", "env-oauth-token");
        assert!(get_claude_ai_oauth_tokens().is_none());
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

        assert!(!is_anthropic_auth_enabled());

        let no_key = get_anthropic_api_key_with_source(GetAnthropicApiKeyOptions::default());
        assert_eq!(no_key.source, ApiKeySource::None);
        assert_eq!(no_key.key, None);

        crate::utils::process_env::set("ANTHROPIC_API_KEY", "sk-ant-test");
        let env_key = get_anthropic_api_key_with_source(GetAnthropicApiKeyOptions::default());
        assert_eq!(env_key.source, ApiKeySource::AnthropicApiKey);
        assert_eq!(env_key.key.as_deref(), Some("sk-ant-test"));
        crate::utils::config::set_test_global_config(None);
    }

    #[test]
    fn anthropic_unix_socket_gate_matches_official_remote_proxy_auth() {
        let _env_guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _socket = EnvVarGuard::set("ANTHROPIC_UNIX_SOCKET", "/tmp/claude.sock");
        let _oauth = EnvVarGuard::unset("CLAUDE_CODE_OAUTH_TOKEN");

        assert!(!is_anthropic_auth_enabled());
        crate::utils::process_env::set("CLAUDE_CODE_OAUTH_TOKEN", "oauth-placeholder");
        assert!(is_anthropic_auth_enabled());
    }

    #[test]
    fn claude_ai_subscriber_does_not_treat_account_metadata_as_a_token() {
        let _env_guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = unique_temp_dir("subscriber-account-metadata");
        std::fs::create_dir_all(&dir).unwrap();
        let _config_home = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &dir);
        let _oauth = EnvVarGuard::set("CLAUDE_CODE_OAUTH_TOKEN", "");
        crate::bootstrap::state::reset_auth_file_descriptor_caches_for_testing();
        let mut config = GlobalConfig::default();
        crate::utils::config::set_test_global_config(Some(config.clone()));
        assert!(!is_claude_ai_subscriber());
        config.oauth_account = Some(crate::utils::config::AccountInfo::default());
        crate::utils::config::set_test_global_config(Some(config));
        assert!(!is_claude_ai_subscriber());
        crate::utils::config::set_test_global_config(None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn subscription_display_name_matches_official_get_subscription_name() {
        let _env_guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = unique_temp_dir("subscription-name");
        std::fs::create_dir_all(&dir).unwrap();
        let _config_home = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &dir);
        let _oauth = EnvVarGuard::unset("CLAUDE_CODE_OAUTH_TOKEN");
        crate::utils::config::set_test_global_config(Some(GlobalConfig::default()));

        assert_eq!(claude_ai_subscription_name(Some("max")), "Claude Max");
        assert_eq!(claude_ai_subscription_name(Some("MAX")), "Claude Max");
        assert_eq!(claude_ai_subscription_name(Some("pro")), "Claude Pro");
        assert_eq!(claude_ai_subscription_name(Some("team")), "Claude Team");
        assert_eq!(
            claude_ai_subscription_name(Some("enterprise")),
            "Claude Enterprise"
        );
        assert_eq!(claude_ai_subscription_name(None), "Claude API");
        // Payment-rail billingType values are not subscription types.
        assert_eq!(
            claude_ai_subscription_name(Some("stripe_subscription")),
            "Claude API"
        );
        assert_eq!(get_subscription_name(), "Claude API");
        crate::utils::config::set_test_global_config(None);
        let _ = std::fs::remove_dir_all(dir);
    }
}
