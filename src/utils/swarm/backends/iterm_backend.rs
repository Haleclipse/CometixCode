//! iTerm2 pane backend for teammate execution.
//! Maps to: CC `utils/swarm/backends/ITermBackend.ts`.

use std::process::Command;
use std::sync::{LazyLock, Mutex};

use crate::tools::agent_tool::agent_color_manager::AgentColorName;

use super::detection::{IT2_COMMAND, is_in_iterm2, is_it2_cli_available};
use super::tmux_backend::CreatePaneResult;

/// Maps to the `{ stdout, stderr, code }` result returned by CC
/// `execFileNoThrow(...)` calls in `ITermBackend.ts`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct It2CommandResult {
    pub stdout: String,
    pub stderr: String,
    pub code: i32,
}

static TEAMMATE_SESSION_IDS: LazyLock<Mutex<Vec<String>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));
static FIRST_PANE_USED: LazyLock<Mutex<bool>> = LazyLock::new(|| Mutex::new(false));
static PANE_CREATION_LOCK: LazyLock<tokio::sync::Mutex<()>> =
    LazyLock::new(|| tokio::sync::Mutex::new(()));

fn run_it2(args: &[String]) -> It2CommandResult {
    let mut command = Command::new(IT2_COMMAND);
    // CC `execFileNoThrow` inherits process.env (execa default); the carrier is its counterpart.
    crate::utils::subprocess_env::apply_process_env_std(&mut command);
    let output = command.args(args).output();
    match output {
        Ok(output) => It2CommandResult {
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            code: output.status.code().unwrap_or(1),
        },
        Err(error) => It2CommandResult {
            stdout: String::new(),
            stderr: error.to_string(),
            code: 1,
        },
    }
}

/// Maps to: CC `parseSplitOutput(...)`.
pub fn parse_split_output(output: &str) -> String {
    for line in output.lines() {
        let Some((_, rest)) = line.split_once("Created new pane:") else {
            continue;
        };
        return rest.trim().to_string();
    }
    String::new()
}

/// Maps to: CC `getLeaderSessionId()`.
pub fn leader_session_id_from_env(iterm_session_id: Option<&str>) -> Option<String> {
    let value = iterm_session_id?.trim();
    if value.is_empty() {
        return None;
    }
    let colon_index = value.find(':')?;
    let session_id = value[colon_index + 1..].trim();
    if session_id.is_empty() {
        None
    } else {
        Some(session_id.to_string())
    }
}

fn get_leader_session_id() -> Option<String> {
    leader_session_id_from_env(crate::utils::process_env::var("ITERM_SESSION_ID").as_deref())
}

/// Pure split-target/orientation calculation from CC
/// `ITermBackend.createTeammatePaneInSwarmView(...)`.
pub fn split_args_for_next_pane(
    first_pane_used: bool,
    teammate_session_ids: &[String],
    leader_session_id: Option<&str>,
) -> (Vec<String>, Option<String>) {
    if !first_pane_used {
        if let Some(leader_session_id) = leader_session_id.filter(|value| !value.is_empty()) {
            (
                vec![
                    "session".to_string(),
                    "split".to_string(),
                    "-v".to_string(),
                    "-s".to_string(),
                    leader_session_id.to_string(),
                ],
                None,
            )
        } else {
            (
                vec!["session".to_string(), "split".to_string(), "-v".to_string()],
                None,
            )
        }
    } else if let Some(targeted_teammate_id) = teammate_session_ids.last() {
        (
            vec![
                "session".to_string(),
                "split".to_string(),
                "-s".to_string(),
                targeted_teammate_id.clone(),
            ],
            Some(targeted_teammate_id.clone()),
        )
    } else {
        (vec!["session".to_string(), "split".to_string()], None)
    }
}

/// Maps to: CC dead-target recovery guard in
/// `ITermBackend.createTeammatePaneInSwarmView(...)`.
pub fn should_prune_dead_target(
    split_result: &It2CommandResult,
    targeted_teammate_id: Option<&str>,
    list_result: &It2CommandResult,
) -> bool {
    split_result.code != 0
        && targeted_teammate_id.is_some_and(|id| {
            !id.is_empty() && list_result.code == 0 && !list_result.stdout.contains(id)
        })
}

/// Maps to: CC `class ITermBackend implements PaneBackend`.
#[derive(Clone, Copy, Debug, Default)]
pub struct ITermBackend;

