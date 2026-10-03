//! Maps to: CC `utils/appleTerminalBackup.ts`.

use std::path::{Path, PathBuf};

use super::config::{load_global_config, save_global_config};
use super::log::{LogError, log_error};

/// Maps to: CC `utils/appleTerminalBackup.ts#markTerminalSetupInProgress:7-13`.
pub fn mark_terminal_setup_in_progress(backup_path: &str) -> anyhow::Result<()> {
    #[cfg(test)]
    if let Some(result) = tests::save(Some(backup_path)) {
        return result;
    }
    save_global_config(|current| {
        current.apple_terminal_setup_in_progress = Some(true);
        current.apple_terminal_backup_path = Some(backup_path.to_owned());
    })
}

/// Maps to: CC `utils/appleTerminalBackup.ts#markTerminalSetupComplete:15-20`.
pub fn mark_terminal_setup_complete() -> anyhow::Result<()> {
    #[cfg(test)]
    if let Some(result) = tests::save(None) {
        return result;
    }
    save_global_config(|current| {
        current.apple_terminal_setup_in_progress = Some(false);
    })
}

/// Maps to: CC `utils/appleTerminalBackup.ts#getTerminalRecoveryInfo:22-31`.
/// Tuple carries the source's private `{inProgress, backupPath}` record.
fn get_terminal_recovery_info() -> (bool, Option<String>) {
    #[cfg(test)]
    if let Some(info) = tests::recovery_info() {
        return info;
    }
    let config = load_global_config();
    (
        config.apple_terminal_setup_in_progress.unwrap_or(false),
        config
            .apple_terminal_backup_path
            .filter(|path| !path.is_empty()),
    )
}

/// Maps to: CC `utils/appleTerminalBackup.ts#getTerminalPlistPath:33-35`.
pub fn get_terminal_plist_path() -> PathBuf {
    #[cfg(test)]
    if tests::is_active() {
        return PathBuf::from("/oracle/Library/Preferences/com.apple.Terminal.plist");
    }
    crate::utils::node_os::homedir().join("Library/Preferences/com.apple.Terminal.plist")
}

/// Maps to: CC `utils/appleTerminalBackup.ts#backupTerminalPreferences:37-71`.
pub async fn backup_terminal_preferences() -> Option<String> {
    let terminal_plist_path = get_terminal_plist_path();
    let terminal_plist_path = terminal_plist_path.to_string_lossy();
    let backup_path = format!("{terminal_plist_path}.bak");
    let result: anyhow::Result<Option<String>> = async {
        let code = exec(
            "defaults",
            &["export", "com.apple.Terminal", &terminal_plist_path],
        )
        .await?;
        if code != 0 {
            return Ok(None);
        }
        if stat(Path::new(terminal_plist_path.as_ref())).await.is_err() {
            return Ok(None);
        }
        // The source deliberately ignores the second export's exit status.
        exec("defaults", &["export", "com.apple.Terminal", &backup_path]).await?;
        mark_terminal_setup_in_progress(&backup_path)?;
        Ok(Some(backup_path))
    }
    .await;
    match result {
        Ok(path) => path,
        Err(error) => {
            let error = LogError::new(error.to_string());
            #[cfg(test)]
            tests::log(&error);
            log_error(error);
            None
        }
    }
}

/// Maps to: CC `utils/appleTerminalBackup.ts#RestoreResult:73-80`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RestoreResult {
    Restored,
    NoBackup,
    Failed {
        #[serde(rename = "backupPath")]
        backup_path: String,
    },
}

/// Maps to: CC `utils/appleTerminalBackup.ts#checkAndRestoreTerminalBackup:82-124`.
pub async fn check_and_restore_terminal_backup() -> anyhow::Result<RestoreResult> {
    let (in_progress, backup_path) = get_terminal_recovery_info();
    if !in_progress {
        return Ok(RestoreResult::NoBackup);
    }
    let Some(backup_path) = backup_path else {
        mark_terminal_setup_complete()?;
        return Ok(RestoreResult::NoBackup);
    };
    if stat(Path::new(&backup_path)).await.is_err() {
        mark_terminal_setup_complete()?;
        return Ok(RestoreResult::NoBackup);
    }
    let result: anyhow::Result<RestoreResult> = async {
        let code = exec("defaults", &["import", "com.apple.Terminal", &backup_path]).await?;
        if code != 0 {
            return Ok(RestoreResult::Failed {
                backup_path: backup_path.clone(),
            });
        }
        exec("killall", &["cfprefsd"]).await?;
        mark_terminal_setup_complete()?;
        Ok(RestoreResult::Restored)
    }
    .await;
    match result {
        Ok(result) => Ok(result),
        Err(error) => {
            let error = LogError::new(format!(
                "Failed to restore Terminal.app settings with: Error: {error}"
            ));
            #[cfg(test)]
            tests::log(&error);
            log_error(error);
            mark_terminal_setup_complete()?;
            Ok(RestoreResult::Failed { backup_path })
        }
    }
}

// Async I/O carriers for the source's imported execFileNoThrow/stat calls.
// No backup policy lives in these adapters; subprocess work cannot block TUI.
async fn exec(program: &str, args: &[&str]) -> anyhow::Result<i32> {
    #[cfg(test)]
    if let Some(result) = tests::exec(program, args) {
        return result;
    }
    let program = program.to_owned();
    let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
    let cwd =
        std::env::current_dir().unwrap_or_else(|_| crate::bootstrap::state::get_original_cwd());
    Ok(tokio::task::spawn_blocking(move || {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        super::exec_file_no_throw::exec_file_no_throw_with_cwd(
            &program,
            &args,
            std::time::Duration::from_secs(600),
            Some(&cwd),
            true,
        )
        .code
    })
    .await?)
}

