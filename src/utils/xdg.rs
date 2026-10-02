//! Maps to: CC `utils/xdg.ts`.
//!
//! XDG Base Directory helpers used by the native installer and Doctor version
//! lock diagnostics. These helpers are pure path projections.

use std::path::PathBuf;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct XdgOptions {
    pub xdg_state_home: Option<String>,
    pub xdg_cache_home: Option<String>,
    pub xdg_data_home: Option<String>,
    pub home: Option<String>,
}

/// Maps to CC `utils/xdg.ts#resolveOptions`:
/// `options?.homedir ?? process.env.HOME ?? osHomedir()`.
fn resolve_home(options: Option<&XdgOptions>) -> String {
    options
        .and_then(|options| options.home.clone())
        .or_else(|| crate::utils::process_env::var("HOME"))
        .unwrap_or_else(|| {
            crate::utils::node_os::homedir()
                .to_string_lossy()
                .into_owned()
        })
}

/// Maps to CC `utils/xdg.ts#getXDGStateHome`.
pub fn get_xdg_state_home() -> PathBuf {
    get_xdg_state_home_with_options(None)
}

pub fn get_xdg_state_home_with_options(options: Option<&XdgOptions>) -> PathBuf {
    options
        .and_then(|options| options.xdg_state_home.clone())
        .or_else(|| crate::utils::process_env::var("XDG_STATE_HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(resolve_home(options))
                .join(".local")
                .join("state")
        })
}

/// Maps to CC `utils/xdg.ts#getXDGCacheHome`.
pub fn get_xdg_cache_home() -> PathBuf {
    get_xdg_cache_home_with_options(None)
}

pub fn get_xdg_cache_home_with_options(options: Option<&XdgOptions>) -> PathBuf {
    options
        .and_then(|options| options.xdg_cache_home.clone())
        .or_else(|| crate::utils::process_env::var("XDG_CACHE_HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(resolve_home(options)).join(".cache"))
}

/// Maps to CC `utils/xdg.ts#getXDGDataHome`.
pub fn get_xdg_data_home() -> PathBuf {
    get_xdg_data_home_with_options(None)
}

pub fn get_xdg_data_home_with_options(options: Option<&XdgOptions>) -> PathBuf {
    options
        .and_then(|options| options.xdg_data_home.clone())
        .or_else(|| crate::utils::process_env::var("XDG_DATA_HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(resolve_home(options))
                .join(".local")
                .join("share")
        })
}

/// Maps to CC `utils/xdg.ts#getUserBinDir`.
pub fn get_user_bin_dir() -> PathBuf {
    get_user_bin_dir_with_options(None)
}

pub fn get_user_bin_dir_with_options(options: Option<&XdgOptions>) -> PathBuf {
    PathBuf::from(resolve_home(options))
        .join(".local")
        .join("bin")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xdg_paths_match_official_defaults_and_overrides() {
        let options = XdgOptions {
            home: Some("/home/alice".to_string()),
            ..Default::default()
        };
        assert_eq!(
            get_xdg_state_home_with_options(Some(&options)),
            PathBuf::from("/home/alice/.local/state")
        );
        assert_eq!(
            get_xdg_cache_home_with_options(Some(&options)),
            PathBuf::from("/home/alice/.cache")
        );
        assert_eq!(
            get_xdg_data_home_with_options(Some(&options)),
            PathBuf::from("/home/alice/.local/share")
        );
        assert_eq!(
            get_user_bin_dir_with_options(Some(&options)),
            PathBuf::from("/home/alice/.local/bin")
        );

        let overrides = XdgOptions {
            xdg_state_home: Some("/state".to_string()),
            xdg_cache_home: Some("/cache".to_string()),
            xdg_data_home: Some("/data".to_string()),
            home: Some("/home/alice".to_string()),
        };
        assert_eq!(
            get_xdg_state_home_with_options(Some(&overrides)),
            PathBuf::from("/state")
        );
        assert_eq!(
            get_xdg_cache_home_with_options(Some(&overrides)),
            PathBuf::from("/cache")
        );
        assert_eq!(
            get_xdg_data_home_with_options(Some(&overrides)),
            PathBuf::from("/data")
        );
    }
}
