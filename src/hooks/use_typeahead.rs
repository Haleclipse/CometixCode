//! Maps to: CC hooks/useTypeahead.tsx (1800+ lines)
//! CC's useTypeahead is the central suggestion engine handling:
//!   - Slash command suggestions (generateCommandSuggestions)
//!   - File path suggestions (@-mentions)
//!   - Agent suggestions
//!   - Shell completion
//!   - Prompt suggestions (AI-generated)
//! Returns TypeaheadState with suggestions list + selection index.

use crate::commands::Command;
use crate::components::prompt_input::prompt_input_footer_suggestions::SuggestionItem;
use crate::services::mcp::types::{McpServerSnapshot, ServerResource};
use crate::tools::agent_tool::load_agents_dir::AgentDefinition;
use crate::utils::bash::shell_completion::ShellCompletionType;
use crate::utils::suggestions::command_suggestions::{
    CommandSuggestionIndex, find_mid_input_slash_command, generate_command_suggestions_with_index,
    get_best_command_match,
};
use crate::utils::suggestions::directory_completion::{
    CompletionOptions, get_directory_completions, get_path_completions, is_path_like_token,
};
use iocraft::hooks::UseMemo;
use iocraft::prelude::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Maps to: CC `currentShellCompletionAbortController` (module-level `let`).
static CURRENT_SHELL_COMPLETION_ABORT_CONTROLLER: std::sync::LazyLock<
    std::sync::Mutex<Option<crate::tool::AbortController>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(None));

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SuggestionKind {
    #[default]
    None,
    Command,
    File,
    Directory,
    Agent,
    Shell,
    CustomTitle,
    SlackChannel,
}

/// UTF-8 carrier for the inline file completion token returned by the source
/// `useTypeahead` hook.  The source keeps this as an object literal; Rust
/// keeps the byte range and derived query together so every consumer uses the
/// canonical `extract_completion_token` implementation below.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AtMentionToken {
    /// UTF-8 byte range of the whole token, including `@` and optional quotes.
    pub start: usize,
    pub end: usize,
    pub query: String,
    pub quoted: bool,
    /// Whether the source token actually contained `@`.
    pub has_at_prefix: bool,
}

/// Maps to CC `types/textInputTypes.ts#InlineGhostText`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlineGhostText {
    pub text: String,
    pub full_command: String,
    pub insert_position: usize,
}

/// Maps to: CC `useTypeahead.tsx:isPathMetadata`.
pub fn is_path_metadata(metadata: Option<&serde_json::Value>) -> bool {
    metadata
        .and_then(|value| value.get("type"))
        .and_then(serde_json::Value::as_str)
        .is_some_and(|kind| matches!(kind, "directory" | "file"))
}

/// Maps to: CC `useTypeahead.tsx:getPreservedSelection`.
pub fn get_preserved_selection(
    previous: &[SuggestionItem],
    previous_selection: i32,
    next: &[SuggestionItem],
) -> i32 {
    if next.is_empty() {
        return -1;
    }
    if previous_selection < 0 {
        return 0;
    }
    let Some(previous_item) = previous.get(previous_selection as usize) else {
        return 0;
    };
    next.iter()
        .position(|item| item.id == previous_item.id)
        .map(|index| index as i32)
        .unwrap_or(0)
}

/// Maps to: CC `handleAutocompletePrevious` (`useTypeahead.tsx:1715-1723`).
/// The selector is circular, including the first → last transition. The
/// source reads `suggestions.length` from its closure; the Rust port takes it
/// as `count` and returns the new `selectedSuggestion`.
pub fn handle_autocomplete_previous(selected_suggestion: i32, count: i32) -> i32 {
    if count <= 0 {
        return -1;
    }
    if selected_suggestion <= 0 {
        count - 1
    } else {
        (selected_suggestion - 1).min(count - 1)
    }
}

/// Maps to: CC `handleAutocompleteNext` (`useTypeahead.tsx:1726-1734`).
/// The selector is circular, including the last → first transition.
pub fn handle_autocomplete_next(selected_suggestion: i32, count: i32) -> i32 {
    if count <= 0 {
        return -1;
    }
    if selected_suggestion < 0 || selected_suggestion >= count - 1 {
        0
    } else {
        selected_suggestion + 1
    }
}

/// Maps to: CC `useTypeahead.tsx:buildResumeInputFromSuggestion`.
pub fn build_resume_input_from_suggestion(suggestion: &SuggestionItem) -> String {
    suggestion
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("sessionId"))
        .and_then(serde_json::Value::as_str)
        .map(|session_id| format!("/resume {session_id}"))
        .unwrap_or_else(|| format!("/resume {}", suggestion.display_text))
}

/// Maps to CC `useTypeahead.tsx:applyDirectorySuggestion`. Directory
/// completion replaces the complete token while preserving surrounding text.
/// The source helper always emits an `@` prefix, even when reached from the
/// plain-token fallback; the Rust range is UTF-8 based because PromptInput's
/// cursor state is too.
pub fn apply_directory_suggestion(
    input: &str,
    token: &AtMentionToken,
    suggestion: &SuggestionItem,
) -> (String, usize) {
    let is_directory = is_path_metadata(suggestion.metadata.as_ref())
        && suggestion
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("type"))
            .and_then(serde_json::Value::as_str)
            == Some("directory");
    let suffix = if is_directory { "/" } else { " " };
    let replacement = format!("@{}{suffix}", suggestion.id);
    let mut next = String::with_capacity(input.len() + replacement.len());
    next.push_str(&input[..token.start]);
    next.push_str(&replacement);
    next.push_str(&input[token.end..]);
    (next, token.start + replacement.len())
}

/// The UTF-8 equivalent of the source completion token carrier. All ranges
/// are byte offsets so callers can slice Rust strings without splitting a
/// Unicode scalar value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionToken {
    pub token: String,
    pub start_pos: usize,
    pub end_pos: usize,
    pub is_quoted: bool,
}

fn is_completion_char(ch: char) -> bool {
    ch.is_alphanumeric()
        || unicode_normalization::char::is_combining_mark(ch)
        || matches!(
            ch,
            '_' | '-' | '.' | '/' | '\\' | '(' | ')' | '[' | ']' | '~' | ':'
        )
}

/// Maps to: CC `useTypeahead.tsx` `HAS_AT_SYMBOL_RE`
/// `/(^|\s)@([\p{L}\p{N}\p{M}_\-./\\()[\]~:]*|"[^"]*"?)$/u`.
/// Evaluated against the text *before* the cursor, matching
/// `updateSuggestions`'s `hasAtSymbol` gate.
pub fn has_at_symbol(text_before_cursor: &str) -> bool {
    let Some(at_idx) = text_before_cursor.rfind('@') else {
        return false;
    };
    if at_idx > 0
        && !text_before_cursor[..at_idx]
            .chars()
            .next_back()
            .is_some_and(char::is_whitespace)
    {
        return false;
    }
    let after = &text_before_cursor[at_idx + 1..];
    if let Some(quoted) = after.strip_prefix('"') {
        return !quoted.contains('"')
            || quoted
                .strip_suffix('"')
                .is_some_and(|inner| !inner.contains('"'));
    }
    after.chars().all(is_completion_char)
}

/// Maps to CC `updateSuggestions` file branches:
/// `@` + `token.startsWith('@')` auto-starts Path/Unified (`isAtSymbol=true`);
/// `suggestionType === 'file'` continues with Unified (`isAtSymbol=false`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileSuggestionTrigger {
    AutoAt { query: String, path_like: bool },
    ContinueFile { query: String },
}

/// Maps to: CC `useTypeahead.tsx:1040-1107`.
pub fn file_suggestion_trigger(
    token: Option<&AtMentionToken>,
    text_before_cursor: &str,
    showing_file_suggestions: bool,
) -> Option<FileSuggestionTrigger> {
    if has_at_symbol(text_before_cursor) && token.is_some_and(|token| token.has_at_prefix) {
        let token = token?;
        return Some(FileSuggestionTrigger::AutoAt {
            query: token.query.clone(),
            path_like: is_path_like_token(&token.query),
        });
    }
    if showing_file_suggestions {
        return Some(FileSuggestionTrigger::ContinueFile {
            query: token?.query.clone(),
        });
    }
    None
}

fn token_start_before_cursor(text: &str, cursor: usize) -> usize {
    text[..cursor]
        .char_indices()
        .rev()
        .find_map(|(index, ch)| (!is_completion_char(ch)).then_some(index + ch.len_utf8()))
        .unwrap_or(0)
}

fn extend_token_end(text: &str, cursor: usize) -> usize {
    let mut end = cursor;
    for (offset, ch) in text[cursor..].char_indices() {
        if !is_completion_char(ch) {
            break;
        }
        end = cursor + offset + ch.len_utf8();
    }
    end
}

/// Maps to: CC `useTypeahead.tsx:extractCompletionToken`.
pub fn extract_completion_token(
    text: &str,
    cursor_offset: usize,
    include_at_symbol: bool,
) -> Option<CompletionToken> {
    if text.is_empty() {
        return None;
    }
    let cursor = crate::utils::cursor::clamp_cursor(text, cursor_offset);

    if include_at_symbol {
        let before = &text[..cursor];
        if let Some(at_quote) = before.rfind("@\"") {
            // The source's quoted regex is deliberately independent of the
            // whitespace boundary used by the unquoted fast path:
            // /@"([^"]*)"?$/. Preserve that behavior for inputs such as
            // `prefix@"file name` instead of silently switching to the plain
            // token scanner.
            let content_start = at_quote + 2;
            let end = text[content_start..]
                .find('"')
                .map(|offset| content_start + offset + 1)
                .unwrap_or_else(|| extend_token_end(text, cursor).max(text.len()));
            if cursor <= end {
                return Some(CompletionToken {
                    token: text[at_quote..end].to_string(),
                    start_pos: at_quote,
                    end_pos: end,
                    is_quoted: true,
                });
            }
        }

        if let Some(at_idx) = before.rfind('@') {
            let boundary_ok = at_idx == 0
                || text[..at_idx]
                    .chars()
                    .next_back()
                    .is_some_and(char::is_whitespace);
            if boundary_ok {
                let end = extend_token_end(text, cursor);
                if end >= at_idx + 1 && text[at_idx + 1..end].chars().all(is_completion_char) {
                    return Some(CompletionToken {
                        token: text[at_idx..end].to_string(),
                        start_pos: at_idx,
                        end_pos: end,
                        is_quoted: false,
                    });
                }
            }
        }
    }

    let start = token_start_before_cursor(text, cursor);
    if start == cursor {
        return None;
    }
    let end = extend_token_end(text, cursor);
    Some(CompletionToken {
        token: text[start..end].to_string(),
        start_pos: start,
        end_pos: end,
        is_quoted: false,
    })
}

/// Maps to: CC `useTypeahead.tsx:extractSearchToken`.
pub fn extract_search_token(token: &CompletionToken) -> String {
    if token.is_quoted {
        token
            .token
            .strip_prefix("@\"")
            .unwrap_or(token.token.as_str())
            .strip_suffix('"')
            .unwrap_or_else(|| {
                token
                    .token
                    .strip_prefix("@\"")
                    .unwrap_or(token.token.as_str())
            })
            .to_string()
    } else {
        token
            .token
            .strip_prefix('@')
            .unwrap_or(token.token.as_str())
            .to_string()
    }
}

fn at_mention_token_from_completion(token: CompletionToken) -> AtMentionToken {
    AtMentionToken {
        start: token.start_pos,
        end: token.end_pos,
        query: extract_search_token(&token),
        quoted: token.is_quoted,
        has_at_prefix: token.token.starts_with('@'),
    }
}

/// Maps to: CC `useTypeahead.tsx:formatReplacementValue`.
pub fn format_replacement_value(
    display_text: &str,
    mode: &str,
    has_at_prefix: bool,
    needs_quotes: bool,
    is_quoted: bool,
    is_complete: bool,
) -> String {
    let suffix = if is_complete { " " } else { "" };
    if is_quoted || needs_quotes {
        if mode == "bash" {
            format!("\"{display_text}\"{suffix}")
        } else {
            format!("@\"{display_text}\"{suffix}")
        }
    } else if has_at_prefix {
        if mode == "bash" {
            format!("{display_text}{suffix}")
        } else {
            format!("@{display_text}{suffix}")
        }
    } else {
        display_text.to_string()
    }
}

