//! Resume restore pipeline.
//! Maps to official `utils/sessionRestore.ts`: shared CLI startup and
//! in-session `/resume` state derivation, session identity switching, metadata
//! restoration, and non-fork transcript adoption.

use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

const TODO_WRITE_TOOL_NAME: &str = "TodoWrite";

/// Everything `restoreSessionStateFromLog` reconstructs, carried as one value.
///
/// Rust-only aggregate, no CC counterpart type: upstream restores each of these
/// into its own module-level store, so there is nothing to name. It lives here
/// because this module produces it (`:369`) and because most of its field types
/// are declared right below — parking it in `state/` gave the aggregate a home
/// in a family whose CC original never mentions it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ResumeRestoreStores {
    pub session_id: Option<String>,
    pub project_path: Option<String>,
    pub entrypoint: Option<crate::types::command::ResumeEntrypoint>,
    pub turn_interruption_state: Option<crate::utils::conversation::TurnInterruptionState>,
    /// In-memory counterpart of official AppState.fileHistory restored by
    /// `restoreSessionStateFromLog`. Cometix does not copy backup files or
    /// write file-history records in this UI-only phase.
    pub file_history: Option<RestoredFileHistoryState>,
    /// Commit attribution payloads are ant-only upstream; keep the restored
    /// snapshots available without enabling git attribution side effects.
    pub attribution_snapshots: Vec<serde_json::Value>,
    pub content_replacements: Vec<crate::utils::tool_result_storage::ContentReplacementRecord>,
    /// Always replaced on resume, including empty inputs, to avoid stale
    /// context-collapse state leaking from the previous session.
    pub context_collapse: RestoredContextCollapseState,
    pub skill_restore: crate::utils::conversation::SkillRestoreState,
    pub read_file_state: Vec<crate::utils::query_helpers::ReadFileStateEntry>,
    pub bash_tools: Vec<String>,
    /// Official SDK/non-interactive restore stores TodoWrite state by session
    /// id. Interactive Cometix only exposes this as an in-memory seam.
    pub todos_by_session: std::collections::BTreeMap<String, Vec<serde_json::Value>>,
    pub standalone_agent_context: Option<RestoredStandaloneAgentContext>,
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

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RestoredFileHistoryState {
    /// Mirrors official `FileHistoryState.snapshots`. Backup paths are copied
    /// from the transcript payload and shortened relative to the restored cwd
    /// when possible; backup files are never copied or created here.
    pub snapshots: Vec<Value>,
    /// Deterministic Vec counterpart of official `trackedFiles: Set<string>`.
    pub tracked_files: Vec<String>,
    pub snapshot_sequence: usize,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RestoredContextCollapseState {
    /// Mirrors official context-collapse persisted commit entries. The store is
    /// reset from the payload on every resume, even when this vector is empty.
    pub commits: Vec<Value>,
    pub snapshot: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestoredStandaloneAgentContext {
    pub name: String,
    pub color: Option<String>,
}

/// Maps to: CC `utils/sessionRestore.ts:295-324` `ResumeLoadResult`.
/// `renderable_messages` is the documented retained-render projection of the
/// same official `messages` array.
#[derive(Clone, Debug, PartialEq)]
pub struct ResumeLoadResult {
    pub messages: Arc<Vec<crate::types::message::Message>>,
    pub renderable_messages: Arc<Vec<crate::types::message::RenderableMessage>>,
    pub turn_interruption_state: crate::utils::conversation::TurnInterruptionState,
    pub file_history_snapshots: Vec<Value>,
    pub attribution_snapshots: Vec<Value>,
    pub content_replacements: Vec<Value>,
    pub context_collapse_commits: Vec<Value>,
    pub context_collapse_snapshot: Option<Value>,
    pub session_id: Option<String>,
    /// Rust storage/launcher payload retained beside the official fields.
    pub project_path: Option<String>,
    pub entrypoint: Option<crate::types::command::ResumeEntrypoint>,
    pub agent_name: Option<String>,
    pub agent_color: Option<String>,
    pub agent_setting: Option<String>,
    pub custom_title: Option<String>,
    pub tag: Option<String>,
    pub mode: Option<String>,
    pub worktree_session: Option<Value>,
    pub pr_number: Option<u64>,
    pub pr_url: Option<String>,
    pub pr_repository: Option<String>,
    pub full_path: Option<String>,
    /// Rust typed restore-store payloads derived at the same load boundary.
    pub skill_restore: crate::utils::conversation::SkillRestoreState,
    pub read_file_state: Vec<crate::utils::query_helpers::ReadFileStateEntry>,
    pub bash_tools: Vec<String>,
    pub todos: Vec<Value>,
}

impl TryFrom<&crate::commands::resume::ResumeTarget> for ResumeLoadResult {
    type Error = String;

    fn try_from(target: &crate::commands::resume::ResumeTarget) -> Result<Self, Self::Error> {
        // ONE cold parse (batch D3 item 6, CC conversationRecovery.ts:154):
        // the render half is the normalize projection of the same messages
        // (CC Messages.tsx via utils/messages.ts:741 normalizeMessages).
        let messages = crate::utils::conversation_recovery::messages_from_entries(&target.entries);
        if messages.is_empty() {
            return Err(format!(
                "Session {} has no renderable transcript messages.",
                target.session_id
            ));
        }
        let metadata = &target.metadata;
        Ok(Self {
            renderable_messages: Arc::new(crate::utils::messages::normalize_messages(&messages)),
            messages: Arc::new(messages),
            turn_interruption_state: target.turn_interruption_state.clone(),
            file_history_snapshots: metadata.file_history_snapshots.clone(),
            attribution_snapshots: metadata.attribution_snapshots.clone(),
            content_replacements: metadata.content_replacements.clone(),
            context_collapse_commits: metadata.context_collapse_commits.clone(),
            context_collapse_snapshot: metadata.context_collapse_snapshot.clone(),
            session_id: metadata
                .session_id
                .clone()
                .or_else(|| Some(target.session_id.clone())),
            project_path: target.project_path.clone(),
            entrypoint: target.entrypoint,
            agent_name: metadata.agent_name.clone(),
            agent_color: metadata.agent_color.clone(),
            agent_setting: metadata.agent_setting.clone(),
            custom_title: metadata.custom_title.clone(),
            tag: metadata.tag.clone(),
            mode: metadata.mode.clone(),
            worktree_session: metadata.worktree_session.clone(),
            pr_number: metadata.pr_number,
            pr_url: metadata.pr_url.clone(),
            pr_repository: metadata.pr_repository.clone(),
            full_path: metadata.full_path.clone(),
            skill_restore: metadata.skill_restore.clone(),
            read_file_state: metadata.read_file_state.clone(),
            bash_tools: metadata.bash_tools.clone(),
            todos: metadata.todos.clone(),
        })
    }
}

/// Maps to: CC `utils/sessionRestore.ts:267-286` `ProcessedResume`.
/// Large immutable launch arrays are Arc-backed and cloned by reference.
#[derive(Clone, Debug, PartialEq)]
pub struct ProcessedResume {
    pub messages: Arc<Vec<crate::types::message::Message>>,
    pub renderable_messages: Arc<Vec<crate::types::message::RenderableMessage>>,
    pub file_history_snapshots: Option<Arc<Vec<crate::utils::file_history::FileHistorySnapshot>>>,
    pub content_replacements:
        Option<Arc<Vec<crate::utils::tool_result_storage::ContentReplacementRecord>>>,
    pub agent_name: Option<Arc<str>>,
    pub agent_color: Option<Arc<str>>,
    pub restored_agent_def: Option<Arc<crate::tools::agent_tool::load_agents_dir::AgentDefinition>>,
    pub agent_definitions: Arc<crate::tools::agent_tool::load_agents_dir::AgentDefinitionsResult>,
    pub resume_restore_stores: Arc<ResumeRestoreStores>,
    restored_agent_type: Option<String>,
    restored_file_history: Option<Arc<crate::utils::file_history::FileHistoryState>>,
}

impl ProcessedResume {
    /// Maps to: CC `processResumedConversation` constructing `initialState`
    /// before `launchRepl` (`utils/sessionRestore.ts:509-544`).
    pub fn apply_to_app_state(&self, state: &mut crate::state::app_state_store::AppState) {
        state.agent_definitions = self.agent_definitions.clone();
        state.agent = self
            .restored_agent_def
            .as_ref()
            .map(|agent| agent.agent_type.clone());
        state.standalone_agent_context =
            self.resume_restore_stores.standalone_agent_context.clone();
        if let Some(file_history) = self.restored_file_history.as_ref() {
            state.file_history = file_history.clone();
        }

        // Maps to `restoreAgentFromSession`: a resumed agent supplies its model
        // when the bootstrap override is falsey (undefined, null, or the empty
        // string). A model inherited from env/settings may already be present in AppState, but
        // unlike an explicit CLI/session override it must not block the agent.
        if self.restored_agent_type.is_some()
            && match crate::bootstrap::state::get_main_loop_model_override() {
                None | Some(None) => true,
                Some(Some(model)) => model.is_empty(),
            }
        {
            if let Some(model) = self
                .restored_agent_def
                .as_ref()
                .and_then(|agent| agent.model.as_deref())
                .filter(|model| *model != "inherit")
            {
                let resolved = crate::utils::model::model::parse_user_specified_model(model);
                crate::bootstrap::state::set_main_loop_model_override(Some(Some(resolved)));
                state.main_loop_model = Some(model.to_string());
            }
        }
    }
}

/// Maps to: CC `utils/sessionRestore.ts:179-225` `restoreAgentFromSession`.
pub fn restore_agent_from_session(
    agent_setting: Option<&str>,
    current_agent_definition: Option<
        Arc<crate::tools::agent_tool::load_agents_dir::AgentDefinition>,
    >,
    agent_definitions: &crate::tools::agent_tool::load_agents_dir::AgentDefinitionsResult,
) -> (
    Option<Arc<crate::tools::agent_tool::load_agents_dir::AgentDefinition>>,
    Option<String>,
) {
    if let Some(current) = current_agent_definition {
        return (Some(current), None);
    }
    let Some(agent_setting) = agent_setting else {
        return (None, None);
    };
    let restored = agent_definitions
        .active_agents
        .iter()
        .find(|agent| agent.agent_type == agent_setting)
        .cloned()
        .map(Arc::new);
    if restored.is_none() {
        crate::utils::debug::log_for_debugging(&format!(
            "Resumed session had agent \"{agent_setting}\" but it is no longer available. Using default behavior."
        ));
    }
    let restored_type = restored.as_ref().map(|agent| agent.agent_type.clone());
    (restored, restored_type)
}

fn append_resume_system_warning(
    model_messages: &mut Vec<crate::types::message::Message>,
    renderable_messages: &mut Vec<crate::types::message::RenderableMessage>,
    warning: String,
) {
    let uuid = uuid::Uuid::new_v4().to_string();
    let message = crate::types::message::SystemMessage::informational_with_uuid(
        uuid.clone(),
        warning,
        crate::types::message::SystemMessageLevel::Warning,
    );
    model_messages.push(crate::types::message::Message::System(message.clone()));
    renderable_messages.push(crate::types::message::RenderableMessage {
        uuid,
        kind: crate::types::message::RenderableMessageKind::System(message),
    });
}

/// Maps to: CC `utils/sessionRestore.ts` `restoreWorktreeForResume(...)`.
pub fn restore_worktree_for_resume(worktree_session: Option<&Value>) {
    if let Some(fresh) = crate::utils::worktree::get_current_worktree_session() {
        crate::utils::session_storage::save_worktree_state(
            crate::utils::worktree::worktree_session_to_persisted_json(&fresh),
        );
        return;
    }

    let Some(worktree_session) = worktree_session.filter(|value| !value.is_null()) else {
        return;
    };
    let Ok(worktree_session) =
        serde_json::from_value::<crate::utils::worktree::WorktreeSession>(worktree_session.clone())
    else {
        return;
    };
    if std::env::set_current_dir(&worktree_session.worktree_path).is_err() {
        // Preserve CC's tri-state: JSON null means the persisted worktree was
        // exited, while Rust None means this field was never touched.
        crate::utils::session_storage::save_worktree_state(Value::Null);
        return;
    }

    let cwd = std::env::current_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from(&worktree_session.worktree_path));
    crate::bootstrap::state::set_original_cwd(cwd);
    crate::utils::worktree::restore_worktree_session(Some(worktree_session));
    // Existing Rust cache owners covered by CC's memory/system-prompt clears.
    crate::context::set_system_prompt_injection(None);
    crate::utils::plans::clear_plans_directory_cache();
}

/// Maps to: CC `utils/sessionRestore.ts` `exitRestoredWorktree()`.
pub fn exit_restored_worktree() {
    let Some(current) = crate::utils::worktree::get_current_worktree_session() else {
        return;
    };
    crate::utils::worktree::restore_worktree_session(None);
    crate::context::set_system_prompt_injection(None);
    crate::utils::plans::clear_plans_directory_cache();
    if std::env::set_current_dir(&current.original_cwd).is_err() {
        return;
    }
    let cwd =
        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(&current.original_cwd));
    crate::bootstrap::state::set_original_cwd(cwd);
}

