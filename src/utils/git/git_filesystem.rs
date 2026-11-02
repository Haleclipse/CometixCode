//! Maps to: CC `utils/git/gitFilesystem.ts`.
//!
//! Arbitrary-directory HEAD reader used by plugin installation metadata. This
//! slice does not implement the independent GitFileWatcher/global-CWD readers.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

// Maps to: CC utils/git/gitFilesystem.ts:28 resolveGitDirCache.
// Keys are resolved path strings, not PathBuf equality (which folds spelling).
static RESOLVE_GIT_DIR_CACHE: LazyLock<Mutex<HashMap<String, Option<PathBuf>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Maps to: CC utils/git/gitFilesystem.ts:31-33 clearResolveGitDirCache.
/// Deliberately does not clear the separate findGitRoot memo.
pub fn clear_resolve_git_dir_cache() {
    RESOLVE_GIT_DIR_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear();
}

/// Maps to: CC utils/git/gitFilesystem.ts:40-76 resolveGitDir.
/// getHeadForDir always passes startPath. The optional getCwd() branch belongs
/// to the not-yet-ported global-CWD consumers, and is not synthesized here.
pub async fn resolve_git_dir(start_path: &Path) -> Option<PathBuf> {
    let cwd = if start_path.is_absolute() {
        super::normalize_lexically(start_path)
    } else {
        super::resolve_from(&std::env::current_dir().ok()?, start_path)
    };
    let key = cwd.to_string_lossy().into_owned();
    if let Some(cached) = RESOLVE_GIT_DIR_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&key)
        .cloned()
    {
        return cached;
    }
    let result = if let Some(root) = super::find_git_root(&cwd) {
        let git_path = root.join(".git");
        match tokio::fs::metadata(&git_path).await {
            Ok(metadata) => {
                if metadata.is_file() {
                    match read_utf8_trimmed(&git_path).await {
                        Ok(content) => {
                            if let Some(raw_dir) = content.strip_prefix("gitdir:") {
                                Some(super::resolve_from(&root, Path::new(js_trim(raw_dir))))
                            } else {
                                Some(git_path)
                            }
                        }
                        Err(_) => None,
                    }
                } else {
                    Some(git_path)
                }
            }
            Err(_) => None,
        }
    } else {
        None
    };
    RESOLVE_GIT_DIR_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(key, result.clone());
    result
}

/// Maps to: CC utils/git/gitFilesystem.ts:98-119 isSafeRefName.
pub fn is_safe_ref_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with(['-', '/'])
        && !name.contains("..")
        && !name.split('/').any(|part| part == "." || part.is_empty())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"/._+@-".contains(&byte))
}