/// Maps to CC `useTypeahead.tsx:acceptSuggestionText`. Prompt suggestions may
/// carry the `!` mode marker; the source switches the input mode and removes
/// that marker before handing the text to the controlled input. Keeping this
/// normalization in the typeahead owner avoids reinterpreting suggestion text
/// independently in each iocraft key path.
pub fn accept_suggestion_text(
    text: &str,
) -> (
    crate::components::prompt_input::input_modes::PromptInputMode,
    String,
) {
    match crate::components::prompt_input::input_modes::get_mode_from_input(text) {
        crate::components::prompt_input::input_modes::HistoryMode::Bash => (
            crate::components::prompt_input::input_modes::PromptInputMode::Bash,
            crate::components::prompt_input::input_modes::get_value_from_input(text),
        ),
        crate::components::prompt_input::input_modes::HistoryMode::Prompt => (
            crate::components::prompt_input::input_modes::PromptInputMode::Prompt,
            text.to_string(),
        ),
    }
}

/// The closure scope CC's `handleTab` / `handleEnter` / `handleKeyDown` read
/// from `useTypeahead`. PromptInput fills it from `TypeaheadState` plus its
/// own controlled input/cursor state. Field names follow the source
/// identifiers (`input`, `cursorOffset`, `mode`, `suggestions`,
/// `selectedSuggestion`, `suggestionType`, `commands`, `effectiveGhostText`).
pub struct TypeaheadHandlerScope<'a> {
    pub input: &'a str,
    pub cursor_offset: usize,
    /// CC `mode === 'bash'`.
    pub is_bash_mode: bool,
    /// The rows currently visible to the user. PromptInput passes an empty
    /// slice while its `dismissedForInputRef` marker hides them.
    pub suggestions: &'a [SuggestionItem],
    pub selected_suggestion: i32,
    pub suggestion_type: SuggestionKind,
    /// The `@` token the hook extracted for the current file/directory rows;
    /// the source re-runs `extractCompletionToken` inside each handler.
    pub file_token: Option<&'a AtMentionToken>,
    pub commands: &'a [Command],
    /// CC `effectiveGhostText`: already gated on the cursor position.
    pub effective_ghost_text: Option<&'a InlineGhostText>,
}

impl TypeaheadHandlerScope<'_> {
    fn mode(&self) -> &'static str {
        if self.is_bash_mode { "bash" } else { "prompt" }
    }

    /// CC `suggestions[index]`; the handlers guard each use with
    /// `if (suggestion)`.
    fn suggestion_at(&self, index: usize) -> Option<&SuggestionItem> {
        self.suggestions.get(index)
    }
}

/// The side effects CC's `handleTab` / `handleEnter` perform through
/// `onInputChange` / `setCursorOffset` / `onSubmit` / `clearSuggestions`.
/// The Rust handlers return them so PromptInput, which owns the controlled
/// input and the deferred submit callbacks, can apply them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SuggestionAcceptance {
    /// `onInputChange(input)` + `setCursorOffset(cursor_offset)`. `dismiss`
    /// mirrors a following `clearSuggestions()`; `submit_action` mirrors
    /// `onSubmit(newInput, true)`.
    Replace {
        input: String,
        cursor_offset: usize,
        dismiss: bool,
        submit_action: bool,
    },
    /// `applyCommandSuggestion(suggestion, true, …)`: PromptInput owns the
    /// deferred submit of the selected command suggestion.
    SubmitSuggestion,
    /// `clearSuggestions()` and let the normal submit path handle the input.
    SubmitAction,
    /// `clearSuggestions()` only — the input is left untouched.
    ClearSuggestions,
}

fn is_directory_suggestion(suggestion: &SuggestionItem) -> bool {
    // CC: `isPathMetadata(suggestion.metadata) && suggestion.metadata.type === 'directory'`
    is_path_metadata(suggestion.metadata.as_ref())
        && suggestion
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("type"))
            .and_then(serde_json::Value::as_str)
            == Some("directory")
}

/// Maps to: CC `handleTab` (`useTypeahead.tsx:1158-1516`).
///
/// The trailing `else if (input.trim() !== '')` producer branch is omitted:
/// `handleTab` only runs through `autocomplete:accept`, whose context requires
/// suggestions or ghost text, so that branch is unreachable in 2.1.88.
pub fn handle_tab(scope: &TypeaheadHandlerScope<'_>) -> Option<SuggestionAcceptance> {
    // If we have inline ghost text, apply it
    if let Some(ghost) = scope.effective_ghost_text {
        // Check for bash mode history completion first
        if scope.is_bash_mode {
            // Replace the input with the full command from history
            return Some(SuggestionAcceptance::Replace {
                cursor_offset: ghost.full_command.len(),
                input: ghost.full_command.clone(),
                dismiss: false,
                submit_action: false,
            });
        }

        // Find the mid-input command to get its position (for prompt mode)
        if let Some(mid_input_command) =
            find_mid_input_slash_command(scope.input, scope.cursor_offset)
        {
            // Replace the partial command with the full command + space
            let before = &scope.input[..mid_input_command.start_pos];
            let after = &scope.input[mid_input_command.start_pos + mid_input_command.token.len()..];
            let new_input = format!("{before}/{} {after}", ghost.full_command);
            let new_cursor_offset = mid_input_command.start_pos + 1 + ghost.full_command.len() + 1;
            return Some(SuggestionAcceptance::Replace {
                input: new_input,
                cursor_offset: new_cursor_offset,
                dismiss: false,
                submit_action: false,
            });
        }
    }

    // If we have active suggestions, select one
    if scope.suggestions.is_empty() {
        return None;
    }
    let index = if scope.selected_suggestion == -1 {
        0
    } else {
        scope.selected_suggestion.max(0) as usize
    };
    let suggestion = scope.suggestion_at(index);

    match scope.suggestion_type {
        SuggestionKind::Command => {
            let suggestion = suggestion?;
            // applyCommandSuggestion(suggestion, false /* don't execute on tab */, …)
            let applied = crate::utils::suggestions::command_suggestions::apply_command_suggestion(
                suggestion,
                false,
                scope.commands,
            )?;
            Some(SuggestionAcceptance::Replace {
                cursor_offset: applied.input.len(),
                input: applied.input,
                dismiss: true,
                submit_action: false,
            })
        }
        SuggestionKind::CustomTitle => {
            // Apply custom title to /resume command with sessionId
            let new_input = build_resume_input_from_suggestion(suggestion?);
            Some(SuggestionAcceptance::Replace {
                cursor_offset: new_input.len(),
                input: new_input,
                dismiss: true,
                submit_action: false,
            })
        }
        SuggestionKind::Directory => {
            let suggestion = suggestion?;
            // Check if this is a command context (e.g., /add-dir) or general path completion
            let is_in_command_context = scope.input.starts_with('/');
            let is_dir = is_directory_suggestion(suggestion);
            if is_in_command_context {
                // Command context: replace just the argument portion
                let command_part = scope
                    .input
                    .find(' ')
                    .map_or("", |space_index| &scope.input[..=space_index]);
                let cmd_suffix = if is_dir { "/" } else { " " };
                let new_input = format!("{command_part}{}{cmd_suffix}", suggestion.id);
                // Directories re-run `updateSuggestions(newInput)` for the new
                // path; files `clearSuggestions()`.
                Some(SuggestionAcceptance::Replace {
                    cursor_offset: new_input.len(),
                    input: new_input,
                    dismiss: !is_dir,
                    submit_action: false,
                })
            } else {
                // General path completion: replace the path token in input
                // with @-prefixed path. No completion token (e.g., cursor
                // after space) → just clear suggestions without modifying
                // input to avoid data loss.
                let Some(completion_token) = scope.file_token else {
                    return Some(SuggestionAcceptance::ClearSuggestions);
                };
                let (new_input, cursor_pos) =
                    apply_directory_suggestion(scope.input, completion_token, suggestion);
                Some(SuggestionAcceptance::Replace {
                    input: new_input,
                    cursor_offset: cursor_pos,
                    dismiss: !is_dir,
                    submit_action: false,
                })
            }
        }
        SuggestionKind::Shell => {
            let suggestion = suggestion?;
            let (input, cursor_offset) = apply_shell_suggestion(
                suggestion,
                scope.input,
                scope.cursor_offset,
                ShellCompletionType::from_metadata(suggestion.metadata.as_ref()),
            );
            Some(SuggestionAcceptance::Replace {
                input,
                cursor_offset,
                dismiss: true,
                submit_action: false,
            })
        }
        SuggestionKind::Agent => {
            let suggestion = suggestion.filter(|suggestion| suggestion.id.starts_with("dm-"))?;
            let (input, cursor_offset) = apply_trigger_suggestion(
                suggestion,
                scope.input,
                scope.cursor_offset,
                TriggerSuggestionKind::DirectMessage,
            )?;
            Some(SuggestionAcceptance::Replace {
                input,
                cursor_offset,
                dismiss: true,
                submit_action: false,
            })
        }
        SuggestionKind::SlackChannel => {
            let (input, cursor_offset) = apply_trigger_suggestion(
                suggestion?,
                scope.input,
                scope.cursor_offset,
                TriggerSuggestionKind::SlackChannel,
            )?;
            Some(SuggestionAcceptance::Replace {
                input,
                cursor_offset,
                dismiss: true,
                submit_action: false,
            })
        }
        SuggestionKind::File => {
            let Some(completion_token) = scope.file_token else {
                return Some(SuggestionAcceptance::ClearSuggestions);
            };
            let partial_path = &scope.input[completion_token.start..completion_token.end];

            // Check if all suggestions share a common prefix longer than the current input
            let common_prefix =
                crate::hooks::file_suggestions::find_longest_common_prefix(scope.suggestions);
            // `effectiveTokenLength` excludes the `@` and quotes; CC compares
            // JS UTF-16 lengths.
            let effective_token_length = completion_token.query.encode_utf16().count();

            if common_prefix.encode_utf16().count() > effective_token_length {
                // Replace the current input with the common prefix
                let replacement_value = format_replacement_value(
                    &common_prefix,
                    scope.mode(),
                    completion_token.has_at_prefix,
                    false, // common prefix doesn't need quotes unless already quoted
                    completion_token.quoted,
                    false, // partial completion
                );
                let (new_input, cursor_offset) =
                    crate::hooks::file_suggestions::apply_file_suggestion(
                        &replacement_value,
                        scope.input,
                        partial_path,
                        completion_token.start,
                    )?;
                // Don't clear suggestions so user can continue typing or
                // select a specific option; `updateSuggestions` refreshes them.
                Some(SuggestionAcceptance::Replace {
                    input: new_input,
                    cursor_offset,
                    dismiss: false,
                    submit_action: false,
                })
            } else {
                // Otherwise, apply the selected suggestion
                let suggestion = suggestion?;
                let needs_quotes = suggestion.display_text.contains(' ');
                let replacement_value = format_replacement_value(
                    &suggestion.display_text,
                    scope.mode(),
                    completion_token.has_at_prefix,
                    needs_quotes,
                    completion_token.quoted,
                    true, // complete suggestion
                );
                let (new_input, cursor_offset) =
                    crate::hooks::file_suggestions::apply_file_suggestion(
                        &replacement_value,
                        scope.input,
                        partial_path,
                        completion_token.start,
                    )?;
                Some(SuggestionAcceptance::Replace {
                    input: new_input,
                    cursor_offset,
                    dismiss: true,
                    submit_action: false,
                })
            }
        }
        SuggestionKind::None => None,
    }
}

