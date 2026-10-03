//! Markdown config directory loader.
//! Maps to: CC `utils/markdownConfigLoader.ts`.
//!
//! Collects managed/user/project `.claude/<subdir>/*.md` files, follows
//! symlinks with physical-directory cycle detection, applies worktree fallback
//! and source/policy gates, parses frontmatter, and deduplicates file identity.
//! Telemetry and the alternate ripgrep implementation remain intentionally
//! omitted.

use crate::utils::frontmatter_parser::{FrontmatterData, parse_frontmatter};
use crate::utils::settings::managed_path::get_managed_file_path;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MarkdownConfigSource {
    PolicySettings,
    UserSettings,
    ProjectSettings,
}

impl MarkdownConfigSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PolicySettings => "policySettings",
            Self::UserSettings => "userSettings",
            Self::ProjectSettings => "projectSettings",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkdownFile {
    pub file_path: PathBuf,
    pub base_dir: PathBuf,
    pub frontmatter: FrontmatterData,
    pub content: String,
    pub source: MarkdownConfigSource,
}

/// Maps to: CC `utils/markdownConfigLoader.ts#extractDescriptionFromMarkdown`.
pub fn extract_description_from_markdown(content: &str, default_description: &str) -> String {
    // ECMAScript trim and RegExp \s, not Rust's Unicode White_Space (NEL).
    let whitespace = |ch: char| matches!(ch, '\u{0009}'..='\u{000d}' | ' ' | '\u{00a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}');
    for line in content.split('\n') {
        let trimmed = line.trim_matches(whitespace);
        if trimmed.is_empty() {
            continue;
        }
        let after_hashes = trimmed.trim_start_matches('#');
        let header_text = after_hashes.trim_start_matches(whitespace);
        let text = if after_hashes.len() < trimmed.len()
            && header_text.len() < after_hashes.len()
            && !header_text.is_empty()
            && !header_text.contains(['\r', '\u{2028}', '\u{2029}'])
        {
            header_text
        } else {
            trimmed
        };
        let units: Vec<_> = text.encode_utf16().collect();
        // Existing String boundary projects a cut lone surrogate to U+FFFD.
        return if units.len() > 100 {
            format!("{}...", String::from_utf16_lossy(&units[..97]))
        } else {
            text.to_owned()
        };
    }
    default_description.to_owned()
}

fn normalize_path_for_comparison(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Maps to: CC `utils/markdownConfigLoader.ts#getProjectDirsUpToHome`.
pub fn get_project_dirs_up_to_home(subdir: &str, cwd: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let home = crate::utils::node_os::homedir();
    let home = home.canonicalize().unwrap_or(home);
    let git_root = crate::utils::git::find_git_root(cwd);
    let mut current = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());

    loop {
        if normalize_path_for_comparison(&current) == normalize_path_for_comparison(&home) {
            break;
        }

        let candidate = current.join(".claude").join(subdir);
        if candidate.is_dir() {
            dirs.push(candidate);
        }

        if let Some(git_root) = &git_root {
            if normalize_path_for_comparison(&current) == normalize_path_for_comparison(git_root) {
                break;
            }
        }

        if !current.pop() {
            break;
        }
    }

    dirs
}

/// Maps to: CC `utils/markdownConfigLoader.ts#findMarkdownFilesNative`.
fn find_markdown_files_native(dir: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, files: &mut Vec<PathBuf>, visited_dirs: &mut HashSet<String>) {
        let Ok(metadata) = std::fs::metadata(dir) else {
            return;
        };
        if !metadata.is_dir() {
            return;
        }
        #[cfg(unix)]
        let directory_identity = {
            use std::os::unix::fs::MetadataExt as _;
            format!("{}:{}", metadata.dev(), metadata.ino())
        };
        #[cfg(not(unix))]
        let directory_identity = dir
            .canonicalize()
            .unwrap_or_else(|_| dir.to_path_buf())
            .to_string_lossy()
            .to_string();
        if !visited_dirs.insert(directory_identity) {
            return;
        }

        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                walk(&path, files, visited_dirs);
                continue;
            }
            if file_type.is_symlink() {
                let Ok(target_metadata) = std::fs::metadata(&path) else {
                    continue;
                };
                if target_metadata.is_dir() {
                    walk(&path, files, visited_dirs);
                } else if target_metadata.is_file()
                    && path.extension().and_then(|ext| ext.to_str()) == Some("md")
                {
                    files.push(path);
                }
                continue;
            }
            if file_type.is_file() && path.extension().and_then(|ext| ext.to_str()) == Some("md") {
                files.push(path);
            }
        }
    }

    let mut files = Vec::new();
    walk(dir, &mut files, &mut HashSet::new());
    files
}

