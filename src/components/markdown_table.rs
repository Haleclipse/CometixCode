//! Maps to: CC `components/MarkdownTable.tsx`.
//!
//! Official MarkdownTable receives a marked table token, formats each cell to
//! ANSI, computes terminal-aware column widths, and either renders a bordered
//! horizontal table or switches to a vertical key/value layout when wrapping
//! would make rows too tall. This Rust boundary receives the already-formatted
//! ANSI cell strings from `Markdown` (equivalent to CC's `formatToken(...)`)
//! and owns the table layout/wrapping/rendering responsibility.

use crate::components::markdown::{PERMISSION_COLOR_END, PERMISSION_COLOR_START};
use crate::constants::figures::figures;
use iocraft::prelude::*;
use marked_rs::Align;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Accounts for parent indentation and terminal resize races.
/// Maps to: CC `MarkdownTable.tsx#SAFETY_MARGIN`.
const SAFETY_MARGIN: usize = 4;

/// Maps to: CC `MarkdownTable.tsx#MIN_COLUMN_WIDTH`.
const MIN_COLUMN_WIDTH: usize = 3;

/// Maps to: CC `MarkdownTable.tsx#MAX_ROW_LINES`.
const MAX_ROW_LINES: usize = 4;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TableAlignment {
    #[default]
    None,
    Left,
    Center,
    Right,
}

impl From<Option<Align>> for TableAlignment {
    fn from(value: Option<Align>) -> Self {
        match value {
            Some(Align::Left) => Self::Left,
            Some(Align::Center) => Self::Center,
            Some(Align::Right) => Self::Right,
            None => Self::None,
        }
    }
}

#[derive(Default, Props)]
pub struct MarkdownTableProps {
    /// ANSI-formatted header cell contents. Maps to CC `token.header` after
    /// `formatToken(...)`.
    pub headers: Vec<String>,
    pub aligns: Vec<TableAlignment>,
    /// ANSI-formatted body rows. Maps to CC `token.rows` after
    /// `formatToken(...)`.
    pub rows: Vec<Vec<String>>,
    /// Maps to CC test-only `forceWidth` prop.
    pub force_width: Option<usize>,
}

/// Maps to: CC `components/MarkdownTable.tsx#MarkdownTable`.
#[component]
pub fn MarkdownTable(
    props: &MarkdownTableProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let (columns, _) = hooks.use_terminal_size();
    let terminal_width = props.force_width.unwrap_or(columns as usize).max(1);
    let text = render_markdown_table_text(
        props.headers.clone(),
        props.aligns.clone(),
        props.rows.clone(),
        terminal_width,
    );

    element! {
        Ansi(content: text, wrap: TextWrap::NoWrap)
    }
}

/// Maps to: CC `MarkdownTable` final `tableLines`/vertical string result.
pub fn render_markdown_table_text(
    headers: Vec<String>,
    aligns: Vec<TableAlignment>,
    rows: Vec<Vec<String>>,
    terminal_width: usize,
) -> String {
    render_markdown_table_lines(headers, aligns, rows, terminal_width).join("\n")
}

/// Maps to: CC `components/MarkdownTable.tsx#MarkdownTable` layout logic.
pub fn render_markdown_table_lines(
    headers: Vec<String>,
    aligns: Vec<TableAlignment>,
    rows: Vec<Vec<String>>,
    terminal_width: usize,
) -> Vec<String> {
    let column_count = headers
        .len()
        .max(rows.iter().map(Vec::len).max().unwrap_or(0));
    if column_count == 0 {
        return Vec::new();
    }

    MarkdownTableLayout::new(headers, aligns, rows, terminal_width.max(1)).render()
}

