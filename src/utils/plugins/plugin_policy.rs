//! Maps to: CC `utils/plugins/pluginPolicy.ts`.

use crate::utils::settings::constants::SettingSource;
use serde_json::Value;

/// Maps to: CC `utils/plugins/pluginPolicy.ts:17-20#isPluginBlockedByPolicy`.
pub fn is_plugin_blocked_by_policy(plugin_id: &str) -> bool {
    crate::utils::settings::get_settings_for_source(SettingSource::Policy)
        .and_then(|settings| settings.enabled_plugins)
        .as_ref()
        .and_then(Value::as_object)
        .and_then(|plugins| plugins.get(plugin_id))
        .and_then(Value::as_bool)
        == Some(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::settings::settings_cache::set_cached_settings_for_source;
    use crate::utils::settings::types::SettingsJson;
    use crate::utils::test_env::TEST_ENV_LOCK;
    use serde_json::json;

    #[test]
    fn plugin_policy_strict_false_matches_official_bun_oracle() {
        // CC pluginPolicy.ts:17-20: only the exact boolean false blocks;
        // absent settings/key, null, zero, string and array do not.
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        set_cached_settings_for_source(SettingSource::Policy, None);
        assert!(!is_plugin_blocked_by_policy("p@m"));
        set_cached_settings_for_source(SettingSource::Policy, Some(SettingsJson::default()));
        assert!(!is_plugin_blocked_by_policy("p@m"));
        for value in [
            json!(false),
            json!(true),
            Value::Null,
            json!(0),
            json!("false"),
            json!([]),
        ] {
            set_cached_settings_for_source(
                SettingSource::Policy,
                Some(SettingsJson {
                    enabled_plugins: Some(json!({"p@m": value})),
                    ..SettingsJson::default()
                }),
            );
            assert_eq!(
                is_plugin_blocked_by_policy("p@m"),
                value == json!(false),
                "{value}"
            );
            assert!(!is_plugin_blocked_by_policy("different@m"));
        }
    }

    #[test]
    fn plugin_policy_reads_only_managed_source_and_observes_new_settings() {
        // CC pluginPolicy.ts:18 reads policySettings on each invocation;
        // editable/flag values cannot substitute for that source.
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        for source in [
            SettingSource::User,
            SettingSource::Project,
            SettingSource::Local,
            SettingSource::Flag,
        ] {
            set_cached_settings_for_source(
                source,
                Some(SettingsJson {
                    enabled_plugins: Some(json!({"p@m": false})),
                    ..SettingsJson::default()
                }),
            );
        }
        set_cached_settings_for_source(
            SettingSource::Policy,
            Some(SettingsJson {
                enabled_plugins: Some(json!({"p@m": true})),
                ..SettingsJson::default()
            }),
        );
        assert!(!is_plugin_blocked_by_policy("p@m"));
        set_cached_settings_for_source(
            SettingSource::Policy,
            Some(SettingsJson {
                enabled_plugins: Some(json!({"p@m": false})),
                ..SettingsJson::default()
            }),
        );
        assert!(is_plugin_blocked_by_policy("p@m"));
    }
}
