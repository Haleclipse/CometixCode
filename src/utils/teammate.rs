//! Teammate utilities for agent swarm coordination.
//!
//! Maps to: CC `utils/teammate.ts`.
//!
//! Claude Code resolves teammate identity in this order:
//! 1. in-process teammate context (`utils/teammateContext.ts` AsyncLocalStorage),
//! 2. process-local dynamic team context populated from hidden teammate CLI args.
//!
//! CometixCode already ports the in-process context shape in
//! `utils/teammate_context.rs`; this module ports the dynamic process teammate
//! context used by tmux-pane teammates. It intentionally does not invent an
//! alternate global environment contract: pane teammates are identified through
//! the official `--agent-id`, `--agent-name`, and `--team-name` CLI args.

#[cfg(test)]
use crate::utils::test_env::TestStateLock;
use std::sync::{LazyLock, RwLock};

/// Maps to: CC `utils/teammate.ts` `dynamicTeamContext` object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DynamicTeamContext {
    pub agent_id: String,
    pub agent_name: String,
    pub team_name: String,
    pub color: Option<String>,
    pub plan_mode_required: bool,
    pub parent_session_id: Option<String>,
}

static DYNAMIC_TEAM_CONTEXT: LazyLock<RwLock<Option<DynamicTeamContext>>> =
    LazyLock::new(|| RwLock::new(None));

#[cfg(test)]
pub(crate) static TEST_TEAMMATE_CONTEXT_LOCK: LazyLock<TestStateLock> =
    LazyLock::new(TestStateLock::new);

/// Maps to: CC `utils/teammate.ts#setDynamicTeamContext`.
pub fn set_dynamic_team_context(context: Option<DynamicTeamContext>) {
    if let Ok(mut slot) = DYNAMIC_TEAM_CONTEXT.write() {
        *slot = context;
    }
}

/// Maps to: CC `utils/teammate.ts#clearDynamicTeamContext`.
pub fn clear_dynamic_team_context() {
    set_dynamic_team_context(None);
}

/// Maps to: CC `utils/teammate.ts#getDynamicTeamContext`.
pub fn get_dynamic_team_context() -> Option<DynamicTeamContext> {
    DYNAMIC_TEAM_CONTEXT
        .read()
        .ok()
        .and_then(|context| context.clone())
}

/// Maps to: CC `utils/teammate.ts#getParentSessionId`.
///
/// CC short-circuits on the in-process ALS store first:
/// `if (inProcessCtx) return inProcessCtx.parentSessionId` — the context's
/// `parentSessionId` is a required string, so an in-process teammate never
/// falls through to `dynamicTeamContext`.
pub fn get_parent_session_id() -> Option<String> {
    if let Some(context) = crate::utils::teammate_context::get_teammate_context() {
        return Some(context.parent_session_id);
    }
    get_dynamic_team_context().and_then(|context| context.parent_session_id)
}

/// Maps to: CC `utils/teammate.ts#getAgentId`.
pub fn get_agent_id() -> Option<String> {
    if let Some(context) = crate::utils::teammate_context::get_teammate_context() {
        return Some(context.agent_id);
    }
    get_dynamic_team_context().map(|context| context.agent_id)
}

/// Maps to: CC `utils/teammate.ts#getAgentName`.
pub fn get_agent_name() -> Option<String> {
    if let Some(context) = crate::utils::teammate_context::get_teammate_context() {
        return Some(context.agent_name);
    }
    get_dynamic_team_context().map(|context| context.agent_name)
}

/// Maps to: CC `utils/teammate.ts#getTeamName`.
///
/// `team_context_name` represents official `AppState.teamContext.teamName` for
/// leader sessions. Priority per CC: in-process ALS context (unconditional
/// short-circuit) > `dynamicTeamContext?.teamName` (JS-truthy — an empty
/// string falls through) > the passed team context.
pub fn get_team_name(team_context_name: Option<&str>) -> Option<String> {
    if let Some(context) = crate::utils::teammate_context::get_teammate_context() {
        return Some(context.team_name);
    }
    get_dynamic_team_context()
        .map(|context| context.team_name)
        .filter(|team_name| !team_name.is_empty())
        .or_else(|| team_context_name.map(ToOwned::to_owned))
}

