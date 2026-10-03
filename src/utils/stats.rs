//! Maps to: CC `utils/stats.ts`.
//!
//! Session discovery and aggregation run on a caller-owned worker thread. The
//! command component never performs transcript I/O from a retained render
//! frame. Historical all-time aggregation is delegated to the bounded,
//! atomic `stats_cache` owner; filtered ranges are computed directly from the
//! available main/subagent JSONL transcripts with the official stat/head-scan
//! skip path.

use chrono::{DateTime, Local, NaiveDate, TimeZone, Timelike, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum StatsDateRange {
    SevenDays,
    ThirtyDays,
    #[default]
    All,
}

impl StatsDateRange {
    pub fn official_value(self) -> &'static str {
        match self {
            Self::SevenDays => "7d",
            Self::ThirtyDays => "30d",
            Self::All => "all",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DailyActivity {
    pub date: String,
    pub message_count: u64,
    pub session_count: u64,
    pub tool_call_count: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DailyModelTokens {
    pub date: String,
    pub tokens_by_model: BTreeMap<String, u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ModelUsageStats {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct LongestSessionStats {
    pub session_id: String,
    pub duration: u64,
    pub message_count: u64,
    pub timestamp: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StreakStats {
    pub current_streak: u64,
    pub longest_streak: u64,
    pub current_streak_start: Option<String>,
    pub longest_streak_start: Option<String>,
    pub longest_streak_end: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClaudeCodeStats {
    pub total_sessions: u64,
    pub total_messages: u64,
    pub total_days: u64,
    pub active_days: u64,
    pub longest_session: Option<LongestSessionStats>,
    pub streaks: StreakStats,
    pub peak_activity_day: Option<String>,
    pub peak_activity_hour: Option<u8>,
    pub daily_activity: Vec<DailyActivity>,
    pub daily_model_tokens: Vec<DailyModelTokens>,
    pub model_usage: BTreeMap<String, ModelUsageStats>,
    pub first_session_date: Option<String>,
    pub last_session_date: Option<String>,
    pub total_speculation_time_saved_ms: u64,
    pub shot_distribution: Option<BTreeMap<u64, u64>>,
    pub one_shot_rate: Option<u64>,
}
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

const SYNTHETIC_MODEL: &str = "<synthetic>";
const TRANSCRIPT_MESSAGE_TYPES: [&str; 5] =
    ["user", "assistant", "attachment", "system", "progress"];

#[derive(Clone, Debug, Default)]
pub(crate) struct ProcessedStats {
    pub(crate) daily_activity: BTreeMap<String, DailyActivity>,
    pub(crate) daily_model_tokens: BTreeMap<String, BTreeMap<String, u64>>,
    pub(crate) model_usage: BTreeMap<String, ModelUsageStats>,
    pub(crate) session_stats: Vec<LongestSessionStats>,
    pub(crate) hour_counts: BTreeMap<u8, u64>,
    pub(crate) total_messages: u64,
    pub(crate) total_speculation_time_saved_ms: u64,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ProcessOptions {
    pub(crate) from_date: Option<NaiveDate>,
    pub(crate) to_date: Option<NaiveDate>,
}

fn is_transcript_message(value: &Value) -> bool {
    value
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|kind| TRANSCRIPT_MESSAGE_TYPES.contains(&kind))
}

fn is_sidechain(value: &Value) -> bool {
    value
        .get("isSidechain")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn parse_timestamp(value: &Value) -> Option<DateTime<chrono::FixedOffset>> {
    DateTime::parse_from_rfc3339(value.get("timestamp")?.as_str()?).ok()
}

fn date_key(timestamp: &DateTime<chrono::FixedOffset>) -> String {
    timestamp.with_timezone(&Utc).format("%Y-%m-%d").to_string()
}

fn date_in_range(date: NaiveDate, options: ProcessOptions) -> bool {
    options.from_date.is_none_or(|from| date >= from) && options.to_date.is_none_or(|to| date <= to)
}

/// Maps to: CC `utils/stats.ts#readSessionStartDate`.
pub fn read_session_start_date(path: &Path) -> Option<NaiveDate> {
    let mut file = File::open(path).ok()?;
    let mut buffer = vec![0u8; 4096];
    let bytes_read = file.read(&mut buffer).ok()?;
    if bytes_read == 0 {
        return None;
    }
    let head = String::from_utf8_lossy(&buffer[..bytes_read]);
    let last_newline = head.rfind('\n')?;
    for line in head[..last_newline].lines() {
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if !is_transcript_message(&entry) || is_sidechain(&entry) {
            continue;
        }
        let timestamp = parse_timestamp(&entry)?;
        return Some(timestamp.with_timezone(&Utc).date_naive());
    }
    None
}

fn add_usage(target: &mut ModelUsageStats, usage: &Value) {
    target.input_tokens = target.input_tokens.saturating_add(
        usage
            .get("input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    );
    target.output_tokens = target.output_tokens.saturating_add(
        usage
            .get("output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    );
    target.cache_read_input_tokens = target.cache_read_input_tokens.saturating_add(
        usage
            .get("cache_read_input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    );
}

pub(crate) fn process_session_files(paths: &[PathBuf], options: ProcessOptions) -> ProcessedStats {
    let mut stats = ProcessedStats::default();

    for path in paths {
        if let Some(from_date) = options.from_date {
            if let Ok(metadata) = std::fs::metadata(path) {
                if let Ok(modified) = metadata.modified() {
                    let modified_date = DateTime::<Utc>::from(modified).date_naive();
                    if modified_date < from_date {
                        continue;
                    }
                }
                if metadata.len() > 65_536
                    && read_session_start_date(path).is_some_and(|date| date < from_date)
                {
                    continue;
                }
            }
        }
        let entries = match crate::utils::json::read_jsonl_file(path) {
            Ok(entries) => entries,
            Err(error) => {
                // Maps to: CC utils/stats.ts:188–194, including the debug sink.
                crate::utils::debug::log_for_debugging(&format!(
                    "Failed to read session file {}: {error}",
                    path.display()
                ));
                continue;
            }
        };

        for entry in &entries {
            if entry.get("type").and_then(Value::as_str) == Some("speculation-accept") {
                stats.total_speculation_time_saved_ms =
                    stats.total_speculation_time_saved_ms.saturating_add(
                        entry
                            .get("timeSavedMs")
                            .and_then(Value::as_u64)
                            .unwrap_or(0),
                    );
            }
        }

        let messages = entries
            .iter()
            .filter(|entry| is_transcript_message(entry))
            .collect::<Vec<_>>();
        if messages.is_empty() {
            continue;
        }

        let is_subagent_file = path
            .components()
            .any(|component| component.as_os_str() == "subagents");
        let main_messages = if is_subagent_file {
            messages
        } else {
            messages
                .into_iter()
                .filter(|entry| !is_sidechain(entry))
                .collect()
        };
        if main_messages.is_empty() {
            continue;
        }

        let Some(first_timestamp) = parse_timestamp(main_messages[0]) else {
            tracing::debug!(path = %path.display(), "skipping stats session with invalid first timestamp");
            continue;
        };
        let Some(last_timestamp) = parse_timestamp(main_messages[main_messages.len() - 1]) else {
            tracing::debug!(path = %path.display(), "skipping stats session with invalid last timestamp");
            continue;
        };
        let key = date_key(&first_timestamp);
        let Some(date) = NaiveDate::parse_from_str(&key, "%Y-%m-%d").ok() else {
            continue;
        };
        if !date_in_range(date, options) {
            continue;
        }

        if !is_subagent_file {
            let duration = last_timestamp
                .signed_duration_since(first_timestamp)
                .num_milliseconds()
                .max(0) as u64;
            let session_id = path
                .file_stem()
                .and_then(|value| value.to_str())
                .unwrap_or_default()
                .to_string();
            stats.session_stats.push(LongestSessionStats {
                session_id,
                duration,
                message_count: main_messages.len() as u64,
                timestamp: main_messages[0]
                    .get("timestamp")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            });
            stats.total_messages = stats
                .total_messages
                .saturating_add(main_messages.len() as u64);
            let activity =
                stats
                    .daily_activity
                    .entry(key.clone())
                    .or_insert_with(|| DailyActivity {
                        date: key.clone(),
                        ..DailyActivity::default()
                    });
            activity.session_count = activity.session_count.saturating_add(1);
            activity.message_count = activity
                .message_count
                .saturating_add(main_messages.len() as u64);
            let hour = first_timestamp.with_timezone(&Local).hour() as u8;
            *stats.hour_counts.entry(hour).or_default() += 1;
        }

        for message in main_messages {
            if message.get("type").and_then(Value::as_str) != Some("assistant") {
                continue;
            }
            let Some(api_message) = message.get("message") else {
                continue;
            };
            if let Some(content) = api_message.get("content").and_then(Value::as_array) {
                let tool_calls = content
                    .iter()
                    .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_use"))
                    .count() as u64;
                if tool_calls > 0 {
                    if let Some(activity) = stats.daily_activity.get_mut(&key) {
                        activity.tool_call_count =
                            activity.tool_call_count.saturating_add(tool_calls);
                    }
                }
            }

            let Some(usage) = api_message.get("usage") else {
                continue;
            };
            let model = api_message
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            if model == SYNTHETIC_MODEL {
                continue;
            }
            let input = usage
                .get("input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let output = usage
                .get("output_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            add_usage(
                stats.model_usage.entry(model.to_string()).or_default(),
                usage,
            );
            let total = input.saturating_add(output);
            if total > 0 {
                *stats
                    .daily_model_tokens
                    .entry(key.clone())
                    .or_default()
                    .entry(model.to_string())
                    .or_default() += total;
            }
        }
    }

    stats
}

/// Maps to: CC `utils/stats.ts#getAllSessionFiles`.
pub fn get_all_session_files() -> std::io::Result<Vec<PathBuf>> {
    let projects_dir = crate::utils::session_storage::get_projects_dir();
    let project_entries = match std::fs::read_dir(&projects_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut main_files = Vec::new();
    let mut subagent_files = Vec::new();

    for project_entry in project_entries.flatten() {
        let project_path = project_entry.path();
        if !project_path.is_dir() {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&project_path) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && path.extension().and_then(|value| value.to_str()) == Some("jsonl")
            {
                main_files.push(path);
                continue;
            }
            if !path.is_dir() {
                continue;
            }
            let subagents = path.join("subagents");
            let Ok(agent_entries) = std::fs::read_dir(subagents) else {
                continue;
            };
            for agent in agent_entries.flatten() {
                let agent_path = agent.path();
                let name = agent_path
                    .file_name()
                    .and_then(|value| value.to_str())
                    .unwrap_or_default();
                if agent_path.is_file() && name.starts_with("agent-") && name.ends_with(".jsonl") {
                    subagent_files.push(agent_path);
                }
            }
        }
    }
    main_files.sort();
    subagent_files.sort();
    main_files.extend(subagent_files);
    Ok(main_files)
}

pub(crate) fn calculate_streaks(activity: &[DailyActivity], today: NaiveDate) -> StreakStats {
    if activity.is_empty() {
        return StreakStats::default();
    }
    let active_dates = activity
        .iter()
        .filter_map(|day| NaiveDate::parse_from_str(&day.date, "%Y-%m-%d").ok())
        .collect::<BTreeSet<_>>();

    let mut current_streak = 0u64;
    let mut current_streak_start = None;
    let mut cursor = today;
    loop {
        let local_midnight = cursor.and_hms_opt(0, 0, 0).unwrap();
        let key = Local
            .from_local_datetime(&local_midnight)
            .earliest()
            .map(|value| value.with_timezone(&Utc).date_naive())
            .unwrap_or(cursor);
        if !active_dates.contains(&key) {
            break;
        }
        current_streak += 1;
        current_streak_start = Some(key.format("%Y-%m-%d").to_string());
        let Some(previous) = cursor.pred_opt() else {
            break;
        };
        cursor = previous;
    }

    let mut longest_streak = 0u64;
    let mut longest_streak_start = None;
    let mut longest_streak_end = None;
    let mut run_start = None;
    let mut previous = None;
    let mut run_length = 0u64;
    for date in active_dates {
        if previous.is_some_and(|prev: NaiveDate| prev.succ_opt() == Some(date)) {
            run_length += 1;
        } else {
            run_start = Some(date);
            run_length = 1;
        }
        if run_length > longest_streak {
            longest_streak = run_length;
            longest_streak_start = run_start.map(|value| value.format("%Y-%m-%d").to_string());
            longest_streak_end = Some(date.format("%Y-%m-%d").to_string());
        }
        previous = Some(date);
    }

    StreakStats {
        current_streak,
        longest_streak,
        current_streak_start,
        longest_streak_start,
        longest_streak_end,
    }
}

fn processed_stats_to_claude_code_stats(stats: ProcessedStats) -> ClaudeCodeStats {
    let daily_activity = stats.daily_activity.into_values().collect::<Vec<_>>();
    let daily_model_tokens = stats
        .daily_model_tokens
        .into_iter()
        .map(|(date, tokens_by_model)| DailyModelTokens {
            date,
            tokens_by_model,
        })
        .collect::<Vec<_>>();
    let longest_session = stats
        .session_stats
        .iter()
        .max_by_key(|session| session.duration)
        .cloned();
    let first_session_date = stats
        .session_stats
        .iter()
        .map(|session| session.timestamp.as_str())
        .min()
        .map(str::to_string);
    let last_session_date = stats
        .session_stats
        .iter()
        .map(|session| session.timestamp.as_str())
        .max()
        .map(str::to_string);
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
    let peak_activity_hour = stats
        .hour_counts
        .iter()
        .reduce(|max, item| if item.1 > max.1 { item } else { max })
        .map(|(hour, _)| *hour);
    let total_days = match (
        first_session_date
            .as_deref()
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok()),
        last_session_date
            .as_deref()
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok()),
    ) {
        (Some(first), Some(last)) => {
            let milliseconds = last.signed_duration_since(first).num_milliseconds().max(0);
            ((milliseconds + 86_400_000 - 1) / 86_400_000 + 1) as u64
        }
        _ => 0,
    };
    let today = Local::now().date_naive();

    ClaudeCodeStats {
        total_sessions: stats.session_stats.len() as u64,
        total_messages: stats.total_messages,
        total_days,
        active_days: daily_activity.len() as u64,
        longest_session,
        streaks: calculate_streaks(&daily_activity, today),
        peak_activity_day,
        peak_activity_hour,
        daily_activity,
        daily_model_tokens,
        model_usage: stats.model_usage,
        first_session_date,
        last_session_date,
        total_speculation_time_saved_ms: stats.total_speculation_time_saved_ms,
        shot_distribution: None,
        one_shot_rate: None,
    }
}

pub fn empty_stats() -> ClaudeCodeStats {
    ClaudeCodeStats::default()
}

/// Maps to: CC `utils/stats.ts#aggregateClaudeCodeStats`.
pub fn aggregate_claude_code_stats() -> anyhow::Result<ClaudeCodeStats> {
    let files = get_all_session_files()?;
    if files.is_empty() {
        return Ok(empty_stats());
    }
    let today = Utc::now().date_naive();
    let yesterday = today.pred_opt().unwrap_or(today);
    let cache = crate::utils::stats_cache::with_stats_cache_lock(|| {
        let mut cache = crate::utils::stats_cache::load_stats_cache();
        let last_computed = cache
            .last_computed_date
            .as_deref()
            .and_then(|date| NaiveDate::parse_from_str(date, "%Y-%m-%d").ok());
        let range_to_process = match last_computed {
            None => Some((None, yesterday)),
            Some(last) if last < yesterday => {
                crate::utils::stats_cache::next_day(last).map(|next| (Some(next), yesterday))
            }
            _ => None,
        };
        if let Some((from_date, to_date)) = range_to_process {
            let historical = process_session_files(
                &files,
                ProcessOptions {
                    from_date,
                    to_date: Some(to_date),
                },
            );
            cache = crate::utils::stats_cache::merge_cache_with_new_stats(
                cache,
                historical,
                to_date.format("%Y-%m-%d").to_string(),
            );
            crate::utils::stats_cache::save_stats_cache(&cache);
        }
        cache
    });
    let today_stats = process_session_files(
        &files,
        ProcessOptions {
            from_date: Some(today),
            to_date: Some(today),
        },
    );
    Ok(crate::utils::stats_cache::cache_to_stats(
        cache,
        Some(today_stats),
    ))
}

/// Maps to: CC `utils/stats.ts#aggregateClaudeCodeStatsForRange`.
pub fn aggregate_claude_code_stats_for_range(
    range: StatsDateRange,
) -> anyhow::Result<ClaudeCodeStats> {
    if range == StatsDateRange::All {
        return aggregate_claude_code_stats();
    }
    let files = get_all_session_files()?;
    if files.is_empty() {
        return Ok(empty_stats());
    }
    let today = Utc::now().date_naive();
    let options = match range {
        StatsDateRange::SevenDays => ProcessOptions {
            from_date: today.checked_sub_days(chrono::Days::new(6)),
            to_date: None,
        },
        StatsDateRange::ThirtyDays => ProcessOptions {
            from_date: today.checked_sub_days(chrono::Days::new(29)),
            to_date: None,
        },
        StatsDateRange::All => unreachable!("all-time stats use the bounded cache path"),
    };
    Ok(processed_stats_to_claude_code_stats(process_session_files(
        &files, options,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::TEST_ENV_LOCK;

    fn write_lines(path: &Path, lines: &[Value]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let content = lines
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(path, format!("{content}\n")).unwrap();
    }

    #[test]
    fn stats_aggregation_counts_main_sessions_and_subagent_tokens_like_official() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!("cometix-stats-{}", uuid::Uuid::new_v4()));
        let _override = crate::utils::session_storage::set_test_projects_dir_override(&root);
        let _cache =
            crate::utils::stats_cache::set_test_stats_cache_path(root.join("stats-cache.json"));
        let project = root.join("project");
        let session_id = "session-one";
        write_lines(
            &project.join(format!("{session_id}.jsonl")),
            &[
                serde_json::json!({"type":"user","timestamp":"2026-07-18T10:00:00Z","message":{"role":"user","content":"hello"}}),
                serde_json::json!({"type":"assistant","timestamp":"2026-07-18T10:30:00Z","message":{"role":"assistant","model":"claude-sonnet-4-6","usage":{"input_tokens":10,"output_tokens":2,"cache_read_input_tokens":3},"content":[{"type":"tool_use","id":"t1","name":"Read","input":{}}]}}),
                serde_json::json!({"type":"speculation-accept","timeSavedMs":250}),
            ],
        );
        write_lines(
            &project
                .join(session_id)
                .join("subagents")
                .join("agent-worker.jsonl"),
            &[
                serde_json::json!({"type":"assistant","timestamp":"2026-07-18T10:05:00Z","isSidechain":true,"message":{"role":"assistant","model":"claude-sonnet-4-6","usage":{"input_tokens":5,"output_tokens":1},"content":[{"type":"tool_use","id":"t2","name":"Grep","input":{}}]}}),
            ],
        );

        let stats = aggregate_claude_code_stats().unwrap();
        assert_eq!(stats.total_sessions, 1);
        assert_eq!(stats.total_messages, 2);
        assert_eq!(stats.longest_session.as_ref().unwrap().duration, 1_800_000);
        assert_eq!(stats.model_usage["claude-sonnet-4-6"].input_tokens, 15);
        assert_eq!(stats.model_usage["claude-sonnet-4-6"].output_tokens, 3);
        assert_eq!(stats.daily_activity[0].tool_call_count, 2);
        // CC accumulates speculation-accept entries before applying the
        // session-date filter, so a cold historical + today pass observes the
        // fixture entry in both passes.
        assert_eq!(stats.total_speculation_time_saved_ms, 500);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn stats_aggregation_skips_synthetic_and_malformed_timestamp_sessions() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!("cometix-stats-{}", uuid::Uuid::new_v4()));
        let _override = crate::utils::session_storage::set_test_projects_dir_override(&root);
        let _cache =
            crate::utils::stats_cache::set_test_stats_cache_path(root.join("stats-cache.json"));
        let project = root.join("project");
        write_lines(
            &project.join("synthetic.jsonl"),
            &[
                serde_json::json!({"type":"user","timestamp":"2026-07-18T00:00:00Z"}),
                serde_json::json!({"type":"assistant","timestamp":"2026-07-18T00:01:00Z","message":{"model":"<synthetic>","usage":{"input_tokens":99,"output_tokens":99},"content":[]}}),
            ],
        );
        write_lines(
            &project.join("malformed.jsonl"),
            &[serde_json::json!({"type":"user","timestamp":"not-a-date"})],
        );

        let stats = aggregate_claude_code_stats().unwrap();
        assert_eq!(stats.total_sessions, 1);
        assert!(stats.model_usage.is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn stats_jsonl_consumer_matches_official_bun_multiline_and_bom() {
        // CC utils/stats.ts:177 delegates to utils/json.ts#readJSONLFile,
        // whose Bun parser accepts a JSON value spanning multiple lines.
        struct FixtureDirectory(PathBuf);
        impl Drop for FixtureDirectory {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let dir = FixtureDirectory(
            std::env::temp_dir().join(format!("cometix-json-stats-{}", uuid::Uuid::new_v4())),
        );
        std::fs::create_dir_all(&dir.0).unwrap();
        let path = dir.0.join("multiline.jsonl");
        let user = serde_json::json!({"type":"user","timestamp":"2026-07-18T10:00:00Z","message":{"role":"user","content":"hello"}});
        let assistant = serde_json::json!({"type":"assistant","timestamp":"2026-07-18T10:30:00Z","message":{"role":"assistant","model":"claude-sonnet-4-6","usage":{"input_tokens":10,"output_tokens":2},"content":[]}});
        std::fs::write(
            &path,
            format!(
                "\u{feff}{}\nBAD\n{}\n",
                serde_json::to_string_pretty(&user).unwrap(),
                serde_json::to_string_pretty(&assistant).unwrap()
            ),
        )
        .unwrap();
        let stats = process_session_files(&[path], ProcessOptions::default());
        assert_eq!(stats.total_messages, 2);
        assert_eq!(stats.session_stats.len(), 1);
        assert_eq!(stats.model_usage["claude-sonnet-4-6"].input_tokens, 10);
    }
}