/// Maps to: CC `utils/sessionRestore.ts:409-545`
/// `processResumedConversation` for the non-fork interactive path.
///
/// Rust-owned cost counters, session identity, metadata, worktree chdir, and
/// file adoption are live; model-usage context enrichment and resume telemetry remain seams.
pub fn process_resumed_conversation(
    mut result: ResumeLoadResult,
    current_agent_definition: Option<
        Arc<crate::tools::agent_tool::load_agents_dir::AgentDefinition>,
    >,
    mut agent_definitions: Arc<crate::tools::agent_tool::load_agents_dir::AgentDefinitionsResult>,
) -> Result<ProcessedResume, String> {
    if let Some(warning) =
        crate::coordinator::coordinator_mode::match_session_mode(result.mode.as_deref())
    {
        append_resume_system_warning(
            Arc::make_mut(&mut result.messages),
            Arc::make_mut(&mut result.renderable_messages),
            warning,
        );
        let cwd = crate::bootstrap::state::get_original_cwd();
        agent_definitions = Arc::new(
            crate::tools::agent_tool::load_agents_dir::get_agent_definitions_with_overrides(&cwd),
        );
    }

    if let Some(session_id) = result.session_id.as_deref() {
        let session_project_dir = result
            .full_path
            .as_deref()
            .and_then(|path| Path::new(path).parent())
            .map(Path::to_path_buf);
        crate::bootstrap::state::switch_session(session_id.to_string(), session_project_dir);
        crate::utils::asciicast::rename_recording_for_session();
        crate::utils::session_storage::reset_session_file_pointer();
        crate::cost_tracker::restore_cost_state_for_session(session_id);
        crate::utils::session_storage::restore_session_metadata(
            &crate::utils::session_storage::SessionMetadataCache {
                session_id: session_id.to_string(),
                custom_title: result.custom_title.clone(),
                tag: result.tag.clone(),
                agent_name: result.agent_name.clone(),
                agent_color: result.agent_color.clone(),
                agent_setting: result.agent_setting.clone(),
                mode: result.mode.clone(),
                worktree_session: result.worktree_session.clone(),
                last_prompt: None,
                pr_number: result.pr_number,
                pr_url: result.pr_url.clone(),
                pr_repository: result.pr_repository.clone(),
            },
        );
        restore_worktree_for_resume(result.worktree_session.as_ref());
        crate::utils::session_storage::adopt_resumed_session_file()
            .map_err(|error| format!("Failed to adopt resumed session {session_id}: {error}"))?;
        let hook_messages = result
            .messages
            .iter()
            .filter(|message| matches!(message, crate::types::message::Message::HookResult(_)))
            .cloned()
            .collect::<Vec<_>>();
        crate::utils::session_storage::record_typed_messages(&hook_messages).map_err(|error| {
            format!("Failed to record resumed SessionStart hooks for {session_id}: {error}")
        })?;
    }

    Ok(ProcessedResume::from_load_result(
        result,
        current_agent_definition,
        agent_definitions,
    ))
}

