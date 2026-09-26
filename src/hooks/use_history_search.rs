//! Maps to: CC `hooks/useHistorySearch.ts` (inline Ctrl-R search subset).
//! Official Claude Code binds Ctrl-R to reverse prompt-history search. While
//! the search is active, the query is edited in `HistorySearchInput` (its
//! TextInput writes the `query` State this hook returns, CC's
//! `setHistoryQuery`), a changed query restarts the search, the prompt display
//! is temporarily replaced by the newest matching history entry, Ctrl-R
//! resumes to the next match, Esc/Tab accept, Ctrl-C cancels, Enter executes,
//! and backspace on an empty query cancels.

use crate::keybindings::keybinding_context::KeybindingRuntime;
use crate::keybindings::types::ContextName;
use crate::keybindings::use_keybinding::{KeybindingHandlers, use_keybinding, use_keybindings};
use crate::utils::cursor::clamp_cursor;
use crate::utils::prompt_history;
use iocraft::prelude::*;

/// Maps to: the `HISTORY_PICKER` build feature, on in the external build
/// (rebuild `scripts/build.ts:49`). With it, ctrl+r opens PromptInput's
/// history picker and this inline search's own `history:search` binding is
/// inactive (`useHistorySearch.ts:236-241`), so the inline search — and the
/// footer's `HistorySearchInput` — is only reachable in a build without the
/// picker, exactly as in CC.
const HISTORY_PICKER: bool = true;

#[derive(Clone, Copy)]
pub struct UseHistorySearchOptions {
    pub input: State<String>,
    pub cursor_offset: State<usize>,
    pub focus: bool,
}

#[derive(Clone, Copy)]
struct SearchCells {
    is_searching: State<bool>,
    query: State<String>,
    failed_match: State<bool>,
    current_match: State<Option<String>>,
    original_input: State<String>,
    original_cursor_offset: State<usize>,
    seen: State<Vec<String>>,
    pending_submit: State<Option<String>>,
}

pub struct HistorySearchState {
    pub is_searching: State<bool>,
    pub query: State<String>,
    pub failed_match: State<bool>,
    pending_submit: State<Option<String>>,
}

impl HistorySearchState {
    pub fn is_active(&self) -> bool {
        self.is_searching.get()
    }

    pub fn take_pending_submit(&mut self) -> Option<String> {
        let value = self.pending_submit.read().clone();
        if value.is_some() {
            self.pending_submit.set(None);
        }
        value
    }
}

pub fn use_history_search(
    hooks: &mut Hooks,
    options: UseHistorySearchOptions,
) -> HistorySearchState {
    let cells = SearchCells {
        is_searching: hooks.use_state(|| false),
        query: hooks.use_state(String::new),
        failed_match: hooks.use_state(|| false),
        current_match: hooks.use_state(|| Option::<String>::None),
        original_input: hooks.use_state(String::new),
        original_cursor_offset: hooks.use_state(|| 0usize),
        seen: hooks.use_state(Vec::<String>::new),
        pending_submit: hooks.use_state(|| Option::<String>::None),
    };

    let runtime = hooks
        .try_use_context::<KeybindingRuntime>()
        .map(|runtime| runtime.clone());
    use_keybinding(
        hooks,
        runtime.clone(),
        "history:search",
        ContextName::Global,
        // CC `isActive: feature('HISTORY_PICKER') ? false : !isSearching`.
        move || !HISTORY_PICKER && options.focus && !cells.is_searching.get(),
        move || {
            start(cells, options);
            true
        },
    );
    let handlers: KeybindingHandlers = vec![
        (
            "historySearch:next".to_string(),
            Box::new(move || {
                search(cells, options, cells.query.read().clone(), true);
                true
            }),
        ),
        (
            "historySearch:accept".to_string(),
            Box::new(move || {
                accept(cells, options);
                true
            }),
        ),
        (
            "historySearch:cancel".to_string(),
            Box::new(move || {
                cancel(cells, options);
                true
            }),
        ),
        (
            "historySearch:execute".to_string(),
            Box::new(move || {
                execute(cells, options);
                true
            }),
        ),
    ];
    use_keybindings(
        hooks,
        runtime,
        handlers,
        ContextName::HistorySearch,
        move || options.focus && cells.is_searching.get(),
    );

    // Maps to: CC `useHistorySearch.ts:258-280` `handleKeyDown`, subscribed
    // through `useInput({ isActive: isSearching })`: backspace on an empty
    // query cancels the search. Everything else a user types edits the query
    // in `HistorySearchInput`'s TextInput (`onChange={setHistoryQuery}`), not
    // here. `useInput` sees every key, including ones that TextInput consumed,
    // so this is a plain subscription; the query it tests is the one this
    // render saw, as CC's closure over `historyQuery` is — a backspace that
    // deletes the last character does not also cancel.
    let query_at_render = cells.query.read().clone();
    hooks.use_terminal_events(move |event| {
        if !options.focus || !cells.is_searching.get() {
            return;
        }
        if let TerminalEvent::Key(KeyEvent {
            code: KeyCode::Backspace,
            kind,
            ..
        }) = event
        {
            if kind != KeyEventKind::Release && query_at_render.is_empty() {
                cancel(cells, options);
            }
        }
    });

    // Maps to: CC `useHistorySearch.ts:285-296` — "Reset history search when
    // query changes": a new query restarts the search from the newest entry.
    // `searchHistory` returns early while not searching (:75-77).
    let query_for_effect = cells.query.read().clone();
    hooks.use_effect(
        move || {
            if cells.is_searching.get() {
                search(cells, options, cells.query.read().clone(), false);
            }
        },
        query_for_effect,
    );

    HistorySearchState {
        is_searching: cells.is_searching,
        query: cells.query,
        failed_match: cells.failed_match,
        pending_submit: cells.pending_submit,
    }
}

