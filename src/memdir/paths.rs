//! Auto-memory path resolution.
//!
//! Maps to CC `memdir/paths.ts`.

use std::path::{Component, Path, PathBuf};

use chrono::Datelike;

use crate::utils::env_utils::{is_env_defined_falsy, is_env_truthy};
use crate::utils::process_env::JsTruthy;
#[cfg(test)]
use crate::utils::settings::types::SettingsJson;

const AUTO_MEM_DIRNAME: &str = "memory";
const AUTO_MEM_ENTRYPOINT_NAME: &str = "MEMORY.md";

/// Maps to CC `memdir/paths.ts:30-55` `isAutoMemoryEnabled()`.
pub fn is_auto_memory_enabled() -> bool {
    let env_val = crate::utils::process_env::var("CLAUDE_CODE_DISABLE_AUTO_MEMORY");
    if is_env_truthy(env_val.as_deref()) {
        return false;
    }
    if is_env_defined_falsy(env_val.as_deref()) {
        return true;
    }
    if is_env_truthy(crate::utils::process_env::var("CLAUDE_CODE_SIMPLE").as_deref()) {
        return false;
    }
    if is_env_truthy(crate::utils::process_env::var("CLAUDE_CODE_REMOTE").as_deref())
        && crate::utils::process_env::var("CLAUDE_CODE_REMOTE_MEMORY_DIR")
            .truthy()
            .is_none()
    {
        return false;
    }
    crate::utils::settings::get_initial_settings()
        .auto_memory_enabled
        .unwrap_or(true)
}

/// Maps to CC `memdir/paths.ts:85-90` `getMemoryBaseDir()`.
pub fn get_memory_base_dir() -> PathBuf {
    crate::utils::process_env::var("CLAUDE_CODE_REMOTE_MEMORY_DIR")
        .truthy()
        .map(PathBuf::from)
        .unwrap_or_else(crate::utils::env_utils::get_claude_config_home_dir)
}

/// Maps to CC `memdir/paths.ts:161-166` `getAutoMemPathOverride()`.
fn get_auto_mem_path_override() -> Option<PathBuf> {
    validate_memory_path(
        crate::utils::process_env::var("CLAUDE_COWORK_MEMORY_PATH_OVERRIDE").as_deref(),
        false,
    )
}

/// Maps to CC `memdir/paths.ts:179-186` `getAutoMemPathSetting()`: first
/// defined wins, including an invalid higher-priority value. Shared project
/// settings are deliberately excluded so a repository cannot redirect memory
/// into a sensitive path. Known deviation: sources disabled by
/// `--setting-sources` are skipped here, while CC's `getSettingsForSource`
/// reads them regardless (`settings.ts:309-367`).
fn get_auto_mem_path_setting() -> Option<PathBuf> {
    use crate::utils::settings::constants::{SettingSource, is_setting_source_enabled};
    use crate::utils::settings::get_settings_for_source;

    let dir = [
        SettingSource::Policy,
        SettingSource::Flag,
        SettingSource::Local,
        SettingSource::User,
    ]
    .into_iter()
    .filter(|source| is_setting_source_enabled(*source))
    .find_map(|source| {
        get_settings_for_source(source).and_then(|settings| settings.auto_memory_directory)
    });
    validate_memory_path(dir.as_deref(), true)
}

/// Maps to CC `memdir/paths.ts:194-196` `hasAutoMemPathOverride()`.
pub fn has_auto_mem_path_override() -> bool {
    get_auto_mem_path_override().is_some()
}

/// Maps to CC `memdir/paths.ts:203-205` `getAutoMemBase()`. CC starts from
/// `getProjectRoot()`, which this port does not have yet; the original cwd
/// stands in for it.
fn get_auto_mem_base() -> PathBuf {
    let project_root = crate::bootstrap::state::get_original_cwd();
    crate::utils::git::find_canonical_git_root(&project_root).unwrap_or(project_root)
}

/// Maps to CC `memdir/paths.ts:223-235` `getAutoMemPath()`. CC memoizes it on
/// the project root; this port recomputes it.
pub fn get_auto_mem_path() -> PathBuf {
    if let Some(path) = get_auto_mem_path_override().or_else(get_auto_mem_path_setting) {
        return path;
    }
    get_memory_base_dir()
        .join("projects")
        .join(sanitize_auto_memory_project_key(&get_auto_mem_base()))
        .join(AUTO_MEM_DIRNAME)
}

