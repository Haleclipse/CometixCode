//! Plugin option storage and substitution helpers.
//!
//! Maps to: CC `utils/plugins/pluginOptionsStorage.ts`.
//!
//! Source-owned options memo and split sensitive/settings persistence.

use crate::types::plugin::LoadedPlugin;
use serde_json::Value;
use std::path::Path;

/// Maps to: CC `utils/plugins/pluginOptionsStorage.ts:44-48` `getPluginStorageId`.
pub fn get_plugin_storage_id(plugin: &LoadedPlugin) -> String {
    plugin.source.clone()
}

/// Maps to: CC `utils/plugins/pluginOptionsStorage.ts:56-100` `loadPluginOptions`.
pub fn load_plugin_options(plugin_id: &str) -> serde_json::Map<String, Value> {
    if let Some(value) = OPTIONS_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(plugin_id)
        .cloned()
    {
        return value;
    }
    let settings = crate::utils::settings::get_initial_settings();
    let credentials = crate::utils::secure_storage::get_secure_storage().read();
    let mut values = settings
        .plugin_configs
        .as_ref()
        .and_then(|configs| configs.get(plugin_id))
        .and_then(|config| config.get("options"))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    if let Some(secrets) = credentials
        .as_ref()
        .and_then(|storage| storage.get("pluginSecrets"))
        .and_then(|plugin_secrets| plugin_secrets.get(plugin_id))
        .and_then(Value::as_object)
    {
        for (key, value) in secrets {
            values.insert(key.clone(), value.clone());
        }
    }

    OPTIONS_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(plugin_id.into(), values.clone());
    values
}

