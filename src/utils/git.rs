//! Maps to: CC `utils/git.ts`.
//!
//! Git readers and the memoized executable resolver retain their source owner.
//! Broader git-state/cache parity remains tracked in `docs/MODULE_MAP.tsv`.

pub mod git_filesystem;
pub mod gitignore;

use std::path::{Component, Path, PathBuf};
use std::process::Command;
use unicode_normalization::UnicodeNormalization;

/// Maps to: CC `utils/git.ts:212-216#gitExe`.
/// The zero-argument memo retains both a located executable and the `git`
/// fallback for the process. PathBuf carries the source executable string;
/// cloning it does not normalize or re-resolve the cached path.
pub fn git_exe() -> PathBuf {
    static GIT_EXE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    GIT_EXE
        .get_or_init(|| {
            crate::utils::which::which_sync("git").unwrap_or_else(|| PathBuf::from("git"))
        })
        .clone()
}

/// Maps to: CC `utils/git.ts:218-230` `getIsGit`.
/// CC memoizes the zero-argument call for the process: only its first invocation
/// reads getCwd(). The explicit cwd carries that invocation's scoped directory;
/// later invocations retain the first answer, including after a directory change.
pub fn get_is_git(cwd: &Path) -> bool {
    static IS_GIT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *IS_GIT.get_or_init(|| find_git_root(cwd).is_some())
}

/// Maps to: CC `utils/git.ts:27-85` findGitRootImpl memoized closure.
static FIND_GIT_ROOT_IMPL: std::sync::LazyLock<
    crate::utils::memoize::LruMemoizedFunction<PathBuf, Option<PathBuf>>,
> = std::sync::LazyLock::new(|| {
    crate::utils::memoize::memoize_with_lru(
        |start_path: &PathBuf| {
            let absolute = if start_path.is_absolute() {
                start_path.to_path_buf()
            } else {
                std::env::current_dir().ok()?.join(start_path)
            };
            let mut current = normalize_lexically(&absolute);

            loop {
                if std::fs::metadata(current.join(".git"))
                    .is_ok_and(|metadata| metadata.is_dir() || metadata.is_file())
                {
                    return Some(normalize_unicode(current));
                }
                let Some(parent) = current.parent() else {
                    return None;
                };
                if parent == current {
                    return None;
                }
                current = parent.to_path_buf();
            }
        },
        |path| path.to_string_lossy().into_owned(),
        Some(50),
    )
});

/// Maps to: CC `utils/git.ts:97-109` findGitRoot / createFindGitRoot wrapper.
/// The raw argument spelling is the memo key; only the cache miss resolves it.
pub fn find_git_root(start_path: &Path) -> Option<PathBuf> {
    FIND_GIT_ROOT_IMPL.call(&start_path.to_path_buf())
}

/// Maps to: CC `utils/git.ts:107` findGitRoot.cache shared property.
/// A Rust accessor carries the cache property on the source callable object.
pub fn find_git_root_cache() -> &'static crate::utils::memoize::LruMemoizedCache<Option<PathBuf>> {
    &FIND_GIT_ROOT_IMPL.cache
}

/// Resolve worktrees to the canonical main repository identity.
///
/// Maps to CC `utils/git.ts#findCanonicalGitRoot`, including validation of the
/// `.git`/`commondir`/backlink chain before trusting paths controlled by a repo.
pub fn find_canonical_git_root(start_path: &Path) -> Option<PathBuf> {
    let git_root = find_git_root(start_path)?;
    resolve_canonical_root(&git_root).or(Some(git_root))
}

fn resolve_canonical_root(git_root: &Path) -> Option<PathBuf> {
    let git_content = read_small_trimmed(&git_root.join(".git"))?;
    let git_dir = git_content.strip_prefix("gitdir:")?.trim();
    let worktree_git_dir = resolve_from(git_root, Path::new(git_dir));
    let common_dir_value = read_small_trimmed(&worktree_git_dir.join("commondir"))?;
    let common_dir = resolve_from(&worktree_git_dir, Path::new(&common_dir_value));

    if worktree_git_dir.parent().map(normalize_lexically)
        != Some(normalize_lexically(&common_dir.join("worktrees")))
    {
        return None;
    }
    let backlink_value = read_small_trimmed(&worktree_git_dir.join("gitdir"))?;
    let backlink = PathBuf::from(backlink_value).canonicalize().ok()?;
    let expected = git_root.canonicalize().ok()?.join(".git");
    if backlink != expected {
        return None;
    }

    if common_dir.file_name().is_some_and(|name| name == ".git") {
        common_dir
            .parent()
            .map(|path| normalize_unicode(path.to_path_buf()))
    } else {
        Some(normalize_unicode(common_dir))
    }
}

fn read_small_trimmed(path: &Path) -> Option<String> {
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > 64 * 1024 {
        return None;
    }
    Some(std::fs::read_to_string(path).ok()?.trim().to_string())
}

fn resolve_from(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        normalize_lexically(path)
    } else {
        normalize_lexically(&base.join(path))
    }
}

fn normalize_lexically(path: &Path) -> PathBuf {
    path.components()
        .fold(PathBuf::new(), |mut output, component| {
            match component {
                Component::CurDir => {}
                Component::ParentDir => {
                    if output.file_name().is_some_and(|name| name != "..") {
                        output.pop();
                    } else if !output.has_root() {
                        output.push("..");
                    }
                }
                other => output.push(other.as_os_str()),
            }
            output
        })
}

fn normalize_unicode(path: PathBuf) -> PathBuf {
    PathBuf::from(path.to_string_lossy().nfc().collect::<String>())
}

