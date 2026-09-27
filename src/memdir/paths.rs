//! Auto-memory path resolution.
//!
//! Maps to CC `memdir/paths.ts`.

use std::path::{Component, Path, PathBuf};

use chrono::Datelike;

use crate::utils::env_utils::{is_env_defined_falsy, is_env_truthy, truthy_env_value};
use crate::utils::settings::types::SettingsJson;

const AUTO_MEM_DIRNAME: &str = "memory";
const AUTO_MEM_ENTRYPOINT_NAME: &str = "MEMORY.md";

/// Maps to CC `memdir/paths.ts` `isAutoMemoryEnabled()`.
pub fn is_auto_memory_enabled(settings: &SettingsJson) -> bool {
    is_auto_memory_enabled_with_env(settings, &|key| crate::utils::process_env::var(key).ok())
}

pub fn is_auto_memory_enabled_with_env(
    settings: &SettingsJson,
    get_env: &impl Fn(&str) -> Option<String>,
) -> bool {
    let disable_auto_memory = get_env("CLAUDE_CODE_DISABLE_AUTO_MEMORY");
    if is_env_truthy(disable_auto_memory.as_deref()) {
        return false;
    }
    if is_env_defined_falsy(disable_auto_memory.as_deref()) {
        return true;
    }
    if is_env_truthy(get_env("CLAUDE_CODE_SIMPLE").as_deref()) {
        return false;
    }
    // CC: `isEnvTruthy(CLAUDE_CODE_REMOTE) && !process.env.CLAUDE_CODE_REMOTE_MEMORY_DIR`.
    if is_env_truthy(get_env("CLAUDE_CODE_REMOTE").as_deref())
        && truthy_env_value(get_env("CLAUDE_CODE_REMOTE_MEMORY_DIR")).is_none()
    {
        return false;
    }
    settings.auto_memory_enabled.unwrap_or(true)
}

/// Maps to CC `memdir/paths.ts` `hasAutoMemPathOverride()`.
pub fn has_auto_mem_path_override() -> bool {
    has_auto_mem_path_override_with_env(&|key| crate::utils::process_env::var(key).ok())
}

pub fn has_auto_mem_path_override_with_env(get_env: &impl Fn(&str) -> Option<String>) -> bool {
    validate_memory_path(
        get_env("CLAUDE_COWORK_MEMORY_PATH_OVERRIDE").as_deref(),
        false,
        None,
    )
    .is_some()
}

fn trusted_auto_memory_directory() -> Option<String> {
    use crate::utils::settings::constants::{SettingSource, is_setting_source_enabled};
    use crate::utils::settings::get_settings_for_source;

    // Maps to `getAutoMemPathSetting()`: first defined wins, including an
    // invalid higher-priority value. Shared project settings are deliberately
    // excluded so a repository cannot redirect memory into a sensitive path.
    [
        SettingSource::Policy,
        SettingSource::Flag,
        SettingSource::Local,
        SettingSource::User,
    ]
    .into_iter()
    .filter(|source| is_setting_source_enabled(*source))
    .find_map(|source| {
        get_settings_for_source(source).and_then(|settings| settings.auto_memory_directory)
    })
}

pub(crate) fn settings_with_trusted_auto_memory_directory(
    mut settings: SettingsJson,
) -> SettingsJson {
    settings.auto_memory_directory = trusted_auto_memory_directory();
    settings
}

/// Maps to CC `memdir/paths.ts` `getAutoMemPath()`.
pub fn get_auto_mem_path(settings: &SettingsJson) -> PathBuf {
    let cwd = crate::bootstrap::state::get_original_cwd();
    let config_home = crate::utils::config::get_config_home();
    let home = crate::utils::process_env::var("HOME")
        .or_else(|_| crate::utils::process_env::var("USERPROFILE"))
        .ok()
        .map(PathBuf::from);
    get_auto_mem_path_with_env(settings, &cwd, &config_home, home.as_deref(), &|key| {
        crate::utils::process_env::var(key).ok()
    })
}

pub fn get_auto_mem_path_from_trusted_sources() -> PathBuf {
    let settings =
        settings_with_trusted_auto_memory_directory(crate::utils::settings::get_initial_settings());
    get_auto_mem_path(&settings)
}

pub fn get_auto_mem_entrypoint_from_trusted_sources() -> PathBuf {
    get_auto_mem_path_from_trusted_sources().join(AUTO_MEM_ENTRYPOINT_NAME)
}

pub fn is_auto_mem_path_from_trusted_sources(absolute_path: &Path) -> bool {
    let normalized_path = normalize_path_lexically(absolute_path.to_path_buf());
    normalized_path.starts_with(get_auto_mem_path_from_trusted_sources())
}