/// Maps to: CC pluginOptionsStorage.ts PluginOptionValues/PluginOptionSchema aliases.
pub type PluginOptionValues = serde_json::Map<String, Value>;
pub type PluginOptionSchema = Value;
/// Maps to: CC loadPluginOptions.cache. Per-ID lifetime memo (including empty).
static OPTIONS_CACHE: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, PluginOptionValues>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));
/// Maps to: CC pluginOptionsStorage.ts#clearPluginOptionsCache.
pub fn clear_plugin_options_cache() {
    OPTIONS_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
}
/// Maps to: CC pluginOptionsStorage.ts#savePluginOptions.
pub fn save_plugin_options(
    plugin_id: &str,
    values: &PluginOptionValues,
    schema: &PluginOptionSchema,
) -> anyhow::Result<()> {
    let mut non_sensitive = PluginOptionValues::new();
    let mut sensitive = PluginOptionValues::new();
    for (key, value) in values {
        if schema
            .get(key)
            .and_then(|s| s.get("sensitive"))
            .and_then(Value::as_bool)
            == Some(true)
        {
            sensitive.insert(
                key.clone(),
                Value::String(plugin_option_value_to_js_string(value).map_err(anyhow::Error::msg)?),
            );
        } else {
            non_sensitive.insert(key.clone(), value.clone());
        }
    }
    let storage = crate::utils::secure_storage::get_secure_storage();
    let first = storage.read();
    let existing_secure = first
        .as_ref()
        .and_then(|s| s.get("pluginSecrets"))
        .and_then(|s| s.get(plugin_id))
        .and_then(Value::as_object);
    let mut secure_scrubbed = existing_secure.cloned().unwrap_or_default();
    secure_scrubbed.retain(|key, _| !non_sensitive.contains_key(key));
    if !sensitive.is_empty() || existing_secure.is_some_and(|s| s.len() != secure_scrubbed.len()) {
        let mut existing = storage.read().unwrap_or_else(|| serde_json::json!({}));
        if !existing.get("pluginSecrets").is_some_and(Value::is_object) {
            existing["pluginSecrets"] = serde_json::json!({});
        }
        secure_scrubbed.extend(sensitive.clone());
        existing["pluginSecrets"][plugin_id] = Value::Object(secure_scrubbed);
        let result = storage.update(&existing)?;
        if !result.success {
            let message = format!(
                "Failed to save sensitive plugin options for {plugin_id} to secure storage"
            );
            crate::utils::log::log_error(crate::utils::log::LogError::new(message.clone()));
            anyhow::bail!(message);
        }
        if let Some(warning) = result.warning {
            crate::utils::debug::log_for_debugging(&format!(
                "Plugin secrets save warning: {warning}"
            ));
        }
    }
    let settings = crate::utils::settings::get_initial_settings();
    let existing = settings
        .plugin_configs
        .as_ref()
        .and_then(|c| c.get(plugin_id))
        .and_then(|c| c.get("options"))
        .and_then(Value::as_object);
    let scrub: Vec<_> = existing
        .into_iter()
        .flat_map(|o| o.keys())
        .filter(|k| sensitive.contains_key(*k))
        .cloned()
        .collect();
    if !non_sensitive.is_empty() || !scrub.is_empty() {
        // Established updateSettingsForSource Null carrier means explicit undefined.
        for key in scrub {
            non_sensitive.insert(key, Value::Null);
        }
        let mut patch = serde_json::to_value(settings)?;
        if !patch.get("pluginConfigs").is_some_and(Value::is_object) {
            patch["pluginConfigs"] = serde_json::json!({});
        }
        if !patch["pluginConfigs"]
            .get(plugin_id)
            .is_some_and(Value::is_object)
        {
            patch["pluginConfigs"][plugin_id] = serde_json::json!({});
        }
        patch["pluginConfigs"][plugin_id]["options"] = Value::Object(non_sensitive);
        if let Err(error) = crate::utils::settings::update_settings_for_source(
            crate::utils::settings::SettingSource::User,
            patch.as_object().unwrap(),
        ) {
            crate::utils::log::log_error(crate::utils::log::LogError::new(error.to_string()));
            anyhow::bail!("Failed to save plugin options for {plugin_id}: {error}");
        }
    }
    clear_plugin_options_cache();
    Ok(())
}
/// Maps to: CC pluginOptionsStorage.ts#deletePluginOptions.
pub fn delete_plugin_options(plugin_id: &str) {
    let settings = crate::utils::settings::get_initial_settings();
    if settings
        .plugin_configs
        .as_ref()
        .is_some_and(|c| c.as_object().is_some_and(|c| c.contains_key(plugin_id)))
    {
        let patch = serde_json::json!({"pluginConfigs":{plugin_id:Value::Null}});
        if let Err(error) = crate::utils::settings::update_settings_for_source(
            crate::utils::settings::SettingSource::User,
            patch.as_object().unwrap(),
        ) {
            crate::utils::debug::log_for_debugging(&format!(
                "deletePluginOptions: failed to clear settings.pluginConfigs[{plugin_id}]: {error}"
            ));
        }
    }
    let storage = crate::utils::secure_storage::get_secure_storage();
    if let Some(mut existing) = storage.read() {
        if let Some(secrets) = existing
            .get_mut("pluginSecrets")
            .and_then(Value::as_object_mut)
        {
            let prefix = format!("{plugin_id}/");
            let old_len = secrets.len();
            secrets.retain(|k, _| k != plugin_id && !k.starts_with(&prefix));
            if old_len != secrets.len() {
                if secrets.is_empty() {
                    existing.as_object_mut().unwrap().remove("pluginSecrets");
                }
                if !storage.update(&existing).is_ok_and(|r| r.success) {
                    crate::utils::debug::log_for_debugging(&format!(
                        "deletePluginOptions: failed to clear pluginSecrets for {plugin_id} from keychain"
                    ));
                }
            }
        }
    }
    clear_plugin_options_cache();
}
/// Maps to: CC pluginOptionsStorage.ts#getUnconfiguredOptions.
pub fn get_unconfigured_options(plugin: &LoadedPlugin) -> PluginOptionSchema {
    let Some(schema) = plugin
        .manifest
        .user_config
        .as_ref()
        .filter(|s| s.as_object().is_some_and(|o| !o.is_empty()))
    else {
        return serde_json::json!({});
    };
    let saved = load_plugin_options(&get_plugin_storage_id(plugin));
    if super::mcpb_handler::validate_user_config(&saved, schema).valid {
        return serde_json::json!({});
    }
    let mut result = PluginOptionValues::new();
    for (key, field) in schema.as_object().unwrap() {
        let mut single = PluginOptionValues::new();
        if let Some(value) = saved.get(key) {
            single.insert(key.clone(), value.clone());
        }
        let one = serde_json::json!({key:field});
        if !super::mcpb_handler::validate_user_config(&single, &one).valid {
            result.insert(key.clone(), field.clone());
        }
    }
    Value::Object(result)
}

/// Maps to: CC `utils/plugins/pluginOptionsStorage.ts:356-383` `substituteUserConfigVariables`.
pub fn substitute_user_config_variables(
    value: &str,
    user_config: &serde_json::Map<String, Value>,
) -> Result<String, String> {
    substitute_user_config_refs(value, |key, original| {
        if key.is_empty() {
            return Ok(original.to_owned());
        }
        match user_config.get(key) {
            Some(value) => plugin_option_value_to_js_string(value),
            None => Err(format!(
                "Missing required user configuration value: {key}. This should have been validated before variable substitution."
            )),
        }
    })
}

