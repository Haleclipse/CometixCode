//! Maps to: CC `utils/systemDirectories.ts`.
use indexmap::IndexMap;
/// Maps to: CC `systemDirectories.ts#SystemDirectoriesOptions`.
#[derive(Default)]
pub struct SystemDirectoriesOptions {
    pub env: Option<IndexMap<String, String>>,
    pub homedir: Option<String>,
    pub platform: Option<String>,
}
/// Maps to: CC `systemDirectories.ts#getSystemDirectories`.
pub fn get_system_directories(
    options: Option<SystemDirectoriesOptions>,
) -> IndexMap<String, String> {
    let options = options.unwrap_or_default();
    let platform = options
        .platform
        .unwrap_or_else(|| crate::utils::platform::get_platform().as_str().into());
    let home = options.homedir.unwrap_or_else(|| {
        crate::utils::node_os::homedir()
            .to_string_lossy()
            .into_owned()
    });
    let env = options.env.unwrap_or_else(|| {
        crate::utils::process_env::snapshot()
            .iter()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.to_string_lossy().into_owned(),
                )
            })
            .collect()
    });
    let mut result = IndexMap::from([("HOME".into(), home.clone())]);
    for (key, folder) in [
        ("DESKTOP", "Desktop"),
        ("DOCUMENTS", "Documents"),
        ("DOWNLOADS", "Downloads"),
    ] {
        let root = if platform == "windows" {
            env.get("USERPROFILE")
                .filter(|v| !v.is_empty())
                .unwrap_or(&home)
        } else {
            &home
        };
        // Native Node path.join representation: normalize without consulting
        // the filesystem; retain HOME itself exactly as supplied above.
        let joined = std::path::Path::new(root).join(folder);
        let mut normalized = std::path::PathBuf::new();
        for component in joined.components() {
            match component {
                std::path::Component::CurDir => {}
                std::path::Component::ParentDir => {
                    if normalized.file_name().is_some_and(|s| s != "..") {
                        normalized.pop();
                    } else if !normalized.has_root() {
                        normalized.push("..");
                    }
                }
                c => normalized.push(c.as_os_str()),
            }
        }
        let default = normalized.to_string_lossy().into_owned();
        let value = if matches!(platform.as_str(), "linux" | "wsl") {
            env.get(&format!(
                "XDG_{}_DIR",
                if key == "DOWNLOADS" { "DOWNLOAD" } else { key }
            ))
            .filter(|s| !s.is_empty())
            .cloned()
            .unwrap_or(default)
        } else {
            default
        };
        result.insert(key.into(), value);
    }
    if platform == "unknown" {
        crate::utils::debug::log_for_debugging("Unknown platform detected, using default paths");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    #[test]
    fn system_directories_keep_home_and_xdg_literals_but_normalize_defaults() {
        let oracle:serde_json::Value=serde_json::from_str(include_str!("../../tests/fixtures/oracles/plugin-marketplace-service-0914/mcpb-path-cache-oracle.json")).unwrap();
        for row in oracle
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["case"] == "dirs")
        {
            let options = &row["options"];
            let result = get_system_directories(Some(SystemDirectoriesOptions {
                env: Some(serde_json::from_value(options["env"].clone()).unwrap()),
                homedir: options["homedir"].as_str().map(str::to_owned),
                platform: options["platform"].as_str().map(str::to_owned),
            }));
            assert_eq!(serde_json::to_value(result).unwrap(), row["result"]);
        }
    }
}
