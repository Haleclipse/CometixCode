//! Incremental port of official `TeamCreateTool`.
//! Official `userFacingName()` is empty, so assistant tool-use chrome is hidden.
//! Real execution writes team files, task directories, analytics, and app state;
//! Cometix now writes the official team-file shape for pane-backed teammate
//! discovery and the leader's AppState teamContext, while analytics events
//! (`tengu_team_created`) remain disabled repo-wide.

pub mod prompt;
pub mod ui;

/// Maps to: CC `utils/agentSwarmsEnabled.ts` `isAgentSwarmsEnabled()` gate.
pub fn is_team_create_tool_enabled() -> bool {
    crate::utils::agent_swarms_enabled::is_agent_swarms_enabled()
}

/// Maps to: CC `TeamCreateTool` metadata.
/// Maps to: CC `TeamCreateTool.ts:37-49` `inputSchema`.
pub fn input_schema() -> &'static crate::utils::zod::Schema {
    static SCHEMA: std::sync::OnceLock<crate::utils::zod::Schema> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| {
        use crate::utils::zod as zod;
        zod::strict_object(vec![
            (
                "team_name",
                zod::string().describe("Name for the new team to create."),
            ),
            (
                "description",
                zod::string().optional().describe("Team description/purpose."),
            ),
            (
                "agent_type",
                zod::string().optional().describe(
                    "Type/role of the team lead (e.g., \"researcher\", \"test-runner\"). Used for team file and inter-agent coordination.",
                ),
            ),
        ])
    })
}

pub fn team_create_tool_schema() -> crate::types::tools::Tool {
    crate::types::tools::Tool {
        name: prompt::TEAM_CREATE_TOOL_NAME.to_string(),
        description: prompt::get_prompt(),
        input_schema: crate::utils::zod_to_json_schema::zod_to_json_schema(input_schema()),
        ..Default::default()
    }
}

/// CC `tools/TeamCreateTool/TeamCreateTool.ts` output type (:52-56).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TeamCreateOutput {
    pub(crate) team_name: String,
    pub(crate) team_file_path: String,
    pub(crate) lead_agent_id: String,
}

/// Maps to: CC `TeamCreateTool.ts:64-72` `generateUniqueTeamName` — one
/// slug draw, no retry loop and no existence re-check on the fresh slug.
fn generate_unique_team_name(provided_name: &str) -> String {
    if !crate::utils::swarm::team_helpers::team_record_exists(provided_name) {
        return provided_name.to_string();
    }
    crate::utils::words::generate_word_slug()
}

