//! Maps to: CC `utils/plugins/addDirPluginSettings.ts`.
use serde_json::{Map, Value};
/// Maps to: CC `utils/plugins/addDirPluginSettings.ts:22-22#SETTINGS_FILES`.
const SETTINGS_FILES: [&str; 2] = ["settings.json", "settings.local.json"];
/// Maps to: CC `utils/plugins/addDirPluginSettings.ts:34-48#getAddDirEnabledPlugins`.
pub fn get_add_dir_enabled_plugins() -> Map<String, Value> {
    let mut result = Map::new();
    for dir in crate::bootstrap::state::get_additional_directories_for_claude_md() {
        for file in SETTINGS_FILES {
            // Node join resolves all lexical segments before filesystem I/O.
            let mut path = std::path::PathBuf::new();
            for component in dir.join(".nyan").join(file).components() {
                match component {
                    std::path::Component::CurDir => {}
                    std::path::Component::ParentDir => {
                        if path.file_name().is_some_and(|n| n != "..") {
                            path.pop();
                        } else if !path.has_root() {
                            path.push("..");
                        }
                    }
                    c => path.push(c.as_os_str()),
                }
            }
            if let (Some(settings), _) = crate::utils::settings::parse_settings_file(&path)
                && let Some(Value::Object(entries)) = settings.enabled_plugins
            {
                result.extend(entries);
            }
        }
    }
    result
}
/// Maps to: CC `utils/plugins/addDirPluginSettings.ts:56-71#getAddDirExtraMarketplaces`.
pub fn get_add_dir_extra_marketplaces() -> Map<String, Value> {
    let mut result = Map::new();
    for dir in crate::bootstrap::state::get_additional_directories_for_claude_md() {
        for file in SETTINGS_FILES {
            // Node join resolves all lexical segments before filesystem I/O.
            let mut path = std::path::PathBuf::new();
            for component in dir.join(".nyan").join(file).components() {
                match component {
                    std::path::Component::CurDir => {}
                    std::path::Component::ParentDir => {
                        if path.file_name().is_some_and(|n| n != "..") {
                            path.pop();
                        } else if !path.has_root() {
                            path.push("..");
                        }
                    }
                    c => path.push(c.as_os_str()),
                }
            }
            if let (Some(settings), _) = crate::utils::settings::parse_settings_file(&path)
                && let Some(Value::Object(entries)) = settings.extra_known_marketplaces
            {
                result.extend(entries);
            }
        }
    }
    result
}
