//! Maps to: CC `cost-tracker.ts` getters used by StatusLine / session stats.
//!
//! Totals live in process state (CC `bootstrap/state.ts`). Accumulation,
//! project-config save, and session-matched resume restoration are live for the
//! counters represented here; live per-model accumulation and restored
//! context-window/max-output enrichment remain outside this Rust subset.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::Instant;

#[derive(Debug, Default)]
struct CostState {
    total_cost_usd: f64,
    total_api_duration_ms: u64,
    total_api_duration_without_retries_ms: u64,
    total_tool_duration_ms: u64,
    total_lines_added: u64,
    total_lines_removed: u64,
    total_input_tokens: u64,
    total_output_tokens: u64,
    model_usage: HashMap<String, crate::utils::config::ModelUsageStats>,
    restored_duration_ms: u64,
    session_started_at: Option<Instant>,
}

static COST_STATE: LazyLock<Mutex<CostState>> = LazyLock::new(|| Mutex::new(CostState::default()));

fn with_state<T>(f: impl FnOnce(&mut CostState) -> T) -> T {
    let mut guard = COST_STATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if guard.session_started_at.is_none() {
        guard.session_started_at = Some(Instant::now());
    }
    f(&mut guard)
}

/// Maps to: CC `getTotalCost` / `getTotalCostUSD`.
pub fn get_total_cost() -> f64 {
    with_state(|state| state.total_cost_usd)
}

/// Maps to: CC `getTotalDuration`.
pub fn get_total_duration() -> u64 {
    with_state(|state| {
        state.restored_duration_ms.saturating_add(
            state
                .session_started_at
                .map(|started| started.elapsed().as_millis() as u64)
                .unwrap_or(0),
        )
    })
}

/// Maps to: CC `getTotalAPIDuration`.
pub fn get_total_api_duration() -> u64 {
    with_state(|state| state.total_api_duration_ms)
}

/// Maps to: CC `getTotalAPIDurationWithoutRetries`.
pub fn get_total_api_duration_without_retries() -> u64 {
    with_state(|state| state.total_api_duration_without_retries_ms)
}

/// Maps to: CC `getTotalToolDuration`.
pub fn get_total_tool_duration() -> u64 {
    with_state(|state| state.total_tool_duration_ms)
}

/// Maps to: CC `getModelUsage`.
pub fn get_model_usage() -> HashMap<String, crate::utils::config::ModelUsageStats> {
    with_state(|state| state.model_usage.clone())
}

/// Maps to: CC `getTotalLinesAdded`.
pub fn get_total_lines_added() -> u64 {
    with_state(|state| state.total_lines_added)
}

/// Maps to: CC `getTotalLinesRemoved`.
pub fn get_total_lines_removed() -> u64 {
    with_state(|state| state.total_lines_removed)
}

/// Maps to: CC `getTotalInputTokens`.
pub fn get_total_input_tokens() -> u64 {
    with_state(|state| state.total_input_tokens)
}

/// Maps to: CC `getTotalOutputTokens`.
pub fn get_total_output_tokens() -> u64 {
    with_state(|state| state.total_output_tokens)
}

/// Maps to: CC `addToTotalCostState`.
pub fn add_to_total_cost(cost_usd: f64, api_duration_ms: u64) {
    with_state(|state| {
        state.total_cost_usd += cost_usd;
        state.total_api_duration_ms = state.total_api_duration_ms.saturating_add(api_duration_ms);
    });
}

/// Maps to: CC `addToTotalLinesChanged`.
pub fn add_to_total_lines_changed(added: u64, removed: u64) {
    with_state(|state| {
        state.total_lines_added = state.total_lines_added.saturating_add(added);
        state.total_lines_removed = state.total_lines_removed.saturating_add(removed);
    });
}

/// Accumulate token totals (StatusLine `context_window.total_*_tokens`).
pub fn add_to_total_tokens(input: u64, output: u64) {
    with_state(|state| {
        state.total_input_tokens = state.total_input_tokens.saturating_add(input);
        state.total_output_tokens = state.total_output_tokens.saturating_add(output);
    });
}