/// Create the single in-memory team record.
/// Maps to: CC `tools/TeamCreateTool/TeamCreateTool.ts` `call` (:128), which
/// persists via `utils/swarm/teamHelpers.ts` `writeTeamFileAsync` (:175).
pub(crate) fn team_create_output(
    input: &serde_json::Value,
    context: &crate::tool::ToolUseContext,
) -> Result<TeamCreateOutput, String> {
    use crate::utils::swarm::team_helpers::{
        TEAM_TOOL_STATE, create_team_record, write_team_record_result,
    };

    // CC destructures the schema-validated fields verbatim (:130) — no
    // trimming; validateInput already rejected a blank team_name.
    let team_name = input
        .get("team_name")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();

    // CC reads the one-team-per-leader guard from AppState
    // (`appState.teamContext?.teamName`, :133-140); the in-memory record
    // store is the fallback carrier when no store is mounted — same
    // resolution order as TeamDelete's team-name lookup.
    let existing_team_name = context
        .app_store
        .store
        .as_ref()
        .or(context.app_store.tasks_store.as_ref())
        .and_then(|store| {
            store
                .get()
                .team_context
                .as_ref()
                .map(|tc| tc.team_name.clone())
        })
        .or_else(|| {
            TEAM_TOOL_STATE
                .lock()
                .unwrap()
                .as_ref()
                .map(|existing| existing.team_name.clone())
        });
    if let Some(existing_team_name) = existing_team_name {
        return Err(format!(
            "Already leading team \"{}\". A leader can only manage one team at a time. Use TeamDelete to end the current team before creating a new one.",
            existing_team_name
        ));
    }

    let final_team_name = generate_unique_team_name(&team_name);

    // CC `agent_type || TEAM_LEAD_NAME` (:147) — JS falsy: only the empty
    // string (or absence) falls back, nothing is trimmed.
    let lead_agent_type = input
        .get("agent_type")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| crate::utils::swarm::constants::TEAM_LEAD_NAME.to_string());
    // CC `cwd: getCwd()` (:171) — the live shell cwd, not the original cwd.
    let cwd = context.effective_cwd().display().to_string();
    // CC passes `description` through untouched (:159).
    let description = input
        .get("description")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    // CC `parseUserSpecifiedModel(appState.mainLoopModelForSession ??
    // appState.mainLoopModel ?? getDefaultMainLoopModel())` (:149-153).
    let app_state = context
        .app_store
        .store
        .as_ref()
        .or(context.app_store.tasks_store.as_ref())
        .map(|store| store.get());
    let lead_model_raw = app_state
        .as_ref()
        .and_then(|state| state.main_loop_model_for_session.clone())
        .or_else(|| {
            app_state
                .as_ref()
                .and_then(|state| state.main_loop_model.clone())
        })
        .unwrap_or_else(crate::utils::model::model::get_default_main_loop_model);
    let lead_model = Some(crate::utils::model::model::parse_user_specified_model(
        &lead_model_raw,
    ));
    let record = create_team_record(
        final_team_name.clone(),
        description,
        Some(lead_agent_type.clone()),
        lead_model,
        cwd.clone(),
    );
    let team_file_path = record.team_file_path.clone();
    let lead_agent_id = record.lead_agent_id.clone();
    write_team_record_result(record)?;
    crate::utils::swarm::team_helpers::register_team_for_session_cleanup(&final_team_name);

    let task_list_id = crate::utils::swarm::team_helpers::sanitize_name(&final_team_name);
    crate::utils::tasks::reset_task_list(&task_list_id)
        .map_err(|err| format!("Failed to reset team task list {task_list_id}: {err}"))?;
    crate::utils::tasks::ensure_tasks_dir(&task_list_id)
        .map_err(|err| format!("Failed to create team task directory {task_list_id}: {err}"))?;
    crate::utils::tasks::set_leader_team_name(&task_list_id);

    // Maps to: CC `TeamCreateTool.ts:194-212` — the leader's AppState gains
    // the team context with the lead registered as its first teammate entry.
    if let Some(store) = context
        .app_store
        .store
        .as_ref()
        .or(context.app_store.tasks_store.as_ref())
    {
        let spawned_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis() as u64)
            .unwrap_or_default();
        let color =
            crate::utils::swarm::teammate_layout_manager::assign_teammate_color(&lead_agent_id);
        let mut teammates = std::collections::BTreeMap::new();
        teammates.insert(
            lead_agent_id.clone(),
            crate::hooks::use_inbox_poller::InboxPollerTeammateInfo {
                name: crate::utils::swarm::constants::TEAM_LEAD_NAME.to_string(),
                agent_type: Some(lead_agent_type),
                color: Some(color.official_name().to_string()),
                tmux_session_name: Some(String::new()),
                tmux_pane_id: Some(String::new()),
                cwd: Some(cwd),
                worktree_path: None,
                spawned_at: Some(spawned_at),
                backend_type: None,
            },
        );
        let team_context = crate::hooks::use_inbox_poller::InboxPollerTeamContext {
            team_name: final_team_name.clone(),
            team_file_path: team_file_path.clone(),
            lead_agent_id: lead_agent_id.clone(),
            self_agent_id: None,
            self_agent_name: None,
            is_leader: None,
            self_agent_color: None,
            teammates,
        };
        store.replace_with(move |state| {
            state.team_context = Some(std::sync::Arc::new(team_context));
        });
    }

    Ok(TeamCreateOutput {
        team_name: final_team_name,
        team_file_path,
        lead_agent_id,
    })
}

pub(crate) fn team_create_output_json(output: &TeamCreateOutput) -> serde_json::Value {
    serde_json::json!({
        "team_name": &output.team_name,
        "team_file_path": &output.team_file_path,
        "lead_agent_id": &output.lead_agent_id,
    })
}

/// Behavioral half of CC `TeamCreateTool` — dispatched via `crate::tool::ToolCall`.
pub(crate) struct TeamCreateTool;

