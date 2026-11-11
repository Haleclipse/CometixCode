//! Maps to: CC `cost-tracker.ts` getters used by StatusLine / session stats.
//!
//! @nyan: every completed model request is recorded once, at the request layer
//! (`services/api/request.rs` and `utils/side_query.rs`), through
//! [`record_request`]. Each record is priced from the model catalog / profile
//! (`utils/model/pricing.rs`), added to the process totals, and appended to the
//! main session transcript as a `usage` entry. Resume replays those entries
//! instead of reading CC's per-project `lastCost` config fields; lines changed
//! and wall clock are kept in a last-wins `session-stats` entry.

use std::collections::BTreeMap;
use std::sync::{LazyLock, Mutex};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::services::provider::ir::Usage;

/// Transcript entry type of one request's usage.
pub const USAGE_ENTRY_TYPE: &str = "usage";
/// Transcript entry type of the session's lines changed and durations.
pub const SESSION_STATS_ENTRY_TYPE: &str = "session-stats";

/// Who a request was made for, grouped for `/cost`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CostSource {
    Main,
    Subagent,
    Compaction,
    KeepAlive,
    SideQuery,
    Utility(String),
}

impl CostSource {
    /// Groups CC's open query-source vocabulary.
    pub fn from_query_source(query_source: &str) -> Self {
        match query_source {
            source if source.starts_with("repl_main_thread") || source == "sdk" => Self::Main,
            source if source == "agent" || source.starts_with("agent:") => Self::Subagent,
            "compact" => Self::Compaction,
            "cache_warm" => Self::KeepAlive,
            "side_query" => Self::SideQuery,
            other => Self::Utility(other.to_string()),
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::Main => "main".to_string(),
            Self::Subagent => "subagent".to_string(),
            Self::Compaction => "compaction".to_string(),
            Self::KeepAlive => "keep-alive".to_string(),
            Self::SideQuery => "side-query".to_string(),
            Self::Utility(role) => format!("utility:{role}"),
        }
    }

    fn from_label(label: &str) -> Self {
        match label {
            "main" => Self::Main,
            "subagent" => Self::Subagent,
            "compaction" => Self::Compaction,
            "keep-alive" => Self::KeepAlive,
            "side-query" => Self::SideQuery,
            other => Self::Utility(other.strip_prefix("utility:").unwrap_or(other).to_string()),
        }
    }
}

impl Serialize for CostSource {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.label())
    }
}

impl<'de> Deserialize<'de> for CostSource {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_label(&String::deserialize(deserializer)?))
    }
}

/// One completed request, as appended to the transcript.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRecord {
    pub profile: String,
    pub provider: String,
    pub model: String,
    pub source: CostSource,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_5m_tokens: u64,
    pub cache_write_1h_tokens: u64,
    /// Display only; already inside `output_tokens`.
    pub reasoning_tokens: u64,
    /// `None` when no price is known for this profile/model.
    pub cost_usd: Option<f64>,
    pub duration_ms: u64,
}

/// Token and cost totals for one `/cost` row.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UsageTotals {
    pub requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub reasoning_tokens: u64,
    /// Sum of the requests whose price is known.
    pub cost_usd: f64,
    /// At least one request had no price, so `cost_usd` is a lower bound.
    pub cost_unknown: bool,
}

impl UsageTotals {
    fn add(&mut self, record: &UsageRecord) {
        self.requests += 1;
        self.input_tokens = self.input_tokens.saturating_add(record.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(record.output_tokens);
        self.cache_read_tokens = self
            .cache_read_tokens
            .saturating_add(record.cache_read_tokens);
        self.cache_write_tokens = self
            .cache_write_tokens
            .saturating_add(record.cache_write_5m_tokens)
            .saturating_add(record.cache_write_1h_tokens);
        self.reasoning_tokens = self
            .reasoning_tokens
            .saturating_add(record.reasoning_tokens);
        match record.cost_usd {
            Some(cost) => self.cost_usd += cost,
            None => self.cost_unknown = true,
        }
    }
}

/// Lines changed and wall clock, persisted per session.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SessionStats {
    pub lines_added: u64,
    pub lines_removed: u64,
    pub wall_duration_ms: u64,
}

