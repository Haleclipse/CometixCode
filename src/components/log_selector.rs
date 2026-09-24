//! Maps to: CC `components/LogSelector.tsx`.
//! This is the `/resume` session selector shell. It owns LogSelector-level
//! state (view mode, search query, focused index, visible window) and delegates
//! row rendering to `CustomSelect/Select` + `design-system/ListItem`, matching
//! the official component split instead of hand-rendering a bespoke list.

use crate::components::custom_select::{Select, SelectLayout, SelectOptionData};
use crate::components::design_system::divider::Divider;
use crate::components::search_box::SearchBox;
use crate::components::session_preview::SessionPreview;
use crate::components::tag_tabs::TagTabs;
use crate::constants::xml::TICK_TAG;
use crate::hooks::use_double_press::use_double_press;
use crate::hooks::use_search_input::use_search_input;
use crate::utils::format::format_log_metadata;
use crate::utils::session_storage::{SessionSelection, SessionSummary};
use crate::utils::fuse::{BitapSearch, FuseOptions, KeyStore};
use crate::utils::theme::Theme;
use iocraft::prelude::*;
use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use std::cmp::Ordering;
use std::collections::HashSet;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const ALL_TAG_LABEL: &str = "All";
const AGENTIC_SEARCH_OPTION_LABEL: &str = "Search deeply using Claude →";
const AGENTIC_SEARCH_READONLY_NOTICE: &str =
    "Agentic search is unavailable in Cometix UI-only mode.";
const DEEP_SEARCH_READ_WINDOW_BYTES: u64 = 256 * 1024;
const DEEP_SEARCH_MAX_TEXT_LENGTH: usize = 50_000;
const DEEP_SEARCH_SNIPPET_CONTEXT_CHARS: usize = 50;
const FUSE_LIKE_THRESHOLD: f64 = 0.3;
const DATE_TIE_THRESHOLD_MS: u128 = 60 * 1000;

#[derive(Default, Props)]
pub struct LogSelectorProps<'a> {
    pub logs: Vec<SessionSummary>,
    pub max_height: usize,
    pub force_width: Option<usize>,
    pub on_cancel: HandlerMut<'a, ()>,
    pub on_select: HandlerMut<'a, SessionSelection>,
    /// Maps to: CC `LogSelectorProps.onLogsChanged`.
    pub on_logs_changed: HandlerMut<'a, ()>,
    /// Maps to: CC `LogSelectorProps.initialSearchQuery`.
    pub initial_search_query: Option<String>,
    pub show_all_projects: bool,
    /// Maps to: CC `LogSelectorProps.onLoadMore`.
    pub on_load_more: HandlerMut<'a, usize>,
    /// Maps to: CC `LogSelectorProps.isLoading`.
    pub is_loading: bool,
    /// Maps to: CC `LogSelectorProps.reloadGeneration`.
    pub reload_generation: u64,
    pub on_toggle_all_projects: HandlerMut<'a, ()>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ViewMode {
    List,
    Search,
    Preview,
    Rename,
}

#[derive(Clone, Debug)]
struct LogDisplayRow {
    log: SessionSummary,
    label: String,
    description: Option<String>,
    value: String,
    group_session_id: String,
    has_children: bool,
    is_group_parent: bool,
    is_child: bool,
}

fn parse_display_xml_open_tag_at(text: &str, start: usize) -> Option<(String, usize)> {
    let bytes = text.as_bytes();
    if bytes.get(start) != Some(&b'<') {
        return None;
    }
    let mut cursor = start + 1;
    let first = *bytes.get(cursor)?;
    if !first.is_ascii_lowercase() {
        return None;
    }

    let tag_start = cursor;
    cursor += 1;
    while let Some(byte) = bytes.get(cursor) {
        if byte.is_ascii_alphanumeric() || *byte == b'_' || *byte == b'-' {
            cursor += 1;
        } else {
            break;
        }
    }
    let tag = text[tag_start..cursor].to_string();

    match bytes.get(cursor).copied() {
        Some(b'>') => Some((tag, cursor + 1)),
        Some(byte) if byte.is_ascii_whitespace() => {
            let tag_end = text[cursor..].find('>')?;
            Some((tag, cursor + tag_end + 1))
        }
        _ => None,
    }
}

fn strip_display_tags_allow_empty(text: &str) -> String {
    // Mirrors official `stripDisplayTagsAllowEmpty()` and its backreferenced
    // lowercase XML-block regex. Rust regexes do not support backreferences,
    // so scan opener/closing pairs explicitly and remove only matching tags.
    let mut result = String::with_capacity(text.len());
    let mut cursor = 0usize;
    while cursor < text.len() {
        if let Some((tag, content_start)) = parse_display_xml_open_tag_at(text, cursor) {
            let closing = format!("</{tag}>");
            if let Some(relative_close_start) = text[content_start..].find(&closing) {
                let mut end = content_start + relative_close_start + closing.len();
                if text[end..].starts_with('\n') {
                    end += 1;
                }
                cursor = end;
                continue;
            }
        }

        let ch = text[cursor..]
            .chars()
            .next()
            .expect("cursor is always on a char boundary");
        result.push(ch);
        cursor += ch.len_utf8();
    }
    result.trim().to_string()
}

fn strip_display_tags(text: &str) -> String {
    let result = strip_display_tags_allow_empty(text);
    if result.is_empty() {
        text.to_string()
    } else {
        result
    }
}

fn truncate_to_width(text: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_string();
    }
    if max_width <= 1 {
        return "…".to_string();
    }

    let mut width = 0usize;
    let mut result = String::new();
    for grapheme in text.graphemes(true) {
        let grapheme_width = UnicodeWidthStr::width(grapheme).max(1);
        if width + grapheme_width > max_width - 1 {
            break;
        }
        result.push_str(grapheme);
        width += grapheme_width;
    }
    result.push('…');
    result
}

fn normalize_and_truncate_to_width(text: &str, max_width: usize) -> String {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate_to_width(normalized.trim(), max_width)
}

fn raw_log_title(log: &SessionSummary) -> String {
    let is_autonomous_prompt = log.display.starts_with(&format!("<{TICK_TAG}>"));
    let stripped_first_prompt = if log.display.is_empty() {
        String::new()
    } else {
        strip_display_tags_allow_empty(&log.display)
    };
    let use_first_prompt = !stripped_first_prompt.is_empty() && !is_autonomous_prompt;

    let title = log
        .agent_name
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            log.custom_title
                .as_deref()
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        })
        .or_else(|| {
            log.summary
                .as_deref()
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        })
        .or_else(|| {
            if use_first_prompt {
                Some(stripped_first_prompt)
            } else {
                None
            }
        })
        .or_else(|| {
            if is_autonomous_prompt {
                Some("Autonomous session".to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| log.session_id.chars().take(8).collect());

    strip_display_tags(&title).trim().to_string()
}

fn build_log_label(log: &SessionSummary, max_label_width: usize) -> String {
    let sidechain_suffix = if log.is_sidechain { " (sidechain)" } else { "" };
    let suffix_width = UnicodeWidthStr::width(sidechain_suffix);
    let title = normalize_and_truncate_to_width(
        &raw_log_title(log),
        max_label_width.saturating_sub(suffix_width).max(1),
    );
    format!("{title}{sidechain_suffix}")
}

fn build_log_metadata(log: &SessionSummary, show_project_path: bool) -> String {
    // The metadata field is the source `utils/format.ts#formatLogMetadata`
    // owner.  LogSelector only appends its view-specific project path.
    let mut metadata = format_log_metadata(
        log.modified,
        None,
        Some(log.file_size),
        log.git_branch.as_deref(),
        log.tag.as_deref(),
        log.agent_setting.as_deref(),
        log.pr_number,
        log.pr_repository.as_deref(),
    );
    if show_project_path {
        if let Some(project_path) = log.project_path.as_deref().filter(|s| !s.is_empty()) {
            if !metadata.is_empty() {
                metadata.push_str(" · ");
            }
            metadata.push_str(project_path);
        }
    }
    metadata
}

fn log_deep_search_key(log: &SessionSummary) -> String {
    log.file_path.display().to_string()
}

#[derive(Clone, Debug, PartialEq)]
struct DeepSearchMatch {
    log_key: String,
    score: f64,
    modified: std::time::SystemTime,
    snippet: Option<String>,
}

fn lookup_deep_search_match<'a>(
    log: &SessionSummary,
    deep_search_matches: &'a [DeepSearchMatch],
) -> Option<&'a DeepSearchMatch> {
    let key = log_deep_search_key(log);
    deep_search_matches
        .iter()
        .find(|candidate| candidate.log_key == key)
}

fn lookup_deep_search_snippet<'a>(
    log: &SessionSummary,
    deep_search_matches: &'a [DeepSearchMatch],
) -> Option<&'a str> {
    lookup_deep_search_match(log, deep_search_matches)
        .and_then(|candidate| candidate.snippet.as_deref())
}

fn build_log_metadata_with_deep_snippet(
    log: &SessionSummary,
    show_project_path: bool,
    is_child: bool,
    deep_search_matches: &[DeepSearchMatch],
) -> String {
    let metadata = build_log_metadata(log, show_project_path);
    match (
        lookup_deep_search_snippet(log, deep_search_matches),
        is_child,
    ) {
        (Some(snippet), true) => format!("    {metadata}\n      {snippet}"),
        (Some(snippet), false) => format!("{metadata}\n  {snippet}"),
        (None, true) => format!("    {metadata}"),
        (None, false) => metadata,
    }
}

fn build_grouped_log_label(
    log: &SessionSummary,
    max_label_width: usize,
    is_child: bool,
    fork_count: usize,
    is_expanded: bool,
) -> String {
    if is_child {
        let prefix = "  ▸ ";
        let label = build_log_label(log, max_label_width.saturating_sub(4).max(1));
        return format!("{prefix}{label}");
    }

    let prefix = if fork_count > 0 {
        if is_expanded { "▼ " } else { "▶ " }
    } else {
        ""
    };
    let suffix = if fork_count > 0 {
        format!(
            " (+{fork_count} other {})",
            if fork_count == 1 {
                "session"
            } else {
                "sessions"
            }
        )
    } else {
        String::new()
    };
    let reserved_width = UnicodeWidthStr::width(prefix) + UnicodeWidthStr::width(suffix.as_str());
    let label = build_log_label(log, max_label_width.saturating_sub(reserved_width).max(1));
    format!("{prefix}{label}{suffix}")
}

fn is_group_expanded(group_id: &str, expanded_group_session_ids: &[String]) -> bool {
    expanded_group_session_ids.iter().any(|id| id == group_id)
}

fn group_log_rows(
    logs: &[SessionSummary],
    max_label_width: usize,
    show_project_path: bool,
    expanded_group_session_ids: &[String],
    expand_all_groups: bool,
    deep_search_matches: &[DeepSearchMatch],
) -> Vec<LogDisplayRow> {
    let mut groups: Vec<(String, Vec<(usize, SessionSummary)>)> = Vec::new();
    for (index, log) in logs.iter().cloned().enumerate() {
        if let Some((_, group_logs)) = groups
            .iter_mut()
            .find(|(session_id, _)| session_id == &log.session_id)
        {
            group_logs.push((index, log));
        } else {
            groups.push((log.session_id.clone(), vec![(index, log)]));
        }
    }

    for (_, group_logs) in groups.iter_mut() {
        group_logs.sort_by(|(_, a), (_, b)| b.modified.cmp(&a.modified));
    }
    groups.sort_by(|(_, a), (_, b)| b[0].1.modified.cmp(&a[0].1.modified));

    let mut rows = Vec::new();
    for (session_id, group_logs) in groups {
        let fork_count = group_logs.len().saturating_sub(1);
        let (parent_original_index, parent_log) = group_logs[0].clone();
        let parent_description = build_log_metadata_with_deep_snippet(
            &parent_log,
            show_project_path,
            false,
            deep_search_matches,
        );
        let parent_is_expanded = fork_count > 0
            && (expand_all_groups || is_group_expanded(&session_id, expanded_group_session_ids));
        let parent_label = build_grouped_log_label(
            &parent_log,
            max_label_width,
            false,
            fork_count,
            parent_is_expanded,
        );
        rows.push(LogDisplayRow {
            log: parent_log,
            label: parent_label,
            description: Some(parent_description),
            value: format!("{parent_original_index}"),
            group_session_id: session_id.clone(),
            has_children: fork_count > 0,
            is_group_parent: true,
            is_child: false,
        });

        if fork_count == 0
            || !(expand_all_groups || is_group_expanded(&session_id, expanded_group_session_ids))
        {
            continue;
        }

        for (child_original_index, child_log) in group_logs.into_iter().skip(1) {
            let child_description = build_log_metadata_with_deep_snippet(
                &child_log,
                show_project_path,
                true,
                deep_search_matches,
            );
            let child_label = build_grouped_log_label(&child_log, max_label_width, true, 0, false);
            rows.push(LogDisplayRow {
                log: child_log,
                label: child_label,
                description: Some(child_description),
                value: format!("{child_original_index}"),
                group_session_id: session_id.clone(),
                has_children: true,
                is_group_parent: false,
                is_child: true,
            });
        }
    }
    rows
}

