//! Incremental port of official `TeamDeleteTool`.
//! Official `userFacingName()` is empty and the success renderer returns null;
//! Cometix keeps that invisible main-screen contract while cleaning the
//! official team/task directories when file-backed team state is enabled.

pub mod prompt;
pub mod ui;

/// Maps to: CC `TeamDeleteTool.isEnabled()` via `isAgentSwarmsEnabled()`.
pub fn is_team_delete_tool_enabled() -> bool {
    crate::utils::agent_swarms_enabled::is_agent_swarms_enabled()
}

/// Maps to: CC `TeamDeleteTool` metadata.
/// Maps to: CC `TeamDeleteTool.ts:21` `inputSchema` — `z.strictObject({})`.
pub fn input_schema() -> &'static crate::utils::zod::Schema {
    static SCHEMA: std::sync::OnceLock<crate::utils::zod::Schema> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| crate::utils::zod::strict_object(vec![]))
}

pub fn team_delete_tool_schema() -> crate::types::tools::Tool {
    crate::types::tools::Tool {
        name: prompt::TEAM_DELETE_TOOL_NAME.to_string(),
        description: prompt::get_prompt(),
        input_schema: crate::utils::zod_to_json_schema::zod_to_json_schema(input_schema()),
        ..Default::default()
    }
}

/// CC `tools/TeamDeleteTool/TeamDeleteTool.ts` output type (:24-28).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TeamDeleteOutput {
    pub(crate) success: bool,
    pub(crate) message: String,
    pub(crate) team_name: Option<String>,
}

/// Clear the in-memory team record.
/// Maps to: CC `tools/TeamDeleteTool/TeamDeleteTool.ts` `call` (:71), which
/// cleans team dirs via `utils/swarm/teamHelpers.ts`.
/// Maps to: CC `TeamDeleteTool.ts` active non-lead member guard.
fn active_non_lead_members(
    team: &crate::utils::swarm::team_helpers::TeamRecord,
) -> Option<Vec<String>> {
    let active = team
        .members
        .iter()
        .filter(|member| member.name != crate::utils::swarm::constants::TEAM_LEAD_NAME)
        .filter(|member| member.is_active != Some(false))
        .map(|member| member.name.clone())
        .collect::<Vec<_>>();
    (!active.is_empty()).then_some(active)
}

pub(crate) fn team_delete_output(
    context: &crate::tool::ToolUseContext,
) -> Result<TeamDeleteOutput, String> {
    use crate::utils::swarm::team_helpers::{TEAM_TOOL_STATE, cleanup_team_directories};

    // CC reads the team name from AppState (`appState.teamContext?.teamName`,
    // :73-74); the in-memory record store is the Rust carrier behind it.
    let store = context
        .app_store
        .store
        .as_ref()
        .or(context.app_store.tasks_store.as_ref());
    let team_name = store
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
                .map(|team| team.team_name.clone())
        });

    if let Some(team_name) = team_name.as_deref() {
        // CC re-reads the team file from disk to count active members
        // (:78-99) — teammate processes update `isActive` there, so the
        // on-disk record is authoritative, not the leader's memory. The
        // memory-first helper would hand back the leader's stale copy; only
        // `read_team_file` (CC `readTeamFile`) sees a teammate's
        // `isActive=false`.
        if let Some(team) = crate::utils::swarm::team_helpers::read_team_file(team_name) {
            if let Some(active_members) = active_non_lead_members(&team) {
                return Ok(TeamDeleteOutput {
                    success: false,
                    message: format!(
                        "Cannot cleanup team with {} active member(s): {}. Use requestShutdown to gracefully terminate teammates first.",
                        active_members.len(),
                        active_members.join(", ")
                    ),
                    team_name: Some(team_name.to_string()),
                });
            }
        }
        // CC `await cleanupTeamDirectories(teamName)` (:101) — a failure
        // throws into the generic tool error path, no soft-failure data.
        cleanup_team_directories(team_name)?;
        crate::utils::swarm::team_helpers::unregister_team_for_session_cleanup(team_name);
        // CC clears colors only after a successful cleanup (:106).
        crate::utils::swarm::teammate_layout_manager::clear_teammate_colors();
        // CC `clearLeaderTeamName()` runs inside the teamName branch (:109).
        crate::utils::tasks::clear_leader_team_name();
        TEAM_TOOL_STATE.lock().unwrap().take();
    }

    // CC clears teamContext and the inbox unconditionally (:118-124), even
    // when no team name was found.
    if let Some(store) = store {
        store.replace_with(|state| {
            state.team_context = None;
            state.inbox =
                std::sync::Arc::new(crate::hooks::use_inbox_poller::InboxState::default());
        });
    }

    Ok(match team_name {
        Some(team_name) => TeamDeleteOutput {
            success: true,
            message: format!("Cleaned up directories and worktrees for team \"{team_name}\""),
            team_name: Some(team_name),
        },
        None => TeamDeleteOutput {
            success: true,
            message: "No team name found, nothing to clean up".to_string(),
            team_name: None,
        },
    })
}

