//! Auto-managed memory-file detection.
//!
//! Maps to: CC `utils/memoryFileDetection.ts`.

use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;

/// Rust discriminant carrier for the inline string union returned by CC
/// `detectSessionFileType` and `detectSessionPatternType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionFileType {
    SessionMemory,
    SessionTranscript,
}

/// Maps to: CC `utils/memoryFileDetection.ts:24-26` `toPosix`.
fn to_posix(path: &str) -> String {
    path.replace('\\', "/")
}

/// Maps to: CC `utils/memoryFileDetection.ts:30-33` `toComparable`.
fn to_comparable(path: &str) -> String {
    let posix = to_posix(path);
    if cfg!(windows) {
        posix.to_lowercase()
    } else {
        posix
    }
}

/// Maps to: CC `utils/memoryFileDetection.ts:40-58` `detectSessionFileType`.
pub fn detect_session_file_type(file_path: &str) -> Option<SessionFileType> {
    let config_dir = crate::utils::env_utils::get_claude_config_home_dir();
    let normalized = to_comparable(file_path);
    let config_dir = to_comparable(&config_dir.display().to_string());
    if !normalized.starts_with(&config_dir) {
        return None;
    }
    if normalized.contains("/session-memory/") && normalized.ends_with(".md") {
        return Some(SessionFileType::SessionMemory);
    }
    if normalized.contains("/projects/") && normalized.ends_with(".jsonl") {
        return Some(SessionFileType::SessionTranscript);
    }
    None
}

/// Maps to: CC `utils/memoryFileDetection.ts:65-81` `detectSessionPatternType`.
pub fn detect_session_pattern_type(pattern: &str) -> Option<SessionFileType> {
    let normalized = to_posix(pattern);
    if normalized.contains("session-memory")
        && (normalized.contains(".md") || normalized.ends_with('*'))
    {
        return Some(SessionFileType::SessionMemory);
    }
    if normalized.contains(".jsonl")
        || (normalized.contains("projects") && normalized.contains("*.jsonl"))
    {
        return Some(SessionFileType::SessionTranscript);
    }
    None
}

/// Maps to: CC `utils/memoryFileDetection.ts:87-92` `isAutoMemFile`.
///
/// Auto-memory enablement observes merged settings (project opt-out is
/// supported), while `isAutoMemPath` takes `autoMemoryDirectory` from the
/// trusted sources only, so a project cannot redirect it.
pub fn is_auto_mem_file(file_path: &Path) -> bool {
    crate::memdir::paths::is_auto_memory_enabled()
        && crate::memdir::paths::is_auto_mem_path(file_path)
}

/// Maps to: CC `utils/memoryFileDetection.ts:119-125` `isAgentMemFile`.
fn is_agent_mem_file(file_path: &Path) -> bool {
    crate::memdir::paths::is_auto_memory_enabled()
        && crate::tools::agent_tool::agent_memory::is_agent_memory_path(
            file_path,
            &crate::bootstrap::state::get_original_cwd(),
        )
}

/// Maps to: CC `utils/memoryFileDetection.ts:133-145`
/// `isAutoManagedMemoryFile`.
pub fn is_auto_managed_memory_file(file_path: &str) -> bool {
    let file_path = Path::new(file_path);
    if is_auto_mem_file(file_path) {
        return true;
    }
    #[cfg(feature = "anthropic_internal")]
    if crate::memdir::team_mem_paths::is_team_mem_file(file_path) {
        return true;
    }
    if detect_session_file_type(&file_path.display().to_string()).is_some() {
        return true;
    }
    is_agent_mem_file(file_path)
}

