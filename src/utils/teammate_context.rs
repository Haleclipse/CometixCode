//! Runtime context for in-process teammates.
//!
//! Maps to: CC `utils/teammateContext.ts`.
//!
//! Claude Code stores this in `AsyncLocalStorage`; the Rust equivalent is a
//! `tokio::task_local!` key scoped over the teammate's future by
//! `run_with_teammate_context` (entered by `utils/swarm/in_process_runner.rs`,
//! CC inProcessRunner.ts:1160). Known carrier difference vs `AsyncLocalStorage`:
//! a `tokio::spawn` or `std::thread::spawn` inside the scope does NOT inherit
//! the value — spawn points that need teammate identity must re-enter the scope
//! explicitly. The ones on a teammate's turn do:
//! `query.rs#spawn_query` (query actor thread),
//! `services/tools/tool_orchestration.rs#run_tools_concurrently` and
//! `services/tools/streaming_tool_executor.rs#execute_tool` (tool workers, so
//! `teammate.rs` helpers and `filterToolsForAgent`'s teammate exception read
//! the live identity inside `Tool.call`). Any NEW handoff to a fresh
//! thread/task on that path must capture and re-enter as well.

use crate::tool::AbortController;

tokio::task_local! {
    /// Maps to: CC `teammateContext.ts` `teammateContextStorage`
    /// (`new AsyncLocalStorage<TeammateContext>()`).
    static TEAMMATE_CONTEXT: TeammateContext;
}

/// Maps to: CC `utils/teammateContext.ts#TeammateContext`.
#[derive(Clone, Debug, PartialEq)]
pub struct TeammateContext {
    pub agent_id: String,
    pub agent_name: String,
    pub team_name: String,
    pub color: Option<String>,
    pub plan_mode_required: bool,
    pub parent_session_id: String,
    pub is_in_process: bool,
    pub abort_controller: AbortController,
}

/// Maps to: CC `utils/teammateContext.ts#createTeammateContext` input object.
#[derive(Clone, Debug, PartialEq)]
pub struct CreateTeammateContextConfig {
    pub agent_id: String,
    pub agent_name: String,
    pub team_name: String,
    pub color: Option<String>,
    pub plan_mode_required: bool,
    pub parent_session_id: String,
    pub abort_controller: AbortController,
}

/// Maps to: CC `utils/teammateContext.ts#getTeammateContext` —
/// `teammateContextStorage.getStore()`; `None` when the current future is not
/// running inside a teammate scope.
pub fn get_teammate_context() -> Option<TeammateContext> {
    TEAMMATE_CONTEXT.try_with(|context| context.clone()).ok()
}

/// Maps to: CC `utils/teammateContext.ts#runWithTeammateContext` —
/// `teammateContextStorage.run(context, fn)`.
pub async fn run_with_teammate_context<F>(context: TeammateContext, future: F) -> F::Output
where
    F: std::future::Future,
{
    TEAMMATE_CONTEXT.scope(context, future).await
}

/// Maps to: CC `utils/teammateContext.ts#isInProcessTeammate` —
/// `teammateContextStorage.getStore() !== undefined`.
pub fn is_in_process_teammate() -> bool {
    TEAMMATE_CONTEXT.try_with(|_| ()).is_ok()
}

/// Carries the current teammate scope across a thread boundary.
///
/// `AsyncLocalStorage` propagates into every continuation of the entering
/// async context, so in CC a teammate's tool calls — including the concurrent
/// batch — still observe `getTeammateContext()`. `tokio::task_local!` is
/// thread-local-backed and does NOT survive `std::thread::spawn`, so every
/// place that hands work to a fresh thread must capture the scope on the
/// spawning side and re-enter it inside. Returns `None` outside a scope, in
/// which case the closure runs unchanged.
pub fn capture_teammate_context() -> Option<TeammateContext> {
    get_teammate_context()
}

/// Re-enters a captured scope around a synchronous body (the thread-side half
/// of [`capture_teammate_context`]). `None` runs the body unchanged.
pub fn with_teammate_context_sync<T>(
    context: Option<TeammateContext>,
    body: impl FnOnce() -> T,
) -> T {
    match context {
        Some(context) => TEAMMATE_CONTEXT.sync_scope(context, body),
        None => body(),
    }
}