/// Maps to: CC `handleEnter` (`useTypeahead.tsx:1518-1693`).
pub fn handle_enter(scope: &TypeaheadHandlerScope<'_>) -> Option<SuggestionAcceptance> {
    if scope.selected_suggestion < 0 || scope.suggestions.is_empty() {
        return None;
    }
    // Every branch is gated on `selectedSuggestion < suggestions.length`.
    let suggestion = scope.suggestion_at(scope.selected_suggestion as usize)?;

    match scope.suggestion_type {
        // applyCommandSuggestion(suggestion, true /* execute on return */, …)
        SuggestionKind::Command => Some(SuggestionAcceptance::SubmitSuggestion),
        SuggestionKind::CustomTitle => {
            // Apply custom title and execute /resume command with sessionId
            let new_input = build_resume_input_from_suggestion(suggestion);
            Some(SuggestionAcceptance::Replace {
                cursor_offset: new_input.len(),
                input: new_input,
                dismiss: true,
                submit_action: true,
            })
        }
        SuggestionKind::Shell => {
            let (input, cursor_offset) = apply_shell_suggestion(
                suggestion,
                scope.input,
                scope.cursor_offset,
                ShellCompletionType::from_metadata(suggestion.metadata.as_ref()),
            );
            Some(SuggestionAcceptance::Replace {
                input,
                cursor_offset,
                dismiss: true,
                submit_action: false,
            })
        }
        SuggestionKind::Agent => {
            if !suggestion.id.starts_with("dm-") {
                return None;
            }
            let (input, cursor_offset) = apply_trigger_suggestion(
                suggestion,
                scope.input,
                scope.cursor_offset,
                TriggerSuggestionKind::DirectMessage,
            )?;
            Some(SuggestionAcceptance::Replace {
                input,
                cursor_offset,
                dismiss: true,
                submit_action: false,
            })
        }
        SuggestionKind::SlackChannel => {
            let (input, cursor_offset) = apply_trigger_suggestion(
                suggestion,
                scope.input,
                scope.cursor_offset,
                TriggerSuggestionKind::SlackChannel,
            )?;
            Some(SuggestionAcceptance::Replace {
                input,
                cursor_offset,
                dismiss: true,
                submit_action: false,
            })
        }
        SuggestionKind::File => {
            // Extract completion token directly when needed; without one the
            // source does nothing.
            let completion_info = scope.file_token?;
            let needs_quotes = suggestion.display_text.contains(' ');
            let replacement_value = format_replacement_value(
                &suggestion.display_text,
                scope.mode(),
                completion_info.has_at_prefix,
                needs_quotes,
                completion_info.quoted,
                true, // complete suggestion
            );
            let partial_path = &scope.input[completion_info.start..completion_info.end];
            let (input, cursor_offset) = crate::hooks::file_suggestions::apply_file_suggestion(
                &replacement_value,
                scope.input,
                partial_path,
                completion_info.start,
            )?;
            Some(SuggestionAcceptance::Replace {
                input,
                cursor_offset,
                dismiss: true,
                submit_action: false,
            })
        }
        SuggestionKind::Directory => {
            // In command context (e.g., /add-dir), Enter submits the command
            // rather than applying the directory suggestion. Just clear
            // suggestions and let the submit handler process the current input.
            if scope.input.starts_with('/') {
                return Some(SuggestionAcceptance::SubmitAction);
            }
            // General path completion: replace the path token. No completion
            // token (e.g., cursor after space) → don't modify input, just clear.
            let Some(completion_token) = scope.file_token else {
                return Some(SuggestionAcceptance::ClearSuggestions);
            };
            let (input, cursor_offset) =
                apply_directory_suggestion(scope.input, completion_token, suggestion);
            Some(SuggestionAcceptance::Replace {
                input,
                cursor_offset,
                dismiss: true,
                submit_action: false,
            })
        }
        SuggestionKind::None => None,
    }
}

/// Maps to: CC `handleAutocompleteAccept` (`useTypeahead.tsx:1696-1698`):
/// `void handleTab()`.
pub fn handle_autocomplete_accept(
    scope: &TypeaheadHandlerScope<'_>,
) -> Option<SuggestionAcceptance> {
    handle_tab(scope)
}

/// The effects CC `handleKeyDown` performs through closures.
#[derive(Clone, Debug)]
pub enum KeyDownEffect {
    /// `markAccepted(); acceptSuggestionText(suggestionText)` for the AppState
    /// prompt suggestion (Right arrow, or Tab while no autocomplete is active).
    AcceptPromptSuggestion,
    /// `addNotification({ key: 'thinking-toggle-hint', … })`.
    AddNotification(crate::context::notifications::Notification),
    /// Ctrl-N / Ctrl-P → `handleAutocompleteNext` / `handleAutocompletePrevious`.
    SetSelectedSuggestion(i32),
    /// Return → `e.preventDefault(); handleEnter()`. The payload is
    /// `handleEnter`'s effect; `None` still consumes the key.
    HandleEnter(Option<SuggestionAcceptance>),
}

/// Maps to: CC `handleKeyDown` (`useTypeahead.tsx:1782-1858`) — the key
/// behaviors not covered by the `autocomplete:*` keybindings.
///
/// `prompt_suggestion_can_accept` is the source's
/// `suggestionText && suggestionShownAt > 0 && input === '' && !isViewingTeammate`.
pub fn handle_key_down(
    code: KeyCode,
    modifiers: KeyModifiers,
    scope: &TypeaheadHandlerScope<'_>,
    prompt_suggestion_can_accept: bool,
    thinking_toggle_shortcut: &str,
    has_pending_chord: bool,
) -> Option<KeyDownEffect> {
    // Handle right arrow to accept prompt suggestion ghost text
    if code == KeyCode::Right && modifiers.is_empty() && prompt_suggestion_can_accept {
        return Some(KeyDownEffect::AcceptPromptSuggestion);
    }

    // Handle Tab key fallback behaviors when no autocomplete suggestions
    // Don't handle tab if shift is pressed (used for mode cycle)
    if code == KeyCode::Tab && !modifiers.contains(KeyModifiers::SHIFT) {
        // Skip if autocomplete is handling this (suggestions or ghost text exist)
        if !scope.suggestions.is_empty() || scope.effective_ghost_text.is_some() {
            return None;
        }
        // Accept prompt suggestion if it exists in AppState
        if prompt_suggestion_can_accept {
            return Some(KeyDownEffect::AcceptPromptSuggestion);
        }
        // Remind user about thinking toggle shortcut if empty input
        if scope.input.trim().is_empty() {
            use crate::context::notifications::{
                Notification, NotificationPriority, NotificationSegment,
            };
            let text = format!("Use {thinking_toggle_shortcut} to toggle thinking");
            return Some(KeyDownEffect::AddNotification(
                Notification::text(
                    "thinking-toggle-hint",
                    text.clone(),
                    NotificationPriority::Immediate,
                )
                .with_timeout_ms(3_000)
                .with_segments(vec![NotificationSegment::text(text).with_dim(true)]),
            ));
        }
        return None;
    }

    // Only continue with navigation if we have suggestions
    if scope.suggestions.is_empty() {
        return None;
    }
    let count = scope.suggestions.len() as i32;

    // Handle Ctrl-N/P for navigation (arrows handled by keybindings)
    // Skip if we're in the middle of a chord sequence to allow chords like ctrl+f n
    if modifiers.contains(KeyModifiers::CONTROL) && !has_pending_chord {
        match code {
            KeyCode::Char('n') => {
                return Some(KeyDownEffect::SetSelectedSuggestion(
                    handle_autocomplete_next(scope.selected_suggestion, count),
                ));
            }
            KeyCode::Char('p') => {
                return Some(KeyDownEffect::SetSelectedSuggestion(
                    handle_autocomplete_previous(scope.selected_suggestion, count),
                ));
            }
            _ => {}
        }
    }

    // Handle selection and execution via return/enter
    // Shift+Enter and Meta+Enter insert newlines (handled by useTextInput),
    // so don't accept the suggestion for those.
    if code == KeyCode::Enter && modifiers.is_empty() {
        return Some(KeyDownEffect::HandleEnter(handle_enter(scope)));
    }
    None
}

/// Maps to: CC `useTypeahead.tsx:applyShellSuggestion`.
///
/// The source replaces only the word immediately before the cursor.  The
/// Rust adapter returns the same pair that PromptInput's state setters need,
/// while keeping the replacement range in UTF-8 byte offsets.
pub fn apply_shell_suggestion(
    suggestion: &SuggestionItem,
    input: &str,
    cursor_offset: usize,
    completion_type: Option<ShellCompletionType>,
) -> (String, usize) {
    let cursor = crate::utils::cursor::clamp_cursor(input, cursor_offset);
    let word_start = input[..cursor].rfind(' ').map_or(0, |index| index + 1);
    let replacement = match completion_type {
        Some(ShellCompletionType::Variable) => format!("${} ", suggestion.display_text),
        Some(ShellCompletionType::Command) => format!("{} ", suggestion.display_text),
        Some(ShellCompletionType::File) | None => suggestion.display_text.clone(),
    };
    let mut next = String::with_capacity(input.len() + replacement.len());
    next.push_str(&input[..word_start]);
    next.push_str(&replacement);
    next.push_str(&input[cursor..]);
    (next, word_start + replacement.len())
}

/// Trigger family consumed by the source `applyTriggerSuggestion` helper.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TriggerSuggestionKind {
    DirectMessage,
    SlackChannel,
}

fn trigger_token_start(input: &str, cursor: usize, kind: TriggerSuggestionKind) -> Option<usize> {
    let before = &input[..cursor];
    let (marker, valid): (&str, fn(char) -> bool) = match kind {
        TriggerSuggestionKind::DirectMessage => ("@", |ch| {
            ch.is_ascii_alphanumeric() || ch == '_' || ch == '-'
        }),
        TriggerSuggestionKind::SlackChannel => ("#", |ch| {
            ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_' || ch == '-'
        }),
    };
    let marker_pos = before.rfind(marker)?;
    if marker_pos > 0
        && !before[..marker_pos]
            .chars()
            .next_back()
            .is_some_and(char::is_whitespace)
    {
        return None;
    }
    let token = &before[marker_pos + marker.len()..];
    if token.is_empty()
        || !token.chars().next().is_some_and(|ch| match kind {
            TriggerSuggestionKind::DirectMessage => {
                ch.is_ascii_alphanumeric() || ch == '_' || ch == '-'
            }
            TriggerSuggestionKind::SlackChannel => ch.is_ascii_lowercase() || ch.is_ascii_digit(),
        })
        || !token.chars().all(valid)
    {
        return None;
    }
    Some(marker_pos)
}

/// Maps to: CC `useTypeahead.tsx:applyTriggerSuggestion`.
pub fn apply_trigger_suggestion(
    suggestion: &SuggestionItem,
    input: &str,
    cursor_offset: usize,
    kind: TriggerSuggestionKind,
) -> Option<(String, usize)> {
    let cursor = crate::utils::cursor::clamp_cursor(input, cursor_offset);
    let start = trigger_token_start(input, cursor, kind)?;
    let replacement = format!("{} ", suggestion.display_text);
    let mut next = String::with_capacity(input.len() + replacement.len());
    next.push_str(&input[..start]);
    next.push_str(&replacement);
    next.push_str(&input[cursor..]);
    Some((next, start + replacement.len()))
}

/// Maps to: CC `useTypeahead.tsx:extractCommandNameAndArgs`.
pub fn extract_command_name_and_args(value: &str) -> Option<(String, String)> {
    if !value.starts_with('/') {
        return None;
    }
    let value_without_slash = &value[1..];
    match value_without_slash.find(' ') {
        Some(space) => Some((
            value_without_slash[..space].to_string(),
            value_without_slash[space + 1..].to_string(),
        )),
        None => Some((value_without_slash.to_string(), String::new())),
    }
}

/// Maps to: CC `useTypeahead.tsx:hasCommandWithArguments`.
pub fn has_command_with_arguments(is_at_end_with_whitespace: bool, value: &str) -> bool {
    !is_at_end_with_whitespace && value.contains(' ') && !value.ends_with(' ')
}