impl ProcessedResume {
    /// Rust retained-state carrier construction shared by CC
    /// `utils/sessionRestore.ts#processResumedConversation:461-544` and
    /// `screens/REPL.tsx#resume:2423-2457`. Session switching remains at each
    /// source caller; this boundary only transports restored state to the UI.
    pub(crate) fn from_load_result(
        result: ResumeLoadResult,
        current_agent_definition: Option<
            Arc<crate::tools::agent_tool::load_agents_dir::AgentDefinition>,
        >,
        agent_definitions: Arc<crate::tools::agent_tool::load_agents_dir::AgentDefinitionsResult>,
    ) -> Self {
        let file_history_snapshots = result
            .file_history_snapshots
            .iter()
            .filter_map(|snapshot| serde_json::from_value(snapshot.clone()).ok())
            .collect::<Vec<crate::utils::file_history::FileHistorySnapshot>>();
        let content_replacements =
            crate::utils::tool_result_storage::content_replacement_records_from_values(
                &result.content_replacements,
            );
        let (restored_agent_def, restored_agent_type) = restore_agent_from_session(
            result.agent_setting.as_deref(),
            current_agent_definition,
            &agent_definitions,
        );
        let standalone_agent_context = compute_standalone_agent_context(
            result.agent_name.as_deref(),
            result.agent_color.as_deref(),
        );
        crate::utils::conversation_recovery::restore_skill_state_from_messages(&result.messages);

        let session_id = result.session_id.clone().unwrap_or_default();
        let mut todos_by_session = BTreeMap::new();
        if !result.todos.is_empty() {
            todos_by_session.insert(session_id.clone(), result.todos.clone());
        }
        let resume_restore_stores = Arc::new(ResumeRestoreStores {
            session_id: result.session_id.clone(),
            project_path: result.project_path.clone(),
            entrypoint: result.entrypoint,
            turn_interruption_state: Some(result.turn_interruption_state.clone()),
            file_history: file_history_restore_state_from_log(
                &result.file_history_snapshots,
                result.project_path.as_deref(),
            ),
            attribution_snapshots: result.attribution_snapshots.clone(),
            content_replacements: content_replacements.clone(),
            context_collapse: context_collapse_restore_state_from_log(
                &result.context_collapse_commits,
                result.context_collapse_snapshot.as_ref(),
            ),
            skill_restore: result.skill_restore.clone(),
            read_file_state: result.read_file_state.clone(),
            bash_tools: result.bash_tools.clone(),
            todos_by_session,
            standalone_agent_context,
            agent_setting: result.agent_setting.clone(),
            custom_title: result.custom_title.clone(),
            tag: result.tag.clone(),
            mode: result.mode.clone(),
            worktree_session: result.worktree_session.clone(),
            pr_number: result.pr_number,
            pr_url: result.pr_url.clone(),
            pr_repository: result.pr_repository.clone(),
            full_path: result.full_path.clone(),
        });

        let mut restored_file_history = None;
        if result.entrypoint != Some(crate::types::command::ResumeEntrypoint::Fork)
            || !file_history_snapshots.is_empty()
        {
            crate::utils::file_history::file_history_restore_state_from_log(
                &file_history_snapshots,
                |state| restored_file_history = Some(Arc::new(state)),
            );
        }

        ProcessedResume {
            messages: result.messages,
            renderable_messages: result.renderable_messages,
            file_history_snapshots: (!file_history_snapshots.is_empty())
                .then(|| Arc::new(file_history_snapshots)),
            content_replacements: (!content_replacements.is_empty())
                .then(|| Arc::new(content_replacements)),
            agent_name: result.agent_name.as_deref().map(Arc::<str>::from),
            agent_color: result
                .agent_color
                .as_deref()
                .filter(|color| *color != "default")
                .map(Arc::<str>::from),
            restored_agent_def,
            agent_definitions,
            resume_restore_stores,
            restored_agent_type,
            restored_file_history,
        }
    }
}

