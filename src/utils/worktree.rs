//! Maps to: CC `utils/worktree.ts` worktree session shape used by
//! `components/WorktreeExitDialog.tsx`.
//!
//! Agent worktree creation/removal ports the git-backed and
//! `WorktreeCreate` hook-backed `createAgentWorktree` slice.
//! Session-mode helpers `createWorktreeForSession` / `keepWorktree` /
//! `cleanupWorktree` / `killTmuxSession` / `countWorktreeChanges` are live
//! for EnterWorktreeTool / ExitWorktreeTool.
//! `getCurrentWorktreeSession` / `restoreWorktreeSession` are live (process
//! cache) for `/clear` re-persist and ExitFlow wiring.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{LazyLock, RwLock};

const MAX_WORKTREE_SLUG_LENGTH: usize = 64;
static PR_URL_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?i)^https?://[^/]+/[^/]+/[^/]+/pull/(\d+)/?(?:[?#].*)?$")
        .expect("valid PR URL regex")
});
static PR_HASH_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^#(\d+)$").expect("valid PR hash regex"));

/// Maps to: CC `utils/worktree.ts` `WorktreeSession`.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeSession {
    pub original_cwd: String,
    pub worktree_path: String,
    pub worktree_name: String,
    pub worktree_branch: Option<String>,
    pub original_branch: Option<String>,
    pub original_head_commit: Option<String>,
    pub session_id: String,
    pub tmux_session_name: Option<String>,
    pub hook_based: Option<bool>,
    pub creation_duration_ms: Option<u64>,
    pub used_sparse_paths: Option<bool>,
}

/// Process-level current worktree session (CC `currentWorktreeSession`).
static CURRENT_WORKTREE_SESSION: LazyLock<RwLock<Option<WorktreeSession>>> =
    LazyLock::new(|| RwLock::new(None));

/// Maps to: CC `getCurrentWorktreeSession()`.
pub fn get_current_worktree_session() -> Option<WorktreeSession> {
    CURRENT_WORKTREE_SESSION
        .read()
        .ok()
        .and_then(|slot| slot.clone())
}

/// Maps to: CC `restoreWorktreeSession(session)`.
pub fn restore_worktree_session(session: Option<WorktreeSession>) {
    if let Ok(mut slot) = CURRENT_WORKTREE_SESSION.write() {
        *slot = session;
    }
}

/// Persistable subset written to session metadata (CC `saveWorktreeState` strip).
pub fn worktree_session_to_persisted_json(session: &WorktreeSession) -> serde_json::Value {
    serde_json::json!({
        "originalCwd": session.original_cwd,
        "worktreePath": session.worktree_path,
        "worktreeName": session.worktree_name,
        "worktreeBranch": session.worktree_branch,
        "originalBranch": session.original_branch,
        "originalHeadCommit": session.original_head_commit,
        "sessionId": session.session_id,
        "tmuxSessionName": session.tmux_session_name,
        "hookBased": session.hook_based,
    })
}

/// Maps to: CC `utils/worktree.ts` `createAgentWorktree(...)` return shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentWorktreeInfo {
    pub worktree_path: String,
    pub worktree_branch: Option<String>,
    pub head_commit: Option<String>,
    pub git_root: Option<String>,
    pub hook_based: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WorktreeCreateResult {
    worktree_path: PathBuf,
    worktree_branch: String,
    head_commit: String,
    existed: bool,
}

/// Maps to: CC `utils/worktree.ts` `validateWorktreeSlug(...)`.
pub fn validate_worktree_slug(slug: &str) -> Result<(), String> {
    if slug.chars().count() > MAX_WORKTREE_SLUG_LENGTH {
        return Err(format!(
            "Invalid worktree name: must be {MAX_WORKTREE_SLUG_LENGTH} characters or fewer (got {})",
            slug.chars().count()
        ));
    }
    for segment in slug.split('/') {
        if segment == "." || segment == ".." {
            return Err(format!(
                "Invalid worktree name \"{slug}\": must not contain \".\" or \"..\" path segments"
            ));
        }
        if segment.is_empty()
            || !segment
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
        {
            return Err(format!(
                "Invalid worktree name \"{slug}\": each \"/\"-separated segment must be non-empty and contain only letters, digits, dots, underscores, and dashes"
            ));
        }
    }
    Ok(())
}

/// Maps to: CC `utils/worktree.ts` `flattenSlug(...)`.
pub fn flatten_worktree_slug(slug: &str) -> String {
    slug.replace('/', "+")
}

/// Maps to: CC `utils/worktree.ts` `worktreeBranchName(...)`.
pub fn worktree_branch_name(slug: &str) -> String {
    format!("worktree-{}", flatten_worktree_slug(slug))
}

/// Maps to: CC `utils/worktree.ts` `worktreesDir(...)`.
pub fn worktrees_dir(repo_root: impl AsRef<Path>) -> PathBuf {
    repo_root.as_ref().join(".claude").join("worktrees")
}

/// Maps to: CC `utils/worktree.ts` `worktreePathFor(...)`.
pub fn worktree_path_for(repo_root: impl AsRef<Path>, slug: &str) -> PathBuf {
    worktrees_dir(repo_root).join(flatten_worktree_slug(slug))
}