/// Maps to: CC `cost-tracker.ts` `StoredCostState`.
#[derive(Clone, Debug)]
pub struct StoredCostState {
    pub total_cost_usd: f64,
    pub total_api_duration: u64,
    pub total_api_duration_without_retries: u64,
    pub total_tool_duration: u64,
    pub total_lines_added: u64,
    pub total_lines_removed: u64,
    pub last_duration: Option<u64>,
    pub model_usage: Option<HashMap<String, crate::utils::config::ModelUsageStats>>,
}

/// Maps to: CC `getStoredSessionCosts(sessionId)`.
pub fn get_stored_session_costs(session_id: &str) -> Option<StoredCostState> {
    let project = crate::utils::config::get_current_project_config();
    if project.last_session_id.as_deref() != Some(session_id) {
        return None;
    }
    Some(StoredCostState {
        total_cost_usd: project.last_cost.unwrap_or(0.0),
        total_api_duration: project.last_api_duration.unwrap_or(0.0).max(0.0) as u64,
        total_api_duration_without_retries: project
            .last_api_duration_without_retries
            .unwrap_or(0.0)
            .max(0.0) as u64,
        total_tool_duration: project.last_tool_duration.unwrap_or(0.0).max(0.0) as u64,
        total_lines_added: project.last_lines_added.unwrap_or(0),
        total_lines_removed: project.last_lines_removed.unwrap_or(0),
        last_duration: project.last_duration.map(|value| value.max(0.0) as u64),
        model_usage: project.last_model_usage,
    })
}

/// Maps to: CC bootstrap `setCostStateForRestore(data)`.
pub fn set_cost_state_for_restore(data: &StoredCostState) {
    let mut guard = COST_STATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard.total_cost_usd = data.total_cost_usd;
    guard.total_api_duration_ms = data.total_api_duration;
    guard.total_api_duration_without_retries_ms = data.total_api_duration_without_retries;
    guard.total_tool_duration_ms = data.total_tool_duration;
    guard.total_lines_added = data.total_lines_added;
    guard.total_lines_removed = data.total_lines_removed;
    if let Some(model_usage) = data.model_usage.as_ref() {
        guard.model_usage = model_usage.clone();
    }
    guard.restored_duration_ms = data.last_duration.unwrap_or(0);
    guard.session_started_at = Some(Instant::now());
}

/// Maps to: CC `restoreCostStateForSession(sessionId)`.
pub fn restore_cost_state_for_session(session_id: &str) -> bool {
    let Some(data) = get_stored_session_costs(session_id) else {
        return false;
    };
    set_cost_state_for_restore(&data);
    true
}

/// Maps to: CC `saveCurrentSessionCosts()` for Rust-owned counters.
pub fn save_current_session_costs() -> anyhow::Result<()> {
    let session_id = crate::bootstrap::state::get_session_id();
    let (snapshot, total_input_tokens, total_output_tokens) = with_state(|state| {
        (
            StoredCostState {
                total_cost_usd: state.total_cost_usd,
                total_api_duration: state.total_api_duration_ms,
                total_api_duration_without_retries: state.total_api_duration_without_retries_ms,
                total_tool_duration: state.total_tool_duration_ms,
                total_lines_added: state.total_lines_added,
                total_lines_removed: state.total_lines_removed,
                last_duration: Some(
                    state.restored_duration_ms.saturating_add(
                        state
                            .session_started_at
                            .map(|started| started.elapsed().as_millis() as u64)
                            .unwrap_or(0),
                    ),
                ),
                model_usage: Some(state.model_usage.clone()),
            },
            state.total_input_tokens,
            state.total_output_tokens,
        )
    });
    crate::utils::config::save_current_project_config(|project| {
        project.last_session_id = Some(session_id);
        project.last_cost = Some(snapshot.total_cost_usd);
        project.last_api_duration = Some(snapshot.total_api_duration as f64);
        project.last_api_duration_without_retries =
            Some(snapshot.total_api_duration_without_retries as f64);
        project.last_tool_duration = Some(snapshot.total_tool_duration as f64);
        project.last_duration = snapshot.last_duration.map(|value| value as f64);
        project.last_lines_added = Some(snapshot.total_lines_added);
        project.last_lines_removed = Some(snapshot.total_lines_removed);
        project.last_total_input_tokens = Some(total_input_tokens);
        project.last_total_output_tokens = Some(total_output_tokens);
        project.last_model_usage = snapshot.model_usage;
    })
}

