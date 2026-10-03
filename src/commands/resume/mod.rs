//! Rust module root for CC `commands/resume/`.
//!
//! `resume.rs::ResumeCommand` owns the corresponding command UI and
//! `filterResumableSessions`. This parent contains the typed Rust adapters that
//! resolve a `SessionSelection` and project persisted records into recovery;
//! filesystem enumeration itself remains owned by `utils/session_storage.rs`.
//! CLI resume is processed by `utils::session_restore` before REPL mount, while
//! in-session resume is applied by `screens/repl.rs`. No launch props use
//! Context and this module never writes session data.

pub mod resume;

use self::resume::filter_resumable_sessions;
use crate::types::command::ResumeEntrypoint;
use crate::utils::conversation;
use crate::utils::get_worktree_paths::get_worktree_paths;
use crate::utils::query_helpers;
use crate::utils::session_restore;
use crate::utils::session_storage::{self, SessionSelection, SessionSummary};
use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Clone, Debug)]
pub struct ResumeTarget {
    pub session_id: String,
    pub project_path: Option<String>,
    pub entries: Vec<serde_json::Value>,
    pub turn_interruption_state: conversation::TurnInterruptionState,
    pub metadata: ResumeMetadata,
    pub entrypoint: Option<ResumeEntrypoint>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ResumeMetadata {
    /// Native carrier for the original LogOption.messages kept alongside
    /// deserializeMessages in CC conversationRecovery.ts:540–556 and REPL.tsx:2359.
    /// Plan recovery must see interrupted tool_use blocks before filtering.
    pub original_messages: Option<Vec<serde_json::Value>>,
    pub session_id: Option<String>,
    pub file_history_snapshots: Vec<serde_json::Value>,
    pub attribution_snapshots: Vec<serde_json::Value>,
    pub content_replacements: Vec<serde_json::Value>,
    pub context_collapse_commits: Vec<serde_json::Value>,
    pub context_collapse_snapshot: Option<serde_json::Value>,
    pub skill_restore: conversation::SkillRestoreState,
    pub read_file_state: Vec<query_helpers::ReadFileStateEntry>,
    pub bash_tools: Vec<String>,
    pub todos: Vec<serde_json::Value>,
    pub agent_name: Option<String>,
    pub agent_color: Option<String>,
    pub agent_setting: Option<String>,
    pub custom_title: Option<String>,
    pub tag: Option<String>,
    pub mode: Option<String>,
    pub worktree_session: Option<serde_json::Value>,
    pub pr_number: Option<u64>,
    pub pr_url: Option<String>,
    pub pr_repository: Option<String>,
    pub full_path: Option<String>,
}

#[derive(Clone, Debug)]
struct ResumeLoad {
    entries: Vec<serde_json::Value>,
    turn_interruption_state: conversation::TurnInterruptionState,
    metadata: ResumeMetadata,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResumeError {
    NoQuery,
    NoConversations,
    SessionNotFound { arg: String },
    MultipleMatches { arg: String, count: usize },
    EmptySession { session_id: String },
}

impl fmt::Display for ResumeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoQuery => write!(f, "No conversation id or search term provided"),
            Self::NoConversations => write!(f, "No conversations found to resume."),
            Self::SessionNotFound { arg } => write!(f, "Session {arg} was not found."),
            Self::MultipleMatches { arg, count } => write!(
                f,
                "Found {count} sessions matching {arg}. Please use /resume to pick a specific session."
            ),
            Self::EmptySession { session_id } => {
                write!(f, "Session {session_id} has no transcript messages.")
            }
        }
    }
}

#[cfg(test)]
/// Maps to: CC `getOriginalCwd()` — the one cwd authority for the resume
/// chain (`main.tsx:5115` feeds it to `getWorktreePaths`, and
/// `getStatOnlyLogsForWorktrees` anchors the project dir on it). The previous
/// `std::env::current_dir()` read diverged from the session-storage anchor
/// whenever the launch cwd and the pinned original cwd differ.
pub fn current_project_path() -> String {
    crate::bootstrap::state::get_original_cwd()
        .display()
        .to_string()
}

pub fn load_for_arg(project_path: &str, arg: &str) -> Result<ResumeTarget, ResumeError> {
    load_resolved_arg(resolve_arg(project_path, arg)?)
}

