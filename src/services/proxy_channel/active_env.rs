//! Environment variable application and restoration for active proxy channels.

use tracing::info;

use super::config::{get_active_channel_id, load_channel_config};

/// Known proxy-related environment variable keys that may be overridden by an active channel.
pub const PROXY_MANAGED_ENV_KEYS: &[&str] = &[
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_DEFAULT_SONNET_MODEL",
    "ANTHROPIC_DEFAULT_SONNET_1M_MODEL",
    "ANTHROPIC_DEFAULT_OPUS_MODEL",
    "ANTHROPIC_DEFAULT_OPUS_1M_MODEL",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
];

/// Applies the active proxy channel's environment variable overrides if a channel is active.
/// Call this after `managed_env::apply_safe_config_environment_variables()`.
pub fn apply_active_channel_env() {
    let Some(active_id) = get_active_channel_id() else {
        return;
    };

    let Some(config) = load_channel_config(&active_id) else {
        info!("Active proxy channel '{active_id}' has no configuration file; skipping env overrides.");
        return;
    };

    if config.env.is_empty() {
        return;
    }

    info!("Applying proxy channel '{active_id}' environment variable overrides");
    for (key, value) in &config.env {
        crate::utils::process_env::set(key, value);
        // SAFETY: Synchronization into the OS environment so callers reading `std::env::var`
        // directly (e.g. client.rs and truthy_env_var) observe the overridden values on all platforms.
        unsafe {
            std::env::set_var(key, value);
        }
    }

    // Non-blocking background model catalog refresh on startup / reactivation if runtime is active
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        if let Some(channel) = crate::services::proxy_channel::get_channel(&active_id) {
            if let Some(base_url) = config.env.get("ANTHROPIC_BASE_URL") {
                if let Ok(ep) = channel.normalize_endpoints(base_url) {
                    let key = config
                        .env
                        .get("ANTHROPIC_API_KEY")
                        .cloned()
                        .unwrap_or_default();
                    let act_id = active_id.clone();
                    handle.spawn(async move {
                        if let Ok(models) = channel.fetch_models(&ep, &key).await {
                            let fetched_at = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_millis() as u64)
                                .unwrap_or(0);
                            let cache = crate::services::proxy_channel::ChannelModelsCache {
                                base_url: ep.inference_base_url,
                                fetched_at,
                                models,
                            };
                            let _ = crate::services::proxy_channel::save_channel_models_cache(
                                &act_id, &cache,
                            );
                        }
                    });
                }
            }
        }
    }
}

/// Deactivates proxy environment variable overrides and reapplies native settings.
pub fn deactivate_active_channel_env() {
    for key in PROXY_MANAGED_ENV_KEYS {
        crate::utils::process_env::remove(key);
        unsafe {
            std::env::remove_var(key);
        }
    }
    // Reapply native environment variables from settings.json
    crate::utils::managed_env::apply_safe_config_environment_variables();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::proxy_channel::config::{set_active_channel_id, update_channel_env};

    #[test]
    fn test_apply_and_deactivate_active_channel_env() {
        let _lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let _guard = crate::utils::env_utils::EnvVarGuard::set("CLAUDE_CONFIG_DIR", temp.path());

        // Setup channel config
        update_channel_env(
            "cpa",
            &[
                ("ANTHROPIC_BASE_URL", "http://127.0.0.1:8317"),
                ("ANTHROPIC_API_KEY", "sk-cpa-token"),
                ("ANTHROPIC_DEFAULT_SONNET_MODEL", "gemini-2.5-pro"),
            ],
        )
        .unwrap();

        // When inactive, apply should do nothing
        set_active_channel_id(None).unwrap();
        apply_active_channel_env();
        assert_ne!(
            crate::utils::process_env::var("ANTHROPIC_BASE_URL").as_deref(),
            Some("http://127.0.0.1:8317")
        );

        // When active, apply sets process_env
        set_active_channel_id(Some("cpa")).unwrap();
        apply_active_channel_env();
        assert_eq!(
            crate::utils::process_env::var("ANTHROPIC_BASE_URL").as_deref(),
            Some("http://127.0.0.1:8317")
        );
        assert_eq!(
            crate::utils::process_env::var("ANTHROPIC_API_KEY").as_deref(),
            Some("sk-cpa-token")
        );
        assert_eq!(
            crate::utils::process_env::var("ANTHROPIC_DEFAULT_SONNET_MODEL").as_deref(),
            Some("gemini-2.5-pro")
        );

        // Deactivate clears them
        deactivate_active_channel_env();
        assert_ne!(
            crate::utils::process_env::var("ANTHROPIC_BASE_URL").as_deref(),
            Some("http://127.0.0.1:8317")
        );
        assert_ne!(
            crate::utils::process_env::var("ANTHROPIC_DEFAULT_SONNET_MODEL").as_deref(),
            Some("gemini-2.5-pro")
        );
    }
}