/// Pipe-table fallback used by `Markdown` text formatting.
/// Maps to: CC `utils/markdown.ts#padAligned` callers for non-component table
/// formatting.
pub fn format_table_token_pipe(
    headers: Vec<String>,
    aligns: Vec<TableAlignment>,
    rows: Vec<Vec<String>>,
) -> String {
    if headers.is_empty() {
        return String::new();
    }

    let mut widths = headers
        .iter()
        .map(|cell| display_width_ansi(cell).max(MIN_COLUMN_WIDTH))
        .collect::<Vec<_>>();
    for row in &rows {
        for (idx, cell) in row.iter().enumerate() {
            if idx >= widths.len() {
                widths.push(MIN_COLUMN_WIDTH);
            }
            widths[idx] = widths[idx].max(display_width_ansi(cell).max(MIN_COLUMN_WIDTH));
        }
    }

    let mut out = String::new();
    out.push_str("| ");
    for (idx, cell) in headers.iter().enumerate() {
        let width = widths[idx];
        let alignment = aligns.get(idx).copied().unwrap_or_default();
        out.push_str(&pad_aligned(
            cell,
            display_width_ansi(cell),
            width,
            alignment,
        ));
        out.push_str(" | ");
    }
    out = out.trim_end().to_string();
    out.push('\n');

    out.push('|');
    for width in &widths {
        out.push_str(&"-".repeat(width + 2));
        out.push('|');
    }
    out.push('\n');

    for row in rows {
        out.push_str("| ");
        for (idx, cell) in row.iter().enumerate() {
            let width = widths[idx];
            let alignment = aligns.get(idx).copied().unwrap_or_default();
            out.push_str(&pad_aligned(
                cell,
                display_width_ansi(cell),
                width,
                alignment,
            ));
            out.push_str(" | ");
        }
        out = out.trim_end().to_string();
        out.push('\n');
    }

    out.push('\n');
    out
}

#[derive(Clone, Debug)]
struct MarkdownTableLayout {
    headers: Vec<String>,
    aligns: Vec<TableAlignment>,
    rows: Vec<Vec<String>>,
    column_widths: Vec<usize>,
    terminal_width: usize,
    needs_hard_wrap: bool,
}

impl MarkdownTableLayout {
    fn new(
        mut headers: Vec<String>,
        mut aligns: Vec<TableAlignment>,
        mut rows: Vec<Vec<String>>,
        terminal_width: usize,
    ) -> Self {
        let column_count = headers
            .len()
            .max(rows.iter().map(Vec::len).max().unwrap_or(0));
        headers.resize(column_count, String::new());
        aligns.resize(column_count, TableAlignment::None);
        for row in &mut rows {
            row.resize(column_count, String::new());
        }

        let min_widths = (0..column_count)
            .map(|idx| {
                let mut width = min_cell_width(&headers[idx]);
                for row in &rows {
                    width = width.max(min_cell_width(&row[idx]));
                }
                width
            })
            .collect::<Vec<_>>();
        let ideal_widths = (0..column_count)
            .map(|idx| {
                let mut width = ideal_cell_width(&headers[idx]);
                for row in &rows {
                    width = width.max(ideal_cell_width(&row[idx]));
                }
                width
            })
            .collect::<Vec<_>>();

        let border_overhead = 1 + column_count * 3;
        let available_width = terminal_width
            .saturating_sub(border_overhead + SAFETY_MARGIN)
            .max(column_count * MIN_COLUMN_WIDTH);
        let total_min = min_widths.iter().sum::<usize>();
        let total_ideal = ideal_widths.iter().sum::<usize>();
        let mut needs_hard_wrap = false;

        let column_widths = if total_ideal <= available_width {
            ideal_widths
        } else if total_min <= available_width {
            let extra_space = available_width - total_min;
            let overflows = ideal_widths
                .iter()
                .zip(&min_widths)
                .map(|(ideal, min)| ideal.saturating_sub(*min))
                .collect::<Vec<_>>();
            let total_overflow = overflows.iter().sum::<usize>();
            min_widths
                .iter()
                .enumerate()
                .map(|(idx, min)| {
                    if total_overflow == 0 {
                        *min
                    } else {
                        min + (overflows[idx] * extra_space / total_overflow)
                    }
                })
                .collect()
        } else {
            needs_hard_wrap = true;
            min_widths
                .iter()
                .map(|width| ((*width * available_width) / total_min.max(1)).max(MIN_COLUMN_WIDTH))
                .collect()
        };

        Self {
            headers,
            aligns,
            rows,
            column_widths,
            terminal_width,
            needs_hard_wrap,
        }
    }

