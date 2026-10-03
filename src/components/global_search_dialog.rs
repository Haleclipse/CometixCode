//! Maps to: CC `components/GlobalSearchDialog.tsx`.
//!
//! Runs an explicit project search through direct-argv ripgrep, loads bounded
//! preview ranges, supports insertion, and performs raw-mode-safe external
//! editor handoff. Analytics are intentionally omitted.

use crate::components::design_system::fuzzy_picker::{
    FuzzyPicker, FuzzyPickerDirection, FuzzyPickerItem, FuzzyPickerPreviewPosition,
};
use crate::hooks::use_search_input::SearchInput;
use crate::utils::prompt_editor::ExternalEditorRuntime;
use crate::utils::truncate::{truncate_path_middle, truncate_to_width};
use iocraft::prelude::*;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const VISIBLE_RESULTS: usize = 12;
const PREVIEW_CONTEXT_LINES: usize = 4;
const MAX_TOTAL_MATCHES: usize = 500;

fn run_project_search(root: &Path, query: &str) -> (Vec<GlobalSearchMatch>, bool) {
    if query.trim().is_empty() {
        return (Vec::new(), false);
    }
    let mut command = std::process::Command::new("rg");
    // CC inherits process.env (ripgrep.ts spawn without env); the carrier is its counterpart.
    crate::utils::subprocess_env::apply_process_env_std(&mut command);
    let output = command
        .current_dir(root)
        .args([
            "--line-number",
            "--no-heading",
            "--color=never",
            "--fixed-strings",
            "--max-filesize=2M",
            "--",
            query,
            ".",
        ])
        .output();
    let Ok(output) = output else {
        return (Vec::new(), false);
    };
    let mut parsed = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(parse_ripgrep_line)
        .map(|mut item| {
            item.file = item.file.trim_start_matches("./").replace('\\', "/");
            item
        })
        .take(MAX_TOTAL_MATCHES + 1)
        .collect::<Vec<_>>();
    let truncated = parsed.len() > MAX_TOTAL_MATCHES;
    parsed.truncate(MAX_TOTAL_MATCHES);
    (parsed, truncated)
}

