//! Maps to: CC `utils/statsCache.ts`.
//!
//! Historical aggregates use the official bounded cache shape and atomic
//! temp-file rename. Compatible newer cache versions are consumed read-only so
//! Cometix can coexist with another installed Claude Code without downgrading
//! its cache; only the source-controlled v3 shape is written.

use crate::utils::stats::{
    ClaudeCodeStats, DailyActivity, DailyModelTokens, LongestSessionStats, ModelUsageStats,
    ProcessedStats, StreakStats,
};
use chrono::{DateTime, Local, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::OnceLock;
use std::sync::{LazyLock, Mutex};

pub const STATS_CACHE_VERSION: u64 = 3;
const MIN_MIGRATABLE_VERSION: u64 = 1;
const STATS_CACHE_FILENAME: &str = "stats-cache.json";
static STATS_CACHE_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PersistedStatsCache {
    pub version: u64,
    pub last_computed_date: Option<String>,
    pub daily_activity: Vec<DailyActivity>,
    pub daily_model_tokens: Vec<DailyModelTokens>,
    pub model_usage: BTreeMap<String, ModelUsageStats>,
    pub total_sessions: u64,
    pub total_messages: u64,
    pub longest_session: Option<LongestSessionStats>,
    pub first_session_date: Option<String>,
    pub hour_counts: BTreeMap<u8, u64>,
    pub total_speculation_time_saved_ms: u64,
    pub shot_distribution: Option<BTreeMap<u64, u64>>,
}

impl PersistedStatsCache {
    pub fn empty() -> Self {
        Self {
            version: STATS_CACHE_VERSION,
            shot_distribution: Some(BTreeMap::new()),
            ..Self::default()
        }
    }

    fn is_compatible(&self) -> bool {
        self.version >= MIN_MIGRATABLE_VERSION
    }
}

#[cfg(test)]
static TEST_STATS_CACHE_PATH: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();

#[cfg(test)]
fn test_cache_path_slot() -> &'static Mutex<Option<PathBuf>> {
    TEST_STATS_CACHE_PATH.get_or_init(|| Mutex::new(None))
}

#[cfg(test)]
pub struct TestStatsCachePathGuard {
    previous: Option<PathBuf>,
}

#[cfg(test)]
impl Drop for TestStatsCachePathGuard {
    fn drop(&mut self) {
        *test_cache_path_slot()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = self.previous.take();
    }
}

#[cfg(test)]
pub fn set_test_stats_cache_path(path: impl Into<PathBuf>) -> TestStatsCachePathGuard {
    let mut slot = test_cache_path_slot()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let previous = slot.replace(path.into());
    TestStatsCachePathGuard { previous }
}

pub fn get_stats_cache_path() -> PathBuf {
    #[cfg(test)]
    if let Some(path) = test_cache_path_slot()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
    {
        return path;
    }
    crate::utils::env_utils::get_claude_config_home_dir().join(STATS_CACHE_FILENAME)
}

pub fn with_stats_cache_lock<T>(callback: impl FnOnce() -> T) -> T {
    let _guard = STATS_CACHE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    callback()
}

/// Maps to: CC `utils/statsCache.ts#loadStatsCache`.
pub fn load_stats_cache() -> PersistedStatsCache {
    let path = get_stats_cache_path();
    let fs = crate::utils::fs_operations::get_fs_implementation();
    let content = match futures::executor::block_on(
        fs.read_file(&path, crate::utils::fs_operations::BufferEncoding::Utf8),
    )
    .map(|text| text.to_string_lossy())
    {
        Ok(content) => content,
        Err(error) => {
            tracing::debug!(path = %path.display(), %error, "failed to load stats cache");
            return PersistedStatsCache::empty();
        }
    };
    let mut parsed = match serde_json::from_str::<PersistedStatsCache>(&content) {
        Ok(parsed) => parsed,
        Err(error) => {
            tracing::debug!(path = %path.display(), %error, "invalid stats cache");
            return PersistedStatsCache::empty();
        }
    };
    if !parsed.is_compatible() {
        tracing::debug!(
            version = parsed.version,
            "stats cache version is not migratable"
        );
        return PersistedStatsCache::empty();
    }
    // v1/v2 are source-supported migrations. A compatible newer cache remains
    // read-only and keeps its version so save_stats_cache will not downgrade it.
    if parsed.version <= STATS_CACHE_VERSION {
        parsed.version = STATS_CACHE_VERSION;
    }
    parsed
}

