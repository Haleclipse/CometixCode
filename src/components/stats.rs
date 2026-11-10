//! Maps to: CC `components/Stats.tsx`.
//!
//! Stats collection lives in `utils/stats.rs` / `utils/stats_cache.rs` and is
//! dispatched from this component to worker threads, matching React Suspense's
//! off-render ownership. This module owns the official Pane/Tabs/heatmap,
//! date-range, overview/model layout, navigation, ANSI export, and loading/error
//! states. PNG screenshot clipboard transport remains an explicit seam.

use crate::components::design_system::pane::Pane;
use crate::components::design_system::tabs::{TabItem, TabsHeader};
use crate::components::spinner::Spinner as InlineSpinner;
use crate::constants::figures;
use crate::utils::format::{format_duration, format_number};
pub use crate::utils::stats::{
    ClaudeCodeStats, DailyActivity, DailyModelTokens, LongestSessionStats, ModelUsageStats,
    StatsDateRange, StreakStats,
};
use crate::utils::theme::Theme;
use iocraft::prelude::*;
use std::collections::BTreeMap;

pub const DATE_RANGE_ORDER: [StatsDateRange; 3] = [
    StatsDateRange::All,
    StatsDateRange::SevenDays,
    StatsDateRange::ThirtyDays,
];

/// Maps to: CC `Stats.tsx#DATE_RANGE_LABELS`.
pub fn date_range_label(range: StatsDateRange) -> &'static str {
    match range {
        StatsDateRange::SevenDays => "Last 7 days",
        StatsDateRange::ThirtyDays => "Last 30 days",
        StatsDateRange::All => "All time",
    }
}

