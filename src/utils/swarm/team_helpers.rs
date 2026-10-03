//! Team state helpers for the TeamCreate/TeamDelete/spawnMultiAgent tools.
//!
//! Maps to: CC `utils/swarm/teamHelpers.ts` (`sanitizeName`,
//! `sanitizeAgentName`, `getTeamDir`, `getTeamFilePath`, `readTeamFile`,
//! `writeTeamFile`, hidden-pane/member mutation helpers, and cleanup helpers).
//! Official Claude Code persists `<teamsDir>/<sanitizeName(team)>/config.json`;
//! Cometix keeps the same file shape for pane-backed teammates while retaining
//! the in-memory leader cache used by the current in-process runner.

#[cfg(test)]
use crate::utils::test_env::TestStateLock;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

/// Maps to: CC `utils/swarm/teamHelpers.ts#TeamAllowedPath`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TeamAllowedPathRecord {
    pub(crate) path: String,
    #[serde(rename = "toolName")]
    pub(crate) tool_name: String,
    #[serde(rename = "addedBy")]
    pub(crate) added_by: String,
    #[serde(rename = "addedAt")]
    pub(crate) added_at_ms: u64,
}

/// Maps to: CC `utils/swarm/teamHelpers.ts#TeamFile.members[number]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TeamMemberRecord {
    #[serde(rename = "agentId")]
    pub(crate) agent_id: String,
    pub(crate) name: String,
    #[serde(rename = "agentType", skip_serializing_if = "Option::is_none", default)]
    pub(crate) agent_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub(crate) model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub(crate) prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub(crate) color: Option<String>,
    #[serde(
        rename = "planModeRequired",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub(crate) plan_mode_required: Option<bool>,
    #[serde(rename = "joinedAt")]
    pub(crate) joined_at_ms: u64,
    #[serde(rename = "tmuxPaneId")]
    pub(crate) tmux_pane_id: String,
    pub(crate) cwd: String,
    #[serde(
        rename = "worktreePath",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub(crate) worktree_path: Option<String>,
    #[serde(rename = "sessionId", skip_serializing_if = "Option::is_none", default)]
    pub(crate) session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) subscriptions: Vec<String>,
    #[serde(
        rename = "backendType",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub(crate) backend_type: Option<String>,
    #[serde(rename = "isActive", skip_serializing_if = "Option::is_none", default)]
    pub(crate) is_active: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub(crate) mode: Option<String>,
}

/// Maps to: CC `utils/swarm/teamHelpers.ts#TeamFile`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TeamRecord {
    #[serde(rename = "name")]
    pub(crate) team_name: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub(crate) description: Option<String>,
    #[serde(rename = "createdAt")]
    pub(crate) created_at_ms: u64,
    /// Runtime convenience field; official JSON uses the path as file location,
    /// not a `TeamFile` property.
    #[serde(skip, default)]
    pub(crate) team_file_path: String,
    #[serde(rename = "leadAgentId")]
    pub(crate) lead_agent_id: String,
    #[serde(
        rename = "leadSessionId",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub(crate) lead_session_id: Option<String>,
    #[serde(
        rename = "hiddenPaneIds",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub(crate) hidden_pane_ids: Option<Vec<String>>,
    #[serde(
        rename = "teamAllowedPaths",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub(crate) team_allowed_paths: Option<Vec<TeamAllowedPathRecord>>,
    pub(crate) members: Vec<TeamMemberRecord>,
}

/// Single-team in-memory state (a leader manages at most one team).
pub(crate) static TEAM_TOOL_STATE: LazyLock<Mutex<Option<TeamRecord>>> =
    LazyLock::new(|| Mutex::new(None));

fn now_ms() -> u64 {
    chrono::Utc::now().timestamp_millis().max(0) as u64
}

fn maybe_skip_disk_io_in_tests() -> bool {
    #[cfg(test)]
    {
        !crate::utils::env_utils::is_env_truthy(
            crate::utils::process_env::var("COMETIX_TEST_TEAM_FILE_IO").as_deref(),
        )
    }
    #[cfg(not(test))]
    {
        false
    }
}

/// Maps to: CC `teamHelpers.ts#sanitizeName`.
pub fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect()
}

/// Maps to: CC `teamHelpers.ts#sanitizeAgentName`.
pub fn sanitize_agent_name(name: &str) -> String {
    name.replace('@', "-")
}

/// Maps to: CC `teamHelpers.ts#getTeamDir`.
pub fn get_team_dir(team_name: &str) -> PathBuf {
    crate::utils::env_utils::get_teams_dir().join(sanitize_name(team_name))
}

fn get_team_file_path_buf(team_name: &str) -> PathBuf {
    get_team_dir(team_name).join("config.json")
}