/// Maps to: CC `utils/statsCache.ts#saveStatsCache`.
pub fn save_stats_cache(cache: &PersistedStatsCache) {
    if cache.version > STATS_CACHE_VERSION
        || !crate::utils::session_storage::is_session_write_enabled()
    {
        return;
    }
    let path = get_stats_cache_path();
    let Some(parent) = path.parent() else {
        return;
    };
    let fs = crate::utils::fs_operations::get_fs_implementation();
    let _ = futures::executor::block_on(fs.mkdir(parent, None));
    let temp = path.with_extension(format!("json.{}.tmp", uuid::Uuid::new_v4()));
    let content = match serde_json::to_vec_pretty(cache) {
        Ok(content) => content,
        Err(error) => {
            tracing::debug!(%error, "failed to encode stats cache");
            return;
        }
    };
    let write_result = (|| -> std::io::Result<()> {
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp)?;
        file.write_all(&content)?;
        file.sync_all()?;
        // CC fs/promises FileHandle.close() completes before activeFs.rename.
        drop(file);
        futures::executor::block_on(fs.rename(&temp, &path))?;
        Ok(())
    })();
    if let Err(error) = write_result {
        tracing::debug!(path = %path.display(), %error, "failed to save stats cache");
        let _ = futures::executor::block_on(fs.unlink(&temp));
    }
}

fn merge_model_usage(
    target: &mut BTreeMap<String, ModelUsageStats>,
    incoming: BTreeMap<String, ModelUsageStats>,
) {
    for (model, usage) in incoming {
        let current = target.entry(model).or_default();
        current.input_tokens = current.input_tokens.saturating_add(usage.input_tokens);
        current.output_tokens = current.output_tokens.saturating_add(usage.output_tokens);
        current.cache_read_input_tokens = current
            .cache_read_input_tokens
            .saturating_add(usage.cache_read_input_tokens);
    }
}

/// Maps to: CC `utils/statsCache.ts#mergeCacheWithNewStats`.
pub(crate) fn merge_cache_with_new_stats(
    existing: PersistedStatsCache,
    incoming: ProcessedStats,
    last_computed_date: String,
) -> PersistedStatsCache {
    let mut activity = existing
        .daily_activity
        .into_iter()
        .map(|day| (day.date.clone(), day))
        .collect::<BTreeMap<_, _>>();
    for (_, day) in incoming.daily_activity {
        let current = activity
            .entry(day.date.clone())
            .or_insert_with(|| DailyActivity {
                date: day.date.clone(),
                ..DailyActivity::default()
            });
        current.message_count = current.message_count.saturating_add(day.message_count);
        current.session_count = current.session_count.saturating_add(day.session_count);
        current.tool_call_count = current.tool_call_count.saturating_add(day.tool_call_count);
    }

    let mut daily_tokens = existing
        .daily_model_tokens
        .into_iter()
        .map(|day| (day.date, day.tokens_by_model))
        .collect::<BTreeMap<_, _>>();
    for (date, models) in incoming.daily_model_tokens {
        let current = daily_tokens.entry(date).or_default();
        for (model, tokens) in models {
            *current.entry(model).or_default() += tokens;
        }
    }
    let mut model_usage = existing.model_usage;
    merge_model_usage(&mut model_usage, incoming.model_usage);
    let mut hour_counts = existing.hour_counts;
    for (hour, count) in incoming.hour_counts {
        *hour_counts.entry(hour).or_default() += count;
    }
    let mut longest_session = existing.longest_session;
    for session in &incoming.session_stats {
        if longest_session
            .as_ref()
            .is_none_or(|current| session.duration > current.duration)
        {
            longest_session = Some(session.clone());
        }
    }
    let mut first_session_date = existing.first_session_date;
    for session in &incoming.session_stats {
        if first_session_date
            .as_ref()
            .is_none_or(|current| session.timestamp < *current)
        {
            first_session_date = Some(session.timestamp.clone());
        }
    }

    PersistedStatsCache {
        // Preserve compatible newer caches in memory; they are never written.
        version: existing.version.max(STATS_CACHE_VERSION),
        last_computed_date: Some(last_computed_date),
        daily_activity: activity.into_values().collect(),
        daily_model_tokens: daily_tokens
            .into_iter()
            .map(|(date, tokens_by_model)| DailyModelTokens {
                date,
                tokens_by_model,
            })
            .collect(),
        model_usage,
        total_sessions: existing
            .total_sessions
            .saturating_add(incoming.session_stats.len() as u64),
        total_messages: existing
            .total_messages
            .saturating_add(incoming.total_messages),
        longest_session,
        first_session_date,
        hour_counts,
        total_speculation_time_saved_ms: existing
            .total_speculation_time_saved_ms
            .saturating_add(incoming.total_speculation_time_saved_ms),
        shot_distribution: existing.shot_distribution,
    }
}

fn parse_date_or_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|value| value.with_timezone(&Utc))
        .or_else(|| {
            NaiveDate::parse_from_str(value, "%Y-%m-%d")
                .ok()?
                .and_hms_opt(0, 0, 0)
                .map(|value| value.and_utc())
        })
}

