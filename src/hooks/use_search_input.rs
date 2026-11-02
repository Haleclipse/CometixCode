//! Maps to: CC hooks/useSearchInput.ts
//! Text input state for search boxes: query string + cursor offset, with
//! full cursor navigation (Left/Right/Home/End/Backspace/Delete/word-jump).
//! Cursor owns editing, grapheme/word boundaries and NFC normalization; the
//! search hook owns key dispatch and the source-specific raw-query yank-pop.
//! Editing state retains source UTF-16 offsets; offset() projects to the
//! existing SearchBox scalar display carrier. Editing delegates to Cursor.

use crate::utils::kill_ring::{
    KillDirection, get_last_kill, push_to_kill_ring, record_yank, reset_kill_accumulation,
    reset_yank_state, update_yank_len, yank_pop,
};
use iocraft::prelude::*;

/// Search input state — maps to CC useSearchInput return value.
#[derive(Clone, Copy)]
pub struct SearchInput {
    pub query: State<String>,
    pub cursor: State<usize>, // source UTF-16 offset
    pub(crate) columns: usize,
}

/// Maps to: CC `hooks/useSearchInput.ts:17-35#UseSearchInputOptions`.
/// Rust callback transport for per-event options; query/columns stay in SearchInput.
pub struct SearchInputOptions<'a> {
    pub is_active: bool,
    pub on_exit: &'a mut dyn FnMut(),
    pub on_cancel: Option<&'a mut dyn FnMut()>,
    pub on_exit_up: Option<&'a mut dyn FnMut()>,
    pub passthrough_ctrl_keys: &'a [&'a str],
    pub backspace_exits_on_empty: bool,
}

impl SearchInput {
    /// Current query text.
    pub fn text(&self) -> String {
        self.query.read().clone()
    }

    /// Whether the query is empty.
    pub fn is_empty(&self) -> bool {
        self.query.read().is_empty()
    }

    /// Cursor offset (character index).
    pub fn offset(&self) -> usize {
        let query = self.query.read();
        let mut units = 0;
        query
            .chars()
            .take_while(|ch| {
                units += ch.len_utf16();
                units <= self.cursor.get()
            })
            .count()
    }

    /// Reset to empty.
    pub fn clear(&mut self) {
        self.query.set(String::new());
        self.cursor.set(0);
    }

    /// Set the full query (cursor moves to end).
    pub fn set(&mut self, q: String) {
        let len = q.encode_utf16().count();
        self.query.set(q);
        self.cursor.set(len);
    }

    /// Native ownership adapter for the source's per-event Cursor. The state
    /// keeps the original UTF-16 number even when MeasuredText normalizes NFC.
    fn editing_cursor(&self) -> crate::utils::cursor::Cursor {
        crate::utils::cursor::Cursor::from_text_utf16(
            self.query.read().clone(),
            self.columns,
            self.cursor.get(),
        )
    }

    fn apply_cursor(&mut self, cursor: crate::utils::cursor::Cursor) {
        self.cursor.set(cursor.offset_utf16());
        self.query.set(cursor.text().to_string());
    }

    fn apply_cursor_offset(&mut self, cursor: crate::utils::cursor::Cursor) {
        // Source navigation changes only cursorOffset, not the raw query.
        self.cursor.set(cursor.offset_utf16());
    }

    /// Maps to: CC cursor.insert.
    pub fn insert(&mut self, c: char) {
        self.insert_text(&c.to_string());
    }

    /// Maps to: CC cursor.insert (including batched input and paste).
    pub fn insert_text(&mut self, text: &str) {
        self.apply_cursor(self.editing_cursor().insert(text));
    }

    /// Maps to: CC cursor.backspace (a grapheme, not a Unicode scalar).
    pub fn backspace(&mut self) {
        self.apply_cursor(self.editing_cursor().backspace());
    }

    /// Maps to: CC cursor.del.
    pub fn delete(&mut self) {
        self.apply_cursor(self.editing_cursor().del());
    }

    /// Maps to: CC cursor.left.
    pub fn move_left(&mut self) {
        self.apply_cursor_offset(self.editing_cursor().left());
    }

