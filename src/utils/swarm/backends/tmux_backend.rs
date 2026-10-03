//! tmux pane backend for teammate execution.
//! Maps to: CC `utils/swarm/backends/TmuxBackend.ts`.

use std::process::Command;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use crate::tools::agent_tool::agent_color_manager::AgentColorName;
use crate::utils::swarm::constants::{
    HIDDEN_SESSION_NAME, SWARM_SESSION_NAME, SWARM_VIEW_WINDOW_NAME, TMUX_COMMAND,
    get_swarm_socket_name,
};

use super::detection::{get_leader_pane_id, is_inside_tmux, is_tmux_available};

/// Maps to: CC `types.ts#CreatePaneResult`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreatePaneResult {
    pub pane_id: String,
    pub is_first_teammate: bool,
}

/// Maps to the `{ stdout, stderr, code }` result returned by CC
/// `execFileNoThrow(...)` calls in `TmuxBackend.ts`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TmuxCommandResult {
    pub stdout: String,
    pub stderr: String,
    pub code: i32,
}

static FIRST_PANE_USED_FOR_EXTERNAL: LazyLock<Mutex<bool>> = LazyLock::new(|| Mutex::new(false));
static CACHED_LEADER_WINDOW_TARGET: LazyLock<Mutex<Option<String>>> =
    LazyLock::new(|| Mutex::new(None));
static PANE_CREATION_LOCK: LazyLock<tokio::sync::Mutex<()>> =
    LazyLock::new(|| tokio::sync::Mutex::new(()));

const PANE_SHELL_INIT_DELAY_MS: u64 = 200;

/// Maps to: CC `waitForPaneShellReady()`.
async fn wait_for_pane_shell_ready() {
    tokio::time::sleep(Duration::from_millis(PANE_SHELL_INIT_DELAY_MS)).await;
}

/// Maps to: CC `getTmuxColorName(...)`.
pub fn tmux_color_name(color: AgentColorName) -> &'static str {
    match color {
        AgentColorName::Red => "red",
        AgentColorName::Blue => "blue",
        AgentColorName::Green => "green",
        AgentColorName::Yellow => "yellow",
        AgentColorName::Purple => "magenta",
        AgentColorName::Orange => "colour208",
        AgentColorName::Pink => "colour205",
        AgentColorName::Cyan => "cyan",
    }
}

fn run_tmux(args: &[String]) -> TmuxCommandResult {
    let mut command = Command::new(TMUX_COMMAND);
    // CC `execFileNoThrow` inherits process.env (execa default); the carrier is its counterpart.
    crate::utils::subprocess_env::apply_process_env_std(&mut command);
    let output = command.args(args).output();
    match output {
        Ok(output) => TmuxCommandResult {
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            code: output.status.code().unwrap_or(1),
        },
        Err(error) => TmuxCommandResult {
            stdout: String::new(),
            stderr: error.to_string(),
            code: 1,
        },
    }
}

/// Maps to: CC `runTmuxInSwarm(...)` socket argument shaping.
pub fn tmux_args_for_session(args: &[&str], use_swarm_socket: bool) -> Vec<String> {
    if use_swarm_socket {
        let mut rendered = vec!["-L".to_string(), get_swarm_socket_name()];
        rendered.extend(args.iter().map(|arg| (*arg).to_string()));
        rendered
    } else {
        args.iter().map(|arg| (*arg).to_string()).collect()
    }
}

fn tmux_args_for_session_strings(args: &[String], use_swarm_socket: bool) -> Vec<String> {
    if use_swarm_socket {
        let mut rendered = vec!["-L".to_string(), get_swarm_socket_name()];
        rendered.extend(args.iter().cloned());
        rendered
    } else {
        args.to_vec()
    }
}

/// Maps to: CC `runTmuxInUserSession(...)`.
fn run_tmux_in_user_session(args: &[String]) -> TmuxCommandResult {
    run_tmux(args)
}