/// Maps to: CC `useTypeahead.tsx:924-981` command-argument-hint branch. The
/// returned text is consumed by `BaseTextInput`; the input itself remains
/// unchanged. The source evaluates `isAtEndWithWhitespace` against the
/// current cursor, rather than the end of the full input string.
pub fn command_argument_hint_for_input(
    value: &str,
    cursor_offset: usize,
    commands: &[Command],
) -> Option<String> {
    let cursor = crate::utils::cursor::clamp_cursor(value, cursor_offset);
    if !value.starts_with('/') || value.len() <= 1 || cursor == 0 {
        return None;
    }
    let is_at_end_with_whitespace = cursor == value.len()
        && value[..cursor]
            .chars()
            .next_back()
            .is_some_and(char::is_whitespace);
    if has_command_with_arguments(is_at_end_with_whitespace, value) {
        return None;
    }
    let (name, args) = extract_command_name_and_args(value)?;
    let command = commands
        .iter()
        .find(|command| crate::commands::get_command_name(command) == name)?;

    if !is_at_end_with_whitespace || !args.trim().is_empty() {
        return None;
    }
    let has_exactly_one_trailing_space = value.ends_with(' ')
        && value
            .strip_suffix(' ')
            .is_some_and(|prefix| !prefix.ends_with(' '));
    if let Some(hint) = command
        .argument_hint
        .as_deref()
        .filter(|hint| !hint.is_empty())
        .filter(|_| has_exactly_one_trailing_space)
    {
        return Some(hint.to_string());
    }
    if command.arg_names.is_empty() {
        return None;
    }
    let arg_names = command
        .arg_names
        .iter()
        .map(|name| name.to_string())
        .collect::<Vec<_>>();
    crate::utils::argument_substitution::generate_progressive_argument_hint(&arg_names, &[])
}

/// Maps to the early return in CC `useTypeahead.tsx`'s command-argument
/// branch. Once a known command has a separator (including one trailing
/// space), the command selector is hidden while its argument producer owns the
/// rows. A non-empty unknown argument likewise must not resurrect stale slash
/// command rows.
fn should_hide_command_rows_after_argument_start(value: &str, commands: &[Command]) -> bool {
    if !value.starts_with('/') || !value.contains(' ') {
        return false;
    }
    let Some((name, args)) = extract_command_name_and_args(value) else {
        return false;
    };
    commands
        .iter()
        .any(|command| crate::commands::get_command_name(command) == name)
        || !args.trim().is_empty()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SuggestionRequestKind {
    Unified,
    Path,
    AddDirectory,
    ResumeTitle,
    Slack,
}

#[derive(Clone, Debug, Default)]
struct FileSuggestionResult {
    query: String,
    input_snapshot: String,
    items: Vec<SuggestionItem>,
    kind: SuggestionKind,
    revision: u64,
    source_key: String,
}

#[derive(Clone, Debug)]
struct FileSuggestionRequest {
    query: String,
    input_snapshot: String,
    kind: SuggestionRequestKind,
    request_id: u64,
    mcp_resources: Arc<std::collections::BTreeMap<String, Vec<ServerResource>>>,
    mcp_clients: Arc<Vec<McpServerSnapshot>>,
    agents: Arc<Vec<AgentDefinition>>,
    /// Source `fetchFileSuggestions`'s `isAtSymbol` flag. It controls whether
    /// an empty query is allowed to return top-level files/resources.
    is_at_symbol: bool,
    source_key: String,
}

/// Maps to: CC `clearSuggestions` (`useTypeahead.tsx:554-563`).
///
/// Drops every async producer row regardless of its type and resets the
/// type to `none`. Any `suggestionType === 'file'` gate the source places
/// around a call stays at that call site. Setting is skipped when the state
/// is already empty so a render-time call cannot re-trigger itself.
fn clear_suggestions(mut file_result: State<FileSuggestionResult>) {
    let current = file_result.read().clone();
    if current.kind == SuggestionKind::None && current.items.is_empty() {
        return;
    }
    file_result.set(FileSuggestionResult {
        revision: current.revision.saturating_add(1),
        ..FileSuggestionResult::default()
    });
}

/// Handle exposed to PromptInput's `autocomplete:dismiss` keybinding so the
/// hook-owned state can be cleared from the key handler.
#[derive(Clone)]
pub struct TypeaheadDismissHandle {
    file_result: State<FileSuggestionResult>,
    latest_file_request: Arc<AtomicU64>,
}

impl TypeaheadDismissHandle {
    /// Maps to: CC `handleAutocompleteDismiss` (`useTypeahead.tsx:1701-1712`)
    /// minus `dismissedForInputRef.current = input`, which PromptInput sets
    /// right after this call (`autocomplete_dismissed_input`).
    pub fn handle_autocomplete_dismiss(&self) {
        // `debouncedFetchFileSuggestions.cancel()` /
        // `debouncedFetchSlackChannels.cancel()`: bumping the generation makes
        // the worker discard any result that lands after the dismissal.
        self.latest_file_request.fetch_add(1, Ordering::AcqRel);
        clear_suggestions(self.file_result);
    }
}

fn resume_title_item(
    log: &crate::utils::session_storage::SessionSummary,
) -> Option<SuggestionItem> {
    let title = log
        .custom_title
        .as_deref()
        .filter(|title| !title.is_empty())?;
    Some(SuggestionItem {
        id: format!("resume-title-{}", log.session_id),
        display_text: title.to_string(),
        tag: None,
        command_text: title.to_string(),
        description: crate::utils::format::format_log_metadata(
            log.modified,
            None,
            Some(log.file_size),
            log.git_branch.as_deref(),
            log.tag.as_deref(),
            log.agent_setting.as_deref(),
            log.pr_number,
            log.pr_repository.as_deref(),
        ),
        metadata: Some(serde_json::json!({"sessionId": log.session_id})),
        color: None,
    })
}

fn command_completion_request(input: &str) -> Option<(SuggestionRequestKind, String)> {
    let (command_name, args) = extract_command_name_and_args(input)?;
    if command_name == "add-dir"
        && !args.is_empty()
        && !args.chars().last().is_some_and(char::is_whitespace)
    {
        return Some((SuggestionRequestKind::AddDirectory, args));
    }
    if command_name == "resume" && input.contains(' ') {
        return Some((SuggestionRequestKind::ResumeTitle, args));
    }
    None
}

fn slack_channel_token(input: &str, cursor_offset: usize) -> Option<String> {
    let cursor = crate::utils::cursor::clamp_cursor(input, cursor_offset);
    let before = &input[..cursor];
    let marker = before.rfind('#')?;
    if marker > 0
        && !before[..marker]
            .chars()
            .next_back()
            .is_some_and(char::is_whitespace)
    {
        return None;
    }
    let token = &before[marker + 1..];
    if token.is_empty()
        || !token
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit())
        || !token
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_' || ch == '-')
    {
        return None;
    }
    Some(token.to_string())
}

/// @cometix: no CC symbol. Rust memo identity for Slack/MCP client
/// snapshots; official `useTypeahead` re-runs from React deps instead.
fn mcp_client_source_key(clients: &[McpServerSnapshot]) -> String {
    clients
        .iter()
        .map(|server| format!("{}:{:?}", server.client.name, server.client.status))
        .collect::<Vec<_>>()
        .join("|")
}

fn direct_agent_suggestions(
    input: &str,
    cursor_offset: usize,
    team_context: Option<&crate::hooks::use_inbox_poller::InboxPollerTeamContext>,
    active_tasks: Option<
        &std::collections::BTreeMap<String, Arc<crate::state::app_state_store::TaskState>>,
    >,
) -> Vec<SuggestionItem> {
    if !crate::utils::agent_swarms_enabled::is_agent_swarms_enabled() {
        return Vec::new();
    }
    let cursor = crate::utils::cursor::clamp_cursor(input, cursor_offset);
    let before = &input[..cursor];
    let marker = before.rfind('@');
    let Some(marker) = marker else {
        return Vec::new();
    };
    if marker > 0
        && !before[..marker]
            .chars()
            .next_back()
            .is_some_and(char::is_whitespace)
    {
        return Vec::new();
    }
    let partial = &before[marker + 1..];
    if !partial
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
    {
        return Vec::new();
    }
    let partial = partial.to_ascii_lowercase();
    let mut suggestions = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut push = |name: &str, description: String| {
        if name == crate::utils::swarm::constants::TEAM_LEAD_NAME
            || !name.to_ascii_lowercase().starts_with(&partial)
            || !seen.insert(name.to_string())
        {
            return;
        }
        suggestions.push(SuggestionItem {
            id: format!("dm-{name}"),
            display_text: format!("@{name}"),
            tag: None,
            command_text: format!("@{name}"),
            description,
            metadata: Some(serde_json::json!({
                "type": "agent",
                "direct": true,
                "agentName": name,
            })),
            color: None,
        });
    };
    if let Some(team_context) = team_context {
        for teammate in team_context.teammates.values() {
            push(&teammate.name, "send message".to_string());
        }
    }
    // CC's second source is `state.agentNameRegistry`. Cometix exposes the
    // same live names through the AppState task projection; preserve its
    // status suffix while avoiding a second registry or suggestion owner.
    if let Some(active_tasks) = active_tasks {
        for task in active_tasks.values() {
            if let Some(teammate) = task.as_in_process_teammate() {
                let description = if teammate.status.is_empty() {
                    "send message".to_string()
                } else {
                    format!("send message · {}", teammate.status)
                };
                push(&teammate.agent_name, description);
            }
        }
    }
    suggestions
}

fn visible_async_suggestions(
    current_input: &str,
    result_input: &str,
    result_items: &[SuggestionItem],
) -> Vec<SuggestionItem> {
    if current_input == result_input {
        result_items.to_vec()
    } else {
        Vec::new()
    }
}

/// Maps to CC `generateBashSuggestions` in `useTypeahead.tsx`. The source
/// aborts the previous request and turns failures into an empty result.
///
/// In CC 2.1.88 this producer is only reached from `handleTab`'s
/// no-suggestion Bash branch, and `handleTab` runs solely through the
/// `autocomplete:accept` keybinding whose context is active only while
/// suggestions or ghost text already exist. The branch is therefore
/// unreachable; Bash mode surfaces `getShellHistoryCompletion` ghost text
/// only. The function is kept for parity with the source file but has no
/// live call site.
pub async fn generate_bash_suggestions(input: &str, cursor_offset: usize) -> Vec<SuggestionItem> {
    if let Ok(mut current) = CURRENT_SHELL_COMPLETION_ABORT_CONTROLLER.lock() {
        if let Some(abort) = current.take() {
            abort.abort();
        }
    }
    let abort = crate::tool::AbortController::default();
    if let Ok(mut current) = CURRENT_SHELL_COMPLETION_ABORT_CONTROLLER.lock() {
        *current = Some(abort.clone());
    }
    let result = crate::utils::bash::shell_completion::get_shell_completions_with_abort(
        input,
        cursor_offset,
        abort.clone(),
    )
    .await;
    if let Ok(mut current) = CURRENT_SHELL_COMPLETION_ABORT_CONTROLLER.lock() {
        if current
            .as_ref()
            .is_some_and(|active| active.same_identity(&abort))
        {
            current.take();
        }
    }
    result
}

/// Keep the previous file rows visible while the next query is being fetched.
///
/// CC's debounced `fetchFileSuggestions` updates the dropdown only when its
/// promise resolves; it does not clear `suggestions` on every keystroke. The
/// previous Rust implementation compared the current token with the result
/// query and returned an empty list during that gap, which produced a visible
/// rows → footer → rows flicker. An empty result is cleared only after it is
/// the result for the current token.
fn visible_file_suggestions(
    current_query: Option<&str>,
    result_query: &str,
    result_items: &[SuggestionItem],
) -> Vec<SuggestionItem> {
    let Some(current_query) = current_query else {
        return Vec::new();
    };
    if current_query == result_query || !result_items.is_empty() {
        result_items.to_vec()
    } else {
        Vec::new()
    }
}