    fn render(&self) -> Vec<String> {
        if self.use_vertical_format() {
            return self.render_vertical_format();
        }

        let mut lines = Vec::new();
        lines.push(self.render_border_line(TableBorderKind::Top));
        lines.extend(self.render_row_lines(&self.headers, true));
        lines.push(self.render_border_line(TableBorderKind::Middle));
        for (idx, row) in self.rows.iter().enumerate() {
            lines.extend(self.render_row_lines(row, false));
            if idx + 1 < self.rows.len() {
                lines.push(self.render_border_line(TableBorderKind::Middle));
            }
        }
        lines.push(self.render_border_line(TableBorderKind::Bottom));

        let max_line_width = lines
            .iter()
            .map(|line| display_width_ansi(line.as_str()))
            .max()
            .unwrap_or(0);
        if max_line_width > self.terminal_width.saturating_sub(SAFETY_MARGIN) {
            self.render_vertical_format()
        } else {
            lines
        }
    }

    fn use_vertical_format(&self) -> bool {
        !self.rows.is_empty() && self.max_row_lines() > MAX_ROW_LINES
    }

    fn max_row_lines(&self) -> usize {
        let mut max_lines = self
            .headers
            .iter()
            .enumerate()
            .map(|(idx, cell)| {
                wrap_table_cell(cell, self.column_widths[idx], self.needs_hard_wrap).len()
            })
            .max()
            .unwrap_or(1);
        for row in &self.rows {
            for (idx, cell) in row.iter().enumerate() {
                max_lines = max_lines.max(
                    wrap_table_cell(cell, self.column_widths[idx], self.needs_hard_wrap).len(),
                );
            }
        }
        max_lines
    }

    fn render_row_lines(&self, cells: &[String], is_header: bool) -> Vec<String> {
        let cell_lines = cells
            .iter()
            .enumerate()
            .map(|(idx, cell)| wrap_table_cell(cell, self.column_widths[idx], self.needs_hard_wrap))
            .collect::<Vec<_>>();
        let max_lines = cell_lines.iter().map(Vec::len).max().unwrap_or(1).max(1);
        let offsets = cell_lines
            .iter()
            .map(|lines| (max_lines - lines.len()) / 2)
            .collect::<Vec<_>>();
        let figures = figures();

        let mut result = Vec::new();
        for line_idx in 0..max_lines {
            let mut line = String::from(figures.line_vertical);
            for col_idx in 0..cells.len() {
                let content_line_idx = line_idx as isize - offsets[col_idx] as isize;
                let content = if content_line_idx >= 0 {
                    cell_lines[col_idx]
                        .get(content_line_idx as usize)
                        .map(String::as_str)
                        .unwrap_or("")
                } else {
                    ""
                };
                let align = if is_header {
                    TableAlignment::Center
                } else {
                    self.aligns[col_idx]
                };
                line.push(' ');
                line.push_str(&pad_aligned(
                    content,
                    display_width_ansi(content),
                    self.column_widths[col_idx],
                    align,
                ));
                line.push(' ');
                line.push_str(figures.line_vertical);
            }
            result.push(line);
        }
        result
    }

    fn render_border_line(&self, kind: TableBorderKind) -> String {
        let figures = figures();
        let (left, middle, cross, right) = match kind {
            TableBorderKind::Top => (
                figures.line_down_right,
                figures.line,
                figures.line_down_left_right,
                figures.line_down_left,
            ),
            TableBorderKind::Middle => (
                figures.line_up_down_right,
                figures.line,
                figures.line_up_down_left_right,
                figures.line_up_down_left,
            ),
            TableBorderKind::Bottom => (
                figures.line_up_right,
                figures.line,
                figures.line_up_left_right,
                figures.line_up_left,
            ),
        };

        let mut line = String::from(left);
        for (idx, width) in self.column_widths.iter().enumerate() {
            line.push_str(&middle.repeat(width + 2));
            if idx + 1 < self.column_widths.len() {
                line.push_str(cross);
            } else {
                line.push_str(right);
            }
        }
        line
    }