    /// Maps to: CC cursor.right.
    pub fn move_right(&mut self) {
        self.apply_cursor_offset(self.editing_cursor().right());
    }

    /// Maps to: CC home / Ctrl+A.
    pub fn move_home(&mut self) {
        self.cursor.set(0);
    }

    /// Maps to: CC end / Ctrl+E (the raw query length).
    pub fn move_end(&mut self) {
        self.cursor.set(self.query.read().encode_utf16().count());
    }

    /// Maps to: CC useSearchInput.ts Ctrl+K → Cursor.deleteToLineEnd.
    pub fn delete_to_line_end(&mut self) -> String {
        let edit = self.editing_cursor().delete_to_line_end();
        self.apply_cursor(edit.cursor);
        edit.killed
    }

    /// Maps to: CC useSearchInput.ts Ctrl+U → Cursor.deleteToLineStart.
    pub fn delete_to_line_start(&mut self) -> String {
        let edit = self.editing_cursor().delete_to_line_start();
        self.apply_cursor(edit.cursor);
        edit.killed
    }

    /// Maps to: CC cursor.prevWord.
    pub fn move_prev_word(&mut self) {
        self.apply_cursor_offset(self.editing_cursor().prev_word());
    }

    /// Maps to: CC cursor.nextWord.
    pub fn move_next_word(&mut self) {
        self.apply_cursor_offset(self.editing_cursor().next_word());
    }

    /// Maps to: CC Cursor.deleteWordBefore, including atomic Image ranges.
    pub fn delete_word_before(&mut self) -> String {
        let edit = self.editing_cursor().delete_word_before();
        self.apply_cursor(edit.cursor);
        edit.killed
    }

    /// Maps to: CC Alt+D → Cursor.deleteWordAfter; source does not kill-ring it.
    pub fn delete_word_after(&mut self) {
        self.apply_cursor(self.editing_cursor().delete_word_after());
    }

    /// Maps to: CC useSearchInput Ctrl+Y. Ring offsets use source UTF-16 units.
    pub fn yank(&mut self) {
        let text = get_last_kill();
        if text.is_empty() {
            return;
        }
        let cursor = self.editing_cursor();
        let start = cursor.offset_utf16();
        let next = cursor.insert(&text);
        record_yank(start, text.encode_utf16().count());
        self.apply_cursor(next);
    }

    /// Maps to: CC useSearchInput Alt+Y's raw query.slice replacement. Unlike
    /// normal edits, the source does not normalize the replaced query here.
    pub fn yank_pop(&mut self) {
        let Some((text, start, len)) = yank_pop() else {
            return;
        };
        let query: Vec<u16> = self.query.read().encode_utf16().collect();
        let mut replacement = query[..start.min(query.len())].to_vec();
        replacement.extend(text.encode_utf16());
        replacement.extend_from_slice(&query[start.saturating_add(len).min(query.len())..]);
        let length = text.encode_utf16().count();
        update_yank_len(length);
        self.query.set(String::from_utf16_lossy(&replacement));
        self.cursor.set(start + length);
    }

    /// Maps to: CC useSearchInput.ts:116-124. The native caller also invokes
    /// this before exit/navigation branches, which return before editing.
    pub(crate) fn reset_key_state(&self, code: &KeyCode, modifiers: &KeyModifiers) {
        if !is_kill_key(code, modifiers) {
            reset_kill_accumulation();
        }
        if !is_yank_key(code, modifiers) {
            reset_yank_state();
        }
    }