/// Maps to: CC `utils/worktree.ts` `generateTmuxSessionName(...)`.
pub fn generate_tmux_session_name(repo_path: impl AsRef<Path>, branch: &str) -> String {
    let repo_name = repo_path
        .as_ref()
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    format!("{repo_name}_{branch}").replace(['/', '.'], "_")
}

/// Maps to: CC `utils/worktree.ts` `parsePRReference(...)`.
pub fn parse_pr_reference(input: &str) -> Option<u64> {
    PR_URL_RE
        .captures(input)
        .and_then(|captures| captures.get(1))
        .or_else(|| {
            PR_HASH_RE
                .captures(input)
                .and_then(|captures| captures.get(1))
        })
        .and_then(|value| value.as_str().parse::<u64>().ok())
}

fn command_output(mut command: Command) -> anyhow::Result<(i32, String, String)> {
    let output = command.output()?;
    Ok((
        output.status.code().unwrap_or(1),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    ))
}

/// Runs git the way CC worktree.ts does outside its fetches
/// (`execFileNoThrowWithCwd(gitExe(), args, { cwd })`): process.env with
/// nothing added. CC sets `GIT_TERMINAL_PROMPT`/`GIT_ASKPASS` only on the
/// fetch env (`worktree.ts:199-201,260`), and this port does not fetch.
fn git_command_output(cwd: &Path, args: &[&str]) -> anyhow::Result<(i32, String, String)> {
    let mut command = Command::new(crate::utils::git::git_exe());
    crate::utils::subprocess_env::apply_process_env_std(&mut command);
    command.args(args).current_dir(cwd);
    command_output(command)
}

fn git_stdout(cwd: &Path, args: &[&str]) -> anyhow::Result<String> {
    let (code, stdout, stderr) = git_command_output(cwd, args)?;
    if code != 0 {
        anyhow::bail!("git {} failed: {}", args.join(" "), stderr.trim());
    }
    Ok(stdout.trim().to_string())
}

fn read_worktree_head_sha(worktree_path: &Path) -> Option<String> {
    if !worktree_path.exists() {
        return None;
    }
    git_stdout(worktree_path, &["rev-parse", "HEAD"]).ok()
}

fn get_or_create_worktree(repo_root: &Path, slug: &str) -> anyhow::Result<WorktreeCreateResult> {
    let worktree_path = worktree_path_for(repo_root, slug);
    let worktree_branch = worktree_branch_name(slug);
    if let Some(existing_head) = read_worktree_head_sha(&worktree_path) {
        return Ok(WorktreeCreateResult {
            worktree_path,
            worktree_branch,
            head_commit: existing_head,
            existed: true,
        });
    }

    std::fs::create_dir_all(worktrees_dir(repo_root))?;
    let head_commit = git_stdout(repo_root, &["rev-parse", "HEAD"])?;
    let worktree_path_arg = worktree_path.to_string_lossy().to_string();
    let (code, _stdout, stderr) = git_command_output(
        repo_root,
        &[
            "worktree",
            "add",
            "-B",
            &worktree_branch,
            &worktree_path_arg,
            "HEAD",
        ],
    )?;
    if code != 0 {
        anyhow::bail!("Failed to create worktree: {stderr}");
    }
    Ok(WorktreeCreateResult {
        worktree_path,
        worktree_branch,
        head_commit,
        existed: false,
    })
}

fn load_worktree_hooks_config_and_context(
    cwd: &Path,
) -> Option<(
    crate::services::hooks::RegisteredHooks,
    crate::services::hooks::HookContext,
    Vec<(String, String)>,
)> {
    let settings = crate::utils::settings::get_initial_settings();
    if settings.disable_all_hooks == Some(true) {
        return None;
    }
    let hooks_value = settings.hooks?;
    let config = serde_json::from_value::<crate::services::hooks::HooksConfig>(hooks_value).ok()?;
    let config: crate::services::hooks::RegisteredHooks = config
        .iter()
        .map(|(event, entries)| {
            (
                event.clone(),
                entries
                    .iter()
                    .map(crate::schemas::hooks::RegisteredHookMatcher::from_config_entry)
                    .collect(),
            )
        })
        .collect();
    let cwd = cwd.display().to_string();
    let hook_context = crate::services::hooks::HookContext {
        cwd,
        ..Default::default()
    };
    Some((config, hook_context, Vec::new()))
}

/// Maps to: CC `utils/worktree.ts` `createAgentWorktree(...)`.
///
/// Official `WorktreeCreate` hooks take precedence over git worktrees so custom
/// VCS backends can provide the isolation directory. The git fallback
/// deliberately uses local `HEAD` instead of fetching remotes to avoid
/// credential/network prompts inside a subagent launch path.
pub async fn create_agent_worktree(slug: &str) -> anyhow::Result<AgentWorktreeInfo> {
    let cwd = std::env::current_dir()?;
    create_agent_worktree_from_cwd(slug, &cwd).await
}