/// Maps to CC `memdir/paths.ts:246-251` `getAutoMemDailyLogPath(date)`.
pub fn get_auto_mem_daily_log_path(date: chrono::NaiveDate) -> PathBuf {
    let yyyy = format!("{:04}", date.year());
    let mm = format!("{:02}", date.month());
    let dd = format!("{:02}", date.day());
    get_auto_mem_path()
        .join("logs")
        .join(&yyyy)
        .join(&mm)
        .join(format!("{yyyy}-{mm}-{dd}.md"))
}

/// Maps to CC `memdir/paths.ts:257-259` `getAutoMemEntrypoint()`.
pub fn get_auto_mem_entrypoint() -> PathBuf {
    get_auto_mem_path().join(AUTO_MEM_ENTRYPOINT_NAME)
}

/// Maps to CC `memdir/paths.ts:274-278` `isAutoMemPath(absolutePath)`.
pub fn is_auto_mem_path(absolute_path: &Path) -> bool {
    let normalized_path = normalize_path_lexically(absolute_path.to_path_buf());
    normalized_path.starts_with(get_auto_mem_path())
}

/// Maps to CC `memdir/paths.ts:109-150` `validateMemoryPath(raw, expandTilde)`
/// — `raw` is `string | undefined` and `if (!raw)` folds the unset and empty
/// cases inside the owner, so callers pass the environment value through.
fn validate_memory_path(raw: Option<&str>, expand_tilde: bool) -> Option<PathBuf> {
    let raw = raw?;
    if raw.is_empty() || raw.contains('\0') {
        return None;
    }

    let expanded = if expand_tilde && (raw.starts_with("~/") || raw.starts_with("~\\")) {
        let rest = &raw[2..];
        let rest_normalized = normalize_path_lexically(PathBuf::from(rest));
        let rest_text = rest_normalized.display().to_string();
        if rest_text.is_empty() || rest_text == "." || rest_text == ".." {
            return None;
        }
        crate::utils::node_os::homedir().join(rest)
    } else {
        PathBuf::from(raw)
    };

    let normalized = normalize_path_lexically(expanded);
    let normalized_text = normalized.display().to_string();
    if !normalized.is_absolute()
        || normalized_text.len() < 3
        || normalized_text.starts_with("//")
        || normalized_text.starts_with("\\\\")
    {
        return None;
    }

    Some(normalized)
}

fn normalize_path_lexically(path: PathBuf) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
        }
    }
    normalized
}

fn sanitize_auto_memory_project_key(path: &Path) -> String {
    let mut sanitized = path
        .display()
        .to_string()
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect::<String>();
    const MAX_SANITIZED_LENGTH: usize = 240;
    if sanitized.len() > MAX_SANITIZED_LENGTH {
        sanitized.truncate(MAX_SANITIZED_LENGTH);
    }
    sanitized
}

