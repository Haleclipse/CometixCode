//! Pane backend executor for out-of-process teammates.
//!
//! Maps to: CC `utils/swarm/backends/PaneBackendExecutor.ts`.
//!
//! This adapter owns the official high-level pane lifecycle boundary: create a
//! backend pane, send the teammate CLI command, write the initial prompt to the
//! file-backed mailbox, and expose mailbox-based send/terminate plus pane kill
//! helpers. Concrete pane backends map to the official tmux and iTerm2 backend
//! modules rather than embedding terminal-specific logic in AgentTool.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::tools::agent_tool::agent_color_manager::AgentColorName;
use crate::utils::agent_id::{format_agent_id, parse_agent_id};
use crate::utils::swarm::constants::TEAM_LEAD_NAME;
use crate::utils::swarm::spawn_utils::{
    build_inherited_cli_flags, build_inherited_env_vars, get_teammate_command, shell_quote,
};
use crate::utils::teammate_mailbox::{TeammateMessageInput, write_to_mailbox};

use super::iterm_backend::ITermBackend;
use super::tmux_backend::TmuxBackend;
use super::types::{
    BackendType, PaneBackendType, TeammateMessage, TeammateSpawnConfig, TeammateSpawnResult,
};

#[derive(Clone, Debug, PartialEq, Eq)]
struct SpawnedTeammatePane {
    pane_id: String,
    inside_tmux: bool,
}

/// Maps to: CC `PaneBackendExecutor`.
#[derive(Clone, Debug)]
pub struct PaneBackendExecutor {
    backend_type: PaneBackendType,
    tmux_backend: Option<TmuxBackend>,
    iterm_backend: Option<ITermBackend>,
    spawned_teammates: Arc<Mutex<HashMap<String, SpawnedTeammatePane>>>,
    /// Maps to: CC `PaneBackendExecutor.cleanupRegistered` (`:50`).
    cleanup_registered: Arc<AtomicBool>,
}

