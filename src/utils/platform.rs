//! Maps to: CC `utils/platform.ts`, for `getPlatform()`. The rest of the
//! module (WSL version, Linux distro and VCS detection) has no Rust caller
//! yet; elsewhere Rust tests the target with `cfg!`.

use std::sync::OnceLock;

/// CC `utils/platform.ts:7` `Platform`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    MacOS,
    Windows,
    Wsl,
    Linux,
    Unknown,
}

impl Platform {
    /// The string CC's `Platform` is, as settings such as
    /// `sandbox.enabledPlatforms` name it.
    pub fn as_str(self) -> &'static str {
        match self {
            Platform::MacOS => "macos",
            Platform::Windows => "windows",
            Platform::Wsl => "wsl",
            Platform::Linux => "linux",
            Platform::Unknown => "unknown",
        }
    }
}

/// Maps to: CC `utils/platform.ts:11-49` `getPlatform`, memoized: Linux whose
/// `/proc/version` names Microsoft or WSL is `wsl`, decided once per process.
/// `process.platform` names Android `android`, which is `unknown` here as
/// there. A `OnceLock`, not a `LazyLock`: lodash `memoize` caches nothing
/// when the function throws, so a panicking first call is retried.
pub fn get_platform() -> Platform {
    static PLATFORM: OnceLock<Platform> = OnceLock::new();
    *PLATFORM.get_or_init(detect_platform)
}

fn detect_platform() -> Platform {
    if cfg!(target_os = "macos") {
        return Platform::MacOS;
    }
    if cfg!(windows) {
        return Platform::Windows;
    }
    if cfg!(target_os = "linux") {
        // Check if running in WSL (Windows Subsystem for Linux)
        match crate::utils::fs_operations::get_fs_implementation().read_file_sync(
            std::path::Path::new("/proc/version"),
            crate::utils::fs_operations::BufferEncoding::Utf8,
        ) {
            Ok(version) => {
                let version = version.to_string_lossy().to_lowercase();
                if version.contains("microsoft") || version.contains("wsl") {
                    return Platform::Wsl;
                }
            }
            // Error reading /proc/version, assume regular Linux
            Err(error) => {
                crate::utils::log::log_error(crate::utils::log::LogError::new(error.to_string()));
            }
        }
        return Platform::Linux;
    }
    Platform::Unknown
}