    fn render_vertical_format(&self) -> Vec<String> {
        let separator_width = self.terminal_width.saturating_sub(1).min(40);
        let separator = figures().line.repeat(separator_width);
        let wrap_indent = "  ";
        let mut lines = Vec::new();

        for (row_idx, row) in self.rows.iter().enumerate() {
            if row_idx > 0 && !separator.is_empty() {
                lines.push(separator.clone());
            }

            for (col_idx, cell) in row.iter().enumerate() {
                let label = self
                    .headers
                    .get(col_idx)
                    .filter(|label| !label.is_empty())
                    .cloned()
                    .unwrap_or_else(|| format!("Column {}", col_idx + 1));
                let value = normalize_vertical_table_value(cell);
                let first_line_width = self
                    .terminal_width
                    .saturating_sub(display_width_ansi(label.as_str()) + 3);
                let subsequent_line_width =
                    self.terminal_width.saturating_sub(wrap_indent.len() + 1);
                let first_pass = wrap_table_cell(&value, first_line_width.max(10), false);
                let first_line = first_pass.first().cloned().unwrap_or_default();
                let wrapped_value =
                    if first_pass.len() <= 1 || subsequent_line_width <= first_line_width {
                        first_pass
                    } else {
                        let remaining = first_pass
                            .iter()
                            .skip(1)
                            .map(|line| line.trim())
                            .collect::<Vec<_>>()
                            .join(" ");
                        let mut next = vec![first_line];
                        next.extend(wrap_table_cell(&remaining, subsequent_line_width, false));
                        next
                    };

                lines.push(format!(
                    "\x1b[1m{}:\x1b[22m {}",
                    label,
                    wrapped_value.first().map(String::as_str).unwrap_or("")
                ));
                for line in wrapped_value.iter().skip(1) {
                    if !line.trim().is_empty() {
                        lines.push(format!("{wrap_indent}{line}"));
                    }
                }
            }
        }

        lines
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TableBorderKind {
    Top,
    Middle,
    Bottom,
}

fn min_cell_width(cell: &str) -> usize {
    let plain = strip_ansi_for_width(cell);
    plain
        .split_whitespace()
        .map(UnicodeWidthStr::width)
        .max()
        .unwrap_or(MIN_COLUMN_WIDTH)
        .max(MIN_COLUMN_WIDTH)
}

fn ideal_cell_width(cell: &str) -> usize {
    display_width_ansi(cell).max(MIN_COLUMN_WIDTH)
}

/// Maps to: CC `MarkdownTable.tsx#wrapText`.
pub(crate) fn wrap_table_cell(text: &str, width: usize, hard: bool) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }

    let text = text.trim_end();
    if text.is_empty() {
        return vec![String::new()];
    }

    let mut lines = Vec::new();
    for source_line in text.lines() {
        wrap_table_source_line(source_line, width, hard, &mut lines);
    }
    if lines.is_empty() {
        vec![String::new()]
    } else {
        lines
    }
}

