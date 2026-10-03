//! Shell-agnostic git operation tracking.
//!
//! Maps to: CC `tools/shared/gitOperationTracking.ts:1-278`.
//! Analytics/OTLP counters are intentionally omitted by project policy; the
//! user-visible PR-to-session link side effect is retained.

use regex::Regex;
use std::sync::LazyLock;

static GH_PR_CREATE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bgh\s+pr\s+create\b").expect("valid gh PR regex"));
static GITHUB_PR_URL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"https://github\.com/([^/\s]+/[^/\s]+)/pull/(\d+)")
        .expect("valid GitHub PR URL regex")
});
static GIT_AMEND_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"--amend\b").expect("valid amend regex"));
static GIT_COMMIT_ID_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\[[[:alnum:]_./-]+(?: \(root-commit\))? ([0-9a-f]+)\]")
        .expect("valid git commit output regex")
});

/// Maps to: CC `gitOperationTracking.ts:29-33` — the five `gitCmdRe(...)`
/// results are MODULE CONSTANTS, compiled once at import. This port used to
/// call [`git_command_regex`] inline in [`detect_git_operation`], recompiling
/// up to five regexes on every collapsed Bash row
/// (`utils/collapse_read_search.rs:771`).
static GIT_COMMIT_RE: LazyLock<Regex> = LazyLock::new(|| git_command_regex("commit"));
static GIT_PUSH_RE: LazyLock<Regex> = LazyLock::new(|| git_command_regex("push"));
static GIT_CHERRY_PICK_RE: LazyLock<Regex> = LazyLock::new(|| git_command_regex("cherry-pick"));
static GIT_MERGE_RE: LazyLock<Regex> = LazyLock::new(|| git_command_regex("merge"));
static GIT_REBASE_RE: LazyLock<Regex> = LazyLock::new(|| git_command_regex("rebase"));

/// Maps to: CC `gitOperationTracking.ts:45-52` `GH_PR_ACTIONS` — also a module
/// constant. `op` is analytics-only (`logEvent('tengu_git_operation')`) and is
/// omitted per the n-a analytics policy; `re`/`action` are the detection half.
static GH_PR_ACTIONS: LazyLock<Vec<(Regex, PrAction)>> = LazyLock::new(|| {
    [
        (r"\bgh\s+pr\s+create\b", PrAction::Created),
        (r"\bgh\s+pr\s+edit\b", PrAction::Edited),
        (r"\bgh\s+pr\s+merge\b", PrAction::Merged),
        (r"\bgh\s+pr\s+comment\b", PrAction::Commented),
        (r"\bgh\s+pr\s+close\b", PrAction::Closed),
        (r"\bgh\s+pr\s+ready\b", PrAction::Ready),
    ]
    .into_iter()
    .map(|(pattern, action)| {
        (
            Regex::new(pattern).expect("valid gh pr action regex"),
            action,
        )
    })
    .collect()
});