impl ITermBackend {
    pub fn new() -> Self {
        Self
    }

    /// Maps to: CC `ITermBackend.type`.
    pub fn backend_type(&self) -> &'static str {
        "iterm2"
    }

    /// Maps to: CC `ITermBackend.displayName`.
    pub fn display_name(&self) -> &'static str {
        "iTerm2"
    }

    /// Maps to: CC `ITermBackend.supportsHideShow`.
    pub fn supports_hide_show(&self) -> bool {
        false
    }

    /// Maps to: CC `ITermBackend.isAvailable()`.
    pub async fn is_available(&self) -> bool {
        is_in_iterm2() && is_it2_cli_available().await
    }

    /// Maps to: CC `ITermBackend.isRunningInside()`.
    pub async fn is_running_inside(&self) -> bool {
        is_in_iterm2()
    }

    /// Maps to: CC `ITermBackend.createTeammatePaneInSwarmView(...)`.
    pub async fn create_teammate_pane_in_swarm_view(
        &self,
        _name: &str,
        _color: AgentColorName,
    ) -> Result<CreatePaneResult, String> {
        let _guard = PANE_CREATION_LOCK.lock().await;

        loop {
            let first_pane_used = *FIRST_PANE_USED.lock().unwrap();
            let existing_sessions = TEAMMATE_SESSION_IDS.lock().unwrap().clone();
            let leader_session_id = get_leader_session_id();
            let (split_args, targeted_teammate_id) = split_args_for_next_pane(
                first_pane_used,
                &existing_sessions,
                leader_session_id.as_deref(),
            );

            let split_result = run_it2(&split_args);
            if split_result.code != 0 {
                if let Some(targeted_id) = targeted_teammate_id.as_deref() {
                    let list_args = vec!["session".to_string(), "list".to_string()];
                    let list_result = run_it2(&list_args);
                    if should_prune_dead_target(&split_result, Some(targeted_id), &list_result) {
                        let mut sessions = TEAMMATE_SESSION_IDS.lock().unwrap();
                        if let Some(idx) = sessions.iter().position(|id| id == targeted_id) {
                            sessions.remove(idx);
                        }
                        if sessions.is_empty() {
                            *FIRST_PANE_USED.lock().unwrap() = false;
                        }
                        continue;
                    }
                }
                return Err(format!(
                    "Failed to create iTerm2 split pane: {}",
                    split_result.stderr
                ));
            }

            if !first_pane_used {
                *FIRST_PANE_USED.lock().unwrap() = true;
            }

            let pane_id = parse_split_output(&split_result.stdout);
            if pane_id.is_empty() {
                return Err(format!(
                    "Failed to parse session ID from split output: {}",
                    split_result.stdout
                ));
            }

            TEAMMATE_SESSION_IDS.lock().unwrap().push(pane_id.clone());
            return Ok(CreatePaneResult {
                pane_id,
                is_first_teammate: !first_pane_used,
            });
        }
    }

    /// Maps to: CC `ITermBackend.sendCommandToPane(...)`.
    pub async fn send_command_to_pane(
        &self,
        pane_id: &str,
        command: &str,
        _use_external_session: bool,
    ) -> Result<(), String> {
        let args = if pane_id.is_empty() {
            vec![
                "session".to_string(),
                "run".to_string(),
                command.to_string(),
            ]
        } else {
            vec![
                "session".to_string(),
                "run".to_string(),
                "-s".to_string(),
                pane_id.to_string(),
                command.to_string(),
            ]
        };
        let result = run_it2(&args);
        if result.code != 0 {
            return Err(format!(
                "Failed to send command to iTerm2 pane {pane_id}: {}",
                result.stderr
            ));
        }
        Ok(())
    }

    /// Maps to: CC `ITermBackend.setPaneBorderColor(...)` no-op.
    pub async fn set_pane_border_color(
        &self,
        _pane_id: &str,
        _color: AgentColorName,
        _use_external_session: bool,
    ) -> Result<(), String> {
        Ok(())
    }

    /// Maps to: CC `ITermBackend.setPaneTitle(...)` no-op.
    pub async fn set_pane_title(
        &self,
        _pane_id: &str,
        _name: &str,
        _color: AgentColorName,
        _use_external_session: bool,
    ) -> Result<(), String> {
        Ok(())
    }

    /// Maps to: CC `ITermBackend.enablePaneBorderStatus(...)` no-op.
    pub async fn enable_pane_border_status(
        &self,
        _window_target: Option<&str>,
        _use_external_session: bool,
    ) -> Result<(), String> {
        Ok(())
    }

    /// Maps to: CC `ITermBackend.rebalancePanes(...)` no-op.
    pub async fn rebalance_panes(
        &self,
        _window_target: &str,
        _has_leader: bool,
    ) -> Result<(), String> {
        Ok(())
    }

    /// Maps to: CC `ITermBackend.killPane(...)`.
    pub async fn kill_pane(&self, pane_id: &str, _use_external_session: bool) -> bool {
        let args = vec![
            "session".to_string(),
            "close".to_string(),
            "-f".to_string(),
            "-s".to_string(),
            pane_id.to_string(),
        ];
        let result = run_it2(&args);

        let mut sessions = TEAMMATE_SESSION_IDS.lock().unwrap();
        if let Some(idx) = sessions.iter().position(|id| id == pane_id) {
            sessions.remove(idx);
        }
        if sessions.is_empty() {
            *FIRST_PANE_USED.lock().unwrap() = false;
        }

        result.code == 0
    }

    /// Maps to: CC `ITermBackend.hidePane(...)` unsupported return.
    pub async fn hide_pane(&self, _pane_id: &str, _use_external_session: bool) -> bool {
        false
    }

    /// Maps to: CC `ITermBackend.showPane(...)` unsupported return.
    pub async fn show_pane(
        &self,
        _pane_id: &str,
        _target_window_or_pane: &str,
        _use_external_session: bool,
    ) -> bool {
        false
    }
}