/// Maps to: CC `runTmuxInSwarm(...)`.
fn run_tmux_in_swarm(args: &[String]) -> TmuxCommandResult {
    run_tmux(&tmux_args_for_session_strings(args, true))
}

fn split_lines(stdout: &str) -> Vec<String> {
    stdout
        .trim()
        .split('\n')
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

/// Pure port of the additional-teammate split target/orientation calculation
/// in CC `createTeammatePaneWithLeader(...)` and
/// `createTeammatePaneExternal(...)`.
pub fn additional_teammate_split_strategy(panes: &[String]) -> Option<(String, &'static str)> {
    if panes.is_empty() {
        return None;
    }
    let teammate_count = panes.len();
    let split_flag = if teammate_count % 2 == 1 { "-v" } else { "-h" };
    let target_pane_index = (teammate_count - 1) / 2;
    let target_pane = panes
        .get(target_pane_index)
        .or_else(|| panes.last())?
        .clone();
    Some((target_pane, split_flag))
}

/// Maps to: CC `class TmuxBackend implements PaneBackend`.
#[derive(Clone, Copy, Debug, Default)]
pub struct TmuxBackend;

impl TmuxBackend {
    pub fn new() -> Self {
        Self
    }

    /// Maps to: CC `TmuxBackend.type`.
    pub fn backend_type(&self) -> &'static str {
        "tmux"
    }

    /// Maps to: CC `TmuxBackend.displayName`.
    pub fn display_name(&self) -> &'static str {
        "tmux"
    }

    /// Maps to: CC `TmuxBackend.supportsHideShow`.
    pub fn supports_hide_show(&self) -> bool {
        true
    }

    /// Maps to: CC `TmuxBackend.isAvailable()`.
    pub async fn is_available(&self) -> bool {
        is_tmux_available().await
    }

    /// Maps to: CC `TmuxBackend.isRunningInside()`.
    pub async fn is_running_inside(&self) -> bool {
        is_inside_tmux().await
    }

    /// Maps to: CC `TmuxBackend.createTeammatePaneInSwarmView(...)`.
    pub async fn create_teammate_pane_in_swarm_view(
        &self,
        name: &str,
        color: AgentColorName,
    ) -> Result<CreatePaneResult, String> {
        let _guard = PANE_CREATION_LOCK.lock().await;
        if self.is_running_inside().await {
            self.create_teammate_pane_with_leader(name, color).await
        } else {
            self.create_teammate_pane_external(name, color).await
        }
    }

    /// Maps to: CC `TmuxBackend.sendCommandToPane(...)`.
    pub async fn send_command_to_pane(
        &self,
        pane_id: &str,
        command: &str,
        use_external_session: bool,
    ) -> Result<(), String> {
        let args = vec![
            "send-keys".to_string(),
            "-t".to_string(),
            pane_id.to_string(),
            command.to_string(),
            "Enter".to_string(),
        ];
        let result = if use_external_session {
            run_tmux_in_swarm(&args)
        } else {
            run_tmux_in_user_session(&args)
        };
        if result.code != 0 {
            return Err(format!(
                "Failed to send command to pane {pane_id}: {}",
                result.stderr
            ));
        }
        Ok(())
    }

    /// Maps to: CC `TmuxBackend.setPaneBorderColor(...)`.
    pub async fn set_pane_border_color(
        &self,
        pane_id: &str,
        color: AgentColorName,
        use_external_session: bool,
    ) -> Result<(), String> {
        let tmux_color = tmux_color_name(color);
        let commands = vec![
            vec![
                "select-pane".to_string(),
                "-t".to_string(),
                pane_id.to_string(),
                "-P".to_string(),
                format!("bg=default,fg={tmux_color}"),
            ],
            vec![
                "set-option".to_string(),
                "-p".to_string(),
                "-t".to_string(),
                pane_id.to_string(),
                "pane-border-style".to_string(),
                format!("fg={tmux_color}"),
            ],
            vec![
                "set-option".to_string(),
                "-p".to_string(),
                "-t".to_string(),
                pane_id.to_string(),
                "pane-active-border-style".to_string(),
                format!("fg={tmux_color}"),
            ],
        ];
        for args in commands {
            if use_external_session {
                let _ = run_tmux_in_swarm(&args);
            } else {
                let _ = run_tmux_in_user_session(&args);
            }
        }
        Ok(())
    }

    /// Maps to: CC `TmuxBackend.setPaneTitle(...)`.
    pub async fn set_pane_title(
        &self,
        pane_id: &str,
        name: &str,
        color: AgentColorName,
        use_external_session: bool,
    ) -> Result<(), String> {
        let tmux_color = tmux_color_name(color);
        let commands = vec![
            vec![
                "select-pane".to_string(),
                "-t".to_string(),
                pane_id.to_string(),
                "-T".to_string(),
                name.to_string(),
            ],
            vec![
                "set-option".to_string(),
                "-p".to_string(),
                "-t".to_string(),
                pane_id.to_string(),
                "pane-border-format".to_string(),
                format!("#[fg={tmux_color},bold] #{{pane_title}} #[default]"),
            ],
        ];
        for args in commands {
            if use_external_session {
                let _ = run_tmux_in_swarm(&args);
            } else {
                let _ = run_tmux_in_user_session(&args);
            }
        }
        Ok(())
    }

    /// Maps to: CC `TmuxBackend.enablePaneBorderStatus(...)`.
    pub async fn enable_pane_border_status(
        &self,
        window_target: Option<&str>,
        use_external_session: bool,
    ) -> Result<(), String> {
        let target = match window_target {
            Some(target) => Some(target.to_string()),
            None => self.get_current_window_target().await,
        };
        let Some(target) = target else {
            return Ok(());
        };
        let args = vec![
            "set-option".to_string(),
            "-w".to_string(),
            "-t".to_string(),
            target,
            "pane-border-status".to_string(),
            "top".to_string(),
        ];
        if use_external_session {
            let _ = run_tmux_in_swarm(&args);
        } else {
            let _ = run_tmux_in_user_session(&args);
        }
        Ok(())
    }

    /// Maps to: CC `TmuxBackend.rebalancePanes(...)`.
    pub async fn rebalance_panes(
        &self,
        window_target: &str,
        has_leader: bool,
    ) -> Result<(), String> {
        if has_leader {
            self.rebalance_panes_with_leader(window_target).await
        } else {
            self.rebalance_panes_tiled(window_target).await
        }
    }

    /// Maps to: CC `TmuxBackend.killPane(...)`.
    pub async fn kill_pane(&self, pane_id: &str, use_external_session: bool) -> bool {
        let args = vec![
            "kill-pane".to_string(),
            "-t".to_string(),
            pane_id.to_string(),
        ];
        let result = if use_external_session {
            run_tmux_in_swarm(&args)
        } else {
            run_tmux_in_user_session(&args)
        };
        result.code == 0
    }

    /// Maps to: CC `TmuxBackend.hidePane(...)`.
    pub async fn hide_pane(&self, pane_id: &str, use_external_session: bool) -> bool {
        let new_session_args = vec![
            "new-session".to_string(),
            "-d".to_string(),
            "-s".to_string(),
            HIDDEN_SESSION_NAME.to_string(),
        ];
        if use_external_session {
            let _ = run_tmux_in_swarm(&new_session_args);
        } else {
            let _ = run_tmux_in_user_session(&new_session_args);
        }

        let args = vec![
            "break-pane".to_string(),
            "-d".to_string(),
            "-s".to_string(),
            pane_id.to_string(),
            "-t".to_string(),
            format!("{HIDDEN_SESSION_NAME}:"),
        ];
        let result = if use_external_session {
            run_tmux_in_swarm(&args)
        } else {
            run_tmux_in_user_session(&args)
        };
        result.code == 0
    }

    /// Maps to: CC `TmuxBackend.showPane(...)`.
    pub async fn show_pane(
        &self,
        pane_id: &str,
        target_window_or_pane: &str,
        use_external_session: bool,
    ) -> bool {
        let join_args = vec![
            "join-pane".to_string(),
            "-h".to_string(),
            "-s".to_string(),
            pane_id.to_string(),
            "-t".to_string(),
            target_window_or_pane.to_string(),
        ];
        let result = if use_external_session {
            run_tmux_in_swarm(&join_args)
        } else {
            run_tmux_in_user_session(&join_args)
        };
        if result.code != 0 {
            return false;
        }

        let select_args = vec![
            "select-layout".to_string(),
            "-t".to_string(),
            target_window_or_pane.to_string(),
            "main-vertical".to_string(),
        ];
        if use_external_session {
            let _ = run_tmux_in_swarm(&select_args);
        } else {
            let _ = run_tmux_in_user_session(&select_args);
        }

        let list_args = vec![
            "list-panes".to_string(),
            "-t".to_string(),
            target_window_or_pane.to_string(),
            "-F".to_string(),
            "#{pane_id}".to_string(),
        ];
        let panes_result = if use_external_session {
            run_tmux_in_swarm(&list_args)
        } else {
            run_tmux_in_user_session(&list_args)
        };
        let panes = split_lines(&panes_result.stdout);
        if let Some(leader) = panes.first() {
            let resize_args = vec![
                "resize-pane".to_string(),
                "-t".to_string(),
                leader.clone(),
                "-x".to_string(),
                "30%".to_string(),
            ];
            if use_external_session {
                let _ = run_tmux_in_swarm(&resize_args);
            } else {
                let _ = run_tmux_in_user_session(&resize_args);
            }
        }
        true
    }

    /// Maps to: CC private `getCurrentPaneId()`.
    async fn get_current_pane_id(&self) -> Option<String> {
        if let Some(leader_pane) = get_leader_pane_id() {
            return Some(leader_pane);
        }
        let args = vec![
            "display-message".to_string(),
            "-p".to_string(),
            "#{pane_id}".to_string(),
        ];
        let result = run_tmux_in_user_session(&args);
        if result.code != 0 {
            return None;
        }
        Some(result.stdout.trim().to_string()).filter(|value| !value.is_empty())
    }

    /// Maps to: CC private `getCurrentWindowTarget()`.
    async fn get_current_window_target(&self) -> Option<String> {
        if let Some(cached) = CACHED_LEADER_WINDOW_TARGET.lock().unwrap().clone() {
            return Some(cached);
        }
        let mut args = vec!["display-message".to_string()];
        if let Some(leader_pane) = get_leader_pane_id() {
            args.push("-t".to_string());
            args.push(leader_pane);
        }
        args.push("-p".to_string());
        args.push("#{session_name}:#{window_index}".to_string());
        let result = run_tmux_in_user_session(&args);
        if result.code != 0 {
            return None;
        }
        let target = result.stdout.trim().to_string();
        if target.is_empty() {
            return None;
        }
        *CACHED_LEADER_WINDOW_TARGET.lock().unwrap() = Some(target.clone());
        Some(target)
    }

    /// Maps to: CC private `getCurrentWindowPaneCount(...)`.
    async fn get_current_window_pane_count(
        &self,
        window_target: Option<&str>,
        use_swarm_socket: bool,
    ) -> Option<usize> {
        let target = match window_target {
            Some(target) => Some(target.to_string()),
            None => self.get_current_window_target().await,
        }?;
        let args = vec![
            "list-panes".to_string(),
            "-t".to_string(),
            target,
            "-F".to_string(),
            "#{pane_id}".to_string(),
        ];
        let result = if use_swarm_socket {
            run_tmux_in_swarm(&args)
        } else {
            run_tmux_in_user_session(&args)
        };
        if result.code != 0 {
            return None;
        }
        Some(split_lines(&result.stdout).len())
    }

    /// Maps to: CC private `hasSessionInSwarm(...)`.
    async fn has_session_in_swarm(&self, session_name: &str) -> bool {
        let args = vec![
            "has-session".to_string(),
            "-t".to_string(),
            session_name.to_string(),
        ];
        run_tmux_in_swarm(&args).code == 0
    }

    /// Maps to: CC private `createExternalSwarmSession()`.
    async fn create_external_swarm_session(&self) -> Result<(String, String), String> {
        let session_exists = self.has_session_in_swarm(SWARM_SESSION_NAME).await;
        let window_target = format!("{SWARM_SESSION_NAME}:{SWARM_VIEW_WINDOW_NAME}");
        if !session_exists {
            let args = vec![
                "new-session".to_string(),
                "-d".to_string(),
                "-s".to_string(),
                SWARM_SESSION_NAME.to_string(),
                "-n".to_string(),
                SWARM_VIEW_WINDOW_NAME.to_string(),
                "-P".to_string(),
                "-F".to_string(),
                "#{pane_id}".to_string(),
            ];
            let result = run_tmux_in_swarm(&args);
            if result.code != 0 {
                return Err(format!(
                    "Failed to create swarm session: {}",
                    if result.stderr.is_empty() {
                        "Unknown error"
                    } else {
                        result.stderr.as_str()
                    }
                ));
            }
            return Ok((window_target, result.stdout.trim().to_string()));
        }

        let list_args = vec![
            "list-windows".to_string(),
            "-t".to_string(),
            SWARM_SESSION_NAME.to_string(),
            "-F".to_string(),
            "#{window_name}".to_string(),
        ];
        let list_result = run_tmux_in_swarm(&list_args);
        let windows = split_lines(&list_result.stdout);
        if windows
            .iter()
            .any(|window| window == SWARM_VIEW_WINDOW_NAME)
        {
            let pane_args = vec![
                "list-panes".to_string(),
                "-t".to_string(),
                window_target.clone(),
                "-F".to_string(),
                "#{pane_id}".to_string(),
            ];
            let pane_result = run_tmux_in_swarm(&pane_args);
            let panes = split_lines(&pane_result.stdout);
            return Ok((window_target, panes.first().cloned().unwrap_or_default()));
        }

        let create_args = vec![
            "new-window".to_string(),
            "-t".to_string(),
            SWARM_SESSION_NAME.to_string(),
            "-n".to_string(),
            SWARM_VIEW_WINDOW_NAME.to_string(),
            "-P".to_string(),
            "-F".to_string(),
            "#{pane_id}".to_string(),
        ];
        let create_result = run_tmux_in_swarm(&create_args);
        if create_result.code != 0 {
            return Err(format!(
                "Failed to create swarm-view window: {}",
                if create_result.stderr.is_empty() {
                    "Unknown error"
                } else {
                    create_result.stderr.as_str()
                }
            ));
        }
        Ok((window_target, create_result.stdout.trim().to_string()))
    }

    /// Maps to: CC private `createTeammatePaneWithLeader(...)`.
    async fn create_teammate_pane_with_leader(
        &self,
        teammate_name: &str,
        teammate_color: AgentColorName,
    ) -> Result<CreatePaneResult, String> {
        let current_pane_id = self
            .get_current_pane_id()
            .await
            .ok_or_else(|| "Could not determine current tmux pane/window".to_string())?;
        let window_target = self
            .get_current_window_target()
            .await
            .ok_or_else(|| "Could not determine current tmux pane/window".to_string())?;
        let pane_count = self
            .get_current_window_pane_count(Some(&window_target), false)
            .await
            .ok_or_else(|| "Could not determine pane count for current window".to_string())?;
        let is_first_teammate = pane_count == 1;

        let split_result = if is_first_teammate {
            let args = vec![
                "split-window".to_string(),
                "-t".to_string(),
                current_pane_id,
                "-h".to_string(),
                "-l".to_string(),
                "70%".to_string(),
                "-P".to_string(),
                "-F".to_string(),
                "#{pane_id}".to_string(),
            ];
            run_tmux_in_user_session(&args)
        } else {
            let list_args = vec![
                "list-panes".to_string(),
                "-t".to_string(),
                window_target.clone(),
                "-F".to_string(),
                "#{pane_id}".to_string(),
            ];
            let panes = split_lines(&run_tmux_in_user_session(&list_args).stdout);
            let teammate_panes = panes.into_iter().skip(1).collect::<Vec<_>>();
            let (target_pane, split_flag) = additional_teammate_split_strategy(&teammate_panes)
                .ok_or_else(|| "Could not determine target teammate pane".to_string())?;
            let args = vec![
                "split-window".to_string(),
                "-t".to_string(),
                target_pane,
                split_flag.to_string(),
                "-P".to_string(),
                "-F".to_string(),
                "#{pane_id}".to_string(),
            ];
            run_tmux_in_user_session(&args)
        };

        if split_result.code != 0 {
            return Err(format!(
                "Failed to create teammate pane: {}",
                split_result.stderr
            ));
        }
        let pane_id = split_result.stdout.trim().to_string();
        self.set_pane_border_color(&pane_id, teammate_color, false)
            .await?;
        self.set_pane_title(&pane_id, teammate_name, teammate_color, false)
            .await?;
        self.rebalance_panes_with_leader(&window_target).await?;
        wait_for_pane_shell_ready().await;
        Ok(CreatePaneResult {
            pane_id,
            is_first_teammate,
        })
    }

    /// Maps to: CC private `createTeammatePaneExternal(...)`.
    async fn create_teammate_pane_external(
        &self,
        teammate_name: &str,
        teammate_color: AgentColorName,
    ) -> Result<CreatePaneResult, String> {
        let (window_target, first_pane_id) = self.create_external_swarm_session().await?;
        let pane_count = self
            .get_current_window_pane_count(Some(&window_target), true)
            .await
            .ok_or_else(|| "Could not determine pane count for swarm window".to_string())?;
        let is_first_teammate = {
            let mut used = FIRST_PANE_USED_FOR_EXTERNAL.lock().unwrap();
            let is_first = !*used && pane_count == 1;
            if is_first {
                *used = true;
            }
            is_first
        };

        let pane_id = if is_first_teammate {
            self.enable_pane_border_status(Some(&window_target), true)
                .await?;
            first_pane_id
        } else {
            let list_args = vec![
                "list-panes".to_string(),
                "-t".to_string(),
                window_target.clone(),
                "-F".to_string(),
                "#{pane_id}".to_string(),
            ];
            let panes = split_lines(&run_tmux_in_swarm(&list_args).stdout);
            let (target_pane, split_flag) = additional_teammate_split_strategy(&panes)
                .ok_or_else(|| "Could not determine target teammate pane".to_string())?;
            let split_args = vec![
                "split-window".to_string(),
                "-t".to_string(),
                target_pane,
                split_flag.to_string(),
                "-P".to_string(),
                "-F".to_string(),
                "#{pane_id}".to_string(),
            ];
            let split_result = run_tmux_in_swarm(&split_args);
            if split_result.code != 0 {
                return Err(format!(
                    "Failed to create teammate pane: {}",
                    split_result.stderr
                ));
            }
            split_result.stdout.trim().to_string()
        };

        self.set_pane_border_color(&pane_id, teammate_color, true)
            .await?;
        self.set_pane_title(&pane_id, teammate_name, teammate_color, true)
            .await?;
        self.rebalance_panes_tiled(&window_target).await?;
        wait_for_pane_shell_ready().await;
        Ok(CreatePaneResult {
            pane_id,
            is_first_teammate,
        })
    }

    /// Maps to: CC private `rebalancePanesWithLeader(...)`.
    async fn rebalance_panes_with_leader(&self, window_target: &str) -> Result<(), String> {
        let list_args = vec![
            "list-panes".to_string(),
            "-t".to_string(),
            window_target.to_string(),
            "-F".to_string(),
            "#{pane_id}".to_string(),
        ];
        let panes = split_lines(&run_tmux_in_user_session(&list_args).stdout);
        if panes.len() <= 2 {
            return Ok(());
        }
        let select_args = vec![
            "select-layout".to_string(),
            "-t".to_string(),
            window_target.to_string(),
            "main-vertical".to_string(),
        ];
        let _ = run_tmux_in_user_session(&select_args);
        if let Some(leader_pane) = panes.first() {
            let resize_args = vec![
                "resize-pane".to_string(),
                "-t".to_string(),
                leader_pane.clone(),
                "-x".to_string(),
                "30%".to_string(),
            ];
            let _ = run_tmux_in_user_session(&resize_args);
        }
        Ok(())
    }

    /// Maps to: CC private `rebalancePanesTiled(...)`.
    async fn rebalance_panes_tiled(&self, window_target: &str) -> Result<(), String> {
        let list_args = vec![
            "list-panes".to_string(),
            "-t".to_string(),
            window_target.to_string(),
            "-F".to_string(),
            "#{pane_id}".to_string(),
        ];
        let panes = split_lines(&run_tmux_in_swarm(&list_args).stdout);
        if panes.len() <= 1 {
            return Ok(());
        }
        let select_args = vec![
            "select-layout".to_string(),
            "-t".to_string(),
            window_target.to_string(),
            "tiled".to_string(),
        ];
        let _ = run_tmux_in_swarm(&select_args);
        Ok(())
    }
}