/// Maps to: CC `Stats.tsx#getNextDateRange`.
pub fn get_next_date_range(current: StatsDateRange) -> StatsDateRange {
    let index = DATE_RANGE_ORDER
        .iter()
        .position(|range| *range == current)
        .unwrap_or(0);
    DATE_RANGE_ORDER[(index + 1) % DATE_RANGE_ORDER.len()]
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum StatsResult {
    Success(ClaudeCodeStats),
    Error(String),
    Empty,
    #[default]
    Loading,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StatsTab {
    #[default]
    Overview,
    Models,
}

/// Maps to: CC `Stats.tsx#formatPeakDay`.
pub fn format_peak_day(date_str: &str) -> String {
    let mut parts = date_str.split('-');
    let _year = parts.next();
    let month = parts
        .next()
        .and_then(|month| month.parse::<usize>().ok())
        .unwrap_or(0);
    let day = parts
        .next()
        .and_then(|day| day.parse::<u32>().ok())
        .unwrap_or(0);
    let month_name = [
        "", "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ]
    .get(month)
    .copied()
    .unwrap_or("");
    if month_name.is_empty() || day == 0 {
        date_str.to_string()
    } else {
        format!("{month_name} {day}")
    }
}

/// Maps to: CC `Stats.tsx` model usage sort comparator.
pub fn sorted_model_entries(stats: &ClaudeCodeStats) -> Vec<(String, ModelUsageStats)> {
    let mut entries = stats
        .model_usage
        .iter()
        .map(|(model, usage)| (model.clone(), usage.clone()))
        .collect::<Vec<_>>();
    entries.sort_by(|(model_a, a), (model_b, b)| {
        let total_b = b.input_tokens + b.output_tokens;
        let total_a = a.input_tokens + a.output_tokens;
        total_b.cmp(&total_a).then_with(|| model_a.cmp(model_b))
    });
    entries
}

pub fn total_tokens_for_models(entries: &[(String, ModelUsageStats)]) -> u64 {
    entries
        .iter()
        .map(|(_, usage)| usage.input_tokens + usage.output_tokens)
        .sum()
}

fn render_model_name(model: &str) -> String {
    crate::utils::model::model::render_model_name(model)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShotStatsData {
    pub avg_shots: String,
    pub buckets: Vec<ShotStatsBucket>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShotStatsBucket {
    pub label: String,
    pub count: u64,
    pub pct: u64,
}

/// Maps to: CC `Stats.tsx#OverviewTab` shot distribution shaping.
pub fn compute_shot_stats_data(
    distribution: Option<&BTreeMap<u64, u64>>,
    shot_stats_feature_enabled: bool,
) -> Option<ShotStatsData> {
    if !shot_stats_feature_enabled {
        return None;
    }
    let distribution = distribution?;
    let total: u64 = distribution.values().sum();
    if total == 0 {
        return None;
    }
    let total_shots: u64 = distribution
        .iter()
        .map(|(count, sessions)| count * sessions)
        .sum();
    let bucket = |min: u64, max: Option<u64>| -> u64 {
        distribution
            .iter()
            .filter(|(count, _)| **count >= min && max.is_none_or(|max| **count <= max))
            .map(|(_, sessions)| *sessions)
            .sum()
    };
    let pct = |count: u64| ((count as f64 / total as f64) * 100.0).round() as u64;
    let b1 = bucket(1, Some(1));
    let b2_5 = bucket(2, Some(5));
    let b6_10 = bucket(6, Some(10));
    let b11 = bucket(11, None);
    Some(ShotStatsData {
        avg_shots: format!("{:.1}", total_shots as f64 / total as f64),
        buckets: vec![
            ShotStatsBucket {
                label: "1-shot".to_string(),
                count: b1,
                pct: pct(b1),
            },
            ShotStatsBucket {
                label: "2–5 shot".to_string(),
                count: b2_5,
                pct: pct(b2_5),
            },
            ShotStatsBucket {
                label: "6–10 shot".to_string(),
                count: b6_10,
                pct: pct(b6_10),
            },
            ShotStatsBucket {
                label: "11+ shot".to_string(),
                count: b11,
                pct: pct(b11),
            },
        ],
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OverviewData {
    pub favorite_model: Option<String>,
    pub total_tokens: u64,
    pub range_days: u64,
    pub factoid: String,
    pub shot_stats: Option<ShotStatsData>,
}

/// Maps to: CC `Stats.tsx#OverviewTab` derived values.
pub fn overview_data(
    stats: &ClaudeCodeStats,
    date_range: StatsDateRange,
    shot_stats_feature_enabled: bool,
    factoid_index: usize,
) -> OverviewData {
    let model_entries = sorted_model_entries(stats);
    let favorite_model = model_entries
        .first()
        .map(|(model, _)| render_model_name(model));
    let total_tokens = total_tokens_for_models(&model_entries);
    let range_days = match date_range {
        StatsDateRange::SevenDays => 7,
        StatsDateRange::ThirtyDays => 30,
        StatsDateRange::All => stats.total_days,
    };
    OverviewData {
        favorite_model,
        total_tokens,
        range_days,
        factoid: generate_fun_factoid_at(stats, total_tokens, factoid_index),
        shot_stats: compute_shot_stats_data(
            stats.shot_distribution.as_ref(),
            shot_stats_feature_enabled,
        ),
    }
}

pub const BOOK_COMPARISONS: [(&str, u64); 24] = [
    ("The Little Prince", 22_000),
    ("The Old Man and the Sea", 35_000),
    ("A Christmas Carol", 37_000),
    ("Animal Farm", 39_000),
    ("Fahrenheit 451", 60_000),
    ("The Great Gatsby", 62_000),
    ("Slaughterhouse-Five", 64_000),
    ("Brave New World", 83_000),
    ("The Catcher in the Rye", 95_000),
    ("Harry Potter and the Philosopher's Stone", 103_000),
    ("The Hobbit", 123_000),
    ("1984", 123_000),
    ("To Kill a Mockingbird", 130_000),
    ("Pride and Prejudice", 156_000),
    ("Dune", 244_000),
    ("Moby-Dick", 268_000),
    ("Crime and Punishment", 274_000),
    ("A Game of Thrones", 381_000),
    ("Anna Karenina", 468_000),
    ("Don Quixote", 520_000),
    ("The Lord of the Rings", 576_000),
    ("The Count of Monte Cristo", 603_000),
    ("Les Misérables", 689_000),
    ("War and Peace", 730_000),
];

pub const TIME_COMPARISONS: [(&str, u64); 10] = [
    ("a TED talk", 18),
    ("an episode of The Office", 22),
    ("listening to Abbey Road", 47),
    ("a yoga class", 60),
    ("a World Cup soccer match", 90),
    ("a half marathon (average time)", 120),
    ("the movie Inception", 148),
    ("watching Titanic", 195),
    ("a transatlantic flight", 420),
    ("a full night of sleep", 480),
];

/// Maps to: CC `Stats.tsx#generateFunFactoid` candidate construction.
pub fn generate_fun_factoid_candidates(stats: &ClaudeCodeStats, total_tokens: u64) -> Vec<String> {
    let mut factoids = Vec::new();
    if total_tokens > 0 {
        for (name, tokens) in BOOK_COMPARISONS {
            if total_tokens >= tokens {
                let times = total_tokens as f64 / tokens as f64;
                if times >= 2.0 {
                    factoids.push(format!(
                        "You've used ~{}x more tokens than {name}",
                        times.floor() as u64
                    ));
                } else {
                    factoids.push(format!("You've used the same number of tokens as {name}"));
                }
            }
        }
    }

    if let Some(longest_session) = stats.longest_session.as_ref() {
        let session_minutes = longest_session.duration as f64 / (1000.0 * 60.0);
        for (name, minutes) in TIME_COMPARISONS {
            let ratio = session_minutes / minutes as f64;
            if ratio >= 2.0 {
                factoids.push(format!(
                    "Your longest session is ~{}x longer than {name}",
                    ratio.floor() as u64
                ));
            }
        }
    }
    factoids
}

/// Maps to: CC `Stats.tsx#generateFunFactoid`; caller supplies the random index
/// for deterministic tests/render snapshots.
pub fn generate_fun_factoid_at(
    stats: &ClaudeCodeStats,
    total_tokens: u64,
    random_index: usize,
) -> String {
    let factoids = generate_fun_factoid_candidates(stats, total_tokens);
    if factoids.is_empty() {
        String::new()
    } else {
        factoids[random_index % factoids.len()].clone()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DateRangeSelectorDisplay {
    pub labels: Vec<(StatsDateRange, String, bool)>,
    pub is_loading: bool,
}

/// Maps to: CC `Stats.tsx#DateRangeSelector` label state.
pub fn date_range_selector_display(
    date_range: StatsDateRange,
    is_loading: bool,
) -> DateRangeSelectorDisplay {
    DateRangeSelectorDisplay {
        labels: DATE_RANGE_ORDER
            .into_iter()
            .map(|range| {
                (
                    range,
                    date_range_label(range).to_string(),
                    range == date_range,
                )
            })
            .collect(),
        is_loading,
    }
}

pub const VISIBLE_MODELS: usize = 4;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelsWindow {
    pub visible_models: Vec<(String, ModelUsageStats)>,
    pub left_models: Vec<(String, ModelUsageStats)>,
    pub right_models: Vec<(String, ModelUsageStats)>,
    pub can_scroll_up: bool,
    pub can_scroll_down: bool,
    pub show_scroll_hint: bool,
}

/// Maps to: CC `Stats.tsx#ModelsTab` visible-model window shaping.
pub fn models_window(entries: &[(String, ModelUsageStats)], scroll_offset: usize) -> ModelsWindow {
    let max_offset = entries.len().saturating_sub(VISIBLE_MODELS);
    let scroll_offset = scroll_offset.min(max_offset);
    let visible_models = entries
        .iter()
        .skip(scroll_offset)
        .take(VISIBLE_MODELS)
        .cloned()
        .collect::<Vec<_>>();
    let midpoint = visible_models.len().div_ceil(2);
    let left_models = visible_models
        .iter()
        .take(midpoint)
        .cloned()
        .collect::<Vec<_>>();
    let right_models = visible_models
        .iter()
        .skip(midpoint)
        .cloned()
        .collect::<Vec<_>>();
    ModelsWindow {
        visible_models,
        left_models,
        right_models,
        can_scroll_up: scroll_offset > 0,
        can_scroll_down: scroll_offset < max_offset,
        show_scroll_hint: entries.len() > VISIBLE_MODELS,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChartLegend {
    pub model: String,
    pub colored_bullet: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChartOutput {
    pub chart: String,
    pub legend: Vec<ChartLegend>,
    pub x_axis_labels: String,
}

const CHART_TONE_RESET: char = '\u{e010}';

fn chart_tone_marker(series_index: usize) -> char {
    char::from_u32(0xe000 + series_index.min(2) as u32).unwrap_or('\u{e000}')
}

fn chart_colored(symbol: char, series_index: usize) -> String {
    format!(
        "{}{}{}",
        chart_tone_marker(series_index),
        symbol,
        CHART_TONE_RESET
    )
}

fn chart_axis_label(value: f64) -> String {
    if value >= 1_000_000.0 {
        format!("{:.1}M", value / 1_000_000.0)
    } else if value >= 1_000.0 {
        format!("{:.0}k", value / 1_000.0)
    } else {
        format!("{value:.0}")
    }
}

/// Native translation of the `asciichart.plot(...)` dependency used by
/// `Stats.tsx#generateTokenChart` (height=8, offset=3, default symbols).
fn compact_chart(series: &[Vec<u64>]) -> (String, usize) {
    let Some(first) = series.first().and_then(|values| values.first()) else {
        return (String::new(), 0);
    };
    let mut min = *first as f64;
    let mut max = *first as f64;
    for value in series.iter().flatten() {
        min = min.min(*value as f64);
        max = max.max(*value as f64);
    }
    let range = (max - min).abs();
    let height = 8.0;
    let ratio = if range != 0.0 { height / range } else { 1.0 };
    let min_scaled = (min * ratio).round() as i64;
    let max_scaled = (max * ratio).round() as i64;
    let row_count = (max_scaled - min_scaled).unsigned_abs() as usize;
    let data_width = series.iter().map(Vec::len).max().unwrap_or(0);
    let symbols = ['┼', '┤', '╶', '╴', '─', '╰', '╭', '╮', '╯', '│'];

    // Precompute every coordinate label before allocating the plot. The JS
    // dependency uses a fixed offset and stores a whole label in one grid
    // cell, so a 7-character label beside 6-character labels shifts the axis
    // by one column. Right-align against the longest rendered label instead.
    let labels = (0..=row_count)
        .map(|row| {
            let value = if row_count > 0 {
                max - row as f64 * range / row_count as f64
            } else {
                min_scaled as f64
            };
            chart_axis_label(value)
        })
        .collect::<Vec<_>>();
    let label_width = labels.iter().map(String::len).max().unwrap_or(0);
    // `generate_x_axis_labels` adds one space before its first label. Passing
    // label_width + 1 therefore starts it below the first data column, after
    // `<right-aligned label><space><axis>`.
    let y_axis_offset = label_width + 1;
    let mut grid = vec![vec![String::from(" "); data_width + 1]; row_count + 1];

    for scaled in min_scaled..=max_scaled {
        let row = (scaled - min_scaled) as usize;
        grid[row][0] = if scaled == 0 { symbols[0] } else { symbols[1] }.to_string();
    }

    for (series_index, values) in series.iter().enumerate() {
        let Some(first_value) = values.first() else {
            continue;
        };
        let first_y = ((*first_value as f64 * ratio).round() as i64 - min_scaled)
            .clamp(0, row_count as i64) as usize;
        grid[row_count - first_y][0] = chart_colored(symbols[0], series_index);
        for x in 0..values.len().saturating_sub(1) {
            let y0 = ((values[x] as f64 * ratio).round() as i64 - min_scaled)
                .clamp(0, row_count as i64) as usize;
            let y1 = ((values[x + 1] as f64 * ratio).round() as i64 - min_scaled)
                .clamp(0, row_count as i64) as usize;
            if y0 == y1 {
                grid[row_count - y0][x + 1] = chart_colored(symbols[4], series_index);
            } else {
                grid[row_count - y1][x + 1] =
                    chart_colored(if y0 > y1 { symbols[5] } else { symbols[6] }, series_index);
                grid[row_count - y0][x + 1] =
                    chart_colored(if y0 > y1 { symbols[7] } else { symbols[8] }, series_index);
                for y in y0.min(y1) + 1..y0.max(y1) {
                    grid[row_count - y][x + 1] = chart_colored(symbols[9], series_index);
                }
            }
        }
    }

    let text = grid
        .into_iter()
        .zip(labels)
        .map(|(row, label)| format!("{label:>label_width$} {}", row.concat()))
        .collect::<Vec<_>>()
        .join("\n");
    (text, y_axis_offset)
}

fn strip_chart_tone_markers(chart: &str) -> String {
    chart
        .chars()
        .filter(|character| {
            *character != CHART_TONE_RESET && !('\u{e000}'..='\u{e002}').contains(character)
        })
        .collect()
}

/// Maps to: CC `Stats.tsx#generateTokenChart` data selection, top-three model
/// filtering, legend, and x-axis labels. The chart body translates the
/// upstream `asciichart` dependency, with a longest-label axis alignment fix.
pub fn generate_token_chart(
    daily_tokens: &[DailyModelTokens],
    models: &[String],
    terminal_width: usize,
) -> Option<ChartOutput> {
    if daily_tokens.len() < 2 || models.is_empty() {
        return None;
    }
    let y_axis_width = 7usize;
    let available_width = terminal_width.saturating_sub(y_axis_width);
    let chart_width = 52usize.min(20usize.max(available_width));

    let recent_data = if daily_tokens.len() >= chart_width {
        daily_tokens[daily_tokens.len() - chart_width..].to_vec()
    } else {
        let repeat_count = chart_width / daily_tokens.len();
        let mut expanded = Vec::new();
        for day in daily_tokens {
            for _ in 0..repeat_count {
                expanded.push(day.clone());
            }
        }
        expanded
    };

    let mut series = Vec::<Vec<u64>>::new();
    let mut legend = Vec::<ChartLegend>::new();
    for model in models.iter().take(3) {
        let data = recent_data
            .iter()
            .map(|day| day.tokens_by_model.get(model).copied().unwrap_or(0))
            .collect::<Vec<_>>();
        if data.iter().any(|value| *value > 0) {
            series.push(data);
            legend.push(ChartLegend {
                model: render_model_name(model),
                colored_bullet: figures::get().bullet.to_string(),
            });
        }
    }
    if series.is_empty() {
        return None;
    }

    let (chart, y_axis_offset) = compact_chart(&series);
    Some(ChartOutput {
        chart,
        legend,
        x_axis_labels: generate_x_axis_labels(&recent_data, recent_data.len(), y_axis_offset),
    })
}

/// Maps to: CC `Stats.tsx#generateXAxisLabels`.
pub fn generate_x_axis_labels(
    data: &[DailyModelTokens],
    _chart_width: usize,
    y_axis_offset: usize,
) -> String {
    if data.is_empty() {
        return String::new();
    }
    let num_labels = 4usize.min(2usize.max(data.len() / 8));
    let usable_length = data.len().saturating_sub(6);
    let step = (usable_length / (num_labels.saturating_sub(1))).max(1);
    let mut label_positions = Vec::<(usize, String)>::new();
    for i in 0..num_labels {
        let idx = (i * step).min(data.len() - 1);
        label_positions.push((idx, format_peak_day(&data[idx].date)));
    }

    let mut result = " ".repeat(y_axis_offset);
    let mut current_pos = 0usize;
    for (pos, label) in label_positions {
        let spaces = pos.saturating_sub(current_pos).max(1);
        result.push_str(&" ".repeat(spaces));
        result.push_str(&label);
        current_pos = pos + label.len();
    }
    result
}

fn two_column_row(l1: &str, v1: &str, l2: &str, v2: &str) -> String {
    const COL1_LABEL_WIDTH: usize = 18;
    const COL2_START: usize = 40;
    const COL2_LABEL_WIDTH: usize = 18;
    let label1 = format!("{}:", l1);
    let label1 = format!("{label1:<COL1_LABEL_WIDTH$}");
    let col1_plain_len = label1.len() + v1.len();
    let space_between = 2usize.max(COL2_START.saturating_sub(col1_plain_len));
    let label2 = format!("{}:", l2);
    let label2 = format!("{label2:<COL2_LABEL_WIDTH$}");
    format!("{label1}{v1}{}{label2}{v2}", " ".repeat(space_between))
}

/// Maps to: CC `Stats.tsx#renderOverviewToAnsi` (without chalk SGR colors).
pub fn render_overview_to_ansi(stats: &ClaudeCodeStats) -> Vec<String> {
    let mut lines = Vec::new();
    let model_entries = sorted_model_entries(stats);
    let favorite_model = model_entries.first();
    let total_tokens = total_tokens_for_models(&model_entries);

    if favorite_model.is_some() {
        lines.push(two_column_row(
            "Favorite model",
            &render_model_name(&favorite_model.unwrap().0),
            "Total tokens",
            &format_number(total_tokens),
        ));
    }
    lines.push(String::new());
    lines.push(two_column_row(
        "Sessions",
        &format_number(stats.total_sessions),
        "Longest session",
        &stats
            .longest_session
            .as_ref()
            .map(|session| format_duration(session.duration))
            .unwrap_or_else(|| "N/A".to_string()),
    ));
    let current_streak = format!(
        "{} {}",
        stats.streaks.current_streak,
        if stats.streaks.current_streak == 1 {
            "day"
        } else {
            "days"
        }
    );
    let longest_streak = format!(
        "{} {}",
        stats.streaks.longest_streak,
        if stats.streaks.longest_streak == 1 {
            "day"
        } else {
            "days"
        }
    );
    lines.push(two_column_row(
        "Current streak",
        &current_streak,
        "Longest streak",
        &longest_streak,
    ));
    let active_days = format!("{}/{}", stats.active_days, stats.total_days);
    let peak_hour = stats
        .peak_activity_hour
        .map(|hour| format!("{}:00-{}:00", hour, hour + 1))
        .unwrap_or_else(|| "N/A".to_string());
    lines.push(two_column_row(
        "Active days",
        &active_days,
        "Peak hour",
        &peak_hour,
    ));
    lines.push(String::new());
    lines.push(generate_fun_factoid_at(stats, total_tokens, 0));
    lines.push(format!("Stats from the last {} days", stats.total_days));
    lines
}

/// Maps to: CC `Stats.tsx#renderModelsToAnsi` (without chalk SGR colors).
pub fn render_models_to_ansi(stats: &ClaudeCodeStats) -> Vec<String> {
    let mut lines = Vec::new();
    let model_entries = sorted_model_entries(stats);
    if model_entries.is_empty() {
        lines.push("No model usage data available".to_string());
        return lines;
    }
    let total_tokens = total_tokens_for_models(&model_entries);
    let models = model_entries
        .iter()
        .map(|(model, _)| model.clone())
        .collect::<Vec<_>>();
    if let Some(chart) = generate_token_chart(&stats.daily_model_tokens, &models, 80) {
        lines.push("Tokens per Day".to_string());
        lines.push(strip_chart_tone_markers(&chart.chart));
        lines.push(chart.x_axis_labels);
        lines.push(
            chart
                .legend
                .iter()
                .map(|item| format!("{} {}", item.colored_bullet, item.model))
                .collect::<Vec<_>>()
                .join(" · "),
        );
        lines.push(String::new());
    }
    let fig = figures::get();
    lines.push(format!(
        "{} Favorite: {} · {} Total: {} tokens",
        fig.star,
        render_model_name(&model_entries[0].0),
        fig.circle,
        format_number(total_tokens)
    ));
    lines.push(String::new());
    for (model, usage) in model_entries.iter().take(3) {
        let model_tokens = usage.input_tokens + usage.output_tokens;
        let percentage = if total_tokens == 0 {
            "0.0".to_string()
        } else {
            format!("{:.1}", model_tokens as f64 / total_tokens as f64 * 100.0)
        };
        lines.push(format!(
            "{} {} ({}%)",
            fig.bullet,
            render_model_name(model),
            percentage
        ));
        lines.push(format!(
            "  In: {} · Out: {}",
            format_number(usage.input_tokens),
            format_number(usage.output_tokens)
        ));
    }
    lines
}

/// Maps to: CC `Stats.tsx#renderStatsToAnsi`.
pub fn render_stats_to_ansi(stats: &ClaudeCodeStats, active_tab: StatsTab) -> String {
    let mut lines = match active_tab {
        StatsTab::Overview => render_overview_to_ansi(stats),
        StatsTab::Models => render_models_to_ansi(stats),
    };
    while lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.pop();
    }
    if let Some(last_line) = lines.last_mut() {
        let content_width: usize = if active_tab == StatsTab::Overview {
            70
        } else {
            80
        };
        let padding = 2usize.max(content_width.saturating_sub(last_line.len() + "/stats".len()));
        last_line.push_str(&" ".repeat(padding));
        last_line.push_str("/stats");
    }
    lines.join("\n")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatsViewData {
    pub result: StatsResult,
    pub all_time_stats: Option<ClaudeCodeStats>,
    pub date_range: StatsDateRange,
    pub active_tab: StatsTab,
    pub header_focused: bool,
    pub is_loading_filtered: bool,
    pub copy_status: Option<String>,
    pub scroll_offset: usize,
    pub terminal_width: usize,
}

impl Default for StatsViewData {
    fn default() -> Self {
        Self {
            result: StatsResult::Loading,
            all_time_stats: None,
            date_range: StatsDateRange::All,
            active_tab: StatsTab::Overview,
            header_focused: true,
            is_loading_filtered: false,
            copy_status: None,
            scroll_offset: 0,
            terminal_width: 80,
        }
    }
}

#[derive(Clone, Debug)]
enum StatsAsyncEvent {
    Loaded(StatsDateRange, Result<ClaudeCodeStats, String>),
    ClearCopyStatus,
}

fn spawn_stats_load(sender: async_channel::Sender<StatsAsyncEvent>, range: StatsDateRange) {
    std::thread::Builder::new()
        .name(format!("nyan-stats-{}", range.official_value()))
        .spawn(move || {
            let result = crate::utils::stats::aggregate_claude_code_stats_for_range(range)
                .map_err(|error| error.to_string());
            let _ = sender.send_blocking(StatsAsyncEvent::Loaded(range, result));
        })
        .ok();
}

#[derive(Default, Props)]
pub struct StatsProps<'a> {
    pub on_close: HandlerMut<'a, String>,
}

/// Maps to: CC `components/Stats.tsx#Stats` / `StatsContent`.
/// Transcript discovery and aggregation are dispatched to worker threads; the
/// retained component owns only React-equivalent view/cache/navigation state.
#[component]
pub fn Stats<'a>(props: &mut StatsProps<'a>, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut result = hooks.use_state(StatsResult::default);
    let mut all_time_stats = hooks.use_state(|| Option::<ClaudeCodeStats>::None);
    let mut stats_cache = hooks.use_state(BTreeMap::<StatsDateRange, ClaudeCodeStats>::new);
    let mut date_range = hooks.use_state(StatsDateRange::default);
    let mut active_tab = hooks.use_state(StatsTab::default);
    let mut header_focused = hooks.use_state(|| true);
    let mut is_loading_filtered = hooks.use_state(|| false);
    let mut copy_status = hooks.use_state(|| Option::<String>::None);
    let mut scroll_offset = hooks.use_state(|| 0usize);
    let mut initial_load_sent = hooks.use_state(|| false);
    let mut pending_close = hooks.use_state(|| Option::<String>::None);
    let (terminal_width, _) = hooks.use_terminal_size();
    let channel =
        hooks.use_const(|| std::sync::Arc::new(async_channel::unbounded::<StatsAsyncEvent>()));
    let sender_for_events = channel.0.clone();
    let sender_for_initial = channel.0.clone();
    let receiver = channel.1.clone();

    hooks.use_future(async move {
        while let Ok(event) = receiver.recv().await {
            match event {
                StatsAsyncEvent::Loaded(range, loaded) => match loaded {
                    Ok(stats) => {
                        if range == StatsDateRange::All {
                            all_time_stats.set(Some(stats.clone()));
                        }
                        let mut cache = stats_cache.read().clone();
                        cache.insert(range, stats.clone());
                        stats_cache.set(cache);
                        if date_range.get() == range {
                            result.set(
                                if range == StatsDateRange::All && stats.total_sessions == 0 {
                                    StatsResult::Empty
                                } else {
                                    StatsResult::Success(stats)
                                },
                            );
                            is_loading_filtered.set(false);
                        }
                    }
                    Err(message) => {
                        if date_range.get() == range {
                            if range == StatsDateRange::All {
                                result.set(StatsResult::Error(message));
                            }
                            is_loading_filtered.set(false);
                        }
                    }
                },
                StatsAsyncEvent::ClearCopyStatus => copy_status.set(None),
            }
        }
    });

    crate::components::design_system::tabs::use_tabs_keybindings(
        &mut hooks,
        header_focused.get(),
        {
            let mut active_tab = active_tab;
            let mut header_focused = header_focused;
            move || {
                active_tab.set(match active_tab.get() {
                    StatsTab::Overview => StatsTab::Models,
                    StatsTab::Models => StatsTab::Overview,
                });
                header_focused.set(true);
            }
        },
        {
            let mut active_tab = active_tab;
            let mut header_focused = header_focused;
            move || {
                active_tab.set(match active_tab.get() {
                    StatsTab::Overview => StatsTab::Models,
                    StatsTab::Models => StatsTab::Overview,
                });
                header_focused.set(true);
            }
        },
    );

    hooks.use_propagated_terminal_events({
        let current_result = result.read().clone();
        move |event| {
            let TerminalEvent::Key(KeyEvent {
                code,
                kind,
                modifiers,
                ..
            }) = event.event()
            else {
                return;
            };
            if *kind == KeyEventKind::Release {
                return;
            }
            let ctrl = modifiers.contains(KeyModifiers::CONTROL);
            match code {
                KeyCode::Esc | KeyCode::Char('c' | 'd') if *code == KeyCode::Esc || ctrl => {
                    pending_close.set(Some("Stats dialog dismissed".to_string()));
                    event.stop_propagation();
                }
                KeyCode::Char('r') if !ctrl && !modifiers.contains(KeyModifiers::ALT) => {
                    let next = get_next_date_range(date_range.get());
                    date_range.set(next);
                    scroll_offset.set(0);
                    if let Some(cached) = stats_cache.read().get(&next).cloned() {
                        result.set(
                            if next == StatsDateRange::All && cached.total_sessions == 0 {
                                StatsResult::Empty
                            } else {
                                StatsResult::Success(cached)
                            },
                        );
                    } else {
                        is_loading_filtered.set(next != StatsDateRange::All);
                        spawn_stats_load(sender_for_events.clone(), next);
                    }
                    event.stop_propagation();
                }
                KeyCode::Char('s') if ctrl => {
                    if matches!(current_result, StatsResult::Success(_)) {
                        // `ansiToPng`/image clipboard transport has no Rust
                        // source-equivalent yet. Surface the official failure
                        // status rather than reporting a fake successful copy.
                        copy_status.set(Some("copy failed".to_string()));
                        let sender = sender_for_events.clone();
                        std::thread::spawn(move || {
                            std::thread::sleep(std::time::Duration::from_secs(2));
                            let _ = sender.send_blocking(StatsAsyncEvent::ClearCopyStatus);
                        });
                    }
                    event.stop_propagation();
                }
                KeyCode::Down if active_tab.get() == StatsTab::Models => {
                    if header_focused.get() {
                        header_focused.set(false);
                    } else {
                        let entries = match &current_result {
                            StatsResult::Success(stats) => sorted_model_entries(stats).len(),
                            _ => 0,
                        };
                        let max_offset = entries.saturating_sub(VISIBLE_MODELS);
                        scroll_offset.set((scroll_offset.get() + 2).min(max_offset));
                    }
                    event.stop_propagation();
                }
                KeyCode::Up if active_tab.get() == StatsTab::Models => {
                    if !header_focused.get() && scroll_offset.get() > 0 {
                        scroll_offset.set(scroll_offset.get().saturating_sub(2));
                    } else {
                        header_focused.set(true);
                    }
                    event.stop_propagation();
                }
                _ => {}
            }
        }
    });

    if !initial_load_sent.get() {
        initial_load_sent.set(true);
        spawn_stats_load(sender_for_initial, StatsDateRange::All);
    }
    let close_message = { pending_close.read().clone() };
    if let Some(message) = close_message {
        pending_close.set(None);
        (props.on_close)(message);
    }

    let display_result = result.read().clone();
    let fallback_all_time = all_time_stats.read().clone();
    let display_result = if is_loading_filtered.get() {
        fallback_all_time
            .clone()
            .map(StatsResult::Success)
            .unwrap_or(display_result)
    } else {
        display_result
    };
    element! {
        StatsContent(data: StatsViewData {
            result: display_result,
            all_time_stats: fallback_all_time,
            date_range: date_range.get(),
            active_tab: active_tab.get(),
            header_focused: header_focused.get(),
            is_loading_filtered: is_loading_filtered.get(),
            copy_status: copy_status.read().clone(),
            scroll_offset: scroll_offset.get(),
            terminal_width: terminal_width as usize,
        })
    }
}

#[derive(Default, Props)]
pub struct StatsContentProps {
    pub data: StatsViewData,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StatsGlyphTone {
    Default,
    Inactive,
    Claude,
    Suggestion,
    Success,
    Warning,
}

fn stats_glyph_tone(character: char, chart: bool) -> StatsGlyphTone {
    if chart {
        match character {
            '●' => StatsGlyphTone::Suggestion,
            '◆' => StatsGlyphTone::Success,
            '▲' => StatsGlyphTone::Warning,
            _ => StatsGlyphTone::Default,
        }
    } else {
        match character {
            '░' | '▒' | '▓' | '█' => StatsGlyphTone::Claude,
            '·' => StatsGlyphTone::Inactive,
            _ => StatsGlyphTone::Default,
        }
    }
}

fn stats_styled_line(line: &str, chart: bool, theme: Theme) -> AnyElement<'static> {
    if line.is_empty() {
        return element! { View(height: 1u32) {} }.into_any();
    }
    let mut runs = Vec::<(StatsGlyphTone, String)>::new();
    let mut forced_tone = None;
    for character in line.chars() {
        forced_tone = match character {
            '\u{e000}' => Some(StatsGlyphTone::Suggestion),
            '\u{e001}' => Some(StatsGlyphTone::Success),
            '\u{e002}' => Some(StatsGlyphTone::Warning),
            CHART_TONE_RESET => None,
            _ => {
                let tone = forced_tone.unwrap_or_else(|| stats_glyph_tone(character, chart));
                if let Some((last_tone, text)) = runs.last_mut()
                    && *last_tone == tone
                {
                    text.push(character);
                    continue;
                }
                runs.push((tone, character.to_string()));
                forced_tone
            }
        };
    }
    element! {
        View(flex_direction: FlexDirection::Row, flex_shrink: 0.0f32) {
            #(runs.into_iter().map(|(tone, text)| {
                let color = match tone {
                    StatsGlyphTone::Default => None,
                    StatsGlyphTone::Inactive => Some(theme.inactive),
                    StatsGlyphTone::Claude => Some(theme.claude),
                    StatsGlyphTone::Suggestion => Some(theme.suggestion),
                    StatsGlyphTone::Success => Some(theme.success),
                    StatsGlyphTone::Warning => Some(theme.warning),
                };
                element! { Text(content: text, color: color, wrap: TextWrap::NoWrap) }
            }).collect::<Vec<_>>())
        }
    }
    .into_any()
}

#[derive(Default, Props)]
struct StatsStyledBlockProps {
    text: String,
    chart: bool,
}

#[component]
fn StatsStyledBlock(
    props: &StatsStyledBlockProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    element! {
        View(flex_direction: FlexDirection::Column, flex_shrink: 0.0f32) {
            #(props.text.lines().map(|line| stats_styled_line(line, props.chart, *theme)).collect::<Vec<_>>())
        }
    }
}

fn stats_value_cell(
    label: &'static str,
    value: Option<String>,
    suffix: Option<(String, Option<Color>)>,
    value_color: Color,
    bold: bool,
) -> AnyElement<'static> {
    element! {
        View(
            flex_direction: FlexDirection::Row,
            width: 28u32,
            height: 1u32,
            overflow: Overflow::Hidden,
            flex_shrink: 0.0f32,
        ) {
            #(value.map(|value| element! {
                Fragment {
                    Text(content: format!("{label}: "), wrap: TextWrap::NoWrap)
                    Text(
                        content: value,
                        color: value_color,
                        weight: if bold { Weight::Bold } else { Weight::Normal },
                        wrap: TextWrap::NoWrap,
                    )
                    #(suffix.map(|(text, color)| element! {
                        Text(content: text, color: color, wrap: TextWrap::NoWrap)
                    }))
                }
            }))
        }
    }
    .into_any()
}

#[derive(Default, Props)]
struct StatsOverviewContentProps {
    stats: ClaudeCodeStats,
    all_time_stats: ClaudeCodeStats,
    date_range: StatsDateRange,
    is_loading: bool,
    terminal_width: usize,
}

#[component]
fn StatsOverviewContent(
    props: &StatsOverviewContentProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let overview = overview_data(&props.stats, props.date_range, false, 0);
    let heatmap = (!props.all_time_stats.daily_activity.is_empty()).then(|| {
        crate::utils::heatmap::generate_heatmap(
            &props.all_time_stats.daily_activity,
            crate::utils::heatmap::HeatmapOptions {
                terminal_width: Some(props.terminal_width),
                show_month_labels: Some(true),
                ..crate::utils::heatmap::HeatmapOptions::default()
            },
        )
    });
    let longest_session = props
        .stats
        .longest_session
        .as_ref()
        .map(|session| format_duration(session.duration));
    let most_active_day = props
        .stats
        .peak_activity_day
        .as_deref()
        .map(format_peak_day);
    let longest_unit = if props.stats.streaks.longest_streak == 1 {
        " day"
    } else {
        " days"
    };
    let current_unit = if props.all_time_stats.streaks.current_streak == 1 {
        " day"
    } else {
        " days"
    };

    element! {
        View(flex_direction: FlexDirection::Column, margin_top: 1u32) {
            #(heatmap.map(|heatmap| element! {
                View(flex_direction: FlexDirection::Column, margin_bottom: 1u32) {
                    StatsStyledBlock(text: heatmap, chart: false)
                }
            }))
            DateRangeSelector(date_range: props.date_range, is_loading: props.is_loading)
            View(flex_direction: FlexDirection::Row, column_gap: 4u32, margin_bottom: 1u32) {
                #(stats_value_cell(
                    "Favorite model",
                    overview.favorite_model.clone(),
                    None,
                    theme.claude,
                    true,
                ))
                #(stats_value_cell(
                    "Total tokens",
                    Some(format_number(overview.total_tokens)),
                    None,
                    theme.claude,
                    false,
                ))
            }
            View(flex_direction: FlexDirection::Row, column_gap: 4u32) {
                #(stats_value_cell(
                    "Sessions",
                    Some(format_number(props.stats.total_sessions)),
                    None,
                    theme.claude,
                    false,
                ))
                #(stats_value_cell(
                    "Longest session",
                    longest_session,
                    None,
                    theme.claude,
                    false,
                ))
            }
            View(flex_direction: FlexDirection::Row, column_gap: 4u32) {
                #(stats_value_cell(
                    "Active days",
                    Some(props.stats.active_days.to_string()),
                    Some((format!("/{}", overview.range_days), Some(theme.subtle))),
                    theme.claude,
                    false,
                ))
                #(stats_value_cell(
                    "Longest streak",
                    Some(props.stats.streaks.longest_streak.to_string()),
                    Some((longest_unit.to_string(), None)),
                    theme.claude,
                    true,
                ))
            }
            View(flex_direction: FlexDirection::Row, column_gap: 4u32) {
                #(stats_value_cell(
                    "Most active day",
                    most_active_day,
                    None,
                    theme.claude,
                    false,
                ))
                #(stats_value_cell(
                    "Current streak",
                    Some(props.all_time_stats.streaks.current_streak.to_string()),
                    Some((current_unit.to_string(), None)),
                    theme.claude,
                    true,
                ))
            }
            #((!overview.factoid.is_empty()).then(|| element! {
                View(margin_top: 1u32) {
                    Text(content: overview.factoid, color: theme.suggestion, wrap: TextWrap::Wrap)
                }
            }))
        }
    }
}