/// Maps to: CC utils/git/gitFilesystem.ts:129-131 isValidGitSha.
pub fn is_valid_git_sha(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Maps to: CC utils/git/gitFilesystem.ts:151-152 readGitHead return union.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitHead {
    Branch { name: String },
    Detached { sha: String },
}

/// Maps to: CC utils/git/gitFilesystem.ts:149-183 readGitHead.
pub async fn read_git_head(git_dir: &Path) -> Option<GitHead> {
    let content = read_utf8_trimmed(&git_dir.join("HEAD")).await.ok()?;
    if let Some(reference) = content.strip_prefix("ref:") {
        let reference = js_trim(reference);
        if let Some(name) = reference.strip_prefix("refs/heads/") {
            return is_safe_ref_name(name).then(|| GitHead::Branch { name: name.into() });
        }
        if !is_safe_ref_name(reference) {
            return None;
        }
        return Some(GitHead::Detached {
            sha: resolve_ref(git_dir, reference).await.unwrap_or_default(),
        });
    }
    is_valid_git_sha(&content).then_some(GitHead::Detached { sha: content })
}

/// Maps to: CC utils/git/gitFilesystem.ts:203-219 resolveRef.
pub async fn resolve_ref(git_dir: &Path, reference: &str) -> Option<String> {
    if let Some(result) = resolve_ref_in_dir(git_dir, reference).await {
        return Some(result);
    }
    if let Some(common_dir) = get_common_dir(git_dir).await
        && common_dir.as_os_str() != git_dir.as_os_str()
    {
        return resolve_ref_in_dir(&common_dir, reference).await;
    }
    None
}

/// Maps to: CC utils/git/gitFilesystem.ts:221-266 resolveRefInDir.
async fn resolve_ref_in_dir(dir: &Path, reference: &str) -> Option<String> {
    // Node path.join does not discard dir when its second string starts '/'.
    let mut joined = dir.as_os_str().to_os_string();
    if !joined.is_empty() {
        joined.push(std::path::MAIN_SEPARATOR_STR);
    }
    joined.push(reference);
    let ref_path = super::normalize_lexically(Path::new(&joined));
    if let Ok(content) = read_utf8_trimmed(&ref_path).await {
        if let Some(target) = content.strip_prefix("ref:") {
            let target = js_trim(target);
            if !is_safe_ref_name(target) {
                return None;
            }
            // Async recursion requires indirection in Rust; there is no source
            // hop limit/cycle fallback to invent here.
            return Box::pin(resolve_ref(dir, target)).await;
        }
        return is_valid_git_sha(&content).then_some(content);
    }
    if let Ok(bytes) = tokio::fs::read(dir.join("packed-refs")).await {
        let packed = String::from_utf8_lossy(&bytes);
        for line in packed.split('\n') {
            if line.starts_with(['#', '^']) {
                continue;
            }
            let Some((sha, name)) = line.split_once(' ') else {
                continue;
            };
            if name == reference {
                return is_valid_git_sha(sha).then(|| sha.to_string());
            }
        }
    }
    None
}

/// Maps to: CC utils/git/gitFilesystem.ts:273-280 getCommonDir.
pub async fn get_common_dir(git_dir: &Path) -> Option<PathBuf> {
    let content = read_utf8_trimmed(&git_dir.join("commondir")).await.ok()?;
    let base = if git_dir.is_absolute() {
        git_dir.to_path_buf()
    } else {
        super::resolve_from(&std::env::current_dir().ok()?, git_dir)
    };
    Some(super::resolve_from(&base, Path::new(&content)))
}

/// Maps to: CC utils/git/gitFilesystem.ts:593-606 getHeadForDir.
pub async fn get_head_for_dir(cwd: &Path) -> Option<String> {
    let git_dir = resolve_git_dir(cwd).await?;
    match read_git_head(&git_dir).await? {
        GitHead::Branch { name } => resolve_ref(&git_dir, &format!("refs/heads/{name}")).await,
        GitHead::Detached { sha } => Some(sha),
    }
}

// Native expressions for source readFile(..., 'utf-8').trim(). Decoding must be
// lossy, and ECMAScript trim includes FEFF but excludes NEL (unlike str::trim).
async fn read_utf8_trimmed(path: &Path) -> std::io::Result<String> {
    let bytes = tokio::fs::read(path).await?;
    Ok(js_trim(&String::from_utf8_lossy(&bytes)).to_owned())
}

fn js_trim(value: &str) -> &str {
    value.trim_matches(|ch: char| (ch.is_whitespace() && ch != '\u{85}') || ch == '\u{feff}')
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDir(PathBuf);
    impl TestDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("nyan-git-head-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn put(&self, path: &str, bytes: impl AsRef<[u8]>) {
            let path = self.0.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }
    }
    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[tokio::test]
    async fn git_head_reader_matches_official_bun_head_union_and_empty_sha() {
        // Verbatim results of AST-extracted CC gitFilesystem.ts:149-183/593-606,
        // proof/plugin-installed-git-0914/bun-oracle.json (Bun 1.3.14).
        let cases: serde_json::Value = serde_json::from_str(r#"[{"kind":"head","input":"ref: refs/heads/main\n","parsed":{"type":"branch","name":"main"},"result":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},{"kind":"head","input":"ref:\trefs/heads/feature/a+b@c\r\n","parsed":{"type":"branch","name":"feature/a+b@c"},"result":null},{"kind":"head","input":"ref: refs/heads/../escape","parsed":null,"result":null},{"kind":"head","input":"ref: refs/heads/foo//bar","parsed":null,"result":null},{"kind":"head","input":"ref: refs/heads/-bad","parsed":null,"result":null},{"kind":"head","input":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n","parsed":{"type":"detached","sha":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},"result":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},{"kind":"head","input":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","parsed":{"type":"detached","sha":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"},"result":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"},{"kind":"head","input":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA","parsed":null,"result":null},{"kind":"head","input":"abc","parsed":null,"result":null},{"kind":"head","input":"ref: refs/remotes/origin/HEAD","parsed":{"type":"detached","sha":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"},"result":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"},{"kind":"head","input":"ref: refs/remotes/origin/missing","parsed":{"type":"detached","sha":""},"result":""},{"kind":"head","input":"\ufeffaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\ufeff","parsed":{"type":"detached","sha":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},"result":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},{"kind":"head","input":"\u0085aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\u0085","parsed":null,"result":null}]"#).unwrap();
        let root = TestDir::new();
        let sha = "a".repeat(40);
        let sha2 = "b".repeat(64);
        for (index, case) in cases.as_array().unwrap().iter().enumerate() {
            let prefix = format!("head-{index}/.git");
            root.put(&format!("{prefix}/HEAD"), case["input"].as_str().unwrap());
            root.put(&format!("{prefix}/refs/heads/main"), &sha);
            root.put(
                &format!("{prefix}/refs/remotes/origin/HEAD"),
                "ref: refs/remotes/origin/main\n",
            );
            root.put(&format!("{prefix}/refs/remotes/origin/main"), &sha2);
            let parsed = read_git_head(&root.0.join(&prefix)).await;
            let parsed = match parsed {
                None => serde_json::Value::Null,
                Some(GitHead::Branch { name }) => serde_json::json!({"type":"branch","name":name}),
                Some(GitHead::Detached { sha }) => serde_json::json!({"type":"detached","sha":sha}),
            };
            assert_eq!(parsed, case["parsed"], "head {index}");
            assert_eq!(
                serde_json::to_value(get_head_for_dir(&root.0.join(format!("head-{index}"))).await)
                    .unwrap(),
                case["result"],
                "result {index}"
            );
        }
    }

    #[tokio::test]
    async fn git_ref_reader_matches_official_bun_loose_packed_and_error_order() {
        // CC gitFilesystem.ts:203-266: invalid loose contents return early;
        // only read errors allow packed fallback; packed lines are not trimmed.
        let sha = "a".repeat(40);
        let sha2 = "b".repeat(64);
        let cases: Vec<(&str, Vec<(&str, Vec<u8>)>, Option<&str>)> = vec![
            (
                "loose",
                vec![("refs/heads/main", format!("{sha}\n").into_bytes())],
                Some(&sha),
            ),
            (
                "packed",
                vec![(
                    "packed-refs",
                    format!("# pack-refs with: peeled\n^{sha}\n{sha2} refs/heads/main\n")
                        .into_bytes(),
                )],
                Some(&sha2),
            ),
            (
                "invalid-loose",
                vec![
                    ("refs/heads/main", b"bad".to_vec()),
                    (
                        "packed-refs",
                        format!("{sha} refs/heads/main\n").into_bytes(),
                    ),
                ],
                None,
            ),
            (
                "symref",
                vec![
                    ("refs/heads/main", b"ref: refs/remotes/origin/main".to_vec()),
                    ("refs/remotes/origin/main", sha.as_bytes().to_vec()),
                ],
                Some(&sha),
            ),
            (
                "unsafe-symref",
                vec![
                    ("refs/heads/main", b"ref: ../escape".to_vec()),
                    (
                        "packed-refs",
                        format!("{sha} refs/heads/main\n").into_bytes(),
                    ),
                ],
                None,
            ),
            (
                "crlf",
                vec![(
                    "packed-refs",
                    format!("{sha} refs/heads/main\r\n").into_bytes(),
                )],
                None,
            ),
            (
                "first-invalid",
                vec![(
                    "packed-refs",
                    format!("bad refs/heads/main\n{sha} refs/heads/main\n").into_bytes(),
                )],
                None,
            ),
            (
                "directory",
                vec![(
                    "packed-refs",
                    format!("{sha} refs/heads/main\n").into_bytes(),
                )],
                Some(&sha),
            ),
            (
                "utf8",
                vec![
                    ("refs/heads/main", vec![255, 10]),
                    (
                        "packed-refs",
                        format!("{sha} refs/heads/main\n").into_bytes(),
                    ),
                ],
                None,
            ),
        ];
        for (name, files, expected) in cases {
            let root = TestDir::new();
            for (path, bytes) in files {
                root.put(path, bytes);
            }
            if name == "directory" {
                std::fs::create_dir_all(root.0.join("refs/heads/main")).unwrap();
            }
            assert_eq!(
                resolve_ref(&root.0, "refs/heads/main").await.as_deref(),
                expected,
                "{name}"
            );
        }
    }

    #[tokio::test]
    async fn git_directory_reader_matches_official_bun_worktree_and_lexical_paths() {
        // CC gitFilesystem.ts:40-76/203-219/273-280: worktree-local loose/packed
        // refs precede common refs; pointer paths are lexical, not realpath.
        let root = TestDir::new();
        let sha = "a".repeat(40);
        let sha2 = "b".repeat(64);
        root.put(
            "work/.git",
            " \u{feff}gitdir: ../main/.git/worktrees/work\r\n",
        );
        root.put("main/.git/worktrees/work/HEAD", "ref: refs/heads/main");
        root.put("main/.git/worktrees/work/commondir", "../..\n");
        root.put("main/.git/refs/heads/main", &sha);
        let dir = root.0.join("main/.git/worktrees/work");
        assert_eq!(
            resolve_git_dir(&root.0.join("work")).await,
            Some(dir.clone())
        );
        assert_eq!(get_common_dir(&dir).await, Some(root.0.join("main/.git")));
        assert_eq!(
            get_head_for_dir(&root.0.join("work")).await.as_deref(),
            Some(sha.as_str())
        );
        root.put("main/.git/worktrees/work/refs/heads/main", &sha2);
        assert_eq!(
            get_head_for_dir(&root.0.join("work")).await.as_deref(),
            Some(sha2.as_str())
        );
        root.put("main/.git/worktrees/work/refs/heads/main", "bad");
        assert_eq!(
            get_head_for_dir(&root.0.join("work")).await.as_deref(),
            Some(sha.as_str())
        );
        // The original exported helpers also accept relative gitDir strings.
        // Build an equivalent relative spelling without mutating process cwd.
        #[cfg(unix)]
        {
            let cwd = std::env::current_dir().unwrap();
            let mut relative = PathBuf::new();
            for _ in cwd
                .components()
                .filter(|part| matches!(part, std::path::Component::Normal(_)))
            {
                relative.push("..");
            }
            relative.push(dir.strip_prefix("/").unwrap());
            assert_eq!(
                get_common_dir(&relative).await,
                Some(root.0.join("main/.git"))
            );
            assert_eq!(
                resolve_ref(&relative, "refs/heads/main").await.as_deref(),
                Some(sha.as_str())
            );
        }
        root.put("malformed/.git", "invalid pointer");
        assert_eq!(
            resolve_git_dir(&root.0.join("malformed")).await,
            Some(root.0.join("malformed/.git"))
        );
        assert_eq!(get_head_for_dir(&root.0.join("malformed")).await, None);
        root.put(
            "pointer/.git",
            "gitdir: ../absent/../main/.git/worktrees/work",
        );
        assert_eq!(resolve_git_dir(&root.0.join("pointer")).await, Some(dir));
        root.put("unborn/.git/HEAD", "ref: refs/heads/unborn");
        assert_eq!(get_head_for_dir(&root.0.join("unborn")).await, None);
    }

    #[tokio::test]
    async fn git_directory_cache_matches_official_bun_negative_cache_and_separate_reset() {
        // CC gitFilesystem.ts:28-76 and git.ts:27-109: clearing either cache
        // does not clear the other; null resolutions are real cached entries.
        let root = TestDir::new();
        let fresh = root.0.join("fresh");
        std::fs::create_dir(&fresh).unwrap();
        super::super::find_git_root_cache().clear();
        clear_resolve_git_dir_cache();
        assert_eq!(resolve_git_dir(&fresh).await, None);
        std::fs::create_dir(fresh.join(".git")).unwrap();
        assert_eq!(resolve_git_dir(&fresh).await, None);
        clear_resolve_git_dir_cache();
        assert_eq!(resolve_git_dir(&fresh).await, None);
        super::super::find_git_root_cache().clear();
        assert_eq!(resolve_git_dir(&fresh).await, None);
        clear_resolve_git_dir_cache();
        assert_eq!(resolve_git_dir(&fresh).await, Some(fresh.join(".git")));
    }
}
