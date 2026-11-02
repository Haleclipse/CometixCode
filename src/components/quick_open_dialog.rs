//! Maps to: CC `components/QuickOpenDialog.tsx`.
//!
//! Implements the explicit quick-open flow: project file discovery, bounded
//! previews, fuzzy filtering, insert callbacks, and raw-mode-safe external
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

const VISIBLE_RESULTS: usize = 8;
const PREVIEW_LINES: usize = 20;
const MAX_DISCOVERED_FILES: usize = 20_000;

fn discover_quick_open_files(root: &Path) -> Vec<String> {
    fn walk(root: &Path, directory: &Path, files: &mut Vec<String>) {
        if files.len() >= MAX_DISCOVERED_FILES {
            return;
        }
        let Ok(entries) = std::fs::read_dir(directory) else {
            return;
        };
        let mut entries = entries.filter_map(Result::ok).collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            if files.len() >= MAX_DISCOVERED_FILES {
                break;
            }
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if path.is_dir() {
                if matches!(name.as_ref(), ".git" | "node_modules" | "target") {
                    continue;
                }
                walk(root, &path, files);
            } else if path.is_file()
                && let Ok(relative) = path.strip_prefix(root)
            {
                files.push(relative.to_string_lossy().replace('\\', "/"));
            }
        }
    }

    let mut files = Vec::new();
    walk(root, root, &mut files);
    files
}

fn read_quick_open_preview(root: &Path, relative: &str, max_lines: usize) -> String {
    let path = root.join(relative);
    let Ok(file) = std::fs::File::open(path) else {
        return "(preview unavailable)".to_string();
    };
    std::io::BufReader::new(file)
        .lines()
        .take(max_lines)
        .filter_map(Result::ok)
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QuickOpenPreview {
    pub path: String,
    pub content: String,
}

impl QuickOpenPreview {
    pub fn new(path: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            content: content.into(),
        }
    }
}

#[derive(Default, Props)]
pub struct QuickOpenDialogProps<'a> {
    pub on_done: HandlerMut<'a, ()>,
    pub on_insert: HandlerMut<'a, String>,
    pub initial_query: Option<String>,
    /// Snapshot of file paths that the official `generateFileSuggestions(q,
    /// true)` would return. Directories should be supplied with a trailing path
    /// separator if callers want them filtered like the official component.
    pub results: Vec<String>,
    pub previews: Vec<QuickOpenPreview>,
}

/// Maps to: CC `QuickOpenDialog.tsx` separator normalization and directory
/// filtering before `setResults(paths)`.
pub fn normalize_quick_open_path(path: &str) -> String {
    path.replace('\\', "/")
}

/// Maps to: CC `QuickOpenDialog.tsx` empty-query handling plus the fuzzy file
/// suggestion result shape. The real fuzzy ranking lives in
/// `generateFileSuggestions`; this pure boundary keeps exact-contains before a
/// simple subsequence fallback for snapshot tests and future callers.
pub fn filter_quick_open_paths(paths: &[String], query: &str) -> Vec<String> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Vec::new();
    }

    let mut exact = Vec::new();
    let mut fuzzy = Vec::new();
    for path in paths {
        let normalized = normalize_quick_open_path(path);
        if normalized.ends_with('/') || normalized.ends_with('\\') {
            continue;
        }
        let lower = normalized.to_lowercase();
        if lower.contains(&q) {
            exact.push(normalized);
        } else if is_subsequence(&lower, &q) {
            fuzzy.push(normalized);
        }
    }
    exact.extend(fuzzy);
    exact
}

fn is_subsequence(text: &str, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let mut query_chars = query.chars();
    let mut wanted = query_chars.next();
    for ch in text.chars() {
        if Some(ch) == wanted {
            wanted = query_chars.next();
            if wanted.is_none() {
                return true;
            }
        }
    }
    false
}

pub fn quick_open_empty_message(query: &str) -> &'static str {
    if query.is_empty() {
        "Start typing to search…"
    } else {
        "No matching files"
    }
}

pub fn quick_open_visible_results(rows: u16) -> usize {
    VISIBLE_RESULTS.min((usize::from(rows)).saturating_sub(14).max(4))
}

pub fn quick_open_preview_on_right(columns: u16) -> bool {
    columns >= 120
}