fn read_global_search_preview(root: &Path, item: &GlobalSearchMatch) -> GlobalSearchPreview {
    let path = root.join(&item.file);
    let start = item.line.saturating_sub(PREVIEW_CONTEXT_LINES + 1);
    let content = std::fs::File::open(path)
        .ok()
        .map(|file| {
            std::io::BufReader::new(file)
                .lines()
                .skip(start)
                .take(PREVIEW_CONTEXT_LINES * 2 + 1)
                .filter_map(Result::ok)
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_else(|| "(preview unavailable)".to_string());
    GlobalSearchPreview::new(item.file.clone(), item.line, content)
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GlobalSearchMatch {
    pub file: String,
    pub line: usize,
    pub text: String,
}

impl GlobalSearchMatch {
    pub fn new(file: impl Into<String>, line: usize, text: impl Into<String>) -> Self {
        Self {
            file: file.into(),
            line,
            text: text.into(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GlobalSearchPreview {
    pub file: String,
    pub line: usize,
    pub content: String,
}

impl GlobalSearchPreview {
    pub fn new(file: impl Into<String>, line: usize, content: impl Into<String>) -> Self {
        Self {
            file: file.into(),
            line,
            content: content.into(),
        }
    }
}

#[derive(Default, Props)]
pub struct GlobalSearchDialogProps<'a> {
    pub on_done: HandlerMut<'a, ()>,
    pub on_insert: HandlerMut<'a, String>,
    pub initial_query: Option<String>,
    /// Snapshot of ripgrep matches for the active query. This slice does not
    /// spawn ripgrep; callers/future runtime own that effect.
    pub matches: Vec<GlobalSearchMatch>,
    pub previews: Vec<GlobalSearchPreview>,
    pub truncated: bool,
    pub is_searching: bool,
}

/// Maps to: CC `GlobalSearchDialog.tsx` `matchKey`.
pub fn global_search_match_key(m: &GlobalSearchMatch) -> String {
    format!("{}:{}", m.file, m.line)
}

/// Maps to: CC `GlobalSearchDialog.tsx` `parseRipgrepLine`.
pub fn parse_ripgrep_line(line: &str) -> Option<GlobalSearchMatch> {
    for (colon_index, ch) in line.char_indices() {
        if ch != ':' {
            continue;
        }
        let after_colon = colon_index + ch.len_utf8();
        let mut digit_end = after_colon;
        let mut saw_digit = false;
        for (offset, candidate) in line[after_colon..].char_indices() {
            if candidate.is_ascii_digit() {
                saw_digit = true;
                digit_end = after_colon + offset + candidate.len_utf8();
                continue;
            }
            if candidate == ':' && saw_digit {
                let file = &line[..colon_index];
                if file.is_empty() {
                    return None;
                }
                let line_num = line[after_colon..digit_end].parse::<usize>().ok()?;
                let text_start = after_colon + offset + candidate.len_utf8();
                return Some(GlobalSearchMatch::new(file, line_num, &line[text_start..]));
            }
            break;
        }
    }
    None
}

/// Maps to: CC `GlobalSearchDialog.tsx` client-filtering of existing results
/// while ripgrep walks.
pub fn filter_global_search_matches(
    matches: &[GlobalSearchMatch],
    query: &str,
) -> Vec<GlobalSearchMatch> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Vec::new();
    }
    matches
        .iter()
        .filter(|m| m.text.to_lowercase().contains(&q))
        .take(MAX_TOTAL_MATCHES)
        .cloned()
        .collect()
}

pub fn global_search_match_label(count: usize, truncated: bool, is_searching: bool) -> String {
    if count == 0 {
        " ".to_string()
    } else {
        format!(
            "{}{} matches{}",
            count,
            if truncated { "+" } else { "" },
            if is_searching { "…" } else { "" }
        )
    }
}

pub fn global_search_empty_message(query: &str, is_searching: bool) -> &'static str {
    if is_searching {
        "Searching…"
    } else if query.is_empty() {
        "Type to search…"
    } else {
        "No matches"
    }
}

pub fn global_search_visible_results(rows: u16) -> usize {
    VISIBLE_RESULTS.min((usize::from(rows)).saturating_sub(14).max(4))
}

pub fn global_search_preview_on_right(columns: u16) -> bool {
    columns >= 140
}

fn preview_for_match(
    m: &GlobalSearchMatch,
    previews: &[GlobalSearchPreview],
) -> Option<GlobalSearchPreview> {
    previews
        .iter()
        .find(|preview| preview.file == m.file && preview.line == m.line)
        .cloned()
}

fn global_search_preview_text(
    m: &GlobalSearchMatch,
    preview: Option<GlobalSearchPreview>,
    preview_width: usize,
) -> String {
    let Some(preview) = preview else {
        return "Loading…".to_string();
    };
    let mut lines = vec![format!(
        "{}:{}",
        truncate_path_middle(&m.file, preview_width),
        m.line
    )];
    lines.extend(
        preview
            .content
            .split('\n')
            .take(PREVIEW_CONTEXT_LINES * 2 + 1)
            .map(|line| truncate_to_width(line, preview_width)),
    );
    lines.join("\n")
}

fn global_search_items(
    matches: &[GlobalSearchMatch],
    previews: &[GlobalSearchPreview],
    columns: u16,
    preview_on_right: bool,
) -> Vec<FuzzyPickerItem> {
    let list_width = if preview_on_right {
        ((usize::from(columns)).saturating_sub(10)) / 2
    } else {
        usize::from(columns).saturating_sub(8)
    };
    let max_path_width = (list_width * 4 / 10).max(20);
    let max_text_width = list_width
        .saturating_sub(max_path_width)
        .saturating_sub(4)
        .max(20);
    let preview_width = if preview_on_right {
        usize::from(columns)
            .saturating_sub(list_width)
            .saturating_sub(14)
            .max(40)
    } else {
        usize::from(columns).saturating_sub(6)
    };

    matches
        .iter()
        .map(|m| FuzzyPickerItem {
            key: global_search_match_key(m),
            label: format!(
                "{}:{} {}",
                truncate_path_middle(&m.file, max_path_width),
                m.line,
                truncate_to_width(m.text.trim_start(), max_text_width)
            ),
            description: None,
            preview: Some(global_search_preview_text(
                m,
                preview_for_match(m, previews),
                preview_width,
            )),
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum GlobalSearchAction {
    Done,
    Insert(String),
}

/// Maps to: CC `components/GlobalSearchDialog.tsx` `GlobalSearchDialog`.
#[component]
pub fn GlobalSearchDialog<'a>(
    props: &mut GlobalSearchDialogProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let initial_query = props.initial_query.clone().unwrap_or_default();
    let initial_query_for_query = initial_query.clone();
    let initial_query_len = initial_query.encode_utf16().count();
    let mut input = SearchInput {
        columns: hooks.use_terminal_size().0 as usize,
        query: hooks.use_state(move || initial_query_for_query.clone()),
        cursor: hooks.use_state(move || initial_query_len),
    };
    let mut focused_index = hooks.use_state(|| 0usize);
    let mut pending_action = hooks.use_state(|| Option::<GlobalSearchAction>::None);
    let root = hooks.use_const(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let root_path = root.clone();
    let mut searched_query = hooks.use_state(String::new);
    let mut search_generation = hooks.use_state(|| 0u64);
    let mut live_matches = hooks.use_state(Vec::<GlobalSearchMatch>::new);
    let mut live_truncated = hooks.use_state(|| false);
    let mut live_searching = hooks.use_state(|| false);
    let search_results = hooks.use_const(|| {
        Arc::new(async_channel::unbounded::<(
            u64,
            Vec<GlobalSearchMatch>,
            bool,
        )>())
    });
    let search_receiver = search_results.1.clone();
    hooks.use_future(async move {
        while let Ok((generation, matches, truncated)) = search_receiver.recv().await {
            if generation == search_generation.get() {
                live_matches.set(matches);
                live_truncated.set(truncated);
                live_searching.set(false);
            }
        }
    });
    let editor_runtime = hooks
        .try_use_context::<ExternalEditorRuntime>()
        .map(|runtime| *runtime);
    let editor_channel = hooks.use_const(|| Arc::new(async_channel::unbounded::<PathBuf>()));
    let editor_receiver = editor_channel.1.clone();
    hooks.use_future(async move {
        while let Ok(path) = editor_receiver.recv().await {
            if let Some(runtime) = editor_runtime {
                let _ = runtime.edit_file(&path).await;
            }
            pending_action.set(Some(GlobalSearchAction::Done));
        }
    });

    let pending = {
        let action = pending_action.read();
        action.clone()
    };
    if let Some(action) = pending {
        pending_action.set(None);
        match action {
            GlobalSearchAction::Done => (props.on_done)(()),
            GlobalSearchAction::Insert(text) => {
                (props.on_insert)(text);
                (props.on_done)(());
            }
        }
    }

    let (columns, rows) = hooks.use_terminal_size();
    let query = input.text();
    if props.matches.is_empty() && searched_query.read().as_str() != query {
        searched_query.set(query.clone());
        let generation = search_generation.get().wrapping_add(1);
        search_generation.set(generation);
        if query.trim().is_empty() {
            live_matches.set(Vec::new());
            live_truncated.set(false);
            live_searching.set(false);
        } else {
            live_searching.set(true);
            let root = root_path.clone();
            let query = query.clone();
            let sender = search_results.0.clone();
            std::thread::spawn(move || {
                let (matches, truncated) = run_project_search(&root, &query);
                let _ = sender.send_blocking((generation, matches, truncated));
            });
        }
    }
    let source_matches = if props.matches.is_empty() {
        live_matches.read().clone()
    } else {
        props.matches.clone()
    };
    let truncated = props.truncated || live_truncated.get();
    let is_searching = props.is_searching || live_searching.get();
    let preview_on_right = global_search_preview_on_right(columns);
    let filtered_matches = filter_global_search_matches(&source_matches, &query);
    let mut previews = props.previews.clone();
    if let Some(item) = filtered_matches.get(focused_index.get()) {
        if !previews
            .iter()
            .any(|preview| preview.file == item.file && preview.line == item.line)
        {
            previews.push(read_global_search_preview(&root_path, item));
        }
    }
    let items = global_search_items(&filtered_matches, &previews, columns, preview_on_right);
    let item_count = items.len();

    hooks.use_terminal_events({
        let mut focused_index = focused_index;
        let mut pending_action = pending_action;
        let filtered_matches = filtered_matches.clone();
        let root_path = root_path.clone();
        let editor_sender = editor_channel.0.clone();
        move |event| {
            let TerminalEvent::Key(KeyEvent {
                code,
                kind,
                modifiers,
                ..
            }) = event
            else {
                return;
            };
            if kind == KeyEventKind::Release {
                return;
            }

            let ctrl = modifiers.contains(KeyModifiers::CONTROL);
            let shift = modifiers.contains(KeyModifiers::SHIFT);
            match code {
                KeyCode::Esc => pending_action.set(Some(GlobalSearchAction::Done)),
                KeyCode::Char('c' | 'C' | 'd' | 'D') if ctrl => {
                    pending_action.set(Some(GlobalSearchAction::Done));
                }
                KeyCode::Up | KeyCode::Char('p') if matches!(code, KeyCode::Up) || ctrl => {
                    focused_index.set(
                        (focused_index.get() + 1).min(filtered_matches.len().saturating_sub(1)),
                    );
                }
                KeyCode::Down | KeyCode::Char('n') if matches!(code, KeyCode::Down) || ctrl => {
                    focused_index.set(focused_index.get().saturating_sub(1));
                }
                KeyCode::Enter => {
                    if let Some(item) = filtered_matches.get(focused_index.get()) {
                        if editor_sender.try_send(root_path.join(&item.file)).is_err() {
                            pending_action.set(Some(GlobalSearchAction::Done));
                        }
                    }
                }
                KeyCode::Tab => {
                    if let Some(m) = filtered_matches.get(focused_index.get()) {
                        let text = if shift {
                            format!("{}:{} ", m.file, m.line)
                        } else {
                            format!("@{}#L{} ", m.file, m.line)
                        };
                        pending_action.set(Some(GlobalSearchAction::Insert(text)));
                    }
                }
                _ => {
                    if input.handle_edit_key(&code, &modifiers) {
                        focused_index.set(0);
                    }
                }
            }
        }
    });

    element! {
        FuzzyPicker(
            title: "Global Search".to_string(),
            placeholder: Some("Type to search…".to_string()),
            query: query.clone(),
            cursor_offset: Some(input.offset()),
            items: items,
            focused_index: focused_index.get().min(item_count.saturating_sub(1)),
            visible_count: Some(global_search_visible_results(rows) as u32),
            direction: FuzzyPickerDirection::Up,
            preview_position: if preview_on_right { FuzzyPickerPreviewPosition::Right } else { FuzzyPickerPreviewPosition::Bottom },
            empty_message: Some(global_search_empty_message(&query, is_searching).to_string()),
            match_label: Some(global_search_match_label(filtered_matches.len(), truncated, is_searching)),
            select_action: Some("open in editor".to_string()),
            tab_action: Some("mention".to_string()),
            shift_tab_action: Some("insert path".to_string()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;
    use futures::{StreamExt, stream};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn key(code: KeyCode) -> TerminalEvent {
        TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code))
    }

    fn modified_key(code: KeyCode, modifiers: KeyModifiers) -> TerminalEvent {
        let mut event = KeyEvent::new(KeyEventKind::Press, code);
        event.modifiers = modifiers;
        TerminalEvent::Key(event)
    }

    #[test]
    fn global_search_helpers_match_official_parse_filter_and_labels() {
        assert_eq!(
            parse_ripgrep_line("src/main.rs:12:fn main()"),
            Some(GlobalSearchMatch::new("src/main.rs", 12, "fn main()"))
        );
        assert_eq!(
            parse_ripgrep_line("C:\\repo\\main.rs:7:needle"),
            Some(GlobalSearchMatch::new("C:\\repo\\main.rs", 7, "needle"))
        );
        assert_eq!(parse_ripgrep_line(":7:needle"), None);
        assert_eq!(global_search_match_label(0, false, false), " ");
        assert_eq!(global_search_match_label(42, true, true), "42+ matches…");
        assert_eq!(global_search_empty_message("", false), "Type to search…");
        assert_eq!(global_search_empty_message("needle", false), "No matches");
        assert_eq!(global_search_empty_message("needle", true), "Searching…");
        assert_eq!(global_search_visible_results(24), 10);
        assert!(global_search_preview_on_right(140));

        let matches = vec![
            GlobalSearchMatch::new("a", 1, "Needle here"),
            GlobalSearchMatch::new("b", 2, "other"),
        ];
        assert_eq!(
            filter_global_search_matches(&matches, "needle"),
            vec![GlobalSearchMatch::new("a", 1, "Needle here")]
        );
        assert!(filter_global_search_matches(&matches, "").is_empty());
    }

    #[test]
    fn global_search_dialog_renders_matches_preview_label_and_hints() {
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                GlobalSearchDialog(
                    initial_query: Some("needle".to_string()),
                    matches: vec![GlobalSearchMatch::new("src/main.rs", 12, "let needle = true;")],
                    previews: vec![GlobalSearchPreview::new("src/main.rs", 12, "fn main() {\n  let needle = true;\n}")],
                    truncated: true,
                    is_searching: true,
                )
            }
        }
        .render(Some(160))
        .to_string();

        assert!(text.contains("Global Search"), "canvas=\n{text}");
        assert!(text.contains("src/main.rs:12"), "canvas=\n{text}");
        assert!(text.contains("let needle = true;"), "canvas=\n{text}");
        assert!(text.contains("1+ matches…"), "canvas=\n{text}");
        assert!(text.contains("Enter to open"), "canvas=\n{text}");
        assert!(text.contains("Tab to mention"), "canvas=\n{text}");
    }

    #[test]
    fn global_search_dialog_tab_inserts_file_line_mention_callback_only() {
        let inserts = Arc::new(Mutex::new(Vec::<String>::new()));
        let done = Arc::new(Mutex::new(0usize));
        let inserts_for_handler = Arc::clone(&inserts);
        let done_for_handler = Arc::clone(&done);

        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(*theme::current())) {
                    GlobalSearchDialog(
                        initial_query: Some("needle".to_string()),
                        matches: vec![GlobalSearchMatch::new("src/main.rs", 12, "needle")],
                        on_insert: move |text| inserts_for_handler.lock().expect("inserts mutex").push(text),
                        on_done: move |_| *done_for_handler.lock().expect("done mutex") += 1,
                    )
                }
            };
            let mut render_loop = Box::pin(
                app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(stream::iter(vec![key(KeyCode::Tab)]))
                        .with_size(150, 24),
                ),
            );
            for _ in 0..8 {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(100)).await;
                    None
                })
                .await;
                if next.is_none() {
                    break;
                }
            }
        });

        assert_eq!(
            inserts.lock().expect("inserts mutex").as_slice(),
            &["@src/main.rs#L12 ".to_string()]
        );
        assert_eq!(*done.lock().expect("done mutex"), 1);
    }

    #[test]
    fn global_search_dialog_shift_tab_inserts_plain_file_line() {
        let inserts = Arc::new(Mutex::new(Vec::<String>::new()));
        let inserts_for_handler = Arc::clone(&inserts);

        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(*theme::current())) {
                    GlobalSearchDialog(
                        initial_query: Some("needle".to_string()),
                        matches: vec![GlobalSearchMatch::new("src/main.rs", 12, "needle")],
                        on_insert: move |text| inserts_for_handler.lock().expect("inserts mutex").push(text),
                    )
                }
            };
            let mut render_loop = Box::pin(
                app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(stream::iter(vec![modified_key(
                        KeyCode::Tab,
                        KeyModifiers::SHIFT,
                    )]))
                    .with_size(150, 24),
                ),
            );
            for _ in 0..8 {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(100)).await;
                    None
                })
                .await;
                if next.is_none() {
                    break;
                }
            }
        });

        assert_eq!(
            inserts.lock().expect("inserts mutex").as_slice(),
            &["src/main.rs:12 ".to_string()]
        );
    }
}