/// Rust carrier adaptation for the already-resolved LogOption versus direct
/// getLastSessionLog source in CC commands/resume/resume.tsx:265-294 and
/// utils/conversationRecovery.ts:520-523. The provenance must survive resolution:
/// a matching lite log and the UUID fallback have the same analytics entrypoint
/// but intentionally select different transcript leaves.
fn load_resolved_arg(resolved: ResolvedResume) -> Result<ResumeTarget, ResumeError> {
    let mut project_path = resolved.selection.project_path.clone();
    let loaded = if !matches!(resolved.source, ResumeLogSource::LogOption) {
        let selection = &resolved.selection;
        // CC getLastSessionLog:3918 deliberately projects fullPath through
        // getTranscriptPathForSession, although loadSessionFile reads using the
        // active project dir. These two source contracts are not interchangeable.
        let session_file = session_storage::get_transcript_path_for_session(&selection.session_id);
        let (session, entries) = match resolved.source {
            ResumeLogSource::DirectLog { session, entries } => (session, entries),
            ResumeLogSource::SessionId => {
                session_storage::get_last_session_log(&selection.session_id).ok_or_else(|| {
                    ResumeError::EmptySession {
                        session_id: selection.session_id.clone(),
                    }
                })?
            }
            ResumeLogSource::LogOption => unreachable!(),
        };
        let cwd = selection.project_path.clone().unwrap_or_else(|| {
            crate::bootstrap::state::get_original_cwd()
                .display()
                .to_string()
        });
        // getLastSessionLog spreads convertToLogOption, not loadFullLog's
        // richer indexed metadata (sessionStorage.ts:2479-2524, 3906-3930).
        // Capture fields before deserialization and do not replace absence
        // with the index selection or standalone metadata records.
        let first = entries.first();
        let agent_name = first
            .and_then(|entry| entry.get("agentName"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        project_path = first
            .and_then(|entry| entry.get("cwd"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let mut loaded = deserialize_session_entries(
            &session,
            entries,
            &session_file,
            &selection.session_id,
            &cwd,
        )?;
        loaded.metadata.agent_name = agent_name;
        loaded.metadata.agent_color = None;
        loaded.metadata.mode = None;
        loaded.metadata.pr_number = None;
        loaded.metadata.pr_url = None;
        loaded.metadata.pr_repository = None;
        // getLastSessionLog keys these maps by the requested ID; only title
        // and tag above come from the final transcript message's sessionId.
        let session_id = &selection.session_id;
        loaded.metadata.session_id = Some(session_id.clone());
        loaded.metadata.agent_setting = session.agent_settings.get(session_id).cloned();
        loaded.metadata.content_replacements = session
            .content_replacements
            .get(session_id)
            .cloned()
            .unwrap_or_default();
        loaded.metadata.worktree_session = session.worktree_states.get(session_id).cloned();
        loaded.metadata.context_collapse_commits = session
            .context_collapse_commits
            .iter()
            .filter(|entry| entry_session_id(entry) == Some(session_id.as_str()))
            .cloned()
            .collect();
        loaded.metadata.context_collapse_snapshot = session
            .context_collapse_snapshot
            .as_ref()
            .filter(|entry| entry_session_id(entry) == Some(session_id.as_str()))
            .cloned();
        loaded
    } else {
        load_session_entries_for_selection(&resolved.selection)?
    };
    Ok(ResumeTarget {
        session_id: resolved.selection.session_id,
        project_path,
        entries: loaded.entries,
        turn_interruption_state: loaded.turn_interruption_state,
        metadata: loaded.metadata,
        entrypoint: Some(resolved.entrypoint),
    })
}

/// CLI interactive launch intent for `-c` / `-r`.
/// Consumed directly by main; never injected through Context.
#[derive(Clone, Debug)]
pub enum CliSessionLaunch {
    None,
    /// Bare `--resume` (or `--from-pr` assembled by main) → startup picker.
    OpenPicker {
        initial_search_query: Option<String>,
    },
    /// `-c` → resolve and restore the latest conversation after setup.
    Continue,
    /// `--resume <uuid|custom-title>` → resolve after setup.
    Resume {
        value: String,
    },
}

/// Resolve continue/resume argv into the IO-free launch intent consumed by
/// `main.rs::Main` after `showSetupScreens` completes.
///
/// Maps to: CC `main.tsx` continue (~4238) / resume (~4570). Session discovery
/// deliberately does not run here: official CC performs it after setup.
pub fn resolve_cli_session_launch(
    continue_session: bool,
    resume: Option<&str>,
) -> CliSessionLaunch {
    if continue_session {
        return CliSessionLaunch::Continue;
    }
    match resume {
        None => CliSessionLaunch::None,
        Some(arg) if arg.trim().is_empty() => CliSessionLaunch::OpenPicker {
            initial_search_query: None,
        },
        Some(arg) => CliSessionLaunch::Resume {
            value: arg.trim().to_string(),
        },
    }
}

pub fn load_for_picker(project_path: &str, session_id: &str) -> Result<ResumeTarget, ResumeError> {
    let selection = SessionSelection {
        session_id: session_id.to_string(),
        project_path: Some(project_path.to_string()),
        file_path: session_storage::get_session_file_path(project_path, session_id),
    };
    load_for_picker_selection(&selection)
}

/// Maps to: CC `-c/--continue` → latest same-repo resumable session.
pub fn load_for_continue(project_path: &str) -> Result<ResumeTarget, ResumeError> {
    let worktree_paths = get_worktree_paths(project_path);
    let sessions = filter_resumable_sessions(
        session_storage::list_same_repo_sessions(project_path, &worktree_paths),
        None,
    );
    let Some(latest) = sessions.first() else {
        return Err(ResumeError::NoConversations);
    };
    let mut target = load_for_picker_selection(&SessionSelection::from(latest))?;
    // CC's continue branch does not use `ResumeEntrypoint`; that union is
    // reserved for resume/fork analytics.
    target.entrypoint = None;
    Ok(target)
}

/// Maps to: CC `main.tsx` direct `--resume <uuid|custom-title>` resolution.
pub fn load_for_cli_resume(project_path: &str, arg: &str) -> Result<ResumeTarget, ResumeError> {
    // `searchSessionsByCustomTitle` is same-repository/worktree aware, exactly
    // like `resolve_arg`; non-unique/no-match values are sent to the chooser by
    // `resolve_cli_session_launch` rather than treated as direct substring hits.
    // CLI UUID input reaches getLastSessionLog without the slash command's
    // prerequisite of a nonempty enriched list (conversationRecovery.ts:520).
    let mut resolved = if Uuid::parse_str(arg.trim()).is_ok() {
        ResolvedResume {
            selection: SessionSelection {
                session_id: arg.trim().to_string(),
                project_path: Some(project_path.to_string()),
                file_path: session_storage::load_session_file_path(arg.trim()),
            },
            entrypoint: ResumeEntrypoint::SlashCommandSessionId,
            source: ResumeLogSource::SessionId,
        }
    } else {
        resolve_arg(project_path, arg)?
    };
    // CC utils/conversationRecovery.ts:520-523 receives a string for CLI UUID
    // resumes and calls getLastSessionLog even when the picker index has a hit.
    if resolved.entrypoint == ResumeEntrypoint::SlashCommandSessionId {
        resolved.source = ResumeLogSource::SessionId;
    }
    let mut target = load_resolved_arg(resolved)?;
    target.entrypoint = Some(ResumeEntrypoint::CliFlag);
    Ok(target)
}

pub fn load_for_picker_selection(
    selection: &SessionSelection,
) -> Result<ResumeTarget, ResumeError> {
    let loaded = load_session_entries_for_selection(selection)?;
    Ok(ResumeTarget {
        session_id: selection.session_id.clone(),
        project_path: selection.project_path.clone(),
        entries: loaded.entries,
        turn_interruption_state: loaded.turn_interruption_state,
        metadata: loaded.metadata,
        entrypoint: Some(ResumeEntrypoint::SlashCommandPicker),
    })
}

#[derive(Clone, Debug)]
struct ResolvedResume {
    selection: SessionSelection,
    entrypoint: ResumeEntrypoint,
    source: ResumeLogSource,
}

/// Necessary Rust carrier for CC's string session ID versus LogOption source
/// in utils/conversationRecovery.ts#loadConversationForResume:520-527.
#[derive(Clone, Debug)]
enum ResumeLogSource {
    SessionId,
    /// The slash fallback already awaited getLastSessionLog; transport that
    /// result instead of rereading the file or confusing existence with a log.
    DirectLog {
        session: session_storage::LoadedSession,
        entries: Vec<serde_json::Value>,
    },
    LogOption,
}

fn resolve_arg(project_path: &str, arg: &str) -> Result<ResolvedResume, ResumeError> {
    let query = arg.trim();
    if query.is_empty() {
        return Err(ResumeError::NoQuery);
    }

    let worktree_paths = get_worktree_paths(project_path);
    let sessions = filter_resumable_sessions(
        session_storage::list_same_repo_sessions(project_path, &worktree_paths),
        None,
    );
    // CC commands/resume/resume.tsx:252-262 checks the loaded list before
    // UUID/direct lookup. CLI UUID resume intentionally bypasses this branch.
    if sessions.is_empty() {
        return Err(ResumeError::NoConversations);
    }
    // Official first checks a valid UUID against loaded logs, then falls back
    // to a direct file lookup for logs filtered out during enrichment.
    if Uuid::parse_str(query).is_ok() {
        if let Some(session) = latest_session_match(
            sessions
                .iter()
                .filter(|session| session.session_id == query)
                .collect(),
        ) {
            return Ok(ResolvedResume {
                selection: SessionSelection::from(session),
                entrypoint: ResumeEntrypoint::SlashCommandSessionId,
                source: ResumeLogSource::LogOption,
            });
        }

        let exact_path = session_storage::load_session_file_path(query);
        if let Some((session, entries)) = session_storage::get_last_session_log(query) {
            return Ok(ResolvedResume {
                selection: SessionSelection {
                    session_id: query.to_string(),
                    project_path: Some(project_path.to_string()),
                    file_path: exact_path,
                },
                entrypoint: ResumeEntrypoint::SlashCommandSessionId,
                source: ResumeLogSource::DirectLog { session, entries },
            });
        }
    }

    // Official next tries exact custom title matches when the feature is on.
    // The Rust port also accepts the display title because that is what the
    // current LogSelector-equivalent already exposes.
    let exact_title_matches = unique_deduped_session_match(
        sessions
            .iter()
            .filter(|session| matches_exact_resume_title(session, query))
            .collect(),
        query,
    )?;
    if let Some(session) = exact_title_matches {
        return Ok(ResolvedResume {
            selection: SessionSelection::from(session),
            entrypoint: ResumeEntrypoint::SlashCommandTitle,
            source: ResumeLogSource::LogOption,
        });
    }

    Err(ResumeError::SessionNotFound {
        arg: query.to_string(),
    })
}

fn latest_session_match<'a>(candidates: Vec<&'a SessionSummary>) -> Option<&'a SessionSummary> {
    dedupe_latest_by_session_id(candidates).into_iter().next()
}

fn unique_deduped_session_match<'a>(
    candidates: Vec<&'a SessionSummary>,
    arg: &str,
) -> Result<Option<&'a SessionSummary>, ResumeError> {
    let deduped = dedupe_latest_by_session_id(candidates);
    match deduped.len() {
        0 => Ok(None),
        1 => Ok(deduped.into_iter().next()),
        count => Err(ResumeError::MultipleMatches {
            arg: arg.to_string(),
            count,
        }),
    }
}