/// Maps to: CC `teamHelpers.ts#getTeamFilePath`.
pub fn get_team_file_path(team_name: &str) -> String {
    get_team_file_path_buf(team_name).display().to_string()
}

/// Maps to: CC `teamHelpers.ts:130-142` `readTeamFile` — a pure
/// `readFileSync` per call with no cache; `null` on ENOENT/parse failure.
/// (Rust's record type is named `TeamRecord` where CC's is `TeamFile`; the
/// function name follows the CC function.)
pub(crate) fn read_team_file(team_name: &str) -> Option<TeamRecord> {
    let path = get_team_file_path_buf(team_name);
    let content = std::fs::read_to_string(&path).ok()?;
    let mut record = serde_json::from_str::<TeamRecord>(&content).ok()?;
    record.team_file_path = path.display().to_string();
    Some(record)
}

/// Maps to: CC `TeamCreateTool.ts:66` `!readTeamFile(providedName)` — a pure
/// disk existence probe. The memory-OR here is the same invented carrier
/// shortcut as [`memory_first_team_record`] (equivalent in production where a
/// memory record implies an on-disk file; divergent under DRY_RUN tests) —
/// same migration ledger: converge on the disk read when TeamCreate's batch
/// revisits it.
pub(crate) fn team_record_exists(team_name: &str) -> bool {
    TEAM_TOOL_STATE
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|record| record.team_name == team_name)
        || get_team_file_path_buf(team_name).exists()
}

fn write_team_record_to_disk(record: &TeamRecord) -> Result<(), String> {
    let path = get_team_file_path_buf(&record.team_name);
    let dir = path
        .parent()
        .ok_or_else(|| format!("Invalid team file path: {}", path.display()))?;
    std::fs::create_dir_all(dir)
        .map_err(|err| format!("Failed to create team directory {}: {err}", dir.display()))?;
    let content = serde_json::to_string_pretty(record)
        .map_err(|err| format!("Failed to serialize team file: {err}"))?;
    std::fs::write(&path, content)
        .map_err(|err| format!("Failed to write team file {}: {err}", path.display()))
}

/// Read the leader's current in-memory team record for AppState-style
/// consumers such as the inbox poller.
/// Maps to: CC `AppState.teamContext` as populated by `TeamCreateTool`.
pub(crate) fn current_team_record() -> Option<TeamRecord> {
    TEAM_TOOL_STATE.lock().unwrap().clone()
}

/// Invented memory-first read with NO CC counterpart — the name says so on
/// purpose. `TEAM_TOOL_STATE` (this port's carrier for CC
/// `appState.teamContext`) wins on a name hit; a miss falls back to disk and
/// backfills. Disk-authoritative reads (teammate processes flip `isActive` on
/// disk) must never come through here.
///
/// Migration ledger (consumers whose CC call sites use the pure disk
/// `readTeamFile`/`readTeamFileAsync`) — CLOSED:
/// - SendMessage broadcast/shutdown (#97)
/// - `spawn_multi_agent`'s three write chains (#104 cut 2)
/// - every `teamHelpers.ts` mutate chain in this file plus
///   `permission_sync.rs#get_leader_name` (#104 K6). CC `teamHelpers.ts`
///   contains zero `appState`/`teamContext` reads: every mutator is
///   `readTeamFile → modify → writeTeamFile`
///   (`teamHelpers.ts:200/236/260/289/330/362/419/599/645`, plus the async
///   pair at `:459`/`:481`), and `permissionSync.ts:657` is
///   `readTeamFileAsync`.
///
/// The only remaining caller — and the only one that legitimately stays
/// memory-first — is SendMessage's `teammate_color_by_name`
/// (`send_message_tool/mod.rs:276`), whose CC counterpart `findTeammateColor`
/// (`SendMessageTool.ts:133-147`) reads `appState.teamContext.teammates`, not
/// a team file. Do not add new callers without the same evidence.
pub(crate) fn memory_first_team_record(team_name: &str) -> Option<TeamRecord> {
    if let Some(record) = TEAM_TOOL_STATE
        .lock()
        .unwrap()
        .as_ref()
        .filter(|record| record.team_name == team_name)
        .cloned()
    {
        return Some(record);
    }

    let record = read_team_file(team_name)?;
    *TEAM_TOOL_STATE.lock().unwrap() = Some(record.clone());
    Some(record)
}

/// Maps to: CC `teamHelpers.ts#writeTeamFile` / `writeTeamFileAsync`.
pub(crate) fn write_team_record(record: TeamRecord) {
    let _ = write_team_record_result(record);
}

/// Fallible write path used by tools that need to surface official file errors.
pub(crate) fn write_team_record_result(mut record: TeamRecord) -> Result<(), String> {
    record.team_file_path = get_team_file_path(&record.team_name);
    if !maybe_skip_disk_io_in_tests() {
        write_team_record_to_disk(&record)?;
    }
    *TEAM_TOOL_STATE.lock().unwrap() = Some(record);
    Ok(())
}

