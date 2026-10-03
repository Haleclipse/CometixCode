//! Maps to: CC `utils/secureStorage/keychainPrefetch.ts`.
//!
//! macOS keychain reads are started off-thread during process startup and
//! joined before settings/auth initialization. Completed values, including a
//! confirmed missing entry, are cached so render-time auth consumers never
//! launch `security`. A timed-out prefetch is deliberately not cached, matching
//! CC's fallback to the ordinary synchronous reader.

use std::sync::{Condvar, LazyLock, Mutex};

#[cfg(all(target_os = "macos", not(test)))]
const KEYCHAIN_PREFETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum PrefetchStatus {
    #[default]
    NotStarted,
    Running,
    Complete,
}

#[derive(Default)]
struct PrefetchState {
    status: PrefetchStatus,
}

static PREFETCH_STATE: LazyLock<(Mutex<PrefetchState>, Condvar)> =
    LazyLock::new(|| (Mutex::new(PrefetchState::default()), Condvar::new()));

/// Presence distinguishes "not completed" from "completed with no key".
static LEGACY_API_KEY_PREFETCH: LazyLock<Mutex<Option<Option<String>>>> =
    LazyLock::new(|| Mutex::new(None));

/// Maps to: CC `utils/secureStorage/keychainPrefetch.ts:69-91`
/// `startKeychainPrefetch`.
///
/// Rust threads are the L1 transport equivalent of the two non-blocking
/// `execFile('security', ...)` promises. The caller must later invoke
/// [`ensure_keychain_prefetch_completed`] before auth initialization.
#[cfg(all(target_os = "macos", not(test)))]
pub fn start_keychain_prefetch() {
    if crate::utils::env_utils::is_bare_mode() {
        mark_complete();
        return;
    }

    let Ok(oauth_service) =
        super::mac_os_keychain_helpers::get_mac_os_keychain_storage_service_name(
            super::mac_os_keychain_helpers::CREDENTIALS_SERVICE_SUFFIX,
        )
    else {
        mark_complete();
        return;
    };
    let Ok(legacy_service) =
        super::mac_os_keychain_helpers::get_mac_os_keychain_storage_service_name("")
    else {
        mark_complete();
        return;
    };

    {
        let (state, _) = &*PREFETCH_STATE;
        let mut state = state.lock().expect("keychain prefetch lock poisoned");
        if state.status != PrefetchStatus::NotStarted {
            return;
        }
        state.status = PrefetchStatus::Running;
    }

    let spawn = std::thread::Builder::new()
        .name("cometix-keychain-prefetch".to_string())
        .spawn(move || {
            let oauth_service_for_read = oauth_service.clone();
            let oauth = std::thread::spawn(move || spawn_security(&oauth_service_for_read));

            let legacy_service_for_read = legacy_service.clone();
            let legacy = std::thread::spawn(move || spawn_security(&legacy_service_for_read));

            let oauth = oauth.join().unwrap_or(SpawnResult::TimedOut);
            let legacy = legacy.join().unwrap_or(SpawnResult::TimedOut);

            if let SpawnResult::Completed(value) = oauth {
                super::mac_os_keychain_helpers::prime_keychain_cache_from_prefetch(value);
            }
            if let SpawnResult::Completed(value) = legacy {
                *LEGACY_API_KEY_PREFETCH
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(value);
            }

            let (state, wake) = &*PREFETCH_STATE;
            let mut state = state.lock().expect("keychain prefetch lock poisoned");
            state.status = PrefetchStatus::Complete;
            wake.notify_all();
        });

    if spawn.is_err() {
        mark_complete();
    }
}

/// Maps to: CC `utils/secureStorage/keychainPrefetch.ts:69-91`
/// `startKeychainPrefetch` non-darwin no-op.
#[cfg(any(not(target_os = "macos"), test))]
pub fn start_keychain_prefetch() {
    #[cfg(test)]
    crate::utils::auth::record_auth_io(crate::utils::auth::AuthIoOperation::KeychainSubprocess);
    mark_complete();
}