pub fn get_auto_mem_path_with_env(
    settings: &SettingsJson,
    cwd: &Path,
    config_home: &Path,
    home: Option<&Path>,
    get_env: &impl Fn(&str) -> Option<String>,
) -> PathBuf {
    if let Some(override_path) = validate_memory_path(
        get_env("CLAUDE_COWORK_MEMORY_PATH_OVERRIDE").as_deref(),
        false,
        home,
    ) {
        return override_path;
    }

    if let Some(setting_path) =
        validate_memory_path(settings.auto_memory_directory.as_deref(), true, home)
    {
        return setting_path;
    }

    default_auto_memory_path(cwd, config_home, get_env)
}

/// Maps to CC `memdir/paths.ts` `getMemoryBaseDir()`.
pub fn get_memory_base_dir() -> PathBuf {
    memory_base_dir(&crate::utils::config::get_config_home(), &|key| {
        crate::utils::process_env::var(key).ok()
    })
}

fn memory_base_dir(config_home: &Path, get_env: &impl Fn(&str) -> Option<String>) -> PathBuf {
    truthy_env_value(get_env("CLAUDE_CODE_REMOTE_MEMORY_DIR"))
        .map(PathBuf::from)
        .unwrap_or_else(|| config_home.to_path_buf())
}

fn default_auto_memory_path(
    cwd: &Path,
    config_home: &Path,
    get_env: &impl Fn(&str) -> Option<String>,
) -> PathBuf {
    memory_base_dir(config_home, get_env)
        .join("projects")
        .join(sanitize_auto_memory_project_key(&git_root_or_cwd(cwd)))
        .join(AUTO_MEM_DIRNAME)
}

/// Maps to CC `memdir/paths.ts` `getAutoMemDailyLogPath(...)`.
pub fn get_auto_mem_daily_log_path(settings: &SettingsJson, date: chrono::NaiveDate) -> PathBuf {
    let yyyy = format!("{:04}", date.year());
    let mm = format!("{:02}", date.month());
    let dd = format!("{:02}", date.day());
    get_auto_mem_path(settings)
        .join("logs")
        .join(&yyyy)
        .join(&mm)
        .join(format!("{yyyy}-{mm}-{dd}.md"))
}

/// Maps to CC `memdir/paths.ts` `getAutoMemEntrypoint()`.
pub fn get_auto_mem_entrypoint(settings: &SettingsJson) -> PathBuf {
    get_auto_mem_path(settings).join(AUTO_MEM_ENTRYPOINT_NAME)
}

/// Maps to CC `memdir/paths.ts` `isAutoMemPath(...)`.
pub fn is_auto_mem_path(settings: &SettingsJson, absolute_path: &Path) -> bool {
    let normalized_path = normalize_path_lexically(absolute_path.to_path_buf());
    normalized_path.starts_with(get_auto_mem_path(settings))
}

