//! Maps to: CC `components/Markdown.tsx` + `utils/markdown.ts`.
//! Official Claude Code uses `marked.lexer` + an ANSI formatter, with a cheap
//! plain-text fast path and a small token cache. This port uses the local
//! `marked-rs` lexer so block/inline token boundaries match the official
//! `marked + formatToken + <Ansi>` rendering path instead of CommonMark parser
//! approximations.

use crate::components::markdown_table::{self, TableAlignment};
use crate::constants::figures::BLOCKQUOTE_BAR;
use crate::utils::cli_highlight::highlight_markdown_code;
use crate::utils::debug::component_profile_enabled;
// Maps to: CC `components/Markdown.tsx` importing `createHyperlink` from
// `utils/hyperlink.js` — the one owner of the OSC 8 bytes.
use crate::utils::hyperlink::create_hyperlink;
// Maps to: CC `components/Markdown.tsx:11` importing from `utils/messages.js`.
use crate::utils::messages::strip_prompt_xml_tags;
use crate::utils::theme::Theme;
use iocraft::prelude::*;
use marked_rs::{Align, ListStart, MarkedOptions, TableCell, Token as MarkedToken};
use regex::{Captures, Regex};
use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const TOKEN_CACHE_MAX: usize = 500;
const DEFAULT_MARKDOWN_WIDTH: usize = 80;
pub(crate) const PERMISSION_COLOR_START: &str = "\x1b]1337;cometix-markdown-permission=start\x07";
pub(crate) const PERMISSION_COLOR_END: &str = "\x1b]1337;cometix-markdown-permission=end\x07";

pub type MarkdownBlock = MarkedToken;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkdownLineKind {
    Text,
    Heading { level: u8 },
    Code,
    Table,
    Quote,
    Rule,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkdownLine {
    pub kind: MarkdownLineKind,
    pub text: String,
}

#[derive(Default)]
struct TokenCache {
    entries: HashMap<u64, Vec<MarkdownBlock>>,
    order: VecDeque<u64>,
}

impl TokenCache {
    fn get(&mut self, key: u64) -> Option<Vec<MarkdownBlock>> {
        let value = self.entries.get(&key)?.clone();
        if let Some(pos) = self.order.iter().position(|existing| *existing == key) {
            self.order.remove(pos);
        }
        self.order.push_back(key);
        Some(value)
    }

    fn insert(&mut self, key: u64, value: Vec<MarkdownBlock>) {
        if self.entries.contains_key(&key) {
            self.entries.insert(key, value);
            if let Some(pos) = self.order.iter().position(|existing| *existing == key) {
                self.order.remove(pos);
            }
            self.order.push_back(key);
            return;
        }

        if self.entries.len() >= TOKEN_CACHE_MAX {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            }
        }
        self.order.push_back(key);
        self.entries.insert(key, value);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FormatParent {
    Link,
    ListItem,
    Text,
}

fn token_cache() -> &'static Mutex<TokenCache> {
    static CACHE: OnceLock<Mutex<TokenCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(TokenCache::default()))
}

fn content_hash(content: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    content.hash(&mut hasher);
    hasher.finish()
}

/// Mirrors Markdown.tsx's cheap syntax check before invoking `marked.lexer`.
pub fn has_markdown_syntax(content: &str) -> bool {
    let sample = if content.len() > 500 {
        &content[..safe_char_boundary(content, 500)]
    } else {
        content
    };
    // Official `MD_SYNTAX_RE` checks for any of these marker characters in
    // the first 500 chars, plus blank-line and ordered-list starts. Keep this
    // intentionally broad: false positives are cheaper than missing markdown
    // and match the JS fast path's behavior.
    sample.contains("\n\n")
        || sample
            .chars()
            .any(|ch| matches!(ch, '#' | '*' | '`' | '|' | '[' | '>' | '-' | '_' | '~'))
        || sample
            .lines()
            .any(|line| ordered_list_prefix(line).is_some())
}

fn safe_char_boundary(content: &str, max: usize) -> usize {
    if max >= content.len() {
        return content.len();
    }
    let mut idx = max;
    while idx > 0 && !content.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

pub fn cached_parse_markdown(content: &str) -> Vec<MarkdownBlock> {
    let stripped = strip_prompt_xml_tags(content);
    if !has_markdown_syntax(&stripped) {
        return vec![MarkedToken::Paragraph {
            raw: stripped.clone(),
            pre: None,
            text: stripped.clone(),
            tokens: vec![MarkedToken::Text {
                raw: stripped.clone(),
                text: stripped,
                tokens: None,
                escaped: Some(false),
            }],
        }];
    }

    let key = content_hash(&stripped);
    if let Ok(mut cache) = token_cache().lock() {
        if let Some(hit) = cache.get(key) {
            return hit;
        }
    }

    let parsed = parse_markdown_uncached(&stripped);
    if let Ok(mut cache) = token_cache().lock() {
        cache.insert(key, parsed.clone());
    }
    parsed
}

pub fn parse_markdown_uncached(content: &str) -> Vec<MarkdownBlock> {
    let options = MarkedOptions::default();
    // Cometix intentionally keeps GFM strikethrough enabled: iocraft supports
    // SGR 9/29 and users expect `~~deleted~~` to render as crossed-out text.
    // Lone approximation markers such as `~100` remain plain text because the
    // marked-rs del tokenizer requires a closing tilde run.
    marked_rs::lexer_with_options(content, options).tokens
}

fn ordered_list_prefix(trimmed: &str) -> Option<(usize, usize)> {
    let dot = trimmed.find('.')?;
    if dot == 0 || !trimmed[..dot].chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let after = trimmed[dot + 1..].chars().next()?;
    if !after.is_whitespace() {
        return None;
    }
    let number = trimmed[..dot].parse::<usize>().ok()?;
    Some((number, dot + 2))
}

fn render_markdown_link(href: &str, content: &str) -> String {
    if href.contains('\x1b')
        || href.contains('\x07')
        || content.contains('\x1b')
        || content.contains('\x07')
    {
        // Preserve already-terminal-ready OSC/SGR sequences instead of nesting
        // another OSC 8 hyperlink around marked's GFM autolink token.
        return content.to_string();
    }

    if let Some(email) = href.strip_prefix("mailto:") {
        return email.to_string();
    }

    let plain_content = strip_ansi_for_width(content);
    if !plain_content.is_empty() && plain_content != href {
        create_hyperlink(href, Some(content))
    } else {
        create_hyperlink(href, None)
    }
}

fn issue_ref_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"(^|[^\w./-])([A-Za-z0-9][\w-]*/[A-Za-z0-9][\w.-]*)#(\d+)\b")
            .expect("official issue reference regex is valid")
    })
}

