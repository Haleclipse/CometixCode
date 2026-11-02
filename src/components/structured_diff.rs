//! UI-only main-screen StructuredDiff renderer.
//! This ports the official `components/StructuredDiff.tsx` native color-diff
//! path where it is safe for terminal scrollback: recorded hunks are formatted
//! in memory, syntax-highlighted with `syntect` using `bat`'s bundled syntax
//! and theme assets (dark/light), plus an ANSI palette matching official
//! `color-diff-napi` `ANSI_SCOPES`, and word-diffed with `similar`. It does
//! not read files, execute tools, or enter RawAnsi/fullscreen/NoSelect branches.

pub mod color_diff;

use crate::components::messages::user_tool_result_message::utils::{
    ToolRenderBackground, ToolRenderLine, ToolRenderSegment, ToolRenderTone,
};
use crate::components::structured_diff::color_diff::SyntaxHighlightTheme;
use crate::types::message::StructuredDiffHunk;
use bat::assets::HighlightingAssets;
use iocraft::prelude::*;
use similar::{Algorithm, ChangeTag, capture_diff_slices};
use std::{collections::HashMap, path::Path, str::FromStr, sync::OnceLock};
use syntect::{
    easy::HighlightLines,
    highlighting::{
        Color as SyntectColor, ScopeSelectors, Style as SyntectStyle, StyleModifier,
        Theme as SyntectTheme, ThemeItem, ThemeSettings,
    },
    parsing::{SyntaxReference, SyntaxSet},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const CHANGE_THRESHOLD: f64 = 0.4;

#[derive(Clone, Debug)]
pub struct SyntaxHighlightOptions {
    pub file_path: Option<String>,
    pub first_line: Option<String>,
    pub theme: SyntaxHighlightTheme,
    pub prefix_content: Option<String>,
}

struct SyntaxHighlighter {
    inner: HighlightLines<'static>,
    syntax_set: &'static SyntaxSet,
}

impl SyntaxHighlighter {
    fn new(options: &SyntaxHighlightOptions) -> Option<Self> {
        let syntax_set = syntax_set();
        let syntax = detect_syntax(
            syntax_set,
            options.file_path.as_deref(),
            options.first_line.as_deref(),
        )?;
        let mut highlighter = Self {
            inner: HighlightLines::new(syntax, syntect_theme(options.theme)),
            syntax_set,
        };
        if let Some(prefix_content) = options
            .prefix_content
            .as_deref()
            .filter(|content| !content.is_empty())
        {
            highlighter.warm_prefix(prefix_content);
        }
        Some(highlighter)
    }

    fn warm_prefix(&mut self, prefix_content: &str) {
        for line in prefix_content.lines() {
            let line = format!("{line}\n");
            let _ = self.inner.highlight_line(&line, self.syntax_set);
        }
    }

    fn highlight_line(&mut self, code: &str) -> Vec<ToolRenderSegment> {
        let line = format!("{code}\n");
        let ranges = match self.inner.highlight_line(&line, self.syntax_set) {
            Ok(ranges) => ranges,
            Err(_) => return vec![ToolRenderSegment::new(code.to_string())],
        };
        let mut segments = Vec::new();
        for (style, text) in ranges {
            let text = text.trim_end_matches(['\r', '\n']);
            if text.is_empty() {
                continue;
            }
            push_segment_with_style(&mut segments, text, None, color_from_syntect_style(style));
        }
        if segments.is_empty() {
            vec![ToolRenderSegment::new(code.to_string())]
        } else {
            segments
        }
    }
}

fn syntax_set() -> &'static SyntaxSet {
    static SYNTAX_SET: OnceLock<SyntaxSet> = OnceLock::new();
    SYNTAX_SET.get_or_init(|| {
        let assets = HighlightingAssets::from_binary();
        assets
            .get_syntax_set()
            .cloned()
            .unwrap_or_else(|_| SyntaxSet::load_defaults_newlines())
    })
}

#[derive(Clone, Debug)]
struct BatThemeSet {
    dark: SyntectTheme,
    light: SyntectTheme,
    ansi: SyntectTheme,
}

fn bat_themes() -> &'static BatThemeSet {
    static THEMES: OnceLock<BatThemeSet> = OnceLock::new();
    THEMES.get_or_init(|| {
        let assets = HighlightingAssets::from_binary();
        BatThemeSet {
            dark: assets.get_theme("Monokai Extended").clone(),
            light: assets.get_theme("GitHub").clone(),
            // Stock bat `ansi` uses 0–7; official color-diff-napi uses bright 8–15.
            ansi: official_ansi_syntax_theme(),
        }
    })
}

/// Syntect theme matching `rebuild/packages/color-diff-napi` `ANSI_SCOPES`
/// (+ `foreground: ansiIdx(7)`). Scope names are TextMate/syntect equivalents
/// of the highlight.js kinds the TS port colors.
fn official_ansi_syntax_theme() -> SyntectTheme {
    // color-diff-napi/index.ts ANSI_SCOPES
    const KEYWORD: u8 = 13;
    const STORAGE: u8 = 14; // _storage / built_in / type
    const LITERAL: u8 = 12; // literal / number
    const STRING: u8 = 10;
    const TITLE: u8 = 11; // title / title.function / title.class
    const COMMENT: u8 = 8; // comment / meta
    const FOREGROUND: u8 = 7;

    fn ansi_idx(index: u8) -> SyntectColor {
        SyntectColor {
            r: index,
            g: 0,
            b: 0,
            a: 0,
        }
    }

    fn item(scopes: &str, index: u8) -> ThemeItem {
        ThemeItem {
            scope: ScopeSelectors::from_str(scopes).unwrap_or_else(|err| {
                panic!("invalid ANSI syntax scope selector {scopes:?}: {err:?}")
            }),
            style: StyleModifier {
                foreground: Some(ansi_idx(index)),
                background: None,
                font_style: None,
            },
        }
    }

    SyntectTheme {
        name: Some("claude-code-ansi".to_string()),
        author: Some("color-diff-napi ANSI_SCOPES".to_string()),
        settings: ThemeSettings {
            foreground: Some(ansi_idx(FOREGROUND)),
            // alpha=1 → terminal default (bat / color-diff-napi convention)
            background: Some(SyntectColor {
                r: 0,
                g: 0,
                b: 0,
                a: 1,
            }),
            ..ThemeSettings::default()
        },
        scopes: vec![
            item("comment, punctuation.definition.comment", COMMENT),
            item("meta.preprocessor, meta.annotation, annotation", COMMENT),
            item("keyword", KEYWORD),
            item("storage, storage.type, storage.modifier", STORAGE),
            item(
                "support.type, support.class, support.function, support.constant, support.module",
                STORAGE,
            ),
            item(
                "constant.numeric, constant.language, constant.character.escape, constant",
                LITERAL,
            ),
            item("string", STRING),
            item(
                "entity.name.function, entity.name.method, entity.name.class, entity.name.type, entity.name.type.class, entity.other.inherited-class, entity.name.section",
                TITLE,
            ),
        ],
    }
}

