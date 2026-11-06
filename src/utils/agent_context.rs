//! Agent context for analytics attribution across concurrent in-process agents.
//!
//! Maps to: CC `utils/agentContext.ts`.
//!
//! Claude Code uses `AsyncLocalStorage`; Nyan uses `tokio::task_local!` so
//! backgrounded/concurrent agents keep isolated identity without mutating a
//! shared AppState slot.

use crate::utils::agent_swarms_enabled::is_agent_swarms_enabled;
use std::cell::RefCell;

/// Maps to: CC `agentContext.ts#SubagentContext`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubagentContext {
    pub agent_id: String,
    pub parent_session_id: Option<String>,
    pub subagent_name: Option<String>,
    pub is_built_in: Option<bool>,
    pub invoking_request_id: Option<String>,
    pub invocation_kind: Option<InvocationKind>,
    pub invocation_emitted: bool,
}

/// Maps to: CC `agentContext.ts#TeammateAgentContext`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TeammateAgentContext {
    pub agent_id: String,
    pub agent_name: String,
    pub team_name: String,
    pub agent_color: Option<String>,
    pub plan_mode_required: bool,
    pub parent_session_id: String,
    pub is_team_lead: bool,
    pub invoking_request_id: Option<String>,
    pub invocation_kind: Option<InvocationKind>,
    pub invocation_emitted: bool,
}

/// Maps to: CC `invocationKind` on agent contexts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvocationKind {
    Spawn,
    Resume,
}

impl InvocationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Spawn => "spawn",
            Self::Resume => "resume",
        }
    }
}

/// Maps to: CC `agentContext.ts#AgentContext`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentContext {
    Subagent(SubagentContext),
    Teammate(TeammateAgentContext),
}

impl AgentContext {
    pub fn agent_id(&self) -> &str {
        match self {
            Self::Subagent(context) => context.agent_id.as_str(),
            Self::Teammate(context) => context.agent_id.as_str(),
        }
    }

    fn invoking_request_id(&self) -> Option<&str> {
        match self {
            Self::Subagent(context) => context.invoking_request_id.as_deref(),
            Self::Teammate(context) => context.invoking_request_id.as_deref(),
        }
    }

    fn invocation_kind(&self) -> Option<InvocationKind> {
        match self {
            Self::Subagent(context) => context.invocation_kind,
            Self::Teammate(context) => context.invocation_kind,
        }
    }

    fn invocation_emitted(&self) -> bool {
        match self {
            Self::Subagent(context) => context.invocation_emitted,
            Self::Teammate(context) => context.invocation_emitted,
        }
    }

    fn mark_invocation_emitted(&mut self) {
        match self {
            Self::Subagent(context) => context.invocation_emitted = true,
            Self::Teammate(context) => context.invocation_emitted = true,
        }
    }
}

tokio::task_local! {
    static AGENT_CONTEXT: RefCell<AgentContext>;
}

/// Maps to: CC `agentContext.ts#getAgentContext`.
pub fn get_agent_context() -> Option<AgentContext> {
    AGENT_CONTEXT.try_with(|slot| slot.borrow().clone()).ok()
}

/// Maps to: CC `agentContext.ts#runWithAgentContext` for async work.
pub async fn run_with_agent_context<F, Fut, T>(context: AgentContext, f: F) -> T
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = T>,
{
    AGENT_CONTEXT.scope(RefCell::new(context), f()).await
}

/// Maps to: CC `agentContext.ts#isSubagentContext`.
pub fn is_subagent_context(context: Option<&AgentContext>) -> bool {
    matches!(context, Some(AgentContext::Subagent(_)))
}

/// Maps to: CC `agentContext.ts#isTeammateAgentContext`.
pub fn is_teammate_agent_context(context: Option<&AgentContext>) -> bool {
    if !is_agent_swarms_enabled() {
        return false;
    }
    matches!(context, Some(AgentContext::Teammate(_)))
}