/// Synchronous Rust projection of CC `utils/git.ts::getBranch()`.
///
/// CC resolves the branch asynchronously; this projection blocks on a git
/// subprocess. UI callers must run it off the render thread and land the
/// result through `use_future` (as `LogSelector` does for CC's mount-effect
/// `getBranch().then(setCurrentBranch)`) — calling it from a render body
/// freezes the TUI for the whole git round-trip.
pub fn get_branch() -> String {
    let cwd = crate::bootstrap::state::get_original_cwd();
    Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(cwd)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn git_exe_matches_official_first_lookup_memo_for_path_and_fallback() {
        use std::os::unix::fs::PermissionsExt;
        // CC utils/git.ts:212-216. Each case runs in a fresh process so neither
        // the zero-argument memo nor Bun-compatible startup PATH is reset by
        // a test-only production hook. A candidate is never executed.
        const PROBE: &str = "COMETIX_GIT_EXE_MEMO_PROBE";
        if let Ok(mode) = std::env::var(PROBE) {
            crate::utils::process_env::capture_startup();
            let directory = PathBuf::from(std::env::var_os("PATH").unwrap());
            let executable = directory.join("git");
            let expected = if mode == "found" {
                executable.clone()
            } else {
                PathBuf::from("git")
            };
            assert_eq!(git_exe().as_os_str(), expected.as_os_str());
            if mode == "found" {
                std::fs::remove_file(&executable).unwrap();
                assert_eq!(crate::utils::which::which_sync("git"), None);
            } else {
                std::fs::write(&executable, "#!/bin/sh\nprintf unexpected > executed\n").unwrap();
                std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755))
                    .unwrap();
                assert_eq!(crate::utils::which::which_sync("git"), Some(executable));
            }
            assert_eq!(git_exe().as_os_str(), expected.as_os_str());
            assert_eq!(git_exe().as_os_str(), expected.as_os_str());
            assert!(!directory.join("executed").exists());
            return;
        }
        for mode in ["found", "fallback"] {
            let root = TestDir::new();
            if mode == "found" {
                let executable = root.path().join("git");
                std::fs::write(&executable, "#!/bin/sh\nprintf unexpected > executed\n").unwrap();
                std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755))
                    .unwrap();
            }
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "utils::git::tests::git_exe_matches_official_first_lookup_memo_for_path_and_fallback",
                    "--nocapture",
                ])
                .env(PROBE, mode)
                .env("PATH", root.path())
                .current_dir(root.path())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
            while child.try_wait().unwrap().is_none() {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("git_exe memo probe timed out: {mode}");
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            let output = child.wait_with_output().unwrap();
            assert!(
                output.status.success(),
                "{mode}: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(!root.path().join("executed").exists());
        }
    }

    // Test-only RAII fixture; no runtime counterpart or additional dependency.
    struct TestDir(std::path::PathBuf);

    impl TestDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("cometix-git-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&path).expect("create isolated test directory");
            Self(path)
        }

        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn get_is_git_matches_official_marker_and_first_invocation_cache() {
        let root = TestDir::new();
        // CC utils/git.ts:218-230 delegates to findGitRoot, not git rev-parse.
        // A .git marker is sufficient, even if not a usable Git repository.
        std::fs::write(root.path().join(".git"), "not a git repository").unwrap();
        assert!(get_is_git(root.path()));
        std::fs::remove_file(root.path().join(".git")).unwrap();
        let elsewhere = TestDir::new();
        // The source memoized closure has no arguments: later cwd changes or
        // marker removal cannot cause an uncached second filesystem check.
        assert!(get_is_git(elsewhere.path()));
    }

    #[test]
    fn get_is_git_matches_official_cached_false_after_marker_creation() {
        let root = TestDir::new();
        // CC utils/git.ts:218-230 caches false as well as true.
        assert!(!get_is_git(root.path()));
        std::fs::create_dir(root.path().join(".git")).unwrap();
        assert!(!get_is_git(root.path()));
    }

    #[test]
    fn get_branch_is_total_like_official_cached_reader() {
        // The checkout may or may not be a git repository in test packaging;
        // the API must return a string rather than surfacing process failures.
        let _ = get_branch();
    }

    #[test]
    fn find_git_root_matches_official_bun_raw_key_lru_promotion_and_null() {
        // CC utils/git.ts:27-109 + memoize.ts:234-269. Original Bun oracle:
        // proof/plugin-installed-git-0914/bun-oracle.json root-lru/raw-key.
        let root = TestDir::new();
        let missing = root.path().join("missing");
        find_git_root_cache().clear();
        assert_eq!(find_git_root(&missing), None);
        std::fs::create_dir_all(missing.join(".git")).unwrap();
        for index in 0..49 {
            find_git_root(&root.path().join(format!("miss-{index}")));
        }
        assert_eq!(find_git_root_cache().size(), 50);
        assert_eq!(find_git_root(&missing), None);
        find_git_root(&root.path().join("miss-49"));
        assert!(find_git_root_cache().has(&missing.to_string_lossy()));
        for index in 50..100 {
            find_git_root(&root.path().join(format!("miss-{index}")));
        }
        assert!(!find_git_root_cache().has(&missing.to_string_lossy()));
        assert_eq!(find_git_root(&missing), Some(missing.clone()));
        find_git_root_cache().clear();
        find_git_root(&missing);
        let spelled = format!("{}/./", missing.display());
        find_git_root(Path::new(&spelled));
        assert_eq!(find_git_root_cache().size(), 2);
        assert!(find_git_root_cache().has(&missing.to_string_lossy()));
        assert!(find_git_root_cache().has(&spelled));
        find_git_root_cache().clear();
        assert_eq!(find_git_root_cache().size(), 0);
    }
}