/// Maps to: CC `utils/memoryFileDetection.ts:152-208` `isMemoryDirectory`.
pub fn is_memory_directory(dir_path: &str) -> bool {
    // Mechanical projection of Node `path.normalize`: remove `.` and collapse
    // `..` before the source's string/path-owner checks.
    let normalized_path = {
        let path = Path::new(dir_path);
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
    };
    let normalized_cmp = to_comparable(&normalized_path.display().to_string());
    let auto_memory_enabled = crate::memdir::paths::is_auto_memory_enabled();

    if auto_memory_enabled
        && (normalized_cmp.contains("/agent-memory/")
            || normalized_cmp.contains("/agent-memory-local/"))
    {
        return true;
    }
    #[cfg(feature = "anthropic_internal")]
    if crate::memdir::team_mem_paths::is_team_memory_enabled()
        && crate::memdir::team_mem_paths::is_team_mem_path(&normalized_path)
    {
        return true;
    }
    if auto_memory_enabled && crate::memdir::paths::is_auto_mem_path(&normalized_path) {
        return true;
    }

    let config_dir = to_comparable(
        &crate::utils::env_utils::get_claude_config_home_dir()
            .display()
            .to_string(),
    );
    let memory_base = to_comparable(
        &crate::memdir::paths::get_memory_base_dir()
            .display()
            .to_string(),
    );
    let under_config = normalized_cmp.starts_with(&config_dir);
    let under_memory_base = normalized_cmp.starts_with(&memory_base);
    if !under_config && !under_memory_base {
        return false;
    }
    if normalized_cmp.contains("/session-memory/") {
        return true;
    }
    if under_config && normalized_cmp.contains("/projects/") {
        return true;
    }
    auto_memory_enabled && normalized_cmp.contains("/memory/")
}

static ABSOLUTE_SHELL_PATH: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r#"(?:[A-Za-z]:[/\\]|/)[^\s'\"]+"#).expect("memory shell path regex is valid")
});

/// Maps to: CC `utils/memoryFileDetection.ts:215-269`
/// `isShellCommandTargetingMemory`.
pub fn is_shell_command_targeting_memory(command: &str) -> bool {
    let config_dir = crate::utils::env_utils::get_claude_config_home_dir();
    let memory_base = crate::memdir::paths::get_memory_base_dir();
    let auto_memory_enabled = crate::memdir::paths::is_auto_memory_enabled();
    let auto_mem_dir = auto_memory_enabled.then(crate::memdir::paths::get_auto_mem_path);

    let command_cmp = to_comparable(command);
    let dirs = [
        Some(config_dir.display().to_string()),
        Some(memory_base.display().to_string()),
        auto_mem_dir.map(|path| {
            path.display()
                .to_string()
                .trim_end_matches(['/', '\\'])
                .to_string()
        }),
    ];
    let matches_any_dir = dirs.into_iter().flatten().any(|directory| {
        if command_cmp.contains(&to_comparable(&directory)) {
            return true;
        }
        #[cfg(windows)]
        {
            return command_cmp.contains(
                &crate::utils::windows_paths::windows_path_to_posix_path(&directory).to_lowercase(),
            );
        }
        #[cfg(not(windows))]
        false
    });
    if !matches_any_dir {
        return false;
    }

    for matched in ABSOLUTE_SHELL_PATH.find_iter(command) {
        let clean_path = matched.as_str().trim_end_matches([',', ';', '|', '&', '>']);
        #[cfg(windows)]
        let native_path = crate::utils::windows_paths::posix_path_to_windows_path(clean_path);
        #[cfg(not(windows))]
        let native_path = clean_path.to_string();
        if is_auto_managed_memory_file(&native_path) || is_memory_directory(&native_path) {
            return true;
        }
    }
    false
}