/// Maps to: CC `utils/worktree.ts` `createAgentWorktree(...)` using
/// `getCwd()` as the repository lookup base. AgentTool passes the parent
/// `ToolUseContext` cwd override here instead of mutating process cwd.
pub async fn create_agent_worktree_from_cwd(
    slug: &str,
    cwd: &Path,
) -> anyhow::Result<AgentWorktreeInfo> {
    validate_worktree_slug(slug).map_err(anyhow::Error::msg)?;

    if let Some((config, hook_context, base_env)) = load_worktree_hooks_config_and_context(cwd) {
        if crate::services::hooks::worktree::has_worktree_create_hook(&config) {
            let base_input = crate::services::hooks::create_base_hook_input(&hook_context);
            let hook_result = crate::services::hooks::worktree::execute_worktree_create_hook(
                &config, slug, base_input, base_env,
            )
            .await
            .map_err(anyhow::Error::msg)?;
            return Ok(AgentWorktreeInfo {
                worktree_path: hook_result.worktree_path,
                worktree_branch: None,
                head_commit: None,
                git_root: None,
                hook_based: Some(true),
            });
        }
    }

    let git_root = crate::utils::git::find_canonical_git_root(cwd).ok_or_else(|| {
        anyhow::anyhow!(
            "Cannot create agent worktree: not in a git repository and no WorktreeCreate hooks are configured. Configure WorktreeCreate/WorktreeRemove hooks in settings.json to use worktree isolation with other VCS systems."
        )
    })?;
    let created = get_or_create_worktree(&git_root, slug)?;
    if created.existed {
        let _ = std::fs::OpenOptions::new()
            .append(true)
            .open(&created.worktree_path);
    }
    Ok(AgentWorktreeInfo {
        worktree_path: created.worktree_path.to_string_lossy().to_string(),
        worktree_branch: Some(created.worktree_branch),
        head_commit: Some(created.head_commit),
        git_root: Some(git_root.to_string_lossy().to_string()),
        hook_based: None,
    })
}

/// Maps to: CC `utils/worktree.ts` `removeAgentWorktree(...)`.
pub fn remove_agent_worktree(
    worktree_path: &str,
    worktree_branch: Option<&str>,
    git_root: Option<&str>,
    hook_based: Option<bool>,
) -> bool {
    if hook_based.unwrap_or(false) {
        return false;
    }
    let Some(git_root) = git_root else {
        return false;
    };
    let root = Path::new(git_root);
    let Ok((remove_code, _stdout, _stderr)) =
        git_command_output(root, &["worktree", "remove", "--force", worktree_path])
    else {
        return false;
    };
    if remove_code != 0 {
        return false;
    }
    if let Some(branch) = worktree_branch {
        let _ = git_command_output(root, &["branch", "-D", branch]);
    }
    true
}

/// Maps to: CC `utils/worktree.ts` `hasWorktreeChanges(...)`.
pub fn has_worktree_changes(worktree_path: &str, head_commit: &str) -> bool {
    let path = Path::new(worktree_path);
    let Ok((status_code, status_stdout, _stderr)) =
        git_command_output(path, &["status", "--porcelain"])
    else {
        return true;
    };
    if status_code != 0 || !status_stdout.trim().is_empty() {
        return true;
    }
    let range = format!("{head_commit}..HEAD");
    let Ok((rev_code, rev_stdout, _stderr)) =
        git_command_output(path, &["rev-list", "--count", &range])
    else {
        return true;
    };
    if rev_code != 0 {
        return true;
    }
    rev_stdout.trim().parse::<u64>().unwrap_or(1) > 0
}

/// Maps to: CC `ExitWorktreeTool.countWorktreeChanges` return shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorktreeChangeSummary {
    pub changed_files: u64,
    pub commits: u64,
}

/// Maps to: CC `ExitWorktreeTool.countWorktreeChanges` — fail-closed (`None` =
/// unknown, treat as unsafe).
pub fn count_worktree_changes(
    worktree_path: &str,
    original_head_commit: Option<&str>,
) -> Option<WorktreeChangeSummary> {
    let path = Path::new(worktree_path);
    let (status_code, status_stdout, _stderr) =
        git_command_output(path, &["status", "--porcelain"]).ok()?;
    if status_code != 0 {
        return None;
    }
    let changed_files = status_stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count() as u64;
    let Some(original_head_commit) = original_head_commit.filter(|s| !s.is_empty()) else {
        // git status succeeded → git repo, but no baseline → fail-closed.
        return None;
    };
    let range = format!("{original_head_commit}..HEAD");
    let (rev_code, rev_stdout, _stderr) =
        git_command_output(path, &["rev-list", "--count", &range]).ok()?;
    if rev_code != 0 {
        return None;
    }
    let commits = rev_stdout.trim().parse::<u64>().unwrap_or(0);
    Some(WorktreeChangeSummary {
        changed_files,
        commits,
    })
}

fn get_branch(cwd: &Path) -> anyhow::Result<String> {
    git_stdout(cwd, &["rev-parse", "--abbrev-ref", "HEAD"])
}

/// Maps to: CC `utils/worktree.ts` `killTmuxSession(...)`.
pub fn kill_tmux_session(session_name: &str) -> bool {
    let mut command = Command::new("tmux");
    // CC killTmuxSession: execFileNoThrow, so process.env.
    crate::utils::subprocess_env::apply_process_env_std(&mut command);
    command.args(["kill-session", "-t", session_name]);
    match command_output(command) {
        Ok((code, _, _)) => code == 0,
        Err(_) => false,
    }
}