fn start(mut cells: SearchCells, options: UseHistorySearchOptions) {
    cells.is_searching.set(true);
    cells.query.set(String::new());
    cells.failed_match.set(false);
    cells.current_match.set(None);
    cells.original_input.set(options.input.read().clone());
    cells
        .original_cursor_offset
        .set(options.cursor_offset.get());
    cells.seen.set(Vec::new());
}

fn accept(cells: SearchCells, mut options: UseHistorySearchOptions) {
    if let Some(value) = cells.current_match.read().clone() {
        let offset = options.cursor_offset.get().min(value.len());
        options.input.set(value.clone());
        options.cursor_offset.set(clamp_cursor(&value, offset));
    } else {
        restore_original(cells, options);
    }
    clear(cells);
}

fn cancel(cells: SearchCells, options: UseHistorySearchOptions) {
    restore_original(cells, options);
    clear(cells);
}

fn execute(mut cells: SearchCells, options: UseHistorySearchOptions) {
    let query = cells.query.read().clone();
    let text = if query.is_empty() {
        Some(cells.original_input.read().clone())
    } else {
        cells.current_match.read().clone()
    };

    if let Some(text) = text.filter(|text| !text.trim().is_empty()) {
        cells.pending_submit.set(Some(text));
    }
    clear(cells);

    // PromptInput clears the input once it consumes pending_submit. If no match
    // was executable, keep the current visual state until the next key.
    let _ = options;
}

fn search(
    mut cells: SearchCells,
    mut options: UseHistorySearchOptions,
    query: String,
    resume: bool,
) {
    if query.is_empty() {
        restore_original(cells, options);
        cells.current_match.set(None);
        cells.failed_match.set(false);
        cells.seen.set(Vec::new());
        return;
    }

    if !resume {
        cells.seen.set(Vec::new());
    }

    let seen = cells.seen.read().clone();
    if let Some(entry) = prompt_history::find_history_match(&query, &seen) {
        let match_offset = entry.display.rfind(&query).unwrap_or(entry.display.len());
        let mut next_seen = seen;
        next_seen.push(entry.display.clone());
        cells.seen.set(next_seen);
        cells.failed_match.set(false);
        cells.current_match.set(Some(entry.display.clone()));
        options
            .cursor_offset
            .set(clamp_cursor(&entry.display, match_offset));
        options.input.set(entry.display);
    } else {
        // CC keeps the last successful match visible and marks the query as
        // failed when no further result exists.
        cells.failed_match.set(true);
    }
}

fn restore_original(cells: SearchCells, mut options: UseHistorySearchOptions) {
    let original = cells.original_input.read().clone();
    let cursor = cells.original_cursor_offset.get();
    options.cursor_offset.set(clamp_cursor(&original, cursor));
    options.input.set(original);
}

fn clear(mut cells: SearchCells) {
    cells.is_searching.set(false);
    cells.query.set(String::new());
    cells.failed_match.set(false);
    cells.current_match.set(None);
    cells.original_input.set(String::new());
    cells.original_cursor_offset.set(0);
    cells.seen.set(Vec::new());
}