fn stats_model_entry(
    model: &str,
    usage: &ModelUsageStats,
    total_tokens: u64,
    theme: Theme,
) -> AnyElement<'static> {
    let model_tokens = usage.input_tokens.saturating_add(usage.output_tokens);
    let percentage = if total_tokens == 0 {
        0.0
    } else {
        model_tokens as f64 / total_tokens as f64 * 100.0
    };
    element! {
        View(flex_direction: FlexDirection::Column, flex_shrink: 0.0f32) {
            View(flex_direction: FlexDirection::Row, overflow: Overflow::Hidden) {
                Text(content: format!("{} ", figures::get().bullet), wrap: TextWrap::NoWrap)
                Text(content: render_model_name(model), weight: Weight::Bold, wrap: TextWrap::NoWrap)
                Text(content: format!(" ({percentage:.1}%)"), color: theme.subtle, wrap: TextWrap::NoWrap)
            }
            Text(
                content: format!(
                    "  In: {} · Out: {}",
                    format_number(usage.input_tokens),
                    format_number(usage.output_tokens),
                ),
                color: theme.subtle,
                wrap: TextWrap::NoWrap,
            )
        }
    }
    .into_any()
}

#[derive(Default, Props)]
struct StatsModelsContentProps {
    stats: ClaudeCodeStats,
    date_range: StatsDateRange,
    is_loading: bool,
    scroll_offset: usize,
    terminal_width: usize,
}