/// Maps to: CC `utils/secureStorage/keychainPrefetch.ts:96-98`
/// `ensureKeychainPrefetchCompleted`.
///
/// Waiting occurs during process/bootstrap setup, before the retained TUI root
/// is mounted. No render/update/draw path calls this function.
pub fn ensure_keychain_prefetch_completed() {
    let (state, wake) = &*PREFETCH_STATE;
    let mut state = state.lock().expect("keychain prefetch lock poisoned");
    while state.status == PrefetchStatus::Running {
        state = wake
            .wait(state)
            .expect("keychain prefetch lock poisoned while waiting");
    }
}

/// Maps to: CC `utils/secureStorage/keychainPrefetch.ts:105-109`
/// `getLegacyApiKeyPrefetchResult`.
pub(crate) fn get_legacy_api_key_prefetch_result() -> Option<Option<String>> {
    LEGACY_API_KEY_PREFETCH
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// Maps to: CC `utils/secureStorage/keychainPrefetch.ts:115-117`
/// `clearLegacyApiKeyPrefetch`.
pub(crate) fn clear_legacy_api_key_prefetch() {
    *LEGACY_API_KEY_PREFETCH
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
}

fn mark_complete() {
    let (state, wake) = &*PREFETCH_STATE;
    let mut state = state.lock().expect("keychain prefetch lock poisoned");
    state.status = PrefetchStatus::Complete;
    wake.notify_all();
}

#[cfg(all(target_os = "macos", not(test)))]
enum SpawnResult {
    Completed(Option<String>),
    TimedOut,
}

/// Maps to: CC `utils/secureStorage/keychainPrefetch.ts:50-63` `spawnSecurity`.
#[cfg(all(target_os = "macos", not(test)))]
fn spawn_security(service_name: &str) -> SpawnResult {
    crate::utils::auth::record_auth_io(crate::utils::auth::AuthIoOperation::KeychainSubprocess);
    let username = super::mac_os_keychain_helpers::get_username();
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::time::Instant;

    let mut security = Command::new("security");
    // CC `execFile('security', ...)` inherits process.env; the carrier is its counterpart.
    crate::utils::subprocess_env::apply_process_env_std(&mut security);
    let mut child = match security
        .args([
            "find-generic-password",
            "-a",
            &username,
            "-w",
            "-s",
            service_name,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return SpawnResult::Completed(None),
    };

    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut stdout = String::new();
                if let Some(mut pipe) = child.stdout.take() {
                    let _ = pipe.read_to_string(&mut stdout);
                }
                let value = status
                    .success()
                    .then(|| stdout.trim().to_string())
                    .filter(|value| !value.is_empty());
                return SpawnResult::Completed(value);
            }
            Ok(None) if started.elapsed() < KEYCHAIN_PREFETCH_TIMEOUT => {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return SpawnResult::TimedOut;
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return SpawnResult::Completed(None);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completed_missing_legacy_key_is_distinct_from_incomplete_prefetch() {
        clear_legacy_api_key_prefetch();
        assert_eq!(get_legacy_api_key_prefetch_result(), None);
        *LEGACY_API_KEY_PREFETCH
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(None);
        assert_eq!(get_legacy_api_key_prefetch_result(), Some(None));
        clear_legacy_api_key_prefetch();
    }

    #[test]
    fn oauth_and_legacy_prefetch_results_keep_source_owned_independent_state() {
        super::super::mac_os_keychain_helpers::clear_keychain_cache();
        clear_legacy_api_key_prefetch();
        super::super::mac_os_keychain_helpers::prime_keychain_cache_from_prefetch(Some(
            r#"{"claudeAiOauth":{"accessToken":"oauth"}}"#.to_string(),
        ));
        *LEGACY_API_KEY_PREFETCH
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Some("legacy".to_string()));

        let oauth = super::super::mac_os_keychain_helpers::KEYCHAIN_CACHE_STATE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .cache
            .data
            .clone();
        assert_eq!(oauth.unwrap()["claudeAiOauth"]["accessToken"], "oauth");
        assert_eq!(
            get_legacy_api_key_prefetch_result(),
            Some(Some("legacy".to_string()))
        );

        super::super::mac_os_keychain_helpers::clear_keychain_cache();
        clear_legacy_api_key_prefetch();
    }
}