fn linkify_issue_references(text: &str) -> String {
    if !supports_hyperlinks() {
        return text.to_string();
    }

    issue_ref_regex()
        .replace_all(text, |captures: &Captures<'_>| {
            let prefix = captures.get(1).map_or("", |m| m.as_str());
            let repo = captures.get(2).map_or("", |m| m.as_str());
            let num = captures.get(3).map_or("", |m| m.as_str());
            format!(
                "{}{}",
                prefix,
                create_hyperlink(
                    &format!("https://github.com/{repo}/issues/{num}"),
                    Some(&format!("{repo}#{num}")),
                )
            )
        })
        .into_owned()
}

fn normalize_inline_text(text: &str) -> String {
    text.trim().to_string()
}

pub fn markdown_to_lines(content: &str) -> Vec<MarkdownLine> {
    markdown_to_lines_with_width(content, DEFAULT_MARKDOWN_WIDTH)
}

pub fn markdown_to_lines_with_width(content: &str, terminal_width: usize) -> Vec<MarkdownLine> {
    let tokens = cached_parse_markdown(content);
    markdown_tokens_to_lines(&tokens, terminal_width)
}

fn markdown_tokens_to_lines(tokens: &[MarkedToken], terminal_width: usize) -> Vec<MarkdownLine> {
    let mut lines = Vec::new();
    let mut non_table_content = String::new();
    let mut emitted_element = false;

    for token in tokens {
        if let MarkedToken::Table {
            align,
            header,
            rows,
            ..
        } = token
        {
            flush_non_table_content(&mut non_table_content, &mut lines, &mut emitted_element);
            let table_lines = render_marked_table_lines(align, header, rows, terminal_width);
            if !table_lines.is_empty() {
                push_element_gap(&mut lines, emitted_element);
                lines.extend(table_lines);
                emitted_element = true;
            }
        } else {
            non_table_content.push_str(&format_token(token, 0, None, None));
        }
    }

    flush_non_table_content(&mut non_table_content, &mut lines, &mut emitted_element);
    lines
}

/// One element of a rendered `Markdown`, mirroring CC `Markdown.tsx`
/// `MarkdownBody` (`flushNonTableContent`, :112-121): every run of non-table
/// tokens is a single `<Ansi>` — one text node however many lines it spans —
/// and each table is its own element. The column's `gap={1}` separates them.
///
/// The line-based [`markdown_to_lines_with_width`] splits the same run into
/// one node per line; a 400-line reply became ~1700 layout nodes where CC
/// has one, which is what made long sessions lay out slowly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MarkdownRenderBlock {
    Ansi(String),
    Table(Vec<MarkdownLine>),
}