fn wrap_table_source_line(source: &str, width: usize, hard: bool, lines: &mut Vec<String>) {
    let mut current = String::new();
    let mut current_width = 0usize;
    let mut active_sgr = String::new();
    let mut permission_color_active = false;

    for word in source.split_whitespace() {
        let word_width = display_width_ansi(word);
        if current_width == 0 {
            if hard && word_width > width {
                push_hard_wrapped_word(
                    word,
                    width,
                    lines,
                    &mut current,
                    &mut current_width,
                    &mut active_sgr,
                    &mut permission_color_active,
                );
            } else {
                current.push_str(word);
                current_width = word_width;
                update_active_style_from_text(&mut active_sgr, &mut permission_color_active, word);
            }
            continue;
        }

        if current_width + 1 + word_width <= width {
            current.push(' ');
            current.push_str(word);
            current_width += 1 + word_width;
            update_active_style_from_text(&mut active_sgr, &mut permission_color_active, word);
        } else {
            push_wrapped_table_line(
                lines,
                &mut current,
                &mut current_width,
                &active_sgr,
                permission_color_active,
            );
            if hard && word_width > width {
                push_hard_wrapped_word(
                    word,
                    width,
                    lines,
                    &mut current,
                    &mut current_width,
                    &mut active_sgr,
                    &mut permission_color_active,
                );
            } else {
                current.push_str(word);
                current_width = word_width;
                update_active_style_from_text(&mut active_sgr, &mut permission_color_active, word);
            }
        }
    }

    if current_width > 0 || source.is_empty() {
        lines.push(current);
    }
}

fn push_hard_wrapped_word(
    word: &str,
    width: usize,
    lines: &mut Vec<String>,
    current: &mut String,
    current_width: &mut usize,
    active_sgr: &mut String,
    permission_color_active: &mut bool,
) {
    let mut idx = 0;
    while idx < word.len() {
        if let Some(end) = ansi_escape_end(word, idx) {
            current.push_str(&word[idx..end]);
            update_active_style_from_text(active_sgr, permission_color_active, &word[idx..end]);
            idx = end;
            continue;
        }

        let Some(ch) = word[idx..].chars().next() else {
            break;
        };
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0).max(1);
        if *current_width > 0 && *current_width + ch_width > width {
            push_wrapped_table_line(
                lines,
                current,
                current_width,
                active_sgr,
                *permission_color_active,
            );
        }
        current.push(ch);
        *current_width += ch_width;
        idx += ch.len_utf8();
    }
}

fn push_wrapped_table_line(
    lines: &mut Vec<String>,
    current: &mut String,
    current_width: &mut usize,
    active_sgr: &str,
    permission_color_active: bool,
) {
    let mut line = std::mem::take(current).trim_end().to_string();
    if !active_sgr.is_empty() {
        line.push_str("\x1b[0m");
    }
    if permission_color_active {
        line.push_str(PERMISSION_COLOR_END);
    }
    lines.push(line);
    *current_width = 0;
    if permission_color_active {
        current.push_str(PERMISSION_COLOR_START);
    }
    if !active_sgr.is_empty() {
        current.push_str(active_sgr);
    }
}

fn update_active_style_from_text(
    active_sgr: &mut String,
    permission_color_active: &mut bool,
    text: &str,
) {
    let mut idx = 0;
    while idx < text.len() {
        let Some((end, params)) = csi_sgr_params(text, idx) else {
            if text[idx..].starts_with(PERMISSION_COLOR_START) {
                *permission_color_active = true;
                idx += PERMISSION_COLOR_START.len();
                continue;
            }
            if text[idx..].starts_with(PERMISSION_COLOR_END) {
                *permission_color_active = false;
                idx += PERMISSION_COLOR_END.len();
                continue;
            }
            idx += text[idx..].chars().next().map(char::len_utf8).unwrap_or(1);
            continue;
        };
        let sequence = &text[idx..end];
        if sgr_params_reset_style(params) {
            active_sgr.clear();
        } else if sgr_params_start_style(params) {
            active_sgr.push_str(sequence);
        }
        idx = end;
    }
}

fn csi_sgr_params(input: &str, start: usize) -> Option<(usize, &str)> {
    if input.as_bytes().get(start) != Some(&0x1b) || !input[start..].starts_with("\x1b[") {
        return None;
    }
    let rest = &input[start + 2..];
    let final_rel = rest.find(|ch: char| ('@'..='~').contains(&ch))?;
    let final_idx = start + 2 + final_rel;
    let final_char = input[final_idx..].chars().next()?;
    if final_char != 'm' {
        return None;
    }
    Some((
        final_idx + final_char.len_utf8(),
        &input[start + 2..final_idx],
    ))
}