/// Test-only: seeds the settings this module reads. `initial` is what
/// `getInitialSettings()` returns (`isAutoMemoryEnabled`); `user` is the only
/// trusted source `getAutoMemPathSetting` finds settings in, the other three
/// are cached as having none so the machine's own managed settings cannot
/// reach in. The caller holds `TEST_ENV_LOCK`, whose guard resets these
/// caches when it is taken and when it is released.
#[cfg(test)]
pub(crate) fn seed_settings(initial: SettingsJson, user: Option<SettingsJson>) {
    use crate::utils::settings::constants::SettingSource;
    use crate::utils::settings::settings_cache;

    settings_cache::set_session_settings_cache(
        crate::utils::settings::validation::SettingsWithErrors {
            settings: initial,
            ..Default::default()
        },
    );
    for source in [
        SettingSource::Policy,
        SettingSource::Flag,
        SettingSource::Local,
    ] {
        settings_cache::set_cached_settings_for_source(source, None);
    }
    settings_cache::set_cached_settings_for_source(SettingSource::User, user);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{
        EnvVarGuard, HOME_VAR, PinnedProjectDir, TEST_ENV_LOCK, TestEnvGuard,
    };

    fn default_settings() -> SettingsJson {
        SettingsJson::default()
    }

    /// Pins the inputs `isAutoMemoryEnabled` and `getAutoMemPath` read: the
    /// auto-memory variables (cleared), the config root, the home directory,
    /// the project root, and (through [`seed_settings`]) the settings caches.
    /// Fields drop in declaration order, so the lock goes last; its guard
    /// resets the seeded caches.
    struct AutoMemoryInputs {
        _project: PinnedProjectDir,
        _env: Vec<EnvVarGuard>,
        _lock: TestEnvGuard<'static>,
    }

    const CONFIG_HOME: &str = "/tmp/claude-config";
    const HOME: &str = "/home/tester";
    const PROJECT: &str = "/workspace/project";

    fn pin_auto_memory_inputs() -> AutoMemoryInputs {
        let lock = TEST_ENV_LOCK.lock().unwrap();
        // One guard per variable: a Vec drops front to back, so a second
        // guard on the same key would restore in the wrong order.
        let env = AUTO_MEMORY_ENV_KEYS
            .into_iter()
            .map(|key| match key {
                "CLAUDE_CONFIG_DIR" => EnvVarGuard::set(key, CONFIG_HOME),
                _ => EnvVarGuard::unset(key),
            })
            .chain([EnvVarGuard::set(HOME_VAR, HOME)])
            .collect::<Vec<_>>();
        let project = PinnedProjectDir::at(PROJECT);
        seed_settings(default_settings(), None);
        AutoMemoryInputs {
            _project: project,
            _env: env,
            _lock: lock,
        }
    }

    fn default_auto_mem_path() -> PathBuf {
        PathBuf::from(CONFIG_HOME)
            .join("projects")
            .join("-workspace-project")
            .join(AUTO_MEM_DIRNAME)
    }

    const AUTO_MEMORY_ENV_KEYS: [&str; 7] = [
        "CLAUDE_COWORK_MEMORY_PATH_OVERRIDE",
        "CLAUDE_CODE_DISABLE_AUTO_MEMORY",
        "CLAUDE_CODE_SIMPLE",
        "CLAUDE_CODE_REMOTE",
        "CLAUDE_CODE_REMOTE_MEMORY_DIR",
        "CLAUDE_CONFIG_DIR",
        "CLAUDE_CODE_MANAGED_SETTINGS_PATH",
    ];

    struct IsolatedAutoMemoryEnv {
        _env: Vec<EnvVarGuard>,
        _lock: TestEnvGuard<'static>,
    }

    fn isolated_auto_memory_env() -> IsolatedAutoMemoryEnv {
        let lock = TEST_ENV_LOCK.lock().unwrap();
        let env = AUTO_MEMORY_ENV_KEYS
            .into_iter()
            .map(EnvVarGuard::unset)
            .collect();
        IsolatedAutoMemoryEnv {
            _env: env,
            _lock: lock,
        }
    }

    #[test]
    fn auto_memory_enabled_matches_official_env_and_settings_precedence() {
        let _inputs = pin_auto_memory_inputs();
        let disabled_in_settings = SettingsJson {
            auto_memory_enabled: Some(false),
            ..default_settings()
        };
        seed_settings(disabled_in_settings.clone(), None);
        assert!(!is_auto_memory_enabled());

        seed_settings(
            SettingsJson {
                auto_memory_enabled: Some(true),
                ..default_settings()
            },
            None,
        );
        assert!(is_auto_memory_enabled());
        {
            let _env = EnvVarGuard::set("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "true");
            assert!(!is_auto_memory_enabled());
        }

        seed_settings(disabled_in_settings, None);
        {
            let _env = EnvVarGuard::set("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "false");
            assert!(is_auto_memory_enabled());
        }

        seed_settings(default_settings(), None);
        {
            let _env = EnvVarGuard::set("CLAUDE_CODE_SIMPLE", "1");
            assert!(!is_auto_memory_enabled());
        }
        {
            let _env = EnvVarGuard::set("CLAUDE_CODE_REMOTE", "1");
            assert!(!is_auto_memory_enabled());
        }
    }

    #[test]
    fn get_auto_mem_path_matches_official_override_setting_and_default_order() {
        let _inputs = pin_auto_memory_inputs();
        assert_eq!(get_auto_mem_path(), default_auto_mem_path());

        seed_settings(
            default_settings(),
            Some(SettingsJson {
                auto_memory_directory: Some("~/memory-dir".to_string()),
                ..default_settings()
            }),
        );
        assert_eq!(get_auto_mem_path(), PathBuf::from(HOME).join("memory-dir"));

        let _override = EnvVarGuard::set("CLAUDE_COWORK_MEMORY_PATH_OVERRIDE", "/mnt/memory");
        assert_eq!(get_auto_mem_path(), PathBuf::from("/mnt/memory"));
    }

    #[test]
    fn programmatic_memory_override_is_not_trimmed_into_a_valid_path() {
        let _inputs = pin_auto_memory_inputs();
        let _override = EnvVarGuard::set("CLAUDE_COWORK_MEMORY_PATH_OVERRIDE", " /mnt/memory ");
        assert_eq!(get_auto_mem_path(), default_auto_mem_path());
    }

    #[test]
    fn project_settings_cannot_redirect_auto_memory_directory() {
        let _guard = isolated_auto_memory_env();
        let root = std::env::temp_dir().join(format!(
            "cometix-auto-memory-trust-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let project = root.join("project");
        let config = root.join("config");
        let malicious = root.join("project-selected-sensitive-path");
        std::fs::create_dir_all(project.join(".claude")).unwrap();
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(
            project.join(".claude/settings.json"),
            serde_json::json!({"autoMemoryDirectory": malicious}).to_string(),
        )
        .unwrap();
        // `projectSettings` is rooted at the original cwd, not the process cwd.
        let _project = PinnedProjectDir::at(&project);
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config);
        let _managed = EnvVarGuard::set(
            "CLAUDE_CODE_MANAGED_SETTINGS_PATH",
            root.join("missing-managed-root"),
        );
        // The redirect is really in the merged settings, so only the
        // trusted-source chain keeps it out of the path.
        assert_eq!(
            crate::utils::settings::get_initial_settings()
                .auto_memory_directory
                .as_deref(),
            malicious.to_str()
        );

        let resolved = get_auto_mem_path();
        assert_ne!(resolved, malicious);
        assert!(resolved.starts_with(config.join("projects")));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn has_auto_mem_path_override_requires_valid_absolute_env() {
        let _guard = isolated_auto_memory_env();
        for (value, valid) in [
            ("/mnt/memory", true),
            ("relative/memory", false),
            ("/", false),
        ] {
            let _override = EnvVarGuard::set("CLAUDE_COWORK_MEMORY_PATH_OVERRIDE", value);
            assert_eq!(has_auto_mem_path_override(), valid, "{value}");
        }
    }

    #[test]
    fn auto_mem_entrypoint_and_daily_log_paths_match_official_shape() {
        let _guard = isolated_auto_memory_env();
        crate::utils::process_env::set("CLAUDE_COWORK_MEMORY_PATH_OVERRIDE", "/tmp/cometix-memory");
        let date = chrono::NaiveDate::from_ymd_opt(2026, 7, 3).unwrap();

        assert_eq!(
            get_auto_mem_entrypoint(),
            PathBuf::from("/tmp/cometix-memory").join(AUTO_MEM_ENTRYPOINT_NAME)
        );
        assert_eq!(
            get_auto_mem_daily_log_path(date),
            PathBuf::from("/tmp/cometix-memory")
                .join("logs")
                .join("2026")
                .join("07")
                .join("2026-07-03.md")
        );
    }

    #[test]
    fn is_auto_mem_path_matches_normalized_descendants_only() {
        let _guard = isolated_auto_memory_env();
        crate::utils::process_env::set("CLAUDE_COWORK_MEMORY_PATH_OVERRIDE", "/tmp/cometix-memory");

        assert!(is_auto_mem_path(Path::new("/tmp/cometix-memory/topic.md")));
        assert!(is_auto_mem_path(Path::new(
            "/tmp/cometix-memory/sub/../topic.md"
        )));
        assert!(!is_auto_mem_path(Path::new(
            "/tmp/cometix-memory2/topic.md"
        )));
    }
}