impl crate::tool::ToolCall for TeamCreateTool {
    fn name(&self) -> &'static str {
        "TeamCreate"
    }

    /// Maps to: CC `TeamCreateTool.ts:111-113` `async prompt() { return
    /// getPrompt() }` — same source the wire schema renders eagerly.
    fn prompt(
        &self,
        _tool: &crate::types::tools::Tool,
        _options: &crate::tool::ToolPromptOptions<'_>,
    ) -> String {
        prompt::get_prompt()
    }

    /// Maps to: CC `TeamCreateTool.ts:88-90` `isEnabled()`.
    fn is_enabled(&self) -> bool {
        is_team_create_tool_enabled()
    }

    /// Maps to: CC `TeamCreateTool.ts:76` `searchHint`.
    fn search_hint(&self) -> Option<&'static str> {
        Some("create a multi-agent swarm team")
    }

    /// Maps to: CC `TeamCreateTool.ts:78` `shouldDefer: true`.
    fn should_defer(&self) -> bool {
        true
    }

    /// Maps to: CC `TeamCreateTool.ts:80-82` `userFacingName() => ''` — the
    /// empty name hides the assistant tool-use chrome.
    fn user_facing_name(&self, _args: Option<&serde_json::Value>) -> String {
        String::new()
    }

    /// Maps to: CC `TeamCreateTool.ts:92-94` `toAutoClassifierInput(input)`.
    fn to_auto_classifier_input(&self, args: &serde_json::Value) -> String {
        args.get("team_name")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string()
    }

    /// Maps to: CC `TeamCreateTool.ts:96-105` `validateInput(...)`.
    fn validate_input(
        &self,
        args: &serde_json::Value,
        _context: &crate::tool::ToolUseContext,
    ) -> crate::tool::ValidationResult {
        let team_name = args
            .get("team_name")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        if team_name.trim().is_empty() {
            return crate::tool::ValidationResult::error("team_name is required for TeamCreate", 9);
        }
        crate::tool::ValidationResult::Ok
    }

    fn call<'a>(
        &'a self,
        args: &'a serde_json::Value,
        request: &'a crate::types::permissions::PermissionRequest,
        context: &'a crate::tool::ToolUseContext,
        _can_use_tool: Option<crate::tool::CanUseToolFn<'a>>,
        _parent_message: Option<&'a crate::types::message::AssistantMessage>,
        _on_progress: Option<crate::tool::ToolCallProgressFn<'a>>,
    ) -> futures::future::BoxFuture<'a, crate::tool::ToolResult> {
        Box::pin(async move {
            match team_create_output(args, context) {
                Ok(output) => crate::tool::ToolResult {
                    data: crate::tool::ToolOutput::TeamCreate(output),
                    new_messages: Vec::new(),
                },
                Err(error) => {
                    let _ = request;
                    crate::tool::ToolResult {
                        data: crate::tool::ToolOutput::Composed {
                            content: error,
                            status: crate::types::message::ToolResultStatus::Error,
                        },
                        new_messages: Vec::new(),
                    }
                }
            }
        })
    }

    /// Maps to: CC `tools/TeamCreateTool/TeamCreateTool.ts`
    /// `mapToolResultToToolResultBlockParam` (:115-126).
    fn map_tool_result_to_tool_result_block_param(
        &self,
        data: &crate::tool::ToolOutput,
        _tool_use_id: &str,
    ) -> (String, crate::types::message::ToolResultStatus) {
        match data {
            crate::tool::ToolOutput::TeamCreate(output) => (
                team_create_output_json(output).to_string(),
                crate::types::message::ToolResultStatus::Success,
            ),
            crate::tool::ToolOutput::Composed {
                content, status, ..
            } => (content.clone(), *status),
            _ => (
                "<tool_use_error>TeamCreate returned an unexpected output variant</tool_use_error>"
                    .to_string(),
                crate::types::message::ToolResultStatus::Error,
            ),
        }
    }

    /// Maps to: CC recording this tool's `Output` (the `call()` data) as the
    /// message's `toolUseResult`.
    fn tool_use_result(&self, data: &crate::tool::ToolOutput) -> Option<serde_json::Value> {
        match data {
            crate::tool::ToolOutput::TeamCreate(output) => Some(team_create_output_json(output)),
            _ => None,
        }
    }

    // CC `TeamCreateTool` defines no `renderToolResultMessage` — the render
    // layer hides success rows by name (`success_tool_result_is_nonvisual`).
}