fn expand_collapse_hint(
    row: Option<&LogDisplayRow>,
    expanded_group_session_ids: &[String],
) -> Option<&'static str> {
    let row = row?;
    if !row.has_children {
        return None;
    }
    if row.is_child {
        return Some("← to collapse");
    }
    if is_group_expanded(&row.group_session_id, expanded_group_session_ids) {
        Some("← to collapse")
    } else {
        Some("→ to expand")
    }
}

fn apply_custom_title_overrides(
    logs: impl IntoIterator<Item = SessionSummary>,
    overrides: &[(String, String)],
) -> Vec<SessionSummary> {
    logs.into_iter()
        .map(|mut log| {
            if let Some((_, title)) = overrides
                .iter()
                .rev()
                .find(|(session_id, _)| session_id == &log.session_id)
            {
                log.custom_title = Some(title.clone());
            }
            log
        })
        .collect()
}

fn upsert_custom_title_override(
    overrides: &mut Vec<(String, String)>,
    session_id: &str,
    title: String,
) {
    overrides.retain(|(existing_session_id, _)| existing_session_id != session_id);
    overrides.push((session_id.to_string(), title));
}

fn log_matches_query(log: &SessionSummary, query_lower: &str) -> bool {
    if query_lower.is_empty() {
        return true;
    }
    let pr_info = log
        .pr_number
        .map(|number| {
            format!(
                "pr #{number} {}",
                log.pr_repository.as_deref().unwrap_or("")
            )
        })
        .unwrap_or_default()
        .to_lowercase();

    raw_log_title(log).to_lowercase().contains(query_lower)
        || log
            .git_branch
            .as_deref()
            .unwrap_or("")
            .to_lowercase()
            .contains(query_lower)
        || log
            .tag
            .as_deref()
            .unwrap_or("")
            .to_lowercase()
            .contains(query_lower)
        || pr_info.contains(query_lower)
}

fn normalize_search_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn read_bounded_text(path: &Path) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let file_size = file.metadata().ok()?.len();
    if file_size <= DEEP_SEARCH_READ_WINDOW_BYTES.saturating_mul(2) {
        let mut text = String::new();
        file.read_to_string(&mut text).ok()?;
        return Some(text);
    }

    let mut head = vec![0u8; DEEP_SEARCH_READ_WINDOW_BYTES as usize];
    file.read_exact(&mut head).ok()?;
    file.seek(SeekFrom::End(-(DEEP_SEARCH_READ_WINDOW_BYTES as i64)))
        .ok()?;
    let mut tail = vec![0u8; DEEP_SEARCH_READ_WINDOW_BYTES as usize];
    file.read_exact(&mut tail).ok()?;

    let mut text = String::from_utf8_lossy(&head).to_string();
    text.push('\n');
    text.push_str(&String::from_utf8_lossy(&tail));
    Some(text)
}

fn searchable_text_from_content(content: &serde_json::Value) -> String {
    match content {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Array(blocks) => blocks
            .iter()
            .filter_map(|block| match block {
                serde_json::Value::String(text) => Some(text.as_str()),
                serde_json::Value::Object(_) => block.get("text").and_then(|value| value.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

fn searchable_text_from_entry(entry: &serde_json::Value) -> String {
    if !matches!(
        entry.get("type").and_then(|value| value.as_str()),
        Some("user" | "assistant")
    ) {
        return String::new();
    }

    entry
        .get("message")
        .and_then(|message| message.get("content"))
        .map(searchable_text_from_content)
        .unwrap_or_default()
}

fn searchable_text_from_jsonl(raw: &str) -> String {
    raw.lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .map(|entry| searchable_text_from_entry(&entry))
        .filter(|text| !text.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn build_deep_search_text(log: &SessionSummary) -> String {
    let metadata = [
        log.custom_title.as_deref(),
        log.summary.as_deref(),
        Some(log.display.as_str()),
        log.git_branch.as_deref(),
        log.tag.as_deref(),
        log.pr_repository.as_deref(),
    ]
    .into_iter()
    .flatten()
    .filter(|value| !value.is_empty())
    .collect::<Vec<_>>()
    .join(" ");
    let transcript = read_bounded_text(&log.file_path)
        .map(|raw| searchable_text_from_jsonl(&raw))
        .unwrap_or_default();
    let normalized = normalize_search_text(&format!("{metadata} {transcript}"));
    truncate_to_width(&normalized, DEEP_SEARCH_MAX_TEXT_LENGTH)
}

fn slice_chars(text: &str, start: usize, end: usize) -> String {
    text.chars()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect()
}

fn format_deep_search_snippet(normalized: &str, match_start: usize, match_end: usize) -> String {
    let total_chars = normalized.chars().count();
    let snippet_start = match_start.saturating_sub(DEEP_SEARCH_SNIPPET_CONTEXT_CHARS);
    let snippet_end = (match_end + DEEP_SEARCH_SNIPPET_CONTEXT_CHARS).min(total_chars);
    let mut snippet = String::new();
    if snippet_start > 0 {
        snippet.push('…');
    }
    snippet.push_str(&slice_chars(normalized, snippet_start, snippet_end));
    if snippet_end < total_chars {
        snippet.push('…');
    }
    snippet
}

fn fuzzy_deep_search_match_range(normalized: &str, query: &str) -> Option<(usize, usize)> {
    let pattern = Pattern::new(
        query,
        CaseMatching::Ignore,
        Normalization::Smart,
        AtomKind::Fuzzy,
    );
    let mut matcher = Matcher::new(Config::DEFAULT);
    let mut buf = Vec::new();
    let mut indices = Vec::new();
    pattern.indices(
        Utf32Str::new(normalized, &mut buf),
        &mut matcher,
        &mut indices,
    )?;
    indices.sort_unstable();
    indices.dedup();
    let first = *indices.first()? as usize;
    let last = *indices.last()? as usize;
    Some((first, last.saturating_add(1)))
}

fn extract_deep_search_snippet(text: &str, query_lower: &str) -> Option<String> {
    let query = normalize_search_text(query_lower).to_lowercase();
    if query.trim().is_empty() {
        return None;
    }
    let normalized = normalize_search_text(text);
    let lower = normalized.to_lowercase();
    if let Some(match_byte) = lower.find(&query) {
        let match_char = lower[..match_byte].chars().count();
        let query_chars = query.chars().count().max(1);
        return Some(format_deep_search_snippet(
            &normalized,
            match_char,
            match_char + query_chars,
        ));
    }

    let (match_start, match_end) = fuzzy_deep_search_match_range(&normalized, &query)?;
    Some(format_deep_search_snippet(
        &normalized,
        match_start,
        match_end,
    ))
}

/// Maps to: CC `LogSelector.tsx:301-306`
/// `new Fuse(logsWithText, { keys: ['searchableText'], threshold,
/// ignoreLocation: true, includeScore: true })`.
///
/// One `searchableText` key, so there are no weights to apply here — the
/// single-field `score` is the whole result. The Fuse stand-in itself lives in
/// `utils/fuse.rs`, shared with the other two sites that import Fuse in CC.
fn fuse_like_score(text: &str, query_lower: &str) -> Option<f64> {
    let query = normalize_search_text(query_lower).to_lowercase();
    // An empty query is not a match-everything here: the caller already
    // rejects blank input, and Fuse would have nothing to search for.
    if query.is_empty() {
        return None;
    }
    BitapSearch::new(
        &query,
        FuseOptions {
            threshold: FUSE_LIKE_THRESHOLD,
            // `ignoreLocation: true` here, unlike the other two sites: a hit
            // anywhere in a transcript counts, so `location` / `distance` drop
            // out of the score entirely.
            ignore_location: true,
            // A single `searchableText` key, so there is nothing to weigh
            // against.
            keys: KeyStore::new(&[1.0]),
            ..FuseOptions::default()
        },
    )
    .search_in(text)
}

fn sort_deep_search_matches(matches: &mut [DeepSearchMatch]) {
    // CC `LogSelector.tsx:439-447` — the `> DATE_TIE_THRESHOLD_MS` gate is not
    // a total order (a≈b, b≈c but a̸≈c across the window). Kept verbatim, so
    // the sort goes through the non-validating JS-sort primitive; `sort_by`
    // panics on comparators like this (see `utils/js_sort.rs`; mirror PR #7).
    crate::utils::js_sort::sort_by(matches, |a, b| {
        let date_diff_ms = if a.modified >= b.modified {
            a.modified
                .duration_since(b.modified)
                .map(|duration| duration.as_millis())
                .unwrap_or(0)
        } else {
            b.modified
                .duration_since(a.modified)
                .map(|duration| duration.as_millis())
                .unwrap_or(0)
        };

        if date_diff_ms > DATE_TIE_THRESHOLD_MS {
            return b.modified.cmp(&a.modified);
        }

        a.score
            .partial_cmp(&b.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| b.modified.cmp(&a.modified))
    });
}

fn deep_search_matches_for_logs(
    logs: &[SessionSummary],
    query_lower: &str,
) -> Vec<DeepSearchMatch> {
    if query_lower.trim().is_empty() {
        return Vec::new();
    }

    let mut matches = logs
        .iter()
        .filter_map(|log| {
            let text = build_deep_search_text(log);
            let score = fuse_like_score(&text, query_lower)?;
            Some(DeepSearchMatch {
                log_key: log_deep_search_key(log),
                score,
                modified: log.modified,
                snippet: extract_deep_search_snippet(&text, query_lower),
            })
        })
        .collect::<Vec<_>>();
    sort_deep_search_matches(&mut matches);
    matches
}

fn merge_title_and_deep_search_logs(
    logs: &[SessionSummary],
    query_lower: &str,
    deep_search_matches: &[DeepSearchMatch],
) -> Vec<SessionSummary> {
    if query_lower.trim().is_empty() {
        return logs.to_vec();
    }

    let mut title_matched_keys = HashSet::new();
    let mut merged = logs
        .iter()
        .filter(|log| log_matches_query(log, query_lower))
        .map(|log| {
            title_matched_keys.insert(log_deep_search_key(log));
            log.clone()
        })
        .collect::<Vec<_>>();

    for deep_match in deep_search_matches {
        if title_matched_keys.contains(&deep_match.log_key) {
            continue;
        }
        if let Some(log) = logs
            .iter()
            .find(|candidate| log_deep_search_key(candidate) == deep_match.log_key)
        {
            title_matched_keys.insert(deep_match.log_key.clone());
            merged.push(log.clone());
        }
    }

    merged
}

fn unique_tags(logs: &[SessionSummary]) -> Vec<String> {
    let mut tags = logs
        .iter()
        .filter_map(|log| log.tag.as_deref())
        .filter(|tag| !tag.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    tags.sort();
    tags.dedup();
    tags
}

fn tag_tabs_for_logs(logs: &[SessionSummary]) -> Vec<String> {
    let tags = unique_tags(logs);
    if tags.is_empty() {
        Vec::new()
    } else {
        std::iter::once(ALL_TAG_LABEL.to_string())
            .chain(tags)
            .collect()
    }
}

fn selected_tag<'a>(tag_tabs: &'a [String], selected_tag_index: usize) -> Option<&'a str> {
    tag_tabs
        .get(selected_tag_index)
        .and_then(|tag| (tag != ALL_TAG_LABEL).then_some(tag.as_str()))
}

fn log_matches_structured_filters(
    log: &SessionSummary,
    selected_tag: Option<&str>,
    branch_filter_enabled: bool,
    current_branch: Option<&str>,
    has_multiple_worktrees: bool,
    show_all_worktrees: bool,
    current_project_path: &str,
) -> bool {
    if let Some(tag) = selected_tag {
        if log.tag.as_deref() != Some(tag) {
            return false;
        }
    }

    if branch_filter_enabled {
        if let Some(branch) = current_branch {
            if log.git_branch.as_deref() != Some(branch) {
                return false;
            }
        }
    }

    if has_multiple_worktrees && !show_all_worktrees {
        if log.project_path.as_deref() != Some(current_project_path) {
            return false;
        }
    }

    true
}

fn clamp_focus(focused: usize, count: usize) -> usize {
    focused.min(count.saturating_sub(1))
}

fn clamp_visible_from(visible_from: usize, visible_count: usize, count: usize) -> usize {
    if count == 0 {
        0
    } else {
        visible_from.min(count.saturating_sub(visible_count.max(1)))
    }
}

fn visible_from_for_focus(
    focused: usize,
    visible_from: usize,
    visible_count: usize,
    count: usize,
) -> usize {
    if count == 0 {
        return 0;
    }
    let visible_count = visible_count.max(1);
    let mut next = clamp_visible_from(visible_from, visible_count, count);
    if focused < next {
        next = focused;
    } else if focused >= next + visible_count {
        next = focused.saturating_sub(visible_count).saturating_add(1);
    }
    clamp_visible_from(next, visible_count, count)
}

fn is_plain_printable_search_char(c: char, modifiers: KeyModifiers) -> bool {
    !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) && !c.is_control()
}

fn is_empty_search_trigger(code: &KeyCode, modifiers: &KeyModifiers) -> bool {
    matches!(code, KeyCode::Char('/'))
        && !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
}

fn is_search_exit_shortcut(code: &KeyCode, modifiers: &KeyModifiers) -> bool {
    matches!(code, KeyCode::Char('n')) && modifiers.contains(KeyModifiers::CONTROL)
}

fn is_empty_search_backspace_exit_key(code: &KeyCode, modifiers: &KeyModifiers) -> bool {
    (matches!(code, KeyCode::Backspace) && !modifiers.contains(KeyModifiers::ALT))
        || (matches!(code, KeyCode::Char('h') | KeyCode::Char('H'))
            && modifiers.contains(KeyModifiers::CONTROL))
}

fn is_empty_search_ctrl_d_exit_key(code: &KeyCode, modifiers: &KeyModifiers) -> bool {
    matches!(code, KeyCode::Char('d') | KeyCode::Char('D'))
        && modifiers.contains(KeyModifiers::CONTROL)
}

fn is_search_ctrl_c_swallow_key(code: &KeyCode, modifiers: &KeyModifiers) -> bool {
    matches!(code, KeyCode::Char('c') | KeyCode::Char('C'))
        && modifiers.contains(KeyModifiers::CONTROL)
}

fn should_show_agentic_search_option(query: &str, notice_active: bool) -> bool {
    !query.trim().is_empty() && !notice_active
}

fn is_agentic_search_focus_key(code: &KeyCode) -> bool {
    matches!(code, KeyCode::Enter | KeyCode::Down)
}

fn search_footer_text() -> &'static str {
    // Official LogSelector keeps the footer action as `Enter to select` even
    // when Enter/Down first moves focus to the optional agentic-search row.
    "Type to Search · Enter to select · Esc to clear"
}

fn exit_double_press_key(code: &KeyCode, modifiers: &KeyModifiers) -> Option<&'static str> {
    if !modifiers.contains(KeyModifiers::CONTROL) {
        return None;
    }
    match code {
        KeyCode::Char('c') | KeyCode::Char('C') => Some("Ctrl-C"),
        KeyCode::Char('d') | KeyCode::Char('D') => Some("Ctrl-D"),
        _ => None,
    }
}

fn exit_pending_footer_text(key_name: &str) -> String {
    format!("Press {key_name} again to exit")
}

fn active_exit_pending_footer(
    last_key_name: Option<&'static str>,
    ctrl_c_pending: bool,
    ctrl_d_pending: bool,
) -> Option<String> {
    match last_key_name {
        Some("Ctrl-C") if ctrl_c_pending => Some(exit_pending_footer_text("Ctrl-C")),
        Some("Ctrl-D") if ctrl_d_pending => Some(exit_pending_footer_text("Ctrl-D")),
        _ if ctrl_c_pending => Some(exit_pending_footer_text("Ctrl-C")),
        _ if ctrl_d_pending => Some(exit_pending_footer_text("Ctrl-D")),
        _ => None,
    }
}

fn rename_footer_text() -> &'static str {
    "Enter to save · Esc to cancel"
}

fn agentic_search_option_footer_text() -> &'static str {
    "Enter to search · ↓ to skip · Esc to cancel"
}

fn log_selector_title_label() -> &'static str {
    "Resume Session"
}