/// Maps to: CC `TeamCreateTool.ts` `TeamFile` construction.
pub(crate) fn create_team_record(
    team_name: String,
    description: Option<String>,
    lead_agent_type: Option<String>,
    lead_model: Option<String>,
    cwd: String,
) -> TeamRecord {
    let lead_agent_id = crate::utils::agent_id::format_agent_id(
        crate::utils::swarm::constants::TEAM_LEAD_NAME,
        &team_name,
    );
    let team_file_path = get_team_file_path(&team_name);
    TeamRecord {
        team_name: team_name.clone(),
        description,
        created_at_ms: now_ms(),
        team_file_path,
        lead_agent_id: lead_agent_id.clone(),
        lead_session_id: Some(crate::bootstrap::state::get_session_id()),
        hidden_pane_ids: None,
        team_allowed_paths: None,
        members: vec![TeamMemberRecord {
            agent_id: lead_agent_id,
            name: crate::utils::swarm::constants::TEAM_LEAD_NAME.to_string(),
            agent_type: lead_agent_type,
            model: lead_model,
            prompt: None,
            color: None,
            plan_mode_required: None,
            joined_at_ms: now_ms(),
            tmux_pane_id: String::new(),
            cwd,
            worktree_path: None,
            session_id: None,
            subscriptions: Vec::new(),
            backend_type: None,
            is_active: None,
            mode: None,
        }],
    }
}

/// Maps to: CC `spawnMultiAgent.ts:267-292` `generateUniqueTeammateName`.
/// The member-name census is a pure disk read (`readTeamFileAsync`,
/// `spawnMultiAgent.ts:275`) — a stale leader snapshot here would hand out a
/// name a teammate process already took on disk.
///
/// PLACEMENT SEAM (recorded 2026-08-30, #167 verification batch): the `Maps to`
/// above is the whole finding — this symbol is DECLARED in
/// `tools/shared/spawnMultiAgent.ts`, so it belongs in
/// `tools/shared/spawn_multi_agent.rs`, not in the file named after
/// `teamHelpers.ts`. `teamHelpers.ts` declares no such export (verified:
/// `generateUniqueTeammateName` matches only spawnMultiAgent.ts:267 in
/// `../rebuild/src`), and all three production callers are already in
/// spawn_multi_agent.rs (`:482`, `:923`, `:1104`) — the same shape that got
/// `getDefaultTeammateModel`/`resolveTeammateModel` moved OUT of
/// `teammate_model.rs` on 2026-08-29. Not moved with the note because the test
/// that covers it (`unique_teammate_name_checks_existing_members_case_
/// insensitively`) is built on four private test helpers of this module
/// (`TEST_TEAM_HELPERS_LOCK`, `team_disk_guards`, `create_team_record`,
/// `clear_team_tool_state_for_test`), so the move is a teamHelpers.ts
/// file-alignment batch, not a two-file edit. Ledger: MODULE_MAP rows
/// `tools/shared/spawnMultiAgent.ts` and `utils/swarm/teamHelpers.ts`.
pub(crate) fn generate_unique_teammate_name(base_name: &str, team_name: Option<&str>) -> String {
    let Some(team_name) = team_name else {
        return base_name.to_string();
    };
    let Some(record) = read_team_file(team_name) else {
        return base_name.to_string();
    };
    let existing = record
        .members
        .iter()
        .map(|member| member.name.to_ascii_lowercase())
        .collect::<std::collections::HashSet<_>>();
    if !existing.contains(&base_name.to_ascii_lowercase()) {
        return base_name.to_string();
    }
    let mut suffix = 2;
    loop {
        let candidate = format!("{base_name}-{suffix}");
        if !existing.contains(&candidate.to_ascii_lowercase()) {
            return candidate;
        }
        suffix += 1;
    }
}

/// Maps to: CC `teamHelpers.ts:235-251` `addHiddenPaneId` —
/// `readTeamFile` (`:236`) → modify → `writeTeamFile` (`:245`).
pub(crate) fn add_hidden_pane_id(team_name: &str, pane_id: &str) -> bool {
    let Some(mut record) = read_team_file(team_name) else {
        return false;
    };
    let hidden = record.hidden_pane_ids.get_or_insert_with(Vec::new);
    if !hidden.iter().any(|value| value == pane_id) {
        hidden.push(pane_id.to_string());
        write_team_record(record);
    }
    true
}

/// Maps to: CC `teamHelpers.ts:259-276` `removeHiddenPaneId` —
/// `readTeamFile` (`:260`) → modify → `writeTeamFile` (`:270`).
pub(crate) fn remove_hidden_pane_id(team_name: &str, pane_id: &str) -> bool {
    let Some(mut record) = read_team_file(team_name) else {
        return false;
    };
    if let Some(hidden) = &mut record.hidden_pane_ids {
        let original_len = hidden.len();
        hidden.retain(|value| value != pane_id);
        if hidden.len() != original_len {
            write_team_record(record);
        }
    }
    true
}