fn preview_for_path(path: &str, previews: &[QuickOpenPreview]) -> Option<String> {
    previews
        .iter()
        .find(|preview| preview.path == path)
        .map(|preview| preview.content.clone())
}

fn quick_open_preview_text(
    path: &str,
    content: Option<String>,
    query: &str,
    preview_width: usize,
    effective_preview_lines: usize,
) -> String {
    let Some(content) = content else {
        return "Loading preview…".to_string();
    };

    let mut lines = vec![truncate_path_middle(path, preview_width)];
    lines.extend(
        content
            .split('\n')
            .take(effective_preview_lines)
            .map(|line| truncate_to_width(line, preview_width)),
    );
    if !query.is_empty() {
        lines.push(format!("query: {query}"));
    }
    lines.join("\n")
}

fn quick_open_items(
    paths: &[String],
    previews: &[QuickOpenPreview],
    query: &str,
    columns: u16,
    preview_on_right: bool,
) -> Vec<FuzzyPickerItem> {
    let max_path_width = if preview_on_right {
        ((usize::from(columns)).saturating_sub(10) * 4 / 10).max(20)
    } else {
        usize::from(columns).saturating_sub(8).max(20)
    };
    let preview_width = if preview_on_right {
        usize::from(columns)
            .saturating_sub(max_path_width)
            .saturating_sub(14)
            .max(40)
    } else {
        usize::from(columns).saturating_sub(6)
    };
    let effective_preview_lines = if preview_on_right {
        VISIBLE_RESULTS.saturating_sub(1)
    } else {
        PREVIEW_LINES
    };

    paths
        .iter()
        .map(|path| FuzzyPickerItem {
            key: path.clone(),
            label: truncate_path_middle(path, max_path_width),
            description: None,
            preview: Some(quick_open_preview_text(
                path,
                preview_for_path(path, previews),
                query,
                preview_width,
                effective_preview_lines,
            )),
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum QuickOpenAction {
    Done,
    Insert(String),
}

/// Maps to: CC `components/QuickOpenDialog.tsx` `QuickOpenDialog`.
#[component]
pub fn QuickOpenDialog<'a>(
    props: &mut QuickOpenDialogProps<'a>,
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
    let mut pending_action = hooks.use_state(|| Option::<QuickOpenAction>::None);
    let root = hooks.use_const(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let root_path = root.clone();
    let discovered_paths = hooks.use_state(|| {
        if props.results.is_empty() {
            discover_quick_open_files(&root_path)
        } else {
            props.results.clone()
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
            pending_action.set(Some(QuickOpenAction::Done));
        }
    });

    let pending = {
        let action = pending_action.read();
        action.clone()
    };
    if let Some(action) = pending {
        pending_action.set(None);
        match action {
            QuickOpenAction::Done => (props.on_done)(()),
            QuickOpenAction::Insert(text) => {
                (props.on_insert)(text);
                (props.on_done)(());
            }
        }
    }

    let (columns, rows) = hooks.use_terminal_size();
    let query = input.text();
    let preview_on_right = quick_open_preview_on_right(columns);
    let filtered_paths = filter_quick_open_paths(&discovered_paths.read(), &query);
    let focused_path = filtered_paths.get(focused_index.get()).cloned();
    let mut previews = props.previews.clone();
    if let Some(path) = focused_path.as_deref()
        && !previews.iter().any(|preview| preview.path == path)
    {
        previews.push(QuickOpenPreview::new(
            path,
            read_quick_open_preview(&root_path, path, PREVIEW_LINES),
        ));
    }
    let items = quick_open_items(
        &filtered_paths,
        &previews,
        &query,
        columns,
        preview_on_right,
    );
    let item_count = items.len();

    hooks.use_terminal_events({
        let mut focused_index = focused_index;
        let mut pending_action = pending_action;
        let filtered_paths = filtered_paths.clone();
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
                KeyCode::Esc => pending_action.set(Some(QuickOpenAction::Done)),
                KeyCode::Char('c' | 'C' | 'd' | 'D') if ctrl => {
                    pending_action.set(Some(QuickOpenAction::Done));
                }
                KeyCode::Up | KeyCode::Char('p') if matches!(code, KeyCode::Up) || ctrl => {
                    focused_index
                        .set((focused_index.get() + 1).min(filtered_paths.len().saturating_sub(1)));
                }
                KeyCode::Down | KeyCode::Char('n') if matches!(code, KeyCode::Down) || ctrl => {
                    focused_index.set(focused_index.get().saturating_sub(1));
                }
                KeyCode::Enter => {
                    if let Some(path) = filtered_paths.get(focused_index.get())
                        && editor_sender.try_send(root_path.join(path)).is_err()
                    {
                        pending_action.set(Some(QuickOpenAction::Done));
                    }
                }
                KeyCode::Tab => {
                    if let Some(path) = filtered_paths.get(focused_index.get()) {
                        let text = if shift {
                            format!("{path} ")
                        } else {
                            format!("@{path} ")
                        };
                        pending_action.set(Some(QuickOpenAction::Insert(text)));
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
            title: "Quick Open".to_string(),
            placeholder: Some("Type to search files…".to_string()),
            query: query.clone(),
            cursor_offset: Some(input.offset()),
            items: items,
            focused_index: focused_index.get().min(item_count.saturating_sub(1)),
            visible_count: Some(quick_open_visible_results(rows) as u32),
            direction: FuzzyPickerDirection::Up,
            preview_position: if preview_on_right { FuzzyPickerPreviewPosition::Right } else { FuzzyPickerPreviewPosition::Bottom },
            empty_message: Some(quick_open_empty_message(&query).to_string()),
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
    fn quick_open_helpers_match_official_empty_and_path_filtering() {
        assert_eq!(quick_open_empty_message(""), "Start typing to search…");
        assert_eq!(quick_open_empty_message("src"), "No matching files");
        assert_eq!(quick_open_visible_results(24), 8);
        assert_eq!(quick_open_visible_results(16), 4);
        assert!(quick_open_preview_on_right(120));
        assert!(!quick_open_preview_on_right(119));

        let paths = vec![
            "src/main.rs".to_string(),
            "src/components/".to_string(),
            "src\\lib.rs".to_string(),
            "README.md".to_string(),
        ];
        assert_eq!(
            filter_quick_open_paths(&paths, "sr"),
            vec!["src/main.rs".to_string(), "src/lib.rs".to_string()]
        );
        assert!(filter_quick_open_paths(&paths, "").is_empty());
    }

    #[test]
    fn quick_open_dialog_renders_picker_results_preview_and_hints() {
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                QuickOpenDialog(
                    initial_query: Some("main".to_string()),
                    results: vec!["src/main.rs".to_string(), "src/lib.rs".to_string()],
                    previews: vec![QuickOpenPreview::new("src/main.rs", "fn main() {}")],
                )
            }
        }
        .render(Some(140))
        .to_string();

        assert!(text.contains("Quick Open"), "canvas=\n{text}");
        assert!(text.contains("src/main.rs"), "canvas=\n{text}");
        assert!(text.contains("fn main() {}"), "canvas=\n{text}");
        assert!(text.contains("Enter to open"), "canvas=\n{text}");
        assert!(text.contains("Tab to mention"), "canvas=\n{text}");
    }

    #[test]
    fn quick_open_dialog_tab_inserts_mention_callback_only() {
        let inserts = Arc::new(Mutex::new(Vec::<String>::new()));
        let done = Arc::new(Mutex::new(0usize));
        let inserts_for_handler = Arc::clone(&inserts);
        let done_for_handler = Arc::clone(&done);

        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(*theme::current())) {
                    QuickOpenDialog(
                        initial_query: Some("main".to_string()),
                        results: vec!["src/main.rs".to_string()],
                        on_insert: move |text| inserts_for_handler.lock().expect("inserts mutex").push(text),
                        on_done: move |_| *done_for_handler.lock().expect("done mutex") += 1,
                    )
                }
            };
            let mut render_loop = Box::pin(
                app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(stream::iter(vec![key(KeyCode::Tab)]))
                        .with_size(140, 24),
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
            &["@src/main.rs ".to_string()]
        );
        assert_eq!(*done.lock().expect("done mutex"), 1);
    }

    #[test]
    fn quick_open_dialog_shift_tab_inserts_plain_path() {
        let inserts = Arc::new(Mutex::new(Vec::<String>::new()));
        let inserts_for_handler = Arc::clone(&inserts);

        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(*theme::current())) {
                    QuickOpenDialog(
                        initial_query: Some("main".to_string()),
                        results: vec!["src/main.rs".to_string()],
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
                    .with_size(140, 24),
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
            &["src/main.rs ".to_string()]
        );
    }
}
