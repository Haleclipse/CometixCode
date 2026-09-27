//! Maps to: CC `utils/cachePaths.ts`.

use std::io;
use std::path::PathBuf;
use std::sync::OnceLock;

const MAX_SANITIZED_LENGTH: usize = 200;

/// Maps to: CC `utils/cachePaths.ts#sanitizePath:13-19`.
fn sanitize_path(name: &str) -> String {
    let sanitized: String = name
        .encode_utf16()
        .map(|unit| {
            if unit <= 127 && (unit as u8).is_ascii_alphanumeric() {
                char::from(unit as u8)
            } else {
                '-'
            }
        })
        .collect();
    if sanitized.len() <= MAX_SANITIZED_LENGTH {
        return sanitized;
    }
    let mut hash = i64::from(crate::utils::hash::djb2_hash(name)).unsigned_abs();
    let mut suffix = Vec::new();
    loop {
        suffix.push(b"0123456789abcdefghijklmnopqrstuvwxyz"[(hash % 36) as usize] as char);
        hash /= 36;
        if hash == 0 {
            break;
        }
    }
    suffix.reverse();
    format!(
        "{}-{}",
        &sanitized[..MAX_SANITIZED_LENGTH],
        suffix.into_iter().collect::<String>()
    )
}

/// Maps to: CC `utils/cachePaths.ts#getProjectDir:21-23`.
fn get_project_dir(cwd: &str) -> String {
    sanitize_path(cwd)
}

/// Native dependency adapter for `envPaths('claude-cli').cache` at CC
/// `utils/cachePaths.ts:6`. The env-paths implementation is outside the allowed
/// source tree; this conventional platform layout is not a claim of full
/// dependency equivalence. macOS layout is also observed in the original cache.
fn cache_root() -> io::Result<&'static PathBuf> {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    if let Some(root) = ROOT.get() {
        return Ok(root);
    }
    let home = std::env::home_dir()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Home directory unavailable"))?;
    #[cfg(target_os = "macos")]
    let root = home
        .join("Library")
        .join("Caches")
        .join("claude-cli-nodejs");
    #[cfg(target_os = "windows")]
    let root = crate::utils::process_env::var_os("LOCALAPPDATA")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join("AppData").join("Local"))
        .join("claude-cli-nodejs")
        .join("Cache");
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let root = crate::utils::process_env::var_os("XDG_CACHE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".cache"))
        .join("claude-cli-nodejs");
    Ok(ROOT.get_or_init(|| root))
}

/// Maps to: CC `utils/cachePaths.ts#CACHE_PATHS:25-38` (object carrier).
pub struct CachePaths;
pub const CACHE_PATHS: CachePaths = CachePaths;

impl CachePaths {
    /// Maps to: CC `utils/cachePaths.ts#CACHE_PATHS.baseLogs:26`.
    pub fn base_logs(&self) -> io::Result<PathBuf> {
        Ok(cache_root()?.join(get_project_dir(&std::env::current_dir()?.to_string_lossy())))
    }
    /// Maps to: CC `utils/cachePaths.ts#CACHE_PATHS.errors:27-28`.
    pub fn errors(&self) -> io::Result<PathBuf> {
        Ok(self.base_logs()?.join("errors"))
    }
    /// Maps to: CC `utils/cachePaths.ts#CACHE_PATHS.messages:29-30`.
    pub fn messages(&self) -> io::Result<PathBuf> {
        Ok(self.base_logs()?.join("messages"))
    }
    /// Maps to: CC `utils/cachePaths.ts#CACHE_PATHS.mcpLogs:31-37`.
    pub fn mcp_logs(&self, server_name: &str) -> io::Result<PathBuf> {
        Ok(self
            .base_logs()?
            .join(format!("mcp-logs-{}", sanitize_path(server_name))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_path_matches_official_utf16_and_long_hash_suffix() {
        assert_eq!(sanitize_path("/a:B/😀"), "-a-B---");
        assert_eq!(sanitize_path(&"A".repeat(200)), "A".repeat(200));
        let long = "A".repeat(201);
        assert_eq!(sanitize_path(&long), format!("{}-psycht", "A".repeat(200)));
        assert_ne!(sanitize_path(&long), sanitize_path(&format!("{long}B")));
    }

    #[test]
    fn cache_paths_matches_official_project_and_server_layout() {
        let base = CACHE_PATHS.base_logs().unwrap();
        assert_eq!(
            base.file_name().unwrap().to_string_lossy(),
            get_project_dir(&std::env::current_dir().unwrap().to_string_lossy())
        );
        assert_eq!(CACHE_PATHS.errors().unwrap(), base.join("errors"));
        assert_eq!(CACHE_PATHS.messages().unwrap(), base.join("messages"));
        assert_eq!(
            CACHE_PATHS.mcp_logs("stdio: server/😀").unwrap(),
            base.join("mcp-logs-stdio--server---")
        );
        #[cfg(target_os = "macos")]
        assert_eq!(
            base.parent().unwrap(),
            std::env::home_dir()
                .unwrap()
                .join("Library/Caches/claude-cli-nodejs")
        );
    }
}
