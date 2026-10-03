//! Shared plugin-recommendation install helper.
//!
//! Maps to: CC `hooks/usePluginRecommendationBase.tsx`
//! `installPluginAndNotify(...)`.
//!
//! CC starts an async promise from the React callback. The Rust TUI adapts that
//! promise to a tracked dedicated worker, using the existing sync/async bridge.
//! Cleanup joins workers even after the interactive runtime has been dropped;
//! filesystem materialization never occupies the shared TUI runtime.

use std::sync::{LazyLock, Mutex, Once};

static INSTALL_PLUGIN_AND_NOTIFY_WORKERS: LazyLock<Mutex<Vec<std::thread::JoinHandle<()>>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));
static REGISTER_INSTALL_CLEANUP: Once = Once::new();

fn drain_install_plugin_and_notify_workers() {
    let handles = INSTALL_PLUGIN_AND_NOTIFY_WORKERS
        .lock()
        .map(|mut handles| std::mem::take(&mut *handles))
        .unwrap_or_default();
    for handle in handles {
        let _ = handle.join();
    }
}

fn ensure_install_cleanup_registered() {
    REGISTER_INSTALL_CLEANUP.call_once(|| {
        crate::utils::cleanup_registry::register_cleanup(|| async {
            drain_install_plugin_and_notify_workers();
        });
    });
}

/// Maps to CC `installPluginAndNotify(pluginId, pluginName, keyPrefix, ...)`.
pub fn install_plugin_and_notify<F, Fut>(
    plugin_id: String,
    plugin_name: String,
    key_prefix: &'static str,
    app_store: crate::state::store::AppStore,
    install: F,
) -> std::io::Result<()>
where
    F: FnOnce(crate::utils::plugins::marketplace_manager::MarketplacePluginMetadata) -> Fut
        + Send
        + 'static,
    Fut: std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
{
    ensure_install_cleanup_registered();
    let mut workers = INSTALL_PLUGIN_AND_NOTIFY_WORKERS
        .lock()
        .map_err(|_| std::io::Error::other("plugin install worker registry is unavailable"))?;
    let worker = std::thread::Builder::new()
        .name("cometix-plugin-install-notify".to_string())
        .spawn(move || {
            let result = crate::utils::process_runtime::block_on_from_sync(async move {
                let plugin_data =
                    crate::utils::plugins::marketplace_manager::get_plugin_by_id(&plugin_id)
                        .await
                        .ok_or_else(|| {
                            anyhow::anyhow!("Plugin {plugin_id} not found in marketplace")
                        })?;
                install(plugin_data).await
            })
            .unwrap_or_else(|| {
                Err(anyhow::anyhow!(
                    "plugin installation runtime could not be created"
                ))
            });
            let mut notifications =
                crate::context::notifications::NotificationsWriter::new(app_store);
            match result {
                Ok(_) => notifications.add_notification(
                    crate::context::notifications::Notification::text(
                        format!("{key_prefix}-installed"),
                        format!("✓ {plugin_name} installed · restart to apply"),
                        crate::context::notifications::NotificationPriority::Immediate,
                    )
                    .with_color(crate::context::notifications::NotificationColor::Success)
                    .with_timeout_ms(5_000),
                ),
                Err(error) => {
                    // CC installPluginAndNotify catches and logs before notifying.
                    crate::utils::log::log_error(crate::utils::log::LogError::new(
                        error.to_string(),
                    ));
                    notifications.add_notification(
                        crate::context::notifications::Notification::text(
                            format!("{key_prefix}-install-failed"),
                            format!("Failed to install {plugin_name}"),
                            crate::context::notifications::NotificationPriority::Immediate,
                        )
                        .with_color(crate::context::notifications::NotificationColor::Error)
                        .with_timeout_ms(5_000),
                    );
                }
            }
        })?;
    workers.push(worker);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::TEST_ENV_LOCK;

    #[test]
    fn graceful_shutdown_drain_joins_recommendation_install_workers() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        drain_install_plugin_and_notify_workers();
        ensure_install_cleanup_registered();
        let completed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_completed = std::sync::Arc::clone(&completed);
        let parent = tokio::runtime::Runtime::new().unwrap();
        let (release, wait) = tokio::sync::oneshot::channel::<()>();
        let (started, ready) = std::sync::mpsc::channel();
        parent.block_on(async {
            let worker = std::thread::spawn(move || {
                crate::utils::process_runtime::block_on_from_sync(async move {
                    started.send(()).unwrap();
                    wait.await.unwrap();
                    // Exercise the async filesystem driver after the parent is gone,
                    // as installation's getGitCommitSha does before registration.
                    tokio::fs::metadata(env!("CARGO_MANIFEST_DIR"))
                        .await
                        .unwrap();
                    worker_completed.store(true, std::sync::atomic::Ordering::Release);
                })
                .expect("worker runtime");
            });
            INSTALL_PLUGIN_AND_NOTIFY_WORKERS
                .lock()
                .unwrap()
                .push(worker);
        });
        ready
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        drop(parent);
        assert!(!completed.load(std::sync::atomic::Ordering::Acquire));
        release.send(()).unwrap();
        drain_install_plugin_and_notify_workers();
        assert!(completed.load(std::sync::atomic::Ordering::Acquire));
    }
}