fn load_markdown_files_from_dir(
    base_dir: &Path,
    source: MarkdownConfigSource,
) -> Vec<MarkdownFile> {
    let mut paths = find_markdown_files_native(base_dir);
    paths.sort();
    paths
        .into_iter()
        .filter_map(|file_path| {
            let raw = std::fs::read_to_string(&file_path).ok()?;
            let parsed = parse_frontmatter(&raw);
            Some(MarkdownFile {
                file_path,
                base_dir: base_dir.to_path_buf(),
                frontmatter: parsed.frontmatter,
                content: parsed.content,
                source,
            })
        })
        .collect()
}

/// Maps to: CC `utils/markdownConfigLoader.ts#loadMarkdownFilesForSubdir`.
pub fn load_markdown_files_for_subdir(subdir: &str, cwd: &Path) -> Vec<MarkdownFile> {
    // CC :303-305 order.
    let user_dir = crate::utils::env_utils::get_claude_config_home_dir().join(subdir);
    let managed_dir = get_managed_file_path().join(".claude").join(subdir);
    let mut project_dirs = get_project_dirs_up_to_home(subdir, cwd);

    // Worktree sparse-checkout fallback: only consult the main repository when
    // the worktree itself has no `.claude/<subdir>` directory.
    if let (Some(git_root), Some(canonical_root)) = (
        crate::utils::git::find_git_root(cwd),
        crate::utils::git::find_canonical_git_root(cwd),
    ) {
        if normalize_path_for_comparison(&git_root)
            != normalize_path_for_comparison(&canonical_root)
        {
            let worktree_subdir = git_root.join(".claude").join(subdir);
            let worktree_has_subdir = project_dirs.iter().any(|directory| {
                normalize_path_for_comparison(directory)
                    == normalize_path_for_comparison(&worktree_subdir)
            });
            if !worktree_has_subdir {
                let main_subdir = canonical_root.join(".claude").join(subdir);
                if !project_dirs.iter().any(|directory| {
                    normalize_path_for_comparison(directory)
                        == normalize_path_for_comparison(&main_subdir)
                }) {
                    project_dirs.push(main_subdir);
                }
            }
        }
    }

    let customization_locked = subdir == "agents"
        && crate::utils::settings::plugin_only_policy::is_restricted_to_plugin_only("agents");
    let mut files = Vec::new();
    files.extend(load_markdown_files_from_dir(
        &managed_dir,
        MarkdownConfigSource::PolicySettings,
    ));
    if crate::utils::settings::constants::is_setting_source_enabled(
        crate::utils::settings::constants::SettingSource::User,
    ) && !customization_locked
    {
        files.extend(load_markdown_files_from_dir(
            &user_dir,
            MarkdownConfigSource::UserSettings,
        ));
    }
    if crate::utils::settings::constants::is_setting_source_enabled(
        crate::utils::settings::constants::SettingSource::Project,
    ) && !customization_locked
    {
        for project_dir in project_dirs {
            files.extend(load_markdown_files_from_dir(
                &project_dir,
                MarkdownConfigSource::ProjectSettings,
            ));
        }
    }

    let mut seen = HashSet::new();
    files
        .into_iter()
        .filter(|file| {
            let key = file
                .file_path
                .canonicalize()
                .unwrap_or_else(|_| file.file_path.clone());
            seen.insert(normalize_path_for_comparison(&key))
        })
        .collect()
}

