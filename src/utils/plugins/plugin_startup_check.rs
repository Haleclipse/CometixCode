//! Maps to: CC `utils/plugins/pluginStartupCheck.ts`.
use crate::utils::settings::constants::SettingSource;
use indexmap::IndexMap;
use serde_json::Value;
/// Maps to: CC pluginStartupCheck.ts:96-166#getPluginEditableScopes.
pub fn get_plugin_editable_scopes() -> IndexMap<String, String> {
    let mut result = IndexMap::new();
    let extra = super::add_dir_plugin_settings::get_add_dir_enabled_plugins();
    for (id, value) in &extra {
        if id.contains('@') {
            if value == &Value::Bool(true) {
                result.insert(id.clone(), "flag".into());
            } else if value == &Value::Bool(false) {
                result.shift_remove(id);
            }
        }
    }
    for (scope, source) in [
        ("user", SettingSource::User),
        ("project", SettingSource::Project),
        ("local", SettingSource::Local),
        ("flag", SettingSource::Flag),
    ] {
        if let Some(entries) = crate::utils::settings::get_settings_for_source(source)
            .and_then(|s| s.enabled_plugins)
            .and_then(|v| v.as_object().cloned())
        {
            for (id, value) in entries {
                if !id.contains('@') {
                    continue;
                }
                if extra.get(&id).is_some_and(|prior| prior != &value) {
                    crate::utils::debug::log_for_debugging(&format!(
                        "Plugin {id} from --add-dir ({}) overridden by {scope}Settings ({value})",
                        extra[&id]
                    ));
                }
                if value == Value::Bool(true) {
                    result.insert(id, scope.into());
                } else if value == Value::Bool(false) {
                    result.shift_remove(&id);
                }
            }
        }
    }
    crate::utils::debug::log_for_debugging(&format!(
        "Found {} enabled plugins with scopes: {}",
        result.len(),
        result
            .iter()
            .map(|(id, scope)| format!("{id}({scope})"))
            .collect::<Vec<_>>()
            .join(", ")
    ));
    result
}