fn sgr_params_reset_style(params: &str) -> bool {
    params.is_empty()
        || params
            .split(';')
            .filter(|part| !part.is_empty())
            .any(|part| matches!(part, "0" | "22" | "23" | "24" | "39" | "49"))
}

fn sgr_params_start_style(params: &str) -> bool {
    params
        .split(';')
        .filter_map(|part| part.parse::<u16>().ok())
        .any(|value| {
            matches!(
                value,
                1 | 2 | 3 | 4 | 30..=37 | 38 | 40..=47 | 48 | 90..=97 | 100..=107
            )
        })
}

/// Maps to: CC `utils/markdown.ts#padAligned`.
pub(crate) fn pad_aligned(
    content: &str,
    display_width: usize,
    target_width: usize,
    align: TableAlignment,
) -> String {
    let padding = target_width.saturating_sub(display_width);
    match align {
        TableAlignment::Center => {
            let left = padding / 2;
            format!(
                "{}{}{}",
                " ".repeat(left),
                content,
                " ".repeat(padding - left)
            )
        }
        TableAlignment::Right => format!("{}{}", " ".repeat(padding), content),
        TableAlignment::Left | TableAlignment::None => {
            format!("{}{}", content, " ".repeat(padding))
        }
    }
}

fn ansi_escape_end(input: &str, start: usize) -> Option<usize> {
    let rest = &input[start..];
    if !rest.starts_with('\x1b') {
        return None;
    }

    if rest.starts_with("\x1b[") {
        let final_rel = rest[2..].find(|ch: char| ('@'..='~').contains(&ch))?;
        let final_idx = start + 2 + final_rel;
        let final_char = input[final_idx..].chars().next()?;
        return Some(final_idx + final_char.len_utf8());
    }

    if rest.starts_with("\x1b]") {
        let body_start = start + 2;
        let bel_end = input[body_start..]
            .find('\x07')
            .map(|rel| body_start + rel + 1);
        let st_end = input[body_start..]
            .find("\x1b\\")
            .map(|rel| body_start + rel + 2);
        return match (bel_end, st_end) {
            (Some(bel), Some(st)) => Some(bel.min(st)),
            (Some(bel), None) => Some(bel),
            (None, Some(st)) => Some(st),
            (None, None) => None,
        };
    }

    let mut end = start + 1;
    if let Some(next) = input[end..].chars().next() {
        end += next.len_utf8();
        if matches!(next, '(' | ')' | '*' | '+')
            && let Some(designator) = input[end..].chars().next()
        {
            end += designator.len_utf8();
        }
    }
    Some(end)
}

pub(crate) fn strip_ansi_for_width(input: &str) -> String {
    let mut out = String::new();
    let mut idx = 0;
    while idx < input.len() {
        if let Some(end) = ansi_escape_end(input, idx) {
            idx = end;
            continue;
        }
        let Some(ch) = input[idx..].chars().next() else {
            break;
        };
        out.push(ch);
        idx += ch.len_utf8();
    }
    out
}

pub(crate) fn display_width_ansi(input: &str) -> usize {
    UnicodeWidthStr::width(strip_ansi_for_width(input).as_str())
}