impl PaneBackendExecutor {
    /// Maps to: CC `new PaneBackendExecutor(backend)` for `TmuxBackend`.
    pub fn new_tmux(backend: TmuxBackend) -> Self {
        Self {
            backend_type: PaneBackendType::Tmux,
            tmux_backend: Some(backend),
            iterm_backend: None,
            spawned_teammates: Arc::new(Mutex::new(HashMap::new())),
            cleanup_registered: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Maps to: CC `new PaneBackendExecutor(backend)` for `ITermBackend`.
    pub fn new_iterm2(backend: ITermBackend) -> Self {
        Self {
            backend_type: PaneBackendType::ITerm2,
            tmux_backend: None,
            iterm_backend: Some(backend),
            spawned_teammates: Arc::new(Mutex::new(HashMap::new())),
            cleanup_registered: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Maps to: CC `PaneBackendExecutor.ts:163-175` — the one-shot
    /// `registerCleanup` that kills every pane this executor spawned when the
    /// leader exits (e.g. SIGHUP). `spawnedTeammates` is shared by `Arc`, so
    /// the registered closure observes later spawns exactly like CC's `this`.
    fn register_pane_cleanup_once(&self) {
        if self.cleanup_registered.swap(true, Ordering::SeqCst) {
            return;
        }
        let backend_type = self.backend_type;
        let tmux_backend = self.tmux_backend.clone();
        let iterm_backend = self.iterm_backend.clone();
        let spawned_teammates = Arc::clone(&self.spawned_teammates);
        crate::utils::cleanup_registry::register_cleanup(move || {
            let tmux_backend = tmux_backend.clone();
            let iterm_backend = iterm_backend.clone();
            let spawned_teammates = Arc::clone(&spawned_teammates);
            async move {
                // CC `:167` iterates the live map, `:173` clears it afterwards.
                let panes = std::mem::take(&mut *spawned_teammates.lock().unwrap())
                    .into_iter()
                    .collect::<Vec<_>>();
                for (agent_id, info) in panes {
                    crate::utils::debug::log_for_debugging(&format!(
                        "[PaneBackendExecutor] Cleanup: killing pane for {agent_id}"
                    ));
                    match backend_type {
                        PaneBackendType::Tmux => {
                            if let Some(backend) = tmux_backend.as_ref() {
                                backend.kill_pane(&info.pane_id, !info.inside_tmux).await;
                            }
                        }
                        PaneBackendType::ITerm2 => {
                            if let Some(backend) = iterm_backend.as_ref() {
                                backend.kill_pane(&info.pane_id, !info.inside_tmux).await;
                            }
                        }
                    }
                }
            }
        });
    }

    pub fn backend_type(&self) -> BackendType {
        self.backend_type.as_backend_type()
    }

    /// Maps to: CC `PaneBackendExecutor.isAvailable()`.
    pub async fn is_available(&self) -> bool {
        match self.backend_type {
            PaneBackendType::Tmux => match self.tmux_backend.as_ref() {
                Some(backend) => backend.is_available().await,
                None => false,
            },
            PaneBackendType::ITerm2 => match self.iterm_backend.as_ref() {
                Some(backend) => backend.is_available().await,
                None => false,
            },
        }
    }

    /// Maps to: CC `PaneBackendExecutor.spawn(...)`.
    pub async fn spawn(
        &self,
        config: TeammateSpawnConfig,
        context: &crate::tool::ToolUseContext,
    ) -> TeammateSpawnResult {
        let agent_id = format_agent_id(&config.name, &config.team_name);

        let color = config.color.unwrap_or_else(|| {
            crate::utils::swarm::teammate_layout_manager::assign_teammate_color(&agent_id)
        });

        let spawn_result = async {
            let pane = match self.backend_type {
                PaneBackendType::Tmux => {
                    self.tmux_backend
                        .as_ref()
                        .ok_or_else(|| "Tmux pane backend is not registered".to_string())?
                        .create_teammate_pane_in_swarm_view(&config.name, color)
                        .await?
                }
                PaneBackendType::ITerm2 => {
                    self.iterm_backend
                        .as_ref()
                        .ok_or_else(|| "iTerm2 pane backend is not registered".to_string())?
                        .create_teammate_pane_in_swarm_view(&config.name, color)
                        .await?
                }
            };
            let inside_tmux = super::detection::is_inside_tmux().await;
            if pane.is_first_teammate && inside_tmux {
                if let Some(backend) = self.tmux_backend.as_ref() {
                    backend.enable_pane_border_status(None, false).await?;
                }
            }
            let command = build_pane_spawn_command(&config, context, &agent_id, color);
            match self.backend_type {
                PaneBackendType::Tmux => {
                    self.tmux_backend
                        .as_ref()
                        .ok_or_else(|| "Tmux pane backend is not registered".to_string())?
                        .send_command_to_pane(&pane.pane_id, &command, !inside_tmux)
                        .await?
                }
                PaneBackendType::ITerm2 => {
                    self.iterm_backend
                        .as_ref()
                        .ok_or_else(|| "iTerm2 pane backend is not registered".to_string())?
                        .send_command_to_pane(&pane.pane_id, &command, !inside_tmux)
                        .await?
                }
            };

            self.spawned_teammates.lock().unwrap().insert(
                agent_id.clone(),
                SpawnedTeammatePane {
                    pane_id: pane.pane_id.clone(),
                    inside_tmux,
                },
            );

            // CC `:163-175` — registered after the first successful spawn.
            self.register_pane_cleanup_once();

            write_to_mailbox(
                &config.name,
                TeammateMessageInput {
                    from: TEAM_LEAD_NAME.to_string(),
                    text: config.prompt.clone(),
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    color: None,
                    summary: None,
                },
                Some(&config.team_name),
            )?;

            Ok::<String, String>(pane.pane_id)
        }
        .await;

        match spawn_result {
            Ok(pane_id) => TeammateSpawnResult {
                success: true,
                agent_id,
                error: None,
                task_id: None,
                pane_id: Some(pane_id),
            },
            Err(error) => TeammateSpawnResult {
                success: false,
                agent_id,
                error: Some(error),
                task_id: None,
                pane_id: None,
            },
        }
    }

    /// Maps to: CC `PaneBackendExecutor.sendMessage(...)`.
    pub fn send_message(&self, agent_id: &str, message: TeammateMessage) -> Result<(), String> {
        let Some((agent_name, team_name)) = parse_agent_id(agent_id) else {
            return Err(format!(
                "Invalid agentId format: {agent_id}. Expected format: agentName@teamName"
            ));
        };
        write_to_mailbox(
            &agent_name,
            TeammateMessageInput {
                from: message.from,
                text: message.text,
                timestamp: message
                    .timestamp
                    .unwrap_or_else(|| chrono::Utc::now().to_rfc3339()),
                color: message.color,
                summary: message.summary,
            },
            Some(&team_name),
        )
    }

    /// Maps to: CC `PaneBackendExecutor.terminate(...)`.
    pub fn terminate(&self, agent_id: &str, reason: Option<&str>) -> bool {
        let Some((agent_name, team_name)) = parse_agent_id(agent_id) else {
            return false;
        };
        let request_id = crate::utils::agent_id::generate_request_id("shutdown", &agent_name);
        let request = crate::utils::teammate_mailbox::create_shutdown_request_message(
            &request_id,
            TEAM_LEAD_NAME,
            reason,
        );
        write_to_mailbox(
            &agent_name,
            TeammateMessageInput {
                from: TEAM_LEAD_NAME.to_string(),
                text: request.to_string(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                color: None,
                summary: None,
            },
            Some(&team_name),
        )
        .is_ok()
    }

    /// Maps to: CC `PaneBackendExecutor.kill(...)`.
    pub async fn kill(&self, agent_id: &str) -> bool {
        let Some(info) = self
            .spawned_teammates
            .lock()
            .unwrap()
            .get(agent_id)
            .cloned()
        else {
            return false;
        };
        let killed = match self.backend_type {
            PaneBackendType::Tmux => match self.tmux_backend.as_ref() {
                Some(backend) => backend.kill_pane(&info.pane_id, !info.inside_tmux).await,
                None => false,
            },
            PaneBackendType::ITerm2 => match self.iterm_backend.as_ref() {
                Some(backend) => backend.kill_pane(&info.pane_id, !info.inside_tmux).await,
                None => false,
            },
        };
        if killed {
            self.spawned_teammates.lock().unwrap().remove(agent_id);
        }
        killed
    }

    /// Maps to: CC `PaneBackendExecutor.isActive(...)`.
    pub fn is_active(&self, agent_id: &str) -> bool {
        self.spawned_teammates
            .lock()
            .unwrap()
            .contains_key(agent_id)
    }
}

/// Maps to: CC `createPaneBackendExecutor(...)`.
pub fn create_pane_backend_executor(
    backend_type: PaneBackendType,
) -> Result<PaneBackendExecutor, String> {
    match backend_type {
        PaneBackendType::Tmux => Ok(PaneBackendExecutor::new_tmux(TmuxBackend::new())),
        PaneBackendType::ITerm2 => Ok(PaneBackendExecutor::new_iterm2(ITermBackend::new())),
    }
}

/// Maps to: CC `PaneBackendExecutor.ts:117-126` teammate identity args.
///
/// Private on purpose: `tools/shared/spawn_multi_agent.rs` used to borrow this
/// helper for its separate-window handler, which dragged the whole
/// `spawnUtils.ts` flag copy onto a path CC builds from its OWN module-local
/// copy. Both spawn handlers now build their args in place, like CC.
///
/// Known remaining divergence, booked as a follow-up: the `--agent-type` arm
/// below belongs to `spawnMultiAgent.ts:411`/`:618`, not to CC's executor,
/// which emits five identity flags plus `--plan-mode-required` and nothing
/// else. Harmless while this class has no production caller (CC reaches it only
/// through `registry.ts#getTeammateExecutor`, zero callers in 2.1.88).
fn build_teammate_identity_cli_args(
    config: &TeammateSpawnConfig,
    agent_id: &str,
    color: AgentColorName,
) -> String {
    let mut args = vec![
        format!("--agent-id {}", shell_quote(agent_id)),
        format!("--agent-name {}", shell_quote(&config.name)),
        format!("--team-name {}", shell_quote(&config.team_name)),
        format!("--agent-color {}", shell_quote(color.official_name())),
        format!(
            "--parent-session-id {}",
            shell_quote(&config.parent_session_id)
        ),
    ];
    if config.plan_mode_required {
        args.push("--plan-mode-required".to_string());
    }
    if let Some(agent_type) = config.agent_type.as_ref().filter(|value| !value.is_empty()) {
        args.push(format!("--agent-type {}", shell_quote(agent_type)));
    }
    args.join(" ")
}

fn without_model_flags(flags: &str) -> String {
    let mut parts = flags.split_whitespace().peekable();
    let mut out = Vec::new();
    while let Some(part) = parts.next() {
        if part == "--model" {
            let _ = parts.next();
            continue;
        }
        out.push(part.to_string());
    }
    out.join(" ")
}

/// Maps to: CC `PaneBackendExecutor.ts:113-154` command construction — the
/// `spawnUtils.ts` flag copy (`--teammate-mode`, no `auto` arm) belongs HERE
/// and only here. Private for the same reason as
/// [`build_teammate_identity_cli_args`].
fn build_pane_spawn_command(
    config: &TeammateSpawnConfig,
    context: &crate::tool::ToolUseContext,
    agent_id: &str,
    color: AgentColorName,
) -> String {
    let binary_path = get_teammate_command();
    let teammate_args = build_teammate_identity_cli_args(config, agent_id, color);
    let mut inherited_flags = build_inherited_cli_flags(
        config.plan_mode_required,
        Some(context.tool_permission_context.mode),
    );
    if let Some(model) = config.model.as_ref().filter(|value| !value.is_empty()) {
        inherited_flags = without_model_flags(&inherited_flags);
        if inherited_flags.is_empty() {
            inherited_flags = format!("--model {}", shell_quote(model));
        } else {
            inherited_flags = format!("{inherited_flags} --model {}", shell_quote(model));
        }
    }
    let flags_str = if inherited_flags.is_empty() {
        String::new()
    } else {
        format!(" {inherited_flags}")
    };
    let env_str = build_inherited_env_vars();
    format!(
        "cd {} && env {} {} {}{}",
        shell_quote(&config.cwd),
        env_str,
        shell_quote(&binary_path),
        teammate_args,
        flags_str
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    fn config() -> TeammateSpawnConfig {
        TeammateSpawnConfig {
            name: "reviewer".to_string(),
            team_name: "alpha team".to_string(),
            prompt: "review this".to_string(),
            cwd: "/tmp/project dir".to_string(),
            color: Some(AgentColorName::Green),
            plan_mode_required: true,
            model: Some("claude model".to_string()),
            parent_session_id: "session-1".to_string(),
            agent_type: Some("general-purpose".to_string()),
        }
    }

    #[test]
    fn teammate_identity_args_match_official_cli_shape() {
        let args = build_teammate_identity_cli_args(
            &config(),
            "reviewer@alpha team",
            AgentColorName::Green,
        );
        assert!(args.contains("--agent-id 'reviewer@alpha team'"));
        assert!(args.contains("--agent-name reviewer"));
        assert!(args.contains("--team-name 'alpha team'"));
        assert!(args.contains("--agent-color green"));
        assert!(args.contains("--parent-session-id session-1"));
        assert!(args.contains("--plan-mode-required"));
        assert!(args.contains("--agent-type general-purpose"));
    }

    #[test]
    fn pane_backend_executor_supports_official_iterm2_backend_type() {
        let executor = create_pane_backend_executor(PaneBackendType::ITerm2).unwrap();
        assert_eq!(executor.backend_type(), BackendType::ITerm2);
    }

    #[test]
    fn pane_spawn_command_matches_official_cd_env_binary_args_flags_shape() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _teammate = EnvVarGuard::set(
            crate::utils::swarm::constants::TEAMMATE_COMMAND_ENV_VAR,
            "/bin/claude code",
        );
        let command = build_pane_spawn_command(
            &config(),
            &crate::tool::ToolUseContext::default(),
            "reviewer@alpha team",
            AgentColorName::Green,
        );
        assert!(command.starts_with("cd '/tmp/project dir' && env CLAUDECODE=1"));
        assert!(command.contains("'/bin/claude code' --agent-id 'reviewer@alpha team'"));
        assert!(command.contains("--team-name 'alpha team'"));
        assert!(command.contains("--model 'claude model'"));
    }

    #[test]
    fn terminate_and_send_message_route_through_file_mailbox_contract() {
        let _mailbox_lock = crate::utils::teammate_mailbox::TEST_TEAMMATE_MAILBOX_LOCK
            .lock()
            .unwrap();
        crate::utils::teammate_mailbox::clear_mailboxes_for_test();
        let executor = PaneBackendExecutor::new_tmux(TmuxBackend::new());
        executor
            .send_message(
                "reviewer@alpha",
                TeammateMessage {
                    text: "hello".to_string(),
                    from: TEAM_LEAD_NAME.to_string(),
                    color: Some("green".to_string()),
                    timestamp: Some("2026-01-01T00:00:00Z".to_string()),
                    summary: Some("greet".to_string()),
                },
            )
            .unwrap();
        assert_eq!(
            crate::utils::teammate_mailbox::read_mailbox("reviewer", Some("alpha"))[0].text,
            "hello"
        );
        assert!(executor.terminate("reviewer@alpha", Some("done")));
        let messages = crate::utils::teammate_mailbox::read_mailbox("reviewer", Some("alpha"));
        assert_eq!(messages.len(), 2);
        assert!(crate::utils::teammate_mailbox::is_shutdown_request(&messages[1].text).is_some());
    }
}