/// Maps to: CC `utils/memoryFileDetection.ts:277-289`
/// `isAutoManagedMemoryPattern`.
pub fn is_auto_managed_memory_pattern(pattern: &str) -> bool {
    if detect_session_pattern_type(pattern).is_some() {
        return true;
    }
    if crate::memdir::paths::is_auto_memory_enabled() {
        let normalized = pattern.replace('\\', "/");
        return normalized.contains("agent-memory/") || normalized.contains("agent-memory-local/");
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK, TestEnvGuard};

    const ENV_KEYS: [&str; 4] = [
        "CLAUDE_CONFIG_DIR",
        "CLAUDE_CODE_REMOTE_MEMORY_DIR",
        "CLAUDE_COWORK_MEMORY_PATH_OVERRIDE",
        "CLAUDE_CODE_DISABLE_AUTO_MEMORY",
    ];

    struct DetectionTestEnv {
        _lock: TestEnvGuard<'static>,
        previous_env: Vec<EnvVarGuard>,
        previous_cwd: PathBuf,
    }

    impl DetectionTestEnv {
        fn new(root: &Path) -> Self {
            let lock = TEST_ENV_LOCK
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let previous_env = ENV_KEYS
                .into_iter()
                .map(EnvVarGuard::preserve)
                .collect::<Vec<_>>();
            for key in ENV_KEYS {
                crate::utils::process_env::remove(key);
            }
            crate::utils::process_env::set("CLAUDE_CONFIG_DIR", root.join("config"));
            crate::utils::process_env::set(
                "CLAUDE_COWORK_MEMORY_PATH_OVERRIDE",
                root.join("auto-memory"),
            );
            crate::utils::process_env::set("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "false");
            let previous_cwd = crate::bootstrap::state::get_original_cwd();
            crate::bootstrap::state::set_original_cwd(root.join("project"));
            Self {
                _lock: lock,
                previous_env,
                previous_cwd,
            }
        }
    }

    impl Drop for DetectionTestEnv {
        fn drop(&mut self) {
            crate::bootstrap::state::set_original_cwd(self.previous_cwd.clone());
            self.previous_env.clear();
        }
    }

    #[test]
    fn session_file_and_pattern_detection_matches_official_raw_strings() {
        let root = std::env::temp_dir().join(format!(
            "cometix-memory-detection-session-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let _env = DetectionTestEnv::new(&root);
        let config = root.join("config");

        assert_eq!(
            detect_session_file_type(
                &config
                    .join("session-memory/session.md")
                    .display()
                    .to_string()
            ),
            Some(SessionFileType::SessionMemory)
        );
        assert_eq!(
            detect_session_file_type(
                &config
                    .join("projects/demo/session.jsonl")
                    .display()
                    .to_string()
            ),
            Some(SessionFileType::SessionTranscript)
        );
        assert_eq!(
            detect_session_file_type(
                &root
                    .join("elsewhere/session-memory/session.md")
                    .display()
                    .to_string()
            ),
            None
        );
        assert_eq!(
            detect_session_pattern_type(r"session-memory\*.md"),
            Some(SessionFileType::SessionMemory)
        );
        assert_eq!(
            detect_session_pattern_type("projects/**/*.jsonl"),
            Some(SessionFileType::SessionTranscript)
        );
    }

    #[test]
    fn managed_file_directory_and_pattern_detection_delegate_to_canonical_owners() {
        let root = std::env::temp_dir().join(format!(
            "cometix-memory-detection-owner-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let _env = DetectionTestEnv::new(&root);
        let auto_file = root.join("auto-memory/MEMORY.md");
        let session_dir = root.join("config/session-memory/nested");
        let agent_file = root.join("project/.claude/agent-memory/reviewer/MEMORY.md");

        assert!(is_auto_mem_file(&auto_file));
        assert!(is_auto_managed_memory_file(
            &auto_file.display().to_string()
        ));
        assert!(is_auto_managed_memory_file(
            &agent_file.display().to_string()
        ));
        assert!(is_memory_directory(&session_dir.display().to_string()));
        assert!(!is_memory_directory(
            &root
                .join("config/session-memory-sibling")
                .display()
                .to_string()
        ));
        assert!(is_auto_managed_memory_pattern("agent-memory/**/*.md"));
        assert!(!is_auto_managed_memory_pattern("CLAUDE.md"));
    }

    #[test]
    fn shell_memory_detection_requires_an_extracted_canonical_memory_path() {
        let root = std::env::temp_dir().join(format!(
            "cometix-memory-detection-shell-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let _env = DetectionTestEnv::new(&root);
        let session_file = root.join("config/session-memory/session.md");

        assert!(is_shell_command_targeting_memory(&format!(
            "rg needle '{}'",
            session_file.display()
        )));
        assert!(!is_shell_command_targeting_memory(
            "rg session-memory relative/path"
        ));
        assert!(!is_shell_command_targeting_memory(&format!(
            "Write-Output '{}'",
            root.join("config").display()
        )));
    }
}