async fn stat(path: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    if let Some(result) = tests::stat(path) {
        return result;
    }
    tokio::fs::metadata(path).await.map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};
    use serde_json::{Value, json};
    use std::cell::RefCell;
    use std::collections::VecDeque;

    #[derive(Default)]
    struct State {
        config: Value,
        events: Vec<Value>,
        logs: Vec<Value>,
        codes: VecDeque<i32>,
        stat_fails: bool,
        save_failures: usize,
    }
    thread_local! {
        static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    }

    pub(super) fn is_active() -> bool {
        STATE.with(|state| state.borrow().is_some())
    }

    pub(super) fn log(error: &LogError) {
        STATE.with(|cell| {
            if let Some(state) = cell.borrow_mut().as_mut() {
                state
                    .logs
                    .push(json!(format!("{}: {}", error.name, error.message)));
            }
        });
    }

    pub(super) fn save(path: Option<&str>) -> Option<anyhow::Result<()>> {
        STATE.with(|cell| {
            let mut cell = cell.borrow_mut();
            let state = cell.as_mut()?;
            let mut config = state.config.clone();
            config["appleTerminalSetupInProgress"] = json!(path.is_some());
            if let Some(path) = path {
                config["appleTerminalBackupPath"] = json!(path);
            }
            state.events.push(json!(["save", config]));
            if state.save_failures > 0 {
                state.save_failures -= 1;
                return Some(Err(anyhow::anyhow!("save failed")));
            }
            state.config = config;
            Some(Ok(()))
        })
    }

    pub(super) fn recovery_info() -> Option<(bool, Option<String>)> {
        STATE.with(|cell| {
            let cell = cell.borrow();
            let state = cell.as_ref()?;
            Some((
                state.config["appleTerminalSetupInProgress"]
                    .as_bool()
                    .unwrap_or(false),
                state.config["appleTerminalBackupPath"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned),
            ))
        })
    }

    pub(super) fn exec(program: &str, args: &[&str]) -> Option<anyhow::Result<i32>> {
        STATE.with(|cell| {
            let mut cell = cell.borrow_mut();
            let state = cell.as_mut()?;
            state.events.push(json!(["exec", program, args]));
            Some(Ok(state.codes.pop_front().unwrap_or(0)))
        })
    }

    pub(super) fn stat(path: &Path) -> Option<std::io::Result<()>> {
        STATE.with(|cell| {
            let mut cell = cell.borrow_mut();
            let state = cell.as_mut()?;
            state.events.push(json!(["stat", path]));
            Some(if state.stat_fails {
                Err(std::io::Error::from(std::io::ErrorKind::NotFound))
            } else {
                Ok(())
            })
        })
    }

    struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            STATE.with(|cell| *cell.borrow_mut() = None);
        }
    }

    #[test]
    fn apple_terminal_backup_markers_match_official_persisted_config() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let root =
            std::env::temp_dir().join(format!("cometix-terminal-backup-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", root.to_str().unwrap());
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        mark_terminal_setup_in_progress("/fixture/Terminal.plist.bak").unwrap();
        assert_eq!(
            get_terminal_recovery_info(),
            (true, Some("/fixture/Terminal.plist.bak".into()))
        );
        mark_terminal_setup_complete().unwrap();
        let config: Value = serde_json::from_slice(
            &std::fs::read(crate::utils::env::get_global_claude_file()).unwrap(),
        )
        .unwrap();
        // CC utils/appleTerminalBackup.ts:7-20 clears only the progress flag;
        // the backup path intentionally survives both in memory and on disk.
        assert_eq!(config["appleTerminalSetupInProgress"], false);
        assert_eq!(
            config["appleTerminalBackupPath"],
            "/fixture/Terminal.plist.bak"
        );
        assert_eq!(
            get_terminal_recovery_info(),
            (false, Some("/fixture/Terminal.plist.bak".into()))
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn apple_terminal_backup_matches_official_bun_effect_order_and_recovery() {
        let rows: Vec<Value> = serde_json::from_str(include_str!(
            "../../tests/fixtures/oracles/terminal-setup-0913/apple/oracle.json"
        )).unwrap();
        for row in rows {
            let _guard = Guard;
            let input = &row["input"];
            STATE.with(|cell| {
                *cell.borrow_mut() = Some(State {
                    config: input["config"].clone(),
                    codes: input["codes"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|code| code.as_i64().unwrap() as i32)
                        .collect(),
                    stat_fails: input["statFails"].as_bool().unwrap_or(false),
                    save_failures: input["saveFailures"].as_u64().unwrap_or(0) as usize,
                    ..Default::default()
                })
            });
            let result = if input["operation"] == "backup" {
                json!(backup_terminal_preferences().await)
            } else {
                match check_and_restore_terminal_backup().await {
                    Ok(result) => serde_json::to_value(result).unwrap(),
                    Err(error) => json!({"thrown": error.to_string()}),
                }
            };
            // CC utils/appleTerminalBackup.ts:37-71,82-124, executed by Bun.
            // Exit failures and config exceptions differ in whether the recovery
            // marker survives; the full event sequence also pins backup order.
            assert_eq!(result, row["result"], "{} result", row["name"]);
            STATE.with(|cell| {
                let cell = cell.borrow();
                let state = cell.as_ref().unwrap();
                assert_eq!(state.config, row["config"], "{} config", row["name"]);
                assert_eq!(json!(state.events), row["events"], "{} events", row["name"]);
                assert_eq!(json!(state.logs), row["logs"], "{} logs", row["name"]);
            });
        }
    }
}
