//! Excludes orphaned plugin versions from Grep/Glob results.
//!
//! Maps to: CC `utils/plugins/orphanedPluginFilter.ts:1-113`.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

const ORPHANED_AT_FILENAME: &str = ".orphaned_at";

/// Session-scoped cache, frozen until an explicit plugin reload clears it.
static CACHED_EXCLUSIONS: Mutex<Option<Vec<String>>> = Mutex::new(None);

fn normalize_for_compare(path: &Path) -> String {
    let mut normalized_path = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => match normalized_path.components().next_back() {
                Some(std::path::Component::Normal(_)) => {
                    normalized_path.pop();
                }
                Some(std::path::Component::RootDir | std::path::Component::Prefix(_)) => {}
                _ => normalized_path.push(".."),
            },
            component => normalized_path.push(component.as_os_str()),
        }
    }
    if normalized_path.as_os_str().is_empty() {
        normalized_path.push(".");
    }
    let mut normalized = normalized_path.display().to_string();
    if cfg!(windows) {
        normalized.make_ascii_lowercase();
    }
    normalized
}

fn paths_overlap(left: &Path, right: &Path) -> bool {
    let left = normalize_for_compare(left);
    let right = normalize_for_compare(right);
    let separator = std::path::MAIN_SEPARATOR;
    left == right
        || left == separator.to_string()
        || right == separator.to_string()
        || left.starts_with(&format!("{right}{separator}"))
        || right.starts_with(&format!("{left}{separator}"))
}

fn exclusions_from_markers(cache_path: &Path, markers: Vec<String>) -> Vec<String> {
    markers
        .into_iter()
        .map(PathBuf::from)
        .filter_map(|marker| marker.parent().map(Path::to_path_buf))
        .map(|version_dir| {
            if version_dir.is_absolute() {
                version_dir
                    .strip_prefix(cache_path)
                    .map(Path::to_path_buf)
                    .unwrap_or(version_dir)
            } else {
                version_dir
            }
        })
        .map(|relative| relative.display().to_string().replace('\\', "/"))
        .map(|relative| format!("!**/{relative}/**"))
        .collect()
}

/// Maps to CC `getGlobExclusionsForPluginCache(searchPath?)`.
pub fn get_glob_exclusions_for_plugin_cache(search_path: Option<&Path>) -> Vec<String> {
    let cache_path =
        crate::utils::plugins::plugin_directories::get_plugins_directory().join("cache");
    if search_path.is_some_and(|search_path| !paths_overlap(search_path, &cache_path)) {
        return Vec::new();
    }

    if let Some(cached) = CACHED_EXCLUSIONS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
    {
        return cached;
    }

    let exclusions = (|| {
        if !cache_path.is_dir() {
            return Vec::new();
        }
        let arguments = vec![
            "--files".to_string(),
            "--hidden".to_string(),
            "--no-ignore".to_string(),
            "--max-depth".to_string(),
            "4".to_string(),
            "--glob".to_string(),
            ORPHANED_AT_FILENAME.to_string(),
        ];
        let markers = crate::utils::ripgrep::rip_grep(
            &arguments,
            &cache_path,
            &crate::tool::AbortController::default(),
        )
        .unwrap_or_default();
        exclusions_from_markers(&cache_path, markers)
    })();

    *CACHED_EXCLUSIONS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(exclusions.clone());
    exclusions
}

/// Fire-and-forget startup warm matching CC `main.tsx`'s session freeze.
pub fn warm_plugin_cache_exclusions_in_background() {
    let _ = std::thread::Builder::new()
        .name("cometix-plugin-glob-exclusions".to_string())
        .spawn(|| {
            let _ = get_glob_exclusions_for_plugin_cache(None);
        });
}