/// Maps to: CC `utils/worktree.ts` `createWorktreeForSession(...)`.
///
/// Creates (or resumes) a session-scoped worktree and records
/// [`CURRENT_WORKTREE_SESSION`]. Caller is responsible for `chdir` /
/// `setOriginalCwd` / prompt cache invalidation (EnterWorktreeTool.call).
pub async fn create_worktree_for_session(
    session_id: &str,
    slug: &str,
    tmux_session_name: Option<String>,
) -> anyhow::Result<WorktreeSession> {
    validate_worktree_slug(slug).map_err(anyhow::Error::msg)?;
    let original_cwd = std::env::current_dir()?.to_string_lossy().to_string();
    let original_branch = get_branch(Path::new(&original_cwd)).ok();

    let info = create_agent_worktree_from_cwd(slug, Path::new(&original_cwd)).await?;
    let session = WorktreeSession {
        original_cwd,
        worktree_path: info.worktree_path,
        worktree_name: slug.to_string(),
        worktree_branch: info.worktree_branch,
        original_branch,
        original_head_commit: info.head_commit,
        session_id: session_id.to_string(),
        tmux_session_name,
        hook_based: info.hook_based,
        creation_duration_ms: None,
        used_sparse_paths: Some(
            crate::utils::settings::get_initial_settings()
                .worktree
                .as_ref()
                .and_then(|wt| wt.sparse_paths.as_ref())
                .map(|paths| !paths.is_empty())
                .unwrap_or(false),
        ),
    };
    restore_worktree_session(Some(session.clone()));
    // Maps to: CC `worktree.ts:772-775` — the whole session object persists
    // into the project config for cross-restart recovery.
    let persisted = session.clone();
    let _ = crate::utils::config::save_current_project_config(move |config| {
        config.active_worktree_session = Some(persisted);
    });
    Ok(session)
}

/// Maps to: CC `utils/worktree.ts` `keepWorktree()` — chdir back, clear session,
/// leave worktree on disk.
pub fn keep_worktree() {
    let Some(session) = get_current_worktree_session() else {
        return;
    };
    // CC wraps the whole body in try/catch (:785-811): a failing
    // `process.chdir(originalCwd)` throws into the catch and NOTHING after
    // it runs — the session stays set and the config slot stays populated.
    if std::env::set_current_dir(&session.original_cwd).is_err() {
        return;
    }
    restore_worktree_session(None);
    // Maps to: CC `worktree.ts:795-798` — clear the persisted session slot.
    let _ = crate::utils::config::save_current_project_config(|config| {
        config.active_worktree_session = None;
    });
}

/// Maps to: CC `utils/worktree.ts` `cleanupWorktree()` — chdir back, remove
/// worktree/branch (git) or leave hook-based path, clear session.
pub fn cleanup_worktree() {
    let Some(session) = get_current_worktree_session() else {
        return;
    };
    let original = session.original_cwd.clone();
    // CC wraps the whole body in try/catch (:818-869): a failing
    // `process.chdir(originalCwd)` throws into the catch and nothing after
    // it runs — no removal, session stays set, config slot stays populated.
    if std::env::set_current_dir(&original).is_err() {
        return;
    }

    if !session.hook_based.unwrap_or(false) {
        let git_root = crate::utils::git::find_canonical_git_root(Path::new(&original))
            .map(|p| p.to_string_lossy().to_string());
        let _ = remove_agent_worktree(
            &session.worktree_path,
            session.worktree_branch.as_deref(),
            git_root.as_deref(),
            session.hook_based,
        );
    }
    // The hook-based branch above leaves the directory best-effort when hook
    // execution is not fully wired (matches CC's warn branch); both branches
    // fall through to the shared teardown like CC's single try-block tail.
    restore_worktree_session(None);
    // Maps to: CC `worktree.ts:861-864` — clear the persisted session slot.
    let _ = crate::utils::config::save_current_project_config(|config| {
        config.active_worktree_session = None;
    });
}

/// Maps to: CC `components/WorktreeExitDialog.tsx` `CommandResultDisplay`
/// usage for the no-session branch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandResultDisplay {
    Skip,
    System,
    User,
}

/// Rust representation of official `LocalJSXCommandOnDone` result/options.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorktreeExitDone {
    pub result: Option<String>,
    pub display: Option<CommandResultDisplay>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorktreeExitAction {
    Keep,
    KeepWithTmux,
    KeepKillTmux,
    Remove,
    RemoveWithTmux,
}