/// Maps to: CC `utils/teammateContext.ts#createTeammateContext`.
pub fn create_teammate_context(config: CreateTeammateContextConfig) -> TeammateContext {
    TeammateContext {
        agent_id: config.agent_id,
        agent_name: config.agent_name,
        team_name: config.team_name,
        color: config.color,
        plan_mode_required: config.plan_mode_required,
        parent_session_id: config.parent_session_id,
        is_in_process: true,
        abort_controller: config.abort_controller,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_teammate_context_sets_in_process_discriminator() {
        let abort_controller = AbortController::default();
        let context = create_teammate_context(CreateTeammateContextConfig {
            agent_id: "researcher@team".to_string(),
            agent_name: "researcher".to_string(),
            team_name: "team".to_string(),
            color: Some("blue".to_string()),
            plan_mode_required: true,
            parent_session_id: "parent-session".to_string(),
            abort_controller: abort_controller.clone(),
        });

        assert!(context.is_in_process);
        assert_eq!(context.agent_id, "researcher@team");
        assert_eq!(context.agent_name, "researcher");
        assert_eq!(context.team_name, "team");
        assert_eq!(context.color.as_deref(), Some("blue"));
        assert!(context.plan_mode_required);
        assert_eq!(context.parent_session_id, "parent-session");
        abort_controller.abort();
        assert!(context.abort_controller.is_aborted());
    }

    fn sample_context(agent_id: &str) -> TeammateContext {
        create_teammate_context(CreateTeammateContextConfig {
            agent_id: agent_id.to_string(),
            agent_name: "researcher".to_string(),
            team_name: "team".to_string(),
            color: None,
            plan_mode_required: false,
            parent_session_id: "parent-session".to_string(),
            abort_controller: AbortController::default(),
        })
    }

    #[tokio::test]
    async fn teammate_scope_is_readable_inside_and_absent_outside() {
        assert!(get_teammate_context().is_none());
        assert!(!is_in_process_teammate());

        let seen = run_with_teammate_context(sample_context("researcher@team"), async {
            assert!(is_in_process_teammate());
            get_teammate_context().map(|context| context.agent_id)
        })
        .await;
        assert_eq!(seen.as_deref(), Some("researcher@team"));

        assert!(get_teammate_context().is_none());
        assert!(!is_in_process_teammate());
    }

    /// The carrier difference that matters: `AsyncLocalStorage` reaches a
    /// teammate's tool workers automatically, but `tokio::task_local!` is
    /// thread-local-backed, so a bare `std::thread::spawn` sees nothing. Tool
    /// execution forks real threads (`tool_orchestration.rs`,
    /// `streaming_tool_executor.rs`), which is why those sites capture and
    /// re-enter the scope.
    #[tokio::test]
    async fn teammate_scope_needs_explicit_carry_across_a_thread_boundary() {
        let seen = run_with_teammate_context(sample_context("worker@team"), async {
            // Bare spawn: the scope does NOT cross.
            let bare = std::thread::spawn(is_in_process_teammate).join().unwrap();

            // Captured + re-entered: it does.
            let carried_context = capture_teammate_context();
            let carried = std::thread::spawn(move || {
                with_teammate_context_sync(carried_context, || {
                    get_teammate_context().map(|context| context.agent_id)
                })
            })
            .join()
            .unwrap();

            (bare, carried)
        })
        .await;

        assert!(!seen.0, "a bare thread must not observe the scope");
        assert_eq!(seen.1.as_deref(), Some("worker@team"));

        // Outside any scope the carry is a no-op, not a panic.
        assert!(!with_teammate_context_sync(
            capture_teammate_context(),
            is_in_process_teammate
        ));
    }

    #[tokio::test]
    async fn teammate_scope_survives_awaits_on_the_same_future_chain() {
        let seen = run_with_teammate_context(sample_context("a@t"), async {
            tokio::task::yield_now().await;
            get_teammate_context().map(|context| context.agent_id)
        })
        .await;
        assert_eq!(seen.as_deref(), Some("a@t"));
    }
}