pub fn markdown_to_blocks_with_width(
    content: &str,
    terminal_width: usize,
) -> Vec<MarkdownRenderBlock> {
    let tokens = cached_parse_markdown(content);
    let mut blocks = Vec::new();
    let mut non_table_content = String::new();

    fn flush(non_table_content: &mut String, blocks: &mut Vec<MarkdownRenderBlock>) {
        // CC: `{nonTableContent.trim()}`.
        let trimmed = non_table_content.trim();
        if !trimmed.is_empty() {
            blocks.push(MarkdownRenderBlock::Ansi(trimmed.to_string()));
        }
        non_table_content.clear();
    }

    for token in &tokens {
        if let MarkedToken::Table {
            align,
            header,
            rows,
            ..
        } = token
        {
            flush(&mut non_table_content, &mut blocks);
            let table_lines = render_marked_table_lines(align, header, rows, terminal_width);
            if !table_lines.is_empty() {
                blocks.push(MarkdownRenderBlock::Table(table_lines));
            }
        } else {
            non_table_content.push_str(&format_token(token, 0, None, None));
        }
    }

    flush(&mut non_table_content, &mut blocks);
    blocks
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamingMarkdownParts {
    pub stable_prefix: String,
    pub unstable_suffix: String,
}

/// Mirrors official `StreamingMarkdown`: keep every completed top-level block
/// before the final growing block stable, and re-parse only the suffix.
pub fn split_streaming_markdown_blocks(
    previous_stable_prefix: &str,
    content: &str,
) -> StreamingMarkdownParts {
    let stripped = strip_prompt_xml_tags(content);
    let stable_base = if stripped.starts_with(previous_stable_prefix) {
        previous_stable_prefix
    } else {
        ""
    };

    let boundary = stable_base.len();
    let suffix = &stripped[boundary..];
    let tokens = parse_markdown_uncached(suffix);
    let advance = tokens
        .iter()
        .rposition(|token| !matches!(token, MarkedToken::Space { .. }))
        .map(|last_content_idx| tokens[..last_content_idx].iter().map(token_raw_len).sum())
        .unwrap_or(0usize);

    let stable_prefix = if advance > 0 {
        stripped[..boundary + advance].to_string()
    } else {
        stable_base.to_string()
    };
    let unstable_suffix = stripped[stable_prefix.len()..].to_string();

    StreamingMarkdownParts {
        stable_prefix,
        unstable_suffix,
    }
}

fn token_raw_len(token: &MarkedToken) -> usize {
    match token {
        MarkedToken::Space { raw }
        | MarkedToken::Code { raw, .. }
        | MarkedToken::Heading { raw, .. }
        | MarkedToken::Hr { raw }
        | MarkedToken::Blockquote { raw, .. }
        | MarkedToken::List { raw, .. }
        | MarkedToken::ListItem { raw, .. }
        | MarkedToken::Paragraph { raw, .. }
        | MarkedToken::Text { raw, .. }
        | MarkedToken::Table { raw, .. }
        | MarkedToken::Html { raw, .. }
        | MarkedToken::Def { raw, .. }
        | MarkedToken::Escape { raw, .. }
        | MarkedToken::Strong { raw, .. }
        | MarkedToken::Em { raw, .. }
        | MarkedToken::Codespan { raw, .. }
        | MarkedToken::Br { raw }
        | MarkedToken::Del { raw, .. }
        | MarkedToken::Link { raw, .. }
        | MarkedToken::Image { raw, .. } => raw.len(),
    }
}

fn flush_non_table_content(
    non_table_content: &mut String,
    lines: &mut Vec<MarkdownLine>,
    emitted_element: &mut bool,
) {
    if non_table_content.is_empty() {
        return;
    }

    let trimmed = non_table_content.trim();
    if !trimmed.is_empty() {
        push_element_gap(lines, *emitted_element);
        lines.extend(trimmed.split('\n').map(|line| MarkdownLine {
            kind: MarkdownLineKind::Text,
            text: line.to_string(),
        }));
        *emitted_element = true;
    }
    non_table_content.clear();
}

fn push_element_gap(lines: &mut Vec<MarkdownLine>, emitted_element: bool) {
    if emitted_element {
        lines.push(MarkdownLine {
            kind: MarkdownLineKind::Text,
            text: String::new(),
        });
    }
}

fn format_tokens(
    tokens: &[MarkedToken],
    list_depth: usize,
    ordered_list_number: Option<usize>,
    parent: Option<FormatParent>,
) -> String {
    tokens
        .iter()
        .map(|token| format_token(token, list_depth, ordered_list_number, parent))
        .collect::<Vec<_>>()
        .join("")
}

fn format_token(
    token: &MarkedToken,
    list_depth: usize,
    ordered_list_number: Option<usize>,
    parent: Option<FormatParent>,
) -> String {
    match token {
        MarkedToken::Space { .. } => "\n".to_string(),
        MarkedToken::Code { text, lang, .. } => {
            format!("{}\n", highlight_markdown_code(text, lang.as_deref()))
        }
        MarkedToken::Codespan { text, .. } => {
            format!("{PERMISSION_COLOR_START}{text}{PERMISSION_COLOR_END}")
        }
        MarkedToken::Em { tokens, .. } => format!(
            "\x1b[3m{}\x1b[23m",
            format_tokens(tokens, list_depth, ordered_list_number, parent)
        ),
        MarkedToken::Strong { tokens, .. } => format!(
            "\x1b[1m{}\x1b[22m",
            format_tokens(tokens, list_depth, ordered_list_number, parent)
        ),
        MarkedToken::Heading { depth, tokens, .. } => {
            let content = format_tokens(tokens, 0, None, None);
            if *depth == 1 {
                format!("\x1b[1;3;4m{content}\x1b[24;23;22m\n\n")
            } else {
                format!("\x1b[1m{content}\x1b[22m\n\n")
            }
        }
        MarkedToken::Hr { .. } => "---".to_string(),
        MarkedToken::Image { href, .. } => href.clone(),
        MarkedToken::Link { href, tokens, .. } => {
            let link_text = format_tokens(tokens, 0, None, Some(FormatParent::Link));
            render_markdown_link(href, &link_text)
        }
        MarkedToken::List {
            ordered,
            start,
            items,
            ..
        } => {
            let start = list_start_number(start);
            items
                .iter()
                .enumerate()
                .map(|(idx, item)| {
                    format_token(
                        item,
                        list_depth,
                        if *ordered { Some(start + idx) } else { None },
                        None,
                    )
                })
                .collect::<Vec<_>>()
                .join("")
        }
        MarkedToken::ListItem { tokens, .. } => tokens
            .iter()
            .map(|child| {
                format!(
                    "{}{}",
                    "  ".repeat(list_depth),
                    format_token(
                        child,
                        list_depth + 1,
                        ordered_list_number,
                        Some(FormatParent::ListItem),
                    )
                )
            })
            .collect::<Vec<_>>()
            .join(""),
        MarkedToken::Paragraph { tokens, .. } => {
            format!("{}\n", format_tokens(tokens, 0, None, None))
        }
        MarkedToken::Blockquote { tokens, .. } => {
            let inner = format_tokens(tokens, 0, None, None);
            inner
                .split('\n')
                .map(|line| {
                    if strip_ansi_for_width(line).trim().is_empty() {
                        line.to_string()
                    } else {
                        format!("\x1b[2m{BLOCKQUOTE_BAR}\x1b[22m \x1b[3m{line}\x1b[23m")
                    }
                })
                .collect::<Vec<_>>()
                .join("\n")
        }
        MarkedToken::Br { .. } => "\n".to_string(),
        MarkedToken::Text { text, tokens, .. } => {
            if parent == Some(FormatParent::Link) {
                return text.clone();
            }

            let content = if let Some(tokens) = tokens {
                format_tokens(
                    tokens,
                    list_depth,
                    ordered_list_number,
                    Some(FormatParent::Text),
                )
            } else {
                linkify_issue_references(text)
            };

            if parent == Some(FormatParent::ListItem) {
                let marker = ordered_list_number
                    .map(|number| format!("{}.", get_list_number(list_depth, number)))
                    .unwrap_or_else(|| "-".to_string());
                format!("{marker} {content}\n")
            } else {
                content
            }
        }
        MarkedToken::Table {
            align,
            header,
            rows,
            ..
        } => format_table_token_pipe(align, header, rows),
        MarkedToken::Escape { text, .. } => text.clone(),
        MarkedToken::Del { tokens, .. } => format!(
            "\x1b[9m{}\x1b[29m",
            format_tokens(tokens, list_depth, ordered_list_number, parent)
        ),
        MarkedToken::Def { .. } | MarkedToken::Html { .. } => String::new(),
    }
}

fn list_start_number(start: &ListStart) -> usize {
    match start {
        ListStart::Number(value) => (*value as usize).max(1),
        ListStart::Empty(_) => 1,
    }
}

fn number_to_letter(mut n: usize) -> String {
    if n == 0 {
        return "0".to_string();
    }
    let mut chars = Vec::new();
    while n > 0 {
        n -= 1;
        chars.push((b'a' + (n % 26) as u8) as char);
        n /= 26;
    }
    chars.iter().rev().collect()
}

fn number_to_roman(mut n: usize) -> String {
    if n == 0 {
        return "0".to_string();
    }
    const VALUES: &[(usize, &str)] = &[
        (1000, "m"),
        (900, "cm"),
        (500, "d"),
        (400, "cd"),
        (100, "c"),
        (90, "xc"),
        (50, "l"),
        (40, "xl"),
        (10, "x"),
        (9, "ix"),
        (5, "v"),
        (4, "iv"),
        (1, "i"),
    ];
    let mut out = String::new();
    for (value, numeral) in VALUES {
        while n >= *value {
            out.push_str(numeral);
            n -= *value;
        }
    }
    out
}

fn get_list_number(list_depth: usize, ordered_list_number: usize) -> String {
    match list_depth {
        0 | 1 => ordered_list_number.to_string(),
        2 => number_to_letter(ordered_list_number),
        3 => number_to_roman(ordered_list_number),
        _ => ordered_list_number.to_string(),
    }
}

fn format_table_cell(cell: &TableCell) -> String {
    format_tokens(&cell.tokens, 0, None, None)
        .trim_end()
        .to_string()
}

fn render_marked_table_lines(
    align: &[Option<Align>],
    header: &[TableCell],
    rows: &[Vec<TableCell>],
    terminal_width: usize,
) -> Vec<MarkdownLine> {
    let headers = header.iter().map(format_table_cell).collect::<Vec<_>>();
    let aligns = align
        .iter()
        .cloned()
        .map(TableAlignment::from)
        .collect::<Vec<_>>();
    let rows = rows
        .iter()
        .map(|row| row.iter().map(format_table_cell).collect::<Vec<_>>())
        .collect::<Vec<_>>();
    render_table_lines(headers, aligns, rows, terminal_width)
}

fn format_table_token_pipe(
    align: &[Option<Align>],
    header: &[TableCell],
    rows: &[Vec<TableCell>],
) -> String {
    let headers = header.iter().map(format_table_cell).collect::<Vec<_>>();
    let aligns = align
        .iter()
        .cloned()
        .map(TableAlignment::from)
        .collect::<Vec<_>>();
    let row_cells = rows
        .iter()
        .map(|row| row.iter().map(format_table_cell).collect::<Vec<_>>())
        .collect::<Vec<_>>();
    markdown_table::format_table_token_pipe(headers, aligns, row_cells)
}

fn render_table_lines(
    headers: Vec<String>,
    aligns: Vec<TableAlignment>,
    rows: Vec<Vec<String>>,
    terminal_width: usize,
) -> Vec<MarkdownLine> {
    markdown_table::render_markdown_table_lines(headers, aligns, rows, terminal_width)
        .into_iter()
        .map(|text| MarkdownLine {
            kind: MarkdownLineKind::Table,
            text,
        })
        .collect()
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

    if rest
        .as_bytes()
        .get(1)
        .is_some_and(|byte| matches!(byte, b'P' | b'_' | b'^' | b'X'))
    {
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
        if matches!(next, '(' | ')' | '*' | '+') {
            if let Some(designator) = input[end..].chars().next() {
                end += designator.len_utf8();
            }
        }
    }
    Some(end)
}

fn strip_ansi_for_width(input: &str) -> String {
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

fn display_width_ansi(input: &str) -> usize {
    UnicodeWidthStr::width(strip_ansi_for_width(input).as_str())
}

fn ansi_foreground_sequence(color: Color) -> String {
    match color {
        Color::Reset => "\x1b[39m".to_string(),
        Color::Black => "\x1b[38;5;0m".to_string(),
        Color::DarkRed => "\x1b[38;5;1m".to_string(),
        Color::DarkGreen => "\x1b[38;5;2m".to_string(),
        Color::DarkYellow => "\x1b[38;5;3m".to_string(),
        Color::DarkBlue => "\x1b[38;5;4m".to_string(),
        Color::DarkMagenta => "\x1b[38;5;5m".to_string(),
        Color::DarkCyan => "\x1b[38;5;6m".to_string(),
        Color::Grey => "\x1b[38;5;7m".to_string(),
        Color::DarkGrey => "\x1b[38;5;8m".to_string(),
        Color::Red => "\x1b[38;5;9m".to_string(),
        Color::Green => "\x1b[38;5;10m".to_string(),
        Color::Yellow => "\x1b[38;5;11m".to_string(),
        Color::Blue => "\x1b[38;5;12m".to_string(),
        Color::Magenta => "\x1b[38;5;13m".to_string(),
        Color::Cyan => "\x1b[38;5;14m".to_string(),
        Color::White => "\x1b[38;5;15m".to_string(),
        Color::AnsiValue(value) => format!("\x1b[38;5;{value}m"),
        Color::Rgb { r, g, b } => format!("\x1b[38;2;{r};{g};{b}m"),
    }
}

fn apply_optional_foreground(text: &str, color: Option<Color>) -> String {
    if let Some(color) = color {
        format!("{}{}\x1b[0m", ansi_foreground_sequence(color), text)
    } else {
        text.to_string()
    }
}

fn apply_inline_color_markers(
    text: &str,
    base_color: Option<Color>,
    permission_color: Color,
) -> String {
    let permission_start = ansi_foreground_sequence(permission_color);
    let permission_end = base_color
        .map(ansi_foreground_sequence)
        .unwrap_or_else(|| "\x1b[39m".to_string());
    text.replace(PERMISSION_COLOR_START, &permission_start)
        .replace(PERMISSION_COLOR_END, &permission_end)
}

/// Coloring for a whole non-table block: the optional foreground wraps the
/// block once (ANSI color persists across newlines) and the permission-color
/// markers are substituted, exactly what the per-line path did to each line.
fn markdown_block_ansi_content(
    text: &str,
    color: Option<Color>,
    permission_color: Color,
) -> String {
    apply_inline_color_markers(
        &apply_optional_foreground(text, color),
        color,
        permission_color,
    )
}

fn markdown_line_ansi_content(
    line: &MarkdownLine,
    color: Option<Color>,
    permission_color: Color,
) -> String {
    let content = match line.kind {
        MarkdownLineKind::Heading { level } => {
            let styled = if level == 1 {
                // Official formatToken: h1 = bold + italic + underline.
                format!("\x1b[1;3;4m{}\x1b[24;23;22m", line.text)
            } else {
                // Official formatToken: h2+ = bold.
                format!("\x1b[1m{}\x1b[22m", line.text)
            };
            apply_optional_foreground(&styled, color)
        }
        MarkdownLineKind::Quote => {
            let Some(rest) = line.text.strip_prefix(BLOCKQUOTE_BAR) else {
                return apply_inline_color_markers(
                    &apply_optional_foreground(&line.text, color),
                    color,
                    permission_color,
                );
            };
            let content = rest.strip_prefix(' ').unwrap_or(rest);
            let quoted = if content.trim().is_empty() {
                format!("\x1b[2m{BLOCKQUOTE_BAR}\x1b[22m")
            } else {
                // Official formatToken: dim bar, italic content at normal brightness.
                format!("\x1b[2m{BLOCKQUOTE_BAR}\x1b[22m \x1b[3m{content}\x1b[23m")
            };
            apply_optional_foreground(&quoted, color)
        }
        MarkdownLineKind::Text
        | MarkdownLineKind::Code
        | MarkdownLineKind::Table
        | MarkdownLineKind::Rule => apply_optional_foreground(&line.text, color),
    };
    apply_inline_color_markers(&content, color, permission_color)
}

pub fn markdown_to_plain_text(content: &str) -> String {
    markdown_to_lines(content)
        .into_iter()
        .map(|line| strip_ansi_for_width(&line.text))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn wrap_markdown_line(line: &str, max_width: usize) -> Vec<String> {
    if max_width == 0 || UnicodeWidthStr::width(line) <= max_width {
        return vec![line.to_string()];
    }

    let mut out = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;
    for word in line.split_inclusive(char::is_whitespace) {
        let word_width = UnicodeWidthStr::width(word);
        if current_width > 0 && current_width + word_width > max_width {
            out.push(current.trim_end().to_string());
            current.clear();
            current_width = 0;
        }

        if word_width > max_width {
            for ch in word.chars() {
                let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
                if current_width > 0 && current_width + ch_width > max_width {
                    out.push(current.trim_end().to_string());
                    current.clear();
                    current_width = 0;
                }
                current.push(ch);
                current_width += ch_width;
            }
        } else {
            current.push_str(word);
            current_width += word_width;
        }
    }

    if !current.is_empty() {
        out.push(current.trim_end().to_string());
    }
    out
}

#[derive(Default, Props)]
pub struct MarkdownProps {
    pub content: String,
    pub dim_color: bool,
    pub color: Option<Color>,
}

#[component]
pub fn Markdown(props: &MarkdownProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let profile_start = component_profile_enabled().then(Instant::now);
    let theme = hooks.use_context::<Theme>();
    let (terminal_width, _) = hooks.use_terminal_size();
    let convert_start = component_profile_enabled().then(Instant::now);
    let blocks = markdown_to_blocks_with_width(&props.content, terminal_width as usize);
    let convert_elapsed = convert_start.map(|start| start.elapsed());
    let block_count = blocks.len();
    let content_len = props.content.len();

    // CC `MarkdownBody`: `<Box flexDirection="column" gap={1}>{elements}</Box>`
    // where each non-table run is one `<Ansi>` and each table one element.
    let rendered = element! {
        View(flex_direction: FlexDirection::Column, row_gap: 1) {
            #(blocks.into_iter().map(|block| match block {
                MarkdownRenderBlock::Ansi(text) => element! {
                    Ansi(
                        content: markdown_block_ansi_content(&text, props.color, theme.permission),
                        dim_color: props.dim_color,
                    )
                }
                .into_any(),
                MarkdownRenderBlock::Table(lines) => element! {
                    View(flex_direction: FlexDirection::Column) {
                        #(lines.into_iter().map(|line| element! {
                            Ansi(
                                content: markdown_line_ansi_content(&line, props.color, theme.permission),
                                dim_color: props.dim_color,
                            )
                        }))
                    }
                }
                .into_any(),
            }))
        }
    };

    if let Some(start) = profile_start {
        let elapsed = start.elapsed();
        if elapsed >= Duration::from_millis(5) {
            eprintln!(
                "cometix-component name=Markdown elapsed={:?} convert={:?} content_len={} blocks={} width={} dim={}",
                elapsed,
                convert_elapsed.unwrap_or_default(),
                content_len,
                block_count,
                terminal_width,
                props.dim_color,
            );
        }
    }

    rendered
}