fn dedupe_latest_by_session_id<'a>(candidates: Vec<&'a SessionSummary>) -> Vec<&'a SessionSummary> {
    let mut latest: Vec<&SessionSummary> = Vec::new();
    for candidate in candidates {
        if let Some(existing) = latest
            .iter_mut()
            .find(|existing| existing.session_id == candidate.session_id)
        {
            if candidate.modified > existing.modified {
                *existing = candidate;
            }
        } else {
            latest.push(candidate);
        }
    }
    latest.sort_by(|a, b| b.modified.cmp(&a.modified));
    latest
}

fn matches_exact_resume_title(session: &SessionSummary, query: &str) -> bool {
    session
        .custom_title
        .as_deref()
        .is_some_and(|title| title.eq_ignore_ascii_case(query))
}

fn load_session_entries_for_selection(
    selection: &SessionSelection,
) -> Result<ResumeLoad, ResumeError> {
    let cwd = selection.project_path.clone().unwrap_or_else(|| {
        crate::bootstrap::state::get_original_cwd()
            .display()
            .to_string()
    });
    if selection.file_path.is_file() {
        return load_session_entries_from_path(&selection.file_path, &selection.session_id, &cwd);
    }

    if let Some(project_path) = selection.project_path.as_deref() {
        return load_session_entries_from_path(
            &session_storage::get_session_file_path(project_path, &selection.session_id),
            &selection.session_id,
            project_path,
        );
    }

    Err(ResumeError::EmptySession {
        session_id: selection.session_id.clone(),
    })
}

fn load_session_entries_from_path(
    path: &PathBuf,
    session_id: &str,
    cwd: &str,
) -> Result<ResumeLoad, ResumeError> {
    let session = session_storage::load_session_structured_from_path(path);
    let entries = session_storage::get_latest_leaf_uuid(&session)
        .as_deref()
        .map(|leaf_uuid| session_storage::build_conversation_chain(&session, leaf_uuid))
        .unwrap_or_default();

    let entries = if entries.is_empty() {
        session_storage::load_session_raw_from_path(path)
            .into_iter()
            .filter(is_message_log_entry)
            .collect::<Vec<_>>()
    } else {
        session_storage::remove_extra_fields(entries)
    };

    deserialize_session_entries(&session, entries, path, session_id, cwd)
}

/// Rust carrier adaptation for CC utils/conversationRecovery.ts#loadConversationForResume:
/// both the session-ID and LogOption branches feed the same deserialization
/// and restore-state extraction after selecting their source transcript.
fn deserialize_session_entries(
    session: &session_storage::LoadedSession,
    entries: Vec<serde_json::Value>,
    path: &Path,
    session_id: &str,
    cwd: &str,
) -> Result<ResumeLoad, ResumeError> {
    let mut metadata = build_resume_metadata(session, &entries, path, session_id);
    let deserialized = conversation::deserialize_messages_with_interrupt_detection(entries);
    metadata.read_file_state = query_helpers::extract_read_files_from_messages(
        &deserialized.messages,
        cwd,
        crate::utils::file_state_cache::READ_FILE_STATE_CACHE_SIZE,
    );
    metadata.bash_tools = query_helpers::extract_bash_tools_from_messages(&deserialized.messages);
    metadata.todos = session_restore::extract_todos_from_transcript(&deserialized.messages);

    if deserialized.messages.is_empty() {
        Err(ResumeError::EmptySession {
            session_id: session_id.to_string(),
        })
    } else {
        Ok(ResumeLoad {
            entries: deserialized.messages,
            turn_interruption_state: deserialized.turn_interruption_state,
            metadata,
        })
    }
}

