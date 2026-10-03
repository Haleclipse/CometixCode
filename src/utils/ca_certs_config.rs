//! Maps to: CC `utils/caCertsConfig.ts`: settings-backed `NODE_EXTRA_CA_CERTS`
//! for [`crate::utils::ca_certs`].
//!
//! `NODE_EXTRA_CA_CERTS` is not a safe variable, so it reaches the environment
//! only after trust. An HTTPS proxy needs it before that, so startup takes it
//! early from the two user-controlled files, never from project settings.

use crate::utils::debug::log_for_debugging;
use crate::utils::process_env::JsTruthy;
use crate::utils::settings::{SettingSource, get_settings_for_source};

/// Maps to: CC `utils/caCertsConfig.ts:34-45` `applyExtraCACertsFromConfig`,
/// run right after the safe environment at startup (`init.ts:79`), before
/// any TLS connection.
pub fn apply_extra_ca_certs_from_config() {
    if crate::utils::process_env::var("NODE_EXTRA_CA_CERTS").truthy().is_some() {
        return;
    }
    if let Some(config_path) = get_extra_certs_path_from_config() {
        crate::utils::process_env::set("NODE_EXTRA_CA_CERTS", &config_path);
        log_for_debugging(&format!(
            "CA certs: Applied NODE_EXTRA_CA_CERTS from config to process.env: {config_path}"
        ));
    }
}

/// Maps to: CC `utils/caCertsConfig.ts:59-88` `getExtraCertsPathFromConfig`:
/// `~/.claude/settings.json` over `~/.claude.json`, with JS truthiness.
///
/// CC wraps this in `try`; the Rust readers return defaults instead of
/// throwing, so there is no failure branch to log.
fn get_extra_certs_path_from_config() -> Option<String> {
    let global_config = crate::utils::config::load_global_config();
    let global_env = global_config.env.as_ref();
    let settings = get_settings_for_source(SettingSource::User);
    let settings_env = settings.as_ref().and_then(|settings| settings.env.as_deref());

    let keys = |env: Option<Vec<&String>>| {
        env.map_or_else(
            || "none".to_string(),
            |keys| keys.into_iter().map(String::as_str).collect::<Vec<_>>().join(","),
        )
    };
    log_for_debugging(&format!(
        "CA certs: Config fallback - globalEnv keys: {}, settingsEnv keys: {}",
        // CC's default global config has `env: {}` (`config.ts:605`), so an
        // absent one lists no keys rather than "none".
        keys(Some(global_env.map(|env| env.keys().collect()).unwrap_or_default())),
        keys(settings_env.map(|env| env.keys().collect())),
    ));

    let path = settings_env
        .and_then(|env| env.get("NODE_EXTRA_CA_CERTS"))
        .filter(|path| !path.is_empty())
        .or_else(|| global_env.and_then(|env| env.get("NODE_EXTRA_CA_CERTS")))
        .filter(|path| !path.is_empty())
        .cloned();
    if let Some(path) = &path {
        log_for_debugging(&format!(
            "CA certs: Found NODE_EXTRA_CA_CERTS in config/settings: {path}"
        ));
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[test]
    fn user_settings_path_applies_only_when_the_environment_has_none() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = std::env::temp_dir().join(format!("cometix-ca-config-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("settings.json"),
            r#"{"env":{"NODE_EXTRA_CA_CERTS":"/from/settings.pem"}}"#,
        )
        .unwrap();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root);
        crate::utils::settings::settings_cache::reset_settings_cache();

        let _unset = EnvVarGuard::unset("NODE_EXTRA_CA_CERTS");
        apply_extra_ca_certs_from_config();
        assert_eq!(
            crate::utils::process_env::var("NODE_EXTRA_CA_CERTS").as_deref(),
            Some("/from/settings.pem")
        );

        // An environment value wins; an empty one does not count.
        crate::utils::process_env::set("NODE_EXTRA_CA_CERTS", "/from/env.pem");
        apply_extra_ca_certs_from_config();
        assert_eq!(
            crate::utils::process_env::var("NODE_EXTRA_CA_CERTS").as_deref(),
            Some("/from/env.pem")
        );
        crate::utils::process_env::set("NODE_EXTRA_CA_CERTS", "");
        apply_extra_ca_certs_from_config();
        assert_eq!(
            crate::utils::process_env::var("NODE_EXTRA_CA_CERTS").as_deref(),
            Some("/from/settings.pem")
        );

        crate::utils::settings::settings_cache::reset_settings_cache();
        let _ = std::fs::remove_dir_all(root);
    }
}