/// Maps to: CC `gitOperationTracking.ts:105`
/// `stdout.match(/[Pp]ull request (?:\S+#)?#?(\d+)/)`.
///
/// CC folds ONLY the leading `P`; the rest of the literal is case-SENSITIVE.
/// A `(?i)` flag here also matched `PULL REQUEST 42` / `pull REQUEST 42`.
static PR_NUMBER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[Pp]ull request (?:\S+#)?#?(\d+)").expect("valid PR number regex")
});

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommitKind {
    Committed,
    Amended,
    CherryPicked,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BranchAction {
    Merged,
    Rebased,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrAction {
    Created,
    Edited,
    Merged,
    Commented,
    Closed,
    Ready,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitCommitSummary {
    pub sha: String,
    pub kind: CommitKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitPushSummary {
    pub branch: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitBranchSummary {
    pub reference: String,
    pub action: BranchAction,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitPrSummary {
    pub number: u64,
    pub url: Option<String>,
    pub action: PrAction,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DetectedGitOperation {
    pub commit: Option<GitCommitSummary>,
    pub push: Option<GitPushSummary>,
    pub branch: Option<GitBranchSummary>,
    pub pr: Option<GitPrSummary>,
}

/// Maps to: CC `gitOperationTracking.ts:23-27` `gitCmdRe(subcmd, suffix = '')`.
///
/// CC interpolates `subcmd` RAW into the pattern (`\\s+${subcmd}\\b`); there is
/// no escaping step, so a `regex::escape` here was a transformation with no CC
/// counterpart. The `suffix` parameter is CC's `(?!-)` merge guard, which the
/// `regex` crate cannot express — [`detect_git_operation`] emulates it at the
/// match site instead.
fn git_command_regex(subcommand: &str) -> Regex {
    Regex::new(&format!(
        r"\bgit(?:\s+-[cC]\s+\S+|\s+--\S+=\S+)*\s+{subcommand}\b"
    ))
    .expect("valid git subcommand regex")
}

/// Maps to CC `parseGitCommitId`.
pub fn parse_git_commit_id(stdout: &str) -> Option<String> {
    GIT_COMMIT_ID_RE
        .captures(stdout)
        .and_then(|captures| captures.get(1))
        .map(|value| value.as_str().to_string())
}

/// Maps to CC `detectGitOperation` for collapsed Bash summaries.
pub fn detect_git_operation(command: &str, output: &str) -> DetectedGitOperation {
    let mut result = DetectedGitOperation::default();
    let commit = GIT_COMMIT_RE.is_match(command);
    let cherry_pick = GIT_CHERRY_PICK_RE.is_match(command);
    if commit || cherry_pick {
        if let Some(sha) = parse_git_commit_id(output) {
            result.commit = Some(GitCommitSummary {
                sha: sha.chars().take(6).collect(),
                kind: if cherry_pick {
                    CommitKind::CherryPicked
                } else if GIT_AMEND_RE.is_match(command) {
                    CommitKind::Amended
                } else {
                    CommitKind::Committed
                },
            });
        }
    }

    if GIT_PUSH_RE.is_match(command) {
        static PUSH_BRANCH_RE: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"(?m)^\s*[+\-*!= ]?\s*(?:\[new branch\]|\S+\.\.+\S+)\s+\S+\s*->\s*(\S+)")
                .expect("valid push branch regex")
        });
        result.push = PUSH_BRANCH_RE
            .captures(output)
            .and_then(|captures| captures.get(1))
            .map(|value| GitPushSummary {
                branch: value.as_str().to_string(),
            });
    }

    for (verb, detect_re, marker, action) in [
        (
            "merge",
            &*GIT_MERGE_RE,
            ["Fast-forward", "Merge made by"].as_slice(),
            BranchAction::Merged,
        ),
        (
            "rebase",
            &*GIT_REBASE_RE,
            ["Successfully rebased"].as_slice(),
            BranchAction::Rebased,
        ),
    ] {
        // CC spells the merge guard as a `(?!-)` suffix, so the engine keeps
        // scanning past `git merge-base` and a later bare `git merge` still
        // matches. Ref extraction below still starts at the first occurrence,
        // matching `parseRefFromCommand`'s unguarded `String.split` over a
        // FRESH `gitCmdRe(verb)` with no suffix (CC `:117`).
        let matches_verb = detect_re
            .find_iter(command)
            .any(|matched| verb != "merge" || command[matched.end()..].chars().next() != Some('-'));
        if matches_verb && marker.iter().any(|marker| output.contains(marker)) {
            let matcher = git_command_regex(verb);
            if let Some(matched) = matcher.find(command) {
                let reference = command[matched.end()..]
                    .split_whitespace()
                    .take_while(|word| !word.starts_with(['&', '|', ';', '>', '<']))
                    .find(|word| !word.starts_with('-'));
                if let Some(reference) = reference {
                    result.branch = Some(GitBranchSummary {
                        reference: reference.to_string(),
                        action,
                    });
                }
            }
        }
    }

    if let Some((_, action)) = GH_PR_ACTIONS
        .iter()
        .find(|(regex, _)| regex.is_match(command))
    {
        if let Some(captures) = GITHUB_PR_URL_RE.captures(output) {
            if let (Some(url), Some(number)) = (
                captures.get(0).map(|value| value.as_str().to_string()),
                captures
                    .get(2)
                    .and_then(|value| value.as_str().parse::<u64>().ok()),
            ) {
                result.pr = Some(GitPrSummary {
                    number,
                    url: Some(url),
                    action: *action,
                });
            }
        } else {
            if let Some(number) = PR_NUMBER_RE
                .captures(output)
                .and_then(|captures| captures.get(1))
                .and_then(|value| value.as_str().parse::<u64>().ok())
            {
                result.pr = Some(GitPrSummary {
                    number,
                    url: None,
                    action: *action,
                });
            }
        }
    }
    result
}

/// Maps to CC `trackGitOperations`. Telemetry-only branches are omitted; on a
/// successful `gh pr create`, link the active session to the emitted PR URL.
pub fn track_git_operations(command: &str, exit_code: i32, stdout: Option<&str>) {
    if exit_code != 0 || !GH_PR_CREATE_RE.is_match(command) {
        return;
    }
    let Some(stdout) = stdout else { return };
    let Some(captures) = GITHUB_PR_URL_RE.captures(stdout) else {
        return;
    };
    let Some(repository) = captures.get(1).map(|value| value.as_str()) else {
        return;
    };
    let Some(number) = captures
        .get(2)
        .and_then(|value| value.as_str().parse::<u64>().ok())
    else {
        return;
    };
    let Some(url) = captures.get(0).map(|value| value.as_str()) else {
        return;
    };
    let session_id = crate::bootstrap::state::get_session_id();
    let _ = crate::utils::session_storage::link_session_to_pr(
        &session_id,
        number,
        url,
        repository,
        None,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[test]
    fn parses_normal_and_root_commit_ids() {
        assert_eq!(
            parse_git_commit_id("[main abc1234] message").as_deref(),
            Some("abc1234")
        );
        assert_eq!(
            parse_git_commit_id("[main (root-commit) deadbeef] initial").as_deref(),
            Some("deadbeef")
        );
        assert_eq!(parse_git_commit_id("not a commit"), None);
    }

    #[test]
    fn successful_gh_pr_create_links_active_session_metadata() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-git-pr-session-link-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let _projects = crate::utils::session_storage::set_test_projects_dir_override(&root);
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");

        track_git_operations(
            "gh pr create --title test",
            0,
            Some("https://github.com/example/project/pull/42\n"),
        );
        let path = crate::utils::session_storage::get_session_file_path(
            &crate::bootstrap::state::get_original_cwd().to_string_lossy(),
            &crate::bootstrap::state::get_session_id(),
        );
        let transcript = std::fs::read_to_string(path).unwrap();
        let entry: serde_json::Value = serde_json::from_str(transcript.trim()).unwrap();
        assert_eq!(entry["type"], "pr-link");
        assert_eq!(entry["prNumber"], 42);
        assert_eq!(entry["prRepository"], "example/project");
        assert_eq!(entry["prUrl"], "https://github.com/example/project/pull/42");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn detects_commit_push_branch_and_pr_operations_for_collapsed_bash_ui() {
        assert_eq!(
            detect_git_operation("git commit --amend -m fix", "[main abc1234] fix").commit,
            Some(GitCommitSummary {
                sha: "abc123".to_string(),
                kind: CommitKind::Amended,
            })
        );
        assert_eq!(
            detect_git_operation("git commit --amend=true -m fix", "[main abc1234] fix").commit,
            Some(GitCommitSummary {
                sha: "abc123".to_string(),
                kind: CommitKind::Amended,
            })
        );
        assert_eq!(
            detect_git_operation("git cherry-pick abc", "[main fedcba9] pick").commit,
            Some(GitCommitSummary {
                sha: "fedcba".to_string(),
                kind: CommitKind::CherryPicked,
            })
        );
        assert_eq!(
            detect_git_operation(
                "git push origin main",
                "   0123456..89abcde  main -> main\n"
            )
            .push,
            Some(GitPushSummary {
                branch: "main".to_string(),
            })
        );
        assert_eq!(
            detect_git_operation("git merge feature", "Fast-forward\n").branch,
            Some(GitBranchSummary {
                reference: "feature".to_string(),
                action: BranchAction::Merged,
            })
        );
        assert_eq!(
            detect_git_operation(
                "gh pr create --title test",
                "https://github.com/anthropics/claude-code/pull/42\n"
            )
            .pr,
            Some(GitPrSummary {
                number: 42,
                url: Some("https://github.com/anthropics/claude-code/pull/42".to_string()),
                action: PrAction::Created,
            })
        );
    }

    /// Maps to: CC `gitOperationTracking.ts:105`
    /// `/[Pp]ull request (?:\S+#)?#?(\d+)/` — the character class folds ONLY the
    /// leading `P`; `ull request` is case-sensitive and there is no `i` flag.
    #[test]
    fn pr_number_from_text_folds_only_the_leading_p() {
        let pr = |output: &str| detect_git_operation("gh pr merge 42", output).pr;
        assert_eq!(
            pr("✓ Merged pull request example/project#42").map(|pr| pr.number),
            Some(42)
        );
        // CC's own `✓ <Verb> pull request …` output is what the branch exists
        // for; the capitalized sentence-start form is the other half of `[Pp]`.
        assert_eq!(pr("Pull request #42 merged").map(|pr| pr.number), Some(42));
        // `(?i)` used to accept these; CC never does.
        assert_eq!(pr("PULL REQUEST 42"), None);
        assert_eq!(pr("pull REQUEST 42"), None);
    }

    #[test]
    fn git_regex_matches_official_raw_text_boundary_and_merge_guard() {
        assert_eq!(
            detect_git_operation("echo git commit", "[main abc1234] fake").commit,
            Some(GitCommitSummary {
                sha: "abc123".to_string(),
                kind: CommitKind::Committed,
            })
        );
        assert_eq!(
            detect_git_operation("git merge-base main other", "Fast-forward"),
            DetectedGitOperation::default()
        );
        // `parseRefFromCommand` splits on the unguarded regex, so the ref comes
        // from the `merge-base` occurrence even though the guard passed later.
        assert_eq!(
            detect_git_operation(
                "git merge-base main other && git merge feature",
                "Fast-forward"
            )
            .branch,
            Some(GitBranchSummary {
                reference: "main".to_string(),
                action: BranchAction::Merged,
            })
        );
    }
}
