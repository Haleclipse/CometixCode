//! Commit attribution state tracking.
//!
//! Maps to: CC `utils/commitAttribution.ts` (state shape + empty constructor).
//! Full file-diff / git-notes attribution remains a follow-up.

use std::collections::BTreeMap;

/// Maps to: CC `FileAttributionState` (subset kept for AppState parity).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileAttributionState {
    pub content_hash: String,
    pub mtime_ms: i64,
    pub claude_chars: u64,
    pub human_chars: u64,
}

/// Maps to: CC `AttributionState`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttributionState {
    /// File states keyed by relative path (from cwd).
    pub file_states: BTreeMap<String, FileAttributionState>,
    /// Session baseline states for net change calculation.
    pub session_baselines: BTreeMap<String, (String, i64)>,
    /// Surface from which edits were made.
    pub surface: String,
    /// HEAD SHA at session start (for detecting external commits).
    pub starting_head_sha: Option<String>,
    pub prompt_count: u64,
    pub prompt_count_at_last_commit: u64,
    pub permission_prompt_count: u64,
    pub permission_prompt_count_at_last_commit: u64,
    pub escape_count: u64,
    pub escape_count_at_last_commit: u64,
}

/// Maps to: CC `getClientSurface()`.
pub fn get_client_surface() -> String {
    crate::utils::process_env::var("CLAUDE_CODE_ENTRYPOINT").unwrap_or_else(|| "cli".into())
}

/// Maps to: CC `createEmptyAttributionState()`.
pub fn create_empty_attribution_state() -> AttributionState {
    AttributionState {
        file_states: BTreeMap::new(),
        session_baselines: BTreeMap::new(),
        surface: get_client_surface(),
        starting_head_sha: None,
        prompt_count: 0,
        prompt_count_at_last_commit: 0,
        permission_prompt_count: 0,
        permission_prompt_count_at_last_commit: 0,
        escape_count: 0,
        escape_count_at_last_commit: 0,
    }
}

impl Default for AttributionState {
    fn default() -> Self {
        create_empty_attribution_state()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_attribution_state_is_zeroed() {
        let state = create_empty_attribution_state();
        assert!(state.file_states.is_empty());
        assert!(state.session_baselines.is_empty());
        assert_eq!(state.prompt_count, 0);
        assert!(state.starting_head_sha.is_none());
        assert!(!state.surface.is_empty());
    }
}