fn focused_displayed_log_number(
    focused_row: Option<&LogDisplayRow>,
    displayed_logs: &[SessionSummary],
) -> usize {
    let Some(row) = focused_row else {
        return 0;
    };
    displayed_logs
        .iter()
        .position(|log| log.session_id == row.log.session_id)
        .map(|index| index + 1)
        .unwrap_or(0)
}

fn log_selector_title_count(
    view_mode: ViewMode,
    focused_row: Option<&LogDisplayRow>,
    displayed_logs: &[SessionSummary],
    visible_count: usize,
) -> String {
    if view_mode != ViewMode::List || displayed_logs.len() <= visible_count {
        return String::new();
    }
    let focused_number = focused_displayed_log_number(focused_row, displayed_logs).max(1);
    format!(" ({focused_number} of {})", displayed_logs.len())
}

fn list_footer_text(
    show_all_projects: bool,
    current_branch: bool,
    has_multiple_worktrees: bool,
    show_all_worktrees: bool,
    _has_tag_tabs: bool,
    expand_hint: Option<&str>,
) -> String {
    let mut parts = vec![if show_all_projects {
        "Ctrl+A to show current dir".to_string()
    } else {
        "Ctrl+A to show all projects".to_string()
    }];
    if current_branch {
        parts.push("Ctrl+B to toggle branch".to_string());
    }
    if has_multiple_worktrees {
        parts.push(if show_all_worktrees {
            "Ctrl+W to show current worktree".to_string()
        } else {
            "Ctrl+W to show all worktrees".to_string()
        });
    }
    parts.push("Ctrl+V to preview".to_string());
    parts.push("Ctrl+R to rename".to_string());
    parts.push("Type to search".to_string());
    parts.push("Esc to cancel".to_string());
    if let Some(hint) = expand_hint.filter(|hint| !hint.is_empty()) {
        parts.push(hint.to_string());
    }
    parts.join(" · ")
}

