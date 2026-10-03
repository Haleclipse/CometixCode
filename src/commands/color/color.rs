//! Maps to: CC `commands/color/color.ts`.

use crate::tool::ToolUseContext;
use crate::tools::agent_tool::agent_color_manager::{AGENT_COLORS, parse_agent_color_name};
use crate::utils::session_storage::{get_transcript_path, save_agent_color};

/// Maps to: CC `commands/color/color.ts:18` `RESET_ALIASES`.
const RESET_ALIASES: [&str; 5] = ["default", "reset", "none", "gray", "grey"];

/// Maps to: CC `commands/color/color.ts#call:20-93`.
/// The returned text is delivered by the command dispatcher through the
/// existing system-display `onDone` carrier. Errors remain errors (CC has no
/// catch); no success text or AppState update precedes successful persistence.
pub async fn call(context: &ToolUseContext, args: &str) -> anyhow::Result<String> {
    if crate::utils::teammate::is_teammate() {
        return Ok("Cannot set color: This session is a swarm teammate. Teammate colors are assigned by the team leader.".to_string());
    }

    // ECMAScript String.trim includes BOM and excludes U+0085.
    let trimmed =
        args.trim_matches(|ch: char| (ch.is_whitespace() && ch != '\u{85}') || ch == '\u{feff}');
    if trimmed.is_empty() {
        let color_list = AGENT_COLORS.map(|color| color.official_name()).join(", ");
        return Ok(format!(
            "Please provide a color. Available colors: {color_list}, default"
        ));
    }

    let color_arg = trimmed.to_lowercase();
    if RESET_ALIASES.contains(&color_arg.as_str()) {
        let session_id = crate::bootstrap::state::get_session_id();
        let full_path = get_transcript_path(None);
        let app_store = context
            .app_store
            .store
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Cannot set color: app state is unavailable."))?;

        // CC :51 awaits saveAgentColor. The existing synchronous transcript
        // writer must run on the blocking pool, never on the retained frame.
        tokio::task::spawn_blocking(move || {
            save_agent_color(&session_id, "default", Some(&full_path))
        })
        .await??;

        app_store.replace_with(|state| {
            let standalone = state.standalone_agent_context.get_or_insert_with(|| {
                crate::utils::session_restore::RestoredStandaloneAgentContext {
                    name: String::new(),
                    color: None,
                }
            });
            standalone.color = None;
        });
        return Ok("Session color reset to default".to_string());
    }

    if parse_agent_color_name(&color_arg).is_none() {
        let color_list = AGENT_COLORS.map(|color| color.official_name()).join(", ");
        return Ok(format!(
            "Invalid color \"{color_arg}\". Available colors: {color_list}, default"
        ));
    }

    let session_id = crate::bootstrap::state::get_session_id();
    let full_path = get_transcript_path(None);
    let app_store = context
        .app_store
        .store
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Cannot set color: app state is unavailable."))?;
    let saved_color = color_arg.clone();
    tokio::task::spawn_blocking(move || {
        save_agent_color(&session_id, &saved_color, Some(&full_path))
    })
    .await??;

    app_store.replace_with(|state| {
        let standalone = state.standalone_agent_context.get_or_insert_with(|| {
            crate::utils::session_restore::RestoredStandaloneAgentContext {
                name: String::new(),
                color: None,
            }
        });
        standalone.color = Some(color_arg.clone());
    });
    Ok(format!("Session color set to: {color_arg}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::app_state_store::AppState;
    use crate::state::store::AppStore;
    use crate::utils::session_restore::RestoredStandaloneAgentContext;
    use crate::utils::session_storage::{
        clear_session_metadata, get_current_session_metadata, restore_session_metadata,
    };
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    struct SessionFixture {
        root: std::path::PathBuf,
        previous_session: String,
        previous_dir: Option<std::path::PathBuf>,
        previous_metadata: crate::utils::session_storage::SessionMetadataCache,
    }

    impl SessionFixture {
        fn new() -> Self {
            let fixture = Self {
                root: std::env::temp_dir().join(format!("cometix-color-{}", uuid::Uuid::new_v4())),
                previous_session: crate::bootstrap::state::get_session_id(),
                previous_dir: crate::bootstrap::state::get_session_project_dir(),
                previous_metadata: get_current_session_metadata(),
            };
            clear_session_metadata();
            crate::bootstrap::state::switch_session("color-session", Some(fixture.root.clone()));
            fixture
        }
    }

    impl Drop for SessionFixture {
        fn drop(&mut self) {
            clear_session_metadata();
            crate::bootstrap::state::switch_session(
                &self.previous_session,
                self.previous_dir.clone(),
            );
            restore_session_metadata(&self.previous_metadata);
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn color_validation_matches_official_copy_membership_and_js_trim() {
        // CC color.ts:34-42,66-72; reset aliases are tested through persistence below.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let context = ToolUseContext::default();
        let colors = "red, blue, green, yellow, purple, orange, pink, cyan, default";
        for args in ["", " \t\n", "\u{feff}"] {
            assert_eq!(
                runtime.block_on(call(&context, args)).unwrap(),
                format!("Please provide a color. Available colors: {colors}")
            );
        }
        for (args, shown) in [
            ("  BLACK  ", "black"),
            ("green blue", "green blue"),
            ("\u{85}", "\u{85}"),
        ] {
            assert_eq!(
                runtime.block_on(call(&context, args)).unwrap(),
                format!("Invalid color \"{shown}\". Available colors: {colors}")
            );
        }
    }

    #[test]
    fn color_teammate_guard_matches_official_before_argument_validation() {
        // CC color.ts:25-32 is the first branch, including for empty/invalid args.
        struct Restore(Option<crate::utils::teammate::DynamicTeamContext>);
        impl Drop for Restore {
            fn drop(&mut self) {
                crate::utils::teammate::set_dynamic_team_context(self.0.clone());
            }
        }
        let _lock = crate::utils::teammate::TEST_TEAMMATE_CONTEXT_LOCK
            .lock()
            .unwrap();
        let _restore = Restore(crate::utils::teammate::get_dynamic_team_context());
        crate::utils::teammate::set_dynamic_team_context(Some(
            crate::utils::teammate::DynamicTeamContext {
                agent_id: "worker".to_string(),
                agent_name: "worker".to_string(),
                team_name: "team".to_string(),
                color: None,
                plan_mode_required: false,
                parent_session_id: None,
            },
        ));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        for args in ["", "green", "black", "reset"] {
            assert_eq!(
                runtime
                    .block_on(call(&ToolUseContext::default(), args))
                    .unwrap(),
                "Cannot set color: This session is a swarm teammate. Teammate colors are assigned by the team leader."
            );
        }
    }

    #[test]
    fn color_persistence_and_reset_matches_official_state_and_cold_restore() {
        // CC color.ts:45-63,75-92: disk first, same name, default sentinel.
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _write = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let _history = EnvVarGuard::unset("CLAUDE_CODE_SKIP_PROMPT_HISTORY");
        let fixture = SessionFixture::new();
        let store = AppStore::new(AppState::default(), None);
        let context = ToolUseContext::default().with_app_store(store.clone());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        assert_eq!(
            runtime
                .block_on(call(&context, "\u{feff} GREEN \u{feff}"))
                .unwrap(),
            "Session color set to: green"
        );
        assert_eq!(
            store.get().standalone_agent_context,
            Some(RestoredStandaloneAgentContext {
                name: String::new(),
                color: Some("green".to_string())
            })
        );
        store.replace_with(|state| {
            state.standalone_agent_context.as_mut().unwrap().name = "kept-name".to_string()
        });
        for alias in RESET_ALIASES {
            assert_eq!(
                runtime
                    .block_on(call(&context, &alias.to_uppercase()))
                    .unwrap(),
                "Session color reset to default"
            );
            assert_eq!(
                store.get().standalone_agent_context,
                Some(RestoredStandaloneAgentContext {
                    name: "kept-name".to_string(),
                    color: None
                })
            );
            assert_eq!(
                get_current_session_metadata().agent_color.as_deref(),
                Some("default")
            );
        }
        let path = fixture.root.join("color-session.jsonl");
        let contents = std::fs::read_to_string(&path).unwrap();
        let rows: Vec<serde_json::Value> = contents
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(rows.len(), 6);
        assert_eq!(
            rows[0],
            serde_json::json!({"type":"agent-color", "agentColor":"green", "sessionId":"color-session"})
        );
        assert!(rows[1..].iter().all(|row| row["agentColor"] == "default"));
        let restored = crate::utils::session_storage::load_session_structured_from_path(&path);
        assert_eq!(
            restored
                .agent_colors
                .get("color-session")
                .map(String::as_str),
            Some("default")
        );
        assert_eq!(
            crate::utils::session_restore::compute_standalone_agent_context(
                Some("kept-name"),
                Some("default")
            ),
            Some(RestoredStandaloneAgentContext {
                name: "kept-name".to_string(),
                color: None
            })
        );
    }

    #[test]
    fn color_write_error_matches_official_no_success_or_state_mutation() {
        // CC color.ts:51/79 await has no catch; setAppState is after that await.
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _write = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let fixture = SessionFixture::new();
        let path = fixture.root.join("color-session.jsonl");
        std::fs::create_dir_all(path).unwrap();
        let store = AppStore::new(AppState::default(), None);
        let context = ToolUseContext::default().with_app_store(store.clone());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        for args in ["red", "default"] {
            assert!(runtime.block_on(call(&context, args)).is_err());
            assert!(store.get().standalone_agent_context.is_none());
            assert!(get_current_session_metadata().agent_color.is_none());
        }
    }
}