fn syntect_theme(theme: SyntaxHighlightTheme) -> &'static SyntectTheme {
    let bat = bat_themes();
    match theme {
        SyntaxHighlightTheme::Dark => &bat.dark,
        SyntaxHighlightTheme::Light => &bat.light,
        SyntaxHighlightTheme::Ansi => &bat.ansi,
        SyntaxHighlightTheme::Named(name) => named_syntect_theme(name),
    }
}

fn named_syntect_theme(name: &'static str) -> &'static SyntectTheme {
    static NAMED_THEMES: OnceLock<std::sync::Mutex<HashMap<&'static str, &'static SyntectTheme>>> =
        OnceLock::new();
    let mut themes = NAMED_THEMES
        .get_or_init(|| std::sync::Mutex::new(HashMap::new()))
        .lock()
        .expect("syntax theme cache mutex should not be poisoned");
    if let Some(theme) = themes.get(name) {
        return theme;
    }
    let assets = HighlightingAssets::from_binary();
    let theme = Box::leak(Box::new(assets.get_theme(name).clone()));
    themes.insert(name, theme);
    theme
}

fn detect_syntax(
    syntax_set: &'static SyntaxSet,
    file_path: Option<&str>,
    first_line: Option<&str>,
) -> Option<&'static SyntaxReference> {
    if let Some(path) = file_path {
        if let Ok(Some(syntax)) = syntax_set.find_syntax_for_file(Path::new(path)) {
            return Some(syntax);
        }
        if let Some(extension) = Path::new(path).extension().and_then(|value| value.to_str())
            && let Some(syntax) = syntax_set.find_syntax_by_extension(extension)
        {
            return Some(syntax);
        }
    }
    if let Some(line) = first_line {
        let line = line.trim_start_matches('\u{feff}');
        if line.starts_with("#!")
            && let Some(syntax) = syntax_set.find_syntax_by_first_line(line)
        {
            return Some(syntax);
        }
    }
    None
}