/// Maps to: CC `teamHelpers.ts:285-317` `removeMemberFromTeam` —
/// `readTeamFile` (`:289`) → modify → `writeTeamFile` (`:312`).
pub(crate) fn remove_member_from_team(team_name: &str, tmux_pane_id: &str) -> bool {
    let Some(mut record) = read_team_file(team_name) else {
        return false;
    };
    let original_len = record.members.len();
    record
        .members
        .retain(|member| member.tmux_pane_id != tmux_pane_id);
    if record.members.len() == original_len {
        return false;
    }
    if let Some(hidden) = &mut record.hidden_pane_ids {
        hidden.retain(|value| value != tmux_pane_id);
    }
    write_team_record(record);
    true
}

/// Maps to: CC `teamHelpers.ts:326-348` `removeMemberByAgentId` —
/// `readTeamFile` (`:330`) → modify → `writeTeamFile` (`:343`).
pub(crate) fn remove_member_by_agent_id(team_name: &str, agent_id: &str) -> bool {
    let Some(mut record) = read_team_file(team_name) else {
        return false;
    };
    let original_len = record.members.len();
    record.members.retain(|member| member.agent_id != agent_id);
    if record.members.len() == original_len {
        return false;
    }
    write_team_record(record);
    true
}

/// Maps to: CC `teamHelpers.ts:357-389` `setMemberMode` —
/// `readTeamFile` (`:362`) → modify → `writeTeamFile` (`:384`).
pub(crate) fn set_member_mode(team_name: &str, member_name: &str, mode: &str) -> bool {
    let Some(mut record) = read_team_file(team_name) else {
        return false;
    };
    let Some(member) = record
        .members
        .iter_mut()
        .find(|member| member.name == member_name)
    else {
        return false;
    };
    if member.mode.as_deref() == Some(mode) {
        return true;
    }
    member.mode = Some(mode.to_string());
    write_team_record(record);
    true
}

/// Maps to: CC `teamHelpers.ts:415-445` `setMultipleMemberModes` —
/// `readTeamFile` (`:419`) → modify → `writeTeamFile` (`:439`). CC's comment
/// calls this "a single atomic operation" that "avoids race conditions when
/// updating multiple teammates at once"; that only holds if the read is the
/// disk read.
pub(crate) fn set_multiple_member_modes(team_name: &str, updates: &[(&str, &str)]) -> bool {
    let Some(mut record) = read_team_file(team_name) else {
        return false;
    };
    let update_map = updates
        .iter()
        .copied()
        .collect::<std::collections::HashMap<_, _>>();
    let mut changed = false;
    for member in &mut record.members {
        if let Some(mode) = update_map.get(member.name.as_str()) {
            if member.mode.as_deref() != Some(*mode) {
                member.mode = Some((*mode).to_string());
                changed = true;
            }
        }
    }
    if changed {
        write_team_record(record);
    }
    true
}

/// Maps to: CC `teamHelpers.ts:454-485` `setMemberActive` —
/// `readTeamFileAsync` (`:459`) → modify → `writeTeamFileAsync` (`:481`).
/// `isActive` is the field teammate processes flip on disk, so the read has to
/// be the disk read: this is the #92 TeamDelete clobber with a wider blast
/// radius. (CC's version is `async` only because it sits on the fs/promises
/// pair — the read-modify-write is still un-serialized there, so the sync Rust
/// shape is equivalent, not weaker.)
pub(crate) fn set_member_active(team_name: &str, member_name: &str, is_active: bool) -> bool {
    let Some(mut record) = read_team_file(team_name) else {
        return false;
    };
    let Some(member) = record
        .members
        .iter_mut()
        .find(|member| member.name == member_name)
    else {
        return false;
    };
    if member.is_active == Some(is_active) {
        return true;
    }
    member.is_active = Some(is_active);
    write_team_record(record);
    true
}

/// Maps to: CC `teamHelpers.ts#destroyWorktree`.
fn destroy_worktree(worktree_path: &str) {
    let path = Path::new(worktree_path);
    if !path.exists() {
        return;
    }

    let main_repo_path = std::fs::read_to_string(path.join(".git"))
        .ok()
        .and_then(|content| {
            let trimmed = content.trim();
            trimmed
                .strip_prefix("gitdir:")
                .map(str::trim)
                .filter(|gitdir| !gitdir.is_empty())
                .map(PathBuf::from)
        })
        .and_then(|worktree_git_dir| {
            worktree_git_dir
                .parent()
                .and_then(Path::parent)
                .and_then(Path::parent)
                .map(Path::to_path_buf)
        });

    if let Some(main_repo_path) = main_repo_path {
        let mut command = std::process::Command::new(crate::utils::git::git_exe());
        // CC `execFileNoThrowWithCwd` inherits process.env (execa default); the carrier is its counterpart.
        crate::utils::subprocess_env::apply_process_env_std(&mut command);
        if command
            .args(["worktree", "remove", "--force", worktree_path])
            .current_dir(&main_repo_path)
            .output()
            .ok()
            .is_some_and(|output| output.status.success())
        {
            return;
        }
    }

    let _ = std::fs::remove_dir_all(path);
}