#[derive(Default, Props)]
pub struct StreamingMarkdownProps {
    pub content: String,
}

#[component]
pub fn StreamingMarkdown(
    props: &StreamingMarkdownProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let profile_start = component_profile_enabled().then(Instant::now);
    let mut stable_prefix_ref = hooks.use_ref(String::new);
    let current_stable_prefix = stable_prefix_ref.read().clone();
    let split_start = component_profile_enabled().then(Instant::now);
    let parts = split_streaming_markdown_blocks(&current_stable_prefix, &props.content);
    let split_elapsed = split_start.map(|start| start.elapsed());
    if parts.stable_prefix != current_stable_prefix {
        stable_prefix_ref.set(parts.stable_prefix.clone());
    }
    let stable_len = parts.stable_prefix.len();
    let unstable_len = parts.unstable_suffix.len();
    let content_len = props.content.len();

    let rendered = element! {
        View(flex_direction: FlexDirection::Column, row_gap: 1) {
            #(if !parts.stable_prefix.is_empty() {
                Some(element! { Markdown(content: parts.stable_prefix.clone()) })
            } else {
                None
            })
            #(if !parts.unstable_suffix.is_empty() {
                Some(element! { Markdown(content: parts.unstable_suffix.clone()) })
            } else {
                None
            })
        }
    };

    if let Some(start) = profile_start {
        let elapsed = start.elapsed();
        if elapsed >= Duration::from_millis(5) {
            eprintln!(
                "cometix-component name=StreamingMarkdown elapsed={:?} split={:?} content_len={} stable_len={} unstable_len={}",
                elapsed,
                split_elapsed.unwrap_or_default(),
                content_len,
                stable_len,
                unstable_len,
            );
        }
    }

    rendered
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::markdown_table::wrap_table_cell;

    #[test]
    fn markdown_plain_fast_path_skips_parser_shape() {
        assert!(!has_markdown_syntax("plain assistant sentence"));
        assert_eq!(
            markdown_to_plain_text("plain assistant sentence"),
            "plain assistant sentence"
        );
    }

    #[test]
    fn markdown_syntax_detection_matches_official_marker_fast_path() {
        assert!(has_markdown_syntax("snake_case identifier"));
        assert!(has_markdown_syntax("approximately ~100 files"));
        assert!(has_markdown_syntax("1. first item"));
        assert!(!has_markdown_syntax(
            "+ plus list stays on the official plain fast path"
        ));
    }

    #[test]
    fn streaming_markdown_split_advances_only_completed_blocks() {
        let parts = split_streaming_markdown_blocks("", "first\n\nsecond");
        assert_eq!(parts.stable_prefix, "first\n\n");
        assert_eq!(parts.unstable_suffix, "second");

        let parts = split_streaming_markdown_blocks(&parts.stable_prefix, "first\n\nsecond grows");
        assert_eq!(parts.stable_prefix, "first\n\n");
        assert_eq!(parts.unstable_suffix, "second grows");

        let parts =
            split_streaming_markdown_blocks(&parts.stable_prefix, "first\n\nsecond\n\nthird");
        assert_eq!(parts.stable_prefix, "first\n\nsecond\n\n");
        assert_eq!(parts.unstable_suffix, "third");
    }

    #[test]
    fn streaming_markdown_split_resets_when_content_is_replaced_after_xml_strip() {
        let parts =
            split_streaming_markdown_blocks("old\n\n", "<context>hidden</context>new\n\ntail");
        assert_eq!(parts.stable_prefix, "new\n\n");
        assert_eq!(parts.unstable_suffix, "tail");
    }

    #[test]
    fn streaming_markdown_component_renders_growing_block() {
        let canvas = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                StreamingMarkdown(content: "unfinished".to_string())
            }
        }
        .render(None);

        assert_eq!(canvas.to_string(), "unfinished\n");
    }

    #[test]
    fn markdown_supports_gfm_strikethrough_without_breaking_approximation_tilde() {
        assert_eq!(
            markdown_to_plain_text("approximately ~100 files and ~~deleted~~"),
            "approximately ~100 files and deleted"
        );
    }

    #[test]
    fn markdown_renders_common_blocks_through_marked_rs() {
        let input = "# Title\n\n- **one**\n1. `two`\n> quote\n\n```rust\nfn main() {}\n```";
        assert_eq!(
            markdown_to_plain_text(input),
            "Title\n\n- one\n1. two\n▎ quote\n\nfn main() {}"
        );
    }

    #[test]
    fn markdown_preserves_marked_block_spacing_around_hr_and_headings() {
        let input = "intro\n\n---\n\n## Heading\n\nbody";
        assert_eq!(
            markdown_to_plain_text(input),
            "intro\n\n---\nHeading\n\nbody"
        );
    }

    #[test]
    fn markdown_component_preserves_marked_blank_lines() {
        let canvas = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                Markdown(content: "intro\n\n---\n\n## Heading\n\nbody".to_string())
            }
        }
        .render(None);

        assert_eq!(canvas.to_string(), "intro\n\n---\nHeading\n\nbody\n");
    }

    #[test]
    fn markdown_soft_breaks_remain_line_breaks_like_marked() {
        assert_eq!(markdown_to_plain_text("first\nsecond"), "first\nsecond");
    }

    #[test]
    fn markdown_preserves_inline_spacing_like_marked_text_tokens() {
        assert_eq!(markdown_to_plain_text("alpha   beta"), "alpha   beta");
        assert_eq!(markdown_to_plain_text("alpha   **beta**"), "alpha   beta");
    }

    #[test]
    fn markdown_task_list_markers_follow_marked_text_output() {
        assert_eq!(
            markdown_to_plain_text("- [x] done\n- [ ] todo"),
            "- done\n- todo"
        );
    }

    #[test]
    fn markdown_footnote_like_reference_uses_marked_link_reference_behavior() {
        assert_eq!(markdown_to_plain_text("ref[^1]"), "ref[^1]");
        let rendered = markdown_to_plain_text("ref[^1]\n\n[^1]: note");
        assert!(matches!(rendered.as_str(), "ref^1" | "refnote"));
    }

    #[test]
    fn markdown_tables_render_official_box_layout_with_figures() {
        let input = "| 名称 | Count |\n| --- | ---: |\n| 猫 | 2 |\n| crates | 10 |";
        let lines = markdown_to_lines_with_width(input, 80);
        assert_eq!(lines[0].text, "┌────────┬───────┐");
        assert_eq!(lines[2].text, "├────────┼───────┤");
        assert_eq!(lines.last().unwrap().text, "└────────┴───────┘");
        assert!(lines[3].text.contains("猫"));
        assert!(lines[3].text.ends_with("    2 │"));
    }

    #[test]
    fn markdown_tables_apply_left_center_right_alignment() {
        let input = "| L | C | R |\n|:--|:-:|--:|\n| x | y | z |";
        let lines = markdown_to_lines_with_width(input, 80);
        assert_eq!(lines[1].text, "│  L  │  C  │  R  │");
        assert_eq!(lines[3].text, "│ x   │  y  │   z │");
    }

    #[test]
    fn markdown_tables_switch_to_vertical_format_when_wrapping_is_too_tall() {
        let input = "| Key | Value |\n| --- | --- |\n| A | one two three four five six seven eight nine ten |";
        let lines = markdown_to_lines_with_width(input, 24);
        assert_eq!(strip_ansi_for_width(&lines[0].text), "Key: A");
        assert!(
            lines
                .iter()
                .any(|line| strip_ansi_for_width(&line.text).starts_with("Value:"))
        );
        assert!(!lines.iter().any(|line| line.text.starts_with("┌")));
    }

    #[test]
    fn markdown_tables_preserve_wide_char_widths() {
        let input = "| 名 | Count |\n| --- | ---: |\n| 猫猫 | 2 |";
        let lines = markdown_to_lines_with_width(input, 80);
        assert!(lines[3].text.contains("猫猫"));
        assert!(lines[3].text.ends_with("    2 │"));
    }

    #[test]
    fn markdown_links_and_images_match_official_fallback_text() {
        assert_eq!(
            markdown_to_plain_text("[ignored](mailto:dev@example.com)"),
            "dev@example.com"
        );
        assert_eq!(
            markdown_to_plain_text("![alt](https://example.com/image.png)"),
            "https://example.com/image.png"
        );
    }

    #[test]
    fn markdown_table_width_ignores_ansi_segments() {
        let input = "| Col |\n| --- |\n| \x1b[31mred\x1b[0m |";
        let lines = markdown_to_lines_with_width(input, 80);
        assert_eq!(strip_ansi_for_width(&lines[3].text), "│ red │");
        assert_eq!(display_width_ansi(&lines[3].text), 7);
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

        let word_wrapped = wrap_table_cell("\x1b[31mred green\x1b[0m", 3, false);
        assert_eq!(strip_ansi_for_width(&word_wrapped[0]), "red");
        assert_eq!(strip_ansi_for_width(&word_wrapped[1]), "green");
        assert!(word_wrapped[1].starts_with("\x1b[31m"));
    }

    #[test]
    fn markdown_component_renders_through_ansi_boundary() {
        let canvas = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                Markdown(
                    content: "\x1b[31;1mred\x1b[0m plain \x1b[3mitalic\x1b[0m \x1b[4munder\x1b[0m \x1b]8;;https://example.com\x07link\x1b]8;;\x07".to_string(),
                    color: Some(Color::Rgb { r: 1, g: 2, b: 3 }),
                )
            }
        }
        .render(None);

        assert_eq!(canvas.to_string(), "red plain italic under link\n");
        let red = canvas.resolved_text_style(0, 0).unwrap();
        assert_eq!(red.color, Some(Color::DarkRed));
        assert_eq!(red.weight, Weight::Bold);
        assert!(canvas.resolved_text_style(10, 0).unwrap().italic);
        assert!(canvas.resolved_text_style(17, 0).unwrap().underline);
        assert_eq!(
            canvas.hyperlink_at(23, 0).as_deref(),
            Some("https://example.com")
        );
    }

    #[test]
    fn markdown_component_matches_official_inline_styles() {
        let theme = *crate::utils::theme::current();
        let canvas = element! {
            ContextProvider(value: Context::owned(theme)) {
                Markdown(content: "**bold** *italic* `code`".to_string())
            }
        }
        .render(None);

        assert_eq!(canvas.to_string(), "bold italic code\n");
        assert_eq!(
            canvas.resolved_text_style(0, 0).unwrap().weight,
            Weight::Bold
        );
        assert!(canvas.resolved_text_style(5, 0).unwrap().italic);
        assert_eq!(
            canvas.resolved_text_style(12, 0).unwrap().color,
            Some(theme.permission)
        );
    }

    #[test]
    fn markdown_component_renders_gfm_strikethrough() {
        let canvas = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                Markdown(content: "approximately ~100 files and ~~deleted~~".to_string())
            }
        }
        .render(None);

        assert_eq!(canvas.to_string(), "approximately ~100 files and deleted\n");
        assert!(!canvas.resolved_text_style(14, 0).unwrap().strikethrough);
        assert!(canvas.resolved_text_style(30, 0).unwrap().strikethrough);
    }

    #[test]
    fn markdown_component_matches_official_heading_and_quote_styles() {
        let canvas = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                Markdown(content: "# Title".to_string())
            }
        }
        .render(None);
        assert_eq!(canvas.to_string(), "Title\n");
        let h1 = canvas.resolved_text_style(0, 0).unwrap();
        assert_eq!(h1.color, None);
        assert_eq!(h1.weight, Weight::Bold);
        assert!(h1.italic);
        assert!(h1.underline);

        let canvas = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                Markdown(content: "## Subtitle".to_string())
            }
        }
        .render(None);
        let h2 = canvas.resolved_text_style(0, 0).unwrap();
        assert_eq!(h2.color, None);
        assert_eq!(h2.weight, Weight::Bold);
        assert!(!h2.italic);
        assert!(!h2.underline);

        let canvas = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                Markdown(content: "> quote".to_string())
            }
        }
        .render(None);
        assert_eq!(canvas.to_string(), "▎ quote\n");
        let bar = canvas.resolved_text_style(0, 0).unwrap();
        let quote = canvas.resolved_text_style(2, 0).unwrap();
        assert_eq!(bar.weight, Weight::Light);
        assert_eq!(quote.weight, Weight::Normal);
        assert!(quote.italic);
    }

    #[test]
    fn markdown_component_does_not_force_subtle_color_on_code_or_tables() {
        let canvas = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                Markdown(content: "```\nlet x = 1;\n```".to_string())
            }
        }
        .render(None);
        assert_eq!(canvas.to_string(), "let x = 1;\n");
        assert_eq!(canvas.resolved_text_style(0, 0).unwrap().color, None);

        let canvas = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                Markdown(content: "| Col |\n| --- |\n| cell |".to_string())
            }
        }
        .render(None);
        assert!(canvas.to_string().contains("│ cell │"));
        assert_eq!(canvas.resolved_text_style(0, 0).unwrap().color, None);
        assert_eq!(canvas.resolved_text_style(2, 3).unwrap().color, None);
    }

    #[test]
    fn markdown_component_applies_syntax_highlighting_for_supported_code() {
        let canvas = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                Markdown(content: "```rust\nfn main() { let x = 1; }\n```".to_string())
            }
        }
        .render(None);

        assert_eq!(canvas.to_string(), "fn main() { let x = 1; }\n");
        assert!(canvas.resolved_text_style(0, 0).unwrap().color.is_some());
        assert!(canvas.resolved_text_style(12, 0).unwrap().color.is_some());
    }

    #[test]
    fn markdown_component_applies_plain_text_theme_color_via_ansi() {
        let color = Color::Rgb { r: 1, g: 2, b: 3 };
        let canvas = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                Markdown(content: "plain".to_string(), color: Some(color))
            }
        }
        .render(None);

        assert_eq!(canvas.to_string(), "plain\n");
        assert_eq!(canvas.resolved_text_style(0, 0).unwrap().color, Some(color));
    }

    #[test]
    fn markdown_component_preserves_ansi_styles_inside_tables() {
        let canvas = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                Markdown(content: "| Col |\n| --- |\n| \x1b[31mred\x1b[0m |".to_string())
            }
        }
        .render(None);

        assert!(canvas.to_string().contains("│ red │"));
        assert_eq!(
            canvas.resolved_text_style(2, 3).unwrap().color,
            Some(Color::DarkRed)
        );
    }

    #[test]
    fn markdown_component_dim_color_wins_over_bold_ansi() {
        let canvas = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                Markdown(content: "\x1b[1mbold\x1b[0m".to_string(), dim_color: true)
            }
        }
        .render(None);

        assert_eq!(canvas.to_string(), "bold\n");
        assert_eq!(
            canvas.resolved_text_style(0, 0).unwrap().weight,
            Weight::Light
        );
    }

    #[test]
    fn markdown_strips_prompt_xml_tags() {
        let input = "before\n<context>secret</context>\nafter";
        assert_eq!(strip_prompt_xml_tags(input), "before\nafter");
    }

    #[test]
    fn markdown_wrap_uses_terminal_columns_for_wide_chars() {
        let wrapped = wrap_markdown_line("你好 world", 6);
        assert_eq!(wrapped, vec!["你好", "world"]);
    }
}