#[component]
pub fn LogSelector<'a>(
    props: &mut LogSelectorProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let mut focused_index = hooks.use_state(|| 0usize);
    let mut visible_from_index = hooks.use_state(|| 0usize);
    let mut view_mode = hooks.use_state(|| ViewMode::List);
    let search = use_search_input(
        &mut hooks,
        props.initial_search_query.as_deref().unwrap_or_default(),
    );
    let rename_input = use_search_input(&mut hooks, "");
    let mut pending_cancel = hooks.use_state(|| false);
    let exit_ctrl_c = use_double_press(&mut hooks);
    let exit_ctrl_d = use_double_press(&mut hooks);
    let mut exit_pending_key = hooks.use_state(|| Option::<&'static str>::None);
    let mut pending_select = hooks.use_state(|| Option::<usize>::None);
    let mut pending_selection = hooks.use_state(|| Option::<SessionSelection>::None);
    let mut preview_log = hooks.use_state(|| Option::<SessionSummary>::None);
    let mut pending_toggle_all_projects = hooks.use_state(|| false);
    let mut selected_tag_index = hooks.use_state(|| 0usize);
    let mut branch_filter_enabled = hooks.use_state(|| false);
    let mut show_all_worktrees = hooks.use_state(|| false);
    let mut expanded_group_session_ids = hooks.use_state(Vec::<String>::new);
    let mut custom_title_overrides = hooks.use_state(Vec::<(String, String)>::new);
    let mut pending_rename_save =
        hooks.use_state(|| Option::<(String, String, std::path::PathBuf)>::None);
    let mut is_agentic_search_option_focused = hooks.use_state(|| false);
    let mut agentic_search_notice = hooks.use_state(|| Option::<String>::None);
    let mut load_more_signature = hooks.use_state(|| Option::<(u64, usize, usize)>::None);
    let current_project_path = hooks.use_const(|| {
        crate::bootstrap::state::get_original_cwd()
            .display()
            .to_string()
    });
    // Maps to: CC LogSelector.tsx:203,206,277-282 — `currentBranch` starts
    // null and `hasMultipleWorktrees` false; a mount effect fills them from two
    // independent `getBranch().then(...)` / `getWorktreePaths(currentCwd)
    // .then(...)` promises. Both Rust projections spawn git synchronously, so
    // each runs on its own worker thread and lands through `use_future` on the
    // render thread (Contract C: no cross-thread `State::set`). A sync call
    // inside the async block would still block rendering, since `use_future`
    // is polled in the render loop. Calling them from the render body froze
    // the picker for the whole git round-trip on mount.
    let mut current_branch_state = hooks.use_state(|| Option::<String>::None);
    let mut has_multiple_worktrees_state = hooks.use_state(|| false);
    let branch_receiver = hooks.use_const(|| {
        let (sender, receiver) = async_channel::bounded(1);
        let _ = std::thread::Builder::new()
            .name("log-selector-branch".to_string())
            .spawn(move || {
                let branch = crate::utils::git::get_branch();
                let _ = sender.send_blocking((!branch.is_empty()).then_some(branch));
            });
        std::sync::Arc::new(receiver)
    });
    hooks.use_future(async move {
        if let Ok(branch) = branch_receiver.recv().await {
            current_branch_state.set(branch);
        }
    });
    let worktrees_receiver = hooks.use_const({
        let current_project_path = current_project_path.clone();
        move || {
            let (sender, receiver) = async_channel::bounded(1);
            let _ = std::thread::Builder::new()
                .name("log-selector-worktrees".to_string())
                .spawn(move || {
                    let paths =
                        crate::utils::get_worktree_paths::get_worktree_paths(&current_project_path);
                    let _ = sender.send_blocking(paths.len() > 1);
                });
            std::sync::Arc::new(receiver)
        }
    });
    hooks.use_future(async move {
        if let Ok(multiple) = worktrees_receiver.recv().await {
            has_multiple_worktrees_state.set(multiple);
        }
    });
    let current_branch = current_branch_state.read().clone();
    let has_multiple_worktrees = has_multiple_worktrees_state.get();

    let (terminal_width, terminal_rows) = hooks.use_terminal_size();
    let theme = hooks.use_context::<Theme>();
    // No hooks may move below the preview/list conditional return.
    let columns = props.force_width.unwrap_or(terminal_width as usize);
    let max_height = if props.max_height == 0 {
        terminal_rows.saturating_sub(2).max(1) as usize
    } else {
        props.max_height
    };
    let tag_tabs = tag_tabs_for_logs(&props.logs);
    if selected_tag_index.get() >= tag_tabs.len().max(1) {
        selected_tag_index.set(0);
    }
    let selected_tag = selected_tag(&tag_tabs, selected_tag_index.get());
    let filter_indicators = {
        let mut indicators = Vec::new();
        if branch_filter_enabled.get() {
            if let Some(branch) = current_branch.as_deref() {
                indicators.push(branch.to_string());
            }
        }
        if has_multiple_worktrees && !show_all_worktrees.get() {
            indicators.push("current worktree".to_string());
        }
        indicators
    };

    // Maps to CC LogSelector.tsx:
    // searchBoxLines=3, headerLines=5+searchBoxLines(+filters+tagTabs), footerLines=2.
    let search_box_lines = 3usize;
    let tag_tabs_lines = usize::from(!tag_tabs.is_empty());
    let show_additional_filter_line =
        !filter_indicators.is_empty() && view_mode.get() != ViewMode::Search;
    let header_lines =
        5usize + search_box_lines + tag_tabs_lines + usize::from(show_additional_filter_line);
    let footer_lines = 2usize;
    let visible_count = ((max_height).saturating_sub(header_lines + footer_lines) / 3).max(1);

    let query = search.text();
    let query_lower = query.to_lowercase();
    let title_overrides = custom_title_overrides.read().clone();
    let logs_with_title_overrides =
        apply_custom_title_overrides(props.logs.clone(), &title_overrides);
    let structurally_filtered_logs: Vec<SessionSummary> = logs_with_title_overrides
        .into_iter()
        .filter(|log| {
            log_matches_structured_filters(
                log,
                selected_tag,
                branch_filter_enabled.get(),
                current_branch.as_deref(),
                has_multiple_worktrees,
                show_all_worktrees.get(),
                &current_project_path,
            )
        })
        .collect();
    let deep_search_matches =
        deep_search_matches_for_logs(&structurally_filtered_logs, &query_lower);
    let displayed_logs: Vec<SessionSummary> = merge_title_and_deep_search_logs(
        &structurally_filtered_logs,
        &query_lower,
        &deep_search_matches,
    );
    let max_label_width = columns.saturating_sub(4).max(30);
    let expand_all_groups = !query_lower.is_empty() || branch_filter_enabled.get();
    let expanded_groups = expanded_group_session_ids.read().clone();
    let displayed_rows = group_log_rows(
        &displayed_logs,
        max_label_width,
        props.show_all_projects,
        &expanded_groups,
        expand_all_groups,
        &deep_search_matches,
    );
    let displayed_len = displayed_rows.len();
    let focused = clamp_focus(focused_index.get(), displayed_len);
    if focused != focused_index.get() {
        focused_index.set(focused);
    }
    let visible_from = visible_from_for_focus(
        focused,
        visible_from_index.get(),
        visible_count,
        displayed_len,
    );
    if visible_from != visible_from_index.get() {
        visible_from_index.set(visible_from);
    }

    let has_load_more = !props.on_load_more.is_default();
    let load_more_count = visible_count.saturating_mul(3);
    let should_load_more = has_load_more
        && focused.saturating_add(visible_count.saturating_mul(2)) >= displayed_logs.len();
    let current_load_more_signature = (props.reload_generation, focused, displayed_logs.len());

    hooks.use_propagated_terminal_events({
        let displayed_len = displayed_len;
        let display_rows = displayed_rows.clone();
        let visible_count = visible_count;
        let tag_tabs_len = tag_tabs.len();
        let has_current_branch = current_branch.is_some();
        let has_multiple_worktrees = has_multiple_worktrees;
        move |event| match event.event() {
            // CC LogSelector.tsx:231-250 → useSearchInput InputEvent bridge.
            TerminalEvent::Paste(text) if view_mode.get() == ViewMode::Search => {
                let mut search = search;
                search.reset_key_state(&KeyCode::Char(' '), &KeyModifiers::empty());
                search.insert_text(text);
                agentic_search_notice.set(None);
                is_agentic_search_option_focused.set(false);
                focused_index.set(0);
                visible_from_index.set(0);
                event.stop_propagation();
            }
            TerminalEvent::Key(KeyEvent {
                code,
                kind,
                modifiers,
                ..
            }) if *kind != KeyEventKind::Release => {
                if view_mode.get() == ViewMode::Preview {
                    return;
                }

                if view_mode.get() == ViewMode::Rename {
                    let mut rename_input = rename_input;
                    match code {
                        KeyCode::Esc => {
                            rename_input.clear();
                            view_mode.set(ViewMode::List);
                            event.stop_propagation();
                            return;
                        }
                        KeyCode::Enter => {
                            let idx = clamp_focus(focused_index.get(), displayed_len);
                            if let Some(row) = display_rows.get(idx) {
                                let title = rename_input.text().trim().to_string();
                                if !title.is_empty() {
                                    let mut overrides = custom_title_overrides.read().clone();
                                    upsert_custom_title_override(
                                        &mut overrides,
                                        &row.group_session_id,
                                        title,
                                    );
                                    custom_title_overrides.set(overrides);
                                    pending_rename_save.set(Some((
                                        row.group_session_id.clone(),
                                        rename_input.text().trim().to_string(),
                                        row.log.file_path.clone(),
                                    )));
                                }
                            }
                            rename_input.clear();
                            view_mode.set(ViewMode::List);
                            event.stop_propagation();
                            return;
                        }
                        _ => {
                            if rename_input.handle_edit_key(code, modifiers) {
                                event.stop_propagation();
                            }
                            return;
                        }
                    }
                }

                if view_mode.get() == ViewMode::Search {
                    let mut search = search;
                    // Source passthroughCtrlKeys=['n'] precedes ring reset.
                    if !is_search_exit_shortcut(code, modifiers) {
                        search.reset_key_state(code, modifiers);
                    }
                    match code {
                        KeyCode::Esc => {
                            if !search.is_empty() {
                                search.clear();
                                agentic_search_notice.set(None);
                                is_agentic_search_option_focused.set(false);
                                focused_index.set(0);
                                visible_from_index.set(0);
                            } else {
                                view_mode.set(ViewMode::List);
                            }
                            event.stop_propagation();
                            return;
                        }
                        _ if is_search_exit_shortcut(code, modifiers) => {
                            view_mode.set(ViewMode::List);
                            is_agentic_search_option_focused.set(false);
                            event.stop_propagation();
                            return;
                        }
                        _ if search.is_empty()
                            && is_empty_search_backspace_exit_key(code, modifiers) =>
                        {
                            view_mode.set(ViewMode::List);
                            is_agentic_search_option_focused.set(false);
                            event.stop_propagation();
                            return;
                        }
                        _ if search.is_empty()
                            && is_empty_search_ctrl_d_exit_key(code, modifiers) =>
                        {
                            view_mode.set(ViewMode::List);
                            is_agentic_search_option_focused.set(false);
                            event.stop_propagation();
                            return;
                        }
                        _ if is_search_ctrl_c_swallow_key(code, modifiers) => {
                            event.stop_propagation();
                            return;
                        }
                        _ if is_agentic_search_focus_key(code) => {
                            if should_show_agentic_search_option(
                                &search.text(),
                                agentic_search_notice.read().is_some(),
                            ) {
                                view_mode.set(ViewMode::List);
                                is_agentic_search_option_focused.set(true);
                            } else {
                                view_mode.set(ViewMode::List);
                                focused_index.set(clamp_focus(focused_index.get(), displayed_len));
                                visible_from_index.set(visible_from_for_focus(
                                    focused_index.get(),
                                    visible_from_index.get(),
                                    visible_count,
                                    displayed_len,
                                ));
                            }
                            event.stop_propagation();
                            return;
                        }
                        KeyCode::Up => {
                            view_mode.set(ViewMode::List);
                            event.stop_propagation();
                            return;
                        }
                        _ => {
                            let before = search.text();
                            if search.handle_edit_key(code, modifiers) {
                                if search.text() != before {
                                    agentic_search_notice.set(None);
                                    is_agentic_search_option_focused.set(false);
                                    focused_index.set(0);
                                    visible_from_index.set(0);
                                }
                                event.stop_propagation();
                            }
                            return;
                        }
                    }
                }

                if let Some(key_name) = exit_double_press_key(code, modifiers) {
                    exit_pending_key.set(Some(key_name));
                    match key_name {
                        "Ctrl-C" => {
                            exit_ctrl_c.press();
                            if exit_ctrl_c.take_triggered() {
                                exit_pending_key.set(None);
                                pending_cancel.set(true);
                            }
                        }
                        "Ctrl-D" => {
                            exit_ctrl_d.press();
                            if exit_ctrl_d.take_triggered() {
                                exit_pending_key.set(None);
                                pending_cancel.set(true);
                            }
                        }
                        _ => {}
                    }
                    event.stop_propagation();
                    return;
                }

                if is_agentic_search_option_focused.get() {
                    match code {
                        KeyCode::Enter => {
                            agentic_search_notice
                                .set(Some(AGENTIC_SEARCH_READONLY_NOTICE.to_string()));
                            is_agentic_search_option_focused.set(false);
                            event.stop_propagation();
                            return;
                        }
                        KeyCode::Down => {
                            is_agentic_search_option_focused.set(false);
                            event.stop_propagation();
                            return;
                        }
                        KeyCode::Up => {
                            is_agentic_search_option_focused.set(false);
                            view_mode.set(ViewMode::Search);
                            event.stop_propagation();
                            return;
                        }
                        KeyCode::Esc => {
                            pending_cancel.set(true);
                            event.stop_propagation();
                            return;
                        }
                        _ => {}
                    }
                }

                match code {
                    KeyCode::Esc => {
                        pending_cancel.set(true);
                        event.stop_propagation();
                    }
                    KeyCode::Enter => {
                        let idx = clamp_focus(focused_index.get(), displayed_len);
                        if idx < displayed_len {
                            pending_select.set(Some(idx));
                        }
                        event.stop_propagation();
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        if displayed_len == 0 || focused_index.get() == 0 {
                            view_mode.set(ViewMode::Search);
                        } else {
                            let next = focused_index.get().saturating_sub(1);
                            focused_index.set(next);
                            visible_from_index.set(visible_from_for_focus(
                                next,
                                visible_from_index.get(),
                                visible_count,
                                displayed_len,
                            ));
                        }
                        event.stop_propagation();
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if displayed_len > 0 {
                            let next = if focused_index.get() + 1 < displayed_len {
                                focused_index.get() + 1
                            } else {
                                0
                            };
                            focused_index.set(next);
                            visible_from_index.set(visible_from_for_focus(
                                next,
                                visible_from_index.get(),
                                visible_count,
                                displayed_len,
                            ));
                        }
                        event.stop_propagation();
                    }
                    KeyCode::PageDown => {
                        if displayed_len > 0 {
                            let next = (focused_index.get() + visible_count)
                                .min(displayed_len.saturating_sub(1));
                            focused_index.set(next);
                            visible_from_index.set(visible_from_for_focus(
                                next,
                                visible_from_index.get(),
                                visible_count,
                                displayed_len,
                            ));
                        }
                        event.stop_propagation();
                    }
                    KeyCode::PageUp => {
                        if displayed_len > 0 {
                            let next = focused_index.get().saturating_sub(visible_count);
                            focused_index.set(next);
                            visible_from_index.set(visible_from_for_focus(
                                next,
                                visible_from_index.get(),
                                visible_count,
                                displayed_len,
                            ));
                        }
                        event.stop_propagation();
                    }
                    KeyCode::Home => {
                        focused_index.set(0);
                        visible_from_index.set(0);
                        event.stop_propagation();
                    }
                    KeyCode::End => {
                        if displayed_len > 0 {
                            let next = displayed_len - 1;
                            focused_index.set(next);
                            visible_from_index.set(visible_from_for_focus(
                                next,
                                visible_from_index.get(),
                                visible_count,
                                displayed_len,
                            ));
                        }
                        event.stop_propagation();
                    }
                    KeyCode::Right => {
                        let idx = clamp_focus(focused_index.get(), displayed_len);
                        if let Some(row) = display_rows.get(idx) {
                            if row.is_group_parent && row.has_children {
                                let mut expanded = expanded_group_session_ids.read().clone();
                                if !is_group_expanded(&row.group_session_id, &expanded) {
                                    expanded.push(row.group_session_id.clone());
                                    expanded_group_session_ids.set(expanded);
                                }
                                event.stop_propagation();
                            }
                        }
                    }
                    KeyCode::Left => {
                        let idx = clamp_focus(focused_index.get(), displayed_len);
                        if let Some(row) = display_rows.get(idx) {
                            if row.has_children {
                                let mut expanded = expanded_group_session_ids.read().clone();
                                expanded.retain(|id| id != &row.group_session_id);
                                expanded_group_session_ids.set(expanded);
                                if row.is_child {
                                    if let Some(parent_idx) =
                                        display_rows.iter().position(|candidate| {
                                            candidate.is_group_parent
                                                && candidate.group_session_id
                                                    == row.group_session_id
                                        })
                                    {
                                        focused_index.set(parent_idx);
                                        visible_from_index.set(visible_from_for_focus(
                                            parent_idx,
                                            visible_from_index.get(),
                                            visible_count,
                                            displayed_len,
                                        ));
                                    }
                                }
                                event.stop_propagation();
                            }
                        }
                    }
                    KeyCode::Tab if tag_tabs_len > 0 => {
                        selected_tag_index.set((selected_tag_index.get() + 1) % tag_tabs_len);
                        focused_index.set(0);
                        visible_from_index.set(0);
                        event.stop_propagation();
                    }
                    KeyCode::BackTab if tag_tabs_len > 0 => {
                        selected_tag_index
                            .set((selected_tag_index.get() + tag_tabs_len - 1) % tag_tabs_len);
                        focused_index.set(0);
                        visible_from_index.set(0);
                        event.stop_propagation();
                    }
                    KeyCode::Char('a') if modifiers.contains(KeyModifiers::CONTROL) => {
                        pending_toggle_all_projects.set(true);
                        focused_index.set(0);
                        visible_from_index.set(0);
                        event.stop_propagation();
                    }
                    KeyCode::Char('b')
                        if modifiers.contains(KeyModifiers::CONTROL) && has_current_branch =>
                    {
                        branch_filter_enabled.set(!branch_filter_enabled.get());
                        focused_index.set(0);
                        visible_from_index.set(0);
                        event.stop_propagation();
                    }
                    KeyCode::Char('w')
                        if modifiers.contains(KeyModifiers::CONTROL) && has_multiple_worktrees =>
                    {
                        show_all_worktrees.set(!show_all_worktrees.get());
                        focused_index.set(0);
                        visible_from_index.set(0);
                        event.stop_propagation();
                    }
                    KeyCode::Char('r') if modifiers.contains(KeyModifiers::CONTROL) => {
                        if displayed_len > 0 {
                            let mut rename_input = rename_input;
                            rename_input.clear();
                            view_mode.set(ViewMode::Rename);
                        }
                        event.stop_propagation();
                    }
                    KeyCode::Char('v') if modifiers.contains(KeyModifiers::CONTROL) => {
                        let idx = clamp_focus(focused_index.get(), displayed_len);
                        if let Some(row) = display_rows.get(idx) {
                            preview_log.set(Some(row.log.clone()));
                            view_mode.set(ViewMode::Preview);
                        }
                        event.stop_propagation();
                    }
                    _ if is_empty_search_trigger(code, modifiers) => {
                        agentic_search_notice.set(None);
                        is_agentic_search_option_focused.set(false);
                        view_mode.set(ViewMode::Search);
                        event.stop_propagation();
                    }
                    KeyCode::Char(c) if is_plain_printable_search_char(*c, *modifiers) => {
                        let mut search = search;
                        search.set(c.to_string());
                        agentic_search_notice.set(None);
                        is_agentic_search_option_focused.set(false);
                        view_mode.set(ViewMode::Search);
                        focused_index.set(0);
                        visible_from_index.set(0);
                        event.stop_propagation();
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    });

    if should_load_more && load_more_signature.get() != Some(current_load_more_signature) {
        load_more_signature.set(Some(current_load_more_signature));
        (props.on_load_more)(load_more_count);
    } else if !should_load_more && load_more_signature.get().is_some() {
        load_more_signature.set(None);
    }

    let pending_rename_value = { pending_rename_save.read().clone() };
    if let Some((session_id, title, full_path)) = pending_rename_value {
        pending_rename_save.set(None);
        match crate::utils::session_storage::save_custom_title(
            &session_id,
            &title,
            Some(&full_path),
        ) {
            Ok(()) => (props.on_logs_changed)(()),
            Err(error) => crate::utils::debug::log_for_debugging(&format!(
                "Failed to rename resumed session {session_id}: {error}"
            )),
        }
    }

    if pending_cancel.get() {
        pending_cancel.set(false);
        (props.on_cancel)(());
    }
    if pending_toggle_all_projects.get() {
        pending_toggle_all_projects.set(false);
        (props.on_toggle_all_projects)(());
    }
    let pending_select_value = pending_select.get();
    if let Some(index) = pending_select_value {
        pending_select.set(None);
        if let Some(row) = displayed_rows.get(index) {
            (props.on_select)(SessionSelection::from(&row.log));
        }
    }
    let pending_selection_value = { pending_selection.read().clone() };
    if let Some(selection) = pending_selection_value {
        pending_selection.set(None);
        (props.on_select)(selection);
    }

    if view_mode.get() == ViewMode::Preview {
        let preview = { preview_log.read().clone() };
        if let Some(log) = preview {
            return element! {
                SessionPreview(
                    log: log,
                    max_height: max_height,
                    on_exit: move |_| {
                        preview_log.set(None);
                        view_mode.set(ViewMode::List);
                    },
                    on_select: move |selection: SessionSelection| {
                        pending_selection.set(Some(selection));
                    },
                )
            }
            .into_any();
        }
        view_mode.set(ViewMode::List);
    }

    let options: Vec<SelectOptionData> = displayed_rows
        .iter()
        .map(|row| SelectOptionData {
            label: row.label.clone(),
            description: row.description.clone(),
            dim_description: true,
            value: row.value.clone(),
            disabled: false,
            input: None,
        })
        .collect();

    let title_count = log_selector_title_count(
        view_mode.get(),
        displayed_rows.get(focused),
        &displayed_logs,
        visible_count,
    );
    let root_height = max_height.saturating_sub(1).max(1) as u32;
    let is_search = view_mode.get() == ViewMode::Search;
    let is_rename = view_mode.get() == ViewMode::Rename;
    let agentic_notice = agentic_search_notice.read().clone();
    let agentic_option_focused = is_agentic_search_option_focused.get();
    let show_agentic_option =
        should_show_agentic_search_option(&query, agentic_notice.is_some()) && !is_rename;
    let rename_value = rename_input.text();
    let rename_placeholder = displayed_rows
        .get(focused)
        .map(|row| raw_log_title(&row.log))
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| "Enter new session name".to_string());
    let title_label = log_selector_title_label();
    let mut footer_text = if let Some(text) = active_exit_pending_footer(
        exit_pending_key.get(),
        exit_ctrl_c.is_pending(),
        exit_ctrl_d.is_pending(),
    ) {
        text
    } else if is_rename {
        rename_footer_text().to_string()
    } else if agentic_option_focused {
        agentic_search_option_footer_text().to_string()
    } else if is_search {
        search_footer_text().to_string()
    } else {
        let expand_hint = (!expand_all_groups)
            .then(|| expand_collapse_hint(displayed_rows.get(focused), &expanded_groups))
            .flatten();
        list_footer_text(
            props.show_all_projects,
            current_branch.is_some(),
            has_multiple_worktrees,
            show_all_worktrees.get(),
            !tag_tabs.is_empty(),
            expand_hint,
        )
    };
    if props.is_loading && !is_search && !is_rename {
        footer_text = format!("Loading conversations… · {footer_text}");
    }

    element! {
        View(flex_direction: FlexDirection::Column, height: root_height, overflow: Overflow::Hidden) {
            View(flex_shrink: 0.0f32) {
                Divider(color: theme.suggestion)
            }
            View(flex_shrink: 0.0f32) {
                Text(content: " ", wrap: TextWrap::NoWrap)
            }
            #(if !tag_tabs.is_empty() {
                Some(element! {
                    TagTabs(
                        tabs: tag_tabs.clone(),
                        selected_index: selected_tag_index.get(),
                        available_width: columns,
                        show_all_projects: props.show_all_projects,
                    )
                }.into_any())
            } else {
                Some(element! {
                    View(flex_shrink: 0.0f32) {
                        View(flex_direction: FlexDirection::Row) {
                            Text(content: title_label, color: theme.suggestion, weight: Weight::Bold, wrap: TextWrap::NoWrap)
                            Text(content: title_count, color: theme.inactive, wrap: TextWrap::NoWrap)
                        }
                    }
                }.into_any())
            })
            SearchBox(
                query: query.clone(),
                is_focused: is_search,
                is_terminal_focused: true,
                cursor_offset: Some(search.offset()),
            )
            #(if show_additional_filter_line {
                Some(element! {
                    View(padding_left: 2u32, flex_shrink: 0.0f32) {
                        Text(content: filter_indicators.join(" · "), color: theme.inactive, wrap: TextWrap::NoWrap)
                    }
                })
            } else {
                None
            })
            View(flex_shrink: 0.0f32) {
                Text(content: " ", wrap: TextWrap::NoWrap)
            }

            #(if show_agentic_option {
                let pointer = if agentic_option_focused { "❯" } else { " " };
                let option_color = if agentic_option_focused { theme.suggestion } else { theme.text };
                Some(element! {
                    View(flex_shrink: 0.0f32, flex_direction: FlexDirection::Column) {
                        View(flex_direction: FlexDirection::Row) {
                            Text(content: pointer, color: option_color, wrap: TextWrap::NoWrap)
                            Text(content: " ", wrap: TextWrap::NoWrap)
                            Text(content: AGENTIC_SEARCH_OPTION_LABEL, color: option_color, weight: if agentic_option_focused { Weight::Bold } else { Weight::Normal }, wrap: TextWrap::NoWrap)
                        }
                        View(height: 1u32) {}
                    }
                }.into_any())
            } else {
                None
            })

            #(if let Some(notice) = agentic_notice.clone() {
                Some(element! {
                    View(padding_left: 1u32, margin_bottom: 1u32, flex_shrink: 0.0f32) {
                        Text(content: notice, color: theme.inactive, italic: true, wrap: TextWrap::NoWrap)
                    }
                }.into_any())
            } else {
                None
            })

            #(if !is_rename && options.is_empty() {
                let text = if props.logs.is_empty() {
                    "No conversations found to resume"
                } else {
                    "No matching sessions found."
                };
                Some(element! {
                    View(padding_left: 1u32, flex_shrink: 0.0f32) {
                        Text(content: text, color: theme.inactive, italic: true, wrap: TextWrap::NoWrap)
                    }
                }.into_any())
            } else {
                None
            })

            #(if is_rename {
                let input_line = if rename_value.is_empty() {
                    rename_placeholder.clone()
                } else {
                    rename_value.clone()
                };
                let input_color = if rename_value.is_empty() { theme.inactive } else { theme.text };
                Some(element! {
                    View(padding_left: 2u32, flex_direction: FlexDirection::Column, flex_shrink: 0.0f32) {
                        Text(content: "Rename session:", weight: Weight::Bold, wrap: TextWrap::NoWrap)
                        View(padding_top: 1u32) {
                            Text(content: input_line, color: input_color, wrap: TextWrap::NoWrap)
                        }
                    }
                }.into_any())
            } else {
                Some(element! {
                    Select(
                        options: options,
                        focused_index: focused,
                        visible_from_index: visible_from,
                        visible_option_count: visible_count,
                        layout: SelectLayout::Expanded,
                        is_disabled: is_search || agentic_option_focused,
                    )
                }.into_any())
            })

            View(padding_left: 2u32, flex_shrink: 0.0f32) {
                Text(
                    content: footer_text,
                    color: theme.inactive,
                    wrap: TextWrap::NoWrap,
                )
            }
        }
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{StreamExt, stream};
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, SystemTime};

    fn summary(
        id: &str,
        tag: Option<&str>,
        branch: Option<&str>,
        project_path: &str,
    ) -> SessionSummary {
        SessionSummary {
            session_id: id.to_string(),
            display: format!("prompt {id}"),
            custom_title: None,
            summary: None,
            tag: tag.map(str::to_string),
            agent_name: None,
            agent_setting: None,
            git_branch: branch.map(str::to_string),
            project_path: Some(project_path.to_string()),
            file_path: PathBuf::from(format!("/{id}.jsonl")),
            is_sidechain: false,
            team_name: None,
            pr_number: None,
            pr_url: None,
            pr_repository: None,
            file_size: 1,
            modified: SystemTime::UNIX_EPOCH,
        }
    }

    fn with_modified(mut summary: SessionSummary, seconds: u64) -> SessionSummary {
        summary.modified = SystemTime::UNIX_EPOCH + Duration::from_secs(seconds);
        summary
    }

    #[test]
    fn progressive_load_more_triggers_once_near_end_and_shows_loading_footer() {
        let requested = Arc::new(AtomicUsize::new(0));
        let call_count = Arc::new(AtomicUsize::new(0));
        let requested_for_handler = requested.clone();
        let count_for_handler = call_count.clone();
        let logs = (0..5)
            .map(|index| summary(&format!("progressive-{index}"), None, None, "/repo"))
            .collect::<Vec<_>>();
        let current_theme = *crate::utils::theme::current();

        let frames = futures::executor::block_on(async move {
            let event_stream = stream::pending::<TerminalEvent>();
            let mut app = element! {
                ContextProvider(value: Context::owned(current_theme)) {
                    LogSelector(
                        logs: logs,
                        max_height: 20usize,
                        force_width: Some(80usize),
                        on_cancel: move |_| {},
                        on_select: move |_| {},
                        show_all_projects: false,
                        is_loading: true,
                        on_load_more: move |count: usize| {
                            count_for_handler.fetch_add(1, AtomicOrdering::SeqCst);
                            requested_for_handler.store(count, AtomicOrdering::SeqCst);
                        },
                        on_toggle_all_projects: move |_| {},
                    )
                }
            };
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(event_stream).with_size(80, 24),
            ));
            let mut frames = Vec::new();
            for _ in 0..2 {
                if let Some(canvas) = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_secs(1)).await;
                    None
                })
                .await
                {
                    frames.push(canvas_text(&canvas));
                }
            }
            frames
        });

        assert_eq!(call_count.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(requested.load(AtomicOrdering::SeqCst), 9);
        assert!(
            frames
                .iter()
                .any(|frame| frame.contains("Loading conversations…")),
            "loading state must be visible in the selector footer; frames={frames:#?}"
        );
    }

    #[test]
    fn initial_search_query_filters_first_frame_like_official_log_selector() {
        let mut matching = summary("matching", None, Some("main"), "/repo");
        matching.display = "needle conversation".to_string();
        let mut hidden = summary("hidden", None, Some("main"), "/repo");
        hidden.display = "unrelated conversation".to_string();
        let current_theme = *crate::utils::theme::current();

        let canvas = futures::executor::block_on(async move {
            let event_stream = stream::pending::<TerminalEvent>();
            let mut app = element! {
                ContextProvider(value: Context::owned(current_theme)) {
                    LogSelector(
                        logs: vec![matching, hidden],
                        max_height: 20usize,
                        force_width: Some(80usize),
                        on_cancel: move |_| {},
                        on_select: move |_| {},
                        initial_search_query: Some("needle".to_string()),
                        show_all_projects: false,
                        on_toggle_all_projects: move |_| {},
                    )
                }
            };
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(event_stream).with_size(80, 24),
            ));
            crate::utils::race(render_loop.next(), async {
                futures_timer::Delay::new(Duration::from_secs(1)).await;
                None
            })
            .await
            .expect("LogSelector should render its first frame")
        });
        let text = canvas_text(&canvas);

        assert!(text.contains("needle conversation"), "canvas=\n{text}");
        assert!(
            !text.contains("unrelated conversation"),
            "initialSearchQuery must filter before input events; canvas=\n{text}"
        );
    }

    fn press(code: KeyCode) -> TerminalEvent {
        TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code))
    }

    fn ctrl_char(ch: char) -> TerminalEvent {
        let mut event = KeyEvent::new(KeyEventKind::Press, KeyCode::Char(ch));
        event.modifiers = KeyModifiers::CONTROL;
        TerminalEvent::Key(event)
    }

    fn canvas_text(canvas: &Canvas) -> String {
        (0..canvas.height())
            .map(|y| {
                let mut line = String::new();
                for x in 0..canvas.width() {
                    if let Some(text) = canvas.cell(x, y).and_then(|cell| cell.text()) {
                        line.push_str(text);
                    } else {
                        line.push(' ');
                    }
                }
                line.trim_end().to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn display_tag_stripping_matches_official_backreferenced_regex_shape() {
        assert_eq!(
            strip_display_tags_allow_empty(
                "<ide_opened_file>/tmp/a.rs</ide_opened_file>\nResume this"
            ),
            "Resume this"
        );
        assert_eq!(
            strip_display_tags_allow_empty("<hook>one</hook>\n<tick>two</tick>\nReal title"),
            "Real title"
        );
        assert_eq!(strip_display_tags_allow_empty("<hook>one</hook>"), "");
        assert_eq!(strip_display_tags("<hook>one</hook>"), "<hook>one</hook>");
        assert_eq!(
            strip_display_tags_allow_empty("<Button>keep user prose</Button>"),
            "<Button>keep user prose</Button>"
        );
        assert_eq!(
            strip_display_tags_allow_empty("<ide_opened_file>keep mismatched</ide_selection>"),
            "<ide_opened_file>keep mismatched</ide_selection>"
        );
    }

    #[test]
    fn tag_tabs_are_sorted_and_prefixed_with_all() {
        let logs = vec![
            summary("a", Some("bug"), None, "/repo"),
            summary("b", Some("alpha"), None, "/repo"),
            summary("c", Some("bug"), None, "/repo"),
        ];

        assert_eq!(
            tag_tabs_for_logs(&logs),
            vec!["All".to_string(), "alpha".to_string(), "bug".to_string()]
        );
    }

    #[test]
    fn search_matches_branch_like_official_title_filter() {
        let log = summary("a", None, Some("feature/resume"), "/repo");
        assert!(log_matches_query(&log, "resume"));
    }

    #[test]
    fn deep_search_extracts_transcript_text_from_jsonl_like_official() {
        let raw = format!(
            "{}\n{}\n",
            json!({"type":"user","message":{"content":[{"type":"text","text":"needle in user text"}]}}),
            json!({"type":"assistant","message":{"content":"assistant followup"}})
        );

        let searchable = searchable_text_from_jsonl(&raw);

        assert!(searchable.contains("needle in user text"));
        assert!(searchable.contains("assistant followup"));
    }

    #[test]
    fn deep_search_snippet_description_uses_official_second_line_indentation() {
        let mut log = summary("snippet", None, None, "/repo");
        log.file_path = PathBuf::from("/tmp/cometix-snippet.jsonl");
        let matches = vec![DeepSearchMatch {
            log_key: log_deep_search_key(&log),
            score: 0.0,
            modified: log.modified,
            snippet: Some("…needle context…".to_string()),
        }];

        let parent = build_log_metadata_with_deep_snippet(&log, false, false, &matches);
        let child = build_log_metadata_with_deep_snippet(&log, false, true, &matches);

        assert!(parent.ends_with("\n  …needle context…"));
        assert!(!parent.contains(" · …needle context…"));
        assert!(child.starts_with("    "));
        assert!(child.ends_with("\n      …needle context…"));
    }

    #[test]
    fn deep_search_snippet_matches_transcript_only_text_without_writes() {
        let path = std::env::temp_dir().join(format!(
            "cometix-log-selector-deep-{}.jsonl",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(
            &path,
            format!(
                "{}\n",
                json!({"type":"user","message":{"content":[{"type":"text","text":"needle appears only in the transcript body"}]}})
            ),
        )
        .expect("fixture write should succeed");
        let mut log = summary("deep", None, None, "/repo");
        log.display = "ordinary title".to_string();
        log.file_path = path.clone();

        let matches = deep_search_matches_for_logs(&[log.clone()], "needle");
        let rows = group_log_rows(&[log.clone()], 80, false, &[], true, &matches);

        assert!(!log_matches_query(&log, "needle"));
        assert!(
            lookup_deep_search_snippet(&log, &matches)
                .expect("snippet should exist")
                .contains("needle appears")
        );
        assert!(
            rows[0]
                .description
                .as_deref()
                .expect("description should exist")
                .contains("needle appears")
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn deep_search_nucleo_query_includes_transcript_with_fuzzy_snippet() {
        let path = std::env::temp_dir().join(format!(
            "cometix-log-selector-fuzzy-{}.jsonl",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(
            &path,
            format!(
                "{}\n",
                json!({"type":"assistant","message":{"content":"instrumentation metrics only appear in the transcript"}})
            ),
        )
        .expect("fixture write should succeed");
        let mut log = summary("fuzzy", None, None, "/repo");
        log.display = "ordinary title".to_string();
        log.file_path = path.clone();

        let matches = deep_search_matches_for_logs(&[log.clone()], "instrmnt");
        let merged = merge_title_and_deep_search_logs(&[log.clone()], "instrmnt", &matches);

        assert!(!log_matches_query(&log, "instrmnt"));
        assert_eq!(matches.len(), 1);
        assert!(matches[0].score > 0.0);
        assert!(
            lookup_deep_search_snippet(&log, &matches)
                .expect("fuzzy snippet should exist")
                .contains("instrumentation metrics")
        );
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].session_id, "fuzzy");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn deep_search_ranking_uses_newest_unless_within_official_date_tie_window() {
        let mut matches = vec![
            DeepSearchMatch {
                log_key: "old-exact".to_string(),
                score: 0.0,
                modified: SystemTime::UNIX_EPOCH + Duration::from_secs(10),
                snippet: None,
            },
            DeepSearchMatch {
                log_key: "new-fuzzy".to_string(),
                score: 0.2,
                modified: SystemTime::UNIX_EPOCH + Duration::from_secs(120),
                snippet: None,
            },
        ];
        sort_deep_search_matches(&mut matches);
        assert_eq!(matches[0].log_key, "new-fuzzy");

        let mut same_minute = vec![
            DeepSearchMatch {
                log_key: "slightly-newer-fuzzy".to_string(),
                score: 0.2,
                modified: SystemTime::UNIX_EPOCH + Duration::from_secs(40),
                snippet: None,
            },
            DeepSearchMatch {
                log_key: "older-exact".to_string(),
                score: 0.0,
                modified: SystemTime::UNIX_EPOCH + Duration::from_secs(10),
                snippet: None,
            },
        ];
        sort_deep_search_matches(&mut same_minute);
        assert_eq!(same_minute[0].log_key, "older-exact");
    }

    /// Regression for the release SIGABRT (mirror PR #7): the verbatim
    /// `> DATE_TIE_THRESHOLD_MS` gate admits ordering cycles when scores rise
    /// while timestamps cross the window (x≈y, y≈z within 60s, x̸≈z beyond),
    /// which `slice::sort_by` rejects as not a total order. The sort routes
    /// through `js_sort::sort_by`; this test drives the former-cycle triple
    /// through it. The asserted order is the deterministic merge outcome of
    /// the OFFICIAL comparator: x/y are within the window (score decides,
    /// x first), z is over the window against both (newest first).
    #[test]
    fn deep_search_ranking_survives_a_cycle_across_the_date_tie_window() {
        let mut matches = vec![
            DeepSearchMatch {
                log_key: "x".to_string(),
                score: 0.0,
                modified: SystemTime::UNIX_EPOCH + Duration::from_secs(0),
                snippet: None,
            },
            DeepSearchMatch {
                log_key: "y".to_string(),
                score: 0.1,
                modified: SystemTime::UNIX_EPOCH + Duration::from_secs(50),
                snippet: None,
            },
            DeepSearchMatch {
                log_key: "z".to_string(),
                score: 0.2,
                modified: SystemTime::UNIX_EPOCH + Duration::from_secs(110),
                snippet: None,
            },
        ];
        sort_deep_search_matches(&mut matches);
        assert_eq!(
            matches
                .iter()
                .map(|entry| entry.log_key.as_str())
                .collect::<Vec<_>>(),
            ["z", "x", "y"]
        );
    }

    #[test]
    fn merge_title_and_deep_search_logs_appends_transcript_only_matches_by_rank() {
        let mut title = summary("title", None, None, "/repo");
        title.display = "needle title match".to_string();
        let transcript_old = with_modified(summary("old", None, None, "/repo"), 10);
        let transcript_new = with_modified(summary("new", None, None, "/repo"), 120);
        let logs = vec![
            title.clone(),
            transcript_old.clone(),
            transcript_new.clone(),
        ];
        let deep_matches = vec![
            DeepSearchMatch {
                log_key: log_deep_search_key(&transcript_new),
                score: 0.2,
                modified: transcript_new.modified,
                snippet: None,
            },
            DeepSearchMatch {
                log_key: log_deep_search_key(&transcript_old),
                score: 0.0,
                modified: transcript_old.modified,
                snippet: None,
            },
        ];

        let merged = merge_title_and_deep_search_logs(&logs, "needle", &deep_matches);

        assert_eq!(
            merged
                .iter()
                .map(|log| log.session_id.as_str())
                .collect::<Vec<_>>(),
            vec!["title", "new", "old"]
        );
    }

    #[test]
    fn slash_and_empty_backspace_search_exits_match_official_use_search_input() {
        assert!(is_empty_search_trigger(
            &KeyCode::Char('/'),
            &KeyModifiers::empty()
        ));
        assert!(!is_empty_search_trigger(
            &KeyCode::Char('/'),
            &KeyModifiers::CONTROL
        ));
        assert!(is_search_exit_shortcut(
            &KeyCode::Char('n'),
            &KeyModifiers::CONTROL
        ));
        assert!(!is_search_exit_shortcut(
            &KeyCode::Char('n'),
            &KeyModifiers::empty()
        ));
        assert!(is_empty_search_backspace_exit_key(
            &KeyCode::Backspace,
            &KeyModifiers::empty()
        ));
        assert!(is_empty_search_backspace_exit_key(
            &KeyCode::Char('h'),
            &KeyModifiers::CONTROL
        ));
        assert!(!is_empty_search_backspace_exit_key(
            &KeyCode::Char('h'),
            &KeyModifiers::empty()
        ));
        assert!(is_empty_search_ctrl_d_exit_key(
            &KeyCode::Char('d'),
            &KeyModifiers::CONTROL
        ));
        assert!(is_search_ctrl_c_swallow_key(
            &KeyCode::Char('c'),
            &KeyModifiers::CONTROL
        ));
    }

    #[test]
    fn agentic_search_option_is_ui_only_and_query_gated() {
        assert!(should_show_agentic_search_option("needle", false));
        assert!(should_show_agentic_search_option("  needle  ", false));
        assert!(!should_show_agentic_search_option("   ", false));
        assert!(!should_show_agentic_search_option("needle", true));
        assert_eq!(AGENTIC_SEARCH_OPTION_LABEL, "Search deeply using Claude →");
        assert!(AGENTIC_SEARCH_READONLY_NOTICE.contains("UI-only"));
    }

    #[test]
    fn agentic_search_option_focus_keys_match_official_entry_points() {
        assert!(is_agentic_search_focus_key(&KeyCode::Enter));
        assert!(is_agentic_search_focus_key(&KeyCode::Down));
        assert!(!is_agentic_search_focus_key(&KeyCode::Up));
    }

    #[test]
    fn title_label_matches_official_log_selector_header() {
        assert_eq!(log_selector_title_label(), "Resume Session");
    }

    #[test]
    fn title_count_uses_official_displayed_log_count_not_flattened_tree_rows() {
        let older_fork = with_modified(summary("shared", None, Some("main"), "/repo"), 10);
        let latest_fork = with_modified(summary("shared", None, Some("feature"), "/repo"), 30);
        let displayed_logs = vec![latest_fork.clone(), older_fork];
        let collapsed_rows = group_log_rows(&displayed_logs, 80, false, &[], false, &[]);

        assert_eq!(collapsed_rows.len(), 1);
        assert_eq!(
            log_selector_title_count(ViewMode::List, collapsed_rows.get(0), &displayed_logs, 1,),
            " (1 of 2)"
        );
        assert_eq!(
            log_selector_title_count(ViewMode::Search, collapsed_rows.get(0), &displayed_logs, 1,),
            ""
        );
        assert_eq!(log_selector_title_count(ViewMode::List, None, &[], 1), "");
    }

    #[test]
    fn rename_submit_persists_selected_full_path_and_notifies_parent() {
        let _env_guard = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let _write = crate::utils::env_utils::EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let path = std::env::temp_dir().join(format!(
            "cometix-log-selector-rename-{}.jsonl",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(
            &path,
            format!(
                "{}\n",
                json!({
                    "type":"user",
                    "uuid":"rename-ui-root",
                    "parentUuid":null,
                    "message":{"role":"user","content":"rename me"}
                })
            ),
        )
        .unwrap();
        let mut log = summary("rename-ui", None, Some("main"), "/other/project");
        log.file_path = path.clone();
        log.file_size = std::fs::metadata(&path).unwrap().len();
        let callback_count = Arc::new(AtomicUsize::new(0));
        let callback_for_handler = callback_count.clone();
        let current_theme = *crate::utils::theme::current();
        let events = vec![
            (ctrl_char('r'), 0),
            (press(KeyCode::Char('N')), 20),
            (press(KeyCode::Char('e')), 20),
            (press(KeyCode::Char('w')), 20),
            (press(KeyCode::Enter), 20),
        ];
        let event_stream = stream::unfold(events.into_iter(), |mut events| async move {
            let (event, delay_ms) = events.next()?;
            if delay_ms > 0 {
                futures_timer::Delay::new(Duration::from_millis(delay_ms)).await;
            }
            Some((event, events))
        });

        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(current_theme)) {
                    LogSelector(
                        logs: vec![log],
                        max_height: 20usize,
                        force_width: Some(80usize),
                        on_cancel: move |_| {},
                        on_select: move |_| {},
                        on_logs_changed: move |_| {
                            callback_for_handler.fetch_add(1, AtomicOrdering::SeqCst);
                        },
                        show_all_projects: false,
                        on_toggle_all_projects: move |_| {},
                    )
                }
            };
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(event_stream).with_size(80, 24),
            ));
            for _ in 0..20 {
                if crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(100)).await;
                    None
                })
                .await
                .is_none()
                {
                    break;
                }
            }
        });

        let entries = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        let renamed = entries.last().unwrap();
        assert_eq!(renamed["type"], "custom-title");
        assert_eq!(renamed["customTitle"], "New");
        assert_eq!(renamed["sessionId"], "rename-ui");
        assert_eq!(callback_count.load(AtomicOrdering::SeqCst), 1);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn preview_escape_restores_selector_without_selecting() {
        let path = std::env::temp_dir().join(format!(
            "cometix-log-selector-preview-{}.jsonl",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(
            &path,
            format!(
                "{}\n{}\n",
                json!({
                    "type": "user",
                    "uuid": "user-1",
                    "parentUuid": null,
                    "message": {"content": [{"type": "text", "text": "Preview restore prompt"}]}
                }),
                json!({
                    "type": "assistant",
                    "uuid": "assistant-1",
                    "parentUuid": "user-1",
                    "message": {"content": [{"type": "text", "text": "Preview restore assistant line"}]}
                })
            ),
        )
        .expect("fixture write should succeed");
        let mut log = summary("preview", Some("audit"), Some("main"), "/repo");
        log.custom_title = Some("Resume Preview Test".to_string());
        log.file_path = path.clone();
        log.file_size = std::fs::metadata(&path)
            .expect("fixture metadata should be readable")
            .len();

        let current_theme = *crate::utils::theme::current();
        let events = vec![(ctrl_char('v'), 0), (press(KeyCode::Esc), 50)];
        let event_stream = stream::unfold(events.into_iter(), |mut events| async move {
            let (event, delay_ms) = events.next()?;
            if delay_ms > 0 {
                futures_timer::Delay::new(Duration::from_millis(delay_ms)).await;
            }
            Some((event, events))
        });
        let canvases = futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(
                    crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings()
                )) {
                    ContextProvider(value: Context::owned(current_theme)) {
                        // The preview pane renders transcript rows, which read
                        // app state — strict since P7.
                        crate::state::app_state::AppStateProvider(
                            children: crate::state::app_state::ProviderChildren::new(move || element! {
                                LogSelector(
                                    logs: vec![log.clone()],
                                    max_height: 20usize,
                                    force_width: Some(80usize),
                                    on_cancel: move |_| {},
                                    on_select: move |_| {},
                                    show_all_projects: false,
                                    on_toggle_all_projects: move |_| {},
                                )
                            }.into_any()),
                        )
                    }
                }
            };
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(event_stream).with_size(80, 24),
            ));
            let mut canvases = Vec::new();
            loop {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(100)).await;
                    None
                })
                .await;
                let Some(canvas) = next else {
                    break;
                };
                canvases.push(canvas);
                if canvases.len() >= 20 {
                    break;
                }
            }
            canvases
        });

        let rendered = canvases.iter().map(canvas_text).collect::<Vec<_>>();
        assert!(
            rendered.iter().any(|text| {
                text.contains("Preview restore assistant line")
                    && text.contains("Enter to resume · Esc to cancel")
            }),
            "Ctrl+V should open the read-only SessionPreview; canvases=\n{}",
            rendered.join("\n--- frame ---\n")
        );
        let last = rendered
            .last()
            .expect("mock render should produce a final canvas");
        assert!(
            last.contains("Resume Preview Test"),
            "Esc in preview should restore the selector; canvas=\n{last}"
        );
        assert!(
            last.contains("Ctrl+V to preview"),
            "restored selector should show the list footer again; canvas=\n{last}"
        );
        assert!(
            !last.contains("Enter to resume · Esc to cancel"),
            "preview footer should be gone after Esc restores the selector; canvas=\n{last}"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn preview_enter_selects_previewed_session() {
        let path = std::env::temp_dir().join(format!(
            "cometix-log-selector-preview-select-{}.jsonl",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(
            &path,
            format!(
                "{}\n{}\n",
                json!({
                    "type": "user",
                    "uuid": "user-select",
                    "parentUuid": null,
                    "message": {"content": [{"type": "text", "text": "Preview select prompt"}]}
                }),
                json!({
                    "type": "assistant",
                    "uuid": "assistant-select",
                    "parentUuid": "user-select",
                    "message": {"content": [{"type": "text", "text": "Preview select assistant line"}]}
                })
            ),
        )
        .expect("fixture write should succeed");
        let mut log = summary("preview-select", None, Some("main"), "/repo");
        log.custom_title = Some("Resume Preview Select".to_string());
        log.file_path = path.clone();
        log.file_size = std::fs::metadata(&path)
            .expect("fixture metadata should be readable")
            .len();

        let selected_session = Arc::new(Mutex::new(None::<String>));
        let selected_for_handler = Arc::clone(&selected_session);
        let current_theme = *crate::utils::theme::current();
        let events = vec![(ctrl_char('v'), 0), (press(KeyCode::Enter), 50)];
        let event_stream = stream::unfold(events.into_iter(), |mut events| async move {
            let (event, delay_ms) = events.next()?;
            if delay_ms > 0 {
                futures_timer::Delay::new(Duration::from_millis(delay_ms)).await;
            }
            Some((event, events))
        });
        let canvases = futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(
                    crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings()
                )) {
                    ContextProvider(value: Context::owned(current_theme)) {
                        // The preview pane renders transcript rows, which read
                        // app state — strict since P7.
                        crate::state::app_state::AppStateProvider(
                            children: crate::state::app_state::ProviderChildren::new(move || element! {
                                LogSelector(
                                    logs: vec![log.clone()],
                                    max_height: 20usize,
                                    force_width: Some(80usize),
                                    on_cancel: move |_| {},
                                    on_select: {
                                        let selected_for_handler = Arc::clone(&selected_for_handler);
                                        move |selection: SessionSelection| {
                                            *selected_for_handler
                                                .lock()
                                                .expect("selected session lock should not be poisoned") =
                                                Some(selection.session_id);
                                        }
                                    },
                                    show_all_projects: false,
                                    on_toggle_all_projects: move |_| {},
                                )
                            }.into_any()),
                        )
                    }
                }
            };
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(event_stream).with_size(80, 24),
            ));
            let mut canvases = Vec::new();
            loop {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(100)).await;
                    None
                })
                .await;
                let Some(canvas) = next else {
                    break;
                };
                canvases.push(canvas);
                if canvases.len() >= 20 {
                    break;
                }
            }
            canvases
        });

        let rendered = canvases.iter().map(canvas_text).collect::<Vec<_>>();
        assert!(
            rendered
                .iter()
                .any(|text| text.contains("Preview select assistant line")),
            "Ctrl+V should open the preview before Enter selects it; canvases=\n{}",
            rendered.join("\n--- frame ---\n")
        );
        assert_eq!(
            selected_session
                .lock()
                .expect("selected session lock should not be poisoned")
                .as_deref(),
            Some("preview-select"),
            "Enter in SessionPreview should select the previewed session"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn footer_copy_matches_official_byline_keyboard_hints() {
        assert_eq!(
            search_footer_text(),
            "Type to Search · Enter to select · Esc to clear"
        );
        assert_eq!(rename_footer_text(), "Enter to save · Esc to cancel");
        assert_eq!(
            agentic_search_option_footer_text(),
            "Enter to search · ↓ to skip · Esc to cancel"
        );
        assert_eq!(
            list_footer_text(false, true, true, false, true, Some("→ to expand")),
            "Ctrl+A to show all projects · Ctrl+B to toggle branch · Ctrl+W to show all worktrees · Ctrl+V to preview · Ctrl+R to rename · Type to search · Esc to cancel · → to expand"
        );
        assert_eq!(
            list_footer_text(true, false, true, true, false, None),
            "Ctrl+A to show current dir · Ctrl+W to show current worktree · Ctrl+V to preview · Ctrl+R to rename · Type to search · Esc to cancel"
        );
    }

    #[test]
    fn exit_double_press_footer_matches_official_log_selector_copy() {
        assert_eq!(
            exit_double_press_key(&KeyCode::Char('c'), &KeyModifiers::CONTROL),
            Some("Ctrl-C")
        );
        assert_eq!(
            exit_double_press_key(&KeyCode::Char('d'), &KeyModifiers::CONTROL),
            Some("Ctrl-D")
        );
        assert_eq!(
            exit_double_press_key(&KeyCode::Char('c'), &KeyModifiers::empty()),
            None
        );
        assert_eq!(
            exit_pending_footer_text("Ctrl-C"),
            "Press Ctrl-C again to exit"
        );
        assert_eq!(
            active_exit_pending_footer(Some("Ctrl-C"), true, false).as_deref(),
            Some("Press Ctrl-C again to exit")
        );
        assert_eq!(
            active_exit_pending_footer(Some("Ctrl-C"), false, false),
            None
        );
    }

    #[test]
    fn log_label_marks_sidechain_like_official_log_label() {
        let mut log = summary("a", None, None, "/repo");
        log.is_sidechain = true;

        assert!(build_log_label(&log, 80).ends_with(" (sidechain)"));
    }

    #[test]
    fn custom_title_overrides_apply_rename_preview_in_memory_only() {
        let logs = vec![summary("a", None, None, "/repo")];
        let mut overrides = Vec::new();
        upsert_custom_title_override(&mut overrides, "a", "Renamed session".to_string());

        let renamed = apply_custom_title_overrides(logs.clone(), &overrides);

        assert_eq!(logs[0].custom_title, None);
        assert_eq!(renamed[0].custom_title.as_deref(), Some("Renamed session"));
        assert_eq!(build_log_label(&renamed[0], 80), "Renamed session");
    }

    #[test]
    fn custom_title_override_upsert_keeps_latest_title_like_official_rename() {
        let mut overrides = Vec::new();
        upsert_custom_title_override(&mut overrides, "a", "First".to_string());
        upsert_custom_title_override(&mut overrides, "a", "Second".to_string());

        assert_eq!(overrides, vec![("a".to_string(), "Second".to_string())]);
    }

    #[test]
    fn structured_filters_match_tag_branch_and_worktree() {
        let log = summary("a", Some("bug"), Some("main"), "/repo");
        assert!(log_matches_structured_filters(
            &log,
            Some("bug"),
            true,
            Some("main"),
            true,
            false,
            "/repo",
        ));
        assert!(!log_matches_structured_filters(
            &log,
            Some("docs"),
            true,
            Some("main"),
            true,
            false,
            "/repo",
        ));
        assert!(!log_matches_structured_filters(
            &log,
            Some("bug"),
            true,
            Some("feature"),
            true,
            false,
            "/repo",
        ));
        assert!(!log_matches_structured_filters(
            &log,
            Some("bug"),
            true,
            Some("main"),
            true,
            false,
            "/other",
        ));
    }

    #[test]
    fn grouped_rows_collapse_session_forks_and_keep_latest_selection_value() {
        let old_fork = with_modified(summary("shared", None, Some("main"), "/repo"), 10);
        let separate = with_modified(summary("separate", None, Some("main"), "/repo"), 20);
        let latest_fork = with_modified(summary("shared", None, Some("feature"), "/repo"), 30);
        let logs = vec![old_fork, separate, latest_fork];

        let rows = group_log_rows(&logs, 80, false, &[], false, &[]);

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].group_session_id, "shared");
        assert_eq!(rows[0].value, "2");
        assert!(rows[0].has_children);
        assert!(rows[0].is_group_parent);
        assert!(rows[0].label.starts_with("▶ "));
        assert!(rows[0].label.contains("(+1 other session)"));
        assert_eq!(rows[1].group_session_id, "separate");
    }

    #[test]
    fn grouped_rows_expand_requested_group_and_indent_children() {
        let old_fork = with_modified(summary("shared", None, Some("main"), "/repo"), 10);
        let latest_fork = with_modified(summary("shared", None, Some("feature"), "/repo"), 30);
        let logs = vec![old_fork, latest_fork];

        let rows = group_log_rows(&logs, 80, false, &["shared".to_string()], false, &[]);

        assert_eq!(rows.len(), 2);
        assert!(rows[0].is_group_parent);
        assert!(rows[0].label.starts_with("▼ "));
        assert!(!rows[1].is_group_parent);
        assert!(rows[1].is_child);
        assert!(rows[1].label.starts_with("  ▸ "));
        assert!(
            rows[1]
                .description
                .as_deref()
                .unwrap_or_default()
                .starts_with("    ")
        );
        assert_eq!(
            expand_collapse_hint(rows.get(0), &["shared".to_string()]),
            Some("← to collapse")
        );
        assert_eq!(
            expand_collapse_hint(rows.get(1), &["shared".to_string()]),
            Some("← to collapse")
        );
    }

    #[test]
    fn grouped_rows_expand_all_for_search_or_branch_filter_mode() {
        let old_fork = with_modified(summary("shared", None, Some("main"), "/repo"), 10);
        let latest_fork = with_modified(summary("shared", None, Some("feature"), "/repo"), 30);
        let logs = vec![old_fork, latest_fork];

        let rows = group_log_rows(&logs, 80, false, &[], true, &[]);

        assert_eq!(rows.len(), 2);
        assert!(rows[0].label.starts_with("▼ "));
        assert!(rows[1].is_child);
    }
    #[component]
    fn LogSearchPeerFrame(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut cancelled = hooks.use_state(|| 0usize);
        let mut selected = hooks.use_state(|| 0usize);
        let mut matching = summary("peer-match", None, None, "/repo");
        matching.display = "needle marker conversation".to_string();
        let mut other = summary("peer-other", None, None, "/repo");
        other.display = "unrelated peer conversation".to_string();
        element! {
            View(flex_direction: FlexDirection::Column) {
                Text(content: format!("peer-cancelled={} selected={}", cancelled.get(), selected.get()))
                LogSelector(
                    logs: vec![matching, other],
                    max_height: 20usize,
                    force_width: Some(180usize),
                    show_all_projects: true,
                    on_cancel: move |_| cancelled.set(cancelled.get() + 1),
                    on_select: move |_| selected.set(selected.get() + 1),
                )
            }
        }
    }

    #[component]
    fn LogSearchPeerHarness(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let runtime = crate::keybindings::keybinding_provider_setup::use_keybinding_setup(
            &mut hooks,
            crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings(),
        );
        let store =
            hooks.use_const(|| crate::state::store::AppStore::new(Default::default(), None));
        element! {
            ContextProvider(value: Context::owned(runtime)) {

                ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                    ContextProvider(value: Context::owned(store.clone())) {
                        FocusScope(handle_keys: false) { LogSearchPeerFrame }
                    }
                }
            }
        }
    }

    #[tokio::test]
    async fn log_search_matches_official_empty_meta_backspace_paste_and_ctrl_c() {
        // Actual source LogSelector.tsx:231-250 useSearchInput options plus
        // complete hook are executed in consumer-search-peer-oracle.json.
        // Alt+Backspace on empty remains in search; a paste inserts the entire
        // chunk; Ctrl+C without onCancel is swallowed without clearing query.
        let _guard = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        // The list footer is intentionally a single NoWrap line. At 100
        // columns the real branch hint clips the final "Type to search" text;
        // use enough terminal width to observe the entire source footer while
        // preserving the full-mode-transition assertion below.
        let (sender, receiver) = async_channel::unbounded();
        let mut app = element! { LogSearchPeerHarness };
        let mut frames = Box::pin(app.mock_terminal_render_loop(
            MockTerminalConfig::with_events(receiver).with_size(180, 32),
        ));
        let deadline = futures_timer::Delay::new(Duration::from_secs(5));
        tokio::pin!(deadline);
        let mut step = 0usize;
        let mut last = String::new();
        loop {
            tokio::select! {
                _ = &mut deadline => panic!("LogSelector source flow stalled at {step}:\n{last}"),
                canvas = frames.next() => {
                    last = canvas_text(&canvas.expect("LogSelector must stay mounted"));
                    let search_line = last.lines().find(|line| line.contains('⌕')).unwrap_or_default();
                    match step {
                        0 if last.contains("unrelated peer conversation") => {
                            sender.send(press(KeyCode::Char('/'))).await.unwrap();
                            step = 1;
                        }
                        1 if last.contains(search_footer_text()) => {
                            let mut key = KeyEvent::new(KeyEventKind::Press, KeyCode::Backspace);
                            key.modifiers = KeyModifiers::ALT;
                            sender.send(TerminalEvent::Key(key)).await.unwrap();
                            sender.send(TerminalEvent::Paste("needle".to_string())).await.unwrap();
                            step = 2;
                        }
                        2 if search_line.contains("needle") && !last.contains("unrelated peer conversation") => {
                            assert!(last.contains(search_footer_text()), "Meta+Backspace must not exit: {last}");
                            assert!(last.contains("needle marker conversation"), "paste must filter real logs: {last}");
                            sender.send(ctrl_char('c')).await.unwrap();
                            sender.send(TerminalEvent::Paste(" marker".to_string())).await.unwrap();
                            step = 3;
                        }
                        3 if search_line.contains("needle marker") => {
                            assert!(last.contains(search_footer_text()), "Ctrl+C must preserve search mode: {last}");
                            assert!(last.contains("peer-cancelled=0 selected=0"), "Ctrl+C must not commit/cancel: {last}");
                            sender.send(ctrl_char('n')).await.unwrap();
                            step = 4;
                        }
                        4 if !last.contains(search_footer_text()) && last.contains("Type to search") => {
                            assert!(search_line.contains("needle marker"), "Ctrl+N must retain query: {last}");
                            assert!(last.contains("peer-cancelled=0 selected=0"), "{last}");
                            break;
                        }
                        _ => {}
                    }
                }
            }
        }
        assert_eq!(step, 4);
    }
}
