//! Session environment scripts produced by hooks.
//!
//! Maps to: CC `utils/sessionEnvironment.ts:1-155`.

use std::path::PathBuf;
use std::sync::{LazyLock, RwLock};

#[derive(Clone, Debug)]
enum SessionEnvCache {
    NotLoaded,
    Missing,
    Loaded(String),
}

static SESSION_ENV_SCRIPT: LazyLock<RwLock<SessionEnvCache>> =
    LazyLock::new(|| RwLock::new(SessionEnvCache::NotLoaded));

const HOOK_ENV_TYPES: &[(&str, u8)] = &[
    ("setup", 0),
    ("sessionstart", 1),
    ("cwdchanged", 2),
    ("filechanged", 3),
];

fn is_hook_env_file(name: &str) -> Option<(u8, u64)> {
    let stem = name.strip_suffix(".sh")?;
    for (prefix, priority) in HOOK_ENV_TYPES {
        let Some(index) = stem
            .strip_prefix(prefix)
            .and_then(|rest| rest.strip_prefix("-hook-"))
        else {
            continue;
        };
        if let Ok(index) = index.parse::<u64>() {
            return Some((*priority, index));
        }
    }
    None
}

/// Maps to CC `getSessionEnvDirPath()`.
pub fn get_session_env_dir_path() -> std::io::Result<PathBuf> {
    if !crate::utils::session_storage::is_session_write_enabled() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "session environment persistence is disabled by COMETIX_WRITE_ENABLED=0",
        ));
    }
    let path = crate::utils::env_utils::get_claude_config_home_dir()
        .join("session-env")
        .join(crate::bootstrap::state::get_session_id());
    std::fs::create_dir_all(&path)?;
    Ok(path)
}

/// Maps to CC `getHookEnvFilePath(hookEvent, hookIndex)`.
pub fn get_hook_env_file_path(hook_event: &str, hook_index: usize) -> std::io::Result<PathBuf> {
    let prefix = hook_event.to_ascii_lowercase();
    Ok(get_session_env_dir_path()?.join(format!("{prefix}-hook-{hook_index}.sh")))
}

/// Maps to CC `clearCwdEnvFiles()`.
pub fn clear_cwd_env_files() -> std::io::Result<()> {
    let dir = get_session_env_dir_path()?;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if (name.starts_with("filechanged-hook-") || name.starts_with("cwdchanged-hook-"))
            && is_hook_env_file(&name).is_some()
        {
            std::fs::write(entry.path(), "")?;
        }
    }
    invalidate_session_env_cache();
    Ok(())
}

/// Maps to CC `invalidateSessionEnvCache()`.
pub fn invalidate_session_env_cache() {
    if let Ok(mut cache) = SESSION_ENV_SCRIPT.write() {
        *cache = SessionEnvCache::NotLoaded;
    }
}

/// Maps to CC `getSessionEnvironmentScript()`.
pub fn get_session_environment_script() -> Option<String> {
    if cfg!(windows) {
        return None;
    }

    if let Ok(cache) = SESSION_ENV_SCRIPT.read() {
        match &*cache {
            SessionEnvCache::Missing => return None,
            SessionEnvCache::Loaded(script) => return Some(script.clone()),
            SessionEnvCache::NotLoaded => {}
        }
    }

    let mut scripts = Vec::new();
    if let Some(env_file) = crate::utils::process_env::var_os("CLAUDE_ENV_FILE") {
        if let Ok(content) = std::fs::read_to_string(env_file) {
            let content = content.trim();
            if !content.is_empty() {
                scripts.push(content.to_string());
            }
        }
    }

    if let Ok(dir) = get_session_env_dir_path() {
        let mut files = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let name = entry.file_name().into_string().ok()?;
                let sort_key = is_hook_env_file(&name)?;
                Some((sort_key, entry.path()))
            })
            .collect::<Vec<_>>();
        files.sort_by_key(|(key, _)| *key);
        for (_, path) in files {
            if let Ok(content) = std::fs::read_to_string(path) {
                let content = content.trim();
                if !content.is_empty() {
                    scripts.push(content.to_string());
                }
            }
        }
    }

    let result = (!scripts.is_empty()).then(|| scripts.join("\n"));
    if let Ok(mut cache) = SESSION_ENV_SCRIPT.write() {
        *cache = result
            .clone()
            .map(SessionEnvCache::Loaded)
            .unwrap_or(SessionEnvCache::Missing);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EnvGuard {
        _env: crate::utils::env_utils::EnvVarGuard,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
            Self {
                _env: crate::utils::env_utils::EnvVarGuard::set(key, value),
            }
        }
    }

    #[test]
    fn environment_scripts_use_official_event_then_index_order() {
        struct CacheGuard;
        impl Drop for CacheGuard {
            fn drop(&mut self) {
                invalidate_session_env_cache();
            }
        }

        let _lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let _cache = CacheGuard;
        let root = std::env::temp_dir().join(format!(
            "cometix-session-env-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let _config = EnvGuard::set("CLAUDE_CONFIG_DIR", &root);
        let _writes = EnvGuard::set("COMETIX_WRITE_ENABLED", "1");
        invalidate_session_env_cache();
        let dir = get_session_env_dir_path().unwrap();
        std::fs::write(dir.join("filechanged-hook-0.sh"), "export D=4").unwrap();
        std::fs::write(dir.join("setup-hook-1.sh"), "export B=2").unwrap();
        std::fs::write(dir.join("setup-hook-0.sh"), "export A=1").unwrap();
        std::fs::write(dir.join("cwdchanged-hook-0.sh"), "export C=3").unwrap();

        assert_eq!(
            get_session_environment_script().as_deref(),
            Some("export A=1\nexport B=2\nexport C=3\nexport D=4")
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