/// UI-safe counterpart of official `fileHistoryRestoreStateFromLog(...)`.
///
/// This rebuilds only the in-memory state shape used by future query/tool
/// seams. It does not check feature flags, copy backup files, notify IDEs, or
/// write session/file-history records.
///
/// The typed subsystem now lives in `utils::file_history`
/// (`file_history_restore_state_from_log` + `copy_file_history_for_resume`);
/// when resume wiring lands, these raw `Value` snapshots deserialize into
/// `file_history::FileHistorySnapshot` (same camelCase serde shape) and feed
/// `AppState.file_history`.
pub fn file_history_restore_state_from_log(
    file_history_snapshots: &[Value],
    cwd: Option<&str>,
) -> Option<RestoredFileHistoryState> {
    if file_history_snapshots.is_empty() {
        return None;
    }

    let cwd = cwd.filter(|value| !value.is_empty());
    let mut snapshots = Vec::with_capacity(file_history_snapshots.len());
    let mut tracked_files = BTreeSet::new();

    for snapshot in file_history_snapshots {
        let mut restored_snapshot = snapshot.clone();
        if let Some(backups) = restored_snapshot
            .get_mut("trackedFileBackups")
            .and_then(Value::as_object_mut)
        {
            let original_backups = std::mem::take(backups);
            let mut restored_backups = Map::new();
            for (path, backup) in original_backups {
                let tracking_path = maybe_shorten_file_path(&path, cwd);
                tracked_files.insert(tracking_path.clone());
                restored_backups.insert(tracking_path, backup);
            }
            *backups = restored_backups;
        }
        snapshots.push(restored_snapshot);
    }

    Some(RestoredFileHistoryState {
        snapshot_sequence: snapshots.len(),
        snapshots,
        tracked_files: tracked_files.into_iter().collect(),
    })
}