/// Maps to: CC test reset of module state for backend detection/layout tests.
#[cfg(test)]
pub fn reset_tmux_backend_state_for_test() {
    *FIRST_PANE_USED_FOR_EXTERNAL.lock().unwrap() = false;
    *CACHED_LEADER_WINDOW_TARGET.lock().unwrap() = None;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tmux_color_mapping_matches_official_builtin_color_names() {
        assert_eq!(tmux_color_name(AgentColorName::Red), "red");
        assert_eq!(tmux_color_name(AgentColorName::Purple), "magenta");
        assert_eq!(tmux_color_name(AgentColorName::Orange), "colour208");
        assert_eq!(tmux_color_name(AgentColorName::Pink), "colour205");
        assert_eq!(tmux_color_name(AgentColorName::Cyan), "cyan");
    }

    #[test]
    fn additional_teammate_split_strategy_matches_official_round_robin_layout() {
        let one = vec!["%2".to_string()];
        assert_eq!(
            additional_teammate_split_strategy(&one),
            Some(("%2".to_string(), "-v"))
        );

        let two = vec!["%2".to_string(), "%3".to_string()];
        assert_eq!(
            additional_teammate_split_strategy(&two),
            Some(("%2".to_string(), "-h"))
        );

        let three = vec!["%2".to_string(), "%3".to_string(), "%4".to_string()];
        assert_eq!(
            additional_teammate_split_strategy(&three),
            Some(("%3".to_string(), "-v"))
        );
    }

    #[test]
    fn tmux_swarm_socket_args_match_official_external_session_shape() {
        let args = tmux_args_for_session(&["has-session", "-t", "claude-swarm"], true);
        assert_eq!(args[0], "-L");
        assert!(args[1].starts_with("claude-swarm-"));
        assert_eq!(&args[2..], ["has-session", "-t", "claude-swarm"]);
        assert_eq!(
            tmux_args_for_session(&["display-message", "-p", "#{pane_id}"], false),
            vec!["display-message", "-p", "#{pane_id}"]
        );
    }

    #[test]
    fn tmux_backend_metadata_matches_official_pane_backend() {
        reset_tmux_backend_state_for_test();
        let backend = TmuxBackend::new();
        assert_eq!(backend.backend_type(), "tmux");
        assert_eq!(backend.display_name(), "tmux");
        assert!(backend.supports_hide_show());
    }
}
