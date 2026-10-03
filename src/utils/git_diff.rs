//! Maps to: CC `utils/gitDiff.ts:16-382` — current-working-tree diff
//! statistics/hunks consumed by `useDiffData` and `DiffDialog`.
//!
//! This module deliberately keeps git process execution in `utils/`, not in
//! components. Commands are local, read-only, disable credential prompting,
//! and retain the official five-second timeout.

use crate::types::message::StructuredDiffHunk;
use crate::utils::process_env::JsTruthy;
use regex::Regex;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::LazyLock;
use std::time::Duration;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GitDiffStats {
    pub files_count: usize,
    pub lines_added: usize,
    pub lines_removed: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PerFileStats {
    pub added: usize,
    pub removed: usize,
    pub is_binary: bool,
    pub is_untracked: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GitDiffResult {
    pub stats: GitDiffStats,
    pub per_file_stats: BTreeMap<String, PerFileStats>,
    pub hunks: BTreeMap<String, Vec<StructuredDiffHunk>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NumstatResult {
    pub stats: GitDiffStats,
    pub per_file_stats: BTreeMap<String, PerFileStats>,
}

const GIT_TIMEOUT_MS: u64 = 5_000;
const MAX_FILES: usize = 50;
const MAX_DIFF_SIZE_BYTES: usize = 1_000_000;
const MAX_LINES_PER_FILE: usize = 400;
const MAX_FILES_FOR_DETAILS: usize = 500;

static HUNK_HEADER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@").expect("valid git hunk regex")
});
static SHORTSTAT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(\d+)\s+files?\s+changed(?:,\s+(\d+)\s+insertions?\(\+\))?(?:,\s+(\d+)\s+deletions?\(-\))?",
    )
    .expect("valid git shortstat regex")
});

async fn git_output(cwd: &Path, args: &[&str]) -> Option<(i32, String)> {
    let mut command = tokio::process::Command::new(crate::utils::git::git_exe());
    // CC gitDiff.ts runs git through execFileNoThrow: process.env, nothing added.
    crate::utils::subprocess_env::apply_process_env(&mut command);
    command
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_millis(GIT_TIMEOUT_MS), command.output())
        .await
        .ok()?
        .ok()?;
    Some((
        output.status.code().unwrap_or(1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    ))
}

async fn git_dir(cwd: &Path) -> Option<PathBuf> {
    let (code, stdout) = git_output(cwd, &["rev-parse", "--git-dir"]).await?;
    if code != 0 || stdout.trim().is_empty() {
        return None;
    }
    let path = PathBuf::from(stdout.trim());
    Some(if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    })
}

/// Maps to: CC `utils/gitDiff.ts:307-326` `isInTransientGitState`.
async fn is_in_transient_git_state(cwd: &Path) -> bool {
    let Some(git_dir) = git_dir(cwd).await else {
        return false;
    };
    [
        "MERGE_HEAD",
        "REBASE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
    ]
    .iter()
    .any(|name| git_dir.join(name).exists())
}

async fn is_git(cwd: &Path) -> bool {
    git_output(cwd, &["rev-parse", "--is-inside-work-tree"])
        .await
        .is_some_and(|(code, stdout)| code == 0 && stdout.trim() == "true")
}