fn normalize_vertical_table_value(text: &str) -> String {
    let mut out = String::new();
    let mut last_was_space = false;
    for ch in text.trim().chars() {
        if ch.is_whitespace() {
            if !last_was_space {
                out.push(' ');
                last_was_space = true;
            }
        } else {
            out.push(ch);
            last_was_space = false;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;

    #[test]
    fn markdown_table_renders_official_box_layout_with_figures() {
        let lines = render_markdown_table_lines(
            vec!["名称".to_string(), "Count".to_string()],
            vec![TableAlignment::None, TableAlignment::Right],
            vec![
                vec!["猫".to_string(), "2".to_string()],
                vec!["crates".to_string(), "10".to_string()],
            ],
            80,
        );

        assert_eq!(lines[0], "┌────────┬───────┐");
        assert_eq!(lines[2], "├────────┼───────┤");
        assert_eq!(lines.last().unwrap(), "└────────┴───────┘");
        assert!(lines[3].contains("猫"));
        assert!(lines[3].ends_with("    2 │"));
    }

    #[test]
    fn markdown_table_applies_left_center_right_alignment() {
        let lines = render_markdown_table_lines(
            vec!["L".to_string(), "C".to_string(), "R".to_string()],
            vec![
                TableAlignment::Left,
                TableAlignment::Center,
                TableAlignment::Right,
            ],
            vec![vec!["x".to_string(), "y".to_string(), "z".to_string()]],
            80,
        );
        assert_eq!(lines[1], "│  L  │  C  │  R  │");
        assert_eq!(lines[3], "│ x   │  y  │   z │");
    }

    #[test]
    fn markdown_table_switches_to_vertical_format_when_wrapping_is_too_tall() {
        let lines = render_markdown_table_lines(
            vec!["Key".to_string(), "Value".to_string()],
            vec![TableAlignment::None, TableAlignment::None],
            vec![vec![
                "A".to_string(),
                "one two three four five six seven eight nine ten".to_string(),
            ]],
            24,
        );
        assert_eq!(strip_ansi_for_width(&lines[0]), "Key: A");
        assert!(
            lines
                .iter()
                .any(|line| strip_ansi_for_width(line).starts_with("Value:"))
        );
        assert!(!lines.iter().any(|line| line.starts_with("┌")));
    }

    #[test]
    fn markdown_table_wrapping_preserves_ansi_style_across_lines() {
        let hard_wrapped = wrap_table_cell("\x1b[31mabcdef\x1b[0m", 3, true);
        assert_eq!(
            hard_wrapped
                .iter()
                .map(|line| strip_ansi_for_width(line))
                .collect::<Vec<_>>(),
            vec!["abc".to_string(), "def".to_string()]
        );
        assert!(hard_wrapped[0].starts_with("\x1b[31m"));
        assert!(hard_wrapped[0].ends_with("\x1b[0m"));
        assert!(hard_wrapped[1].starts_with("\x1b[31m"));
    }

    #[test]
    fn markdown_table_wrapping_preserves_permission_color_boundaries() {
        let source = format!("{PERMISSION_COLOR_START}foo bar baz{PERMISSION_COLOR_END}");
        let wrapped = wrap_table_cell(&source, 7, false);

        assert_eq!(wrapped.len(), 2);
        assert_eq!(strip_ansi_for_width(&wrapped[0]), "foo bar");
        assert_eq!(strip_ansi_for_width(&wrapped[1]), "baz");
        assert!(wrapped[0].ends_with(PERMISSION_COLOR_END));
        assert!(wrapped[1].starts_with(PERMISSION_COLOR_START));
        assert!(wrapped[1].ends_with(PERMISSION_COLOR_END));

        let table_lines = render_markdown_table_lines(
            vec!["Code".to_string(), "Other".to_string()],
            vec![TableAlignment::None, TableAlignment::None],
            vec![vec![source, "x".to_string()]],
            20,
        );
        let colored_rows = table_lines
            .iter()
            .filter(|line| {
                let plain = strip_ansi_for_width(line);
                plain.contains("foo") || plain.contains("baz")
            })
            .collect::<Vec<_>>();
        assert!(colored_rows.len() >= 2, "table={table_lines:?}");
        for line in colored_rows {
            let end_marker = line.find(PERMISSION_COLOR_END).unwrap();
            let right_border = line.rfind('│').unwrap();
            assert!(end_marker < right_border, "line={line:?}");
        }
    }

    #[test]
    fn markdown_table_component_renders_ansi_boundary() {
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                MarkdownTable(
                    headers: vec!["Col".to_string()],
                    aligns: vec![TableAlignment::None],
                    rows: vec![vec!["\x1b[31mred\x1b[0m".to_string()]],
                    force_width: Some(80usize),
                )
            }
        }
        .render(Some(80))
        .to_string();

        assert!(text.contains("red"), "canvas=\n{text}");
        assert!(text.contains("┌─────┐"), "canvas=\n{text}");
    }
}
