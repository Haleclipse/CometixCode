//! @nyan: built-in footer info line (docs/nyan/tui.md §3.1-§3.3).
//!
//! Replaces the deleted statusLine row. The left group is environment
//! (directory, branch, linked-worktree diamond); the right group is session
//! metrics (model, effort, context, cache, tps). The line always occupies one
//! row; narrow terminals drop segments in the §3.2 order instead of wrapping.

use std::path::Path;

use crate::services::compact::auto_compact::calculate_token_warning_state;
use crate::services::provider::anthropic::betas::get_model_betas;
use crate::state::app_state::use_app_state;
use crate::utils::effort::get_displayed_effort_level;
use crate::utils::model::model::render_model_name;
use crate::utils::theme::Theme;
use iocraft::prelude::*;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// The info line always occupies exactly one footer row (§3.1). The footer
/// height budget and the component's render MUST use this same constant.
pub const INFO_LINE_HEIGHT: usize = 1;

/// A directory truncated below this width carries no information; drop it.
const MIN_DIRECTORY_WIDTH: usize = 4;

const LEFT_SEPARATOR: &str = "  ";

/// Linked-worktree marker (§3.1).
const WORKTREE_MARKER: &str = "◇";

/// Everything the info line can show, already formatted. Pure input to
/// [`layout_info_line`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InfoLineSegments {
    pub directory: Option<String>,
    pub branch: Option<String>,
    pub worktree: bool,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub context: Option<String>,
    pub cache: Option<String>,
    /// Spaced spelling (`52 t/s`); the compact spelling drops that space.
    pub tps: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InfoSegmentKind {
    Directory,
    Branch,
    Worktree,
    Model,
    Effort,
    Context,
    Cache,
    Tps,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InfoLineSegment {
    pub kind: InfoSegmentKind,
    pub text: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InfoLineLayout {
    pub left: Vec<InfoLineSegment>,
    pub right: Vec<InfoLineSegment>,
}

impl InfoLineLayout {
    /// Display width including separators and the one-column center gap.
    pub fn width(&self) -> usize {
        let left_width = self
            .left
            .iter()
            .map(|segment| UnicodeWidthStr::width(segment.text.as_str()))
            .sum::<usize>()
            + LEFT_SEPARATOR.len() * self.left.len().saturating_sub(1);
        let right_width = self
            .right
            .iter()
            .map(|segment| UnicodeWidthStr::width(segment.text.as_str()))
            .sum::<usize>()
            + self
                .right
                .windows(2)
                .map(|pair| right_separator(pair[0].kind, pair[1].kind).len())
                .sum::<usize>();
        let gap = usize::from(!self.left.is_empty() && !self.right.is_empty());
        left_width + right_width + gap
    }
}

fn right_separator(previous: InfoSegmentKind, next: InfoSegmentKind) -> &'static str {
    if previous == InfoSegmentKind::Model && next == InfoSegmentKind::Effort {
        " "
    } else {
        " · "
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TpsMode {
    Spaced,
    Compact,
    Hidden,
}

#[derive(Clone, Debug)]
struct BuildFlags {
    directory: bool,
    directory_cap: Option<usize>,
    branch_cap: Option<usize>,
    model_cap: Option<usize>,
    cache: bool,
    tps: TpsMode,
    effort: bool,
}

impl BuildFlags {
    fn full(segments: &InfoLineSegments) -> Self {
        Self {
            directory: segments.directory.is_some(),
            directory_cap: None,
            branch_cap: None,
            model_cap: None,
            cache: segments.cache.is_some(),
            tps: if segments.tps.is_some() {
                TpsMode::Spaced
            } else {
                TpsMode::Hidden
            },
            effort: segments.effort.is_some(),
        }
    }
}

fn build_layout(segments: &InfoLineSegments, flags: &BuildFlags) -> InfoLineLayout {
    let mut left = Vec::new();
    if flags.directory
        && let Some(directory) = segments.directory.as_deref()
    {
        let text = match flags.directory_cap {
            Some(cap) => crate::utils::truncate::truncate_start_to_width(directory, cap),
            None => directory.to_string(),
        };
        left.push(InfoLineSegment {
            kind: InfoSegmentKind::Directory,
            text,
        });
    }
    if let Some(branch) = segments.branch.as_deref() {
        let text = match flags.branch_cap {
            Some(cap) => middle_truncate(branch, cap),
            None => branch.to_string(),
        };
        left.push(InfoLineSegment {
            kind: InfoSegmentKind::Branch,
            text,
        });
    }
    if segments.worktree {
        left.push(InfoLineSegment {
            kind: InfoSegmentKind::Worktree,
            text: WORKTREE_MARKER.to_string(),
        });
    }

    let mut right = Vec::new();
    if let Some(model) = segments.model.as_deref().filter(|model| !model.is_empty()) {
        let text = match flags.model_cap {
            Some(cap) => crate::utils::truncate::truncate_to_width(model, cap),
            None => model.to_string(),
        };
        right.push(InfoLineSegment {
            kind: InfoSegmentKind::Model,
            text,
        });
    }
    if flags.effort
        && let Some(effort) = segments.effort.as_deref()
    {
        right.push(InfoLineSegment {
            kind: InfoSegmentKind::Effort,
            text: effort.to_string(),
        });
    }
    if let Some(context) = segments.context.as_deref() {
        right.push(InfoLineSegment {
            kind: InfoSegmentKind::Context,
            text: context.to_string(),
        });
    }
    if flags.cache
        && let Some(cache) = segments.cache.as_deref()
    {
        right.push(InfoLineSegment {
            kind: InfoSegmentKind::Cache,
            text: cache.to_string(),
        });
    }
    if let Some(tps) = segments.tps.as_deref() {
        match flags.tps {
            TpsMode::Spaced => right.push(InfoLineSegment {
                kind: InfoSegmentKind::Tps,
                text: tps.to_string(),
            }),
            TpsMode::Compact => right.push(InfoLineSegment {
                kind: InfoSegmentKind::Tps,
                text: tps.replace(" t/s", "t/s"),
            }),
            TpsMode::Hidden => {}
        }
    }

    InfoLineLayout { left, right }
}

/// Narrow-screen degradation (§3.2). Pure: segments in, what fits out.
///
/// Order: cache → directory → compact tps → tps → effort → branch middle
/// truncation → model tail truncation. Context survives every step; the model
/// survives as a truncation.
pub fn layout_info_line(segments: &InfoLineSegments, available_width: usize) -> InfoLineLayout {
    let mut flags = BuildFlags::full(segments);
    let mut layout = build_layout(segments, &flags);
    if layout.width() <= available_width {
        return layout;
    }

    // 1. Drop the cache segment.
    if flags.cache {
        flags.cache = false;
        layout = build_layout(segments, &flags);
        if layout.width() <= available_width {
            return layout;
        }
    }

    // 2. Shrink the directory from the left (§3.1 `过长从左截断`), then drop
    //    it (§3.2 step 2). A stub shorter than MIN_DIRECTORY_WIDTH carries no
    //    information, so that case drops it outright.
    if flags.directory {
        if let Some(directory) = segments.directory.as_deref() {
            let overflow = layout.width().saturating_sub(available_width);
            let width = UnicodeWidthStr::width(directory);
            let remaining = width.saturating_sub(overflow);
            if overflow > 0 && remaining >= MIN_DIRECTORY_WIDTH {
                flags.directory_cap = Some(remaining);
                layout = build_layout(segments, &flags);
                if layout.width() <= available_width {
                    return layout;
                }
            }
        }
        flags.directory = false;
        flags.directory_cap = None;
        layout = build_layout(segments, &flags);
        if layout.width() <= available_width {
            return layout;
        }
    }

    // 3. Tighten tps (`52 t/s` → `52t/s`), then drop it.
    if flags.tps == TpsMode::Spaced {
        flags.tps = TpsMode::Compact;
        layout = build_layout(segments, &flags);
        if layout.width() <= available_width {
            return layout;
        }
    }
    if flags.tps == TpsMode::Compact {
        flags.tps = TpsMode::Hidden;
        layout = build_layout(segments, &flags);
        if layout.width() <= available_width {
            return layout;
        }
    }

    // 4. Drop the effort level.
    if flags.effort {
        flags.effort = false;
        layout = build_layout(segments, &flags);
        if layout.width() <= available_width {
            return layout;
        }
    }

    // 5. Middle-truncate the branch, keeping both ends.
    if let Some(branch) = segments.branch.as_deref() {
        let overflow = layout.width().saturating_sub(available_width);
        let width = UnicodeWidthStr::width(branch);
        if overflow > 0 {
            flags.branch_cap = Some(
                width
                    .saturating_sub(overflow)
                    .max(1)
                    .min(width.saturating_sub(1)),
            );
            layout = build_layout(segments, &flags);
            if layout.width() <= available_width {
                return layout;
            }
        }
    }

    // 6. Tail-truncate the model name. Everything that can be dropped is gone;
    // this is the last reduction.
    if let Some(model) = segments.model.as_deref().filter(|model| !model.is_empty()) {
        let overflow = layout.width().saturating_sub(available_width);
        let width = UnicodeWidthStr::width(model);
        if overflow > 0 {
            flags.model_cap = Some(
                width
                    .saturating_sub(overflow)
                    .max(1)
                    .min(width.saturating_sub(1)),
            );
            layout = build_layout(segments, &flags);
        }
    }
    layout
}

/// Middle truncation for branch names: `feat/…footer`.
pub fn middle_truncate(text: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_string();
    }
    if max_width <= 1 {
        return "…".to_string();
    }
    let budget = max_width - 1;
    let left_budget = budget / 2;
    let right_budget = budget - left_budget;
    let left = take_start(text, left_budget);
    let right = take_end(text, right_budget);
    format!("{left}…{right}")
}

fn take_start(text: &str, max_width: usize) -> String {
    crate::utils::truncate::truncate_to_width_no_ellipsis(text, max_width)
}

fn take_end(text: &str, max_width: usize) -> String {
    let mut width = 0usize;
    let mut kept = Vec::new();
    for grapheme in UnicodeSegmentation::graphemes(text, true).rev() {
        let grapheme_width = UnicodeWidthStr::width(grapheme);
        if width + grapheme_width > max_width {
            break;
        }
        kept.push(grapheme);
        width += grapheme_width;
    }
    kept.reverse();
    kept.concat()
}

/// `<1000` raw, `<1M` integer thousands, `≥1M` one decimal (§3.3).
pub fn format_context_tokens(tokens: u64) -> String {
    if tokens < 1_000 {
        return tokens.to_string();
    }
    if tokens < 1_000_000 {
        let thousands = (tokens + 500) / 1_000;
        if thousands < 1_000 {
            return format!("{thousands}k");
        }
    }
    let tenths = (tokens + 50_000) / 100_000;
    if tenths < 100 {
        let whole = tenths / 10;
        let tenth = tenths % 10;
        if tenth == 0 {
            format!("{whole}M")
        } else {
            format!("{whole}.{tenth}M")
        }
    } else {
        format!("{}M", (tokens + 500_000) / 1_000_000)
    }
}

/// `52 t/s` (one decimal below 10, §5.1).
pub fn format_tps(rate: f64) -> String {
    if rate < 10.0 {
        format!("{rate:.1} t/s")
    } else {
        format!("{rate:.0} t/s")
    }
}

/// `cache 91%`.
pub fn format_cache(rate: f64) -> String {
    format!("cache {}%", (rate * 100.0).round().clamp(0.0, 100.0) as i64)
}

/// Directory segment: the cwd basename, or `<repo>/<sub path>` inside a repo
/// (§3.1).
///
/// `find_git_root` is the memoized in-process filesystem walk; it does not
/// spawn git, so it is safe on the render path.
pub fn directory_display(cwd: &Path) -> String {
    let name_of = |path: &Path| {
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.is_empty())
    };
    if let Some(root) = crate::utils::git::find_git_root(cwd)
        && let Ok(relative) = cwd.strip_prefix(&root)
        && let Some(root_name) = name_of(&root)
    {
        let relative = relative.to_string_lossy().replace('\\', "/");
        if relative.is_empty() {
            return root_name;
        }
        return format!("{root_name}/{relative}");
    }
    name_of(cwd).unwrap_or_else(|| cwd.display().to_string())
}

/// Two-level context color, from the same state auto-compact uses (§3.3).
pub fn context_line_color(token_usage: u64, model: &str, theme: &Theme) -> Color {
    let state = calculate_token_warning_state(token_usage as i64, model);
    if state.is_above_error_threshold {
        theme.error
    } else if state.is_above_warning_threshold {
        theme.warning
    } else {
        theme.inactive
    }
}

#[derive(Default, Props)]
pub struct InfoLineProps {
    /// Same number auto-compact uses (the caller's `token_usage` memo).
    pub token_usage: u64,
    /// Terminal columns. `None` reads the process terminal.
    pub width: Option<u32>,
}

#[component]
pub fn InfoLine(props: &InfoLineProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let (terminal_columns, _) = hooks.use_terminal_size();
    let width = props
        .width
        .map(|width| width as usize)
        .unwrap_or(terminal_columns as usize);
    let metrics = use_app_state(&mut hooks, |state| state.footer_metrics.clone());
    let model = use_app_state(&mut hooks, |state| {
        crate::utils::model::model::profile_model(state.main_loop_profile.as_deref())
    });
    let effort = use_app_state(&mut hooks, |state| state.effort_value);

    // @nyan: `git_cwd` is written synchronously (before the probe) by the
    // main thread's Bash completion / cwd change, so it tracks a `cd` within
    // the turn and matches the branch shown next to it. Before the first
    // Bash it is unset; `session_cwd` (last main-thread `effective_cwd`, else
    // the original cwd) covers that.
    let cwd = metrics
        .git_cwd
        .clone()
        .unwrap_or_else(crate::state::footer_metrics::session_cwd);
    let directory = Some(directory_display(&cwd)).filter(|text| !text.is_empty());
    let (branch, worktree) = if metrics.git.in_repo {
        let branch = if metrics.git.detached {
            metrics
                .git
                .short_sha
                .as_deref()
                .map(|sha| format!("@{sha}"))
        } else {
            metrics.git.branch.clone()
        };
        (branch, metrics.git.linked_worktree)
    } else {
        (None, false)
    };
    let display_model = render_model_name(&model);
    let model_segment = (!display_model.is_empty()).then_some(display_model);
    let displayed_effort = get_displayed_effort_level(&model, effort.as_ref());
    let effort_segment = (displayed_effort != "off").then(|| displayed_effort.to_string());
    let context = (props.token_usage > 0).then(|| {
        let window =
            crate::utils::context::get_context_window_for_model(&model, &get_model_betas(&model))
                .max(0) as u64;
        format!(
            "{}/{}",
            format_context_tokens(props.token_usage),
            format_context_tokens(window),
        )
    });
    let cache = metrics
        .cache
        .session_reported_cache
        .then(|| format_cache(metrics.cache.hit_rate.unwrap_or(0.0)));
    let tps = metrics
        .displayed_turn()
        .and_then(crate::state::footer_metrics::TpsTurn::tokens_per_second)
        .map(format_tps);

    let segments = InfoLineSegments {
        directory,
        branch,
        worktree,
        model: model_segment,
        effort: effort_segment,
        context,
        cache,
        tps,
    };
    // Two columns of padding on each side, matching the mode row.
    let layout = layout_info_line(&segments, width.saturating_sub(4));
    let context_color = context_line_color(props.token_usage, &model, &theme);
    let tps_generating = metrics.turn_open && !metrics.current_turn.is_empty();
    let cache_warning = metrics.cache.miss_warning;

    let mut cells: Vec<AnyElement<'static>> = Vec::new();
    for (index, segment) in layout.left.iter().enumerate() {
        if index > 0 {
            cells.push(separator_text(LEFT_SEPARATOR, &theme));
        }
        cells.push(info_segment_text(
            segment,
            &theme,
            context_color,
            tps_generating,
            cache_warning,
        ));
    }
    // Right-align the metrics group: the filler also lands when the left group
    // has been degraded away, so the line keeps its two-column shape.
    if !layout.right.is_empty() {
        cells.push(element! { View(flex_grow: 1.0f32) }.into_any());
    }
    for (index, segment) in layout.right.iter().enumerate() {
        if index > 0 {
            cells.push(separator_text(
                right_separator(layout.right[index - 1].kind, segment.kind),
                &theme,
            ));
        }
        cells.push(info_segment_text(
            segment,
            &theme,
            context_color,
            tps_generating,
            cache_warning,
        ));
    }

    element! {
        View(
            flex_direction: FlexDirection::Row,
            width: 100pct,
            height: INFO_LINE_HEIGHT as u32,
            padding_left: 2u32,
            padding_right: 2u32,
            overflow: Overflow::Hidden,
        ) {
            #(cells)
        }
    }
    .into_any()
}

fn separator_text(text: &str, theme: &Theme) -> AnyElement<'static> {
    element! {
        Text(content: text.to_string(), color: theme.inactive, wrap: TextWrap::NoWrap)
    }
    .into_any()
}