/// Maps to: CC `utils/gitDiff.ts:334-362` `fetchUntrackedFiles`.
async fn fetch_untracked_files(
    cwd: &Path,
    max_files: usize,
) -> Option<BTreeMap<String, PerFileStats>> {
    let (code, stdout) = git_output(
        cwd,
        &[
            "--no-optional-locks",
            "ls-files",
            "--others",
            "--exclude-standard",
        ],
    )
    .await?;
    if code != 0 || stdout.trim().is_empty() {
        return None;
    }
    let entries = stdout
        .trim()
        .lines()
        .filter(|path| !path.is_empty())
        .take(max_files)
        .map(|path| {
            (
                path.to_string(),
                PerFileStats {
                    is_untracked: true,
                    ..PerFileStats::default()
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    (!entries.is_empty()).then_some(entries)
}

/// Maps to: CC `utils/gitDiff.ts:49-108` `fetchGitDiff`.
pub async fn fetch_git_diff() -> Option<GitDiffResult> {
    let cwd = std::env::current_dir().ok()?;
    fetch_git_diff_from(&cwd).await
}

pub async fn fetch_git_diff_from(cwd: &Path) -> Option<GitDiffResult> {
    if !is_git(cwd).await || is_in_transient_git_state(cwd).await {
        return None;
    }

    if let Some((0, shortstat)) =
        git_output(cwd, &["--no-optional-locks", "diff", "HEAD", "--shortstat"]).await
    {
        if let Some(stats) = parse_shortstat(&shortstat) {
            if stats.files_count > MAX_FILES_FOR_DETAILS {
                return Some(GitDiffResult {
                    stats,
                    ..GitDiffResult::default()
                });
            }
        }
    }

    let (code, numstat) =
        git_output(cwd, &["--no-optional-locks", "diff", "HEAD", "--numstat"]).await?;
    if code != 0 {
        return None;
    }
    let NumstatResult {
        mut stats,
        mut per_file_stats,
    } = parse_git_numstat(&numstat);

    let remaining_slots = MAX_FILES.saturating_sub(per_file_stats.len());
    if remaining_slots > 0 {
        if let Some(untracked) = fetch_untracked_files(cwd, remaining_slots).await {
            stats.files_count += untracked.len();
            per_file_stats.extend(untracked);
        }
    }

    Some(GitDiffResult {
        stats,
        per_file_stats,
        hunks: BTreeMap::new(),
    })
}

/// Maps to: CC `utils/gitDiff.ts:114-135` `fetchGitDiffHunks`.
pub async fn fetch_git_diff_hunks() -> BTreeMap<String, Vec<StructuredDiffHunk>> {
    let Some(cwd) = std::env::current_dir().ok() else {
        return BTreeMap::new();
    };
    fetch_git_diff_hunks_from(&cwd).await
}

pub async fn fetch_git_diff_hunks_from(cwd: &Path) -> BTreeMap<String, Vec<StructuredDiffHunk>> {
    if !is_git(cwd).await || is_in_transient_git_state(cwd).await {
        return BTreeMap::new();
    }
    let Some((0, stdout)) = git_output(cwd, &["--no-optional-locks", "diff", "HEAD"]).await else {
        return BTreeMap::new();
    };
    parse_git_diff(&stdout)
}

/// Maps to: CC `utils/gitDiff.ts:148-189` `parseGitNumstat`.
pub fn parse_git_numstat(stdout: &str) -> NumstatResult {
    let mut added = 0usize;
    let mut removed = 0usize;
    let mut valid_file_count = 0usize;
    let mut per_file_stats = BTreeMap::new();

    for line in stdout.trim().lines().filter(|line| !line.is_empty()) {
        let parts = line.split('\t').collect::<Vec<_>>();
        if parts.len() < 3 {
            continue;
        }
        valid_file_count += 1;
        let is_binary = parts[0] == "-" || parts[1] == "-";
        let file_added = if is_binary {
            0
        } else {
            parts[0].parse::<usize>().unwrap_or(0)
        };
        let file_removed = if is_binary {
            0
        } else {
            parts[1].parse::<usize>().unwrap_or(0)
        };
        added += file_added;
        removed += file_removed;
        if per_file_stats.len() < MAX_FILES {
            per_file_stats.insert(
                parts[2..].join("\t"),
                PerFileStats {
                    added: file_added,
                    removed: file_removed,
                    is_binary,
                    is_untracked: false,
                },
            );
        }
    }

    NumstatResult {
        stats: GitDiffStats {
            files_count: valid_file_count,
            lines_added: added,
            lines_removed: removed,
        },
        per_file_stats,
    }
}

fn parse_hunk_number(captures: &regex::Captures<'_>, index: usize, default: usize) -> usize {
    captures
        .get(index)
        .and_then(|value| value.as_str().parse::<usize>().ok())
        .unwrap_or(default)
}

fn is_diff_metadata(line: &str) -> bool {
    line.starts_with("index ")
        || line.starts_with("---")
        || line.starts_with("+++")
        || line.starts_with("new file")
        || line.starts_with("deleted file")
        || line.starts_with("old mode")
        || line.starts_with("new mode")
        || line.starts_with("Binary files")
}

/// Maps to: CC `utils/gitDiff.ts:200-298` `parseGitDiff`.
pub fn parse_git_diff(stdout: &str) -> BTreeMap<String, Vec<StructuredDiffHunk>> {
    let mut result = BTreeMap::new();
    if stdout.trim().is_empty() {
        return result;
    }

    for file_diff in stdout
        .split("diff --git ")
        .filter(|part| !part.trim().is_empty())
    {
        if result.len() >= MAX_FILES {
            break;
        }
        if file_diff.len() > MAX_DIFF_SIZE_BYTES {
            continue;
        }
        let lines = file_diff.split('\n').collect::<Vec<_>>();
        let Some(header) = lines.first().copied() else {
            continue;
        };
        let Some((_, file_path)) = header
            .strip_prefix("a/")
            .and_then(|header| header.split_once(" b/"))
        else {
            continue;
        };

        let mut file_hunks = Vec::new();
        let mut current_hunk = None::<StructuredDiffHunk>;
        let mut line_count = 0usize;
        for line in lines.into_iter().skip(1) {
            if let Some(captures) = HUNK_HEADER_RE.captures(line) {
                if let Some(hunk) = current_hunk.take() {
                    file_hunks.push(hunk);
                }
                current_hunk = Some(StructuredDiffHunk {
                    old_start: parse_hunk_number(&captures, 1, 0),
                    old_lines: parse_hunk_number(&captures, 2, 1),
                    new_start: parse_hunk_number(&captures, 3, 0),
                    new_lines: parse_hunk_number(&captures, 4, 1),
                    lines: Vec::new(),
                });
                continue;
            }
            if is_diff_metadata(line) {
                continue;
            }
            if let Some(hunk) = current_hunk.as_mut() {
                if line_count < MAX_LINES_PER_FILE
                    && (line.starts_with('+')
                        || line.starts_with('-')
                        || line.starts_with(' ')
                        || line.is_empty())
                {
                    hunk.lines.push(line.to_string());
                    line_count += 1;
                }
            }
        }
        if let Some(hunk) = current_hunk {
            file_hunks.push(hunk);
        }
        if !file_hunks.is_empty() {
            result.insert(file_path.to_string(), file_hunks);
        }
    }
    result
}

/// Maps to: CC `utils/gitDiff.ts:371-382` `parseShortstat`.
pub fn parse_shortstat(stdout: &str) -> Option<GitDiffStats> {
    let captures = SHORTSTAT_RE.captures(stdout)?;
    Some(GitDiffStats {
        files_count: parse_hunk_number(&captures, 1, 0),
        lines_added: parse_hunk_number(&captures, 2, 0),
        lines_removed: parse_hunk_number(&captures, 3, 0),
    })
}

/// Maps to CC `utils/gitDiff.ts#ToolUseDiff`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolUseDiff {
    pub filename: String,
    pub status: ToolUseDiffStatus,
    pub additions: usize,
    pub deletions: usize,
    pub changes: usize,
    pub patch: String,
    pub repository: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolUseDiffStatus {
    Modified,
    Added,
}

impl ToolUseDiff {
    /// CC `gitDiffSchema` wire projection (`FileEditTool/types.ts:46-60`) —
    /// the shape `gitDiff` records inside `toolUseResult`. `repository` is
    /// nullable-optional in the schema; the recorder writes it verbatim, so
    /// an absent value rides as `null`.
    pub fn to_official_json(&self) -> serde_json::Value {
        serde_json::json!({
            "filename": self.filename,
            "status": match self.status {
                ToolUseDiffStatus::Modified => "modified",
                ToolUseDiffStatus::Added => "added",
            },
            "additions": self.additions,
            "deletions": self.deletions,
            "changes": self.changes,
            "patch": self.patch,
            "repository": self.repository,
        })
    }

    /// The Rust stand-in for `gitDiffSchema` validation: six required keys,
    /// `status` a strict two-value enum, `repository` nullable-optional.
    pub fn from_official_json(value: &serde_json::Value) -> Option<Self> {
        let map = value.as_object()?;
        Some(Self {
            filename: map.get("filename")?.as_str()?.to_string(),
            status: match map.get("status")?.as_str()? {
                "modified" => ToolUseDiffStatus::Modified,
                "added" => ToolUseDiffStatus::Added,
                _ => return None,
            },
            additions: map.get("additions")?.as_u64()? as usize,
            deletions: map.get("deletions")?.as_u64()? as usize,
            changes: map.get("changes")?.as_u64()? as usize,
            patch: map.get("patch")?.as_str()?.to_string(),
            repository: match map.get("repository") {
                None | Some(serde_json::Value::Null) => None,
                Some(serde_json::Value::String(repository)) => Some(repository.clone()),
                Some(_) => return None,
            },
        })
    }
}

async fn run_single_file_git(root: &Path, args: &[&str]) -> Option<(bool, String)> {
    let mut command = tokio::process::Command::new(crate::utils::git::git_exe());
    // CC gitDiff.ts runs git through execFileNoThrow: process.env, nothing added.
    crate::utils::subprocess_env::apply_process_env(&mut command);
    command.args(args).current_dir(root);
    let output = tokio::time::timeout(Duration::from_millis(3_000), command.output())
        .await
        .ok()?
        .ok()?;
    Some((
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    ))
}

async fn github_repository(root: &Path) -> Option<String> {
    let (success, stdout) =
        run_single_file_git(root, &["config", "--get", "remote.origin.url"]).await?;
    if !success {
        return None;
    }
    let remote = stdout.trim().trim_end_matches(".git");
    let suffix = remote
        .split_once("github.com:")
        .map(|(_, suffix)| suffix)
        .or_else(|| remote.split_once("github.com/").map(|(_, suffix)| suffix))?;
    let mut components = suffix.trim_matches('/').split('/');
    let owner = components.next()?;
    let repository = components.next()?;
    (!owner.is_empty() && !repository.is_empty()).then(|| format!("{owner}/{repository}"))
}

fn parse_raw_single_file_diff(
    filename: String,
    raw_diff: &str,
    status: ToolUseDiffStatus,
    repository: Option<String>,
) -> ToolUseDiff {
    let mut in_hunks = false;
    let mut additions = 0usize;
    let mut deletions = 0usize;
    let mut patch_lines = Vec::new();
    for line in raw_diff.split('\n') {
        if line.starts_with("@@") {
            in_hunks = true;
        }
        if in_hunks {
            patch_lines.push(line);
            if line.starts_with('+') && !line.starts_with("+++") {
                additions += 1;
            } else if line.starts_with('-') && !line.starts_with("---") {
                deletions += 1;
            }
        }
    }
    ToolUseDiff {
        filename,
        status,
        additions,
        deletions,
        changes: additions + deletions,
        patch: patch_lines.join("\n"),
        repository,
    }
}

async fn get_single_file_diff_ref(root: &Path) -> String {
    let base_branch = if let Some(base_ref) =
        crate::utils::process_env::var("CLAUDE_CODE_BASE_REF").truthy()
    {
        base_ref
    } else {
        // Maps to cached `getDefaultBranch()`: origin/HEAD, then remote
        // main/master existence, then the official `main` fallback.
        let symbolic = run_single_file_git(
            root,
            &["symbolic-ref", "refs/remotes/origin/HEAD", "--short"],
        )
        .await
        .and_then(|(success, output)| success.then_some(output))
        .map(|output| {
            output
                .trim()
                .strip_prefix("origin/")
                .unwrap_or(output.trim())
                .to_string()
        });
        if let Some(branch) = symbolic.filter(|branch| !branch.is_empty()) {
            branch
        } else {
            let mut branch = None;
            for candidate in ["main", "master"] {
                if run_single_file_git(
                    root,
                    &[
                        "rev-parse",
                        "--verify",
                        &format!("refs/remotes/origin/{candidate}"),
                    ],
                )
                .await
                .is_some_and(|(success, _)| success)
                {
                    branch = Some(candidate.to_string());
                    break;
                }
            }
            branch.unwrap_or_else(|| "main".to_string())
        }
    };
    let Some((success, stdout)) =
        run_single_file_git(root, &["merge-base", "HEAD", &base_branch]).await
    else {
        return "HEAD".to_string();
    };
    if success && !stdout.trim().is_empty() {
        stdout.trim().to_string()
    } else {
        "HEAD".to_string()
    }
}

/// Maps to CC `utils/gitDiff.ts#fetchSingleFileGitDiff`.
pub async fn fetch_single_file_git_diff(absolute_file_path: &Path) -> Option<ToolUseDiff> {
    const MAX_DIFF_SIZE_BYTES: u64 = 1_000_000;

    let git_root_start = if absolute_file_path.is_dir() {
        absolute_file_path
    } else {
        absolute_file_path.parent().unwrap_or(absolute_file_path)
    };
    let git_root = crate::utils::git::find_git_root(git_root_start)?;
    // Preserve the model/tool's logical path (including a direct symlink)
    // like Node `path.relative`; choose a lexical `.git` ancestor when one is
    // available so macOS `/var` ↔ `/private/var` aliases remain comparable.
    let logical_git_root = absolute_file_path
        .ancestors()
        .skip(1)
        .find(|ancestor| ancestor.join(".git").exists())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| git_root.clone());
    let git_path = absolute_file_path
        .strip_prefix(&logical_git_root)
        .ok()?
        .to_string_lossy()
        .replace('\\', "/");
    let repository = github_repository(&git_root).await;
    let tracked = run_single_file_git(
        &git_root,
        &[
            "--no-optional-locks",
            "ls-files",
            "--error-unmatch",
            &git_path,
        ],
    )
    .await?
    .0;

    if tracked {
        let diff_ref = get_single_file_diff_ref(&git_root).await;
        let (success, stdout) = run_single_file_git(
            &git_root,
            &["--no-optional-locks", "diff", &diff_ref, "--", &git_path],
        )
        .await?;
        if !success || stdout.is_empty() {
            return None;
        }
        return Some(parse_raw_single_file_diff(
            git_path,
            &stdout,
            ToolUseDiffStatus::Modified,
            repository,
        ));
    }

    let metadata = std::fs::metadata(absolute_file_path).ok()?;
    if metadata.len() > MAX_DIFF_SIZE_BYTES || !metadata.is_file() {
        return None;
    }
    let bytes = std::fs::read(absolute_file_path).ok()?;
    let content = String::from_utf8_lossy(&bytes);
    let mut lines = content.split('\n').collect::<Vec<_>>();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    let line_count = lines.len();
    let added_lines = lines
        .into_iter()
        .map(|line| format!("+{line}"))
        .collect::<Vec<_>>()
        .join("\n");
    Some(ToolUseDiff {
        filename: git_path,
        status: ToolUseDiffStatus::Added,
        additions: line_count,
        deletions: 0,
        changes: line_count,
        patch: format!("@@ -0,0 +1,{line_count} @@\n{added_lines}"),
        repository,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_git_numstat_matches_official_counts_binary_and_tab_paths() {
        let parsed =
            parse_git_numstat("2\t1\tsrc/a.rs\n-\t-\tassets/logo.bin\n3\t0\tpath\twith-tab\n");
        assert_eq!(parsed.stats.files_count, 3);
        assert_eq!(parsed.stats.lines_added, 5);
        assert_eq!(parsed.stats.lines_removed, 1);
        assert!(parsed.per_file_stats["assets/logo.bin"].is_binary);
        assert_eq!(parsed.per_file_stats["path\twith-tab"].added, 3);
    }

    #[test]
    fn parse_git_diff_matches_official_hunks_and_metadata_filtering() {
        let parsed = parse_git_diff(
            "diff --git a/src/lib.rs b/src/lib.rs\nindex 111..222 100644\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,2 +1,2 @@\n-old\n+new\n context\n@@ -10 +10,2 @@\n keep\n+added\n",
        );
        let hunks = &parsed["src/lib.rs"];
        assert_eq!(hunks.len(), 2);
        assert_eq!((hunks[0].old_start, hunks[0].new_start), (1, 1));
        assert_eq!(hunks[0].lines, vec!["-old", "+new", " context"]);
        assert_eq!((hunks[1].old_lines, hunks[1].new_lines), (1, 2));
    }

    #[tokio::test]
    async fn single_file_diff_covers_tracked_and_untracked_files_without_network() {
        let root = std::env::temp_dir().join(format!(
            "cometix-single-file-git-diff-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(&root)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {:?}: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "--quiet"]);
        git(&["config", "user.email", "write-test@example.com"]);
        git(&["config", "user.name", "Write Test"]);
        git(&[
            "remote",
            "add",
            "origin",
            "git@github.com:owner/repository.git",
        ]);
        let tracked = root.join("tracked.txt");
        std::fs::write(&tracked, "old\n").unwrap();
        git(&["add", "tracked.txt"]);
        git(&["commit", "--quiet", "-m", "initial"]);
        std::fs::write(&tracked, "new\n").unwrap();

        let diff = fetch_single_file_git_diff(&tracked).await.unwrap();
        assert_eq!(diff.status, ToolUseDiffStatus::Modified);
        assert_eq!((diff.additions, diff.deletions, diff.changes), (1, 1, 2));
        assert!(diff.patch.starts_with("@@"));
        assert!(diff.patch.contains("-old"));
        assert!(diff.patch.contains("+new"));
        assert_eq!(diff.repository.as_deref(), Some("owner/repository"));

        let added = root.join("added.txt");
        std::fs::write(&added, "one\ntwo\n").unwrap();
        let diff = fetch_single_file_git_diff(&added).await.unwrap();
        assert_eq!(diff.status, ToolUseDiffStatus::Added);
        assert_eq!((diff.additions, diff.deletions, diff.changes), (2, 0, 2));
        assert_eq!(diff.patch, "@@ -0,0 +1,2 @@\n+one\n+two");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn parse_shortstat_matches_official_singular_and_optional_counts() {
        assert_eq!(
            parse_shortstat(" 1 file changed, 2 insertions(+), 1 deletion(-)"),
            Some(GitDiffStats {
                files_count: 1,
                lines_added: 2,
                lines_removed: 1,
            })
        );
        assert_eq!(
            parse_shortstat(" 3 files changed"),
            Some(GitDiffStats {
                files_count: 3,
                lines_added: 0,
                lines_removed: 0,
            })
        );
    }
}
