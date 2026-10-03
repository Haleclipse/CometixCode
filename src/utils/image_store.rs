//! Maps to: CC `utils/imageStore.ts`.
//!
//! The in-memory path cache is synchronous; image writes run only from caller
//! worker threads and honor the repository persistence/no-write seam.

use base64::Engine as _;
use std::collections::VecDeque;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

const IMAGE_STORE_DIR: &str = "image-cache";
const MAX_STORED_IMAGE_PATHS: usize = 200;

static STORED_IMAGE_PATHS: LazyLock<Mutex<VecDeque<(u64, PathBuf)>>> =
    LazyLock::new(|| Mutex::new(VecDeque::new()));

/// Rust subset of CC `PastedContent` consumed by `cacheImagePath(...)` and
/// `storeImage(...)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PastedImageContent {
    pub id: u64,
    pub media_type: Option<String>,
    pub data: Option<String>,
}

/// Maps to: CC `utils/imageStore.ts:15-20`.
pub fn get_image_store_dir() -> PathBuf {
    crate::utils::env_utils::get_claude_config_home_dir()
        .join(IMAGE_STORE_DIR)
        .join(crate::bootstrap::state::get_session_id())
}

/// Maps to: CC `utils/imageStore.ts:30-36`.
pub fn get_image_path(image_id: u64, media_type: Option<&str>) -> PathBuf {
    let extension = media_type
        .unwrap_or("image/png")
        .split('/')
        .nth(1)
        .filter(|value| !value.is_empty())
        .unwrap_or("png");
    get_image_store_dir().join(format!("{image_id}.{extension}"))
}

/// Maps to: CC `utils/imageStore.ts:38-49`.
pub fn cache_image_path(content: &PastedImageContent) -> PathBuf {
    let image_path = get_image_path(content.id, content.media_type.as_deref());
    cache_stored_image_path(content.id, image_path.clone());
    image_path
}

/// Maps to: CC `utils/imageStore.ts:51-79`.
///
/// Callers must invoke this outside a retained render frame. Normal production
/// writes by default; tests opt in and `COMETIX_WRITE_ENABLED=0` is an explicit
/// no-write seam through `is_session_write_enabled()`.
pub fn store_image(content: &PastedImageContent) -> Option<PathBuf> {
    if !crate::utils::session_storage::is_session_write_enabled() {
        return None;
    }
    let data = content.data.as_deref()?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .ok()?;
    let image_path = get_image_path(content.id, content.media_type.as_deref());
    std::fs::create_dir_all(image_path.parent()?).ok()?;

    let mut options = std::fs::OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(&image_path).ok()?;
    file.write_all(&bytes).ok()?;
    file.sync_data().ok()?;
    cache_stored_image_path(content.id, image_path.clone());
    Some(image_path)
}

/// Test/adapter seam for entries that have already been written by another
/// boundary. This preserves CC's in-memory `storedImagePaths` behavior without
/// performing disk I/O here.
pub fn cache_stored_image_path(image_id: u64, image_path: impl Into<PathBuf>) {
    let mut paths = STORED_IMAGE_PATHS
        .lock()
        .expect("stored image path cache lock poisoned");
    if let Some(index) = paths.iter().position(|(id, _)| *id == image_id) {
        paths.remove(index);
    }
    while paths.len() >= MAX_STORED_IMAGE_PATHS {
        paths.pop_front();
    }
    paths.push_back((image_id, image_path.into()));
}

/// Maps to: CC `utils/imageStore.ts:101-106`.
pub fn get_stored_image_path(image_id: u64) -> Option<PathBuf> {
    STORED_IMAGE_PATHS.lock().ok().and_then(|paths| {
        paths
            .iter()
            .find(|(id, _)| *id == image_id)
            .map(|(_, path)| path.clone())
    })
}

/// Maps to: CC `utils/imageStore.ts:108-113`.
pub fn clear_stored_image_paths() {
    if let Ok(mut paths) = STORED_IMAGE_PATHS.lock() {
        paths.clear();
    }
}

pub fn image_path_to_file_url(path: &Path) -> String {
    path_to_file_url(path)
}

fn path_to_file_url(path: &Path) -> String {
    let path_string = path.to_string_lossy().replace('\\', "/");
    let mut normalized = path_string.as_str();
    let mut prefix = "file://";
    #[cfg(windows)]
    {
        if normalized.starts_with('/') {
            prefix = "file://";
        } else {
            prefix = "file:///";
        }
    }
    #[cfg(not(windows))]
    {
        if !normalized.starts_with('/') {
            prefix = "file:///";
        }
    }
    let encoded = percent_encode_path(normalized);
    format!("{prefix}{encoded}")
}