#[derive(Debug, Default)]
struct CostState {
    total: UsageTotals,
    by_model: BTreeMap<String, UsageTotals>,
    by_source: BTreeMap<CostSource, UsageTotals>,
    total_api_duration_ms: u64,
    total_lines_added: u64,
    total_lines_removed: u64,
    restored_duration_ms: u64,
    session_started_at: Option<Instant>,
}

impl CostState {
    fn add(&mut self, record: &UsageRecord) {
        self.total.add(record);
        self.by_model
            .entry(record.model.clone())
            .or_default()
            .add(record);
        self.by_source
            .entry(record.source.clone())
            .or_default()
            .add(record);
        self.total_api_duration_ms = self
            .total_api_duration_ms
            .saturating_add(record.duration_ms);
    }

    fn wall_duration_ms(&self) -> u64 {
        self.restored_duration_ms.saturating_add(
            self.session_started_at
                .map(|started| started.elapsed().as_millis() as u64)
                .unwrap_or(0),
        )
    }
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

/// The one accounting point: price a completed request, add it to the
/// session totals and append it to the main session transcript.
///
/// `profile` is the request's profile (`None` = main). A profile that does not
/// resolve leaves the provider unknown, so the cost is unknown.
pub fn record_request(
    profile: Option<&str>,
    model: &str,
    source: CostSource,
    usage: &Usage,
    duration_ms: u64,
) {
    let settings = crate::utils::settings::get_initial_settings();
    let resolved = crate::utils::model::profile::resolve_request_profile(&settings, profile).ok();
    let profile_cost = resolved.as_ref().and_then(|resolved| {
        crate::utils::model::profile::model_profile(settings.nyan.as_deref()?, &resolved.profile)?
            .cost
            .as_ref()
    });
    let price = crate::utils::model::pricing::resolve_price(
        resolved.as_ref().map(|resolved| resolved.provider_kind),
        model,
        profile_cost,
    );
    let cache_write_1h = usage.cache_write_1h_tokens.min(usage.cache_write_tokens);
    let record = UsageRecord {
        profile: resolved
            .as_ref()
            .map(|resolved| resolved.profile.clone())
            .unwrap_or_default(),
        provider: resolved
            .as_ref()
            .map(|resolved| resolved.provider.clone())
            .unwrap_or_default(),
        model: model.to_string(),
        source,
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_read_tokens: usage.cache_read_tokens,
        cache_write_5m_tokens: usage.cache_write_tokens - cache_write_1h,
        cache_write_1h_tokens: cache_write_1h,
        reasoning_tokens: usage.reasoning_tokens,
        cost_usd: price.map(|price| price.cost_usd(usage)),
        duration_ms,
    };
    with_state(|state| state.add(&record));
    if let Err(error) = crate::utils::session_storage::append_usage_entry(&record) {
        crate::utils::debug::log_for_debugging(&format!("Failed to record usage: {error}"));
    }
}

/// Maps to: CC `getTotalCost` / `getTotalCostUSD`. Requests without a price
/// contribute nothing, so this is a lower bound when [`has_unknown_cost`].
pub fn get_total_cost() -> f64 {
    with_state(|state| state.total.cost_usd)
}

/// Maps to: CC `hasUnknownModelCost`.
pub fn has_unknown_cost() -> bool {
    with_state(|state| state.total.cost_unknown)
}

pub fn get_total_usage() -> UsageTotals {
    with_state(|state| state.total.clone())
}

/// Maps to: CC `getModelUsage`.
pub fn get_model_usage() -> BTreeMap<String, UsageTotals> {
    with_state(|state| state.by_model.clone())
}

pub fn get_source_usage() -> BTreeMap<CostSource, UsageTotals> {
    with_state(|state| state.by_source.clone())
}

/// Maps to: CC `getTotalDuration`.
pub fn get_total_duration() -> u64 {
    with_state(|state| state.wall_duration_ms())
}

/// Maps to: CC `getTotalAPIDuration`.
pub fn get_total_api_duration() -> u64 {
    with_state(|state| state.total_api_duration_ms)
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
    with_state(|state| state.total.input_tokens)
}

/// Maps to: CC `getTotalOutputTokens`.
pub fn get_total_output_tokens() -> u64 {
    with_state(|state| state.total.output_tokens)
}

/// Maps to: CC `addToTotalLinesChanged`.
pub fn add_to_total_lines_changed(added: u64, removed: u64) {
    with_state(|state| {
        state.total_lines_added = state.total_lines_added.saturating_add(added);
        state.total_lines_removed = state.total_lines_removed.saturating_add(removed);
    });
}

/// Current lines changed and wall clock, for the `session-stats` entry.
pub fn current_session_stats() -> SessionStats {
    with_state(|state| SessionStats {
        lines_added: state.total_lines_added,
        lines_removed: state.total_lines_removed,
        wall_duration_ms: state.wall_duration_ms(),
    })
}

/// Maps to: CC `saveCurrentSessionCosts()`. Usage is already in the
/// transcript; only the session stats are written here.
pub fn save_current_session_stats() -> anyhow::Result<()> {
    crate::utils::session_storage::append_session_stats_entry(&current_session_stats())
}

/// Rebuild the totals of a resumed session from its transcript entries:
/// every `usage` entry is replayed, the last `session-stats` entry wins.
/// Replaces the current state (CC `setCostStateForRestore`).
pub fn restore_from_transcript_entries<'a>(
    entries: impl IntoIterator<Item = &'a serde_json::Value>,
) {
    let mut state = CostState::default();
    for entry in entries {
        match entry.get("type").and_then(serde_json::Value::as_str) {
            Some(USAGE_ENTRY_TYPE) => {
                if let Some(record) = entry
                    .get("usage")
                    .and_then(|usage| UsageRecord::deserialize(usage).ok())
                {
                    state.add(&record);
                }
            }
            Some(SESSION_STATS_ENTRY_TYPE) => {
                if let Some(stats) = entry
                    .get("stats")
                    .and_then(|stats| SessionStats::deserialize(stats).ok())
                {
                    state.total_lines_added = stats.lines_added;
                    state.total_lines_removed = stats.lines_removed;
                    state.restored_duration_ms = stats.wall_duration_ms;
                }
            }
            _ => {}
        }
    }
    state.session_started_at = Some(Instant::now());
    *COST_STATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = state;
}