/// Maps to CC `clearPluginCacheExclusions()`.
pub fn clear_plugin_cache_exclusions() {
    *CACHED_EXCLUSIONS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[test]
    fn orphaned_plugin_exclusion_shape_matches_official_paths() {
        let cache = Path::new("/plugins/cache");
        assert_eq!(
            exclusions_from_markers(
                cache,
                vec![
                    "market/plugin/1.0/.orphaned_at".to_string(),
                    "/plugins/cache/other/tool/2.0/.orphaned_at".to_string(),
                ],
            ),
            vec![
                "!**/market/plugin/1.0/**".to_string(),
                "!**/other/tool/2.0/**".to_string(),
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn plugin_cache_overlap_matches_official_prefix_and_root_rules() {
        assert!(paths_overlap(Path::new("/"), Path::new("/plugins/cache")));
        assert!(paths_overlap(
            Path::new("/plugins"),
            Path::new("/plugins/cache")
        ));
        assert!(paths_overlap(
            Path::new("/plugins/cache/market"),
            Path::new("/plugins/cache")
        ));
        assert!(!paths_overlap(
            Path::new("/workspace"),
            Path::new("/plugins/cache")
        ));
        assert!(!paths_overlap(
            Path::new("/workspace/plugins"),
            Path::new("plugins/cache")
        ));
    }

    #[test]
    fn orphaned_markers_exclude_version_directories_and_cache_until_cleared() {
        struct CacheGuard;
        impl Drop for CacheGuard {
            fn drop(&mut self) {
                clear_plugin_cache_exclusions();
            }
        }

        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let root = std::env::temp_dir().join(format!(
            "cometix-orphaned-plugin-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let cache = root.join("cache");
        let orphaned = cache.join("market/plugin/1.0");
        let active = cache.join("market/plugin/2.0");
        std::fs::create_dir_all(&orphaned).unwrap();
        std::fs::create_dir_all(&active).unwrap();
        std::fs::write(orphaned.join(ORPHANED_AT_FILENAME), "now").unwrap();
        std::fs::write(orphaned.join("old.txt"), "old").unwrap();
        std::fs::write(active.join("active.txt"), "active").unwrap();
        // Declared before the env guard so the exclusions are cleared after
        // the cache directory variable has been restored.
        let _cache = CacheGuard;
        let _env = EnvVarGuard::set("CLAUDE_CODE_PLUGIN_CACHE_DIR", &root);
        clear_plugin_cache_exclusions();

        assert_eq!(
            get_glob_exclusions_for_plugin_cache(Some(&cache)),
            vec!["!**/market/plugin/1.0/**"]
        );
        let first = crate::utils::glob::glob(
            "**/*.txt",
            &cache,
            100,
            0,
            &crate::tool::AbortController::default(),
            &crate::tool::ToolPermissionContext::default(),
        )
        .expect("plugin cache glob succeeds");
        assert_eq!(first.files, vec![active.join("active.txt")]);

        std::fs::remove_file(orphaned.join(ORPHANED_AT_FILENAME)).unwrap();
        assert_eq!(
            get_glob_exclusions_for_plugin_cache(Some(&cache)),
            vec!["!**/market/plugin/1.0/**"]
        );
        crate::utils::plugins::load_plugin_hooks::clear_plugin_hook_cache();
        assert_eq!(
            get_glob_exclusions_for_plugin_cache(Some(&cache)),
            vec!["!**/market/plugin/1.0/**"],
            "ordinary hook invalidation must not thaw session-frozen exclusions"
        );
        clear_plugin_cache_exclusions();
        let refreshed = crate::utils::glob::glob(
            "**/*.txt",
            &cache,
            100,
            0,
            &crate::tool::AbortController::default(),
            &crate::tool::ToolPermissionContext::default(),
        )
        .expect("refreshed plugin cache glob succeeds");
        assert_eq!(refreshed.files.len(), 2);
        assert!(refreshed.files.contains(&orphaned.join("old.txt")));
        assert!(refreshed.files.contains(&active.join("active.txt")));
        let _ = std::fs::remove_dir_all(root);
    }
}