fn info_segment_text(
    segment: &InfoLineSegment,
    theme: &Theme,
    context_color: Color,
    tps_generating: bool,
    cache_warning: bool,
) -> AnyElement<'static> {
    let color = match segment.kind {
        InfoSegmentKind::Directory => theme.text,
        InfoSegmentKind::Worktree => theme.nyan_blue,
        InfoSegmentKind::Context => context_color,
        InfoSegmentKind::Cache if cache_warning => theme.warning,
        InfoSegmentKind::Tps if tps_generating => theme.text,
        InfoSegmentKind::Branch
        | InfoSegmentKind::Model
        | InfoSegmentKind::Effort
        | InfoSegmentKind::Cache
        | InfoSegmentKind::Tps => theme.inactive,
    };
    element! {
        Text(content: segment.text.clone(), color: color, wrap: TextWrap::NoWrap)
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full_segments() -> InfoLineSegments {
        InfoLineSegments {
            directory: Some("nyan".into()),
            branch: Some("master".into()),
            worktree: true,
            model: Some("Opus 4.6".into()),
            effort: Some("high".into()),
            context: Some("84k/200k".into()),
            cache: Some("cache 91%".into()),
            tps: Some("52 t/s".into()),
        }
    }

    fn kinds(layout: &InfoLineLayout) -> Vec<InfoSegmentKind> {
        layout
            .left
            .iter()
            .chain(layout.right.iter())
            .map(|segment| segment.kind)
            .collect()
    }

    fn texts(layout: &InfoLineLayout) -> Vec<&str> {
        layout
            .left
            .iter()
            .chain(layout.right.iter())
            .map(|segment| segment.text.as_str())
            .collect()
    }

    fn available(width: usize) -> usize {
        width.saturating_sub(4)
    }

    #[test]
    fn wide_layout_keeps_every_segment() {
        let layout = layout_info_line(&full_segments(), available(120));
        assert!(layout.width() <= available(120));
        assert_eq!(
            kinds(&layout),
            vec![
                InfoSegmentKind::Directory,
                InfoSegmentKind::Branch,
                InfoSegmentKind::Worktree,
                InfoSegmentKind::Model,
                InfoSegmentKind::Effort,
                InfoSegmentKind::Context,
                InfoSegmentKind::Cache,
                InfoSegmentKind::Tps,
            ]
        );
        assert_eq!(
            texts(&layout),
            vec![
                "nyan",
                "master",
                "◇",
                "Opus 4.6",
                "high",
                "84k/200k",
                "cache 91%",
                "52 t/s"
            ]
        );
    }

    #[test]
    fn documented_widths_all_fit_without_wrapping() {
        let segments = full_segments();
        for width in [120, 80, 60, 40] {
            let layout = layout_info_line(&segments, available(width));
            assert!(
                layout.width() <= available(width),
                "width={width} layout_width={} kinds={:?}",
                layout.width(),
                kinds(&layout)
            );
            // The context segment is never dropped, and the model survives.
            assert!(
                kinds(&layout).contains(&InfoSegmentKind::Context),
                "width={width}"
            );
            assert!(
                kinds(&layout).contains(&InfoSegmentKind::Model),
                "width={width}"
            );
        }
        // At 120 the whole line fits.
        let wide = layout_info_line(&segments, available(120));
        assert!(kinds(&wide).contains(&InfoSegmentKind::Cache));
        // At 40, cache, directory and tps are gone. Effort may legitimately
        // survive because degradation stops as soon as the line fits.
        let narrow = layout_info_line(&segments, available(40));
        for dropped in [
            InfoSegmentKind::Cache,
            InfoSegmentKind::Directory,
            InfoSegmentKind::Tps,
        ] {
            assert!(
                !kinds(&narrow).contains(&dropped),
                "width=40 kinds={:?}",
                kinds(&narrow)
            );
        }
        assert!(kinds(&narrow).contains(&InfoSegmentKind::Context));
    }

    #[test]
    fn segments_drop_in_the_documented_order() {
        let segments = full_segments();
        let full_width = layout_info_line(&segments, usize::MAX).width();

        // One column short: only the cache goes.
        let layout = layout_info_line(&segments, full_width - 1);
        assert!(!kinds(&layout).contains(&InfoSegmentKind::Cache));
        assert!(kinds(&layout).contains(&InfoSegmentKind::Directory));
        let after_cache = layout.width();

        // One more column short: the directory goes next.
        let layout = layout_info_line(&segments, after_cache - 1);
        assert!(!kinds(&layout).contains(&InfoSegmentKind::Directory));
        assert!(kinds(&layout).contains(&InfoSegmentKind::Tps));
        let after_directory = layout.width();

        // Then tps compacts rather than disappearing.
        let layout = layout_info_line(&segments, after_directory - 1);
        let tps = layout
            .right
            .iter()
            .find(|segment| segment.kind == InfoSegmentKind::Tps)
            .expect("tps still present, compact");
        assert_eq!(tps.text, "52t/s");
        let after_compact = layout.width();

        // Then tps goes entirely.
        let layout = layout_info_line(&segments, after_compact - 1);
        assert!(!kinds(&layout).contains(&InfoSegmentKind::Tps));
        assert!(kinds(&layout).contains(&InfoSegmentKind::Effort));
        let after_tps = layout.width();

        // Then effort.
        let layout = layout_info_line(&segments, after_tps - 1);
        assert!(!kinds(&layout).contains(&InfoSegmentKind::Effort));
        assert!(layout.width() < after_tps);
    }

    #[test]
    fn long_branch_middle_truncates_before_the_model_is_cut() {
        let mut segments = full_segments();
        segments.branch = Some("feature/footer-info-line-degradation".into());
        segments.cache = None;
        segments.tps = None;
        segments.effort = None;
        segments.directory = None;
        let width = UnicodeWidthStr::width("Opus 4.6") + 3 + UnicodeWidthStr::width("84k/200k") + 1;
        let branch_budget = 16;
        let layout = layout_info_line(
            &segments,
            width + 1 + UnicodeWidthStr::width("◇") + 2 + branch_budget,
        );
        let branch = layout
            .left
            .iter()
            .find(|segment| segment.kind == InfoSegmentKind::Branch)
            .expect("branch survives truncation");
        assert!(branch.text.contains('…'), "branch={}", branch.text);
        assert!(branch.text.starts_with("feat"), "branch={}", branch.text);
        assert!(branch.text.ends_with("tion"), "branch={}", branch.text);
        let model = layout
            .right
            .iter()
            .find(|segment| segment.kind == InfoSegmentKind::Model)
            .expect("model survives");
        assert_eq!(model.text, "Opus 4.6");
    }

    #[test]
    fn long_non_claude_model_tail_truncates_last() {
        let segments = InfoLineSegments {
            model: Some("moonshotai/kimi-k2-0905-preview".into()),
            context: Some("84k/200k".into()),
            ..Default::default()
        };
        let context_width = UnicodeWidthStr::width("84k/200k");
        let layout = layout_info_line(&segments, 20);
        assert!(layout.width() <= 20, "width={}", layout.width());
        let model = layout
            .right
            .iter()
            .find(|segment| segment.kind == InfoSegmentKind::Model)
            .expect("model survives");
        assert!(model.text.ends_with('…'), "model={}", model.text);
        assert!(model.text.starts_with("moonsho"), "model={}", model.text);
        assert_eq!(
            layout
                .right
                .iter()
                .find(|segment| segment.kind == InfoSegmentKind::Context)
                .expect("context always survives")
                .text,
            "84k/200k"
        );
        assert!(context_width > 0);
    }

    #[test]
    fn chinese_directory_width_is_measured_not_counted() {
        let dir = "nyan/项目/文档归档目录";
        let dir_width = UnicodeWidthStr::width(dir);
        assert_eq!(dir_width, 22);
        let segments = InfoLineSegments {
            directory: Some(dir.into()),
            context: Some("84k/200k".into()),
            ..Default::default()
        };
        // Fits only when measured by display width.
        let layout = layout_info_line(&segments, dir_width + 1 + 8);
        assert!(
            kinds(&layout).contains(&InfoSegmentKind::Directory),
            "display width must fit the directory; kinds={:?}",
            kinds(&layout)
        );
        // Too narrow: the directory is dropped, context stays.
        let layout = layout_info_line(&segments, 12);
        assert!(!kinds(&layout).contains(&InfoSegmentKind::Directory));
        assert!(kinds(&layout).contains(&InfoSegmentKind::Context));
    }

    #[test]
    fn directory_display_prefers_repo_relative_shape() {
        let root = std::env::temp_dir().join(format!(
            "nyan-info-line-dir-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let nested = root.join("项目").join("子目录");
        std::fs::create_dir_all(&nested).unwrap();
        let root_name = root.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(
            directory_display(&nested),
            format!("{root_name}/项目/子目录")
        );
        assert_eq!(directory_display(&root), root_name);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn context_tokens_match_the_three_formats() {
        assert_eq!(format_context_tokens(0), "0");
        assert_eq!(format_context_tokens(999), "999");
        assert_eq!(format_context_tokens(1_000), "1k");
        assert_eq!(format_context_tokens(84_400), "84k");
        assert_eq!(format_context_tokens(84_600), "85k");
        assert_eq!(format_context_tokens(999_999), "1M");
        assert_eq!(format_context_tokens(1_000_000), "1M");
        assert_eq!(format_context_tokens(1_200_000), "1.2M");
        assert_eq!(format_context_tokens(12_400_000), "12M");
    }

    #[test]
    fn tps_and_cache_formats_match_metric_contract() {
        assert_eq!(format_tps(7.25), "7.2 t/s");
        assert_eq!(format_tps(52.4), "52 t/s");
        assert_eq!(format_tps(52.4).replace(" t/s", "t/s"), "52t/s");
        assert_eq!(format_cache(0.9123), "cache 91%");
        assert_eq!(format_cache(0.0), "cache 0%");
    }

    #[test]
    fn empty_segments_produce_an_empty_layout() {
        let layout = layout_info_line(&InfoLineSegments::default(), 80);
        assert!(layout.left.is_empty());
        assert!(layout.right.is_empty());
        assert_eq!(layout.width(), 0);
    }

    #[test]
    fn middle_truncate_preserves_both_ends_by_display_width() {
        assert_eq!(middle_truncate("feat/footer", 20), "feat/footer");
        let truncated = middle_truncate("feature/footer-layout", 12);
        assert!(UnicodeWidthStr::width(truncated.as_str()) <= 12);
        assert!(truncated.starts_with("feat"));
        assert!(truncated.ends_with("layout"));
        assert!(truncated.contains('…'));
        let chinese = middle_truncate("功能/页脚信息行", 6);
        assert!(UnicodeWidthStr::width(chinese.as_str()) <= 6);
    }

    // --- component rendering -------------------------------------------------

    use crate::services::provider::ir::Usage;
    use crate::state::app_state_store::AppState;
    use crate::state::store::AppStore;
    use crate::utils::test_env::{HOME_VAR, TEST_ENV_LOCK, TestEnvGuard};

    /// Pins everything the component reads through process state: the session
    /// profile settings, the global config (auto-compact threshold), the cwd
    /// and the git memo cache.
    struct InfoLineFixture {
        _lock: TestEnvGuard<'static>,
        _home: crate::utils::test_env::EnvVarGuard,
        _cwd: OriginalCwdRestore,
        _auto_compact: crate::utils::test_env::EnvVarGuard,
    }

    struct OriginalCwdRestore(std::path::PathBuf);

    impl Drop for OriginalCwdRestore {
        fn drop(&mut self) {
            crate::bootstrap::state::set_original_cwd(&self.0);
            crate::utils::git::find_git_root_cache().clear();
        }
    }

    impl InfoLineFixture {
        fn new(model: &str) -> Self {
            let lock = TEST_ENV_LOCK.lock().unwrap();
            crate::bootstrap::state::set_session_profile(None);
            crate::utils::settings::settings_cache::install_test_settings(
                serde_json::from_value(serde_json::json!({
                    "nyan": {
                        "providers": {
                            "anthropic": { "kind": "anthropic", "protocol": "anthropicMessages" }
                        },
                        "models": {
                            "main": {
                                "provider": "anthropic",
                                "model": model,
                                "capabilities": {
                                    "contextWindow": 200000,
                                    "maxOutputTokens": 32000
                                }
                            }
                        },
                        "selection": { "main": "main" }
                    }
                }))
                .unwrap(),
            );
            let home = std::env::temp_dir().join(format!(
                "nyan-info-line-home-{}",
                uuid::Uuid::new_v4().simple()
            ));
            std::fs::create_dir_all(&home).unwrap();
            let home_var =
                crate::utils::test_env::EnvVarGuard::set(HOME_VAR, home.to_string_lossy().as_ref());
            let cwd = std::env::temp_dir().join(format!(
                "nyan-info-line-cwd-{}",
                uuid::Uuid::new_v4().simple()
            ));
            std::fs::create_dir_all(&cwd).unwrap();
            let original = crate::bootstrap::state::get_original_cwd();
            crate::bootstrap::state::set_original_cwd(&cwd);
            crate::utils::git::find_git_root_cache().clear();
            let auto_compact = crate::utils::test_env::EnvVarGuard::unset("DISABLE_AUTO_COMPACT");
            Self {
                _lock: lock,
                _home: home_var,
                _cwd: OriginalCwdRestore(original),
                _auto_compact: auto_compact,
            }
        }
    }

    fn usage(input: u64, output: u64, cache_read: u64, cache_write: u64) -> Usage {
        Usage {
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: cache_read,
            cache_write_tokens: cache_write,
            ..Usage::default()
        }
    }

    fn store_with(
        footer_metrics: crate::state::footer_metrics::FooterMetrics,
        effort: Option<crate::utils::effort::Effort>,
        profile: Option<&str>,
    ) -> AppStore {
        let mut initial = AppState::default();
        initial.footer_metrics = footer_metrics;
        initial.effort_value = effort;
        initial.main_loop_profile = profile.map(str::to_string);
        AppStore::new(initial, None)
    }

    fn render_info_line(store: AppStore, token_usage: u64, width: usize) -> Canvas {
        element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                crate::state::app_state::AppStateProvider(
                    prebuilt_store: Some(store),
                    children: crate::state::app_state::ProviderChildren::new(move || element! {
                        InfoLine(token_usage: token_usage, width: Some(width as u32))
                    }.into_any()),
                )
            }
        }
        .render(Some(width))
    }

    fn style_at(canvas: &Canvas, needle: &str) -> Color {
        let text = canvas.to_string();
        let line_index = text
            .lines()
            .position(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("missing {needle:?} in canvas=\n{text}"));
        let line = text.lines().nth(line_index).unwrap();
        let byte_index = line.find(needle).unwrap();
        let x = UnicodeWidthStr::width(&line[..byte_index]);
        canvas
            .resolved_text_style(x, line_index)
            .unwrap_or_else(|| panic!("no style at {needle:?} in canvas=\n{text}"))
            .color
            .expect("styled segment")
    }

    fn git_snapshot(
        branch: Option<&str>,
        detached: bool,
        linked: bool,
    ) -> crate::state::footer_metrics::GitSnapshot {
        crate::state::footer_metrics::GitSnapshot {
            probed: true,
            in_repo: true,
            branch: branch.map(str::to_string),
            short_sha: Some("abc123def456".to_string()),
            detached,
            linked_worktree: linked,
        }
    }

    #[test]
    fn worktree_diamond_is_nyan_blue() {
        let _fixture = InfoLineFixture::new("claude-sonnet-4-20250514");
        let mut metrics = crate::state::footer_metrics::FooterMetrics::default();
        metrics.git = git_snapshot(Some("master"), false, true);
        let canvas = render_info_line(store_with(metrics, None, None), 0, 120);
        let text = canvas.to_string();
        assert!(text.contains("◇"), "canvas=\n{text}");
        assert_eq!(
            style_at(&canvas, "◇"),
            crate::utils::theme::current().nyan_blue
        );
    }

    #[test]
    fn detached_head_shows_the_short_sha() {
        let _fixture = InfoLineFixture::new("claude-sonnet-4-20250514");
        let mut metrics = crate::state::footer_metrics::FooterMetrics::default();
        metrics.git = git_snapshot(None, true, false);
        let canvas = render_info_line(store_with(metrics, None, None), 0, 120);
        let text = canvas.to_string();
        assert!(text.contains("@abc123def456"), "canvas=\n{text}");
        assert!(!text.contains("master"), "canvas=\n{text}");
    }

    #[test]
    fn outside_a_repo_renders_no_branch_segment() {
        let _fixture = InfoLineFixture::new("claude-sonnet-4-20250514");
        let metrics = crate::state::footer_metrics::FooterMetrics::default();
        let canvas = render_info_line(store_with(metrics, None, None), 0, 120);
        let text = canvas.to_string();
        assert!(!text.contains("master"), "canvas=\n{text}");
        assert!(!text.contains('◇'), "canvas=\n{text}");
        assert!(text.contains("Sonnet 4"), "canvas=\n{text}");
    }

    #[test]
    fn effort_off_hides_the_effort_segment() {
        let _fixture = InfoLineFixture::new("claude-sonnet-4-20250514");
        let metrics = crate::state::footer_metrics::FooterMetrics::default();

        let on = render_info_line(
            store_with(
                metrics.clone(),
                Some(crate::utils::effort::Effort::High),
                None,
            ),
            0,
            120,
        )
        .to_string();
        assert!(on.contains(" high"), "canvas=\n{on}");

        let off = render_info_line(
            store_with(metrics, Some(crate::utils::effort::Effort::Off), None),
            0,
            120,
        );
        let text = off.to_string();
        assert!(!text.contains(" high"), "canvas=\n{text}");
        assert!(!text.contains(" off"), "canvas=\n{text}");
    }

    #[test]
    fn cache_segment_hides_until_the_session_reports_cache() {
        let _fixture = InfoLineFixture::new("claude-sonnet-4-20250514");
        let mut metrics = crate::state::footer_metrics::FooterMetrics::default();
        metrics.observe_cache("claude-sonnet-4-20250514", &usage(40, 10, 60, 0), 0.0, 0.0);

        let hidden = render_info_line(
            store_with(
                crate::state::footer_metrics::FooterMetrics::default(),
                None,
                None,
            ),
            0,
            120,
        )
        .to_string();
        assert!(!hidden.contains("cache"), "canvas=\n{hidden}");

        let shown = render_info_line(store_with(metrics.clone(), None, None), 0, 120);
        let text = shown.to_string();
        assert!(text.contains("cache 60%"), "canvas=\n{text}");
        assert_eq!(
            style_at(&shown, "cache 60%"),
            crate::utils::theme::current().inactive
        );

        metrics.cache.miss_warning = true;
        let warned = render_info_line(store_with(metrics, None, None), 0, 120);
        let text = warned.to_string();
        assert!(text.contains("cache 60%"), "canvas=\n{text}");
        assert_eq!(
            style_at(&warned, "cache 60%"),
            crate::utils::theme::current().warning
        );
    }

    #[test]
    fn context_segment_uses_the_two_level_colors() {
        let _fixture = InfoLineFixture::new("claude-sonnet-4-20250514");
        // 200k profile window, 20k reserved for the summary → effective 180k;
        // auto-compact threshold 167k, warning at 127k, error at 147k.
        let theme = *crate::utils::theme::current();
        let metrics = crate::state::footer_metrics::FooterMetrics::default();

        let calm = render_info_line(store_with(metrics.clone(), None, None), 84_000, 120);
        assert_eq!(style_at(&calm, "84k/200k"), theme.inactive);

        let warning = render_info_line(store_with(metrics.clone(), None, None), 132_000, 120);
        assert!(warning.to_string().contains("132k/200k"));
        assert_eq!(style_at(&warning, "132k/200k"), theme.warning);

        let error = render_info_line(store_with(metrics, None, None), 150_000, 120);
        assert!(error.to_string().contains("150k/200k"));
        assert_eq!(style_at(&error, "150k/200k"), theme.error);
    }

    #[test]
    fn tps_color_follows_the_generating_state() {
        let _fixture = InfoLineFixture::new("claude-sonnet-4-20250514");
        let theme = *crate::utils::theme::current();
        let mut metrics = crate::state::footer_metrics::FooterMetrics::default();
        metrics.current_turn = crate::state::footer_metrics::TpsTurn {
            output_tokens: 100,
            decode_ms: 2_000,
        };

        let mut generating = metrics.clone();
        generating.turn_open = true;
        let canvas = render_info_line(store_with(generating, None, None), 0, 120);
        assert!(canvas.to_string().contains("50 t/s"), "canvas=\n{}", canvas);
        assert_eq!(style_at(&canvas, "50 t/s"), theme.text);

        let idle = render_info_line(store_with(metrics, None, None), 0, 120);
        assert!(idle.to_string().contains("50 t/s"));
        assert_eq!(style_at(&idle, "50 t/s"), theme.inactive);
    }

    #[test]
    fn first_turn_without_a_sample_renders_no_tps() {
        let _fixture = InfoLineFixture::new("claude-sonnet-4-20250514");
        let metrics = crate::state::footer_metrics::FooterMetrics::default();
        let canvas = render_info_line(store_with(metrics, None, None), 0, 120);
        let text = canvas.to_string();
        assert!(!text.contains("t/s"), "canvas=\n{text}");
    }

    #[test]
    fn width_degradation_applies_on_the_rendered_canvas() {
        let _fixture = InfoLineFixture::new("moonshotai/kimi-k2-0905-preview");
        let mut metrics = crate::state::footer_metrics::FooterMetrics::default();
        metrics.git = git_snapshot(Some("feature/footer-info-line"), false, true);
        metrics.observe_cache(
            "moonshotai/kimi-k2-0905-preview",
            &usage(0, 10, 100, 0),
            0.0,
            0.0,
        );
        metrics.current_turn = crate::state::footer_metrics::TpsTurn {
            output_tokens: 100,
            decode_ms: 2_000,
        };

        let narrow = render_info_line(store_with(metrics, None, None), 84_000, 40);
        let text = narrow.to_string();
        assert!(text.contains("84k/200k"), "canvas=\n{text}");
        assert!(!text.contains("cache"), "canvas=\n{text}");
        assert!(!text.contains("t/s"), "canvas=\n{text}");
        for line in text.lines() {
            assert!(UnicodeWidthStr::width(line) <= 40, "line={line:?}");
        }
    }
}
