//! Maps to: CC `utils/settings/managedPath.ts`.

use std::path::PathBuf;

/// Maps to: CC `utils/settings/managedPath.ts#getManagedFilePath`.
pub(crate) fn get_managed_file_path() -> PathBuf {
    let override_path =
        crate::utils::process_env::var_os("CLAUDE_CODE_MANAGED_SETTINGS_PATH").map(PathBuf::from);
    #[cfg(test)]
    if let Some(path) = override_path.clone() {
        // Unit tests inject an isolated managed root while holding TEST_ENV_LOCK.
        // This branch is absent from production binaries; external production
        // builds must ignore the Ant-only environment variable.
        return path;
    }
    if crate::utils::build_profile::build_audience().is_internal() {
        if let Some(path) = override_path {
            return path;
        }
    }

    #[cfg(target_os = "macos")]
    {
        PathBuf::from("/Library/Application Support/ClaudeCode")
    }
    #[cfg(target_os = "windows")]
    {
        PathBuf::from(r"C:\Program Files\ClaudeCode")
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        PathBuf::from("/etc/claude-code")
    }
}

/// Maps to: CC `utils/settings/managedPath.ts#getManagedSettingsDropInDir`.
pub(crate) fn get_managed_settings_drop_in_dir() -> PathBuf {
    get_managed_file_path().join("managed-settings.d")
}