pub(crate) fn team_delete_output_json(output: &TeamDeleteOutput) -> serde_json::Value {
    let mut object = serde_json::Map::new();
    object.insert("success".to_string(), serde_json::json!(output.success));
    object.insert("message".to_string(), serde_json::json!(&output.message));
    if let Some(team_name) = &output.team_name {
        object.insert("team_name".to_string(), serde_json::json!(team_name));
    }
    serde_json::Value::Object(object)
}

/// Behavioral half of CC `TeamDeleteTool` — dispatched via `crate::tool::ToolCall`.
pub(crate) struct TeamDeleteTool;

impl crate::tool::ToolCall for TeamDeleteTool {
    fn name(&self) -> &'static str {
        "TeamDelete"
    }

    /// Maps to: CC `TeamDeleteTool.ts:54-56` `async prompt() { return
    /// getPrompt() }` — same source the wire schema renders eagerly.
    fn prompt(
        &self,
        _tool: &crate::types::tools::Tool,
        _options: &crate::tool::ToolPromptOptions<'_>,
    ) -> String {
        prompt::get_prompt()
    }

    /// Maps to: CC `TeamDeleteTool.ts:46-48` `isEnabled()`.
    fn is_enabled(&self) -> bool {
        is_team_delete_tool_enabled()
    }

    /// Maps to: CC `TeamDeleteTool.ts:34` `searchHint`.
    fn search_hint(&self) -> Option<&'static str> {
        Some("disband a swarm team and clean up")
    }

    /// Maps to: CC `TeamDeleteTool.ts:36` `shouldDefer: true`.
    fn should_defer(&self) -> bool {
        true
    }

    /// Maps to: CC `TeamDeleteTool.ts:38-40` `userFacingName() => ''`.
    fn user_facing_name(&self, _args: Option<&serde_json::Value>) -> String {
        String::new()
    }

    fn call<'a>(
        &'a self,
        args: &'a serde_json::Value,
        _request: &'a crate::types::permissions::PermissionRequest,
        context: &'a crate::tool::ToolUseContext,
        _can_use_tool: Option<crate::tool::CanUseToolFn<'a>>,
        _parent_message: Option<&'a crate::types::message::AssistantMessage>,
        _on_progress: Option<crate::tool::ToolCallProgressFn<'a>>,
    ) -> futures::future::BoxFuture<'a, crate::tool::ToolResult> {
        Box::pin(async move {
            let _ = args;
            match team_delete_output(context) {
                Ok(output) => crate::tool::ToolResult {
                    data: crate::tool::ToolOutput::TeamDelete(output),
                    new_messages: Vec::new(),
                },
                Err(error) => crate::tool::ToolResult {
                    data: crate::tool::ToolOutput::Composed {
                        content: error,
                        status: crate::types::message::ToolResultStatus::Error,
                    },
                    new_messages: Vec::new(),
                },
            }
        })
    }

    /// Maps to: CC `tools/TeamDeleteTool/TeamDeleteTool.ts`
    /// `mapToolResultToToolResultBlockParam` (:58-69).
    fn map_tool_result_to_tool_result_block_param(
        &self,
        data: &crate::tool::ToolOutput,
        _tool_use_id: &str,
    ) -> (String, crate::types::message::ToolResultStatus) {
        match data {
            crate::tool::ToolOutput::TeamDelete(output) => (
                team_delete_output_json(output).to_string(),
                crate::types::message::ToolResultStatus::Success,
            ),
            crate::tool::ToolOutput::Composed {
                content, status, ..
            } => (content.clone(), *status),
            _ => (
                "<tool_use_error>TeamDelete returned an unexpected output variant</tool_use_error>"
                    .to_string(),
                crate::types::message::ToolResultStatus::Error,
            ),
        }
    }

    // CC `TeamDeleteTool/UI.tsx:11-25` renders null for every result shape —
    // the render layer hides success rows by name
    // (`success_tool_result_is_nonvisual`).

    /// Maps to: CC recording TeamDeleteTool's call data
    /// (`TeamDeleteTool.ts:88-97/:126-133` `{ success, message,
    /// team_name? }`) as the message's `toolUseResult`.
    fn tool_use_result(&self, data: &crate::tool::ToolOutput) -> Option<serde_json::Value> {
        match data {
            crate::tool::ToolOutput::TeamDelete(output) => Some(team_delete_output_json(output)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[test]
    fn team_delete_tool_schema_matches_official_empty_input_shape() {
        let schema = super::team_delete_tool_schema();
        assert_eq!(schema.name, "TeamDelete");
        // `z.strictObject({})` — zod emits no `required` key at all.
        assert_eq!(schema.input_schema.get("required"), None);
        assert!(schema.description.contains("TeamDelete"));
    }

    #[test]
    fn team_delete_unregisters_session_cleanup_after_successful_cleanup() {
        let _team_lock = crate::utils::swarm::team_helpers::TEST_TEAM_HELPERS_LOCK
            .lock()
            .unwrap();
        let _task_lock = crate::utils::tasks::TASK_TOOL_TEST_LOCK.lock().unwrap();
        crate::utils::swarm::team_helpers::clear_team_tool_state_for_test();
        crate::bootstrap::state::clear_session_created_teams();
        let record = crate::utils::swarm::team_helpers::create_team_record(
            "Alpha".to_string(),
            None,
            Some("team-lead".to_string()),
            None,
            "/tmp".to_string(),
        );
        crate::utils::swarm::team_helpers::write_team_record(record);
        crate::utils::swarm::team_helpers::register_team_for_session_cleanup("Alpha");

        let output = super::team_delete_output(&crate::tool::ToolUseContext::default()).unwrap();

        assert!(output.success);
        assert!(crate::bootstrap::state::get_session_created_teams().is_empty());
        crate::utils::swarm::team_helpers::clear_team_tool_state_for_test();
        crate::bootstrap::state::clear_session_created_teams();
    }

    /// CC `TeamDeleteTool.ts:78` calls `readTeamFile` — a pure disk read —
    /// precisely because teammate processes flip `isActive` on disk. The
    /// leader's in-memory record must not decide the active-member gate in
    /// either direction.
    #[test]
    fn team_delete_reads_member_activity_from_disk_not_memory() {
        let _team_lock = crate::utils::swarm::team_helpers::TEST_TEAM_HELPERS_LOCK
            .lock()
            .unwrap();
        let _task_lock = crate::utils::tasks::TASK_TOOL_TEST_LOCK.lock().unwrap();
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::swarm::team_helpers::clear_team_tool_state_for_test();
        crate::bootstrap::state::clear_session_created_teams();
        crate::utils::tasks::clear_leader_team_name();
        let root = std::env::temp_dir().join(format!(
            "cometix-team-delete-disk-auth-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let _config_guard = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root);
        let _io_guard = EnvVarGuard::set("COMETIX_TEST_TEAM_FILE_IO", "1");

        let mut record = crate::utils::swarm::team_helpers::create_team_record(
            "alpha".to_string(),
            None,
            Some("team-lead".to_string()),
            None,
            "/tmp".to_string(),
        );
        record
            .members
            .push(crate::utils::swarm::team_helpers::TeamMemberRecord {
                agent_id: "researcher@alpha".to_string(),
                name: "researcher".to_string(),
                agent_type: None,
                model: None,
                prompt: None,
                color: None,
                plan_mode_required: None,
                joined_at_ms: 1,
                tmux_pane_id: String::new(),
                cwd: "/tmp".to_string(),
                worktree_path: None,
                session_id: None,
                subscriptions: Vec::new(),
                backend_type: None,
                is_active: Some(false),
                mode: None,
            });
        crate::utils::swarm::team_helpers::write_team_record(record);

        // Disk says active while the leader's memory says idle → blocked.
        let path = crate::utils::swarm::team_helpers::get_team_file_path("alpha");
        let mut on_disk: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        on_disk["members"][1]["isActive"] = serde_json::json!(true);
        std::fs::write(&path, on_disk.to_string()).unwrap();

        let blocked = super::team_delete_output(&crate::tool::ToolUseContext::default()).unwrap();
        assert!(!blocked.success);
        assert!(
            blocked
                .message
                .contains("Cannot cleanup team with 1 active member(s): researcher")
        );

        // Disk says idle while the leader's memory says active → allowed.
        on_disk["members"][1]["isActive"] = serde_json::json!(false);
        std::fs::write(&path, on_disk.to_string()).unwrap();
        {
            let mut state = crate::utils::swarm::team_helpers::TEAM_TOOL_STATE
                .lock()
                .unwrap();
            state.as_mut().unwrap().members[1].is_active = Some(true);
        }

        let allowed = super::team_delete_output(&crate::tool::ToolUseContext::default()).unwrap();
        assert!(allowed.success);

        crate::utils::swarm::team_helpers::clear_team_tool_state_for_test();
        crate::bootstrap::state::clear_session_created_teams();
        crate::utils::tasks::clear_leader_team_name();
        let _ = std::fs::remove_dir_all(root);
    }
}