/// Maps to: CC `utils/teammate.ts#isTeammate`.
///
/// CC: any in-process ALS context means teammate; tmux teammates require BOTH
/// a truthy agent ID and a truthy team name on `dynamicTeamContext`.
pub fn is_teammate() -> bool {
    if crate::utils::teammate_context::is_in_process_teammate() {
        return true;
    }
    get_dynamic_team_context()
        .is_some_and(|context| !context.agent_id.is_empty() && !context.team_name.is_empty())
}

/// Maps to: CC `utils/teammate.ts#getTeammateColor`.
///
/// CC short-circuits on the ALS context even when its `color` is undefined.
pub fn get_teammate_color() -> Option<String> {
    if let Some(context) = crate::utils::teammate_context::get_teammate_context() {
        return context.color;
    }
    get_dynamic_team_context().and_then(|context| context.color)
}

/// Maps to: CC `utils/teammate.ts#isPlanModeRequired`.
/// Priority per CC: ALS > dynamicTeamContext > env var.
pub fn is_plan_mode_required() -> bool {
    if let Some(context) = crate::utils::teammate_context::get_teammate_context() {
        return context.plan_mode_required;
    }
    if let Some(context) = get_dynamic_team_context() {
        return context.plan_mode_required;
    }
    crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var(crate::utils::swarm::constants::PLAN_MODE_REQUIRED_ENV_VAR)
            .as_deref(),
    )
}

/// Maps to: CC `utils/teammate.ts#isTeamLead`.
///
/// `lead_agent_id` is official `AppState.teamContext.leadAgentId`.
pub fn is_team_lead(lead_agent_id: Option<&str>) -> bool {
    let Some(lead_agent_id) = lead_agent_id.filter(|value| !value.is_empty()) else {
        return false;
    };
    match get_agent_id() {
        Some(agent_id) => agent_id == lead_agent_id,
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::TEST_ENV_LOCK;

    fn context() -> DynamicTeamContext {
        DynamicTeamContext {
            agent_id: "reviewer@alpha".to_string(),
            agent_name: "reviewer".to_string(),
            team_name: "alpha".to_string(),
            color: Some("green".to_string()),
            plan_mode_required: true,
            parent_session_id: Some("parent-session".to_string()),
        }
    }

    #[test]
    fn dynamic_team_context_getters_match_official_teammate_identity_order() {
        let _lock = TEST_TEAMMATE_CONTEXT_LOCK.lock().unwrap();
        clear_dynamic_team_context();

        assert!(!is_teammate());
        assert_eq!(
            get_team_name(Some("leader-team")).as_deref(),
            Some("leader-team")
        );

        set_dynamic_team_context(Some(context()));

        assert!(is_teammate());
        assert_eq!(get_agent_id().as_deref(), Some("reviewer@alpha"));
        assert_eq!(get_agent_name().as_deref(), Some("reviewer"));
        assert_eq!(get_team_name(Some("leader-team")).as_deref(), Some("alpha"));
        assert_eq!(get_teammate_color().as_deref(), Some("green"));
        assert!(is_plan_mode_required());
        assert_eq!(get_parent_session_id().as_deref(), Some("parent-session"));

        clear_dynamic_team_context();
    }

    #[test]
    fn plan_mode_required_falls_back_to_official_env_when_not_dynamic_teammate() {
        let _lock = TEST_TEAMMATE_CONTEXT_LOCK.lock().unwrap();
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        clear_dynamic_team_context();
        crate::utils::process_env::remove(
            crate::utils::swarm::constants::PLAN_MODE_REQUIRED_ENV_VAR,
        );
        assert!(!is_plan_mode_required());

        crate::utils::process_env::set(
            crate::utils::swarm::constants::PLAN_MODE_REQUIRED_ENV_VAR,
            "1",
        );
        assert!(is_plan_mode_required());
        crate::utils::process_env::remove(
            crate::utils::swarm::constants::PLAN_MODE_REQUIRED_ENV_VAR,
        );
    }

    #[test]
    fn is_team_lead_matches_official_agent_id_or_legacy_no_agent_rule() {
        let _lock = TEST_TEAMMATE_CONTEXT_LOCK.lock().unwrap();
        clear_dynamic_team_context();
        assert!(!is_team_lead(None));
        assert!(is_team_lead(Some("team-lead@alpha")));

        set_dynamic_team_context(Some(context()));
        assert!(is_team_lead(Some("reviewer@alpha")));
        assert!(!is_team_lead(Some("team-lead@alpha")));

        clear_dynamic_team_context();
    }
}