/// Maps to: CC `agentContext.ts#getSubagentLogName`.
///
/// Built-in names are returned as-is; custom agents collapse to `user-defined`.
pub fn get_subagent_log_name() -> Option<String> {
    let AgentContext::Subagent(context) = get_agent_context()? else {
        return None;
    };
    let name = context.subagent_name.filter(|value| !value.is_empty())?;
    if context.is_built_in.unwrap_or(false) {
        Some(name)
    } else {
        Some("user-defined".to_string())
    }
}

/// Maps to: CC `agentContext.ts#consumeInvokingRequestId`.
pub fn consume_invoking_request_id() -> Option<(String, Option<InvocationKind>)> {
    AGENT_CONTEXT
        .try_with(|slot| {
            let mut context = slot.borrow_mut();
            let Some(request_id) = context.invoking_request_id().map(ToOwned::to_owned) else {
                return None;
            };
            if context.invocation_emitted() {
                return None;
            }
            let kind = context.invocation_kind();
            context.mark_invocation_emitted();
            Some((request_id, kind))
        })
        .ok()
        .flatten()
}

/// Helper for AgentTool/resume spawn contexts (CC inline object literals).
pub fn subagent_spawn_context(
    agent_id: impl Into<String>,
    subagent_name: impl Into<String>,
    is_built_in: bool,
    invoking_request_id: Option<String>,
) -> AgentContext {
    AgentContext::Subagent(SubagentContext {
        agent_id: agent_id.into(),
        parent_session_id: crate::utils::teammate::get_parent_session_id(),
        subagent_name: Some(subagent_name.into()),
        is_built_in: Some(is_built_in),
        invoking_request_id,
        invocation_kind: Some(InvocationKind::Spawn),
        invocation_emitted: false,
    })
}

/// Helper for resumeAgentBackground spawn contexts.
pub fn subagent_resume_context(
    agent_id: impl Into<String>,
    subagent_name: impl Into<String>,
    is_built_in: bool,
    invoking_request_id: Option<String>,
) -> AgentContext {
    AgentContext::Subagent(SubagentContext {
        agent_id: agent_id.into(),
        parent_session_id: crate::utils::teammate::get_parent_session_id(),
        subagent_name: Some(subagent_name.into()),
        is_built_in: Some(is_built_in),
        invoking_request_id,
        invocation_kind: Some(InvocationKind::Resume),
        invocation_emitted: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn run_with_agent_context_isolates_task_local_store() {
        let outer = subagent_spawn_context("agent-outer", "explore", true, None);
        run_with_agent_context(outer.clone(), || async {
            assert_eq!(
                get_agent_context().as_ref().map(AgentContext::agent_id),
                Some("agent-outer")
            );
            assert_eq!(get_subagent_log_name().as_deref(), Some("explore"));

            let inner = subagent_spawn_context("agent-inner", "reviewer", false, None);
            run_with_agent_context(inner, || async {
                assert_eq!(
                    get_agent_context().as_ref().map(AgentContext::agent_id),
                    Some("agent-inner")
                );
                assert_eq!(get_subagent_log_name().as_deref(), Some("user-defined"));
            })
            .await;

            assert_eq!(
                get_agent_context().as_ref().map(AgentContext::agent_id),
                Some("agent-outer")
            );
        })
        .await;

        assert!(get_agent_context().is_none());
    }

    #[tokio::test]
    async fn consume_invoking_request_id_is_once_per_invocation() {
        let context =
            subagent_spawn_context("agent-1", "explore", true, Some("req_spawn".to_string()));
        run_with_agent_context(context, || async {
            let first = consume_invoking_request_id();
            assert_eq!(
                first,
                Some(("req_spawn".to_string(), Some(InvocationKind::Spawn)))
            );
            assert!(consume_invoking_request_id().is_none());
        })
        .await;
    }
}