    /// Maps to: CC `hooks/useSearchInput.ts:116-357#handleKeyDown`.
    /// The boolean carries KeyboardEvent.preventDefault; existing editing
    /// dispatch below remains the canonical Cursor/kill-ring implementation.
    pub fn handle_key_down(
        &mut self,
        code: &KeyCode,
        modifiers: &KeyModifiers,
        mut options: SearchInputOptions<'_>,
    ) -> bool {
        if !options.is_active {
            return false;
        }
        let ctrl = modifiers.contains(KeyModifiers::CONTROL);
        let meta = modifiers.contains(KeyModifiers::ALT);
        if ctrl
            && matches!(code,KeyCode::Char(c) if options.passthrough_ctrl_keys.iter().any(|key|*key==c.to_lowercase().to_string()))
        {
            return false;
        }
        match code {
            KeyCode::Enter | KeyCode::Down => {
                self.reset_key_state(code, modifiers);
                (options.on_exit)();
                true
            }
            KeyCode::Up => {
                self.reset_key_state(code, modifiers);
                if let Some(callback) = options.on_exit_up.as_mut() {
                    callback();
                }
                true
            }
            KeyCode::Esc => {
                self.reset_key_state(code, modifiers);
                if let Some(callback) = options.on_cancel.as_mut() {
                    callback();
                } else if !self.is_empty() {
                    self.clear();
                } else {
                    (options.on_exit)();
                }
                true
            }
            KeyCode::Backspace if !meta && self.is_empty() => {
                self.reset_key_state(code, modifiers);
                if options.backspace_exits_on_empty {
                    if let Some(callback) = options.on_cancel.as_mut() {
                        callback();
                    } else {
                        (options.on_exit)();
                    }
                }
                true
            }
            KeyCode::Char('d' | 'D' | 'h' | 'H') if ctrl && self.is_empty() => {
                self.reset_key_state(code, modifiers);
                if matches!(code, KeyCode::Char('d' | 'D')) || options.backspace_exits_on_empty {
                    if let Some(callback) = options.on_cancel.as_mut() {
                        callback();
                    } else {
                        (options.on_exit)();
                    }
                }
                true
            }
            KeyCode::Char('g' | 'G' | 'c' | 'C') if ctrl => {
                self.reset_key_state(code, modifiers);
                if let Some(callback) = options.on_cancel.as_mut() {
                    callback();
                }
                true
            }
            _ => self.handle_edit_key(code, modifiers),
        }
    }

    /// Handle a key event. Returns true if consumed.
    /// Maps to: CC useSearchInput handleKeyDown (the editing subset).
    /// Navigation keys (Enter/Down/Up/Esc) are NOT handled here — the
    /// caller owns focus transitions.
    pub fn handle_edit_key(&mut self, code: &KeyCode, modifiers: &KeyModifiers) -> bool {
        let ctrl = modifiers.contains(KeyModifiers::CONTROL);
        let alt = modifiers.contains(KeyModifiers::ALT);
        self.reset_key_state(code, modifiers);
        match code {
            KeyCode::Backspace if alt => {
                let killed = self.delete_word_before();
                push_to_kill_ring(killed, KillDirection::Prepend);
                true
            }
            KeyCode::Backspace => {
                self.backspace();
                true
            }
            KeyCode::Char('h') | KeyCode::Char('H') if ctrl => {
                self.backspace();
                true
            }
            KeyCode::Delete => {
                self.delete();
                true
            }
            KeyCode::Left if ctrl || alt => {
                self.move_prev_word();
                true
            }
            KeyCode::Right if ctrl || alt => {
                self.move_next_word();
                true
            }
            KeyCode::Left => {
                self.move_left();
                true
            }
            KeyCode::Right => {
                self.move_right();
                true
            }
            KeyCode::Home => {
                self.move_home();
                true
            }
            KeyCode::End => {
                self.move_end();
                true
            }
            KeyCode::Char('a') | KeyCode::Char('A') if ctrl => {
                self.move_home();
                true
            }
            KeyCode::Char('e') | KeyCode::Char('E') if ctrl => {
                self.move_end();
                true
            }
            KeyCode::Char('b') | KeyCode::Char('B') if ctrl => {
                self.move_left();
                true
            }
            KeyCode::Char('f') | KeyCode::Char('F') if ctrl => {
                self.move_right();
                true
            }
            KeyCode::Char('d') | KeyCode::Char('D') if ctrl => {
                self.delete();
                true
            }
            KeyCode::Char('k') | KeyCode::Char('K') if ctrl => {
                let killed = self.delete_to_line_end();
                push_to_kill_ring(killed, KillDirection::Append);
                true
            }
            KeyCode::Char('u') | KeyCode::Char('U') if ctrl => {
                let killed = self.delete_to_line_start();
                push_to_kill_ring(killed, KillDirection::Prepend);
                true
            }
            KeyCode::Char('w') | KeyCode::Char('W') if ctrl => {
                let killed = self.delete_word_before();
                push_to_kill_ring(killed, KillDirection::Prepend);
                true
            }
            KeyCode::Char('y') | KeyCode::Char('Y') if ctrl => {
                self.yank();
                true
            }
            KeyCode::Char('b') | KeyCode::Char('B') if alt => {
                self.move_prev_word();
                true
            }
            KeyCode::Char('f') | KeyCode::Char('F') if alt => {
                self.move_next_word();
                true
            }
            KeyCode::Char('d') | KeyCode::Char('D') if alt => {
                self.delete_word_after();
                true
            }
            KeyCode::Char('y') | KeyCode::Char('Y') if alt => {
                self.yank_pop();
                true
            }
            KeyCode::Char(c) if !ctrl && !alt => {
                self.insert(*c);
                true
            }
            // CC useSearchInput.ts:215-333 consumes unsupported Ctrl/Meta
            // keys after passthrough keys have been handled by the caller.
            _ if ctrl || alt => true,
            _ => false,
        }
    }
}