/// Maps to CC `memdir/paths.ts:109-115` `validateMemoryPath(raw, expandTilde)`
/// — `raw` is `string | undefined` and `if (!raw)` folds the unset and empty
/// cases inside the owner, so callers pass the environment value through.
fn validate_memory_path(
    raw: Option<&str>,
    expand_tilde: bool,
    home: Option<&Path>,
) -> Option<PathBuf> {
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
        home?.join(rest)
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

fn git_root_or_cwd(cwd: &Path) -> PathBuf {
    crate::utils::git::find_canonical_git_root(cwd).unwrap_or_else(|| cwd.to_path_buf())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn default_settings() -> SettingsJson {
        SettingsJson::default()
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
        _env: Vec<crate::utils::env_utils::EnvVarGuard>,
        _lock: crate::utils::env_utils::TestEnvGuard<'static>,
    }

    fn isolated_auto_memory_env() -> IsolatedAutoMemoryEnv {
        let lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let env = AUTO_MEMORY_ENV_KEYS
            .into_iter()
            .map(crate::utils::env_utils::EnvVarGuard::unset)
            .collect();
        IsolatedAutoMemoryEnv {
            _env: env,
            _lock: lock,
        }
    }

    #[test]
    fn auto_memory_enabled_matches_official_env_and_settings_precedence() {
        let mut settings = default_settings();
        settings.auto_memory_enabled = Some(false);
        let no_env = |_: &str| None;
        assert!(!is_auto_memory_enabled_with_env(&settings, &no_env));

        settings.auto_memory_enabled = Some(true);
        assert!(is_auto_memory_enabled_with_env(&settings, &no_env));

        let disabled_by_env =
            |key: &str| (key == "CLAUDE_CODE_DISABLE_AUTO_MEMORY").then(|| "true".to_string());
        assert!(!is_auto_memory_enabled_with_env(
            &settings,
            &disabled_by_env
        ));

        let forced_by_env =
            |key: &str| (key == "CLAUDE_CODE_DISABLE_AUTO_MEMORY").then(|| "false".to_string());
        settings.auto_memory_enabled = Some(false);
        assert!(is_auto_memory_enabled_with_env(&settings, &forced_by_env));

        let bare_mode = |key: &str| (key == "CLAUDE_CODE_SIMPLE").then(|| "1".to_string());
        assert!(!is_auto_memory_enabled_with_env(
            &default_settings(),
            &bare_mode
        ));

        let remote_without_memory =
            |key: &str| (key == "CLAUDE_CODE_REMOTE").then(|| "1".to_string());
        assert!(!is_auto_memory_enabled_with_env(
            &default_settings(),
            &remote_without_memory
        ));
    }

    #[test]
    fn get_auto_mem_path_matches_official_override_setting_and_default_order() {
        let cwd = PathBuf::from("/workspace/project");
        let config_home = PathBuf::from("/tmp/claude-config");
        let home = PathBuf::from("/home/tester");
        let no_env = |_: &str| None;
        let default_path = get_auto_mem_path_with_env(
            &default_settings(),
            &cwd,
            &config_home,
            Some(&home),
            &no_env,
        );
        assert_eq!(
            default_path,
            config_home
                .join("projects")
                .join("-workspace-project")
                .join(AUTO_MEM_DIRNAME)
        );

        let mut settings = default_settings();
        settings.auto_memory_directory = Some("~/memory-dir".to_string());
        assert_eq!(
            get_auto_mem_path_with_env(&settings, &cwd, &config_home, Some(&home), &no_env),
            home.join("memory-dir")
        );

        let override_env = |key: &str| {
            (key == "CLAUDE_COWORK_MEMORY_PATH_OVERRIDE").then(|| "/mnt/memory".to_string())
        };
        assert_eq!(
            get_auto_mem_path_with_env(&settings, &cwd, &config_home, Some(&home), &override_env),
            PathBuf::from("/mnt/memory")
        );
    }

    #[test]
    fn programmatic_memory_override_is_not_trimmed_into_a_valid_path() {
        let settings = default_settings();
        let cwd = PathBuf::from("/workspace/project");
        let config_home = PathBuf::from("/tmp/claude-config");
        let malformed = |key: &str| {
            (key == "CLAUDE_COWORK_MEMORY_PATH_OVERRIDE").then(|| " /mnt/memory ".to_string())
        };
        assert_eq!(
            get_auto_mem_path_with_env(&settings, &cwd, &config_home, None, &malformed),
            config_home
                .join("projects")
                .join("-workspace-project")
                .join(AUTO_MEM_DIRNAME)
        );
    }

    #[test]
    fn project_settings_cannot_redirect_auto_memory_directory() {
        let _guard = isolated_auto_memory_env();
        let original_cwd = std::env::current_dir().unwrap();
        struct CwdRestore(PathBuf);
        impl Drop for CwdRestore {
            fn drop(&mut self) {
                let _ = std::env::set_current_dir(&self.0);
            }
        }
        let _cwd_restore = CwdRestore(original_cwd);
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
        std::env::set_current_dir(&project).unwrap();
        crate::utils::process_env::set("CLAUDE_CONFIG_DIR", &config);
        crate::utils::process_env::set(
            "CLAUDE_CODE_MANAGED_SETTINGS_PATH",
            root.join("missing-managed-settings.json"),
        );

        let resolved = get_auto_mem_path_from_trusted_sources();
        assert_ne!(resolved, malicious);
        assert!(resolved.starts_with(config.join("projects")));
        std::env::set_current_dir(&_cwd_restore.0).unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn has_auto_mem_path_override_requires_valid_absolute_env() {
        let valid = |key: &str| {
            (key == "CLAUDE_COWORK_MEMORY_PATH_OVERRIDE").then(|| "/mnt/memory".to_string())
        };
        assert!(has_auto_mem_path_override_with_env(&valid));

        let relative = |key: &str| {
            (key == "CLAUDE_COWORK_MEMORY_PATH_OVERRIDE").then(|| "relative/memory".to_string())
        };
        assert!(!has_auto_mem_path_override_with_env(&relative));

        let root =
            |key: &str| (key == "CLAUDE_COWORK_MEMORY_PATH_OVERRIDE").then(|| "/".to_string());
        assert!(!has_auto_mem_path_override_with_env(&root));
    }

    #[test]
    fn auto_mem_entrypoint_and_daily_log_paths_match_official_shape() {
        let _guard = isolated_auto_memory_env();
        crate::utils::process_env::set("CLAUDE_COWORK_MEMORY_PATH_OVERRIDE", "/tmp/cometix-memory");
        let settings = default_settings();
        let date = chrono::NaiveDate::from_ymd_opt(2026, 7, 3).unwrap();

        assert_eq!(
            get_auto_mem_entrypoint(&settings),
            PathBuf::from("/tmp/cometix-memory").join(AUTO_MEM_ENTRYPOINT_NAME)
        );
        assert_eq!(
            get_auto_mem_daily_log_path(&settings, date),
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
        let settings = default_settings();

        assert!(is_auto_mem_path(
            &settings,
            Path::new("/tmp/cometix-memory/topic.md")
        ));
        assert!(is_auto_mem_path(
            &settings,
            Path::new("/tmp/cometix-memory/sub/../topic.md")
        ));
        assert!(!is_auto_mem_path(
            &settings,
            Path::new("/tmp/cometix-memory2/topic.md")
        ));
    }
}