/// Maps to: CC useTypeahead return value (subset)
pub struct TypeaheadState {
    pub suggestions: Arc<Vec<SuggestionItem>>,
    pub selected: State<i32>,
    /// Maps to CC `onChange → setSelectedSuggestion(0)`: set it when the
    /// input text changes and the next suggestion recompute preserves the
    /// selection from index 0 instead of the current one. A flag rather than
    /// a write of `selected`: the recompute runs during render and writes
    /// `selected` only when the preserved index differs, so a keystroke
    /// whose suggestions stay empty (or keep the same item first) does not
    /// force the settle pass to re-run the update.
    pub selection_reset: Ref<bool>,
    pub has_suggestions: bool,
    /// Maps to CC `maxColumnWidth` state (`useTypeahead.tsx:490`), *not* the
    /// `allCommandsMaxWidth` memo behind it.
    ///
    /// The memo is a registry-derived constant (longest command name + 6); the
    /// state gates it. `setMaxColumnWidth(allCommandsMaxWidth)` happens in
    /// exactly one branch (:993-995, command suggestions), while
    /// `clearSuggestions` (:561) and `fetchFileSuggestions` (:587, :600) reset
    /// it to `undefined`. Publishing the memo unconditionally would size file
    /// and directory rows by the width of a slash command, truncating a path
    /// like `../ansi-behavior-followup-0912/` to the command column.
    pub max_column_width: Option<usize>,
    pub kind: SuggestionKind,
    /// Current `@` token, retained for replacement when a file row is accepted.
    pub file_token: Option<AtMentionToken>,
    /// Maps to CC `commandArgumentHint` in the controlled suggestion state.
    pub command_argument_hint: Option<String>,
    /// Synchronous mid-input slash completion, matching the source's
    /// `syncPromptGhostText` memo and its cursor-position gate.
    pub inline_ghost_text: Option<InlineGhostText>,
    /// Maps to CC `handleAutocompleteDismiss`'s hook-side effects.
    pub dismiss: TypeaheadDismissHandle,
}