fn is_kill_key(code: &KeyCode, modifiers: &KeyModifiers) -> bool {
    if modifiers.contains(KeyModifiers::CONTROL) && matches!(code, KeyCode::Char('k' | 'u' | 'w')) {
        return true;
    }
    modifiers.contains(KeyModifiers::ALT) && matches!(code, KeyCode::Backspace)
}

fn is_yank_key(code: &KeyCode, modifiers: &KeyModifiers) -> bool {
    modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        && matches!(code, KeyCode::Char('y'))
}

/// Convert a character offset to a byte index in `s`.
#[cfg(test)]
fn char_to_byte(s: &str, char_off: usize) -> usize {
    s.char_indices()
        .nth(char_off)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}

// Native character ↔ byte offset projection for the source Cursor delegation.
// Word segmentation remains exclusively in utils/cursor.rs, not this hook.
#[cfg(test)]
fn word_before_start(s: &str, columns: usize, char_off: usize) -> usize {
    let cursor =
        crate::utils::cursor::Cursor::from_text(s, columns, char_to_byte(s, char_off)).prev_word();
    cursor.text()[..cursor.offset()].chars().count()
}

#[cfg(test)]
fn word_after_start(s: &str, columns: usize, char_off: usize) -> usize {
    let cursor =
        crate::utils::cursor::Cursor::from_text(s, columns, char_to_byte(s, char_off)).next_word();
    cursor.text()[..cursor.offset()].chars().count()
}