#[cfg(test)]
mod tests {
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    fn unique_config_dir(prefix: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "cometix-team-create-{prefix}-{}",
            uuid::Uuid::new_v4().simple()
        ))
    }

    #[test]
    fn team_create_tool_schema_matches_official_input_shape() {
        let schema = super::team_create_tool_schema();
        assert_eq!(schema.name, "TeamCreate");
        assert_eq!(
            schema.input_schema.get("required"),
            Some(&serde_json::json!(["team_name"]))
        );
        assert!(
            schema
                .input_schema
                .pointer("/properties/agent_type/description")
                .and_then(|value| value.as_str())
                .is_some_and(|description| description.contains("team lead"))
        );
        assert!(schema.description.contains("Team Workflow"));
    }

    #[test]
    fn team_create_generates_unique_team_name_when_file_already_exists() {
        let _team_lock = crate::utils::swarm::team_helpers::TEST_TEAM_HELPERS_LOCK
            .lock()
            .unwrap();
        let _task_lock = crate::utils::tasks::TASK_TOOL_TEST_LOCK.lock().unwrap();
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::swarm::team_helpers::clear_team_tool_state_for_test();
        crate::bootstrap::state::clear_session_created_teams();
        crate::utils::tasks::clear_leader_team_name();
        let root = unique_config_dir("unique");
        let _config_guard = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root);
        let _io_guard = EnvVarGuard::set("COMETIX_TEST_TEAM_FILE_IO", "1");
        let existing_dir = root.join("teams").join("alpha");
        std::fs::create_dir_all(&existing_dir).unwrap();
        std::fs::write(
            existing_dir.join("config.json"),
            serde_json::json!({
                "name": "alpha",
                "createdAt": 1,
                "leadAgentId": "team-lead@alpha",
                "members": []
            })
            .to_string(),
        )
        .unwrap();

        let output = super::team_create_output(
            &serde_json::json!({
                "team_name": "alpha",
                "description": "new work"
            }),
            &crate::tool::ToolUseContext::default(),
        )
        .unwrap();

        assert_ne!(output.team_name, "alpha");
        assert!(output.lead_agent_id.starts_with("team-lead@"));
        assert!(std::path::Path::new(&output.team_file_path).exists());
        assert!(crate::bootstrap::state::get_session_created_teams().contains(&output.team_name));
        crate::utils::swarm::team_helpers::clear_team_tool_state_for_test();
        crate::bootstrap::state::clear_session_created_teams();
        crate::utils::tasks::clear_leader_team_name();
        let _ = std::fs::remove_dir_all(root);
    }

    /// CC `TeamCreateTool.ts:194-212` — a mounted AppState gains the team
    /// context with the lead registered as its first teammate entry.
    #[test]
    fn team_create_writes_team_context_with_lead_as_first_teammate() {
        let _team_lock = crate::utils::swarm::team_helpers::TEST_TEAM_HELPERS_LOCK
            .lock()
            .unwrap();
        let _task_lock = crate::utils::tasks::TASK_TOOL_TEST_LOCK.lock().unwrap();
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::swarm::team_helpers::clear_team_tool_state_for_test();
        crate::bootstrap::state::clear_session_created_teams();
        crate::utils::tasks::clear_leader_team_name();
        let root = unique_config_dir("appstate");
        let _config_guard = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root);
        let _io_guard = EnvVarGuard::set("COMETIX_TEST_TEAM_FILE_IO", "1");

        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        let context = crate::tool::ToolUseContext::default().with_app_store(store.clone());
        let output = super::team_create_output(
            &serde_json::json!({
                "team_name": "beta",
                "description": "review work"
            }),
            &context,
        )
        .unwrap();

        let state = store.get();
        let team_context = state
            .team_context
            .as_ref()
            .expect("TeamCreate must populate AppState teamContext");
        assert_eq!(team_context.team_name, output.team_name);
        assert_eq!(team_context.lead_agent_id, output.lead_agent_id);
        let lead = team_context
            .teammates
            .get(&output.lead_agent_id)
            .expect("lead must be the first teammate entry");
        assert_eq!(lead.name, crate::utils::swarm::constants::TEAM_LEAD_NAME);
        assert!(lead.color.is_some());
        assert!(lead.spawned_at.is_some());

        crate::utils::swarm::team_helpers::clear_team_tool_state_for_test();
        crate::bootstrap::state::clear_session_created_teams();
        crate::utils::tasks::clear_leader_team_name();
        crate::utils::swarm::teammate_layout_manager::clear_teammate_colors();
        let _ = std::fs::remove_dir_all(root);
    }

    /// CC `TeamCreateTool.ts:133-140` reads the one-team-per-leader guard
    /// from `appState.teamContext?.teamName` — AppState decides even when the
    /// in-memory record store is empty.
    #[test]
    fn team_create_rejects_when_app_state_already_has_a_team() {
        let _team_lock = crate::utils::swarm::team_helpers::TEST_TEAM_HELPERS_LOCK
            .lock()
            .unwrap();
        let _task_lock = crate::utils::tasks::TASK_TOOL_TEST_LOCK.lock().unwrap();
        crate::utils::swarm::team_helpers::clear_team_tool_state_for_test();

        let mut initial = crate::state::app_state_store::AppState::default();
        initial.team_context = Some(std::sync::Arc::new(
            crate::hooks::use_inbox_poller::InboxPollerTeamContext {
                team_name: "existing".to_string(),
                team_file_path: String::new(),
                lead_agent_id: "team-lead@existing".to_string(),
                self_agent_id: None,
                self_agent_name: None,
                is_leader: None,
                self_agent_color: None,
                teammates: std::collections::BTreeMap::new(),
            },
        ));
        let store = crate::state::store::AppStore::new(initial, None);
        let context = crate::tool::ToolUseContext::default().with_app_store(store);

        let error = super::team_create_output(&serde_json::json!({"team_name": "gamma"}), &context)
            .unwrap_err();
        assert!(error.contains("Already leading team \"existing\""));

        crate::utils::swarm::team_helpers::clear_team_tool_state_for_test();
    }

    #[test]
    fn team_create_resets_task_list_and_sets_leader_task_context() {
        let _team_lock = crate::utils::swarm::team_helpers::TEST_TEAM_HELPERS_LOCK
            .lock()
            .unwrap();
        let _task_lock = crate::utils::tasks::TASK_TOOL_TEST_LOCK.lock().unwrap();
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::swarm::team_helpers::clear_team_tool_state_for_test();
        crate::bootstrap::state::clear_session_created_teams();
        crate::utils::tasks::clear_leader_team_name();
        let root = unique_config_dir("tasks");
        let _config_guard = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root);
        let _list_guard = EnvVarGuard::set("CLAUDE_CODE_TASK_LIST_ID", "alpha-team");
        crate::utils::tasks::TASK_TOOL_STORE.lock().unwrap().push(
            crate::utils::tasks::TaskRecord {
                id: "1".to_string(),
                subject: "stale".to_string(),
                description: String::new(),
                active_form: None,
                status: "pending".to_string(),
                owner: None,
                blocks: Vec::new(),
                blocked_by: Vec::new(),
                metadata: None,
            },
        );
        let _io_guard = EnvVarGuard::set("COMETIX_TEST_TEAM_FILE_IO", "1");
        let stale_dir = crate::utils::tasks::get_tasks_dir("alpha-team");
        std::fs::create_dir_all(&stale_dir).unwrap();
        std::fs::write(stale_dir.join("4.json"), "{}").unwrap();

        let output = super::team_create_output(
            &serde_json::json!({
                "team_name": "Alpha Team"
            }),
            &crate::tool::ToolUseContext::default(),
        )
        .unwrap();

        assert_eq!(output.team_name, "Alpha Team");
        assert_eq!(crate::utils::tasks::get_task_list_id(), "alpha-team");
        assert!(
            crate::utils::tasks::TASK_TOOL_STORE
                .lock()
                .unwrap()
                .is_empty()
        );
        assert!(!stale_dir.join("4.json").exists());
        assert!(stale_dir.join(".highwatermark").exists());
        assert!(crate::bootstrap::state::get_session_created_teams().contains(&output.team_name));
        crate::utils::swarm::team_helpers::clear_team_tool_state_for_test();
        crate::bootstrap::state::clear_session_created_teams();
        crate::utils::tasks::clear_leader_team_name();
        let _ = std::fs::remove_dir_all(root);
    }
}