/// Maps to: CC `teamHelpers.ts#registerTeamForSessionCleanup`.
pub(crate) fn register_team_for_session_cleanup(team_name: &str) {
    crate::bootstrap::state::add_session_created_team(team_name.to_string());
}

/// Maps to: CC `teamHelpers.ts#unregisterTeamForSessionCleanup`.
pub(crate) fn unregister_team_for_session_cleanup(team_name: &str) {
    crate::bootstrap::state::remove_session_created_team(team_name);
}

/// Maps to: CC `teamHelpers.ts:598-634` `killOrphanedTeammatePanes` —
/// `readTeamFile` (`:599`). Runs at ungraceful leader exit, where the leader's
/// snapshot is exactly the copy most likely to be stale.
async fn kill_orphaned_teammate_panes(team_name: &str) {
    let pane_members = read_team_file(team_name)
        .map(|record| record.members)
        .unwrap_or_default()
        .into_iter()
        .filter(|member| {
            member.name != crate::utils::swarm::constants::TEAM_LEAD_NAME
                && !member.tmux_pane_id.is_empty()
                && member
                    .backend_type
                    .as_deref()
                    .is_some_and(|backend| matches!(backend, "tmux" | "iterm2"))
        })
        .collect::<Vec<_>>();

    if pane_members.is_empty() {
        return;
    }

    let use_external_session = !crate::utils::swarm::backends::detection::is_inside_tmux_sync();
    for member in pane_members {
        match member.backend_type.as_deref() {
            Some("tmux") => {
                if let Ok(executor) =
                    crate::utils::swarm::backends::registry::get_pane_backend_executor_for_type(
                        crate::utils::swarm::backends::types::PaneBackendType::Tmux,
                    )
                {
                    if executor.kill(&member.agent_id).await {
                        continue;
                    }
                }
                let backend = crate::utils::swarm::backends::tmux_backend::TmuxBackend::new();
                let _ = backend
                    .kill_pane(&member.tmux_pane_id, use_external_session)
                    .await;
            }
            Some("iterm2") => {
                if let Ok(executor) =
                    crate::utils::swarm::backends::registry::get_pane_backend_executor_for_type(
                        crate::utils::swarm::backends::types::PaneBackendType::ITerm2,
                    )
                {
                    if executor.kill(&member.agent_id).await {
                        continue;
                    }
                }
                let backend = crate::utils::swarm::backends::iterm_backend::ITermBackend::new();
                let _ = backend
                    .kill_pane(&member.tmux_pane_id, use_external_session)
                    .await;
            }
            _ => {}
        }
    }
}

/// Maps to: CC `teamHelpers.ts#cleanupSessionTeams`.
pub(crate) async fn cleanup_session_teams() -> Vec<String> {
    let teams = crate::bootstrap::state::get_session_created_teams()
        .into_iter()
        .collect::<Vec<_>>();
    if teams.is_empty() {
        return teams;
    }
    for team_name in &teams {
        kill_orphaned_teammate_panes(team_name).await;
    }
    for team_name in &teams {
        let _ = cleanup_team_directories(team_name);
    }
    crate::bootstrap::state::clear_session_created_teams();
    teams
}