fn build_resume_metadata(
    session: &session_storage::LoadedSession,
    conversation: &[serde_json::Value],
    path: &Path,
    fallback_session_id: &str,
) -> ResumeMetadata {
    let session_id = conversation
        .iter()
        .rev()
        .filter_map(|entry| entry.get("sessionId").and_then(|value| value.as_str()))
        .next()
        .unwrap_or(fallback_session_id)
        .to_string();

    ResumeMetadata {
        original_messages: Some(conversation.to_vec()),
        session_id: Some(session_id.clone()),
        file_history_snapshots: build_file_history_snapshot_chain(
            &session.file_history_snapshots,
            conversation,
        ),
        attribution_snapshots: session.attribution_snapshots.clone(),
        content_replacements: session
            .content_replacements
            .get(&session_id)
            .cloned()
            .unwrap_or_default(),
        context_collapse_commits: session
            .context_collapse_commits
            .iter()
            .filter(|entry| entry_session_id(entry) == Some(session_id.as_str()))
            .cloned()
            .collect(),
        context_collapse_snapshot: session
            .context_collapse_snapshot
            .as_ref()
            .and_then(|entry| {
                (entry_session_id(entry) == Some(session_id.as_str())).then(|| entry.clone())
            }),
        skill_restore: conversation::restore_skill_state_from_messages(conversation),
        read_file_state: Vec::new(),
        bash_tools: Vec::new(),
        todos: Vec::new(),
        agent_name: session.agent_names.get(&session_id).cloned(),
        agent_color: session.agent_colors.get(&session_id).cloned(),
        agent_setting: session.agent_settings.get(&session_id).cloned(),
        custom_title: session.custom_titles.get(&session_id).cloned(),
        tag: session.tags.get(&session_id).cloned(),
        mode: session.modes.get(&session_id).cloned(),
        worktree_session: session.worktree_states.get(&session_id).cloned(),
        pr_number: session.pr_numbers.get(&session_id).copied(),
        pr_url: session.pr_urls.get(&session_id).cloned(),
        pr_repository: session.pr_repositories.get(&session_id).cloned(),
        full_path: Some(path.display().to_string()),
    }
}

fn build_file_history_snapshot_chain(
    file_history_snapshots: &HashMap<String, serde_json::Value>,
    conversation: &[serde_json::Value],
) -> Vec<serde_json::Value> {
    let mut snapshots = Vec::new();
    let mut index_by_message_id: HashMap<String, usize> = HashMap::new();

    for message in conversation {
        let Some(uuid) = message.get("uuid").and_then(|value| value.as_str()) else {
            continue;
        };
        let Some(snapshot_message) = file_history_snapshots.get(uuid) else {
            continue;
        };
        let Some(snapshot) = snapshot_message.get("snapshot") else {
            continue;
        };
        let Some(message_id) = snapshot.get("messageId").and_then(|value| value.as_str()) else {
            continue;
        };

        let is_update = snapshot_message
            .get("isSnapshotUpdate")
            .and_then(|value| value.as_bool())
            == Some(true);
        if is_update {
            if let Some(index) = index_by_message_id.get(message_id).copied() {
                snapshots[index] = snapshot.clone();
                continue;
            }
        }

        index_by_message_id.insert(message_id.to_string(), snapshots.len());
        snapshots.push(snapshot.clone());
    }

    snapshots
}

fn entry_session_id(entry: &serde_json::Value) -> Option<&str> {
    entry.get("sessionId").and_then(|value| value.as_str())
}