#[component]
fn StatsModelsContent(
    props: &StatsModelsContentProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let entries = sorted_model_entries(&props.stats);
    let total_tokens = total_tokens_for_models(&entries);
    let window = models_window(&entries, props.scroll_offset);
    let models = entries
        .iter()
        .map(|(model, _)| model.clone())
        .collect::<Vec<_>>();
    if entries.is_empty() {
        return element! {
            Text(content: "No model usage data available", color: theme.subtle)
        }
        .into_any();
    }
    let chart = generate_token_chart(
        &props.stats.daily_model_tokens,
        &models,
        props.terminal_width,
    );

    element! {
        View(flex_direction: FlexDirection::Column, margin_top: 1u32) {
            #(chart.map(|chart| element! {
                View(flex_direction: FlexDirection::Column, margin_bottom: 1u32) {
                    Text(content: "Tokens per Day", weight: Weight::Bold, wrap: TextWrap::NoWrap)
                    StatsStyledBlock(text: chart.chart, chart: true)
                    Text(content: chart.x_axis_labels, color: theme.subtle, wrap: TextWrap::NoWrap)
                    View(flex_direction: FlexDirection::Row) {
                        #(chart.legend.into_iter().enumerate().map(|(index, item)| {
                            let color = match index {
                                0 => theme.suggestion,
                                1 => theme.success,
                                _ => theme.warning,
                            };
                            element! {
                                Fragment {
                                    #((index > 0).then(|| element! {
                                        Text(content: " · ", wrap: TextWrap::NoWrap)
                                    }))
                                    Text(content: item.colored_bullet, color: color, wrap: TextWrap::NoWrap)
                                    Text(content: format!(" {}", item.model), wrap: TextWrap::NoWrap)
                                }
                            }
                        }).collect::<Vec<_>>())
                    }
                }
            }))
            DateRangeSelector(date_range: props.date_range, is_loading: props.is_loading)
            View(flex_direction: FlexDirection::Column) {
                View(flex_direction: FlexDirection::Row, column_gap: 4u32) {
                    View(flex_direction: FlexDirection::Column, width: 36u32) {
                        #(window.left_models.iter().map(|(model, usage)| {
                            stats_model_entry(model, usage, total_tokens, *theme)
                        }).collect::<Vec<_>>())
                    }
                    View(flex_direction: FlexDirection::Column, width: 36u32) {
                        #(window.right_models.iter().map(|(model, usage)| {
                            stats_model_entry(model, usage, total_tokens, *theme)
                        }).collect::<Vec<_>>())
                    }
                }
                #(window.show_scroll_hint.then(|| element! {
                    View(margin_top: 1u32) {
                        Text(
                            content: format!(
                                "{} {} {}-{} of {} models (↑↓ to scroll)",
                                if window.can_scroll_up { figures::get().arrow_up } else { " " },
                                if window.can_scroll_down { figures::get().arrow_down } else { " " },
                                props.scroll_offset + 1,
                                (props.scroll_offset + VISIBLE_MODELS).min(entries.len()),
                                entries.len(),
                            ),
                            color: theme.subtle,
                            wrap: TextWrap::NoWrap,
                        )
                    }
                }))
            }
        }
    }
    .into_any()
}