/// Maps to: CC `resetCostState`.
pub fn reset_cost_state() {
    let mut guard = COST_STATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *guard = CostState::default();
}

/// Test alias retained for existing callers.
pub fn reset_cost_state_for_tests() {
    reset_cost_state();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::TEST_ENV_LOCK;

    #[test]
    fn restore_cost_state_for_session_matches_saved_project_session_only() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let previous_cwd = crate::bootstrap::state::get_original_cwd();
        let root =
            std::env::temp_dir().join(format!("cometix-cost-restore-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let root = std::fs::canonicalize(root).unwrap();
        crate::bootstrap::state::set_original_cwd(&root);

        let mut project = crate::utils::config::ProjectConfig::default();
        project.last_session_id = Some("target-session".to_string());
        project.last_cost = Some(1.25);
        project.last_api_duration = Some(420.0);
        project.last_api_duration_without_retries = Some(300.0);
        project.last_tool_duration = Some(55.0);
        project.last_duration = Some(1_200.0);
        project.last_lines_added = Some(7);
        project.last_lines_removed = Some(3);
        project.last_total_input_tokens = Some(111);
        project.last_total_output_tokens = Some(22);
        project.last_model_usage = Some(HashMap::from([(
            "claude-test".to_string(),
            crate::utils::config::ModelUsageStats {
                input_tokens: 10,
                output_tokens: 4,
                cost_usd: 0.25,
                ..crate::utils::config::ModelUsageStats::default()
            },
        )]));
        let mut global = crate::utils::config::GlobalConfig::default();
        global.projects.insert(
            crate::utils::config::normalize_project_path(&root.to_string_lossy()),
            project,
        );
        crate::utils::config::set_test_global_config(Some(global));
        reset_cost_state();

        assert!(!restore_cost_state_for_session("other-session"));
        assert!(restore_cost_state_for_session("target-session"));
        assert_eq!(get_total_cost(), 1.25);
        assert_eq!(get_total_api_duration(), 420);
        assert_eq!(get_total_api_duration_without_retries(), 300);
        assert_eq!(get_total_tool_duration(), 55);
        assert_eq!(get_total_lines_added(), 7);
        assert_eq!(get_total_lines_removed(), 3);
        // Official StoredCostState restores modelUsage, not these aggregate
        // config fields directly; model-usage accumulation remains separate.
        assert_eq!(get_total_input_tokens(), 0);
        assert_eq!(get_total_output_tokens(), 0);
        let model_usage = get_model_usage();
        assert_eq!(model_usage["claude-test"].input_tokens, 10);
        assert_eq!(model_usage["claude-test"].output_tokens, 4);
        assert_eq!(model_usage["claude-test"].cost_usd, 0.25);
        assert!(get_total_duration() >= 1_200);

        reset_cost_state();
        crate::utils::config::set_test_global_config(None);
        crate::bootstrap::state::set_original_cwd(previous_cwd);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn set_cost_state_for_restore_replaces_instead_of_accumulating() {
        reset_cost_state();
        add_to_total_cost(9.0, 900);
        set_cost_state_for_restore(&StoredCostState {
            total_cost_usd: 2.0,
            total_api_duration: 20,
            total_api_duration_without_retries: 18,
            total_tool_duration: 7,
            total_lines_added: 4,
            total_lines_removed: 1,
            last_duration: Some(30),
            model_usage: None,
        });
        assert_eq!(get_total_cost(), 2.0);
        assert_eq!(get_total_api_duration(), 20);
        assert_eq!(get_total_api_duration_without_retries(), 18);
        assert_eq!(get_total_tool_duration(), 7);
        assert_eq!(get_total_lines_added(), 4);
        assert_eq!(get_total_lines_removed(), 1);
        assert_eq!(get_total_input_tokens(), 0);
        assert_eq!(get_total_output_tokens(), 0);
        reset_cost_state();
    }
}