/// Maps to CC `utils/markdownConfigLoader.ts#parseToolListString`.
fn parse_tool_list_string(value: Option<&serde_json::Value>) -> Option<Vec<String>> {
    let value = value?;
    if value.is_null() {
        return None;
    }
    if value == &serde_json::Value::Bool(false) || value.as_str().is_some_and(str::is_empty) {
        return Some(Vec::new());
    }
    let values = if let Some(value) = value.as_str() {
        vec![value.to_string()]
    } else if let Some(values) = value.as_array() {
        values
            .iter()
            .filter_map(|value| value.as_str().map(ToOwned::to_owned))
            .collect()
    } else {
        Vec::new()
    };
    if values.is_empty() {
        return Some(Vec::new());
    }
    Some(crate::utils::permissions::permission_setup::parse_tool_list_from_cli(&values))
}

/// Maps to CC `utils/markdownConfigLoader.ts#parseAgentToolsFromFrontmatter`.
pub fn parse_agent_tools_from_frontmatter(
    value: Option<&serde_json::Value>,
) -> Option<Vec<String>> {
    if value.is_some_and(serde_json::Value::is_null) {
        return Some(Vec::new());
    }
    let parsed = parse_tool_list_string(value)?;
    if parsed.iter().any(|tool| tool == "*") {
        None
    } else {
        Some(parsed)
    }
}

