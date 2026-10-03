//! Maps to: CC `utils/plugins/gitAvailability.ts:1-69`.

use futures::FutureExt;
use futures::future::{BoxFuture, Shared};
use std::future::Future;
use std::sync::{LazyLock, Mutex};

/// Maps to: CC `utils/plugins/gitAvailability.ts:42-44#checkGitAvailable`.
/// The single undefined memo key owns a shared pending result, not just a bool.
/// PORTING A6/A7: replacing or clearing the slot never cancels/re-publishes an
/// earlier result; each retained future still observes its original result.
static GIT_AVAILABILITY_CACHE: LazyLock<Mutex<Option<Shared<BoxFuture<'static, bool>>>>> =
    LazyLock::new(|| Mutex::new(None));

/// Maps to: CC `utils/plugins/gitAvailability.ts:21-27#isCommandAvailable`.
/// `which` owns PATH lookup and folds lookup failures into absence. Evaluate it
/// at invocation, before the returned future's first await, like the Bun source.
fn is_command_available(command: &str) -> impl Future<Output = bool> + Send + 'static {
    let located = crate::utils::which::which(command);
    async move { located.await.is_some() }
}

/// Maps to: CC `utils/plugins/gitAvailability.ts:42-44#checkGitAvailable`.
pub fn check_git_available() -> Shared<BoxFuture<'static, bool>> {
    let mut cache = GIT_AVAILABILITY_CACHE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    cache
        .get_or_insert_with(|| is_command_available("git").boxed().shared())
        .clone()
}

/// Maps to: CC `utils/plugins/gitAvailability.ts:59-61#markGitUnavailable`.
pub fn mark_git_unavailable() {
    *GIT_AVAILABILITY_CACHE
        .lock()
        .unwrap_or_else(|error| error.into_inner()) =
        Some(futures::future::ready(false).boxed().shared());
}

/// Maps to: CC `utils/plugins/gitAvailability.ts:67-69#clearGitAvailabilityCache`.
pub fn clear_git_availability_cache() {
    *GIT_AVAILABILITY_CACHE
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = None;
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::utils::test_env::TEST_ENV_LOCK;

    // Test-only dependency fixture: exercise the real cache API without requiring
    // a particular host PATH or executing any git binary.
    pub(crate) fn set_cached_git_availability(available: bool) {
        *GIT_AVAILABILITY_CACHE.lock().unwrap() =
            Some(futures::future::ready(available).boxed().shared());
    }

    #[test]
    fn git_availability_matches_official_shared_pending_mark_and_clear() {
        // CC :42-69; real Bun git-oracle.json captures both pending races.
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        clear_git_availability_cache();
        let (send_old, receive_old) = futures::channel::oneshot::channel::<bool>();
        *GIT_AVAILABILITY_CACHE.lock().unwrap() =
            Some(async move { receive_old.await.unwrap() }.boxed().shared());
        let old = check_git_available();
        let same = check_git_available();
        assert!(old.ptr_eq(&same));
        mark_git_unavailable();
        let marked = check_git_available();
        assert!(!old.ptr_eq(&marked));
        assert!(!futures::executor::block_on(marked));
        send_old.send(true).unwrap();
        assert!(futures::executor::block_on(old));
        assert!(futures::executor::block_on(same));
        assert!(!futures::executor::block_on(check_git_available()));

        clear_git_availability_cache();
        assert!(GIT_AVAILABILITY_CACHE.lock().unwrap().is_none());
        let (send_old, receive_old) = futures::channel::oneshot::channel::<bool>();
        *GIT_AVAILABILITY_CACHE.lock().unwrap() =
            Some(async move { receive_old.await.unwrap() }.boxed().shared());
        let old = check_git_available();
        clear_git_availability_cache();
        set_cached_git_availability(false);
        let fresh = check_git_available();
        assert!(!old.ptr_eq(&fresh));
        assert!(!futures::executor::block_on(fresh));
        send_old.send(true).unwrap();
        assert!(futures::executor::block_on(old));
        assert!(!futures::executor::block_on(check_git_available()));
        clear_git_availability_cache();
        mark_git_unavailable();
        assert!(!futures::executor::block_on(check_git_available()));
        clear_git_availability_cache();
    }

    #[test]
    fn command_availability_matches_official_lookup_without_execution() {
        // CC :21-27 delegates to which; a discoverable executable is never run.
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        struct RemoveDirectory(std::path::PathBuf);
        impl Drop for RemoveDirectory {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let dir = RemoveDirectory(
            std::env::temp_dir().join(format!("git-availability-{}", uuid::Uuid::new_v4())),
        );
        std::fs::create_dir_all(&dir.0).unwrap();
        let executable = dir.0.join("discovery-only");
        let marker = dir.0.join("must-not-exist");
        std::fs::write(
            &executable,
            format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert!(futures::executor::block_on(is_command_available(
                executable.to_str().unwrap()
            )));
        }
        assert!(!marker.exists());
        assert!(!futures::executor::block_on(is_command_available(
            dir.0.join("missing").to_str().unwrap()
        )));
        assert!(!futures::executor::block_on(is_command_available(
            dir.0.to_str().unwrap()
        )));
    }
}