fn is_message_log_entry(entry: &serde_json::Value) -> bool {
    matches!(
        entry.get("type").and_then(|value| value.as_str()),
        Some("user" | "assistant" | "system" | "attachment" | "hook_result")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, PinnedProjectDir, TEST_ENV_LOCK};
    use serde_json::json;
    use std::fs;
    use std::time::SystemTime;

    fn summary(id: &str, is_sidechain: bool, team_name: Option<&str>) -> SessionSummary {
        SessionSummary {
            session_id: id.to_string(),
            display: "first prompt".to_string(),
            custom_title: None,
            summary: None,
            tag: None,
            agent_name: None,
            agent_setting: None,
            git_branch: None,
            project_path: Some("/repo".to_string()),
            file_path: PathBuf::from(format!("/repo/{id}.jsonl")),
            is_sidechain,
            team_name: team_name.map(str::to_string),
            pr_number: None,
            pr_url: None,
            pr_repository: None,
            file_size: 1,
            modified: SystemTime::UNIX_EPOCH,
        }
    }

    fn with_modified(mut summary: SessionSummary, seconds: u64) -> SessionSummary {
        summary.modified = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(seconds);
        summary
    }

    #[test]
    fn cli_session_launch_resolution_is_io_free_until_main_finishes_setup() {
        assert!(matches!(
            resolve_cli_session_launch(false, Some("find this session")),
            CliSessionLaunch::Resume { value } if value == "find this session"
        ));
        assert!(matches!(
            resolve_cli_session_launch(false, Some("")),
            CliSessionLaunch::OpenPicker {
                initial_search_query: None
            }
        ));
        assert!(matches!(
            resolve_cli_session_launch(true, Some("ignored")),
            CliSessionLaunch::Continue
        ));
    }

    #[test]
    fn filter_resumable_sessions_matches_official_current_and_sidechain_filter() {
        let logs = vec![
            summary("current", false, None),
            summary("side", true, None),
            summary("team", false, Some("agent")),
            summary("ok", false, None),
        ];

        let filtered = filter_resumable_sessions(logs, Some("current"));
        assert_eq!(
            filtered
                .iter()
                .map(|log| log.session_id.as_str())
                .collect::<Vec<_>>(),
            vec!["team", "ok"]
        );
    }

    #[test]
    fn title_match_dedupes_forks_by_session_id_like_official_custom_title_search() {
        let mut older = with_modified(summary("session-a", false, None), 10);
        older.custom_title = Some("Release notes".to_string());
        older.file_path = PathBuf::from("/repo/session-a-old.jsonl");
        let mut newer = with_modified(summary("session-a", false, None), 20);
        newer.custom_title = Some("Release notes".to_string());
        newer.file_path = PathBuf::from("/repo/session-a-new.jsonl");

        let matched = unique_deduped_session_match(
            vec![&older, &newer]
                .into_iter()
                .filter(|session| matches_exact_resume_title(session, "release notes"))
                .collect(),
            "release notes",
        )
        .expect("same session forks should dedupe")
        .expect("title should match");

        assert_eq!(
            matched.file_path,
            PathBuf::from("/repo/session-a-new.jsonl")
        );
    }

    #[test]
    fn title_match_reports_multiple_after_session_deduplication() {
        let mut first = with_modified(summary("session-a", false, None), 10);
        first.custom_title = Some("Release notes".to_string());
        let mut second = with_modified(summary("session-b", false, None), 20);
        second.custom_title = Some("Release notes".to_string());

        let err = unique_deduped_session_match(
            vec![&first, &second]
                .into_iter()
                .filter(|session| matches_exact_resume_title(session, "release notes"))
                .collect(),
            "release notes",
        )
        .expect_err("two session ids remain ambiguous");

        assert_eq!(
            err,
            ResumeError::MultipleMatches {
                arg: "release notes".to_string(),
                count: 2,
            }
        );
    }

    #[test]
    fn resume_entries_recover_orphaned_parallel_tool_results() {
        let path = std::env::temp_dir().join(format!(
            "cometix-resume-parallel-tools-{}.jsonl",
            Uuid::new_v4()
        ));
        let entries = vec![
            json!({
                "type": "assistant",
                "uuid": "a1",
                "parentUuid": null,
                "isSidechain": false,
                "message": {
                    "id": "msg_parallel",
                    "content": [{"type": "tool_use", "id": "toolu_1", "name": "Bash", "input": {"command": "first"}}]
                }
            }),
            json!({
                "type": "assistant",
                "uuid": "a2",
                "parentUuid": "a1",
                "isSidechain": false,
                "message": {
                    "id": "msg_parallel",
                    "content": [{"type": "tool_use", "id": "toolu_2", "name": "Bash", "input": {"command": "second"}}]
                }
            }),
            json!({
                "type": "user",
                "uuid": "r1",
                "parentUuid": "a1",
                "isSidechain": false,
                "timestamp": "2026-06-13T13:40:32.798Z",
                "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": "first output", "is_error": false}]},
                "toolUseResult": {"stdout": "first output", "stderr": "", "interrupted": false, "isImage": false, "noOutputExpected": false}
            }),
            json!({
                "type": "user",
                "uuid": "r2",
                "parentUuid": "a2",
                "isSidechain": false,
                "timestamp": "2026-06-13T13:40:34.410Z",
                "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "toolu_2", "content": "second output", "is_error": false}]},
                "toolUseResult": {"stdout": "second output", "stderr": "", "interrupted": false, "isImage": false, "noOutputExpected": false}
            }),
        ];
        let contents = entries
            .iter()
            .map(serde_json::Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&path, format!("{contents}\n")).unwrap();

        let cwd = path.parent().unwrap().to_string_lossy().to_string();
        let loaded = load_session_entries_from_path(&path, "session", &cwd).unwrap();
        let _ = fs::remove_file(&path);
        assert_eq!(
            loaded.turn_interruption_state,
            conversation::TurnInterruptionState::InterruptedPrompt
        );
        let uuids = loaded
            .entries
            .iter()
            .filter_map(|entry| entry.get("uuid").and_then(|value| value.as_str()))
            .collect::<Vec<_>>();

        assert_eq!(&uuids[..4], &["a1", "a2", "r1", "r2"]);
        assert!(loaded.entries.iter().any(|entry| {
            entry.get("isMeta").and_then(|value| value.as_bool()) == Some(true)
                && entry
                    .get("message")
                    .and_then(|message| message.get("content"))
                    .and_then(|content| content.as_array())
                    .and_then(|blocks| blocks.first())
                    .and_then(|block| block.get("text"))
                    .and_then(|text| text.as_str())
                    == Some(conversation::CONTINUATION_MESSAGE)
        }));
        assert!(loaded.entries.iter().any(|entry| {
            entry
                .get("message")
                .and_then(|message| message.get("content"))
                .and_then(|content| content.as_array())
                .is_some_and(|blocks| {
                    blocks.iter().any(|block| {
                        block.get("type").and_then(|value| value.as_str()) == Some("text")
                            && block.get("text").and_then(|value| value.as_str())
                                == Some(conversation::NO_RESPONSE_REQUESTED)
                    })
                })
        }));
    }

    #[test]
    fn resume_load_preserves_official_restore_metadata_payloads() {
        let _env = TEST_ENV_LOCK.lock().unwrap();
        struct RestoreProject(String, Option<PathBuf>, PathBuf);
        impl Drop for RestoreProject {
            fn drop(&mut self) {
                crate::bootstrap::state::switch_session(self.0.clone(), self.1.clone());
                let _ = fs::remove_dir_all(&self.2);
            }
        }
        let fixture_dir = std::env::temp_dir().join(format!("resume-metadata-{}", Uuid::new_v4()));
        fs::create_dir_all(&fixture_dir).unwrap();
        let _restore = RestoreProject(
            crate::bootstrap::state::get_session_id(),
            crate::bootstrap::state::get_session_project_dir(),
            fixture_dir.clone(),
        );
        crate::bootstrap::state::switch_session(
            Uuid::new_v4().to_string(),
            Some(fixture_dir.clone()),
        );
        let session_id = "session-meta";
        let path = fixture_dir.join(format!("{session_id}.jsonl"));
        let entries = vec![
            json!({
                "type": "user",
                "uuid": "u1",
                "parentUuid": null,
                "sessionId": session_id,
                "cwd": "/transcript-project",
                "agentName": "TranscriptAgent",
                "timestamp": "2026-06-13T13:40:30.000Z",
                "message": {"role": "user", "content": "hello"}
            }),
            json!({
                "type": "attachment",
                "uuid": "skill-state-1",
                "parentUuid": "u1",
                "sessionId": session_id,
                "timestamp": "2026-06-13T13:40:30.250Z",
                "attachment": {
                    "type": "invoked_skills",
                    "skills": [{"name": "review", "path": "/skills/review", "content": "Use review rules."}]
                }
            }),
            json!({
                "type": "attachment",
                "uuid": "skill-listing-1",
                "parentUuid": "skill-state-1",
                "sessionId": session_id,
                "timestamp": "2026-06-13T13:40:30.500Z",
                "attachment": {"type": "skill_listing", "content": "review"}
            }),
            json!({
                "type": "assistant",
                "uuid": "a1",
                "parentUuid": "skill-listing-1",
                "sessionId": session_id,
                "timestamp": "2026-06-13T13:40:31.000Z",
                "message": {"id": "msg-1", "role": "assistant", "content": [{"type": "text", "text": "hi"}]}
            }),
            json!({
                "type": "assistant",
                "uuid": "a2",
                "parentUuid": "a1",
                "sessionId": session_id,
                "timestamp": "2026-06-13T13:40:32.000Z",
                "message": {"id": "msg-tools", "role": "assistant", "content": [
                    {"type": "tool_use", "id": "toolu_read", "name": "Read", "input": {"file_path": "src/lib.rs"}},
                    {"type": "tool_use", "id": "toolu_write", "name": "Write", "input": {"file_path": "README.md", "content": "updated readme"}},
                    {"type": "tool_use", "id": "toolu_bash", "name": "Bash", "input": {"command": "FOO=bar sudo git status"}},
                    {"type": "tool_use", "id": "toolu_todo", "name": "TodoWrite", "input": {"todos": [{"content": "restore task", "status": "pending", "activeForm": "Restoring task"}]}}
                ]}
            }),
            json!({
                "type": "user",
                "uuid": "u-tools",
                "parentUuid": "a2",
                "sessionId": session_id,
                "timestamp": "2026-06-13T13:40:33.000Z",
                "message": {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_read", "content": "     1→fn main() {}\n<system-reminder>ignore</system-reminder>     2\tprintln!();", "is_error": false},
                    {"type": "tool_result", "tool_use_id": "toolu_write", "content": "ok", "is_error": false},
                    {"type": "tool_result", "tool_use_id": "toolu_bash", "content": "git status", "is_error": false},
                    {"type": "tool_result", "tool_use_id": "toolu_todo", "content": "ok", "is_error": false}
                ]}
            }),
            json!({
                "type": "file-history-snapshot",
                "messageId": "u1",
                "snapshot": {"messageId": "file-state", "paths": ["old.rs"]}
            }),
            json!({
                "type": "file-history-snapshot",
                "messageId": "a1",
                "isSnapshotUpdate": true,
                "snapshot": {"messageId": "file-state", "paths": ["new.rs"]}
            }),
            json!({
                "type": "attribution-snapshot",
                "messageId": "attr-1",
                "snapshot": {"entries": [{"path": "src/lib.rs", "author": "dev"}]}
            }),
            json!({
                "type": "attribution-snapshot",
                "messageId": "attr-1",
                "snapshot": {"entries": [{"path": "src/lib.rs", "author": "dev-latest"}]}
            }),
            json!({
                "type": "content-replacement",
                "sessionId": session_id,
                "replacements": [{"toolUseId": "toolu_1", "path": "big.txt"}]
            }),
            json!({
                "type": "marble-origami-commit",
                "sessionId": "other-session",
                "commit": "ignored"
            }),
            json!({
                "type": "marble-origami-commit",
                "sessionId": session_id,
                "commit": "kept"
            }),
            json!({
                "type": "marble-origami-snapshot",
                "sessionId": session_id,
                "snapshot": {"state": "current"}
            }),
            json!({"type": "custom-title", "sessionId": session_id, "customTitle": "Saved title"}),
            json!({"type": "tag", "sessionId": session_id, "tag": "work"}),
            json!({"type": "agent-name", "sessionId": session_id, "agentName": "ReviewBot"}),
            json!({"type": "agent-color", "sessionId": session_id, "agentColor": "blue"}),
            json!({"type": "agent-setting", "sessionId": session_id, "agentSetting": "reviewer"}),
            json!({"type": "mode", "sessionId": session_id, "mode": "normal"}),
            json!({"type": "worktree-state", "sessionId": session_id, "worktreeSession": {"worktreePath": "/repo-wt", "originalCwd": "/repo"}}),
            json!({"type": "pr-link", "sessionId": session_id, "prNumber": 42, "prUrl": "https://example.test/pr/42", "prRepository": "repo"}),
        ];
        let contents = entries
            .iter()
            .map(serde_json::Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&path, format!("{contents}\n")).unwrap();

        let cwd = path.parent().unwrap().to_string_lossy().to_string();
        let loaded = load_session_entries_from_path(&path, session_id, &cwd).unwrap();
        // getLastSessionLog -> convertToLogOption deliberately omits the
        // richer loadFullLog fields; preserve its first-message fields too.
        let direct = load_resolved_arg(ResolvedResume {
            selection: SessionSelection {
                session_id: session_id.to_string(),
                project_path: Some("/index-project".to_string()),
                file_path: path.clone(),
            },
            entrypoint: ResumeEntrypoint::SlashCommandSessionId,
            source: ResumeLogSource::SessionId,
        })
        .unwrap();
        assert_eq!(direct.project_path.as_deref(), Some("/transcript-project"));
        assert_eq!(
            direct.metadata.agent_name.as_deref(),
            Some("TranscriptAgent")
        );
        assert_eq!(direct.metadata.agent_color, None);
        assert_eq!(direct.metadata.mode, None);
        assert_eq!(direct.metadata.pr_number, None);
        assert_eq!(direct.metadata.pr_url, None);
        assert_eq!(direct.metadata.pr_repository, None);
        assert_eq!(direct.metadata.agent_setting.as_deref(), Some("reviewer"));
        assert_eq!(direct.metadata.custom_title.as_deref(), Some("Saved title"));
        assert_eq!(direct.metadata.tag.as_deref(), Some("work"));
        assert_eq!(
            direct.metadata.worktree_session,
            loaded.metadata.worktree_session
        );
        // Absence remains absent even when the index and standalone metadata
        // contain values; it is not a request for fallback to those values.
        let mut without_first_fields = entries.clone();
        without_first_fields[0]
            .as_object_mut()
            .unwrap()
            .remove("cwd");
        without_first_fields[0]
            .as_object_mut()
            .unwrap()
            .remove("agentName");
        without_first_fields.extend([
            json!({"type":"agent-setting","sessionId":"requested-id","agentSetting":"requested-agent"}),
            json!({"type":"content-replacement","sessionId":"requested-id","replacements":[{"toolUseId":"requested-tool","path":"requested.txt"}]}),
            json!({"type":"worktree-state","sessionId":"requested-id","worktreeSession":{"worktreePath":"/requested-wt"}}),
            json!({"type":"marble-origami-commit","sessionId":"requested-id","commit":"requested-commit"}),
            json!({"type":"marble-origami-snapshot","sessionId":"requested-id","snapshot":{"state":"requested"}}),
        ]);
        fs::write(
            &path,
            without_first_fields
                .iter()
                .map(serde_json::Value::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();
        fs::copy(&path, fixture_dir.join("requested-id.jsonl")).unwrap();
        let absent = load_resolved_arg(ResolvedResume {
            selection: SessionSelection {
                session_id: "requested-id".to_string(),
                project_path: Some("/index-project".to_string()),
                file_path: path.clone(),
            },
            entrypoint: ResumeEntrypoint::SlashCommandSessionId,
            source: ResumeLogSource::SessionId,
        })
        .unwrap();
        assert_eq!(absent.project_path, None);
        assert_eq!(absent.metadata.agent_name, None);
        assert_eq!(absent.metadata.session_id.as_deref(), Some("requested-id"));
        assert_eq!(
            absent.metadata.agent_setting.as_deref(),
            Some("requested-agent")
        );
        assert_eq!(
            absent.metadata.content_replacements[0]["toolUseId"],
            "requested-tool"
        );
        assert_eq!(
            absent.metadata.worktree_session.as_ref().unwrap()["worktreePath"],
            "/requested-wt"
        );
        assert_eq!(absent.metadata.context_collapse_commits.len(), 1);
        assert_eq!(
            absent.metadata.context_collapse_commits[0]["commit"],
            "requested-commit"
        );
        assert_eq!(
            absent.metadata.context_collapse_snapshot.as_ref().unwrap()["sessionId"],
            "requested-id"
        );
        assert_eq!(absent.metadata.custom_title.as_deref(), Some("Saved title"));
        assert_eq!(absent.metadata.tag.as_deref(), Some("work"));
        let _ = fs::remove_file(&path);
        let metadata = loaded.metadata;

        assert_eq!(metadata.session_id.as_deref(), Some(session_id));
        assert_eq!(metadata.file_history_snapshots.len(), 1);
        assert_eq!(
            metadata.file_history_snapshots[0]
                .get("paths")
                .and_then(|value| value.as_array())
                .and_then(|paths| paths.first())
                .and_then(|value| value.as_str()),
            Some("new.rs")
        );
        assert_eq!(metadata.attribution_snapshots.len(), 1);
        assert_eq!(
            metadata.attribution_snapshots[0]
                .get("messageId")
                .and_then(|value| value.as_str()),
            Some("attr-1")
        );
        assert_eq!(
            metadata.attribution_snapshots[0]
                .pointer("/snapshot/entries/0/author")
                .and_then(|value| value.as_str()),
            Some("dev-latest")
        );
        assert_eq!(metadata.content_replacements.len(), 1);
        assert_eq!(
            metadata.content_replacements[0]
                .get("toolUseId")
                .and_then(|value| value.as_str()),
            Some("toolu_1")
        );
        assert_eq!(metadata.context_collapse_commits.len(), 1);
        assert_eq!(
            metadata.context_collapse_commits[0]
                .get("commit")
                .and_then(|value| value.as_str()),
            Some("kept")
        );
        assert!(metadata.context_collapse_snapshot.is_some());
        assert!(metadata.skill_restore.suppress_next_skill_listing);
        assert_eq!(metadata.skill_restore.invoked_skills.len(), 1);
        assert_eq!(metadata.skill_restore.invoked_skills[0].name, "review");
        assert_eq!(
            metadata.skill_restore.invoked_skills[0].path,
            "/skills/review"
        );
        let read_path = Path::new(&cwd).join("src/lib.rs").display().to_string();
        let write_path = Path::new(&cwd).join("README.md").display().to_string();
        assert_eq!(metadata.read_file_state.len(), 2);
        assert_eq!(metadata.read_file_state[0].path, read_path);
        assert_eq!(
            metadata.read_file_state[0].content.as_deref(),
            Some("fn main() {}\nprintln!();")
        );
        assert_eq!(
            metadata.read_file_state[0].source,
            query_helpers::ReadFileStateSource::Read
        );
        assert_eq!(metadata.read_file_state[1].path, write_path);
        assert_eq!(
            metadata.read_file_state[1].content.as_deref(),
            Some("updated readme")
        );
        assert_eq!(
            metadata.read_file_state[1].source,
            query_helpers::ReadFileStateSource::Write
        );
        assert_eq!(metadata.bash_tools, vec!["git".to_string()]);
        assert_eq!(metadata.todos.len(), 1);
        assert_eq!(
            metadata.todos[0]
                .get("content")
                .and_then(|value| value.as_str()),
            Some("restore task")
        );
        assert_eq!(
            metadata.todos[0]
                .get("activeForm")
                .and_then(|value| value.as_str()),
            Some("Restoring task")
        );
        assert_eq!(metadata.custom_title.as_deref(), Some("Saved title"));
        assert_eq!(metadata.tag.as_deref(), Some("work"));
        assert_eq!(metadata.agent_name.as_deref(), Some("ReviewBot"));
        assert_eq!(metadata.agent_color.as_deref(), Some("blue"));
        assert_eq!(metadata.agent_setting.as_deref(), Some("reviewer"));
        assert_eq!(metadata.mode.as_deref(), Some("normal"));
        assert!(metadata.worktree_session.is_some());
        assert_eq!(metadata.pr_number, Some(42));
        assert_eq!(
            metadata.pr_url.as_deref(),
            Some("https://example.test/pr/42")
        );
        assert_eq!(metadata.pr_repository.as_deref(), Some("repo"));
        assert_eq!(
            metadata.full_path.as_deref(),
            Some(path.to_string_lossy().as_ref())
        );
    }
    #[test]
    fn resume_direct_lookup_and_empty_list_match_official_entrypoints() {
        // commands/resume/resume.tsx:252-293 vs conversationRecovery.ts:520;
        // sessionStorage.ts:3830 resolves UUIDs against the ACTIVE project dir.
        let _env = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!("resume-lookup-{}", Uuid::new_v4()));
        let original = root.join("original");
        let active = root.join("active-session-dir");
        std::fs::create_dir_all(&original).unwrap();
        std::fs::create_dir_all(&active).unwrap();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", root.join("config"));
        let _project = PinnedProjectDir::at(&original);
        struct Restore(String, Option<PathBuf>, PathBuf);
        impl Drop for Restore {
            fn drop(&mut self) {
                crate::bootstrap::state::switch_session(self.0.clone(), self.1.clone());
                let _ = std::fs::remove_dir_all(&self.2);
            }
        }
        let _restore = Restore(
            crate::bootstrap::state::get_session_id(),
            crate::bootstrap::state::get_session_project_dir(),
            root.clone(),
        );
        crate::bootstrap::state::switch_session(Uuid::new_v4().to_string(), Some(active.clone()));
        let id = Uuid::new_v4().to_string();
        let row = json!({"type":"user","uuid":"direct-message","parentUuid":null,
            "sessionId":id,"cwd":"/restored-cwd","timestamp":"2026-09-12T01:00:00Z",
            "message":{"role":"user","content":"active directory transcript"}});
        std::fs::write(active.join(format!("{id}.jsonl")), format!("{row}\n")).unwrap();
        let cwd = original.to_str().unwrap();
        assert!(matches!(
            load_for_arg(cwd, &id),
            Err(ResumeError::NoConversations)
        ));
        let cli = load_for_cli_resume(cwd, &id).unwrap();
        assert_eq!(cli.project_path.as_deref(), Some("/restored-cwd"));
        assert_eq!(cli.entries[0]["uuid"], "direct-message");
        assert_eq!(cli.entrypoint, Some(ResumeEntrypoint::CliFlag));
        let listed_id = Uuid::new_v4().to_string();
        let listed = session_storage::get_session_file_path(cwd, &listed_id);
        std::fs::create_dir_all(listed.parent().unwrap()).unwrap();
        let listed_row = json!({"type":"user","uuid":"listed-message","parentUuid":null,
            "sessionId":listed_id,"cwd":cwd,"timestamp":"2026-09-12T01:00:00Z",
            "message":{"role":"user","content":"indexed transcript"}});
        std::fs::write(listed, format!("{listed_row}\n")).unwrap();
        let direct = load_for_arg(cwd, &id).unwrap();
        assert_eq!(direct.entries[0]["uuid"], "direct-message");
        assert_eq!(
            direct.metadata.full_path,
            Some(
                session_storage::get_session_file_path(cwd, &id)
                    .display()
                    .to_string()
            )
        );
        assert_ne!(
            direct.metadata.full_path,
            Some(active.join(format!("{id}.jsonl")).display().to_string()),
            "CC sessionStorage.ts:3918 returns original-cwd fullPath separately from loadSessionFile's active directory"
        );
        assert_eq!(direct.project_path.as_deref(), Some("/restored-cwd"));
        let invalid_id = Uuid::new_v4().to_string();
        std::fs::write(active.join(format!("{invalid_id}.jsonl")), "{}\n").unwrap();
        assert!(
            matches!(
                load_for_arg(cwd, &invalid_id),
                Err(ResumeError::SessionNotFound { .. })
            ),
            "a file without a resumable log continues to the title/not-found branch"
        );
        let indexed = load_for_arg(cwd, &listed_id).unwrap();
        assert_eq!(indexed.entries[0]["uuid"], "listed-message");
    }
    #[test]
    fn resume_preserves_original_plan_messages_before_deserialization_matches_official() {
        // conversationRecovery.ts:540–556 recovers from log.messages before
        // deserializeMessages filters unresolved ExitPlanMode tool_use blocks.
        let _env = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!("resume-plan-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root);
        let _project = PinnedProjectDir::at(&root);
        let _remote = EnvVarGuard::set("CLAUDE_CODE_ENVIRONMENT_KIND", "byoc");
        let _simple = EnvVarGuard::set("CLAUDE_CODE_SIMPLE", "1");
        struct Restore(String, Option<PathBuf>, PathBuf);
        impl Drop for Restore {
            fn drop(&mut self) {
                crate::bootstrap::state::switch_session(self.0.clone(), self.1.clone());
                crate::utils::plans::clear_plans_directory_cache();
                let _ = fs::remove_dir_all(&self.2);
            }
        }
        let _restore = Restore(
            crate::bootstrap::state::get_session_id(),
            crate::bootstrap::state::get_session_project_dir(),
            root.clone(),
        );
        crate::bootstrap::state::switch_session(Uuid::new_v4().to_string(), None);
        crate::utils::plans::clear_plans_directory_cache();
        let id = Uuid::new_v4().to_string();
        let path = session_storage::get_session_file_path(root.to_str().unwrap(), &id);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let entries = [
            json!({"type":"user","uuid":"u","sessionId":id,"slug":"interrupted-plan",
                "cwd":root,"parentUuid":null,"timestamp":"2026-09-12T01:00:00Z",
                "message":{"role":"user","content":"plan"}}),
            json!({"type":"assistant","uuid":"a","sessionId":id,"slug":"interrupted-plan",
                "cwd":root,"parentUuid":"u","timestamp":"2026-09-12T01:00:01Z",
                "message":{"role":"assistant","content":[{"type":"tool_use","id":"pending",
                    "name":"ExitPlanMode","input":{"plan":"must survive interrupted permission"}}]}}),
        ];
        fs::write(
            path,
            entries
                .iter()
                .map(|row| format!("{row}\n"))
                .collect::<String>(),
        )
        .unwrap();
        let target = load_for_cli_resume(root.to_str().unwrap(), &id).unwrap();
        assert!(!target.entries.iter().any(|row| row["uuid"] == "a"));
        assert!(
            target
                .metadata
                .original_messages
                .as_ref()
                .unwrap()
                .iter()
                .any(|row| row["uuid"] == "a")
        );
        let plan_path = root.join("plans/interrupted-plan.md");
        assert!(!plan_path.exists());
        crate::utils::conversation_recovery::load_conversation_for_resume(&target).unwrap();
        assert_eq!(
            fs::read_to_string(plan_path).unwrap(),
            "must survive interrupted permission"
        );
        assert_eq!(
            crate::utils::plans::get_plan_slug(Some(&id)),
            "interrupted-plan"
        );
        crate::utils::plans::clear_plan_slug(Some(&id));
    }
}