fn percent_encode_path(path: &str) -> String {
    let mut encoded = String::new();
    for byte in path.as_bytes() {
        let ch = *byte as char;
        if matches!(ch, 'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' | '/') {
            encoded.push(ch);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    struct SessionRestore(String);

    impl SessionRestore {
        fn set(value: &str) -> Self {
            let previous = crate::bootstrap::state::get_session_id();
            crate::bootstrap::state::set_session_id(value);
            Self(previous)
        }
    }

    impl Drop for SessionRestore {
        fn drop(&mut self) {
            crate::bootstrap::state::set_session_id(self.0.clone());
        }
    }

    #[test]
    fn image_store_cache_path_matches_official_session_directory_shape() {
        let _env_lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        clear_stored_image_paths();
        let tmp =
            std::env::temp_dir().join(format!("cometix-image-store-{}", uuid::Uuid::new_v4()));
        let _config_restore = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &tmp);
        let _session_restore = SessionRestore::set("session-1");

        let path = cache_image_path(&PastedImageContent {
            id: 7,
            media_type: Some("image/jpeg".to_string()),
            data: None,
        });

        assert_eq!(
            path,
            tmp.join("image-cache").join("session-1").join("7.jpeg")
        );
        assert_eq!(get_stored_image_path(7), Some(path));
    }

    #[test]
    fn image_store_defaults_to_png_extension_and_clears_cache() {
        clear_stored_image_paths();
        let path = cache_image_path(&PastedImageContent {
            id: 8,
            media_type: None,
            data: None,
        });
        assert!(path.ends_with("8.png"));
        assert!(get_stored_image_path(8).is_some());
        clear_stored_image_paths();
        assert!(get_stored_image_path(8).is_none());
    }

    #[test]
    fn image_store_evicts_oldest_entries_at_official_cap() {
        clear_stored_image_paths();
        for id in 0..=MAX_STORED_IMAGE_PATHS as u64 {
            cache_stored_image_path(id, PathBuf::from(format!("/tmp/{id}.png")));
        }
        assert!(get_stored_image_path(0).is_none());
        assert_eq!(
            get_stored_image_path(MAX_STORED_IMAGE_PATHS as u64),
            Some(PathBuf::from(format!(
                "/tmp/{}.png",
                MAX_STORED_IMAGE_PATHS
            )))
        );
        clear_stored_image_paths();
    }

    #[test]
    fn image_store_file_url_percent_encodes_paths() {
        assert_eq!(
            image_path_to_file_url(Path::new("/tmp/Image #1.png")),
            "file:///tmp/Image%20%231.png"
        );
    }

    #[test]
    fn store_image_writes_private_decoded_bytes_when_persistence_is_enabled() {
        let _env_lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        clear_stored_image_paths();
        let tmp = std::env::temp_dir().join(format!(
            "cometix-image-store-write-{}",
            uuid::Uuid::new_v4()
        ));
        let _config_restore = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &tmp);
        let _write_restore = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let _session_restore = SessionRestore::set("session-write");
        let image = PastedImageContent {
            id: 11,
            media_type: Some("image/png".to_string()),
            data: Some("aGVsbG8=".to_string()),
        };

        let path = store_image(&image).expect("image should be stored");
        assert_eq!(std::fs::read(&path).unwrap(), b"hello");
        assert_eq!(get_stored_image_path(11), Some(path.clone()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let _ = std::fs::remove_dir_all(tmp);
    }

    #[test]
    fn store_image_honors_explicit_no_write_seam() {
        let _env_lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        clear_stored_image_paths();
        let tmp = std::env::temp_dir().join(format!(
            "cometix-image-store-nowrite-{}",
            uuid::Uuid::new_v4()
        ));
        let _config_restore = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &tmp);
        let _write_restore = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "0");
        let _session_restore = SessionRestore::set("session-nowrite");
        let image = PastedImageContent {
            id: 12,
            media_type: Some("image/png".to_string()),
            data: Some("aGVsbG8=".to_string()),
        };

        assert_eq!(store_image(&image), None);
        assert!(!get_image_path(12, Some("image/png")).exists());
    }
}