/// Maps to: CC `utils/plugins/pluginOptionsStorage.ts:385-417` `substituteUserConfigInContent`.
pub fn substitute_user_config_in_content(
    content: &str,
    options: &serde_json::Map<String, Value>,
    schema: &Value,
) -> Result<String, String> {
    substitute_user_config_refs(content, |key, original| {
        if key.is_empty() {
            return Ok(original.to_string());
        }
        if schema
            .get(key)
            .and_then(|field| field.get("sensitive"))
            .and_then(Value::as_bool)
            == Some(true)
        {
            return Ok(format!(
                "[sensitive option '{key}' not available in skill content]"
            ));
        }
        options
            .get(key)
            .map(plugin_option_value_to_js_string)
            .unwrap_or_else(|| Ok(original.to_string()))
    })
}

fn substitute_user_config_refs(
    content: &str,
    mut replace: impl FnMut(&str, &str) -> Result<String, String>,
) -> Result<String, String> {
    const PREFIX: &str = "${user_config.";
    let mut out = String::with_capacity(content.len());
    let mut rest = content;
    while let Some(start) = rest.find(PREFIX) {
        out.push_str(&rest[..start]);
        let after_prefix = &rest[start + PREFIX.len()..];
        let Some(end) = after_prefix.find('}') else {
            out.push_str(&rest[start..]);
            return Ok(out);
        };
        let key = &after_prefix[..end];
        let original = &rest[start..start + PREFIX.len() + end + 1];
        out.push_str(&replace(key, original)?);
        rest = &after_prefix[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

fn plugin_option_value_to_js_string(value: &Value) -> Result<String, String> {
    // Source String(value), including Number's ECMAScript spelling and array
    // join's empty null elements. Reuse the existing dynamic ToString adapter.
    if value.is_null() {
        Ok("null".into())
    } else {
        crate::utils::json::JsoncValue::from_json(value.clone())
            .array_string()
            // Bun TypeError.message from actual source String(value) oracle.
            .map_err(|()| "No default value".to_owned())
    }
}

/// Maps to: CC `utils/plugins/pluginOptionsStorage.ts:326-354` `substitutePluginVariables`.
pub fn substitute_plugin_variables(
    content: &str,
    plugin_path: &Path,
    source: Option<&str>,
) -> Result<String, String> {
    let root = normalize_plugin_path(plugin_path);
    let out = content.replace("${CLAUDE_PLUGIN_ROOT}", &root);
    let Some(source) = source.filter(|source| !source.is_empty()) else {
        return Ok(out);
    };
    // Function replacement runs lazily once for each DATA occurrence. Text
    // inserted by ROOT participates in this subsequent replacement, as in JS.
    let mut rendered = String::with_capacity(out.len());
    let mut rest = out.as_str();
    const DATA: &str = "${CLAUDE_PLUGIN_DATA}";
    while let Some(index) = rest.find(DATA) {
        rendered.push_str(&rest[..index]);
        let data_dir = crate::utils::plugins::plugin_directories::get_plugin_data_dir(source)
            .map_err(|error| error.to_string())?;
        rendered.push_str(&normalize_plugin_path(&data_dir));
        rest = &rest[index + DATA.len()..];
    }
    rendered.push_str(rest);
    Ok(rendered)
}

fn normalize_plugin_path(path: &Path) -> String {
    let display = path.display().to_string();
    if cfg!(windows) {
        display.replace('\\', "/")
    } else {
        display
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::TEST_ENV_LOCK;

    #[test]
    fn option_string_matches_official_number_and_array_coercion() {
        for (value, expected) in [
            (serde_json::json!(16.0), "16"),
            (serde_json::json!(-0.0), "0"),
            (serde_json::json!(1e21), "1e+21"),
            (serde_json::json!(1e-7), "1e-7"),
            (Value::Null, "null"),
            (
                serde_json::json!([16.0, null, [null, -0.0], {}]),
                "16,,,0,[object Object]",
            ),
        ] {
            assert_eq!(plugin_option_value_to_js_string(&value).unwrap(), expected);
        }
    }
    #[test]
    fn user_config_content_substitution_hides_sensitive_and_keeps_missing_refs() {
        let options = serde_json::json!({
            "name": "Ada",
            "token": "secret"
        })
        .as_object()
        .cloned()
        .unwrap();
        let schema = serde_json::json!({
            "name": {"type":"string"},
            "token": {"type":"string", "sensitive": true}
        });

        let rendered = substitute_user_config_in_content(
            "Hello ${user_config.name}; token=${user_config.token}; missing=${user_config.missing}",
            &options,
            &schema,
        )
        .unwrap();
        assert_eq!(
            rendered,
            "Hello Ada; token=[sensitive option 'token' not available in skill content]; missing=${user_config.missing}"
        );
    }

    #[test]
    fn user_config_variable_substitution_throws_on_missing_refs() {
        let options = serde_json::json!({"name":"Ada","enabled":true})
            .as_object()
            .cloned()
            .unwrap();
        assert_eq!(
            substitute_user_config_variables(
                "--name=${user_config.name} --enabled=${user_config.enabled}",
                &options,
            )
            .unwrap(),
            "--name=Ada --enabled=true"
        );
        let error = substitute_user_config_variables("${user_config.missing}", &options)
            .expect_err("missing key");
        assert!(error.contains("Missing required user configuration value: missing"));
    }

    #[test]
    fn plugin_variable_substitution_matches_bun_lazy_data_directory_and_errors() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!("plugin-vars-{}", uuid::Uuid::new_v4()));
        let previous = crate::utils::process_env::var("CLAUDE_CODE_PLUGIN_CACHE_DIR");
        struct Restore(Option<String>, std::path::PathBuf);
        impl Drop for Restore {
            fn drop(&mut self) {
                if let Some(value) = &self.0 {
                    crate::utils::process_env::set("CLAUDE_CODE_PLUGIN_CACHE_DIR", value);
                } else {
                    crate::utils::process_env::remove("CLAUDE_CODE_PLUGIN_CACHE_DIR");
                }
                let _ = std::fs::remove_dir_all(&self.1);
            }
        }
        let _restore = Restore(previous, root.clone());
        let cache = root.join("cache");
        crate::utils::process_env::set("CLAUDE_CODE_PLUGIN_CACHE_DIR", &cache);
        assert_eq!(
            substitute_plugin_variables("${CLAUDE_PLUGIN_ROOT}", &root, Some("plugin@market"))
                .unwrap(),
            root.display().to_string()
        );
        assert!(!cache.exists());
        for source in [None, Some("")] {
            assert_eq!(
                substitute_plugin_variables("${CLAUDE_PLUGIN_DATA}", &root, source).unwrap(),
                "${CLAUDE_PLUGIN_DATA}"
            );
            assert!(!cache.exists());
        }
        let data = cache.join("data/plugin-market");
        assert_eq!(
            substitute_plugin_variables(
                "${CLAUDE_PLUGIN_DATA}|${CLAUDE_PLUGIN_DATA}",
                &root,
                Some("plugin@market")
            )
            .unwrap(),
            format!("{0}|{0}", data.display())
        );
        assert!(data.is_dir());
        std::fs::remove_dir_all(&cache).unwrap();
        std::fs::write(&cache, "blocks mkdir").unwrap();
        assert!(
            substitute_plugin_variables("ordinary content", &root, Some("plugin@market")).is_ok()
        );
        assert!(
            substitute_plugin_variables("${CLAUDE_PLUGIN_DATA}", &root, Some("plugin@market"))
                .is_err()
        );
    }
    #[test]
    fn malformed_option_string_throws_at_all_three_source_consumers_without_panicking() {
        // Actual source AST oracle: options-tostring-oracle.json. This throws
        // before savePluginOptions touches secure storage/settings or clears cache.
        let options = serde_json::json!({"bad":{"toString":null}})
            .as_object()
            .unwrap()
            .clone();
        let sensitive = serde_json::json!({"bad":{"type":"string","sensitive":true}});
        assert_eq!(
            save_plugin_options("invalid-string@fixture", &options, &sensitive)
                .unwrap_err()
                .to_string(),
            "No default value"
        );
        assert_eq!(
            substitute_user_config_variables("${user_config.bad}", &options).unwrap_err(),
            "No default value"
        );
        assert_eq!(
            substitute_user_config_in_content(
                "${user_config.bad}",
                &options,
                &serde_json::json!({})
            )
            .unwrap_err(),
            "No default value"
        );
        assert_eq!(
            substitute_user_config_in_content("${user_config.bad}", &options, &sensitive).unwrap(),
            "[sensitive option 'bad' not available in skill content]"
        );
        assert_eq!(
            substitute_user_config_in_content(
                "${user_config.other}",
                &options,
                &serde_json::json!({})
            )
            .unwrap(),
            "${user_config.other}"
        );
    }
}