/// Maps to: CC useTypeahead({commands, input, ...})
/// Call each frame with current input text. Returns reactive suggestion state.
pub fn use_typeahead(
    hooks: &mut Hooks,
    input: &str,
    cursor_offset: usize,
    commands: Arc<Vec<Command>>,
    mcp_resources: Arc<std::collections::BTreeMap<String, Vec<ServerResource>>>,
    agents: Arc<Vec<AgentDefinition>>,
    is_bash_mode: bool,
    suppress_suggestions: bool,
) -> TypeaheadState {
    // CC pre-warms its singleton file index on the first hook mount. Keep the
    // refresh in the file-suggestions owner; this call only preserves the
    // lifecycle edge and never duplicates the index implementation here.
    crate::hooks::file_suggestions::start_background_cache_refresh();
    // CC useTypeahead reads these slices directly from AppState for teammate
    // mentions and Slack channel discovery. The optional adapter keeps the
    // isolated hook tests valid when no AppStateProvider is mounted.
    let team_context =
        crate::state::app_state::use_app_state_maybe_outside_of_provider(hooks, |state| {
            state.team_context.clone()
        });
    let active_tasks =
        crate::state::app_state::use_app_state_maybe_outside_of_provider(hooks, |state| {
            state.tasks.clone()
        });
    let mcp_clients =
        crate::state::app_state::use_app_state_maybe_outside_of_provider(hooks, |state| {
            state.mcp.clients.clone()
        })
        .unwrap_or_default();
    let mut selected = hooks.use_state(|| 0i32);
    // A ref: only the suggestion memo reads it, to preserve the selected item
    // across producer refreshes. As a `State` it was written on every
    // recompute — a render-phase write that forced a second update pass.
    let previous_suggestions = hooks.use_ref(|| Arc::<Vec<SuggestionItem>>::default());
    let selection_reset = hooks.use_ref(|| false);
    let file_requests =
        hooks.use_const(|| Arc::new(async_channel::unbounded::<FileSuggestionRequest>()));
    // Monotonic request generation mirrors the source's latestSearchTokenRef:
    // an older async result must never replace a newer query's rows.
    let latest_file_request = hooks.use_const(|| Arc::new(AtomicU64::new(0)));
    let file_result = hooks.use_state(FileSuggestionResult::default);
    let mut requested_file_query = hooks.use_state(|| Option::<String>::None);
    let file_request_receiver = file_requests.1.clone();
    let mut file_result_for_worker = file_result;
    let latest_file_request_for_worker = Arc::clone(&latest_file_request);
    hooks.use_future(async move {
        while let Ok(request) = file_request_receiver.recv().await {
            let (items, kind) = match request.kind {
                SuggestionRequestKind::Path => {
                    let path_items = get_path_completions(
                        &request.query,
                        CompletionOptions {
                            base_path: None,
                            max_results: Some(10),
                            include_files: None,
                            include_hidden: None,
                        },
                    )
                    .await
                    .unwrap_or_default();
                    if path_items.is_empty() {
                        // `updateSuggestions` first tries the path producer for
                        // path-like @ tokens, then falls through to the unified
                        // file/MCP/agent producer when no directory entry
                        // matches. Keep that fallback in the same request so an
                        // empty path scan cannot strand the old rows.
                        let items = crate::hooks::unified_suggestions::generate_unified_suggestions(
                            &request.query,
                            &request.mcp_resources,
                            &request.agents,
                            request.is_at_symbol,
                        )
                        .await;
                        // CC `fetchFileSuggestions`: an empty result inlines
                        // `clearSuggestions` and sets type `none`.
                        let kind = if items.is_empty() {
                            SuggestionKind::None
                        } else {
                            SuggestionKind::File
                        };
                        (items, kind)
                    } else {
                        (path_items, SuggestionKind::Directory)
                    }
                }
                SuggestionRequestKind::AddDirectory => (
                    get_directory_completions(
                        &request.query,
                        CompletionOptions {
                            base_path: None,
                            max_results: Some(10),
                            include_files: Some(false),
                            include_hidden: None,
                        },
                    )
                    .await
                    .unwrap_or_default(),
                    SuggestionKind::Directory,
                ),
                SuggestionRequestKind::ResumeTitle => {
                    let query = request.query.clone();
                    let rows = tokio::task::spawn_blocking(move || {
                        crate::utils::session_storage::search_sessions_by_custom_title(
                            &query,
                            Some(
                                crate::utils::session_storage::SearchSessionsByCustomTitleOptions {
                                    limit: Some(10),
                                    exact: false,
                                },
                            ),
                        )
                    })
                    .await
                    .unwrap_or_default();
                    (
                        rows.iter().filter_map(resume_title_item).collect(),
                        SuggestionKind::CustomTitle,
                    )
                }
                SuggestionRequestKind::Slack => {
                    // CC's `useDebounceCallback(fetchSlackChannels, 150)` does
                    // not start an MCP request for every key repeat.  The
                    // request generation is checked before doing network work
                    // so stale queued requests are discarded like AbortController.
                    futures_timer::Delay::new(Duration::from_millis(150)).await;
                    if latest_file_request_for_worker.load(Ordering::Acquire)
                        != request.request_id
                    {
                        continue;
                    }
                    let items = crate::utils::suggestions::slack_channel_suggestions::get_slack_channel_suggestions(
                        &request.mcp_clients,
                        &request.query,
                    )
                    .await;
                    (items, SuggestionKind::SlackChannel)
                }
                SuggestionRequestKind::Unified => {
                    // The source debounces the expensive unified file fetch
                    // by 50ms (`useDebounceCallback(fetchFileSuggestions, 50)`).
                    futures_timer::Delay::new(Duration::from_millis(50)).await;
                    if latest_file_request_for_worker.load(Ordering::Acquire)
                        != request.request_id
                    {
                        continue;
                    }
                    let items = crate::hooks::unified_suggestions::generate_unified_suggestions(
                        &request.query,
                        &request.mcp_resources,
                        &request.agents,
                        request.is_at_symbol,
                    )
                    .await;
                    // CC `fetchFileSuggestions`: an empty result inlines
                    // `clearSuggestions` and sets type `none`.
                    let kind = if items.is_empty() {
                        SuggestionKind::None
                    } else {
                        SuggestionKind::File
                    };
                    (items, kind)
                }
            };
            if latest_file_request_for_worker.load(Ordering::Acquire) != request.request_id {
                continue;
            }
            let revision = file_result_for_worker.read().revision.saturating_add(1);
            file_result_for_worker.set(FileSuggestionResult {
                query: request.query,
                input_snapshot: request.input_snapshot,
                items,
                kind,
                revision,
                source_key: request.source_key,
            });
        }
    });

    let file_token = if suppress_suggestions || is_bash_mode || input.starts_with('/') {
        None
    } else {
        extract_completion_token(input, cursor_offset, true).map(at_mention_token_from_completion)
    };
    let cursor = crate::utils::cursor::clamp_cursor(input, cursor_offset);
    let text_before_cursor = &input[..cursor];
    // Maps to CC `updateSuggestions`'s mid-input ghost early return: a live
    // prompt ghost owns the line, so dropdown producers must not run.
    let mid_input_blocks_dropdown = !is_bash_mode
        && !suppress_suggestions
        && find_mid_input_slash_command(input, cursor_offset)
            .and_then(|mid_input| {
                get_best_command_match(&mid_input.partial_command, commands.as_slice())
            })
            .is_some();
    let command_request = if !is_bash_mode && !suppress_suggestions && !mid_input_blocks_dropdown {
        command_completion_request(input)
    } else {
        None
    };
    let direct_agents = if !is_bash_mode
        && !suppress_suggestions
        && !mid_input_blocks_dropdown
        && input.contains('@')
    {
        direct_agent_suggestions(
            input,
            cursor_offset,
            team_context.flatten().as_deref(),
            active_tasks.as_deref(),
        )
    } else {
        Vec::new()
    };
    let slack_request = if !is_bash_mode && !suppress_suggestions && !mid_input_blocks_dropdown {
        slack_channel_token(input, cursor_offset).filter(|_| {
            crate::utils::suggestions::slack_channel_suggestions::has_slack_mcp_server(&mcp_clients)
        })
    } else {
        None
    };
    let source_key = format!(
        "{}\u{1}{}",
        crate::hooks::unified_suggestions::source_key(&mcp_resources, &agents),
        mcp_client_source_key(&mcp_clients)
    );
    let showing_file_suggestions = file_result.read().kind == SuggestionKind::File;
    // Maps to CC `useTypeahead.tsx:1040-1107`: `@` auto-starts Path/Unified;
    // `suggestionType === 'file'` continues with Unified and `isAtSymbol=false`.
    let file_trigger = if mid_input_blocks_dropdown || !direct_agents.is_empty() {
        None
    } else {
        file_suggestion_trigger(
            file_token.as_ref(),
            text_before_cursor,
            showing_file_suggestions,
        )
    };
    let request_descriptor = if !direct_agents.is_empty() {
        None
    } else {
        command_request
            .clone()
            .map(|(kind, query)| (kind, query, false))
            .or_else(|| {
                slack_request
                    .clone()
                    .map(|query| (SuggestionRequestKind::Slack, query, false))
            })
            .or_else(|| {
                file_trigger.map(|trigger| match trigger {
                    FileSuggestionTrigger::AutoAt { query, path_like } => (
                        if path_like {
                            SuggestionRequestKind::Path
                        } else {
                            SuggestionRequestKind::Unified
                        },
                        query,
                        true,
                    ),
                    FileSuggestionTrigger::ContinueFile { query } => {
                        (SuggestionRequestKind::Unified, query, false)
                    }
                })
            })
    };
    // CC `updateSuggestions:684-688` (`suppressSuggestions`) and `:704-713`
    // (mid-input ghost text) both clear the dropdown regardless of type.
    if suppress_suggestions || mid_input_blocks_dropdown {
        clear_suggestions(file_result);
    }
    if let Some((request_kind, query, is_at_symbol)) = request_descriptor {
        let request_key = format!(
            "{:?}\u{0}{}\u{0}{}\u{0}{}\u{0}{is_at_symbol}",
            request_kind, query, input, source_key
        );
        if requested_file_query.read().as_deref() != Some(request_key.as_str()) {
            let current = file_result.read().clone();
            let already_current = current.query == query
                && current.input_snapshot == input
                && current.source_key == source_key
                && match request_kind {
                    SuggestionRequestKind::Path => current.kind == SuggestionKind::Directory,
                    SuggestionRequestKind::Unified => current.kind == SuggestionKind::File,
                    SuggestionRequestKind::AddDirectory => {
                        current.kind == SuggestionKind::Directory
                    }
                    SuggestionRequestKind::ResumeTitle => {
                        current.kind == SuggestionKind::CustomTitle
                    }
                    SuggestionRequestKind::Slack => current.kind == SuggestionKind::SlackChannel,
                };
            requested_file_query.set(Some(request_key));
            if !already_current {
                let request_id = latest_file_request.fetch_add(1, Ordering::AcqRel) + 1;
                let _ = file_requests.0.try_send(FileSuggestionRequest {
                    query,
                    input_snapshot: input.to_string(),
                    kind: request_kind,
                    request_id,
                    mcp_resources: Arc::clone(&mcp_resources),
                    mcp_clients: Arc::new(mcp_clients.clone()),
                    agents: Arc::clone(&agents),
                    is_at_symbol,
                    source_key: source_key.clone(),
                });
            }
        }
    } else if requested_file_query.read().is_some() {
        requested_file_query.set(None);
        // Invalidate an in-flight result when the token disappears (for
        // example after inserting a space or switching to a slash command).
        latest_file_request.fetch_add(1, Ordering::AcqRel);
        // CC `updateSuggestions:1089-1106`: `if (suggestionType === 'file')`
        // with no completion token → `clearSuggestions()`. Directory/Slack/
        // title producers keep their rows here, as in the source.
        if file_token.is_none() && file_result.read().kind == SuggestionKind::File {
            clear_suggestions(file_result);
        }
    }
    let commands_identity = Arc::as_ptr(&commands) as usize;
    // Retain the Arc alongside the index. Besides mirroring Fuse's ownership of
    // its command objects, this prevents allocator pointer reuse from making a
    // later command array look identical after the previous props Arc drops.
    let (indexed_commands, index) = hooks.use_memo(
        {
            let commands = Arc::clone(&commands);
            move || {
                let index = Arc::new(CommandSuggestionIndex::new(commands.as_slice()));
                (commands, index)
            }
        },
        commands_identity,
    );
    let all_commands_max_width = hooks.use_memo(
        {
            let commands = Arc::clone(&commands);
            move || {
                commands
                    .iter()
                    .filter(|command| !crate::commands::is_command_hidden(command))
                    // JS `String.length` counts UTF-16 code units.
                    .map(|command| {
                        crate::commands::get_command_name(command)
                            .encode_utf16()
                            .count()
                            + 6
                    })
                    .max()
            }
        },
        commands_identity,
    );
    let input_for_search = input.to_string();
    let file_result_snapshot = file_result.read().clone();
    let file_query = file_token.as_ref().map(|token| token.query.clone());
    let command_argument_hint = if is_bash_mode {
        None
    } else {
        command_argument_hint_for_input(input, cursor_offset, commands.as_slice())
    };
    let inline_ghost_text = if !is_bash_mode && !suppress_suggestions {
        find_mid_input_slash_command(input, cursor_offset).and_then(|mid_input| {
            let (suffix, full_command) =
                get_best_command_match(&mid_input.partial_command, commands.as_slice())?;
            Some(InlineGhostText {
                text: suffix,
                full_command,
                insert_position: mid_input.start_pos + 1 + mid_input.partial_command.len(),
            })
        })
    } else if is_bash_mode && !suppress_suggestions {
        crate::utils::suggestions::shell_history_completion::get_shell_history_completion(input)
            .map(|history| InlineGhostText {
                text: history.suffix,
                full_command: history.full_command,
                insert_position: cursor_offset,
            })
    } else {
        None
    };
    let suggestions = hooks.use_memo(
        {
            let commands = Arc::clone(&indexed_commands);
            let index = Arc::clone(&index);
            let input = input_for_search.clone();
            let file_result = file_result_snapshot.clone();
            let file_query = file_query.clone();
            let direct_agents = direct_agents.clone();
            let mut previous_suggestions = previous_suggestions;
            let mut selection_reset = selection_reset;
            move || {
                // Maps to: CC useTypeahead.tsx:684-688 — suppression clears
                // suggestions before every consumer, including keybindings.
                if suppress_suggestions {
                    if selected.get() != -1 {
                        selected.set(-1);
                    }
                    return Arc::new(Vec::new());
                }
                let suggestions = if !direct_agents.is_empty() {
                    direct_agents
                } else if !is_bash_mode && input.starts_with('/') {
                    if matches!(
                        file_result.kind,
                        SuggestionKind::Directory | SuggestionKind::CustomTitle
                    ) {
                        visible_async_suggestions(
                            &input,
                            &file_result.input_snapshot,
                            &file_result.items,
                        )
                    } else if should_hide_command_rows_after_argument_start(
                        &input,
                        commands.as_slice(),
                    ) {
                        Vec::new()
                    } else {
                        generate_command_suggestions_with_index(&input, commands.as_slice(), &index)
                    }
                } else if is_bash_mode {
                    // CC `updateSuggestions` Bash branch only produces the
                    // shell-history ghost text; the dropdown stays empty.
                    Vec::new()
                } else if file_result.kind == SuggestionKind::SlackChannel {
                    visible_async_suggestions(
                        &input,
                        &file_result.input_snapshot,
                        &file_result.items,
                    )
                } else {
                    visible_file_suggestions(
                        file_query.as_deref(),
                        &file_result.query,
                        &file_result.items,
                    )
                };
                // CC :984-990 regenerates selection too, preserving the same
                // item by id whenever a producer refreshes its result list.
                let suggestions = Arc::new(suggestions);
                let previous = previous_suggestions.read().clone();
                // Only write when the preserved index actually moves: the
                // memo recomputes during render, and a `State` write there is
                // a render-phase update. Typing usually keeps the selection at
                // the same index, so this stays silent on most keystrokes.
                // CC onChange resets the selection to 0 before
                // `updateSuggestions` preserves it; the flag carries that
                // reset here without a state write of its own.
                let base = if selection_reset.get() {
                    selection_reset.set(false);
                    0
                } else {
                    selected.get()
                };
                let preserved =
                    get_preserved_selection(previous.as_slice(), base, suggestions.as_slice());
                if preserved != selected.get() {
                    selected.set(preserved);
                }
                previous_suggestions.set(Arc::clone(&suggestions));
                suggestions
            }
        },
        (
            commands_identity,
            input_for_search,
            suppress_suggestions,
            is_bash_mode,
            file_result_snapshot.query.clone(),
            file_result_snapshot.input_snapshot.clone(),
            file_result_snapshot.items.len(),
            file_result_snapshot.revision,
            file_query.clone(),
            file_result_snapshot.source_key.clone(),
            direct_agents.len(),
            slack_request.clone(),
        ),
    );
    let has_suggestions = !suggestions.is_empty();
    let kind = if has_suggestions && !direct_agents.is_empty() {
        SuggestionKind::Agent
    } else if has_suggestions
        && matches!(
            file_result_snapshot.kind,
            SuggestionKind::Directory | SuggestionKind::CustomTitle
        )
        && file_result_snapshot.input_snapshot == input
    {
        file_result_snapshot.kind
    } else if has_suggestions && input.starts_with('/') {
        SuggestionKind::Command
    } else if has_suggestions
        && (file_token.is_some() || file_result_snapshot.kind == SuggestionKind::SlackChannel)
    {
        file_result_snapshot.kind
    } else {
        SuggestionKind::None
    };

    // CC gates the memo behind the `maxColumnWidth` state: command rows get the
    // stable registry width so filtering does not shift the layout, everything
    // else publishes `undefined` and lets the footer size itself from the rows
    // it actually has (`PromptInputFooterSuggestions.tsx:209-211`).
    let max_column_width = match kind {
        SuggestionKind::Command => all_commands_max_width,
        _ => None,
    };

    TypeaheadState {
        suggestions,
        selected,
        selection_reset,
        has_suggestions,
        max_column_width,
        kind,
        file_token,
        command_argument_hint,
        inline_ghost_text,
        dismiss: TypeaheadDismissHandle {
            file_result,
            latest_file_request: Arc::clone(&latest_file_request),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{StreamExt, stream};
    use std::time::Duration;

    #[component]
    fn SuppressionLifecycle(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut suppressed = hooks.use_state(|| false);
        let commands = hooks.use_const(|| {
            let mut second = crate::commands::help::command();
            second.name = "resume".into();
            Arc::new(vec![crate::commands::help::command(), second])
        });
        let typeahead = use_typeahead(
            &mut hooks,
            "/",
            1,
            commands,
            Arc::new(std::collections::BTreeMap::new()),
            Arc::new(Vec::new()),
            false,
            suppressed.get(),
        );
        let mut selected = typeahead.selected;
        hooks.use_terminal_events(move |event| {
            if let TerminalEvent::Key(event) = event {
                match event.code {
                    KeyCode::Down => selected.set(1),
                    KeyCode::F(2) => suppressed.set(true),
                    KeyCode::F(3) => suppressed.set(false),
                    _ => {}
                }
            }
        });
        // CC `applyCommandSuggestion(suggestion, false /* tab */)` text for
        // the selected row, so the test can watch selection regeneration.
        let accept = typeahead
            .suggestions
            .get(selected.get().max(0) as usize)
            .map(|item| format!("{} ", item.command_text));
        element!(Text(content: format!(
            "suppressed={} count={} selected={} accept={:?}",
            suppressed.get(), typeahead.suggestions.len(), selected.get(), accept,
        )))
    }

    #[test]
    fn history_suppression_matches_official_unchanged_input_selection_lifecycle() {
        // CC useTypeahead.tsx:554-563,684-688,984-990: clearing and
        // regeneration reset selection even when the input remains "/".
        // Drive the canonical hook directly: PromptInput's existing Ctrl-R
        // picker takes priority over its separate inline history-search hook.
        let canvases = futures::executor::block_on(async {
            let events = stream::iter([KeyCode::Down, KeyCode::F(2), KeyCode::F(3)]).then(
                |code| async move {
                    futures_timer::Delay::new(Duration::from_millis(40)).await;
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code))
                },
            );
            let mut app = element!(SuppressionLifecycle);
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(100, 4),
            ));
            let mut canvases = Vec::new();
            while let Some(canvas) = crate::utils::race(render_loop.next(), async {
                futures_timer::Delay::new(Duration::from_millis(250)).await;
                None
            })
            .await
            {
                canvases.push(canvas.to_string());
                assert!(canvases.len() < 20, "unexpected render loop");
            }
            canvases
        });
        assert!(
            canvases.iter().any(|s| s.contains("count=2 selected=1")),
            "{canvases:?}"
        );
        assert!(
            canvases
                .iter()
                .any(|s| s.contains("suppressed=true count=0 selected=-1 accept=None")),
            "{canvases:?}"
        );
        assert!(
            canvases
                .last()
                .unwrap()
                .contains("suppressed=false count=2 selected=0 accept=Some(\"/help \")"),
            "{canvases:?}"
        );
    }

    #[test]
    fn file_rows_remain_visible_until_new_query_resolves() {
        let previous = vec![SuggestionItem {
            id: "file-.mcp.json".into(),
            display_text: ".mcp.json".into(),
            tag: None,
            command_text: ".mcp.json".into(),
            description: String::new(),
            metadata: None,
            color: None,
        }];
        assert_eq!(
            visible_file_suggestions(Some(".mcp.j"), ".mcp", &previous),
            previous
        );
        assert!(visible_file_suggestions(Some(".mcp.j"), ".mcp.j", &[]).is_empty());
        assert!(visible_file_suggestions(None, ".mcp", &previous).is_empty());
    }

    #[test]
    fn ordinary_words_do_not_start_file_suggestions() {
        let plain = at_mention_token_from_completion(
            extract_completion_token("inspect README", 14, true).unwrap(),
        );
        assert!(!plain.has_at_prefix);
        assert_eq!(
            file_suggestion_trigger(Some(&plain), "inspect README", false),
            None
        );

        let mention = at_mention_token_from_completion(
            extract_completion_token("inspect @README", 15, true).unwrap(),
        );
        assert!(mention.has_at_prefix);
        assert_eq!(
            file_suggestion_trigger(Some(&mention), "inspect @README", false),
            Some(FileSuggestionTrigger::AutoAt {
                query: "README".into(),
                path_like: false,
            })
        );
    }

    #[test]
    fn has_at_symbol_matches_official_boundary_and_quoted_gate() {
        assert!(has_at_symbol("@"));
        assert!(has_at_symbol("@foo"));
        assert!(has_at_symbol("see @foo"));
        assert!(has_at_symbol("@\"my file"));
        assert!(has_at_symbol("see @\"my file\""));
        assert!(!has_at_symbol("foo@bar"));
        assert!(!has_at_symbol("prefix@\"file"));
        assert!(!has_at_symbol("@foo bar"));
    }

    #[test]
    fn file_continuation_matches_official_branches() {
        let plain = at_mention_token_from_completion(
            extract_completion_token("inspect README", 14, true).unwrap(),
        );
        assert_eq!(
            file_suggestion_trigger(Some(&plain), "inspect README", true),
            Some(FileSuggestionTrigger::ContinueFile {
                query: "README".into(),
            })
        );
        assert_eq!(file_suggestion_trigger(None, "inspect README ", true), None);
        // Once the `file` result is dismissed the continuation branch is gone;
        // a plain token must not restart the file producer.
        assert_eq!(
            file_suggestion_trigger(Some(&plain), "inspect README", false),
            None
        );
    }

    #[component]
    fn MaxColumnWidthLifecycle(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut is_command = hooks.use_state(|| true);
        let commands = hooks.use_const(|| Arc::new(vec![crate::commands::help::command()]));
        // One hook instance, two producers: a slash prefix drives the command
        // branch, a bare word drives no producer at all.
        let input = if is_command.get() { "/" } else { "plain text" };
        let typeahead = use_typeahead(
            &mut hooks,
            input,
            input.len(),
            commands,
            Arc::new(std::collections::BTreeMap::new()),
            Arc::new(Vec::new()),
            false,
            false,
        );
        hooks.use_terminal_events(move |event| {
            if let TerminalEvent::Key(event) = event {
                if event.code == KeyCode::F(2) {
                    is_command.set(false);
                }
            }
        });
        element!(Text(content: format!(
            "kind={:?} width={:?}",
            typeahead.kind, typeahead.max_column_width,
        )))
    }

    #[test]
    fn max_column_width_is_published_for_command_rows_only() {
        // CC `setMaxColumnWidth(allCommandsMaxWidth)` fires in exactly one
        // branch (useTypeahead.tsx:993-995); `clearSuggestions` (:561) and
        // `fetchFileSuggestions` (:587,:600) reset it to `undefined`. Leaking
        // the memo to every producer sizes file and directory rows by the
        // command column, so a path is truncated to a slash command's width.
        let canvases = futures::executor::block_on(async {
            let events = stream::iter([KeyCode::F(2)]).then(|code| async move {
                futures_timer::Delay::new(Duration::from_millis(40)).await;
                TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code))
            });
            let mut app = element!(MaxColumnWidthLifecycle);
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(100, 4),
            ));
            let mut canvases = Vec::new();
            while let Some(canvas) = crate::utils::race(render_loop.next(), async {
                futures_timer::Delay::new(Duration::from_millis(250)).await;
                None
            })
            .await
            {
                canvases.push(canvas.to_string());
                assert!(canvases.len() < 20, "unexpected render loop");
            }
            canvases
        });
        // "help" is four characters; the memo adds the source's `+ 6`.
        assert!(
            canvases
                .first()
                .is_some_and(|canvas| canvas.contains("kind=Command width=Some(10)")),
            "{canvases:?}"
        );
        assert!(
            canvases
                .last()
                .is_some_and(|canvas| canvas.contains("width=None")),
            "{canvases:?}"
        );
    }

    #[component]
    fn ClearSuggestionsLifecycle(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        // A non-`file` producer result: `clearSuggestions` must drop it too.
        let file_result = hooks.use_state(|| FileSuggestionResult {
            query: "src".into(),
            input_snapshot: "/add-dir src".into(),
            items: vec![SuggestionItem {
                id: "/tmp/src".into(),
                display_text: "src/".into(),
                tag: None,
                command_text: String::new(),
                description: "directory".into(),
                metadata: Some(serde_json::json!({"type":"directory"})),
                color: None,
            }],
            kind: SuggestionKind::Directory,
            revision: 7,
            source_key: "src".into(),
        });
        hooks.use_terminal_events(move |event| {
            if let TerminalEvent::Key(event) = event {
                if event.code == KeyCode::F(2) {
                    clear_suggestions(file_result);
                    // Already-empty state must be a no-op (no revision bump).
                    clear_suggestions(file_result);
                }
            }
        });
        let current = file_result.read().clone();
        element!(Text(content: format!(
            "kind={:?} items={} revision={}",
            current.kind,
            current.items.len(),
            current.revision,
        )))
    }

    #[test]
    fn clear_suggestions_drops_every_producer_type_like_official() {
        // CC useTypeahead.tsx:554-563: `clearSuggestions` resets rows and type
        // unconditionally; it is not scoped to `suggestionType === 'file'`.
        let canvases = futures::executor::block_on(async {
            let events = stream::iter([KeyCode::F(2)]).then(|code| async move {
                futures_timer::Delay::new(Duration::from_millis(40)).await;
                TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code))
            });
            let mut app = element!(ClearSuggestionsLifecycle);
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(100, 4),
            ));
            let mut canvases = Vec::new();
            while let Some(canvas) = crate::utils::race(render_loop.next(), async {
                futures_timer::Delay::new(Duration::from_millis(250)).await;
                None
            })
            .await
            {
                canvases.push(canvas.to_string());
                assert!(canvases.len() < 20, "unexpected render loop");
            }
            canvases
        });
        assert!(
            canvases
                .first()
                .is_some_and(|s| s.contains("kind=Directory items=1 revision=7")),
            "{canvases:?}"
        );
        assert!(
            canvases
                .last()
                .is_some_and(|s| s.contains("kind=None items=0 revision=8")),
            "{canvases:?}"
        );
    }

    fn command_item(name: &str) -> SuggestionItem {
        SuggestionItem {
            id: format!("{name}:built-in"),
            display_text: format!("/{name}"),
            tag: None,
            command_text: format!("/{name}"),
            description: String::new(),
            metadata: None,
            color: None,
        }
    }

    fn scope<'a>(
        input: &'a str,
        cursor_offset: usize,
        suggestion_type: SuggestionKind,
        suggestions: &'a [SuggestionItem],
        selected_suggestion: i32,
        file_token: Option<&'a AtMentionToken>,
        commands: &'a [Command],
    ) -> TypeaheadHandlerScope<'a> {
        TypeaheadHandlerScope {
            input,
            cursor_offset,
            is_bash_mode: false,
            suggestions,
            selected_suggestion,
            suggestion_type,
            file_token,
            commands,
            effective_ghost_text: None,
        }
    }

    #[test]
    fn autocomplete_navigation_wraps_like_official_keybinding_handlers() {
        assert_eq!(handle_autocomplete_previous(0, 3), 2);
        assert_eq!(handle_autocomplete_previous(2, 3), 1);
        assert_eq!(handle_autocomplete_next(2, 3), 0);
        assert_eq!(handle_autocomplete_next(0, 3), 1);
        assert_eq!(handle_autocomplete_previous(0, 0), -1);
        assert_eq!(handle_autocomplete_next(-1, 0), -1);
    }

    #[test]
    fn key_down_ctrl_navigation_matches_source_and_respects_chords() {
        let rows = [command_item("a"), command_item("b"), command_item("c")];
        let scope = scope("/", 1, SuggestionKind::Command, &rows, 0, None, &[]);
        assert!(matches!(
            handle_key_down(
                KeyCode::Char('n'),
                KeyModifiers::CONTROL,
                &scope,
                false,
                "alt+t",
                false
            ),
            Some(KeyDownEffect::SetSelectedSuggestion(1))
        ));
        assert!(matches!(
            handle_key_down(
                KeyCode::Char('p'),
                KeyModifiers::CONTROL,
                &scope,
                false,
                "alt+t",
                false
            ),
            Some(KeyDownEffect::SetSelectedSuggestion(2))
        ));
        // A pending chord (e.g. ctrl+f n) owns the key.
        assert!(
            handle_key_down(
                KeyCode::Char('n'),
                KeyModifiers::CONTROL,
                &scope,
                false,
                "alt+t",
                true
            )
            .is_none()
        );
        // Plain `n` is text input.
        assert!(
            handle_key_down(
                KeyCode::Char('n'),
                KeyModifiers::NONE,
                &scope,
                false,
                "alt+t",
                false
            )
            .is_none()
        );
        // Without rows the navigation section is skipped entirely.
        let empty = scope_empty("/", 1);
        assert!(
            handle_key_down(
                KeyCode::Char('n'),
                KeyModifiers::CONTROL,
                &empty,
                false,
                "alt+t",
                false
            )
            .is_none()
        );
    }

    fn scope_empty(input: &str, cursor_offset: usize) -> TypeaheadHandlerScope<'_> {
        TypeaheadHandlerScope {
            input,
            cursor_offset,
            is_bash_mode: false,
            suggestions: &[],
            selected_suggestion: -1,
            suggestion_type: SuggestionKind::None,
            file_token: None,
            commands: &[],
            effective_ghost_text: None,
        }
    }

    #[test]
    fn key_down_tab_fallbacks_match_source_notification_and_prompt_suggestion() {
        // Empty input → thinking toggle hint.
        let empty = scope_empty("", 0);
        let Some(KeyDownEffect::AddNotification(notification)) = handle_key_down(
            KeyCode::Tab,
            KeyModifiers::NONE,
            &empty,
            false,
            "alt+t",
            false,
        ) else {
            panic!("expected the thinking toggle hint");
        };
        assert_eq!(notification.key, "thinking-toggle-hint");
        assert_eq!(notification.text, "Use alt+t to toggle thinking");
        assert_eq!(notification.timeout_ms(), 3_000);
        assert_eq!(notification.segments.len(), 1);
        assert!(notification.segments[0].dim);

        // Prompt suggestion wins over the hint; Right arrow accepts it too.
        assert!(matches!(
            handle_key_down(
                KeyCode::Tab,
                KeyModifiers::NONE,
                &empty,
                true,
                "alt+t",
                false
            ),
            Some(KeyDownEffect::AcceptPromptSuggestion)
        ));
        assert!(matches!(
            handle_key_down(
                KeyCode::Right,
                KeyModifiers::NONE,
                &empty,
                true,
                "alt+t",
                false
            ),
            Some(KeyDownEffect::AcceptPromptSuggestion)
        ));

        // Shift+Tab is the mode cycle; non-empty input without rows is a no-op.
        assert!(
            handle_key_down(
                KeyCode::Tab,
                KeyModifiers::SHIFT,
                &empty,
                false,
                "alt+t",
                false
            )
            .is_none()
        );
        let typed = scope_empty("inspect", 7);
        assert!(
            handle_key_down(
                KeyCode::Tab,
                KeyModifiers::NONE,
                &typed,
                false,
                "alt+t",
                false
            )
            .is_none()
        );

        // With rows or ghost text Tab belongs to `autocomplete:accept`.
        let rows = [command_item("help")];
        let with_rows = scope("/he", 3, SuggestionKind::Command, &rows, 0, None, &[]);
        assert!(
            handle_key_down(
                KeyCode::Tab,
                KeyModifiers::NONE,
                &with_rows,
                false,
                "alt+t",
                false
            )
            .is_none()
        );
    }

    #[test]
    fn completion_token_keeps_suffix_when_cursor_is_inside_an_at_path() {
        let token = extract_completion_token("read @src/ma.rs now", 10, true).unwrap();
        assert_eq!(token.token, "@src/ma.rs");
        assert_eq!(token.start_pos, 5);
        assert_eq!(extract_search_token(&token), "src/ma.rs");

        let quoted = extract_completion_token("read @\"my file.rs\" now", 11, true).unwrap();
        assert!(quoted.is_quoted);
        assert_eq!(extract_search_token(&quoted), "my file.rs");

        // CC's quoted regex has no whitespace-boundary assertion (the
        // unquoted fast path does), so preserve an embedded `@"...` token.
        let embedded = extract_completion_token("prefix@\"my file.rs", 16, true).unwrap();
        assert!(embedded.is_quoted);
        assert_eq!(extract_search_token(&embedded), "my file.rs");

        let carrier = at_mention_token_from_completion(token);
        assert_eq!(carrier.query, "src/ma.rs");
        assert_eq!(
            &"read @src/ma.rs now"[carrier.start..carrier.end],
            "@src/ma.rs"
        );
    }

    #[test]
    fn file_tab_keeps_rows_for_a_common_prefix_then_accepts_the_selected_item() {
        let input = "read @src/m now";
        let token =
            at_mention_token_from_completion(extract_completion_token(input, 10, true).unwrap());
        let items = vec![
            SuggestionItem {
                id: "file-src/main.rs".into(),
                display_text: "src/main.rs".into(),
                tag: None,
                command_text: "src/main.rs".into(),
                description: String::new(),
                metadata: None,
                color: None,
            },
            SuggestionItem {
                id: "file-src/map.rs".into(),
                display_text: "src/map.rs".into(),
                tag: None,
                command_text: "src/map.rs".into(),
                description: String::new(),
                metadata: None,
                color: None,
            },
        ];
        // Common prefix longer than the token: extend it and keep the rows
        // open (CC re-runs `updateSuggestions` instead of `clearSuggestions`).
        let Some(SuggestionAcceptance::Replace {
            input: next,
            cursor_offset: cursor,
            dismiss,
            submit_action: false,
        }) = handle_tab(&scope(
            input,
            10,
            SuggestionKind::File,
            &items,
            0,
            Some(&token),
            &[],
        ))
        else {
            panic!("expected a replacement");
        };
        assert!(!dismiss);
        assert_eq!(next, "read @src/ma now");
        assert_eq!(&next[..cursor], "read @src/ma");

        // No longer prefix: accept the selected row and `clearSuggestions()`.
        let next_input = "read @src/ma now";
        let next_token = at_mention_token_from_completion(
            extract_completion_token(next_input, 11, true).unwrap(),
        );
        let Some(SuggestionAcceptance::Replace {
            input: next,
            cursor_offset: cursor,
            dismiss,
            submit_action: false,
        }) = handle_tab(&scope(
            next_input,
            11,
            SuggestionKind::File,
            &items,
            1,
            Some(&next_token),
            &[],
        ))
        else {
            panic!("expected a replacement");
        };
        assert!(dismiss);
        assert_eq!(next, "read @src/map.rs  now");
        assert_eq!(&next[..cursor], "read @src/map.rs ");

        // Cursor after a space: no completion token → clear without editing.
        assert_eq!(
            handle_tab(&scope(
                "read @src/ma now",
                16,
                SuggestionKind::File,
                &items,
                0,
                None,
                &[],
            )),
            Some(SuggestionAcceptance::ClearSuggestions)
        );
    }

    #[test]
    fn command_argument_hint_matches_static_and_progressive_source_branches() {
        let command = crate::commands::color::command();
        assert_eq!(
            command_argument_hint_for_input("/color ", 7, &[command.clone()]),
            Some("<color|default>".into())
        );
        assert_eq!(
            command_argument_hint_for_input("/color  ", 8, &[command]),
            None,
            "static hints are only shown for the first trailing space"
        );
        assert_eq!(
            command_argument_hint_for_input("/color ", 6, &[crate::commands::color::command()]),
            None,
            "a cursor before the trailing space is not at the end of the command"
        );

        let mut prompt = crate::commands::help::command();
        prompt.name = "review".into();
        prompt.arg_names = vec!["file".into(), "line".into()];
        assert_eq!(
            command_argument_hint_for_input("/review ", 8, &[prompt]),
            Some("[file] [line]".into())
        );
    }

    #[test]
    fn prompt_suggestion_acceptance_switches_and_strips_bash_mode_marker() {
        let (mode, value) = accept_suggestion_text("!printf 'ok'");
        assert_eq!(
            mode,
            crate::components::prompt_input::input_modes::PromptInputMode::Bash
        );
        assert_eq!(value, "printf 'ok'");

        let (mode, value) = accept_suggestion_text("inspect the file");
        assert_eq!(
            mode,
            crate::components::prompt_input::input_modes::PromptInputMode::Prompt
        );
        assert_eq!(value, "inspect the file");
    }

    #[test]
    fn command_suggestion_execute_gate_preserves_prompt_argument_semantics() {
        let with_args = SuggestionItem {
            id: "review:built-in".into(),
            display_text: "/review".into(),
            tag: None,
            command_text: "/review".into(),
            description: String::new(),
            metadata: Some(serde_json::json!({
                "name": "review",
                "argNames": ["file"]
            })),
            color: None,
        };
        let mut review_command = crate::commands::help::command();
        review_command.name = "review".into();
        review_command.kind = crate::commands::CommandKind::Prompt;
        review_command.arg_names = vec!["file".into()];
        assert!(
            crate::utils::suggestions::command_suggestions::command_suggestion_requires_arguments(
                &with_args,
                &[review_command]
            )
        );
        let local = SuggestionItem {
            command_text: "/color".into(),
            ..with_args
        };
        assert!(
            !crate::utils::suggestions::command_suggestions::command_suggestion_requires_arguments(
                &local,
                &[]
            )
        );
    }

    #[test]
    fn suggestion_acceptance_keeps_tab_and_enter_on_one_source_owner() {
        let command = SuggestionItem {
            id: "color:built-in".into(),
            display_text: "/color".into(),
            tag: None,
            command_text: "/color".into(),
            description: String::new(),
            metadata: None,
            color: None,
        };
        let commands = [crate::commands::color::command()];
        let rows = std::slice::from_ref(&command);
        // handleTab: applyCommandSuggestion(execute=false) + clearSuggestions()
        assert_eq!(
            handle_tab(&scope(
                "/co",
                3,
                SuggestionKind::Command,
                rows,
                0,
                None,
                &commands
            )),
            Some(SuggestionAcceptance::Replace {
                input: "/color ".into(),
                cursor_offset: 7,
                dismiss: true,
                submit_action: false,
            })
        );
        // handleTab with `selectedSuggestion === -1` falls back to index 0.
        assert!(
            handle_tab(&scope(
                "/co",
                3,
                SuggestionKind::Command,
                rows,
                -1,
                None,
                &commands
            ))
            .is_some()
        );
        // handleEnter: applyCommandSuggestion(execute=true)
        assert_eq!(
            handle_enter(&scope(
                "/co",
                3,
                SuggestionKind::Command,
                rows,
                0,
                None,
                &commands
            )),
            Some(SuggestionAcceptance::SubmitSuggestion)
        );
        // handleEnter returns early for `selectedSuggestion < 0`.
        assert_eq!(
            handle_enter(&scope(
                "/co",
                3,
                SuggestionKind::Command,
                rows,
                -1,
                None,
                &commands
            )),
            None
        );

        let title = SuggestionItem {
            metadata: Some(serde_json::json!({"sessionId": "abc"})),
            ..command
        };
        let titles = std::slice::from_ref(&title);
        // handleEnter custom-title: onInputChange + onSubmit(newInput, true)
        assert!(matches!(
            handle_enter(&scope("", 0, SuggestionKind::CustomTitle, titles, 0, None, &[])),
            Some(SuggestionAcceptance::Replace {
                input,
                cursor_offset: 11,
                dismiss: true,
                submit_action: true,
            }) if input == "/resume abc"
        ));
        // handleTab custom-title: apply without submitting.
        assert!(matches!(
            handle_tab(&scope(
                "",
                0,
                SuggestionKind::CustomTitle,
                titles,
                0,
                None,
                &[]
            )),
            Some(SuggestionAcceptance::Replace {
                cursor_offset: 11,
                dismiss: true,
                submit_action: false,
                ..
            })
        ));
    }

    #[test]
    fn shell_suggestion_replaces_only_the_word_before_cursor() {
        let item = SuggestionItem {
            id: "echo".into(),
            display_text: "echo".into(),
            tag: None,
            command_text: "echo".into(),
            description: String::new(),
            metadata: None,
            color: None,
        };
        assert_eq!(
            apply_shell_suggestion(
                &item,
                "git ec --flag",
                6,
                Some(ShellCompletionType::Command)
            ),
            ("git echo  --flag".into(), 9)
        );
        assert_eq!(
            apply_shell_suggestion(&item, "$PA", 3, Some(ShellCompletionType::Variable)),
            ("$echo ".into(), 6)
        );
    }

    #[test]
    fn trigger_suggestion_replaces_only_the_trigger_token() {
        let item = SuggestionItem {
            id: "dm-worker".into(),
            display_text: "@worker".into(),
            tag: None,
            command_text: "@worker".into(),
            description: String::new(),
            metadata: None,
            color: None,
        };
        assert_eq!(
            apply_trigger_suggestion(
                &item,
                "ask @wo please",
                7,
                TriggerSuggestionKind::DirectMessage
            ),
            Some(("ask @worker  please".into(), 12))
        );
        assert!(
            apply_trigger_suggestion(&item, "mail@wo", 7, TriggerSuggestionKind::DirectMessage)
                .is_none()
        );
    }

    #[test]
    fn directory_and_resume_acceptance_keep_their_source_context() {
        let directory = SuggestionItem {
            id: "/tmp/project".into(),
            display_text: "project/".into(),
            tag: None,
            command_text: String::new(),
            description: "directory".into(),
            metadata: Some(serde_json::json!({"type":"directory"})),
            color: None,
        };
        let rows = std::slice::from_ref(&directory);
        // handleTab in command context replaces only the argument portion and,
        // for a directory, re-runs `updateSuggestions` (rows stay open).
        assert_eq!(
            handle_tab(&scope(
                "/add-dir /tmp/p",
                15,
                SuggestionKind::Directory,
                rows,
                0,
                None,
                &[],
            )),
            Some(SuggestionAcceptance::Replace {
                input: "/add-dir /tmp/project/".into(),
                cursor_offset: 22,
                dismiss: false,
                submit_action: false,
            })
        );
        // handleEnter in command context clears and lets the submit handler
        // process the unchanged command text.
        assert_eq!(
            handle_enter(&scope(
                "/add-dir /tmp/p",
                15,
                SuggestionKind::Directory,
                rows,
                0,
                None,
                &[],
            )),
            Some(SuggestionAcceptance::SubmitAction)
        );

        let resume = SuggestionItem {
            id: "resume-title-session".into(),
            display_text: "Yesterday".into(),
            tag: None,
            command_text: "Yesterday".into(),
            description: String::new(),
            metadata: Some(serde_json::json!({"sessionId":"session"})),
            color: None,
        };
        assert_eq!(
            build_resume_input_from_suggestion(&resume),
            "/resume session"
        );
    }

    #[test]
    fn inline_ghost_acceptance_matches_prompt_and_bash_branches() {
        // CC handleTab :1170-1189: `before + '/' + fullCommand + ' ' + after`.
        let prompt_ghost = InlineGhostText {
            text: "lp".into(),
            full_command: "help".into(),
            insert_position: 11,
        };
        let mut prompt_scope = scope_empty("explain /hel please", 11);
        prompt_scope.effective_ghost_text = Some(&prompt_ghost);
        assert_eq!(
            handle_tab(&prompt_scope),
            Some(SuggestionAcceptance::Replace {
                input: "explain /help  please".into(),
                cursor_offset: 14,
                dismiss: false,
                submit_action: false,
            })
        );
        // CC handleTab :1162-1168: bash history ghost replaces the whole input.
        let bash_ghost = InlineGhostText {
            text: " status".into(),
            full_command: "git status".into(),
            insert_position: 3,
        };
        let mut bash_scope = scope_empty("git", 3);
        bash_scope.is_bash_mode = true;
        bash_scope.effective_ghost_text = Some(&bash_ghost);
        assert_eq!(
            handle_tab(&bash_scope),
            Some(SuggestionAcceptance::Replace {
                input: "git status".into(),
                cursor_offset: 10,
                dismiss: false,
                submit_action: false,
            })
        );
    }
}