#[derive(Default, Props)]
struct StatsSuccessContentProps {
    data: StatsViewData,
    stats: ClaudeCodeStats,
}

#[component]
fn StatsSuccessContent(
    props: &StatsSuccessContentProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let all_time_stats = props
        .data
        .all_time_stats
        .clone()
        .unwrap_or_else(|| props.stats.clone());
    let selected_index: usize = if props.data.active_tab == StatsTab::Overview {
        0
    } else {
        1
    };
    let copy_status = props.data.copy_status.clone().unwrap_or_default();

    element! {
        Pane(color: Some(theme.claude)) {
            View(flex_direction: FlexDirection::Row, column_gap: 1u32, margin_bottom: 1u32) {
                View(flex_direction: FlexDirection::Column) {
                    TabsHeader(
                        title: Some(String::new()),
                        color: Some(theme.claude),
                        tabs: vec![
                            TabItem::new("Overview", "Overview"),
                            TabItem::new("Models", "Models"),
                        ],
                        selected_index: selected_index,
                        header_focused: props.data.header_focused,
                    )
                    View(margin_top: 1u32) {
                    #(if props.data.active_tab == StatsTab::Overview {
                        element! {
                            StatsOverviewContent(
                                stats: props.stats.clone(),
                                all_time_stats: all_time_stats,
                                date_range: props.data.date_range,
                                is_loading: props.data.is_loading_filtered,
                                terminal_width: props.data.terminal_width,
                            )
                        }.into_any()
                    } else {
                        element! {
                            StatsModelsContent(
                                stats: props.stats.clone(),
                                date_range: props.data.date_range,
                                is_loading: props.data.is_loading_filtered,
                                scroll_offset: props.data.scroll_offset,
                                terminal_width: props.data.terminal_width,
                            )
                        }.into_any()
                    })
                    }
                }
            }
            View(padding_left: 2u32) {
                Text(
                    content: format!(
                        "Esc to cancel · r to cycle dates · ctrl+s to copy{}",
                        if copy_status.is_empty() { String::new() } else { format!(" · {copy_status}") },
                    ),
                    color: theme.inactive,
                    wrap: TextWrap::NoWrap,
                )
            }
        }
    }
}