/// Maps to: CC `teamHelpers.ts:641-683` `cleanupTeamDirectories` team/task/
/// worktree removal — `readTeamFile` (`:645`) "BEFORE deleting the team
/// directory". Worktree paths land on disk when a teammate is spawned, so a
/// stale leader snapshot would leak the worktrees it never saw.
pub(crate) fn cleanup_team_directories(team_name: &str) -> Result<(), String> {
    let worktree_paths = read_team_file(team_name)
        .map(|record| {
            record
                .members
                .into_iter()
                .filter_map(|member| member.worktree_path)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for worktree_path in worktree_paths {
        destroy_worktree(&worktree_path);
    }

    let team_dir = get_team_dir(team_name);
    if team_dir.exists() {
        std::fs::remove_dir_all(&team_dir).map_err(|err| {
            format!(
                "Failed to clean up team directory {}: {err}",
                team_dir.display()
            )
        })?;
    }
    let task_dir = crate::utils::tasks::get_tasks_dir(&sanitize_name(team_name));
    if task_dir.exists() {
        std::fs::remove_dir_all(&task_dir).map_err(|err| {
            format!(
                "Failed to clean up tasks directory {}: {err}",
                task_dir.display()
            )
        })?;
    }
    Ok(())
}

/// Maps to: CC AppState `teamContext.teamName` lookup in `resolveTeamName`.
pub(crate) fn current_team_name() -> Option<String> {
    TEAM_TOOL_STATE
        .lock()
        .unwrap()
        .as_ref()
        .map(|record| record.team_name.clone())
}

#[cfg(test)]
pub(crate) static TEST_TEAM_HELPERS_LOCK: std::sync::LazyLock<TestStateLock> =
    std::sync::LazyLock::new(TestStateLock::new);

#[cfg(test)]
pub(crate) fn clear_team_tool_state_for_test() {
    *TEAM_TOOL_STATE.lock().unwrap() = None;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    fn unique_config_dir(prefix: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "cometix-{prefix}-{}",
            uuid::Uuid::new_v4().simple()
        ))
    }

    /// Every mutator in this file reads through `read_team_file` (CC
    /// `readTeamFile`, `teamHelpers.ts:131-142`), so a memory-only
    /// `write_team_record` seed is a no-op fixture: `maybe_skip_disk_io_in_tests`
    /// suppresses the write and the mutator then reads nothing. Pin a scratch
    /// `CLAUDE_CONFIG_DIR` and re-enable team-file IO; the returned guards must
    /// stay alive for the whole test body.
    fn team_disk_guards(prefix: &str) -> (PathBuf, EnvVarGuard, EnvVarGuard) {
        let root = unique_config_dir(prefix);
        let config_guard = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root);
        let io_guard = EnvVarGuard::set("COMETIX_TEST_TEAM_FILE_IO", "1");
        super::clear_team_tool_state_for_test();
        (root, config_guard, io_guard)
    }

    #[test]
    fn sanitize_name_and_agent_name_follow_official_constraints() {
        let _lock = TEST_TEAM_HELPERS_LOCK.lock().unwrap();
        assert_eq!(super::sanitize_name("My Team!"), "my-team-");
        assert_eq!(super::sanitize_agent_name("lead@west"), "lead-west");
    }

    #[test]
    fn unique_teammate_name_checks_existing_members_case_insensitively() {
        let _lock = TEST_TEAM_HELPERS_LOCK.lock().unwrap();
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let (root, _config_guard, _io_guard) = team_disk_guards("unique-name");
        let mut record = super::create_team_record(
            "alpha".to_string(),
            None,
            Some("team-lead".to_string()),
            None,
            "/tmp".to_string(),
        );
        record.members.push(super::TeamMemberRecord {
            agent_id: "tester@alpha".to_string(),
            name: "Tester".to_string(),
            agent_type: None,
            model: None,
            prompt: None,
            color: None,
            plan_mode_required: None,
            joined_at_ms: 1,
            tmux_pane_id: "in-process".to_string(),
            cwd: "/tmp".to_string(),
            worktree_path: None,
            session_id: None,
            subscriptions: Vec::new(),
            backend_type: Some("in-process".to_string()),
            is_active: None,
            mode: None,
        });
        record.members.push(super::TeamMemberRecord {
            agent_id: "tester-2@alpha".to_string(),
            name: "tester-2".to_string(),
            agent_type: None,
            model: None,
            prompt: None,
            color: None,
            plan_mode_required: None,
            joined_at_ms: 1,
            tmux_pane_id: "in-process".to_string(),
            cwd: "/tmp".to_string(),
            worktree_path: None,
            session_id: None,
            subscriptions: Vec::new(),
            backend_type: Some("in-process".to_string()),
            is_active: None,
            mode: None,
        });
        super::write_team_record(record);

        assert_eq!(
            super::generate_unique_teammate_name("tester", Some("alpha")),
            "tester-3"
        );
        assert_eq!(
            super::generate_unique_teammate_name("reviewer", Some("alpha")),
            "reviewer"
        );

        super::clear_team_tool_state_for_test();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn team_file_persistence_uses_official_config_json_shape() {
        let _lock = TEST_TEAM_HELPERS_LOCK.lock().unwrap();
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let root = unique_config_dir("team-file");
        let _config_guard = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root);
        let _io_guard = EnvVarGuard::set("COMETIX_TEST_TEAM_FILE_IO", "1");
        super::clear_team_tool_state_for_test();

        let mut record = super::create_team_record(
            "Review Team".to_string(),
            Some("Review code".to_string()),
            Some("team-lead".to_string()),
            Some("claude-opus".to_string()),
            "/repo".to_string(),
        );
        record.members.push(super::TeamMemberRecord {
            agent_id: "tester@Review Team".to_string(),
            name: "tester".to_string(),
            agent_type: Some("general-purpose".to_string()),
            model: Some("claude-sonnet".to_string()),
            prompt: Some("test it".to_string()),
            color: Some("green".to_string()),
            plan_mode_required: Some(true),
            joined_at_ms: 2,
            tmux_pane_id: "%2".to_string(),
            cwd: "/repo".to_string(),
            worktree_path: None,
            session_id: Some("session-2".to_string()),
            subscriptions: vec!["*".to_string()],
            backend_type: Some("tmux".to_string()),
            is_active: Some(true),
            mode: Some("default".to_string()),
        });
        super::write_team_record_result(record).expect("write team file");
        super::clear_team_tool_state_for_test();

        let path = root.join("teams").join("review-team").join("config.json");
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value["name"], "Review Team");
        assert_eq!(value["leadAgentId"], "team-lead@Review Team");
        assert!(value.get("team_file_path").is_none());
        assert_eq!(value["members"][1]["tmuxPaneId"], "%2");
        assert_eq!(value["members"][1]["planModeRequired"], true);

        let read = super::memory_first_team_record("Review Team").expect("read team file");
        assert_eq!(read.members.len(), 2);
        assert_eq!(read.members[1].backend_type.as_deref(), Some("tmux"));
        assert_eq!(read.team_file_path, path.display().to_string());

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn hidden_member_mode_and_active_mutations_persist_to_record() {
        let _lock = TEST_TEAM_HELPERS_LOCK.lock().unwrap();
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let (root, _config_guard, _io_guard) = team_disk_guards("mutations");
        let mut record = super::create_team_record(
            "alpha".to_string(),
            None,
            Some("team-lead".to_string()),
            None,
            "/tmp".to_string(),
        );
        record.members.push(super::TeamMemberRecord {
            agent_id: "reviewer@alpha".to_string(),
            name: "reviewer".to_string(),
            agent_type: None,
            model: None,
            prompt: None,
            color: None,
            plan_mode_required: None,
            joined_at_ms: 1,
            tmux_pane_id: "%2".to_string(),
            cwd: "/tmp".to_string(),
            worktree_path: None,
            session_id: None,
            subscriptions: Vec::new(),
            backend_type: Some("tmux".to_string()),
            is_active: None,
            mode: None,
        });
        super::write_team_record(record);

        assert!(super::add_hidden_pane_id("alpha", "%2"));
        assert!(super::set_member_mode("alpha", "reviewer", "acceptEdits"));
        assert!(super::set_member_active("alpha", "reviewer", false));
        let record = super::read_team_file("alpha").unwrap();
        assert_eq!(record.hidden_pane_ids, Some(vec!["%2".to_string()]));
        assert_eq!(record.members[1].mode.as_deref(), Some("acceptEdits"));
        assert_eq!(record.members[1].is_active, Some(false));

        assert!(super::remove_member_from_team("alpha", "%2"));
        let record = super::read_team_file("alpha").unwrap();
        assert_eq!(record.members.len(), 1);
        assert!(
            record
                .hidden_pane_ids
                .as_ref()
                .is_none_or(|hidden| hidden.is_empty())
        );

        super::clear_team_tool_state_for_test();
        let _ = std::fs::remove_dir_all(root);
    }

    /// Regression for the `memory_first_team_record` clobber (#104 K6, same
    /// family as the #92 TeamDelete `isActive` bug): every mutator here is a
    /// read-modify-WRITE of the *whole* record, so reading the leader's
    /// in-process snapshot instead of the file silently reverts everything a
    /// teammate process wrote since that snapshot was taken.
    ///
    /// CC has no such snapshot to read — `setMemberActive` is
    /// `readTeamFileAsync` (`teamHelpers.ts:459`) → modify →
    /// `writeTeamFileAsync` (`:481`), and `teamHelpers.ts` contains zero
    /// `appState`/`teamContext` reads.
    #[test]
    fn mutators_preserve_on_disk_changes_made_after_the_in_memory_snapshot() {
        let _lock = TEST_TEAM_HELPERS_LOCK.lock().unwrap();
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let (root, _config_guard, _io_guard) = team_disk_guards("clobber");

        // Leader writes the team file and caches it in TEAM_TOOL_STATE.
        let mut record = super::create_team_record(
            "alpha".to_string(),
            None,
            Some("team-lead".to_string()),
            None,
            "/tmp".to_string(),
        );
        record.members.push(member("reviewer", "reviewer@alpha"));
        super::write_team_record_result(record).expect("seed team file");
        assert!(super::current_team_record().is_some(), "snapshot is primed");

        // A teammate process mutates the file directly — no TEAM_TOOL_STATE
        // update, because it is a different process. Written through the file
        // rather than through `write_team_record` on purpose: routing it
        // through the leader's writer would refresh the very snapshot this
        // test needs to be stale.
        let mut on_disk = super::read_team_file("alpha").expect("seeded file");
        on_disk.members.push(member("latecomer", "latecomer@alpha"));
        on_disk.members[1].mode = Some("plan".to_string());
        std::fs::write(
            super::get_team_file_path("alpha"),
            serde_json::to_string_pretty(&on_disk).unwrap(),
        )
        .expect("teammate write");

        // The leader's snapshot is now two changes behind.
        let snapshot = super::current_team_record().expect("stale snapshot");
        assert_eq!(snapshot.members.len(), 2, "snapshot predates the teammate");

        assert!(super::set_member_active("alpha", "reviewer", false));

        let after = super::read_team_file("alpha").expect("team file after mutation");
        assert_eq!(after.members[1].is_active, Some(false), "own edit applied");
        assert_eq!(
            after.members.len(),
            3,
            "teammate's added member must survive the mutator's write-back"
        );
        assert_eq!(after.members[2].name, "latecomer");
        assert_eq!(
            after.members[1].mode.as_deref(),
            Some("plan"),
            "teammate's mode edit must survive the mutator's write-back"
        );

        super::clear_team_tool_state_for_test();
        let _ = std::fs::remove_dir_all(root);
    }

    fn member(name: &str, agent_id: &str) -> super::TeamMemberRecord {
        super::TeamMemberRecord {
            agent_id: agent_id.to_string(),
            name: name.to_string(),
            agent_type: None,
            model: None,
            prompt: None,
            color: None,
            plan_mode_required: None,
            joined_at_ms: 1,
            tmux_pane_id: "in-process".to_string(),
            cwd: "/tmp".to_string(),
            worktree_path: None,
            session_id: None,
            subscriptions: Vec::new(),
            backend_type: Some("in-process".to_string()),
            is_active: None,
            mode: None,
        }
    }

    #[test]
    fn cleanup_team_directories_removes_team_tasks_and_member_worktrees() {
        let _lock = TEST_TEAM_HELPERS_LOCK.lock().unwrap();
        let _task_lock = crate::utils::tasks::TASK_TOOL_TEST_LOCK.lock().unwrap();
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let root = unique_config_dir("cleanup");
        let _config_guard = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root);
        let _io_guard = EnvVarGuard::set("COMETIX_TEST_TEAM_FILE_IO", "1");
        super::clear_team_tool_state_for_test();

        let worktree_dir = root.join("agent-worktree");
        std::fs::create_dir_all(&worktree_dir).unwrap();
        std::fs::write(worktree_dir.join("file.txt"), "work").unwrap();

        let mut record = super::create_team_record(
            "Team Work".to_string(),
            None,
            Some("team-lead".to_string()),
            None,
            "/tmp".to_string(),
        );
        record.members.push(super::TeamMemberRecord {
            agent_id: "worker@Team Work".to_string(),
            name: "worker".to_string(),
            agent_type: None,
            model: None,
            prompt: None,
            color: None,
            plan_mode_required: None,
            joined_at_ms: 1,
            tmux_pane_id: "%2".to_string(),
            cwd: "/tmp".to_string(),
            worktree_path: Some(worktree_dir.display().to_string()),
            session_id: None,
            subscriptions: Vec::new(),
            backend_type: Some("tmux".to_string()),
            is_active: Some(false),
            mode: None,
        });
        super::write_team_record_result(record).unwrap();
        let task_dir = crate::utils::tasks::get_tasks_dir("team-work");
        std::fs::create_dir_all(&task_dir).unwrap();
        std::fs::write(task_dir.join("1.json"), "{}").unwrap();

        super::cleanup_team_directories("Team Work").unwrap();

        assert!(!root.join("teams").join("team-work").exists());
        assert!(!task_dir.exists());
        assert!(!worktree_dir.exists());
        super::clear_team_tool_state_for_test();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn session_team_cleanup_tracks_registered_teams_like_official_state() {
        let _lock = TEST_TEAM_HELPERS_LOCK.lock().unwrap();
        let _task_lock = crate::utils::tasks::TASK_TOOL_TEST_LOCK.lock().unwrap();
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let root = unique_config_dir("session-cleanup");
        let _config_guard = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root);
        let _io_guard = EnvVarGuard::set("COMETIX_TEST_TEAM_FILE_IO", "1");
        super::clear_team_tool_state_for_test();
        crate::bootstrap::state::clear_session_created_teams();

        let record = super::create_team_record(
            "Alpha Team".to_string(),
            None,
            Some("team-lead".to_string()),
            None,
            "/tmp".to_string(),
        );
        super::write_team_record_result(record).unwrap();
        super::register_team_for_session_cleanup("Alpha Team");
        assert!(crate::bootstrap::state::get_session_created_teams().contains("Alpha Team"));

        let cleaned = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(super::cleanup_session_teams());

        assert_eq!(cleaned, vec!["Alpha Team".to_string()]);
        assert!(crate::bootstrap::state::get_session_created_teams().is_empty());
        assert!(!root.join("teams").join("alpha-team").exists());
        super::clear_team_tool_state_for_test();
        let _ = std::fs::remove_dir_all(root);
    }
}