fn maybe_shorten_file_path(file_path: &str, cwd: Option<&str>) -> String {
    let Some(cwd) = cwd else {
        return file_path.to_string();
    };
    let path = Path::new(file_path);
    if !path.is_absolute() {
        return file_path.to_string();
    }

    path.strip_prefix(Path::new(cwd))
        .map(|relative| relative.to_string_lossy().to_string())
        .unwrap_or_else(|_| file_path.to_string())
}

/// UI-safe counterpart of official context-collapse `restoreFromEntries(...)`.
/// The caller should replace any previous in-memory store with this result;
/// empty inputs intentionally clear stale commits/snapshots.
pub fn context_collapse_restore_state_from_log(
    commits: &[Value],
    snapshot: Option<&Value>,
) -> RestoredContextCollapseState {
    RestoredContextCollapseState {
        commits: commits.to_vec(),
        snapshot: snapshot.cloned(),
    }
}

/// Mirrors official `computeStandaloneAgentContext(...)` for resume metadata.
pub fn compute_standalone_agent_context(
    agent_name: Option<&str>,
    agent_color: Option<&str>,
) -> Option<RestoredStandaloneAgentContext> {
    let agent_name = agent_name.filter(|value| !value.is_empty());
    let agent_color = agent_color.filter(|value| !value.is_empty());
    if agent_name.is_none() && agent_color.is_none() {
        return None;
    }

    Some(RestoredStandaloneAgentContext {
        name: agent_name.unwrap_or_default().to_string(),
        color: agent_color
            .filter(|color| *color != "default")
            .map(str::to_string),
    })
}