/// Maps to: CC `utils/stats.ts#cacheToStats`.
pub(crate) fn cache_to_stats(
    mut cache: PersistedStatsCache,
    today: Option<ProcessedStats>,
) -> ClaudeCodeStats {
    let today = today.unwrap_or_default();
    let merged = merge_cache_with_new_stats(
        cache.clone(),
        today.clone(),
        cache
            .last_computed_date
            .clone()
            .unwrap_or_else(|| Utc::now().format("%Y-%m-%d").to_string()),
    );
    // `merge_cache_with_new_stats` increments the bounded aggregates exactly
    // once; preserve the cache's lastComputedDate semantics for this live view.
    cache = merged;
    let daily_activity = cache.daily_activity.clone();
    let daily_model_tokens = cache.daily_model_tokens.clone();
    let last_session_date = today
        .session_stats
        .iter()
        .map(|session| session.timestamp.as_str())
        .max()
        .map(str::to_string)
        .or_else(|| daily_activity.last().map(|day| day.date.clone()));
    let peak_activity_day = daily_activity
        .iter()
        .reduce(|max, day| {
            if day.message_count > max.message_count {
                day
            } else {
                max
            }
        })
        .map(|day| day.date.clone());
    let peak_activity_hour = cache
        .hour_counts
        .iter()
        .reduce(|max, item| if item.1 > max.1 { item } else { max })
        .map(|(hour, _)| *hour);
    let total_days = match (
        cache
            .first_session_date
            .as_deref()
            .and_then(parse_date_or_timestamp),
        last_session_date
            .as_deref()
            .and_then(parse_date_or_timestamp),
    ) {
        (Some(first), Some(last)) => {
            let millis = last.signed_duration_since(first).num_milliseconds().max(0);
            ((millis + 86_400_000 - 1) / 86_400_000 + 1) as u64
        }
        _ => 0,
    };
    let streaks: StreakStats =
        crate::utils::stats::calculate_streaks(&daily_activity, Local::now().date_naive());

    ClaudeCodeStats {
        total_sessions: cache.total_sessions,
        total_messages: cache.total_messages,
        total_days,
        active_days: daily_activity.len() as u64,
        longest_session: cache.longest_session,
        streaks,
        peak_activity_day,
        peak_activity_hour,
        daily_activity,
        daily_model_tokens,
        model_usage: cache.model_usage,
        first_session_date: cache.first_session_date,
        last_session_date,
        total_speculation_time_saved_ms: cache.total_speculation_time_saved_ms,
        shot_distribution: cache.shot_distribution,
        one_shot_rate: None,
    }
}

pub fn is_date_before(left: &str, right: &str) -> bool {
    left < right
}

pub fn next_day(date: NaiveDate) -> Option<NaiveDate> {
    date.succ_opt()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::TEST_ENV_LOCK;

    #[test]
    fn compatible_newer_stats_cache_is_read_without_being_downgraded() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let root =
            std::env::temp_dir().join(format!("cometix-stats-cache-{}", uuid::Uuid::new_v4()));
        let path = root.join("stats-cache.json");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            &path,
            serde_json::json!({
                "version": 4,
                "lastComputedDate": "2026-07-16",
                "dailyActivity": [],
                "dailyModelTokens": [],
                "modelUsage": {},
                "totalSessions": 12,
                "totalMessages": 34
            })
            .to_string(),
        )
        .unwrap();
        let _path = set_test_stats_cache_path(&path);
        let cache = load_stats_cache();
        assert_eq!(cache.version, 4);
        assert_eq!(cache.total_sessions, 12);
        save_stats_cache(&cache);
        let persisted: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(persisted["version"], 4);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn stats_cache_merge_keeps_bounded_daily_model_and_session_aggregates() {
        let mut cache = PersistedStatsCache::empty();
        cache.total_sessions = 2;
        cache.total_messages = 5;
        let mut incoming = ProcessedStats::default();
        incoming.daily_activity.insert(
            "2026-07-18".to_string(),
            DailyActivity {
                date: "2026-07-18".to_string(),
                message_count: 3,
                session_count: 1,
                tool_call_count: 2,
            },
        );
        incoming.session_stats.push(LongestSessionStats {
            session_id: "session".to_string(),
            duration: 1000,
            message_count: 3,
            timestamp: "2026-07-18T00:00:00Z".to_string(),
        });
        incoming.total_messages = 3;
        let merged = merge_cache_with_new_stats(cache, incoming, "2026-07-18".to_string());
        assert_eq!(merged.total_sessions, 3);
        assert_eq!(merged.total_messages, 8);
        assert_eq!(merged.daily_activity[0].tool_call_count, 2);
        assert_eq!(merged.last_computed_date.as_deref(), Some("2026-07-18"));
    }
}