/// Test-only reset for CC module-scope `teammateSessionIds` and
/// `firstPaneUsed` state.
pub fn clear_iterm_backend_state_for_test() {
    TEAMMATE_SESSION_IDS.lock().unwrap().clear();
    *FIRST_PANE_USED.lock().unwrap() = false;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_split_output_matches_official_it2_shape() {
        assert_eq!(
            parse_split_output("Created new pane: 7A4A-SESSION\n"),
            "7A4A-SESSION"
        );
        assert_eq!(parse_split_output("noise"), "");
    }

    #[test]
    fn leader_session_id_extracts_uuid_after_colon_like_official() {
        assert_eq!(
            leader_session_id_from_env(Some("w0t1p0:SESSION-UUID")),
            Some("SESSION-UUID".to_string())
        );
        assert_eq!(leader_session_id_from_env(Some("SESSION-UUID")), None);
        assert_eq!(leader_session_id_from_env(Some("w0t1p0:")), None);
    }

    #[test]
    fn split_args_match_official_first_and_subsequent_layout() {
        let (first_args, target) = split_args_for_next_pane(false, &[], Some("LEADER-SESSION"));
        assert_eq!(
            first_args,
            vec!["session", "split", "-v", "-s", "LEADER-SESSION"]
        );
        assert_eq!(target, None);

        let existing = vec!["FIRST".to_string(), "SECOND".to_string()];
        let (next_args, target) = split_args_for_next_pane(true, &existing, None);
        assert_eq!(next_args, vec!["session", "split", "-s", "SECOND"]);
        assert_eq!(target.as_deref(), Some("SECOND"));
    }

    #[test]
    fn dead_target_prune_guard_matches_official_session_list_check() {
        let failed_split = It2CommandResult {
            stdout: String::new(),
            stderr: "missing".to_string(),
            code: 1,
        };
        let list_without_target = It2CommandResult {
            stdout: "LIVE-1\nLIVE-2\n".to_string(),
            stderr: String::new(),
            code: 0,
        };
        let list_with_target = It2CommandResult {
            stdout: "LIVE-1\nDEAD\n".to_string(),
            stderr: String::new(),
            code: 0,
        };
        assert!(should_prune_dead_target(
            &failed_split,
            Some("DEAD"),
            &list_without_target
        ));
        assert!(!should_prune_dead_target(
            &failed_split,
            Some("DEAD"),
            &list_with_target
        ));
    }

    #[test]
    fn iterm_backend_metadata_matches_official_backend_type() {
        clear_iterm_backend_state_for_test();
        let backend = ITermBackend::new();
        assert_eq!(backend.backend_type(), "iterm2");
        assert_eq!(backend.display_name(), "iTerm2");
        assert!(!backend.supports_hide_show());
    }
}