/// Maps to CC
/// `utils/markdownConfigLoader.ts#parseSlashCommandToolsFromFrontmatter`.
pub fn parse_slash_command_tools_from_frontmatter(
    value: Option<&serde_json::Value>,
) -> Vec<String> {
    parse_tool_list_string(value).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[test]
    fn description_fallback_uses_first_non_empty_line_and_truncates() {
        assert_eq!(
            extract_description_from_markdown("\n## Heading\nbody", "Default"),
            "Heading"
        );
        let long = "a".repeat(101);
        assert_eq!(
            extract_description_from_markdown(&long, "Default"),
            format!("{}...", "a".repeat(97))
        );
        assert_eq!(
            extract_description_from_markdown("\n", "Default"),
            "Default"
        );
    }

    use super::*;

    struct AllowedSourcesGuard(Vec<String>);

    impl AllowedSourcesGuard {
        fn capture() -> Self {
            Self(crate::bootstrap::state::get_allowed_setting_sources())
        }
    }

    impl Drop for AllowedSourcesGuard {
        fn drop(&mut self) {
            crate::bootstrap::state::set_allowed_setting_sources(self.0.clone());
        }
    }

    fn temp_dir(prefix: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("{prefix}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[cfg(unix)]
    #[test]
    fn markdown_discovery_follows_symlinks_and_breaks_directory_cycles() {
        use crate::utils::test_env::HOME_VAR;
        use std::os::unix::fs::symlink;

        let root = temp_dir("cometix-md-loader-symlink");
        let managed_root = root.join("managed");
        let managed = managed_root.join(".claude/commands");
        let external = root.join("external");
        std::fs::create_dir_all(&managed).unwrap();
        std::fs::create_dir_all(&external).unwrap();
        std::fs::write(external.join("linked.md"), "linked file").unwrap();
        let nested = external.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("nested.md"), "nested file").unwrap();
        symlink(external.join("linked.md"), managed.join("linked.md")).unwrap();
        symlink(&nested, managed.join("nested-link")).unwrap();
        symlink(&managed, nested.join("cycle")).unwrap();

        // Only the managed root has a `commands` dir: the user root is
        // missing, and the project walk from `root` stops at once at home.
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _managed = EnvVarGuard::set("CLAUDE_CODE_MANAGED_SETTINGS_PATH", &managed_root);
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", root.join("missing-user"));
        let _home = EnvVarGuard::set(HOME_VAR, &root);
        let files = load_markdown_files_for_subdir("commands", &root);
        let names = files
            .iter()
            .filter_map(|file| file.file_path.file_name()?.to_str())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["linked.md", "nested.md"]);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn sparse_worktree_falls_back_to_main_repository_markdown_directory() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let root = temp_dir("cometix-md-loader-worktree");
        let main = root.join("main");
        let worktree = root.join("worktree");
        let worktree_git_dir = main.join(".git/worktrees/worktree");
        std::fs::create_dir_all(&worktree_git_dir).unwrap();
        std::fs::create_dir_all(main.join(".claude/agents")).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::write(
            main.join(".claude/agents/reviewer.md"),
            "---\nname: reviewer\n---\nreview",
        )
        .unwrap();
        std::fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", worktree_git_dir.display()),
        )
        .unwrap();
        std::fs::write(worktree_git_dir.join("commondir"), "../..\n").unwrap();
        std::fs::write(
            worktree_git_dir.join("gitdir"),
            format!("{}\n", worktree.join(".git").display()),
        )
        .unwrap();

        let config_home = root.join("config");
        let managed = root.join("managed");
        std::fs::create_dir_all(&config_home).unwrap();
        std::fs::create_dir_all(&managed).unwrap();
        let _config_guard = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config_home);
        let _managed_guard = EnvVarGuard::set("CLAUDE_CODE_MANAGED_SETTINGS_PATH", &managed);
        let _sources_guard = AllowedSourcesGuard::capture();
        crate::bootstrap::state::set_allowed_setting_sources(vec![
            "userSettings".to_string(),
            "projectSettings".to_string(),
            "localSettings".to_string(),
        ]);

        let files = load_markdown_files_for_subdir("agents", &worktree);

        assert!(files.iter().any(|file| {
            file.source == MarkdownConfigSource::ProjectSettings
                && file.file_path.ends_with(".claude/agents/reviewer.md")
        }));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn load_markdown_files_for_subdir_preserves_official_source_order() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let root = temp_dir("cometix-md-loader");
        let config_home = root.join("config");
        let managed = root.join("managed");
        let project = root.join("project");
        std::fs::create_dir_all(project.join(".git")).unwrap();
        for dir in [
            config_home.join("output-styles"),
            managed.join(".claude/output-styles"),
            project.join(".claude/output-styles"),
        ] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(
            managed.join(".claude/output-styles/managed.md"),
            "---\nname: Managed\n---\nmanaged prompt",
        )
        .unwrap();
        std::fs::write(
            config_home.join("output-styles/user.md"),
            "---\nname: User\n---\nuser prompt",
        )
        .unwrap();
        std::fs::write(
            project.join(".claude/output-styles/project.md"),
            "---\nname: Project\n---\nproject prompt",
        )
        .unwrap();

        let _config_guard = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config_home);
        let _managed_guard = EnvVarGuard::set("CLAUDE_CODE_MANAGED_SETTINGS_PATH", &managed);
        let old_cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(&project).unwrap();
        let files = load_markdown_files_for_subdir("output-styles", &project);
        std::env::set_current_dir(old_cwd).unwrap();
        let _ = std::fs::remove_dir_all(root);

        let sources = files
            .iter()
            .map(|file| file.source.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            sources,
            vec!["policySettings", "userSettings", "projectSettings"]
        );
        assert_eq!(files[0].content, "managed prompt");
    }
    #[test]
    fn description_matches_actual_bun_header_whitespace_and_utf16_oracle() {
        let cases: serde_json::Value = serde_json::from_str(include_str!("../../tests/fixtures/oracles/plugin-command-dependencies-0916/description-oracle.json")).unwrap();
        for case in cases.as_array().unwrap() {
            let input = case["input"].as_str().unwrap();
            let units = case["units"]
                .as_array()
                .unwrap()
                .iter()
                .map(|unit| unit.as_u64().unwrap() as u16)
                .collect::<Vec<_>>();
            // The existing Rust String output projects a cut lone surrogate.
            assert_eq!(
                extract_description_from_markdown(input, "Default"),
                String::from_utf16_lossy(&units),
                "input={input:?}"
            );
        }
    }
}
