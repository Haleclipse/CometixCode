//! Query config snapshot.
//! Maps to official `query/config.ts`.

use crate::types::ids::SessionId;
use crate::utils::feature_flags::{FeatureFlag, feature_enabled};

/// Immutable values snapshotted once at query entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryConfig {
    pub session_id: SessionId,
    pub gates: QueryConfigGates,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryConfigGates {
    pub streaming_tool_execution: bool,
    pub emit_tool_use_summaries: bool,
    pub is_ant: bool,
    pub fast_mode_enabled: bool,
}

/// Maps to official `buildQueryConfig()`.
///
/// This is read-only: it snapshots environment gates without writing config,
/// starting analytics, or executing model/tool I/O.
pub fn build_query_config() -> QueryConfig {
    build_query_config_from_env_and_session(
        |key| crate::utils::process_env::var(key),
        || SessionId(crate::bootstrap::state::get_session_id()),
    )
}

#[cfg(test)]
thread_local! {
    static STREAMING_TOOL_EXECUTION_TEST_OVERRIDE: std::cell::Cell<Option<bool>> =
        std::cell::Cell::new(None);
}

#[cfg(test)]
pub struct QueryConfigTestOverride {
    previous: Option<bool>,
}

#[cfg(test)]
impl Drop for QueryConfigTestOverride {
    fn drop(&mut self) {
        STREAMING_TOOL_EXECUTION_TEST_OVERRIDE.with(|cell| cell.set(self.previous));
    }
}

#[cfg(test)]
pub fn set_streaming_tool_execution_for_test(enabled: bool) -> QueryConfigTestOverride {
    let previous = STREAMING_TOOL_EXECUTION_TEST_OVERRIDE.with(|cell| cell.replace(Some(enabled)));
    QueryConfigTestOverride { previous }
}

#[cfg(test)]
fn streaming_tool_execution_test_override() -> Option<bool> {
    STREAMING_TOOL_EXECUTION_TEST_OVERRIDE.with(|cell| cell.get())
}

#[cfg(not(test))]
fn streaming_tool_execution_test_override() -> Option<bool> {
    None
}

fn build_query_config_from_env_and_session(
    get_env: impl Fn(&str) -> Option<String>,
    get_session_id: impl Fn() -> SessionId,
) -> QueryConfig {
    QueryConfig {
        // Maps to CC `query/config.ts` `sessionId: getSessionId()`.
        session_id: get_session_id(),
        gates: QueryConfigGates {
            // Maps to CC Statsig/GrowthBook gate
            // `tengu_streaming_tool_execution2`. Cometix uses the
            // source-controlled switch table instead of GrowthBook network/cache
            // state.
            streaming_tool_execution: streaming_tool_execution_test_override()
                .unwrap_or_else(|| feature_enabled(FeatureFlag::StreamingToolExecution)),
            emit_tool_use_summaries: is_truthy(get_env("CLAUDE_CODE_EMIT_TOOL_USE_SUMMARIES")),
            is_ant: crate::utils::build_profile::has_internal_capability(
                crate::utils::build_profile::InternalCapability::Api,
            ),
            fast_mode_enabled: !is_truthy(get_env("CLAUDE_CODE_DISABLE_FAST_MODE")),
        },
    }
}

fn is_truthy(value: Option<String>) -> bool {
    value.is_some_and(|value| {
        matches!(
            value.as_str(),
            "1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_query_config_snapshots_official_env_gates_without_io() {
        let config = build_query_config_from_env_and_session(
            |key| match key {
                "CLAUDE_SESSION_ID" => Some("ignored-env-session".to_string()),
                "CLAUDE_CODE_EMIT_TOOL_USE_SUMMARIES" => Some("1".to_string()),
                _ => None,
            },
            || SessionId("session-query-config".to_string()),
        );

        assert_eq!(config.session_id.0, "session-query-config");
        assert!(!config.gates.streaming_tool_execution);
        assert!(config.gates.emit_tool_use_summaries);
        assert_eq!(
            config.gates.is_ant,
            crate::utils::build_profile::build_audience().is_internal()
        );
        assert!(config.gates.fast_mode_enabled);
    }
}