/// Maps to: CC `restoreCostStateForSession`: rebuild the totals from the
/// resumed session's transcript file.
pub fn restore_cost_state_from_transcript(path: &std::path::Path) {
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let entries = content
        .lines()
        .filter(|line| {
            line.contains("\"type\":\"usage\"") || line.contains("\"type\":\"session-stats\"")
        })
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .collect::<Vec<_>>();
    restore_from_transcript_entries(&entries);
}

/// Maps to: CC `resetCostState`.
pub fn reset_cost_state() {
    let mut guard = COST_STATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *guard = CostState::default();
}

/// Start the per-session stats (lines changed, wall clock) over for a new
/// session, keeping the process-wide cost and usage totals (`/clear`).
pub fn reset_session_stats() {
    with_state(|state| {
        state.total_lines_added = 0;
        state.total_lines_removed = 0;
        state.restored_duration_ms = 0;
        state.session_started_at = Some(Instant::now());
    });
}

#[cfg(test)]
pub(crate) fn add_record_for_tests(record: &UsageRecord) {
    with_state(|state| state.add(record));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::TEST_ENV_LOCK;

    fn record(model: &str, source: CostSource, cost_usd: Option<f64>) -> UsageRecord {
        UsageRecord {
            profile: "main".to_string(),
            provider: "anthropic".to_string(),
            model: model.to_string(),
            source,
            input_tokens: 10,
            output_tokens: 5,
            cache_read_tokens: 100,
            cache_write_5m_tokens: 20,
            cache_write_1h_tokens: 30,
            reasoning_tokens: 2,
            cost_usd,
            duration_ms: 40,
        }
    }

    #[test]
    fn query_sources_group_into_cost_sources() {
        let cases = [
            ("repl_main_thread", CostSource::Main),
            ("repl_main_thread:outputStyle:x", CostSource::Main),
            ("sdk", CostSource::Main),
            ("agent:builtin:general-purpose", CostSource::Subagent),
            ("agent:custom", CostSource::Subagent),
            ("compact", CostSource::Compaction),
            ("cache_warm", CostSource::KeepAlive),
            ("side_query", CostSource::SideQuery),
            (
                "extract_memories",
                CostSource::Utility("extract_memories".to_string()),
            ),
        ];
        for (query_source, expected) in cases {
            let source = CostSource::from_query_source(query_source);
            assert_eq!(source, expected, "{query_source}");
            assert_eq!(CostSource::from_label(&source.label()), source);
        }
        assert_eq!(
            CostSource::Utility("agent_summary".to_string()).label(),
            "utility:agent_summary"
        );
    }

    #[test]
    fn record_request_prices_and_accumulates_once() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        crate::services::provider::anthropic::target::install_test_main_profile(
            "http://127.0.0.1:1",
            "NYAN_COST_TEST_KEY",
        );
        reset_cost_state();
        let usage = Usage {
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
            cache_write_tokens: 1_000_000,
            cache_write_1h_tokens: 1_000_000,
            reasoning_tokens: 500_000,
            ..Usage::default()
        };
        record_request(None, "claude-opus-4-5", CostSource::Main, &usage, 12);
        // 5 input + 25 output + 10 (1h write = 2 × input); reasoning not re-priced.
        assert!((get_total_cost() - 40.0).abs() < 1e-9);
        assert!(!has_unknown_cost());
        assert_eq!(get_total_api_duration(), 12);
        let by_model = get_model_usage();
        assert_eq!(by_model["claude-opus-4-5"].requests, 1);
        assert_eq!(by_model["claude-opus-4-5"].cache_write_tokens, 1_000_000);

        record_request(None, "no-such-model", CostSource::Subagent, &usage, 0);
        assert!((get_total_cost() - 40.0).abs() < 1e-9);
        assert!(has_unknown_cost());
        assert!(get_source_usage()[&CostSource::Subagent].cost_unknown);
        assert_eq!(get_total_usage().requests, 2);

        reset_cost_state();
        crate::utils::settings::settings_cache::reset_settings_cache();
    }

    #[test]
    fn replay_rebuilds_totals_and_breakdowns() {
        reset_cost_state();
        add_record_for_tests(&record("stale", CostSource::Main, Some(9.0)));
        let entries = [
            serde_json::json!({
                "type": USAGE_ENTRY_TYPE,
                "usage": record("a", CostSource::Main, Some(1.5)),
            }),
            serde_json::json!({
                "type": USAGE_ENTRY_TYPE,
                "usage": record("b", CostSource::Utility("agent_summary".into()), None),
            }),
            serde_json::json!({
                "type": SESSION_STATS_ENTRY_TYPE,
                "stats": SessionStats { lines_added: 1, ..Default::default() },
            }),
            serde_json::json!({
                "type": SESSION_STATS_ENTRY_TYPE,
                "stats": SessionStats {
                    lines_added: 7,
                    lines_removed: 3,
                    wall_duration_ms: 1_200,
                },
            }),
            serde_json::json!({"type": "user"}),
        ];
        restore_from_transcript_entries(&entries);
        assert_eq!(get_total_cost(), 1.5);
        assert!(has_unknown_cost());
        assert_eq!(get_total_input_tokens(), 20);
        assert_eq!(get_total_api_duration(), 80);
        let by_model = get_model_usage();
        assert_eq!(by_model.keys().collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(by_model["a"].cache_write_tokens, 50);
        let by_source = get_source_usage();
        assert_eq!(
            by_source[&CostSource::Utility("agent_summary".into())].requests,
            1
        );
        assert_eq!(get_total_lines_added(), 7);
        assert_eq!(get_total_lines_removed(), 3);
        assert!(get_total_duration() >= 1_200);
        reset_cost_state();
    }
}