/// Maps to: CC `useSearchInput({ initialQuery })`.
pub fn use_search_input(hooks: &mut Hooks, initial_query: &str) -> SearchInput {
    let initial_query = initial_query.to_string();
    let initial_cursor = initial_query.encode_utf16().count();
    let (columns, _) = hooks.use_terminal_size();
    SearchInput {
        columns: columns as usize,
        query: hooks.use_state(move || initial_query),
        cursor: hooks.use_state(move || initial_cursor),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctrl_alt_word_boundaries_matches_official_cursor_delegation() {
        // CC useSearchInput.ts:185-197,310-316 delegates to Cursor. Punctuation
        // separates words; the prior whitespace-only implementation crossed both.
        assert_eq!(word_before_start("alpha-beta", 80, 10), 6);
        assert_eq!(word_after_start("alpha-beta", 80, 0), 6);
        assert_eq!(word_before_start("alpha beta", 80, 10), 6);
        assert_eq!(word_before_start("alpha beta   ", 80, 13), 6);
        assert_eq!(word_before_start("alpha", 80, 5), 0);
        // CC MeasuredText constructor (Cursor.ts:1131-1135) normalizes NFC;
        // offsets returned by prevWord/nextWord address "élan café" (start 5).
        assert_eq!(word_before_start("e\u{301}lan cafe\u{301}", 80, 11), 5);

        assert_eq!(word_after_start("alpha beta", 80, 0), 6);
        assert_eq!(word_after_start("alpha beta", 80, 2), 6);
        assert_eq!(word_after_start("alpha beta", 80, 5), 6);
        assert_eq!(word_after_start("alpha beta", 80, 6), 10);
        assert_eq!(word_after_start("  alpha", 80, 0), 2);
        assert_eq!(word_after_start("e\u{301}lan cafe\u{301}", 80, 0), 5);
    }

    #[test]
    fn kill_and_yank_key_classification_matches_official_use_search_input() {
        assert!(is_kill_key(&KeyCode::Char('k'), &KeyModifiers::CONTROL));
        // Source classifier tests lowercase e.key before the switch lowercases it.
        assert!(!is_kill_key(&KeyCode::Char('U'), &KeyModifiers::CONTROL));
        assert!(is_kill_key(&KeyCode::Char('w'), &KeyModifiers::CONTROL));
        assert!(is_kill_key(&KeyCode::Backspace, &KeyModifiers::ALT));
        assert!(!is_kill_key(&KeyCode::Char('d'), &KeyModifiers::ALT));
        assert!(is_yank_key(&KeyCode::Char('y'), &KeyModifiers::CONTROL));
        assert!(!is_yank_key(&KeyCode::Char('Y'), &KeyModifiers::ALT));
        assert!(!is_yank_key(&KeyCode::Char('y'), &KeyModifiers::empty()));
    }

    #[component]
    fn SearchEditingHarness(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut search = use_search_input(&mut hooks, "alpha\nbeta\ngamma");
        hooks.use_effect(move || search.cursor.set(8), ());
        hooks.use_terminal_events(move |event| {
            if let TerminalEvent::Key(KeyEvent {
                code,
                modifiers,
                kind,
                ..
            }) = event
                && kind != KeyEventKind::Release
            {
                search.handle_edit_key(&code, &modifiers);
            }
        });
        element! { Text(content: format!("{}|{}", search.text().replace('\n', "↵"), search.offset())) }
    }

    #[tokio::test]
    async fn search_kill_yank_matches_official_logical_line_and_alt_d_ring() {
        // CC useSearchInput.ts:253-288,319-324 delegates Ctrl+K/U to Cursor;
        // Alt+D deletes the next word without replacing the previous kill.
        use crate::utils::cursor::kill_ring::clear_kill_ring_for_tests;
        use futures::StreamExt;
        clear_kill_ring_for_tests();
        let (sender, receiver) = async_channel::unbounded();
        let mut app = element! { SearchEditingHarness };
        let mut renders =
            Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(receiver).with_size(80, 8),
            ));
        let steps = [
            ("alpha↵beta↵gamma|8", Some(('k', KeyModifiers::CONTROL))),
            ("alpha↵be↵gamma|8", Some(('y', KeyModifiers::CONTROL))),
            ("alpha↵beta↵gamma|10", Some(('u', KeyModifiers::CONTROL))),
            ("alpha↵↵gamma|6", Some(('d', KeyModifiers::ALT))),
            ("alpha↵gamma|6", Some(('y', KeyModifiers::CONTROL))),
            ("alpha↵betagamma|10", None),
        ];
        let mut step = 0;
        let deadline = futures_timer::Delay::new(std::time::Duration::from_secs(4));
        tokio::pin!(deadline);
        let mut last = String::new();
        loop {
            tokio::select! {
                _ = &mut deadline => break,
                canvas = renders.next() => {
                    let Some(canvas) = canvas else { break; };
                    last = canvas.to_string();
                    if last.contains(steps[step].0) {
                        if let Some((character, modifiers)) = steps[step].1 {
                            let mut key = KeyEvent::new(KeyEventKind::Press, KeyCode::Char(character));
                            key.modifiers = modifiers;
                            sender.send(TerminalEvent::Key(key)).await.unwrap();
                            step += 1;
                        } else { break; }
                    }
                }
            }
        }
        assert_eq!(step, steps.len() - 1, "stopped at {step}: {last}");
        assert!(last.contains(steps.last().unwrap().0), "{last}");
        assert_eq!(get_last_kill(), "beta");
    }

    #[derive(Default, Props)]
    struct SourceOracleProps {
        initial: String,
    }

    #[component]
    fn SourceOracleInput(
        props: &SourceOracleProps,
        mut hooks: Hooks,
    ) -> impl Into<AnyElement<'static>> {
        let mut search = use_search_input(&mut hooks, &props.initial);
        let mut processed = hooks.use_state(|| 0usize);
        hooks.use_propagated_terminal_events(move |event| match event.event() {
            TerminalEvent::Paste(text) => {
                search.reset_key_state(&KeyCode::Char(' '), &KeyModifiers::empty());
                search.insert_text(text);
                processed.set(processed.get() + 1);
                event.stop_propagation();
            }
            TerminalEvent::Key(key)
                if key.kind != KeyEventKind::Release
                    && search.handle_edit_key(&key.code, &key.modifiers) =>
            {
                processed.set(processed.get() + 1);
                event.stop_propagation();
            }
            _ => {}
        });
        element! { View(focusable: true, auto_focus: true) {
            Text(content: format!("{:?}|{}|{:?}|{}", search.text(), search.cursor.get(), get_last_kill(), processed.get()))
        } }
    }

    #[tokio::test]
    async fn real_key_dispatch_matches_bun_search_with_accepted_icu78_differences() {
        use crate::utils::cursor::kill_ring::clear_kill_ring_for_tests;
        use futures::StreamExt;
        let key = |code, modifiers| {
            let mut key = KeyEvent::new(KeyEventKind::Press, code);
            key.modifiers = modifiers;
            TerminalEvent::Key(key)
        };
        let plain = |code| key(code, KeyModifiers::empty());
        // Expected states come from executing useSearchInput.ts and Cursor.ts,
        // including raw NFC input navigation and uppercase classifier behavior.
        let cases = vec![
            (
                "👩‍💻x",
                vec![
                    (plain(KeyCode::Left), "👩‍💻x", 5, ""),
                    (plain(KeyCode::Left), "👩‍💻x", 0, ""),
                    (plain(KeyCode::Right), "👩‍💻x", 5, ""),
                    (plain(KeyCode::Backspace), "x", 0, ""),
                ],
            ),
            (
                "a🇨🇳b",
                vec![
                    (plain(KeyCode::Home), "a🇨🇳b", 0, ""),
                    (plain(KeyCode::Right), "a🇨🇳b", 1, ""),
                    (plain(KeyCode::Delete), "ab", 1, ""),
                ],
            ),
            (
                "e\u{301}lan cafe\u{301}",
                vec![
                    (
                        key(KeyCode::Left, KeyModifiers::ALT),
                        "e\u{301}lan cafe\u{301}",
                        5,
                        "",
                    ),
                    (
                        key(KeyCode::Backspace, KeyModifiers::ALT),
                        "café",
                        0,
                        "élan ",
                    ),
                ],
            ),
            (
                "e x",
                vec![
                    (plain(KeyCode::Home), "e x", 0, ""),
                    (plain(KeyCode::Right), "e x", 1, ""),
                    (TerminalEvent::Paste("\u{301}".into()), "é x", 2, ""),
                    (plain(KeyCode::Left), "é x", 1, ""),
                ],
            ),
            // User-accepted ICU78 (2026-09-13): Ctrl+Right from Home
            // targets 12 instead of Bun's 6; Ctrl+W kills "hello:world "
            // instead of "hello:"; Ctrl+Y restores that exact kill at 12.
            // Keep every key acknowledgement and all other source sequences.
            (
                "hello:world next",
                vec![
                    (plain(KeyCode::Home), "hello:world next", 0, ""),
                    (
                        key(KeyCode::Right, KeyModifiers::CONTROL),
                        "hello:world next",
                        12,
                        "",
                    ),
                    (
                        key(KeyCode::Char('w'), KeyModifiers::CONTROL),
                        "next",
                        0,
                        "hello:world ",
                    ),
                    (
                        key(KeyCode::Char('y'), KeyModifiers::CONTROL),
                        "hello:world next",
                        12,
                        "hello:world ",
                    ),
                ],
            ),
            (
                "中文\n你好",
                vec![
                    (
                        key(KeyCode::Char('u'), KeyModifiers::CONTROL),
                        "中文\n",
                        3,
                        "你好",
                    ),
                    (
                        key(KeyCode::Char('y'), KeyModifiers::CONTROL),
                        "中文\n你好",
                        5,
                        "你好",
                    ),
                    (
                        key(KeyCode::Char('k'), KeyModifiers::CONTROL),
                        "中文\n你好",
                        5,
                        "你好",
                    ),
                ],
            ),
            (
                "e\u{301}lan",
                vec![
                    (
                        key(KeyCode::Char('b'), KeyModifiers::ALT),
                        "e\u{301}lan",
                        0,
                        "",
                    ),
                    (TerminalEvent::Paste("abc👩‍💻".into()), "abc👩‍💻élan", 8, ""),
                    (plain(KeyCode::Backspace), "abcélan", 3, ""),
                    (plain(KeyCode::Delete), "abclan", 3, ""),
                ],
            ),
            (
                "alpha beta",
                vec![
                    (
                        key(KeyCode::Char('w'), KeyModifiers::CONTROL),
                        "alpha ",
                        6,
                        "beta",
                    ),
                    (
                        key(KeyCode::Char('U'), KeyModifiers::CONTROL),
                        "",
                        0,
                        "alpha ",
                    ),
                    (
                        key(KeyCode::Char('y'), KeyModifiers::CONTROL),
                        "alpha ",
                        6,
                        "alpha ",
                    ),
                    (
                        key(KeyCode::Char('y'), KeyModifiers::ALT),
                        "beta",
                        4,
                        "alpha ",
                    ),
                ],
            ),
        ];
        let mut differences = Vec::new();
        for (initial, steps) in cases {
            clear_kill_ring_for_tests();
            let mut app = element! {
                ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                    ContextProvider(value: Context::owned(crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings())) {
                        ContextProvider(value: Context::owned(crate::state::store::AppStore::new(Default::default(), None))) {
                            SourceOracleInput(initial: initial.to_string())
                        }
                    }
                }
            };
            let (sender, receiver) = async_channel::unbounded();
            let mut renders = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(receiver).with_size(120, 8),
            ));
            renders.next().await.expect("initial focused render");
            for (step, (event, query, offset, kill)) in steps.into_iter().enumerate() {
                sender.send(event).await.unwrap();
                let expected = format!("{query:?}|{offset}|{kill:?}|{}", step + 1);
                let deadline = futures_timer::Delay::new(std::time::Duration::from_secs(3));
                tokio::pin!(deadline);
                let mut actual = None;
                let mut last = String::new();
                loop {
                    tokio::select! {
                        _ = &mut deadline => break,
                        canvas = renders.next() => {
                            let Some(canvas) = canvas else { break; };
                            last = canvas.to_string();
                            // Wait for the actual key acknowledgement, not for
                            // the expected state: a parity mismatch must not
                            // prevent the remaining source sequence from running.
                            let processed = format!("|{}", step + 1);
                            if let Some(line) = last.lines().map(str::trim_end).find(|line| line.ends_with(&processed)) {
                                actual = Some(line.to_owned());
                                break;
                            }
                        }
                    }
                }
                let Some(actual) = actual else {
                    differences.push(format!(
                        "initial={initial:?}, step={step}: key not acknowledged, expected={expected}, canvas={last}"
                    ));
                    break;
                };
                if actual != expected {
                    differences.push(format!(
                        "initial={initial:?}, step={step}: actual={actual}, Bun oracle / accepted ICU78 expectation={expected}"
                    ));
                }
            }
        }
        assert!(differences.is_empty(), "{}", differences.join("\n"));
    }
}
