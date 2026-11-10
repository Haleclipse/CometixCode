//! Maps to: CC `commands/rename/rename.ts:21-82`.
//!
//! Explicit names complete synchronously because Rust's session-storage
//! metadata append boundary is synchronous. Empty args return a typed request
//! for REPL to await `generateSessionName` without blocking the retained frame.
//! Local title, agent-name, and AppState updates remain atomic from the
//! command's perspective.

use crate::state::store::AppStore;
use crate::tool::ToolUseContext;
use crate::types::message::Message;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenameGenerationRequest {
    pub messages: Vec<Message>,
    pub session_id: String,
    pub full_path: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RenameCall {
    Output { text: String, is_error: bool },
    Generate(RenameGenerationRequest),
}

/// Persists both official rename records and updates prompt-bar identity.
pub fn save_session_name(
    session_id: &str,
    new_name: &str,
    full_path: Option<&Path>,
    app_store: &AppStore,
) -> anyhow::Result<()> {
    crate::utils::session_storage::save_custom_title(session_id, new_name, full_path)?;
    crate::utils::session_storage::save_agent_name(session_id, new_name, full_path)?;
    app_store.replace_with(|state| {
        let color = state
            .standalone_agent_context
            .as_ref()
            .and_then(|context| context.color.clone());
        state.standalone_agent_context = Some(
            crate::utils::session_restore::RestoredStandaloneAgentContext {
                name: new_name.to_string(),
                color,
            },
        );
    });
    Ok(())
}

/// Maps to: CC `commands/rename/rename.ts:21-82::call` up to its awaited
/// model-generation branch.
pub fn call(context: &ToolUseContext, args: &str) -> RenameCall {
    if crate::utils::teammate::is_teammate() {
        return RenameCall::Output {
            text: "Cannot rename: This session is a swarm teammate. Teammate names are set by the team leader.".to_string(),
            is_error: false,
        };
    }

    let new_name = args.trim();
    let session_id = crate::bootstrap::state::get_session_id();
    let full_path = context
        .resume_restore_stores
        .full_path
        .as_deref()
        .map(PathBuf::from);
    if new_name.is_empty() {
        return RenameCall::Generate(RenameGenerationRequest {
            messages: crate::utils::messages::get_messages_after_compact_boundary(
                &context.messages,
            ),
            session_id,
            full_path,
        });
    }

    let Some(app_store) = context.app_store.store.as_ref() else {
        return RenameCall::Output {
            text: "Cannot rename: app state is unavailable.".to_string(),
            is_error: true,
        };
    };
    match save_session_name(&session_id, new_name, full_path.as_deref(), app_store) {
        Ok(()) => RenameCall::Output {
            text: format!("Session renamed to: {new_name}"),
            is_error: false,
        },
        Err(error) => RenameCall::Output {
            text: format!("Failed to rename session: {error}"),
            is_error: true,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::message::{
        AssistantContent, AssistantMessage, CompactMetadata, SystemMessage,
    };
    use chrono::Utc;

    fn assistant(text: &str) -> Message {
        Message::Assistant(AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            content: vec![AssistantContent::Text(text.to_string())],
            model: None,
            stop_reason: None,
            usage: None,
        })
    }

    #[test]
    fn rename_empty_args_uses_messages_after_official_compact_boundary() {
        let mut context = ToolUseContext::default().with_messages(vec![
            assistant("old"),
            Message::System(SystemMessage::compact_boundary(Some(
                CompactMetadata::default(),
            ))),
            assistant("new"),
        ]);
        context.resume_restore_stores.full_path = Some("/tmp/session.jsonl".to_string());

        let RenameCall::Generate(request) = call(&context, "  ") else {
            panic!("expected generation request");
        };
        assert_eq!(request.messages.len(), 2);
        assert!(matches!(
            &request.messages[1],
            Message::Assistant(message)
                if message.content == vec![AssistantContent::Text("new".to_string())]
        ));
        assert_eq!(
            request.full_path.as_deref(),
            Some(Path::new("/tmp/session.jsonl"))
        );
    }

    #[test]
    fn rename_rejects_teammates_with_official_copy() {
        struct RestoreTeammate(Option<crate::utils::teammate::DynamicTeamContext>);
        impl Drop for RestoreTeammate {
            fn drop(&mut self) {
                crate::utils::teammate::set_dynamic_team_context(self.0.clone());
            }
        }

        let _lock = crate::utils::teammate::TEST_TEAMMATE_CONTEXT_LOCK
            .lock()
            .unwrap();
        let _restore = RestoreTeammate(crate::utils::teammate::get_dynamic_team_context());
        crate::utils::teammate::set_dynamic_team_context(Some(
            crate::utils::teammate::DynamicTeamContext {
                agent_id: "agent-1".to_string(),
                agent_name: "worker".to_string(),
                team_name: "team".to_string(),
                color: None,
                plan_mode_required: false,
                parent_session_id: None,
            },
        ));

        assert_eq!(
            call(&ToolUseContext::default(), "new-name"),
            RenameCall::Output {
                text: "Cannot rename: This session is a swarm teammate. Teammate names are set by the team leader.".to_string(),
                is_error: false,
            }
        );
    }

    #[test]
    fn rename_explicit_name_trims_persists_and_updates_app_state() {
        let store = AppStore::new(crate::state::app_state_store::AppState::default(), None);
        let context = ToolUseContext::default().with_app_store(store.clone());
        assert_eq!(
            call(&context, "  fix-login-bug  "),
            RenameCall::Output {
                text: "Session renamed to: fix-login-bug".to_string(),
                is_error: false,
            }
        );
        assert_eq!(
            store
                .get()
                .standalone_agent_context
                .as_ref()
                .map(|context| context.name.as_str()),
            Some("fix-login-bug")
        );
    }

    #[test]
    fn save_session_name_updates_prompt_bar_context_like_official() {
        let store = AppStore::new(crate::state::app_state_store::AppState::default(), None);
        // Persistence is disabled by default in tests, but the same API still
        // exercises the metadata cache and AppState boundary without touching
        // the developer's real session files.
        save_session_name("rename-test", "new-name", None, &store).unwrap();
        assert_eq!(
            store
                .get()
                .standalone_agent_context
                .as_ref()
                .map(|context| context.name.as_str()),
            Some("new-name")
        );
    }
}