impl WorktreeExitAction {
    pub fn value(self) -> &'static str {
        match self {
            Self::Keep => "keep",
            Self::KeepWithTmux => "keep-with-tmux",
            Self::KeepKillTmux => "keep-kill-tmux",
            Self::Remove => "remove",
            Self::RemoveWithTmux => "remove-with-tmux",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeExitOption {
    pub label: String,
    pub value: String,
    pub description: String,
}

pub fn worktree_exit_action_from_value(value: &str) -> Option<WorktreeExitAction> {
    match value {
        "keep" => Some(WorktreeExitAction::Keep),
        "keep-with-tmux" => Some(WorktreeExitAction::KeepWithTmux),
        "keep-kill-tmux" => Some(WorktreeExitAction::KeepKillTmux),
        "remove" => Some(WorktreeExitAction::Remove),
        "remove-with-tmux" => Some(WorktreeExitAction::RemoveWithTmux),
        _ => None,
    }
}

/// Maps to: CC `components/WorktreeExitDialog.tsx` subtitle construction.
pub fn worktree_exit_subtitle(
    session: &WorktreeSession,
    changed_file_count: usize,
    commit_count: usize,
) -> String {
    let branch_name = session.worktree_branch.clone().unwrap_or_default();
    let has_uncommitted = changed_file_count > 0;
    let has_commits = commit_count > 0;

    if has_uncommitted && has_commits {
        format!(
            "You have {changed_file_count} uncommitted {} and {commit_count} {} on {branch_name}. All will be lost if you remove.",
            if changed_file_count == 1 {
                "file"
            } else {
                "files"
            },
            if commit_count == 1 {
                "commit"
            } else {
                "commits"
            },
        )
    } else if has_uncommitted {
        format!(
            "You have {changed_file_count} uncommitted {}. These will be lost if you remove the worktree.",
            if changed_file_count == 1 {
                "file"
            } else {
                "files"
            },
        )
    } else if has_commits {
        format!(
            "You have {commit_count} {} on {branch_name}. The branch will be deleted if you remove the worktree.",
            if commit_count == 1 {
                "commit"
            } else {
                "commits"
            },
        )
    } else {
        "You are working in a worktree. Keep it to continue working there, or remove it to clean up."
            .to_string()
    }
}

/// Maps to: CC `components/WorktreeExitDialog.tsx` `options` and
/// `defaultValue` construction.
pub fn worktree_exit_options(
    session: &WorktreeSession,
    changed_file_count: usize,
    commit_count: usize,
) -> (Vec<WorktreeExitOption>, String) {
    let remove_description = if changed_file_count > 0 || commit_count > 0 {
        "All changes and commits will be lost."
    } else {
        "Clean up the worktree directory."
    };

    if let Some(tmux_session_name) = &session.tmux_session_name {
        (
            vec![
                WorktreeExitOption {
                    label: "Keep worktree and tmux session".to_string(),
                    value: WorktreeExitAction::KeepWithTmux.value().to_string(),
                    description: format!(
                        "Stays at {}. Reattach with: tmux attach -t {}",
                        session.worktree_path, tmux_session_name
                    ),
                },
                WorktreeExitOption {
                    label: "Keep worktree, kill tmux session".to_string(),
                    value: WorktreeExitAction::KeepKillTmux.value().to_string(),
                    description: format!(
                        "Keeps worktree at {}, terminates tmux session.",
                        session.worktree_path
                    ),
                },
                WorktreeExitOption {
                    label: "Remove worktree and tmux session".to_string(),
                    value: WorktreeExitAction::RemoveWithTmux.value().to_string(),
                    description: remove_description.to_string(),
                },
            ],
            WorktreeExitAction::KeepWithTmux.value().to_string(),
        )
    } else {
        (
            vec![
                WorktreeExitOption {
                    label: "Keep worktree".to_string(),
                    value: WorktreeExitAction::Keep.value().to_string(),
                    description: format!("Stays at {}", session.worktree_path),
                },
                WorktreeExitOption {
                    label: "Remove worktree".to_string(),
                    value: WorktreeExitAction::Remove.value().to_string(),
                    description: remove_description.to_string(),
                },
            ],
            WorktreeExitAction::Keep.value().to_string(),
        )
    }
}

/// Maps to: CC `components/WorktreeExitDialog.tsx` `handleSelect` result
/// message construction after keeping/removing.
pub fn worktree_exit_result_message(
    session: &WorktreeSession,
    changed_file_count: usize,
    commit_count: usize,
    action: WorktreeExitAction,
) -> String {
    let has_tmux = session.tmux_session_name.is_some();
    let branch_name = session.worktree_branch.clone().unwrap_or_default();

    match action {
        WorktreeExitAction::Keep | WorktreeExitAction::KeepWithTmux => {
            if let Some(tmux_session_name) = &session.tmux_session_name {
                format!(
                    "Worktree kept. Your work is saved at {} on branch {}. Reattach to tmux session with: tmux attach -t {}",
                    session.worktree_path, branch_name, tmux_session_name
                )
            } else {
                format!(
                    "Worktree kept. Your work is saved at {} on branch {}",
                    session.worktree_path, branch_name
                )
            }
        }
        WorktreeExitAction::KeepKillTmux => format!(
            "Worktree kept at {} on branch {}. Tmux session terminated.",
            session.worktree_path, branch_name
        ),
        WorktreeExitAction::Remove | WorktreeExitAction::RemoveWithTmux => {
            let tmux_note = if has_tmux {
                " Tmux session terminated."
            } else {
                ""
            };
            if commit_count > 0 && changed_file_count > 0 {
                format!(
                    "Worktree removed. {commit_count} {} and uncommitted changes were discarded.{tmux_note}",
                    if commit_count == 1 {
                        "commit"
                    } else {
                        "commits"
                    },
                )
            } else if commit_count > 0 {
                format!(
                    "Worktree removed. {commit_count} {} on {branch_name} {} discarded.{tmux_note}",
                    if commit_count == 1 {
                        "commit"
                    } else {
                        "commits"
                    },
                    if commit_count == 1 { "was" } else { "were" },
                )
            } else if changed_file_count > 0 {
                format!("Worktree removed. Uncommitted changes were discarded.{tmux_note}")
            } else {
                format!("Worktree removed.{tmux_note}")
            }
        }
    }
}

/// Maps to: CC no-change/no-commit silent cleanup result message.
pub fn worktree_exit_clean_result_message() -> &'static str {
    "Worktree removed (no changes)"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    fn session(tmux: bool) -> WorktreeSession {
        WorktreeSession {
            original_cwd: "/repo".to_string(),
            worktree_path: "/repo-feature".to_string(),
            worktree_name: "feature".to_string(),
            worktree_branch: Some("cometix/feature".to_string()),
            original_branch: Some("main".to_string()),
            original_head_commit: Some("abc123".to_string()),
            session_id: "session-1".to_string(),
            tmux_session_name: tmux.then(|| "repo_cometix_feature".to_string()),
            hook_based: None,
            creation_duration_ms: None,
            used_sparse_paths: None,
        }
    }

    struct CwdGuard {
        old: PathBuf,
    }

    impl CwdGuard {
        fn set(path: &Path) -> Self {
            let old = std::env::current_dir().unwrap();
            std::env::set_current_dir(path).unwrap();
            Self { old }
        }
    }

    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.old);
        }
    }

    fn temp_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("cometix-{name}-{}", uuid::Uuid::new_v4()))
    }

    fn git_available() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }

    fn run_git(cwd: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_ASKPASS", "")
            .status()
            .unwrap();
        assert!(status.success(), "git {} failed", args.join(" "));
    }

    #[test]
    fn worktree_slug_branch_tmux_and_pr_helpers_match_official() {
        validate_worktree_slug("agent-a1234567").unwrap();
        validate_worktree_slug("user/feature-1.2_3").unwrap();
        assert!(validate_worktree_slug("../escape").is_err());
        assert!(validate_worktree_slug("bad+plus").is_err());
        assert!(validate_worktree_slug(&"a".repeat(65)).is_err());
        assert_eq!(flatten_worktree_slug("user/feature"), "user+feature");
        assert_eq!(
            worktree_branch_name("user/feature"),
            "worktree-user+feature"
        );
        assert_eq!(
            worktree_path_for("/repo", "user/feature"),
            PathBuf::from("/repo/.claude/worktrees/user+feature")
        );
        assert_eq!(
            generate_tmux_session_name("/tmp/my.repo", "feature/foo.bar"),
            "my_repo_feature_foo_bar"
        );
        assert_eq!(parse_pr_reference("#123"), Some(123));
        assert_eq!(
            parse_pr_reference("https://github.example.com/org/repo/pull/456?foo=bar"),
            Some(456)
        );
        assert_eq!(
            parse_pr_reference("https://example.com/org/repo/issues/456"),
            None
        );
    }

    /// Maps to: CC `worktree.ts:795-798` (keepWorktree) and :861-864
    /// (cleanupWorktree) — both clear the persisted project-config slot that
    /// createWorktreeForSession populated (:772-775).
    #[test]
    fn keep_and_cleanup_clear_the_persisted_worktree_session_slot() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        // Without COMETIX_WRITE_ENABLED the config saves are DRY_RUN no-ops
        // and every `is_none()` below passes vacuously.
        let _write_enabled = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let config_dir = temp_root("worktree-session-slot-config");
        std::fs::create_dir_all(&config_dir).unwrap();
        let _config_dir =
            EnvVarGuard::set("CLAUDE_CONFIG_DIR", config_dir.to_string_lossy().as_ref());

        // keep/cleanup now mirror CC's try/catch: a failing chdir aborts the
        // teardown, so the session must point at a real directory.
        let original_dir = temp_root("worktree-session-slot-original");
        std::fs::create_dir_all(&original_dir).unwrap();
        let real_session = |tmux: bool| {
            let mut value = session(tmux);
            value.original_cwd = original_dir.to_string_lossy().to_string();
            value
        };

        restore_worktree_session(Some(real_session(false)));
        let _ = crate::utils::config::save_current_project_config(|config| {
            config.active_worktree_session = Some(real_session(false));
        });
        // The populated slot must be observable first — otherwise the clears
        // below are indistinguishable from writes never happening at all.
        assert!(
            crate::utils::config::get_current_project_config()
                .active_worktree_session
                .is_some()
        );
        keep_worktree();
        assert!(get_current_worktree_session().is_none());
        assert!(
            crate::utils::config::get_current_project_config()
                .active_worktree_session
                .is_none()
        );

        // A failing chdir (CC: throw into the catch) leaves BOTH the session
        // and the persisted slot untouched.
        restore_worktree_session(Some(session(false)));
        let _ = crate::utils::config::save_current_project_config(|config| {
            config.active_worktree_session = Some(session(false));
        });
        keep_worktree();
        assert!(get_current_worktree_session().is_some());
        assert!(
            crate::utils::config::get_current_project_config()
                .active_worktree_session
                .is_some()
        );
        restore_worktree_session(None);
        let _ = crate::utils::config::save_current_project_config(|config| {
            config.active_worktree_session = None;
        });

        // The hook-based branch skips git removal but still reaches the
        // shared teardown tail.
        let mut hook_session = real_session(false);
        hook_session.hook_based = Some(true);
        restore_worktree_session(Some(hook_session.clone()));
        let _ = crate::utils::config::save_current_project_config(|config| {
            config.active_worktree_session = Some(hook_session);
        });
        assert!(
            crate::utils::config::get_current_project_config()
                .active_worktree_session
                .is_some()
        );
        cleanup_worktree();
        assert!(get_current_worktree_session().is_none());
        assert!(
            crate::utils::config::get_current_project_config()
                .active_worktree_session
                .is_none()
        );

        let _ = std::fs::remove_dir_all(config_dir);
    }

    /// Maps to: CC `worktree.ts:772-775` — createWorktreeForSession persists
    /// the WHOLE session object into the project config (the runtime
    /// structural assignment, not config.ts:126-133's declared six-field
    /// subset).
    #[tokio::test(flavor = "current_thread")]
    async fn create_worktree_for_session_persists_the_whole_session_object() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _write_enabled = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let config_dir = temp_root("worktree-create-slot-config");
        std::fs::create_dir_all(&config_dir).unwrap();
        let _config_dir =
            EnvVarGuard::set("CLAUDE_CONFIG_DIR", config_dir.to_string_lossy().as_ref());
        // The scratch root above also relocates `~/.claude.json`
        // (`utils/config.rs:119-120`), leaving the workspace UNTRUSTED — and CC
        // runs no hooks at all in that state (`utils/hooks.ts:1994-1999`,
        // `utils/config.ts:705-743`), which `should_skip_hook_execution` now
        // honours. This test is about the WorktreeCreate hook supplying the
        // path that gets persisted, so it states trust rather than inheriting
        // it from a config root it just replaced.
        let _trust = crate::services::hooks::test_support::SessionTrustGuard::accepted();
        let previous_flag_settings = crate::bootstrap::state::get_flag_settings_inline();
        let root = temp_root("worktree-create-slot");
        std::fs::create_dir_all(&root).unwrap();
        let hook_worktree = root.join("hook-created-worktree");
        let command = format!("printf '{}\\n'", hook_worktree.display());
        crate::bootstrap::state::set_flag_settings_inline(Some(serde_json::json!({
            "hooks": {
                "WorktreeCreate": [
                    { "hooks": [{ "type": "command", "command": command, "timeout": 5 }] }
                ]
            }
        })));
        restore_worktree_session(None);

        let session = create_worktree_for_session("session-slot", "slot-hook1234", None)
            .await
            .unwrap();
        let persisted = crate::utils::config::get_current_project_config()
            .active_worktree_session
            .expect("createWorktreeForSession persists activeWorktreeSession");
        assert_eq!(persisted, session);
        assert_eq!(persisted.hook_based, Some(true));

        restore_worktree_session(None);
        let _ = crate::utils::config::save_current_project_config(|config| {
            config.active_worktree_session = None;
        });
        crate::bootstrap::state::set_flag_settings_inline(previous_flag_settings);
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(config_dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn create_agent_worktree_git_path_detects_changes_and_removes_like_official() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let previous_flag_settings = crate::bootstrap::state::get_flag_settings_inline();
        crate::bootstrap::state::set_flag_settings_inline(None);
        if !git_available() {
            crate::bootstrap::state::set_flag_settings_inline(previous_flag_settings);
            return;
        }
        let root = temp_root("agent-worktree");
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        run_git(&repo, &["init"]);
        run_git(&repo, &["config", "user.email", "test@example.com"]);
        run_git(&repo, &["config", "user.name", "Cometix Test"]);
        std::fs::write(repo.join("README.md"), "initial\n").unwrap();
        run_git(&repo, &["add", "README.md"]);
        run_git(
            &repo,
            &["-c", "commit.gpgsign=false", "commit", "-m", "initial"],
        );
        let _cwd = CwdGuard::set(&repo);

        let info = create_agent_worktree("agent-a1234567").await.unwrap();
        let worktree_path = PathBuf::from(&info.worktree_path);
        assert!(worktree_path.join("README.md").exists());
        assert_eq!(
            info.worktree_branch.as_deref(),
            Some("worktree-agent-a1234567")
        );
        let repo_canonical = repo.canonicalize().unwrap().to_string_lossy().to_string();
        assert_eq!(info.git_root.as_deref(), Some(repo_canonical.as_str()));
        assert!(!has_worktree_changes(
            &info.worktree_path,
            info.head_commit.as_deref().unwrap()
        ));

        std::fs::write(worktree_path.join("README.md"), "changed\n").unwrap();
        assert!(has_worktree_changes(
            &info.worktree_path,
            info.head_commit.as_deref().unwrap()
        ));
        assert!(remove_agent_worktree(
            &info.worktree_path,
            info.worktree_branch.as_deref(),
            info.git_root.as_deref(),
            info.hook_based,
        ));
        assert!(!worktree_path.exists());
        drop(_cwd);
        crate::bootstrap::state::set_flag_settings_inline(previous_flag_settings);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn create_agent_worktree_prefers_worktree_create_hook_like_official() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let previous_flag_settings = crate::bootstrap::state::get_flag_settings_inline();
        let root = temp_root("agent-worktree-hook");
        std::fs::create_dir_all(&root).unwrap();
        let hook_worktree = root.join("hook-created-worktree");
        let command = format!("printf '{}\\n'", hook_worktree.display());
        // The type tag is required: HookCommandSchema discriminates on it, and
        // the settings loader now validates flag settings through the carrier.
        crate::bootstrap::state::set_flag_settings_inline(Some(serde_json::json!({
            "hooks": {
                "WorktreeCreate": [
                    { "hooks": [{ "type": "command", "command": command, "timeout": 5 }] }
                ]
            }
        })));

        let info = create_agent_worktree_from_cwd("agent-hook1234", &root)
            .await
            .unwrap();
        assert_eq!(
            info.worktree_path,
            hook_worktree.to_string_lossy().to_string()
        );
        assert_eq!(info.worktree_branch, None);
        assert_eq!(info.head_commit, None);
        assert_eq!(info.git_root, None);
        assert_eq!(info.hook_based, Some(true));

        crate::bootstrap::state::set_flag_settings_inline(previous_flag_settings);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn worktree_exit_subtitles_match_official_branches() {
        let s = session(false);

        assert_eq!(
            worktree_exit_subtitle(&s, 2, 3),
            "You have 2 uncommitted files and 3 commits on cometix/feature. All will be lost if you remove."
        );
        assert_eq!(
            worktree_exit_subtitle(&s, 1, 0),
            "You have 1 uncommitted file. These will be lost if you remove the worktree."
        );
        assert_eq!(
            worktree_exit_subtitle(&s, 0, 1),
            "You have 1 commit on cometix/feature. The branch will be deleted if you remove the worktree."
        );
        assert_eq!(
            worktree_exit_subtitle(&s, 0, 0),
            "You are working in a worktree. Keep it to continue working there, or remove it to clean up."
        );
    }

    #[test]
    fn worktree_exit_options_match_official_tmux_and_plain_shapes() {
        let (plain, plain_default) = worktree_exit_options(&session(false), 0, 0);
        assert_eq!(plain_default, "keep");
        assert_eq!(plain[0].label, "Keep worktree");
        assert_eq!(plain[0].value, "keep");
        assert_eq!(plain[0].description, "Stays at /repo-feature");
        assert_eq!(plain[1].label, "Remove worktree");
        assert_eq!(plain[1].value, "remove");
        assert_eq!(plain[1].description, "Clean up the worktree directory.");

        let (tmux, tmux_default) = worktree_exit_options(&session(true), 1, 2);
        assert_eq!(tmux_default, "keep-with-tmux");
        assert_eq!(tmux[0].label, "Keep worktree and tmux session");
        assert_eq!(tmux[0].value, "keep-with-tmux");
        assert_eq!(
            tmux[0].description,
            "Stays at /repo-feature. Reattach with: tmux attach -t repo_cometix_feature"
        );
        assert_eq!(tmux[1].label, "Keep worktree, kill tmux session");
        assert_eq!(tmux[1].value, "keep-kill-tmux");
        assert_eq!(tmux[2].label, "Remove worktree and tmux session");
        assert_eq!(tmux[2].description, "All changes and commits will be lost.");
    }

    #[test]
    fn worktree_exit_result_messages_match_official_branches() {
        let plain = session(false);
        assert_eq!(
            worktree_exit_result_message(&plain, 0, 0, WorktreeExitAction::Keep),
            "Worktree kept. Your work is saved at /repo-feature on branch cometix/feature"
        );
        assert_eq!(
            worktree_exit_result_message(&plain, 2, 3, WorktreeExitAction::Remove),
            "Worktree removed. 3 commits and uncommitted changes were discarded."
        );
        assert_eq!(
            worktree_exit_result_message(&plain, 0, 1, WorktreeExitAction::Remove),
            "Worktree removed. 1 commit on cometix/feature was discarded."
        );
        assert_eq!(
            worktree_exit_result_message(&plain, 2, 0, WorktreeExitAction::Remove),
            "Worktree removed. Uncommitted changes were discarded."
        );

        let tmux = session(true);
        assert_eq!(
            worktree_exit_result_message(&tmux, 0, 0, WorktreeExitAction::KeepWithTmux),
            "Worktree kept. Your work is saved at /repo-feature on branch cometix/feature. Reattach to tmux session with: tmux attach -t repo_cometix_feature"
        );
        assert_eq!(
            worktree_exit_result_message(&tmux, 0, 2, WorktreeExitAction::RemoveWithTmux),
            "Worktree removed. 2 commits on cometix/feature were discarded. Tmux session terminated."
        );
        assert_eq!(
            worktree_exit_result_message(&tmux, 0, 0, WorktreeExitAction::KeepKillTmux),
            "Worktree kept at /repo-feature on branch cometix/feature. Tmux session terminated."
        );
    }
}