fn color_from_syntect_style(style: SyntectStyle) -> Option<Color> {
    let color = style.foreground;
    match color.a {
        // bat / color-diff-napi ANSI themes encode palette indexes as `r` with
        // alpha 0 (including bright 8–15 used by official ANSI_SCOPES).
        0 => Some(Color::AnsiValue(color.r)),
        // bat/syntect use alpha 1 as a terminal-default sentinel.
        1 => None,
        _ => Some(Color::Rgb {
            r: color.r,
            g: color.g,
            b: color.b,
        }),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DiffLineKind {
    Add,
    Remove,
    NoChange,
}

#[derive(Clone, Debug)]
struct LineObject {
    code: String,
    kind: DiffLineKind,
    original_code: String,
    matched_original_code: Option<String>,
}

#[derive(Clone, Debug)]
struct NumberedDiffLine {
    code: String,
    kind: DiffLineKind,
    line_number: usize,
    original_code: String,
    matched_original_code: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WordDiffKind {
    Common,
    Added,
    Removed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WordDiffPart {
    kind: WordDiffKind,
    value: String,
}

#[derive(Clone, Debug)]
pub struct ColorDiff {
    hunk: StructuredDiffHunk,
    first_line: Option<String>,
    file_path: String,
    prefix_content: Option<String>,
}

impl ColorDiff {
    pub fn new(
        hunk: StructuredDiffHunk,
        first_line: Option<String>,
        file_path: impl Into<String>,
        prefix_content: Option<String>,
    ) -> Self {
        Self {
            hunk,
            first_line,
            file_path: file_path.into(),
            prefix_content,
        }
    }

    pub fn render(
        &self,
        theme: SyntaxHighlightTheme,
        width: usize,
        dim: bool,
    ) -> Vec<ToolRenderLine> {
        let syntax = SyntaxHighlightOptions {
            file_path: (!self.file_path.is_empty()).then(|| self.file_path.clone()),
            first_line: self.first_line.clone(),
            theme,
            prefix_content: self.prefix_content.clone(),
        };
        render_hunk(&self.hunk, dim, width.max(1), Some(&syntax))
    }
}

#[derive(Clone, Debug)]
pub struct ColorFile {
    code: String,
    file_path: String,
}

impl ColorFile {
    pub fn new(code: impl Into<String>, file_path: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            file_path: file_path.into(),
        }
    }

    pub fn render(
        &self,
        theme: SyntaxHighlightTheme,
        width: usize,
        dim: bool,
    ) -> Vec<ToolRenderLine> {
        let lines = split_code_lines(&self.code);
        if lines.is_empty() {
            return Vec::new();
        }
        let syntax = SyntaxHighlightOptions {
            file_path: (!self.file_path.is_empty()).then(|| self.file_path.clone()),
            first_line: lines.first().cloned(),
            theme,
            prefix_content: None,
        };
        let mut highlighter = SyntaxHighlighter::new(&syntax);
        let max_digits = lines.len().to_string().len();
        let content_width = width.saturating_sub(max_digits + 2).max(1);

        lines
            .iter()
            .enumerate()
            .flat_map(|(line_idx, code)| {
                let content_segments = if let Some(highlighter) = highlighter.as_mut() {
                    highlighter.highlight_line(code)
                } else {
                    vec![ToolRenderSegment::new(code.clone())]
                };
                wrap_segments(&content_segments, content_width)
                    .into_iter()
                    .enumerate()
                    .map(move |(wrap_idx, content)| {
                        let prefix = if wrap_idx == 0 {
                            format!(" {:>width$} ", line_idx + 1, width = max_digits)
                        } else {
                            " ".repeat(max_digits + 2)
                        };
                        let mut all_segments = vec![ToolRenderSegment::new(prefix)];
                        all_segments.extend(content);
                        ToolRenderLine::new(segments_text(&all_segments), ToolRenderTone::Normal)
                            .with_dim(dim)
                            .with_segments(all_segments)
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }
}

fn split_code_lines(code: &str) -> Vec<String> {
    let mut lines = code.split('\n').map(str::to_string).collect::<Vec<_>>();
    if lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    lines
}

pub fn render_hunks(hunks: &[StructuredDiffHunk], dim: bool, width: usize) -> Vec<ToolRenderLine> {
    render_hunks_inner(hunks, dim, width, None)
}

pub fn render_hunks_with_syntax(
    hunks: &[StructuredDiffHunk],
    dim: bool,
    width: usize,
    syntax: SyntaxHighlightOptions,
) -> Vec<ToolRenderLine> {
    let safe_width = width.max(1);
    let mut rendered = Vec::new();
    for (idx, hunk) in hunks.iter().enumerate() {
        if idx > 0 {
            rendered.push(ToolRenderLine::new("...", ToolRenderTone::Inactive));
        }
        rendered.extend(
            ColorDiff::new(
                hunk.clone(),
                syntax.first_line.clone(),
                syntax.file_path.clone().unwrap_or_default(),
                syntax.prefix_content.clone(),
            )
            .render(syntax.theme, safe_width, dim),
        );
    }
    rendered
}

fn render_hunks_inner(
    hunks: &[StructuredDiffHunk],
    dim: bool,
    width: usize,
    syntax: Option<&SyntaxHighlightOptions>,
) -> Vec<ToolRenderLine> {
    let safe_width = width.max(1);
    let mut rendered = Vec::new();

    for (idx, hunk) in hunks.iter().enumerate() {
        if idx > 0 {
            rendered.push(ToolRenderLine::new("...", ToolRenderTone::Inactive));
        }
        rendered.extend(render_hunk(hunk, dim, safe_width, syntax));
    }

    rendered
}

pub fn render_preformatted_lines(lines: &[String], dim: bool, width: usize) -> Vec<ToolRenderLine> {
    render_preformatted_lines_inner(lines, dim, width, None)
}

pub fn render_preformatted_lines_with_syntax(
    lines: &[String],
    dim: bool,
    width: usize,
    syntax: SyntaxHighlightOptions,
) -> Vec<ToolRenderLine> {
    render_preformatted_lines_inner(lines, dim, width, Some(&syntax))
}

fn render_preformatted_lines_inner(
    lines: &[String],
    dim: bool,
    width: usize,
    syntax: Option<&SyntaxHighlightOptions>,
) -> Vec<ToolRenderLine> {
    let safe_width = width.max(1);
    let mut rendered = Vec::new();
    let mut chunk = Vec::new();

    for line in lines {
        if line == "..." {
            if !chunk.is_empty() {
                rendered.extend(render_hunk(
                    &preformatted_hunk(&chunk),
                    dim,
                    safe_width,
                    syntax,
                ));
                chunk.clear();
            }
            rendered.push(ToolRenderLine::new("...", ToolRenderTone::Inactive));
        } else {
            chunk.push(strip_preformatted_gutter(line));
        }
    }

    if !chunk.is_empty() {
        rendered.extend(render_hunk(
            &preformatted_hunk(&chunk),
            dim,
            safe_width,
            syntax,
        ));
    }

    rendered
}

fn preformatted_hunk(lines: &[String]) -> StructuredDiffHunk {
    StructuredDiffHunk {
        old_start: 1,
        old_lines: 0,
        new_start: 1,
        new_lines: 0,
        lines: lines.to_vec(),
    }
}

fn render_hunk(
    hunk: &StructuredDiffHunk,
    dim: bool,
    width: usize,
    syntax: Option<&SyntaxHighlightOptions>,
) -> Vec<ToolRenderLine> {
    let objects = process_adjacent_lines(transform_lines_to_objects(&hunk.lines));
    let numbered = number_diff_lines(objects, hunk.old_start.max(1), hunk.new_start.max(1));
    let max_line_number = hunk_max_line_number(hunk, &numbered);
    let max_width = max_line_number.to_string().len() + 1;

    let ansi_delete_content_dim =
        syntax.is_some_and(|options| options.theme == SyntaxHighlightTheme::Ansi);
    let mut highlighter = syntax.and_then(SyntaxHighlighter::new);
    numbered
        .into_iter()
        .flat_map(|line| {
            render_numbered_line(
                line,
                max_width,
                dim,
                width,
                &mut highlighter,
                ansi_delete_content_dim,
            )
        })
        .collect()
}

fn transform_lines_to_objects(lines: &[String]) -> Vec<LineObject> {
    lines
        .iter()
        .map(|line| {
            let (kind, code) = split_diff_line(line);
            LineObject {
                code: code.to_string(),
                kind,
                original_code: code.to_string(),
                matched_original_code: None,
            }
        })
        .collect()
}

fn process_adjacent_lines(mut lines: Vec<LineObject>) -> Vec<LineObject> {
    let mut i = 0usize;
    while i < lines.len() {
        if lines[i].kind != DiffLineKind::Remove {
            i += 1;
            continue;
        }

        let remove_start = i;
        let mut add_start = i;
        while add_start < lines.len() && lines[add_start].kind == DiffLineKind::Remove {
            add_start += 1;
        }

        let mut add_end = add_start;
        while add_end < lines.len() && lines[add_end].kind == DiffLineKind::Add {
            add_end += 1;
        }

        if add_start > remove_start && add_end > add_start {
            let pair_count = (add_start - remove_start).min(add_end - add_start);
            for offset in 0..pair_count {
                let remove_idx = remove_start + offset;
                let add_idx = add_start + offset;
                let remove_code = lines[remove_idx].original_code.clone();
                let add_code = lines[add_idx].original_code.clone();
                lines[remove_idx].matched_original_code = Some(add_code);
                lines[add_idx].matched_original_code = Some(remove_code);
            }
            i = add_end;
        } else {
            i += 1;
        }
    }

    lines
}

fn hunk_max_line_number(hunk: &StructuredDiffHunk, numbered: &[NumberedDiffLine]) -> usize {
    let old_end = if hunk.old_lines == 0 {
        0
    } else {
        hunk.old_start
            .max(1)
            .saturating_add(hunk.old_lines.saturating_sub(1))
    };
    let new_end = if hunk.new_lines == 0 {
        0
    } else {
        hunk.new_start
            .max(1)
            .saturating_add(hunk.new_lines.saturating_sub(1))
    };
    let rendered_max = numbered
        .iter()
        .map(|line| line.line_number)
        .max()
        .unwrap_or(0);

    old_end.max(new_end).max(rendered_max)
}

fn number_diff_lines(
    lines: Vec<LineObject>,
    old_start_line: usize,
    new_start_line: usize,
) -> Vec<NumberedDiffLine> {
    let mut old_line = old_start_line;
    let mut new_line = new_start_line;
    let mut result = Vec::new();

    for current in lines {
        let line_number = match current.kind {
            DiffLineKind::Add => {
                let line_number = new_line;
                new_line = new_line.saturating_add(1);
                line_number
            }
            DiffLineKind::Remove => {
                let line_number = old_line;
                old_line = old_line.saturating_add(1);
                line_number
            }
            DiffLineKind::NoChange => {
                let line_number = new_line;
                old_line = old_line.saturating_add(1);
                new_line = new_line.saturating_add(1);
                line_number
            }
        };

        result.push(NumberedDiffLine {
            code: current.code,
            kind: current.kind,
            line_number,
            original_code: current.original_code,
            matched_original_code: current.matched_original_code,
        });
    }

    result
}

fn render_numbered_line(
    line: NumberedDiffLine,
    max_width: usize,
    dim: bool,
    width: usize,
    highlighter: &mut Option<SyntaxHighlighter>,
    ansi_delete_content_dim: bool,
) -> Vec<ToolRenderLine> {
    if !dim
        && line.matched_original_code.is_some()
        && let Some(lines) = render_word_diff_line(
            &line,
            max_width,
            width,
            highlighter,
            ansi_delete_content_dim,
        )
    {
        return lines;
    }

    render_standard_line(
        &line,
        max_width,
        dim,
        width,
        highlighter,
        ansi_delete_content_dim,
    )
}

fn render_word_diff_line(
    line: &NumberedDiffLine,
    max_width: usize,
    width: usize,
    highlighter: &mut Option<SyntaxHighlighter>,
    ansi_delete_content_dim: bool,
) -> Option<Vec<ToolRenderLine>> {
    let matched = line.matched_original_code.as_ref()?;
    let (removed_text, added_text) = match line.kind {
        DiffLineKind::Remove => (&line.original_code, matched),
        DiffLineKind::Add => (matched, &line.original_code),
        DiffLineKind::NoChange => return None,
    };

    let word_diffs = calculate_word_diffs(removed_text, added_text);
    let total_len = removed_text.len() + added_text.len();
    if total_len == 0 {
        return None;
    }
    let changed_len: usize = word_diffs
        .iter()
        .filter(|part| matches!(part.kind, WordDiffKind::Added | WordDiffKind::Removed))
        .map(|part| part.value.len())
        .sum();
    if (changed_len as f64 / total_len as f64) > CHANGE_THRESHOLD {
        return None;
    }

    let content_width = available_content_width(width, max_width, 1);
    let mut content_segments = word_diffs
        .into_iter()
        .filter_map(|part| segment_for_line_kind(part, line.kind))
        .collect::<Vec<_>>();
    if line.kind == DiffLineKind::Add
        && let Some(highlighter) = highlighter.as_mut()
    {
        let syntax_segments = highlighter.highlight_line(&line.original_code);
        content_segments = apply_syntax_foreground(content_segments, &syntax_segments);
    }
    let wrapped = wrap_segments(&content_segments, content_width);

    Some(
        wrapped
            .into_iter()
            .enumerate()
            .map(|(idx, segments)| {
                let prefix = gutter_prefix(line.line_number, max_width, line.kind, idx == 0);
                let dim_content =
                    should_dim_ansi_deleted_content(line.kind, ansi_delete_content_dim);
                let segments = segments_with_dim(segments, dim_content);
                let content_width = segments_width(&segments);
                let mut all_segments = vec![ToolRenderSegment::new(prefix.clone())];
                all_segments.extend(segments);
                all_segments.push(
                    ToolRenderSegment::new(padding_for(width, prefix.width() + content_width))
                        .with_dim(dim_content),
                );

                ToolRenderLine::new(segments_text(&all_segments), ToolRenderTone::Normal)
                    .with_background(line_background(line.kind).expect("word diff line background"))
                    .with_segments(all_segments)
            })
            .collect(),
    )
}

fn render_standard_line(
    line: &NumberedDiffLine,
    max_width: usize,
    dim: bool,
    width: usize,
    highlighter: &mut Option<SyntaxHighlighter>,
    ansi_delete_content_dim: bool,
) -> Vec<ToolRenderLine> {
    let content_width = available_content_width(width, max_width, 1);
    let content_segments = if line.kind == DiffLineKind::Remove {
        vec![ToolRenderSegment::new(line.code.clone())]
    } else if let Some(highlighter) = highlighter.as_mut() {
        highlighter.highlight_line(&line.code)
    } else {
        vec![ToolRenderSegment::new(line.code.clone())]
    };
    let wrapped = wrap_segments(&content_segments, content_width);

    wrapped
        .into_iter()
        .enumerate()
        .map(|(idx, content)| {
            let prefix = gutter_prefix(line.line_number, max_width, line.kind, idx == 0);
            let dim_content = should_dim_ansi_deleted_content(line.kind, ansi_delete_content_dim);
            let content = segments_with_dim(content, dim_content);
            let content_width = segments_width(&content);
            let mut all_segments = vec![ToolRenderSegment::new(prefix.clone())];
            all_segments.extend(content);
            all_segments.push(
                ToolRenderSegment::new(padding_for(width, prefix.width() + content_width))
                    .with_dim(dim_content),
            );
            let mut render_line =
                ToolRenderLine::new(segments_text(&all_segments), ToolRenderTone::Normal)
                    .with_segments(all_segments);
            if let Some(background) = line_background(line.kind) {
                render_line = render_line.with_background(if dim {
                    dimmed_background(background)
                } else {
                    background
                });
            }
            if dim || line.kind == DiffLineKind::NoChange {
                render_line = render_line.with_dim(true);
            }
            render_line
        })
        .collect()
}

fn should_dim_ansi_deleted_content(kind: DiffLineKind, ansi_delete_content_dim: bool) -> bool {
    ansi_delete_content_dim && kind == DiffLineKind::Remove
}

fn segments_with_dim(mut segments: Vec<ToolRenderSegment>, dim: bool) -> Vec<ToolRenderSegment> {
    if dim {
        for segment in &mut segments {
            segment.dim = true;
        }
    }
    segments
}

fn segment_for_line_kind(part: WordDiffPart, line_kind: DiffLineKind) -> Option<ToolRenderSegment> {
    match (line_kind, part.kind) {
        (DiffLineKind::Add, WordDiffKind::Added) => Some(
            ToolRenderSegment::new(part.value).with_background(ToolRenderBackground::DiffAddedWord),
        ),
        (DiffLineKind::Add, WordDiffKind::Common) => Some(ToolRenderSegment::new(part.value)),
        (DiffLineKind::Remove, WordDiffKind::Removed) => Some(
            ToolRenderSegment::new(part.value)
                .with_background(ToolRenderBackground::DiffRemovedWord),
        ),
        (DiffLineKind::Remove, WordDiffKind::Common) => Some(ToolRenderSegment::new(part.value)),
        _ => None,
    }
}

fn calculate_word_diffs(old_text: &str, new_text: &str) -> Vec<WordDiffPart> {
    let old_tokens = tokenize_words_with_space(old_text);
    let new_tokens = tokenize_words_with_space(new_text);
    let ops = capture_diff_slices(Algorithm::Myers, &old_tokens, &new_tokens);

    let mut parts = Vec::new();
    for op in &ops {
        for (tag, tokens) in op.iter_slices(&old_tokens[..], &new_tokens[..]) {
            let value = tokens.concat();
            let kind = match tag {
                ChangeTag::Equal => WordDiffKind::Common,
                ChangeTag::Delete => WordDiffKind::Removed,
                ChangeTag::Insert => WordDiffKind::Added,
            };
            push_word_part(&mut parts, kind, &value);
        }
    }

    parts
}

fn push_word_part(parts: &mut Vec<WordDiffPart>, kind: WordDiffKind, value: &str) {
    if value.is_empty() {
        return;
    }
    if let Some(last) = parts.last_mut()
        && last.kind == kind
    {
        last.value.push_str(value);
        return;
    }
    parts.push(WordDiffPart {
        kind,
        value: value.to_string(),
    });
}

fn tokenize_words_with_space(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut iter = text.char_indices().peekable();

    while let Some((start, ch)) = iter.next() {
        if is_word_token_char(ch) {
            let mut end = start + ch.len_utf8();
            while let Some(&(idx, next)) = iter.peek() {
                if !is_word_token_char(next) {
                    break;
                }
                iter.next();
                end = idx + next.len_utf8();
            }
            tokens.push(text[start..end].to_string());
        } else if ch.is_whitespace() {
            let mut end = start + ch.len_utf8();
            while let Some(&(idx, next)) = iter.peek() {
                if !next.is_whitespace() {
                    break;
                }
                iter.next();
                end = idx + next.len_utf8();
            }
            tokens.push(text[start..end].to_string());
        } else {
            tokens.push(ch.to_string());
        }
    }

    tokens
}

fn is_word_token_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

fn wrap_segments(segments: &[ToolRenderSegment], width: usize) -> Vec<Vec<ToolRenderSegment>> {
    let safe_width = width.max(1);
    let mut lines: Vec<Vec<ToolRenderSegment>> = vec![Vec::new()];
    let mut current_width = 0usize;

    for segment in segments {
        let mut chunk = String::new();
        let mut chunk_width = 0usize;
        for ch in segment.text.chars() {
            let ch_width = ch.width().unwrap_or(0);
            if current_width > 0 && current_width + ch_width > safe_width {
                if !chunk.is_empty() {
                    push_segment_with_style(
                        lines.last_mut().expect("current line"),
                        &chunk,
                        segment.background,
                        segment.foreground,
                    );
                    chunk.clear();
                    chunk_width = 0;
                }
                lines.push(Vec::new());
                current_width = 0;
            }
            chunk.push(ch);
            chunk_width += ch_width;
            current_width += ch_width;
        }
        if !chunk.is_empty() {
            push_segment_with_style(
                lines.last_mut().expect("current line"),
                &chunk,
                segment.background,
                segment.foreground,
            );
        }
        if chunk_width == 0 && segment.text.is_empty() {
            push_segment_with_style(
                lines.last_mut().expect("current line"),
                "",
                segment.background,
                segment.foreground,
            );
        }
    }

    if lines.last().is_some_and(|line| line.is_empty()) && lines.len() > 1 {
        lines.pop();
    }
    if lines.is_empty() {
        vec![Vec::new()]
    } else {
        lines
    }
}

fn push_segment_with_style(
    line: &mut Vec<ToolRenderSegment>,
    text: &str,
    background: Option<ToolRenderBackground>,
    foreground: Option<Color>,
) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = line.last_mut()
        && last.background == background
        && last.foreground == foreground
    {
        last.text.push_str(text);
        return;
    }
    let mut segment = ToolRenderSegment::new(text.to_string());
    segment.background = background;
    segment.foreground = foreground;
    line.push(segment);
}

fn apply_syntax_foreground(
    segments: Vec<ToolRenderSegment>,
    syntax_segments: &[ToolRenderSegment],
) -> Vec<ToolRenderSegment> {
    let syntax_foregrounds = syntax_segments
        .iter()
        .flat_map(|segment| segment.text.chars().map(move |_| segment.foreground))
        .collect::<Vec<_>>();
    let mut offset = 0usize;
    let mut out = Vec::new();
    for segment in segments {
        for ch in segment.text.chars() {
            let foreground = syntax_foregrounds.get(offset).copied().flatten();
            push_segment_with_style(&mut out, &ch.to_string(), segment.background, foreground);
            offset += 1;
        }
    }
    out
}

fn gutter_prefix(
    line_number: usize,
    max_width: usize,
    kind: DiffLineKind,
    show_number: bool,
) -> String {
    let number = if show_number {
        line_number.to_string()
    } else {
        String::new()
    };
    let line_num_str = format!("{:>width$} ", number, width = max_width);
    let sigil = match kind {
        DiffLineKind::Add => '+',
        DiffLineKind::Remove => '-',
        DiffLineKind::NoChange => ' ',
    };
    format!("{line_num_str}{sigil}")
}

fn available_content_width(width: usize, max_width: usize, diff_prefix_width: usize) -> usize {
    width
        .saturating_sub(max_width)
        .saturating_sub(1)
        .saturating_sub(diff_prefix_width)
        .max(1)
}

fn padding_for(width: usize, used_width: usize) -> String {
    " ".repeat(width.saturating_sub(used_width))
}

fn segments_text(segments: &[ToolRenderSegment]) -> String {
    segments
        .iter()
        .map(|segment| segment.text.as_str())
        .collect()
}

fn segments_width(segments: &[ToolRenderSegment]) -> usize {
    segments
        .iter()
        .map(|segment| segment.text.as_str().width())
        .sum()
}

fn line_background(kind: DiffLineKind) -> Option<ToolRenderBackground> {
    match kind {
        DiffLineKind::Add => Some(ToolRenderBackground::DiffAdded),
        DiffLineKind::Remove => Some(ToolRenderBackground::DiffRemoved),
        DiffLineKind::NoChange => None,
    }
}

fn dimmed_background(background: ToolRenderBackground) -> ToolRenderBackground {
    background
}

fn split_diff_line(line: &str) -> (DiffLineKind, &str) {
    match line.chars().next() {
        Some('+') => (DiffLineKind::Add, &line[1..]),
        Some('-') => (DiffLineKind::Remove, &line[1..]),
        Some(' ') => (DiffLineKind::NoChange, &line[1..]),
        _ => (DiffLineKind::NoChange, line),
    }
}

fn strip_preformatted_gutter(line: &str) -> String {
    let trimmed = line.trim_start();
    let digit_count = trimmed.chars().take_while(|ch| ch.is_ascii_digit()).count();
    if digit_count > 0 {
        let after_digits = &trimmed[digit_count..];
        let after_space = after_digits.trim_start();
        if let Some(sigil @ ('+' | '-' | ' ')) = after_space.chars().next() {
            return after_space
                .strip_prefix(sigil)
                .map(|rest| format!("{sigil}{rest}"))
                .unwrap_or_else(|| line.to_string());
        }
    }

    line.to_string()
}

fn component_tone_color(theme: crate::utils::theme::Theme, tone: ToolRenderTone) -> Option<Color> {
    match tone {
        ToolRenderTone::Normal => None,
        ToolRenderTone::Success => Some(theme.success),
        ToolRenderTone::Warning => Some(theme.warning),
        ToolRenderTone::Error => Some(theme.error),
        ToolRenderTone::Inactive => Some(theme.inactive),
    }
}

fn component_background_color(
    theme: crate::utils::theme::Theme,
    background: ToolRenderBackground,
) -> Color {
    match background {
        ToolRenderBackground::DiffAdded => theme.diff_added,
        ToolRenderBackground::DiffRemoved => theme.diff_removed,
        ToolRenderBackground::DiffAddedWord => theme.diff_added_word,
        ToolRenderBackground::DiffRemovedWord => theme.diff_removed_word,
    }
}

#[derive(Default, Props)]
pub struct StructuredDiffProps {
    pub patch: StructuredDiffHunk,
    pub dim: bool,
    pub file_path: String,
    pub first_line: Option<String>,
    pub file_content: Option<String>,
    pub width: usize,
    pub skip_highlighting: bool,
}

/// Maps to: CC `components/StructuredDiff.tsx:118-186` `StructuredDiff`.
///
/// CC's native module returns ANSI rows and uses `RawAnsi`; the Rust port owns
/// the color-diff implementation directly and projects its typed row/segment
/// styles into iocraft `Text` leaves. The official safe-width and fallback
/// behavior are preserved without crossing a NAPI boundary.
#[component]
pub fn StructuredDiff(
    props: &StructuredDiffProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<crate::utils::theme::Theme>();
    let syntax_highlighting_disabled =
        crate::state::app_state::use_app_state(&mut hooks, |state| {
            state.settings.syntax_highlighting_disabled.unwrap_or(false)
        });
    let safe_width = props.width.max(1);
    let lines =
        if props.skip_highlighting || syntax_highlighting_disabled || props.file_path.is_empty() {
            render_hunks(std::slice::from_ref(&props.patch), props.dim, safe_width)
        } else {
            render_hunks_with_syntax(
                std::slice::from_ref(&props.patch),
                props.dim,
                safe_width,
                SyntaxHighlightOptions {
                    file_path: Some(props.file_path.clone()),
                    first_line: props.first_line.clone(),
                    theme: SyntaxHighlightTheme::from_theme(*theme),
                    prefix_content: props.file_content.clone(),
                },
            )
        };

    element! {
        View(flex_direction: FlexDirection::Column, width: 100pct) {
            #(lines.into_iter().map(|line| {
                let line_color = component_tone_color(*theme, line.tone);
                let line_background = line
                    .background
                    .map(|background| component_background_color(*theme, background));
                let line_dim = line.dim;
                if line.segments.is_empty() {
                    element! {
                        Text(
                            content: line.text,
                            color: line_color,
                            background_color: line_background,
                            dim: line_dim,
                            wrap: TextWrap::NoWrap,
                        )
                    }
                    .into_any()
                } else {
                    element! {
                        View(flex_direction: FlexDirection::Row) {
                            #(line.segments.into_iter().map(|segment| {
                                let background = segment
                                    .background
                                    .map(|background| component_background_color(*theme, background))
                                    .or(line_background);
                                element! {
                                    Text(
                                        content: segment.text,
                                        color: segment.foreground.or(line_color),
                                        background_color: background,
                                        dim: line_dim || segment.dim,
                                        wrap: TextWrap::NoWrap,
                                    )
                                }
                            }))
                        }
                    }
                    .into_any()
                }
            }).collect::<Vec<_>>())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structured_diff_pairs_adjacent_remove_add_lines_for_word_highlight() {
        let hunk = StructuredDiffHunk {
            old_start: 10,
            old_lines: 1,
            new_start: 10,
            new_lines: 1,
            lines: vec![
                "-function oldName(param) {".to_string(),
                "+function newName(param) {".to_string(),
            ],
        };

        let lines = render_hunks(&[hunk], false, 80);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].background, Some(ToolRenderBackground::DiffRemoved));
        assert_eq!(lines[1].background, Some(ToolRenderBackground::DiffAdded));
        assert!(
            lines[0]
                .segments
                .iter()
                .any(|segment| segment.text == "oldName"
                    && segment.background == Some(ToolRenderBackground::DiffRemovedWord))
        );
        assert!(
            lines[1]
                .segments
                .iter()
                .any(|segment| segment.text == "newName"
                    && segment.background == Some(ToolRenderBackground::DiffAddedWord))
        );
    }

    #[test]
    fn structured_diff_syntect_highlights_added_and_context_lines_without_coloring_deletes() {
        let hunk = StructuredDiffHunk {
            old_start: 1,
            old_lines: 3,
            new_start: 1,
            new_lines: 3,
            lines: vec![
                " fn main() {".to_string(),
                "-  let old_value = 1;".to_string(),
                "+  let new_value = 1;".to_string(),
                " }".to_string(),
            ],
        };

        let lines = render_hunks_with_syntax(
            &[hunk],
            false,
            80,
            SyntaxHighlightOptions {
                file_path: Some("src/main.rs".to_string()),
                first_line: Some("fn main() {".to_string()),
                theme: SyntaxHighlightTheme::Dark,
                prefix_content: None,
            },
        );

        assert!(
            lines[0]
                .segments
                .iter()
                .any(|segment| segment.text.contains("fn") && segment.foreground.is_some())
        );
        assert!(
            lines[1]
                .segments
                .iter()
                .filter(|segment| !segment.text.trim().is_empty())
                .all(|segment| segment.foreground.is_none())
        );
        assert!(
            lines[2]
                .segments
                .iter()
                .any(|segment| segment.text.contains("let") && segment.foreground.is_some())
        );
    }

    #[test]
    fn structured_diff_replays_prefix_content_for_stateful_syntax() {
        let hunk = StructuredDiffHunk {
            old_start: 1,
            old_lines: 0,
            new_start: 2,
            new_lines: 1,
            lines: vec!["+inside comment */".to_string()],
        };

        let with_prefix =
            ColorDiff::new(hunk.clone(), None, "src/main.rs", Some("/*\n".to_string())).render(
                SyntaxHighlightTheme::Dark,
                80,
                false,
            );
        let without_prefix = ColorDiff::new(hunk, None, "src/main.rs", None).render(
            SyntaxHighlightTheme::Dark,
            80,
            false,
        );

        let foreground_for_inside = |lines: &[ToolRenderLine]| {
            lines[0]
                .segments
                .iter()
                .find(|segment| segment.text.contains("inside comment"))
                .and_then(|segment| segment.foreground)
        };
        let with_prefix_foreground = foreground_for_inside(&with_prefix);
        let without_prefix_foreground = foreground_for_inside(&without_prefix);

        assert!(with_prefix_foreground.is_some());
        assert_ne!(with_prefix_foreground, without_prefix_foreground);
    }

    #[test]
    fn color_file_uses_bat_syntect_assets_for_numbered_preview() {
        let lines = ColorFile::new("fn main() {\n    let value = 1;\n}\n", "src/main.rs").render(
            SyntaxHighlightTheme::Dark,
            80,
            false,
        );

        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].text, " 1 fn main() {");
        assert_eq!(lines[1].text, " 2     let value = 1;");
        assert!(
            lines[0]
                .segments
                .iter()
                .any(|segment| segment.text.contains("fn") && segment.foreground.is_some())
        );
        assert!(
            lines[1]
                .segments
                .iter()
                .any(|segment| segment.text.contains("let") && segment.foreground.is_some())
        );
    }

    #[test]
    fn ansi_deleted_lines_dim_content_without_dimming_gutter_like_official_color_diff() {
        let hunk = StructuredDiffHunk {
            old_start: 1,
            old_lines: 1,
            new_start: 1,
            new_lines: 0,
            lines: vec!["-let old = 1;".to_string()],
        };

        let lines = render_hunks_with_syntax(
            &[hunk],
            false,
            80,
            SyntaxHighlightOptions {
                file_path: Some("src/main.rs".to_string()),
                first_line: Some("let old = 1;".to_string()),
                theme: SyntaxHighlightTheme::Ansi,
                prefix_content: None,
            },
        );

        assert!(lines[0].segments[0].text.contains('-'));
        assert!(!lines[0].segments[0].dim);
        assert!(
            lines[0]
                .segments
                .iter()
                .skip(1)
                .filter(|segment| !segment.text.trim().is_empty())
                .all(|segment| segment.dim)
        );
    }

    #[test]
    fn ansi_syntax_theme_uses_official_ansi_scopes_palette_not_truecolor() {
        let lines = ColorFile::new(
            "fn main() {\n    let value = 1;\n    // c\n    return \"hi\";\n}\n",
            "src/main.rs",
        )
        .render(SyntaxHighlightTheme::Ansi, 80, false);

        let foreground_for = |needle: &str| -> Option<Color> {
            lines.iter().find_map(|line| {
                line.segments.iter().find_map(|segment| {
                    (segment.text.contains(needle))
                        .then_some(segment.foreground)
                        .flatten()
                })
            })
        };

        let ansi_index = |color: Color| -> Option<u8> {
            match color {
                Color::AnsiValue(index) => Some(index),
                _ => None,
            }
        };

        // color-diff-napi ANSI_SCOPES
        assert_eq!(
            ansi_index(foreground_for("fn").expect("fn colored")),
            Some(14),
            "storage → ansi(14)"
        );
        assert_eq!(
            ansi_index(foreground_for("let").expect("let colored")),
            Some(14),
            "storage → ansi(14)"
        );
        assert_eq!(
            ansi_index(foreground_for("main").expect("main colored")),
            Some(11),
            "title.function → ansi(11)"
        );
        assert_eq!(
            ansi_index(foreground_for("1").expect("number colored")),
            Some(12),
            "number → ansi(12)"
        );
        assert_eq!(
            ansi_index(foreground_for("//").expect("comment colored")),
            Some(8),
            "comment → ansi(8)"
        );
        assert_eq!(
            ansi_index(foreground_for("return").expect("return colored")),
            Some(13),
            "keyword → ansi(13)"
        );
        assert_eq!(
            ansi_index(foreground_for("hi").expect("string colored")),
            Some(10),
            "string → ansi(10)"
        );

        let foregrounds = lines
            .iter()
            .flat_map(|line| {
                line.segments
                    .iter()
                    .filter_map(|segment| segment.foreground)
            })
            .collect::<Vec<_>>();
        assert!(
            foregrounds
                .iter()
                .all(|color| !matches!(color, Color::Rgb { .. })),
            "ansi syntax theme should keep terminal palette colors instead of RGB truecolor: {foregrounds:?}"
        );
    }

    #[test]
    fn structured_diff_word_diff_uses_official_single_punctuation_tokens() {
        let hunk = StructuredDiffHunk {
            old_start: 1,
            old_lines: 1,
            new_start: 1,
            new_lines: 1,
            lines: vec!["-a => b".to_string(), "+a -> b".to_string()],
        };

        let lines = render_hunks(&[hunk], false, 80);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].segments.iter().any(|segment| segment.text == "="
            && segment.background == Some(ToolRenderBackground::DiffRemovedWord)));
        assert!(lines[1].segments.iter().any(|segment| segment.text == "-"
            && segment.background == Some(ToolRenderBackground::DiffAddedWord)));
        assert!(!lines[0].segments.iter().any(|segment| segment.text == "=>"
            && segment.background == Some(ToolRenderBackground::DiffRemovedWord)));
        assert!(!lines[1].segments.iter().any(|segment| segment.text == "->"
            && segment.background == Some(ToolRenderBackground::DiffAddedWord)));
    }

    #[test]
    fn structured_diff_uses_new_start_for_added_lines_like_official_color_diff() {
        let hunk = StructuredDiffHunk {
            old_start: 10,
            old_lines: 0,
            new_start: 42,
            new_lines: 1,
            lines: vec!["+new line".to_string()],
        };

        let lines = render_hunks(&[hunk], false, 80);
        assert!(lines[0].text.contains("42 +new line"));
        assert!(!lines[0].text.contains("10 +new line"));
    }

    #[test]
    fn structured_diff_wrap_width_uses_official_gutter_width_without_extra_column_loss() {
        let hunk = StructuredDiffHunk {
            old_start: 1,
            old_lines: 0,
            new_start: 1,
            new_lines: 1,
            lines: vec!["+abcd".to_string()],
        };

        let lines = render_hunks(&[hunk], false, 8);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, " 1 +abcd");
    }

    #[test]
    fn structured_diff_gutter_width_uses_full_hunk_range_like_official_color_diff() {
        let hunk = StructuredDiffHunk {
            old_start: 98,
            old_lines: 10,
            new_start: 1,
            new_lines: 1,
            lines: vec!["+x".to_string()],
        };

        let lines = render_hunks(&[hunk], false, 80);
        assert!(
            lines[0].text.starts_with("   1 +x"),
            "line should reserve width for old hunk end 107: {:?}",
            lines[0].text
        );
    }

    #[test]
    fn structured_diff_keeps_official_remove_numbering_for_adjacent_deletes() {
        let hunk = StructuredDiffHunk {
            old_start: 10,
            old_lines: 3,
            new_start: 10,
            new_lines: 1,
            lines: vec![
                "-one".to_string(),
                "-two".to_string(),
                " context".to_string(),
            ],
        };

        let lines = render_hunks(&[hunk], false, 80);
        assert!(lines[0].text.contains("10 -one"));
        assert!(lines[1].text.contains("11 -two"));
        assert!(lines[2].text.contains("10  context"));
    }
}
