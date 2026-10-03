//! Partial port of CC `utils/concurrentSessions.ts`.
//!
//! Maps to: CC `utils/concurrentSessions.ts:105-136` (`updatePidFile` and
//! `updateSessionName`). Session registration/listing/activity remain in the
//! local background-session backlog; rename only needs this best-effort patch.

fn current_pid_file() -> std::path::PathBuf {
    crate::utils::env_utils::get_claude_config_home_dir()
        .join("sessions")
        .join(format!("{}.json", std::process::id()))
}

/// Maps to: CC `utils/concurrentSessions.ts:131-136` `updateSessionName`.
/// Missing/unreadable registry files are expected and never fail `/rename`.
pub fn update_session_name(name: Option<&str>) {
    let Some(name) = name.filter(|name| !name.is_empty()) else {
        return;
    };
    let path = current_pid_file();
    let result = (|| -> anyhow::Result<()> {
        let mut data = serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(&path)?)?;
        let object = data
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("session registry payload is not an object"))?;
        object.insert(
            "name".to_string(),
            serde_json::Value::String(name.to_string()),
        );
        std::fs::write(&path, serde_json::to_string(&data)?)?;
        Ok(())
    })();
    if let Err(error) = result {
        crate::utils::debug::log_for_debugging(&format!(
            "[concurrentSessions] updatePidFile failed: {error}"
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[test]
    fn update_session_name_matches_official_best_effort_pid_patch() {
        struct RestoreConfigDir {
            previous: Option<EnvVarGuard>,
            root: std::path::PathBuf,
        }
        impl Drop for RestoreConfigDir {
            fn drop(&mut self) {
                drop(self.previous.take());
                let _ = std::fs::remove_dir_all(&self.root);
            }
        }

        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-concurrent-session-{}",
            uuid::Uuid::new_v4()
        ));
        let sessions = root.join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let path = sessions.join(format!("{}.json", std::process::id()));
        std::fs::write(
            &path,
            r#"{"pid":123,"sessionId":"session-1","custom":"keep"}"#,
        )
        .unwrap();
        let _restore = RestoreConfigDir {
            previous: Some(EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root)),
            root: root.clone(),
        };

        update_session_name(Some("renamed-session"));
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(value["name"], "renamed-session");
        assert_eq!(value["custom"], "keep");
    }

    #[test]
    fn update_session_name_ignores_empty_or_missing_registry() {
        update_session_name(None);
        update_session_name(Some(""));
    }
}