/// Mirrors `extractTodosFromTranscript(...)` in `utils/sessionRestore.ts`.
/// Official Claude Code scans assistant messages from newest to oldest and
/// hydrates todos from the last `TodoWrite` tool_use input. Keep this as a
/// read-only payload seam for resume until the full AppState todo store exists.
pub fn extract_todos_from_transcript(messages: &[Value]) -> Vec<Value> {
    for message in messages.iter().rev() {
        if message.get("type").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let Some(blocks) = message
            .get("message")
            .and_then(|message| message.get("content"))
            .and_then(Value::as_array)
        else {
            continue;
        };

        let Some(tool_use) = blocks.iter().find(|block| {
            block.get("type").and_then(Value::as_str) == Some("tool_use")
                && block.get("name").and_then(Value::as_str) == Some(TODO_WRITE_TOOL_NAME)
        }) else {
            continue;
        };

        let Some(input) = tool_use.get("input").and_then(Value::as_object) else {
            return Vec::new();
        };
        // Maps to: CC `sessionRestore.ts:87-90` —
        // `TodoListSchema().safeParse(input.todos)`, falling back to `[]` on
        // failure. The rules (non-empty content/activeForm, closed status set)
        // belong to `utils/todo/types.rs`; this used to re-implement them here.
        let todos = input.get("todos").cloned().unwrap_or(Value::Null);
        return crate::utils::zod::safe_parse(
            crate::utils::todo::types::todo_list_schema(),
            &todos,
        )
        .ok()
        .and_then(|parsed| parsed.as_array().cloned())
        .unwrap_or_default();
    }

    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};
    use serde_json::json;

    #[test]
    fn restore_worktree_for_resume_and_exit_restored_worktree_match_official_cwd_flow() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let previous_cwd = std::env::current_dir().unwrap();
        let previous_original_cwd = crate::bootstrap::state::get_original_cwd();
        let previous_worktree = crate::utils::worktree::get_current_worktree_session();
        let root =
            std::env::temp_dir().join(format!("cometix-restore-worktree-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let root = std::fs::canonicalize(root).unwrap();
        let original = root.join("original");
        let worktree = root.join("worktree");
        std::fs::create_dir_all(&original).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();
        std::env::set_current_dir(&original).unwrap();
        crate::bootstrap::state::set_original_cwd(&original);
        crate::utils::worktree::restore_worktree_session(None);
        let session = crate::utils::worktree::WorktreeSession {
            original_cwd: original.to_string_lossy().to_string(),
            worktree_path: worktree.to_string_lossy().to_string(),
            worktree_name: "restored".to_string(),
            session_id: "session-worktree".to_string(),
            ..crate::utils::worktree::WorktreeSession::default()
        };

        let persisted = serde_json::to_value(&session).unwrap();
        restore_worktree_for_resume(Some(&persisted));
        assert_eq!(std::env::current_dir().unwrap(), worktree);
        assert_eq!(
            crate::utils::worktree::get_current_worktree_session(),
            Some(session.clone())
        );
        assert_eq!(crate::bootstrap::state::get_original_cwd(), worktree);

        exit_restored_worktree();
        assert_eq!(std::env::current_dir().unwrap(), original);
        assert_eq!(crate::bootstrap::state::get_original_cwd(), original);
        assert!(crate::utils::worktree::get_current_worktree_session().is_none());

        std::env::set_current_dir(previous_cwd).unwrap();
        crate::bootstrap::state::set_original_cwd(previous_original_cwd);
        crate::utils::worktree::restore_worktree_session(previous_worktree);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn restore_worktree_for_resume_keeps_fresh_startup_worktree_precedence() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let previous_cwd = std::env::current_dir().unwrap();
        let previous_original_cwd = crate::bootstrap::state::get_original_cwd();
        let previous_worktree = crate::utils::worktree::get_current_worktree_session();
        let root = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        crate::utils::session_storage::clear_session_metadata();
        let fresh = crate::utils::worktree::WorktreeSession {
            original_cwd: root.to_string_lossy().to_string(),
            worktree_path: root.join("fresh").to_string_lossy().to_string(),
            worktree_name: "fresh".to_string(),
            session_id: "fresh-session".to_string(),
            ..crate::utils::worktree::WorktreeSession::default()
        };
        crate::utils::worktree::restore_worktree_session(Some(fresh.clone()));
        let stale = serde_json::json!({
            "originalCwd": root,
            "worktreePath": root.join("stale"),
            "worktreeName": "stale",
            "sessionId": "stale-session"
        });

        restore_worktree_for_resume(Some(&stale));

        assert_eq!(
            crate::utils::worktree::get_current_worktree_session(),
            Some(fresh.clone())
        );
        assert_eq!(
            crate::utils::session_storage::get_current_session_metadata().worktree_session,
            Some(crate::utils::worktree::worktree_session_to_persisted_json(
                &fresh
            ))
        );
        crate::utils::session_storage::clear_session_metadata();
        std::env::set_current_dir(previous_cwd).unwrap();
        crate::bootstrap::state::set_original_cwd(previous_original_cwd);
        crate::utils::worktree::restore_worktree_session(previous_worktree);
    }

    #[test]
    fn restore_worktree_for_resume_marks_missing_directory_as_exited() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let previous_cwd = std::env::current_dir().unwrap();
        let previous_original_cwd = crate::bootstrap::state::get_original_cwd();
        let previous_worktree = crate::utils::worktree::get_current_worktree_session();
        let root =
            std::env::temp_dir().join(format!("cometix-missing-worktree-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let root = std::fs::canonicalize(root).unwrap();
        std::env::set_current_dir(&root).unwrap();
        crate::bootstrap::state::set_original_cwd(&root);
        crate::utils::worktree::restore_worktree_session(None);
        crate::utils::session_storage::clear_session_metadata();
        let persisted = serde_json::json!({
            "originalCwd": root,
            "worktreePath": root.join("deleted"),
            "worktreeName": "deleted",
            "sessionId": "session-worktree"
        });
        crate::utils::session_storage::restore_session_metadata(
            &crate::utils::session_storage::SessionMetadataCache {
                session_id: "session-worktree".to_string(),
                worktree_session: Some(persisted.clone()),
                ..crate::utils::session_storage::SessionMetadataCache::default()
            },
        );

        restore_worktree_for_resume(Some(&persisted));

        assert_eq!(std::env::current_dir().unwrap(), root);
        assert_eq!(
            crate::utils::session_storage::get_current_session_metadata().worktree_session,
            Some(Value::Null)
        );
        assert!(crate::utils::worktree::get_current_worktree_session().is_none());

        crate::utils::session_storage::clear_session_metadata();
        std::env::set_current_dir(previous_cwd).unwrap();
        crate::bootstrap::state::set_original_cwd(previous_original_cwd);
        crate::utils::worktree::restore_worktree_session(previous_worktree);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn file_history_restore_state_rebuilds_tracked_files_without_copying_backups() {
        let snapshots = vec![json!({
            "messageId": "message-1",
            "trackedFileBackups": {
                "/tmp/project/src/lib.rs": {
                    "backupFileName": "src-lib-v1",
                    "version": 1,
                    "backupTime": "2026-06-29T00:00:00.000Z"
                },
                "/var/tmp/outside.txt": {
                    "backupFileName": null,
                    "version": 1,
                    "backupTime": "2026-06-29T00:00:00.000Z"
                }
            },
            "timestamp": "2026-06-29T00:00:00.000Z"
        })];

        let restored = file_history_restore_state_from_log(&snapshots, Some("/tmp/project"))
            .expect("snapshots should restore");

        assert_eq!(restored.snapshot_sequence, 1);
        assert_eq!(
            restored.tracked_files,
            vec!["/var/tmp/outside.txt".to_string(), "src/lib.rs".to_string()]
        );
        let backups = restored.snapshots[0]
            .get("trackedFileBackups")
            .and_then(Value::as_object)
            .expect("tracked backups should remain object");
        assert!(backups.contains_key("src/lib.rs"));
        assert!(backups.contains_key("/var/tmp/outside.txt"));
        assert_eq!(backups.get("/tmp/project/src/lib.rs"), None);
    }

    #[test]
    fn context_collapse_restore_state_empty_input_clears_stale_store_shape() {
        let restored = context_collapse_restore_state_from_log(&[], None);

        assert!(restored.commits.is_empty());
        assert!(restored.snapshot.is_none());
    }

    #[test]
    fn standalone_agent_context_matches_official_default_color_rule() {
        assert_eq!(
            compute_standalone_agent_context(Some("worker"), Some("default")),
            Some(RestoredStandaloneAgentContext {
                name: "worker".to_string(),
                color: None,
            })
        );
        assert_eq!(
            compute_standalone_agent_context(None, Some("green")),
            Some(RestoredStandaloneAgentContext {
                name: String::new(),
                color: Some("green".to_string()),
            })
        );
        assert_eq!(compute_standalone_agent_context(None, None), None);
    }

    #[test]
    fn extract_todos_from_transcript_uses_latest_todowrite_like_official() {
        let messages = vec![
            json!({
                "type": "assistant",
                "message": {"content": [{
                    "type": "tool_use",
                    "name": "TodoWrite",
                    "input": {"todos": [{"content": "old", "status": "completed", "activeForm": "Reviewing old task"}]}
                }]}
            }),
            json!({
                "type": "user",
                "message": {"content": "ignored"}
            }),
            json!({
                "type": "assistant",
                "message": {"content": [{
                    "type": "tool_use",
                    "name": "TodoWrite",
                    "input": {"todos": [{"content": "new", "status": "pending", "activeForm": "Reviewing new task"}]}
                }]}
            }),
        ];

        let todos = extract_todos_from_transcript(&messages);
        assert_eq!(todos.len(), 1);
        assert_eq!(todos[0].get("content").and_then(Value::as_str), Some("new"));
        assert_eq!(
            todos[0].get("activeForm").and_then(Value::as_str),
            Some("Reviewing new task")
        );
    }

    #[test]
    fn extract_todos_from_transcript_rejects_invalid_latest_todolist_like_official() {
        let messages = vec![
            json!({
                "type": "assistant",
                "message": {"content": [{
                    "type": "tool_use",
                    "name": "TodoWrite",
                    "input": {"todos": [{"content": "old", "status": "completed", "activeForm": "Done"}]}
                }]}
            }),
            json!({
                "type": "assistant",
                "message": {"content": [{
                    "type": "tool_use",
                    "name": "TodoWrite",
                    "input": {"todos": [{"content": "new", "status": "pending"}]}
                }]}
            }),
        ];

        assert!(extract_todos_from_transcript(&messages).is_empty());
    }

    #[test]
    fn extract_todos_from_transcript_returns_empty_for_non_object_input() {
        let messages = vec![json!({
            "type": "assistant",
            "message": {"content": [{
                "type": "tool_use",
                "name": "TodoWrite",
                "input": null
            }]}
        })];

        assert!(extract_todos_from_transcript(&messages).is_empty());
    }

    fn test_agent(
        agent_type: &str,
        model: Option<&str>,
    ) -> crate::tools::agent_tool::load_agents_dir::AgentDefinition {
        let mut agent = crate::tools::agent_tool::load_agents_dir::AgentDefinition::new(
            agent_type,
            "test agent",
            crate::tools::agent_tool::load_agents_dir::AgentDefinitionSource::ProjectSettings,
        );
        agent.model = model.map(str::to_string);
        agent
    }

    #[test]
    fn restore_agent_from_session_matches_official_initial_agent_precedence() {
        let current = Arc::new(test_agent("cli-agent", Some("opus")));
        let definitions = crate::tools::agent_tool::load_agents_dir::AgentDefinitionsResult {
            active_agents: vec![test_agent("resume-agent", Some("sonnet"))],
            all_agents: vec![test_agent("resume-agent", Some("sonnet"))],
            failed_files: Vec::new(),
            allowed_agent_types: None,
        };

        let (resolved, resumed_type) =
            restore_agent_from_session(Some("resume-agent"), Some(current.clone()), &definitions);

        assert_eq!(resolved.as_deref(), Some(current.as_ref()));
        assert_eq!(resumed_type, None);
    }

    #[test]
    fn process_resumed_conversation_switches_and_adopts_the_selected_transcript() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _write = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let previous_session_id = crate::bootstrap::state::get_session_id();
        let previous_project_dir = crate::bootstrap::state::get_session_project_dir();
        let root = std::env::temp_dir().join(format!(
            "cometix-process-resume-adopt-{}",
            uuid::Uuid::new_v4()
        ));
        let project_dir = root.join("project-dir");
        let session_id = "process-adopt-session";
        let path = project_dir.join(format!("{session_id}.jsonl"));
        std::fs::create_dir_all(&project_dir).unwrap();
        let entry = json!({
            "type":"user",
            "uuid":"process-adopt-root",
            "parentUuid":null,
            "timestamp":"2026-07-12T00:00:00Z",
            "sessionId":session_id,
            "message":{"role":"user","content":"resume and adopt"}
        });
        std::fs::write(&path, format!("{entry}\n")).unwrap();
        crate::utils::session_storage::clear_session_metadata();
        let target = crate::commands::resume::ResumeTarget {
            session_id: session_id.to_string(),
            project_path: Some("/logical/project".to_string()),
            entries: vec![entry],
            turn_interruption_state: crate::utils::conversation::TurnInterruptionState::None,
            metadata: crate::commands::resume::ResumeMetadata {
                session_id: Some(session_id.to_string()),
                custom_title: Some("Adopted through process".to_string()),
                full_path: Some(path.display().to_string()),
                ..crate::commands::resume::ResumeMetadata::default()
            },
            entrypoint: Some(crate::types::command::ResumeEntrypoint::CliFlag),
        };

        let loaded = ResumeLoadResult::try_from(&target).unwrap();
        process_resumed_conversation(
            loaded,
            None,
            Arc::new(crate::tools::agent_tool::load_agents_dir::AgentDefinitionsResult::default()),
        )
        .unwrap();
        assert_eq!(crate::bootstrap::state::get_session_id(), session_id);
        assert_eq!(
            crate::bootstrap::state::get_session_project_dir().as_deref(),
            Some(project_dir.as_path())
        );
        assert_eq!(
            crate::utils::session_storage::current_session_file_for_test().as_deref(),
            Some(path.as_path())
        );
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("Adopted through process")
        );

        crate::utils::session_storage::clear_session_metadata();
        crate::bootstrap::state::switch_session(previous_session_id, previous_project_dir);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn processed_resume_restores_agent_object_and_model_before_mount() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let previous_override = crate::bootstrap::state::get_main_loop_model_override();
        crate::bootstrap::state::set_main_loop_model_override(None);

        let resume_agent = test_agent("resume-agent", Some("sonnet"));
        let definitions = Arc::new(
            crate::tools::agent_tool::load_agents_dir::AgentDefinitionsResult {
                active_agents: vec![resume_agent.clone()],
                all_agents: vec![resume_agent],
                failed_files: Vec::new(),
                allowed_agent_types: None,
            },
        );
        let target = crate::commands::resume::ResumeTarget {
            session_id: "resume-agent-session".to_string(),
            project_path: None,
            entries: vec![json!({
                "type": "user",
                "timestamp": "2026-07-12T00:00:00.000Z",
                "message": {"role": "user", "content": "resume me"}
            })],
            turn_interruption_state: crate::utils::conversation::TurnInterruptionState::None,
            metadata: crate::commands::resume::ResumeMetadata {
                agent_setting: Some("resume-agent".to_string()),
                agent_name: Some("Restored Name".to_string()),
                agent_color: Some("default".to_string()),
                ..crate::commands::resume::ResumeMetadata::default()
            },
            entrypoint: Some(crate::types::command::ResumeEntrypoint::CliFlag),
        };

        let loaded = ResumeLoadResult::try_from(&target).expect("resume should load");
        let processed =
            process_resumed_conversation(loaded, None, definitions).expect("resume should process");
        let mut state = crate::state::app_state_store::AppState {
            // Saved/env state does not have bootstrap-override identity and
            // therefore must not suppress the resumed agent model.
            main_loop_model: Some("saved-model".to_string()),
            ..crate::state::app_state_store::AppState::default()
        };
        processed.apply_to_app_state(&mut state);

        assert_eq!(
            processed
                .restored_agent_def
                .as_ref()
                .map(|agent| agent.agent_type.as_str()),
            Some("resume-agent")
        );
        assert_eq!(state.agent.as_deref(), Some("resume-agent"));
        assert_eq!(state.main_loop_model.as_deref(), Some("sonnet"));
        assert_eq!(
            crate::bootstrap::state::get_main_loop_model_override(),
            Some(Some(
                crate::utils::model::model::parse_user_specified_model("sonnet")
            ))
        );
        assert_eq!(
            state
                .standalone_agent_context
                .as_ref()
                .map(|context| (context.name.as_str(), context.color.as_deref())),
            Some(("Restored Name", None))
        );

        crate::bootstrap::state::set_main_loop_model_override(Some(Some(String::new())));
        let mut empty_override_state = crate::state::app_state_store::AppState {
            main_loop_model: Some("saved-model".to_string()),
            ..crate::state::app_state_store::AppState::default()
        };
        processed.apply_to_app_state(&mut empty_override_state);
        assert_eq!(
            empty_override_state.main_loop_model.as_deref(),
            Some("sonnet"),
            "CC's falsey empty-string override must not suppress resume metadata"
        );

        crate::bootstrap::state::set_main_loop_model_override(Some(Some(
            "explicit-cli-model".to_string(),
        )));
        let mut explicit_state = crate::state::app_state_store::AppState {
            main_loop_model: Some("explicit-cli-model".to_string()),
            ..crate::state::app_state_store::AppState::default()
        };
        processed.apply_to_app_state(&mut explicit_state);
        assert_eq!(
            explicit_state.main_loop_model.as_deref(),
            Some("explicit-cli-model"),
            "an explicit bootstrap model must retain precedence over resume metadata"
        );

        crate::bootstrap::state::set_main_loop_model_override(previous_override);
    }
}