/// Maps to: CC `components/Stats.tsx#StatsContent` render states.
#[component]
pub fn StatsContent(props: &StatsContentProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let data = props.data.clone();
    let result = data.result.clone();
    let copy_status = data.copy_status.clone().unwrap_or_default();

    match result {
        StatsResult::Loading => element! {
            View(margin_top: 1u32, flex_direction: FlexDirection::Row) {
                InlineSpinner()
                Text(content: " Loading your nyan stats…")
            }
        }
        .into_any(),
        StatsResult::Error(message) => element! {
            View(margin_top: 1u32) {
                Text(content: format!("Failed to load stats: {message}"), color: theme.error, wrap: TextWrap::Wrap)
            }
        }
        .into_any(),
        StatsResult::Empty => element! {
            View(margin_top: 1u32) {
                Text(content: "No stats available yet. Start using nyan!", color: theme.warning, wrap: TextWrap::NoWrap)
            }
        }
        .into_any(),
        StatsResult::Success(stats) => element! {
            StatsSuccessContent(data: data, stats: stats)
        }
        .into_any(),
    }
}

#[derive(Default, Props)]
pub struct DateRangeSelectorProps {
    pub date_range: StatsDateRange,
    pub is_loading: bool,
}

/// Maps to: CC `Stats.tsx#DateRangeSelector`.
#[component]
pub fn DateRangeSelector(
    props: &DateRangeSelectorProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let display = date_range_selector_display(props.date_range, props.is_loading);
    element! {
        View(
            flex_direction: FlexDirection::Row,
            column_gap: 1u32,
            margin_bottom: 1u32,
        ) {
            View(flex_direction: FlexDirection::Row) {
                #(display.labels.into_iter().enumerate().map(|(index, (_range, label, selected))| element! {
                    Fragment {
                        #((index > 0).then(|| element! {
                            Text(content: " · ", color: theme.inactive, wrap: TextWrap::NoWrap)
                        }))
                        Text(
                            content: label,
                            color: if selected { theme.claude } else { theme.inactive },
                            weight: if selected { Weight::Bold } else { Weight::Normal },
                            wrap: TextWrap::NoWrap,
                        )
                    }
                }).collect::<Vec<_>>())
            }
            #(display.is_loading.then(|| element! { InlineSpinner() }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats_canvas(data: StatsViewData, width: usize) -> Canvas {
        element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::get_theme(
                crate::utils::theme::ThemeName::Dark,
            ))) {
                // `use_app_state` is strict since P7: a component reading it
                // outside a provider panics instead of silently taking the
                // default. Mount the provider like the real tree does.
                crate::state::app_state::AppStateProvider(
                    children: crate::state::app_state::ProviderChildren::new(move || element! {
                        StatsContent(data: data.clone())
                    }.into_any()),
                )
            }
        }
        .render(Some(width))
    }

    fn render_stats(data: StatsViewData, width: usize) -> String {
        stats_canvas(data, width).to_string()
    }

    fn sample_stats() -> ClaudeCodeStats {
        let mut model_usage = BTreeMap::new();
        model_usage.insert(
            "claude-sonnet-4-6".to_string(),
            ModelUsageStats {
                input_tokens: 120_000,
                output_tokens: 30_000,
                cache_read_input_tokens: 0,
            },
        );
        model_usage.insert(
            "claude-haiku-4-5".to_string(),
            ModelUsageStats {
                input_tokens: 10_000,
                output_tokens: 5_000,
                cache_read_input_tokens: 0,
            },
        );
        let mut day1 = BTreeMap::new();
        day1.insert("claude-sonnet-4-6".to_string(), 100);
        let mut day2 = BTreeMap::new();
        day2.insert("claude-sonnet-4-6".to_string(), 300);
        ClaudeCodeStats {
            total_sessions: 12,
            total_days: 30,
            active_days: 8,
            longest_session: Some(LongestSessionStats {
                duration: 7_200_000,
                ..LongestSessionStats::default()
            }),
            streaks: StreakStats {
                current_streak: 2,
                longest_streak: 5,
                ..StreakStats::default()
            },
            peak_activity_day: Some("2026-07-09".to_string()),
            peak_activity_hour: Some(14),
            daily_activity: vec![DailyActivity {
                date: "2026-07-09".to_string(),
                message_count: 3,
                session_count: 1,
                tool_call_count: 0,
            }],
            daily_model_tokens: vec![
                DailyModelTokens {
                    date: "2026-07-08".to_string(),
                    tokens_by_model: day1,
                },
                DailyModelTokens {
                    date: "2026-07-09".to_string(),
                    tokens_by_model: day2,
                },
            ],
            model_usage,
            shot_distribution: None,
            ..ClaudeCodeStats::default()
        }
    }

    #[test]
    fn stats_date_range_order_and_labels_match_official() {
        assert_eq!(date_range_label(StatsDateRange::All), "All time");
        assert_eq!(date_range_label(StatsDateRange::SevenDays), "Last 7 days");
        assert_eq!(
            get_next_date_range(StatsDateRange::All),
            StatsDateRange::SevenDays
        );
        assert_eq!(
            get_next_date_range(StatsDateRange::SevenDays),
            StatsDateRange::ThirtyDays
        );
        assert_eq!(
            get_next_date_range(StatsDateRange::ThirtyDays),
            StatsDateRange::All
        );
        assert_eq!(format_peak_day("2026-07-09"), "Jul 9");
    }

    #[test]
    fn stats_overview_sorts_models_and_computes_factoids() {
        let stats = sample_stats();
        let entries = sorted_model_entries(&stats);
        assert_eq!(entries[0].0, "claude-sonnet-4-6");
        let overview = overview_data(&stats, StatsDateRange::SevenDays, false, 0);
        assert_eq!(overview.favorite_model.as_deref(), Some("Sonnet 4.6"));
        assert_eq!(overview.total_tokens, 165_000);
        assert_eq!(overview.range_days, 7);
        assert!(overview.factoid.contains("tokens"), "{}", overview.factoid);
    }

    #[test]
    fn stats_shot_distribution_matches_official_buckets() {
        let mut dist = BTreeMap::new();
        dist.insert(1, 3);
        dist.insert(3, 2);
        dist.insert(8, 1);
        dist.insert(12, 4);
        let shot = compute_shot_stats_data(Some(&dist), true).unwrap();
        assert_eq!(shot.avg_shots, "6.5");
        assert_eq!(shot.buckets[0].count, 3);
        assert_eq!(shot.buckets[1].count, 2);
        assert_eq!(shot.buckets[2].count, 1);
        assert_eq!(shot.buckets[3].count, 4);
        assert!(compute_shot_stats_data(Some(&dist), false).is_none());
    }

    #[test]
    fn stats_models_window_uses_four_visible_split_into_two_columns() {
        let entries = (0..6)
            .map(|i| {
                (
                    format!("m{i}"),
                    ModelUsageStats {
                        input_tokens: i,
                        output_tokens: 0,
                        cache_read_input_tokens: 0,
                    },
                )
            })
            .collect::<Vec<_>>();
        let window = models_window(&entries, 2);
        assert_eq!(
            window
                .visible_models
                .iter()
                .map(|(model, _)| model.as_str())
                .collect::<Vec<_>>(),
            vec!["m2", "m3", "m4", "m5"]
        );
        assert_eq!(window.left_models.len(), 2);
        assert_eq!(window.right_models.len(), 2);
        assert!(window.can_scroll_up);
        assert!(!window.can_scroll_down);
        assert!(window.show_scroll_hint);
    }

    #[test]
    fn stats_chart_axis_precomputes_longest_label_width_without_breaking_vertical_line() {
        let (chart, _) = compact_chart(&[vec![0, 903_200_000, 1_806_400_000]]);
        let plain = strip_chart_tone_markers(&chart);
        let axis_columns = plain
            .lines()
            .filter_map(|line| {
                line.char_indices()
                    .find(|(_, character)| matches!(character, '┼' | '┤'))
                    .map(|(byte, _)| unicode_width::UnicodeWidthStr::width(&line[..byte]))
            })
            .collect::<Vec<_>>();
        assert!(!axis_columns.is_empty(), "chart=\n{plain}");
        assert!(
            axis_columns.iter().all(|column| *column == axis_columns[0]),
            "axis columns must align against the longest label; chart=\n{plain}"
        );
        assert!(plain.contains("1806.4M"), "chart=\n{plain}");
        assert!(plain.contains("903.2M"), "chart=\n{plain}");
    }

    #[test]
    fn stats_token_chart_filters_top_three_and_generates_labels() {
        let stats = sample_stats();
        let models = sorted_model_entries(&stats)
            .into_iter()
            .map(|(model, _)| model)
            .collect::<Vec<_>>();
        let chart = generate_token_chart(&stats.daily_model_tokens, &models, 80).unwrap();
        assert!(
            strip_chart_tone_markers(&chart.chart).contains('─'),
            "{}",
            chart.chart
        );
        assert_eq!(chart.legend[0].model, "Sonnet 4.6");
        assert!(
            chart.x_axis_labels.contains("Jul"),
            "{}",
            chart.x_axis_labels
        );
        assert!(generate_token_chart(&stats.daily_model_tokens[..1], &models, 80).is_none());
    }

    #[test]
    fn stats_ansi_export_adds_stats_label() {
        let stats = sample_stats();
        let overview = render_stats_to_ansi(&stats, StatsTab::Overview);
        assert!(overview.contains("Favorite model"), "{overview}");
        assert!(overview.contains("/stats"), "{overview}");
        let models = render_stats_to_ansi(&stats, StatsTab::Models);
        assert!(models.contains("Favorite"), "{models}");
        assert!(models.contains("/stats"), "{models}");
    }

    #[test]
    fn stats_component_renders_loading_error_empty_and_success() {
        let loading = render_stats(StatsViewData::default(), 100);
        assert!(
            loading.contains("Loading your nyan stats"),
            "canvas=\n{loading}"
        );

        let error = render_stats(
            StatsViewData {
                result: StatsResult::Error("boom".to_string()),
                ..StatsViewData::default()
            },
            100,
        );
        assert!(
            error.contains("Failed to load stats: boom"),
            "canvas=\n{error}"
        );

        let empty = render_stats(
            StatsViewData {
                result: StatsResult::Empty,
                ..StatsViewData::default()
            },
            100,
        );
        assert!(empty.contains("No stats available yet"), "canvas=\n{empty}");

        let success = render_stats(
            StatsViewData {
                result: StatsResult::Success(sample_stats()),
                date_range: StatsDateRange::SevenDays,
                ..StatsViewData::default()
            },
            120,
        );
        assert!(success.contains("Overview"), "canvas=\n{success}");
        assert!(success.contains("Favorite model"), "canvas=\n{success}");
        assert!(success.contains("Last 7 days"), "canvas=\n{success}");
        assert!(success.contains("Esc to cancel"), "canvas=\n{success}");
    }

    #[test]
    fn stats_success_uses_official_pane_tabs_and_fixed_two_column_geometry() {
        let theme = *crate::utils::theme::get_theme(crate::utils::theme::ThemeName::Dark);
        let canvas = stats_canvas(
            StatsViewData {
                result: StatsResult::Success(sample_stats()),
                ..StatsViewData::default()
            },
            120,
        );
        let lines = canvas
            .to_string()
            .lines()
            .map(str::to_string)
            .collect::<Vec<_>>();
        assert!(lines.iter().any(|line| line.starts_with('─')));
        assert!(
            lines
                .iter()
                .all(|line| !line.contains('╭') && !line.contains('╰')),
            "Pane must not become a rounded Panel; canvas=\n{}",
            canvas
        );
        let usage_row = lines
            .iter()
            .find(|line| line.contains("Favorite model"))
            .expect("usage row");
        assert_eq!(usage_row.find("Favorite model"), Some(2));
        assert_eq!(usage_row.find("Total tokens"), Some(34));
        let header_row = lines
            .iter()
            .position(|line| line.contains("Overview") && line.contains("Models"))
            .expect("tabs row");
        let overview_column = lines[header_row].find("Overview").unwrap();
        assert_eq!(
            canvas
                .cell(overview_column, header_row)
                .unwrap()
                .background_color,
            Some(theme.claude)
        );
    }

    #[test]
    fn stats_component_renders_models_tab() {
        let text = render_stats(
            StatsViewData {
                result: StatsResult::Success(sample_stats()),
                active_tab: StatsTab::Models,
                ..StatsViewData::default()
            },
            140,
        );
        assert!(text.contains("Models"), "canvas=\n{text}");
        assert!(text.contains("Tokens per Day"), "canvas=\n{text}");
        let model_row = text
            .lines()
            .find(|line| line.contains("Sonnet 4.6") && line.contains("Haiku 4.5"))
            .expect("model row");
        assert!(model_row.contains("Haiku 4.5"), "canvas=\n{text}");
        let bullet_columns = model_row
            .match_indices('●')
            .map(|(byte, _)| unicode_width::UnicodeWidthStr::width(&model_row[..byte]))
            .collect::<Vec<_>>();
        assert_eq!(bullet_columns, vec![2, 42], "canvas=\n{text}");
    }
}
