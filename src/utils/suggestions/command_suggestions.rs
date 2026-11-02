//! Maps to: CC `utils/suggestions/commandSuggestions.ts`.
//!
//! Official Claude Code uses Fuse.js at this site. Nyan keeps the same
//! command fields, weights, hidden-exact behavior, alias display rule, and
//! post-Fuse sorting policy, while using `nucleo-matcher` as the Rust-native
//! fuzzy engine for the actual query match.

use crate::commands::{
    Command, CommandKind, CommandSource, format_description_with_source, get_command_name,
};
use crate::utils::fuse::{BitapSearch, FuseKey, FuseOptions, KeyStore};
use crate::utils::suggestions::skill_usage_tracking::get_skill_usage_score;
use std::cmp::Ordering;
use std::ops::Range;
use std::sync::OnceLock;

const COMMAND_FUSE_THRESHOLD: f64 = 0.3;
const MAX_RECENTLY_USED_COMMANDS: usize = 5;

use crate::components::prompt_input::prompt_input_footer_suggestions::SuggestionItem;

/// Maps to CC `commandSuggestions.ts#MidInputSlashCommand`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MidInputSlashCommand {
    pub token: String,
    pub start_pos: usize,
    pub partial_command: String,
}

/// Maps to CC `findMidInputSlashCommand`.
///
/// The source uses JavaScript string offsets. PromptInput's Rust cursor is a
/// UTF-8 byte offset, so this adapter keeps the same token grammar while
/// returning byte positions that are safe for Rust slicing.
pub fn find_mid_input_slash_command(
    input: &str,
    cursor_offset: usize,
) -> Option<MidInputSlashCommand> {
    if input.starts_with('/') {
        return None;
    }
    let cursor = crate::utils::cursor::clamp_cursor(input, cursor_offset);
    let before_cursor = &input[..cursor];
    let slash_pos = before_cursor.rfind('/')?;
    let preceded_by_whitespace = slash_pos == 0
        || input[..slash_pos]
            .chars()
            .next_back()
            .is_some_and(char::is_whitespace);
    if !preceded_by_whitespace {
        return None;
    }
    let partial = &before_cursor[slash_pos + 1..];
    if !partial
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || "_:-".contains(character))
    {
        return None;
    }
    let full_command = input[slash_pos + 1..]
        .chars()
        .take_while(|character| character.is_ascii_alphanumeric() || "_:-".contains(*character))
        .collect::<String>();
    let command_end = slash_pos + 1 + full_command.len();
    if cursor > command_end {
        return None;
    }
    Some(MidInputSlashCommand {
        token: format!("/{full_command}"),
        start_pos: slash_pos,
        partial_command: full_command,
    })
}

/// Maps to CC `getBestCommandMatch`.
pub fn get_best_command_match(
    partial_command: &str,
    commands: &[Command],
) -> Option<(String, String)> {
    if partial_command.is_empty() {
        return None;
    }
    let query = partial_command.to_lowercase();
    let partial_len = partial_command.chars().count();
    generate_command_suggestions(&format!("/{partial_command}"), commands)
        .into_iter()
        .find_map(|suggestion| {
            let name = suggestion
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.get("commandName"))
                .and_then(serde_json::Value::as_str)
                .or_else(|| {
                    suggestion
                        .metadata
                        .as_ref()
                        .and_then(|metadata| metadata.get("name"))
                        .and_then(serde_json::Value::as_str)
                })?;
            if !name.to_lowercase().starts_with(&query) {
                return None;
            }
            let suffix = name.chars().skip(partial_len).collect::<String>();
            (!suffix.is_empty()).then(|| (suffix, name.to_string()))
        })
}

#[derive(Debug)]
struct CommandSearchEntry {
    command_index: usize,
    command_name: String,
    parts: Vec<String>,
    aliases: Vec<String>,
    description_words: Vec<String>,
}

/// Rust-native equivalent of the Fuse index cached by official
/// `commandSuggestions.ts#getCommandFuse()`.
///
/// This is the single command-search index. `useTypeahead` owns one instance
/// keyed by the stable merged-command array identity; no parallel registry or
/// suggestion-state owner is introduced.
#[derive(Debug)]
pub(crate) struct CommandSuggestionIndex {
    entries: Vec<CommandSearchEntry>,
}

impl CommandSuggestionIndex {
    pub(crate) fn new(commands: &[Command]) -> Self {
        #[cfg(test)]
        COMMAND_SUGGESTION_INDEX_BUILDS.with(|count| count.set(count.get() + 1));

        let entries = commands
            .iter()
            .enumerate()
            .filter(|(_, command)| !crate::commands::is_command_hidden(command))
            .map(|(command_index, command)| {
                let command_name = get_command_name(command).to_lowercase();
                let parts = command_name
                    .split([':', '_', '-'])
                    .filter(|part| !part.is_empty())
                    .map(ToOwned::to_owned)
                    .collect::<Vec<_>>();
                CommandSearchEntry {
                    command_index,
                    parts: if parts.len() > 1 {
                        parts
                    } else {
                        Default::default()
                    },
                    command_name,
                    aliases: command
                        .aliases
                        .iter()
                        .map(|alias| alias.to_lowercase())
                        .collect(),
                    description_words: command
                        .description
                        .split_whitespace()
                        .map(clean_word)
                        .filter(|word| !word.is_empty())
                        .collect(),
                }
            })
            .collect();
        Self { entries }
    }
}

#[derive(Debug, Clone, Copy)]
struct CommandSuggestionMatch<'a> {
    command: &'a Command,
    entry: &'a CommandSearchEntry,
    matched_alias: Option<&'a str>,
    score: f64,
    usage: f64,
}

#[cfg(test)]
thread_local! {
    static COMMAND_SUGGESTION_INDEX_BUILDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static COMMAND_SUGGESTION_SEARCHES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn reset_command_suggestion_probe() {
    COMMAND_SUGGESTION_INDEX_BUILDS.with(|count| count.set(0));
    COMMAND_SUGGESTION_SEARCHES.with(|count| count.set(0));
}

#[cfg(test)]
pub(crate) fn command_suggestion_probe() -> (usize, usize) {
    (
        COMMAND_SUGGESTION_INDEX_BUILDS.with(std::cell::Cell::get),
        COMMAND_SUGGESTION_SEARCHES.with(std::cell::Cell::get),
    )
}

/// Maps to: CC `generateCommandSuggestions(input, commands)`.
pub fn generate_command_suggestions(input: &str, commands: &[Command]) -> Vec<SuggestionItem> {
    if !is_command_input(input) || has_command_args(input) {
        return Vec::new();
    }

    let query = input[1..].trim().to_lowercase();
    if query.is_empty() {
        #[cfg(test)]
        COMMAND_SUGGESTION_SEARCHES.with(|count| count.set(count.get() + 1));
        return empty_query_commands(commands)
            .into_iter()
            .map(|command| create_command_suggestion_item(command, None))
            .collect();
    }

    let index = CommandSuggestionIndex::new(commands);
    generate_command_suggestions_with_index(input, commands, &index)
}

/// Cached-index entry point used by `hooks/use_typeahead.rs`.
pub(crate) fn generate_command_suggestions_with_index(
    input: &str,
    commands: &[Command],
    index: &CommandSuggestionIndex,
) -> Vec<SuggestionItem> {
    if !is_command_input(input) || has_command_args(input) {
        return Vec::new();
    }

    #[cfg(test)]
    COMMAND_SUGGESTION_SEARCHES.with(|count| count.set(count.get() + 1));

    let query = input[1..].trim().to_lowercase();
    if query.is_empty() {
        return empty_query_commands(commands)
            .into_iter()
            .map(|command| create_command_suggestion_item(command, None))
            .collect();
    }

    let hidden_exact = hidden_exact_command(&query, commands);
    let mut suggestions = command_matches_for_query(&query, commands, index)
        .into_iter()
        .map(|matched| create_command_suggestion_item(matched.command, matched.matched_alias))
        .collect::<Vec<_>>();

    if let Some(command) = hidden_exact {
        let hidden_id = command_id(command);
        if !suggestions
            .iter()
            .any(|suggestion| suggestion.id == hidden_id)
        {
            suggestions.insert(
                0,
                create_command_suggestion_item(
                    command,
                    find_matched_alias(&query, &command.aliases),
                ),
            );
        }
    }

    suggestions
}

/// Result of the source `applyCommandSuggestion` helper. PromptInput owns the
/// state setters and submit callback; this owner keeps command formatting and
/// the no-argument execute gate beside the command suggestion producer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedCommandSuggestion {
    pub input: String,
    pub should_submit: bool,
}

/// Maps to CC `commandSuggestions.ts#applyCommandSuggestion`.
///
/// `SuggestionItem.command_text` is the Rust/iocraft executable command name
/// (the source carries the complete `Command` object in metadata). Resolving
/// it through the merged command list preserves aliases, MCP display labels,
/// and the prompt-argument execute gate without duplicating command dispatch
/// in the UI component.
pub fn apply_command_suggestion(
    suggestion: &SuggestionItem,
    should_execute: bool,
    commands: &[Command],
) -> Option<AppliedCommandSuggestion> {
    let trimmed = suggestion.command_text.trim();
    let typed_name = trimmed.strip_prefix('/').unwrap_or(trimmed);
    if typed_name.is_empty() {
        return None;
    }
    let command = crate::commands::find_command(typed_name, commands)?;
    let executable_name = command.name.as_ref();
    let requires_arguments =
        matches!(command.kind, CommandKind::Prompt) && !command.arg_names.is_empty();
    Some(AppliedCommandSuggestion {
        input: format!("/{executable_name} "),
        should_submit: should_execute && !requires_arguments,
    })
}

/// The execute gate shared by PromptInput's deferred Enter path and tests.
/// This is the source command owner; callers should not inspect metadata
/// directly to decide whether a prompt command needs arguments.
pub fn command_suggestion_requires_arguments(
    suggestion: &SuggestionItem,
    commands: &[Command],
) -> bool {
    let Some(typed_name) = suggestion.command_text.trim().strip_prefix('/') else {
        return false;
    };
    crate::commands::find_command(typed_name, commands).is_some_and(|command| {
        matches!(command.kind, CommandKind::Prompt) && !command.arg_names.is_empty()
    })
}

fn is_command_input(input: &str) -> bool {
    input.starts_with('/')
}

fn has_command_args(input: &str) -> bool {
    if !is_command_input(input) || !input.contains(' ') {
        return false;
    }
    !input.ends_with(' ')
}

fn command_matches_for_query<'a>(
    query: &str,
    commands: &'a [Command],
    index: &'a CommandSuggestionIndex,
) -> Vec<CommandSuggestionMatch<'a>> {
    let mut fuse = command_fuse(query);
    let mut matches = index
        .entries
        .iter()
        .filter_map(|entry| {
            let command = &commands[entry.command_index];
            command_nucleo_score(entry, &mut fuse).map(|score| CommandSuggestionMatch {
                command,
                entry,
                matched_alias: find_matched_indexed_alias(command, entry, query),
                score,
                usage: if command.kind == CommandKind::Prompt {
                    get_skill_usage_score(get_command_name(command))
                } else {
                    0.0
                },
            })
        })
        .collect::<Vec<_>>();

    // CC sorts `fuse.search(query)` output, which Fuse has already ordered by
    // score (`shouldSort` defaults to true). That matters because the
    // comparator's `scoreDiff > 0.1` gate (:467-469) falls through to `usage`
    // for near-ties, and when usage ties too the sort is stable — so the Fuse
    // order is what survives. Feeding registry order instead would decide
    // those ties by whichever command happened to be registered first.
    matches.sort_by(|left, right| {
        left.score
            .partial_cmp(&right.score)
            .unwrap_or(Ordering::Equal)
    });
    // The comparator's `|scoreDiff| > 0.1` gate (:467-469) is not a total
    // order — kept verbatim per source, so the sort must go through the
    // non-validating JS-sort primitive instead of `slice::sort_by`, which
    // panics on it (see `utils/js_sort.rs`; mirror PR #7).
    crate::utils::js_sort::sort_by(&mut matches, |left, right| {
        compare_command_matches(left, right, query)
    });
    matches
}

fn empty_query_commands(commands: &[Command]) -> Vec<&Command> {
    // CC puts the five highest-scoring prompt commands first, then groups the
    // remaining visible commands by source. Keep this ordering in the command
    // suggestion owner instead of making the footer infer source metadata.
    let visible = commands
        .iter()
        .filter(|command| !crate::commands::is_command_hidden(command))
        .collect::<Vec<_>>();
    let mut recently_used = visible
        .iter()
        .copied()
        .filter(|command| command.kind == CommandKind::Prompt)
        .filter_map(|command| {
            let score = get_skill_usage_score(get_command_name(command));
            (score > 0.0).then_some((command, score))
        })
        .collect::<Vec<_>>();
    recently_used.sort_by(|left, right| right.1.partial_cmp(&left.1).unwrap_or(Ordering::Equal));
    recently_used.truncate(MAX_RECENTLY_USED_COMMANDS);
    let recent_ids = recently_used
        .iter()
        .map(|(command, _)| command_id(command))
        .collect::<std::collections::HashSet<_>>();

    let mut builtin = Vec::new();
    let mut user = Vec::new();
    let mut project = Vec::new();
    let mut policy = Vec::new();
    let mut other = Vec::new();
    for command in visible {
        if recent_ids.contains(&command_id(command)) {
            continue;
        }
        match command.source {
            CommandSource::UserSettings => user.push(command),
            CommandSource::ProjectSettings => project.push(command),
            CommandSource::PolicySettings => policy.push(command),
            _ if command.kind == CommandKind::Prompt => other.push(command),
            _ => builtin.push(command),
        }
    }
    for list in [
        &mut builtin,
        &mut user,
        &mut project,
        &mut policy,
        &mut other,
    ] {
        list.sort_by_key(|command| get_command_name(command).to_owned());
    }
    recently_used
        .into_iter()
        .map(|(command, _)| command)
        .chain(builtin)
        .chain(user)
        .chain(project)
        .chain(policy)
        .chain(other)
        .collect()
}

fn hidden_exact_command<'a>(query: &str, commands: &'a [Command]) -> Option<&'a Command> {
    let hidden = commands.iter().find(|command| {
        crate::commands::is_command_hidden(command)
            && get_command_name(command).eq_ignore_ascii_case(query)
    })?;
    let visible_same_name = commands.iter().any(|command| {
        !crate::commands::is_command_hidden(command)
            && get_command_name(command).eq_ignore_ascii_case(query)
    });
    (!visible_same_name).then_some(hidden)
}

fn create_command_suggestion_item(
    command: &Command,
    matched_alias: Option<&str>,
) -> SuggestionItem {
    let command_name = get_command_name(command);
    let alias_text = matched_alias
        .map(|alias| format!(" ({alias})"))
        .unwrap_or_default();

    let args = if command.arg_names.is_empty() {
        String::new()
    } else {
        format!(
            " (arguments: {})",
            command
                .arg_names
                .iter()
                .map(std::borrow::Cow::as_ref)
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    SuggestionItem {
        id: command_id(command),
        display_text: format!("/{command_name}{alias_text}"),
        tag: None,
        // Keep the executable registry identifier separate from an optional
        // human-facing label (notably `server:prompt (MCP)`).
        command_text: format!("/{}", command.name),
        // CC carries the full Command object (including its `local-jsx` type)
        // in metadata. JSON is the lossless carrier available to the iocraft
        // row; PromptInput uses the same name/argNames fields when deciding
        // whether Enter may execute a suggestion immediately. Rust keeps the
        // command kind in the typed Command registry instead of duplicating
        // that source discriminator in JSON metadata.
        metadata: Some(serde_json::json!({
            "name": command.name,
            "commandName": command_name,
            "argNames": command
                .arg_names
                .iter()
                .map(std::borrow::Cow::as_ref)
                .collect::<Vec<_>>(),
            "argumentHint": command.argument_hint.as_deref(),
        })),
        color: None,
        description: format!(
            "{}{}",
            format_description_with_source(command).replace('\n', " "),
            args
        ),
    }
}

fn command_id(command: &Command) -> String {
    // Maps to CC `getCommandId`: prompt commands from different setting
    // sources remain distinct; plugin commands include their repository.
    let name = get_command_name(command);
    match command.kind {
        CommandKind::Prompt => {
            if command.source == CommandSource::Plugin
                && let Some(repository) = command
                    .plugin_info
                    .as_ref()
                    .map(|info| info.repository.as_str())
                    .filter(|repository| !repository.is_empty())
            {
                return format!("{name}:plugin:{repository}");
            }
            format!("{name}:{:?}", command.source)
        }
        kind => format!("{name}:{kind:?}"),
    }
}

/// Maps to the source's Fuse construction (`commandSuggestions.ts:53-76`):
/// `threshold: 0.3` ("relatively strict matching"), `location: 0` ("prefer
/// matches at the beginning of strings") and `distance: 100` ("increased to
/// allow matching in descriptions") — the last two being what lets a hit sit
/// some way into a description and still clear the strict threshold.
///
/// The `keys` table is the four the source declares (:57-75): commandName 3,
/// partKey 2, aliasKey 2, descriptionKey 0.5, totalling 7.5. That total is
/// fixed by configuration, so expanding the three array-valued keys into one
/// entry per element cannot dilute `commandName` — a command with a ten-word
/// description weighs its own name exactly as a one-word command does.
///
/// The engine is shared with the other two sites where CC imports Fuse; see
/// `utils/fuse.rs`.
fn command_fuse(query: &str) -> BitapSearch {
    BitapSearch::new(
        query,
        FuseOptions {
            threshold: COMMAND_FUSE_THRESHOLD,
            location: 0,
            distance: 100.0,
            ignore_location: false,
            keys: KeyStore::new(&[3.0, 2.0, 2.0, 0.5]),
        },
    )
}

fn command_nucleo_score(entry: &CommandSearchEntry, fuse: &mut BitapSearch) -> Option<f64> {
    // Maps to official Fuse keys/weights (`commandSuggestions.ts:57-75`):
    // commandName=3, partKey=2, aliasKey=2, descriptionKey=0.5.
    //
    // Three of those four are arrays in the source (:41-49), and Fuse pushes
    // every matching array element into `result.matches` on its own before
    // `_computeScore` multiplies over them — so they expand into individual
    // keys here rather than being joined into one string.
    //
    // All searchable fields are normalized once in CommandSuggestionIndex,
    // not per keystroke.
    let mut keys: Vec<FuseKey<'_>> = Vec::with_capacity(
        1 + entry.parts.len() + entry.aliases.len() + entry.description_words.len(),
    );
    keys.push(FuseKey::new(&entry.command_name, 3.0));
    keys.extend(entry.parts.iter().map(|part| FuseKey::new(part, 2.0)));
    keys.extend(entry.aliases.iter().map(|alias| FuseKey::new(alias, 2.0)));
    keys.extend(
        entry
            .description_words
            .iter()
            .map(|word| FuseKey::new(word, 0.5)),
    );
    fuse.compute_score(&keys)
}

fn clean_word(word: &str) -> String {
    word.to_lowercase()
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect()
}

fn find_matched_alias<'a>(
    query: &str,
    aliases: &'a [std::borrow::Cow<'static, str>],
) -> Option<&'a str> {
    if query.is_empty() {
        return None;
    }
    aliases
        .iter()
        .map(std::borrow::Cow::as_ref)
        .find(|alias| alias.to_lowercase().starts_with(query))
}

fn find_matched_indexed_alias<'a>(
    command: &'a Command,
    entry: &CommandSearchEntry,
    query: &str,
) -> Option<&'a str> {
    entry
        .aliases
        .iter()
        .position(|alias| alias.starts_with(query))
        .and_then(|index| command.aliases.get(index))
        .map(std::borrow::Cow::as_ref)
}

fn compare_command_matches(
    left: &CommandSuggestionMatch<'_>,
    right: &CommandSuggestionMatch<'_>,
    query: &str,
) -> Ordering {
    let left_name = &left.entry.command_name;
    let right_name = &right.entry.command_name;
    let left_aliases = &left.entry.aliases;
    let right_aliases = &right.entry.aliases;

    let left_exact_name = left_name == query;
    let right_exact_name = right_name == query;
    if left_exact_name != right_exact_name {
        return right_exact_name.cmp(&left_exact_name);
    }

    let left_exact_alias = left_aliases.iter().any(|alias| alias == query);
    let right_exact_alias = right_aliases.iter().any(|alias| alias == query);
    if left_exact_alias != right_exact_alias {
        return right_exact_alias.cmp(&left_exact_alias);
    }

    let left_prefix_name = left_name.starts_with(query);
    let right_prefix_name = right_name.starts_with(query);
    if left_prefix_name != right_prefix_name {
        return right_prefix_name.cmp(&left_prefix_name);
    }
    if left_prefix_name && right_prefix_name && left_name.len() != right_name.len() {
        return left_name.len().cmp(&right_name.len());
    }

    let left_prefix_alias = left_aliases.iter().find(|alias| alias.starts_with(query));
    let right_prefix_alias = right_aliases.iter().find(|alias| alias.starts_with(query));
    match (left_prefix_alias, right_prefix_alias) {
        (Some(_), None) => return Ordering::Less,
        (None, Some(_)) => return Ordering::Greater,
        (Some(left_alias), Some(right_alias)) if left_alias.len() != right_alias.len() => {
            return left_alias.len().cmp(&right_alias.len());
        }
        _ => {}
    }

    let score_diff = left.score - right.score;
    if score_diff.abs() > 0.1 {
        return left
            .score
            .partial_cmp(&right.score)
            .unwrap_or(Ordering::Equal);
    }

    right
        .usage
        .partial_cmp(&left.usage)
        .unwrap_or(Ordering::Equal)
}

/// Maps to: CC `commandSuggestions.ts:552-569#findSlashCommandPositions`.
/// L1: UTF-8 ranges for Rust slicing; PromptInput converts to UTF-16 when
/// building TextHighlight, matching the existing input-position convention.
pub fn find_slash_command_positions(text: &str) -> Vec<Range<usize>> {
    static PATTERN: OnceLock<regex::Regex> = OnceLock::new();
    // Exact ECMAScript \s set: includes BOM, excludes Unicode NEL. Preserve
    // the source's ASCII command grammar and consumed whitespace prefix.
    let pattern = PATTERN.get_or_init(|| {
        regex::Regex::new(
            r"(^|[\x09-\x0D\x20\u{00A0}\u{1680}\u{2000}-\u{200A}\u{2028}\u{2029}\u{202F}\u{205F}\u{3000}\u{FEFF}])(/[a-zA-Z][a-zA-Z0-9:_-]*)",
        )
        .expect("valid slash command position pattern")
    });
    pattern
        .captures_iter(text)
        .filter_map(|captures| captures.get(2).map(|word| word.start()..word.end()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slash_highlights_matches_official_scanner_oracle() {
        // Generated by the defining CC findSlashCommandPositions function,
        // stripping TypeScript annotations only (see task research oracle).
        for (text, expected) in [
            (
                "/help /h /demo:task-id_2, x/help (/help) /123 /_x / /help/file",
                vec![0..5, 6..8, 9..24, 52..57],
            ),
            (
                "😀 /help\u{feff}/h\u{85}/no\u{2007}/yes\u{180e}/no\u{2028}/end",
                vec![3..8, 9..11, 16..20, 25..29],
            ),
        ] {
            let positions = find_slash_command_positions(text)
                .into_iter()
                .map(|pos| {
                    text[..pos.start].encode_utf16().count()..text[..pos.end].encode_utf16().count()
                })
                .collect::<Vec<_>>();
            assert_eq!(positions, expected, "text={text:?}");
        }
    }

    fn commands() -> Vec<Command> {
        crate::commands::declared_commands_for_tests()
    }

    /// Regression for the release SIGABRT (mirror PR #7): the verbatim
    /// `|scoreDiff| > 0.1` gate of `compare_command_matches` admits ordering
    /// cycles (a<b<c<a), which `slice::sort_by` rejects with "user-provided
    /// comparison function does not correctly implement a total order". The
    /// suggestion path therefore sorts through `js_sort::sort_by`; this test
    /// pins that routing by driving the exact former-cycle values plus a
    /// pseudo-random sweep through the real comparator.
    #[test]
    fn near_tie_scores_sort_without_panicking_like_a_js_engine() {
        let all = commands();
        let command = all.first().expect("at least one declared command");
        let entry = CommandSearchEntry {
            command_index: 0,
            command_name: "alpha".to_string(),
            parts: Vec::new(),
            aliases: Vec::new(),
            description_words: Vec::new(),
        };
        let matched = |score: f64, usage: f64| CommandSuggestionMatch {
            command,
            entry: &entry,
            matched_alias: None,
            score,
            usage,
        };

        // The documented former cycle: 0.12/10.0 < 0.06/5.0 < 0.00/0.0 < 0.12/10.0.
        let mut cycle = vec![matched(0.12, 10.0), matched(0.06, 5.0), matched(0.0, 0.0)];
        crate::utils::js_sort::sort_by(&mut cycle, |left, right| {
            compare_command_matches(left, right, "zz")
        });
        assert_eq!(cycle.len(), 3);

        // Pseudo-random near-tie sweep (xorshift32), the load that crashed
        // `sort_by` on most seeds. Output must always be a permutation.
        let mut state = 42u32;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state
        };
        for _ in 0..50 {
            let mut rows: Vec<CommandSuggestionMatch<'_>> = (0..64)
                .map(|_| matched((next() % 40) as f64 * 0.01, (next() % 11) as f64))
                .collect();
            crate::utils::js_sort::sort_by(&mut rows, |left, right| {
                compare_command_matches(left, right, "zz")
            });
            assert_eq!(rows.len(), 64);
        }
    }

    #[test]
    fn command_suggestions_use_nucleo_at_official_command_suggestions_site() {
        let suggestions = generate_command_suggestions("/cfg", &commands());

        assert_eq!(
            suggestions
                .first()
                .map(|suggestion| suggestion.command_text.as_str()),
            Some("/config")
        );
    }

    #[test]
    fn cached_command_index_preserves_uncached_suggestion_results() {
        let commands = commands();
        let index = CommandSuggestionIndex::new(&commands);

        for input in ["/", "/cfg", "/allowed", "/resume ", "/not-a-command"] {
            assert_eq!(
                generate_command_suggestions_with_index(input, &commands, &index),
                generate_command_suggestions(input, &commands),
                "input={input}"
            );
        }
    }

    #[test]
    fn command_suggestions_preserve_alias_display_but_accept_canonical_command() {
        let suggestions = generate_command_suggestions("/allowed", &commands());
        let first = suggestions
            .first()
            .expect("allowed-tools alias should produce a suggestion");

        assert_eq!(first.display_text, "/permissions (allowed-tools)");
        assert_eq!(first.command_text, "/permissions");
    }

    #[test]
    fn apply_command_suggestion_formats_input_and_execute_gate_like_source() {
        let commands = commands();
        let suggestion = generate_command_suggestions("/help", &commands)
            .into_iter()
            .find(|item| item.command_text == "/help")
            .expect("help suggestion");
        assert_eq!(
            apply_command_suggestion(&suggestion, false, &commands),
            Some(AppliedCommandSuggestion {
                input: "/help ".into(),
                should_submit: false,
            })
        );

        let mut prompt = crate::commands::help::command();
        prompt.name = "review".into();
        prompt.arg_names = vec!["file".into()];
        let prompt_suggestion = generate_command_suggestions("/review", &[prompt])
            .into_iter()
            .next()
            .expect("prompt suggestion");
        assert_eq!(
            apply_command_suggestion(&prompt_suggestion, true, &[prompt_suggestion_command()]),
            Some(AppliedCommandSuggestion {
                input: "/review ".into(),
                // `help::command()` maps to the source local-jsx command even
                // after the test renames it; Rust represents that kind as
                // CommandKind::LocalUi. LocalUi commands execute immediately;
                // only prompt commands with declared arguments are held for
                // argument entry.
                should_submit: true,
            })
        );
    }

    fn prompt_suggestion_command() -> Command {
        let mut command = crate::commands::help::command();
        command.name = "review".into();
        command.arg_names = vec!["file".into()];
        command
    }

    #[test]
    fn command_suggestions_suppress_after_arguments_like_official() {
        assert!(generate_command_suggestions("/resume abc", &commands()).is_empty());
        assert!(!generate_command_suggestions("/resume ", &commands()).is_empty());
    }

    #[test]
    fn command_suggestions_read_flat_mcp_commands_and_accept_executable_name() {
        let mut available_commands = commands();
        available_commands.push(crate::services::mcp::client::mcp_command_for_test(
            "docs",
            "summarize",
            Some("Summarize docs"),
            &["path", "style"],
        ));

        let suggestions = generate_command_suggestions("/summ", &available_commands);
        let first = suggestions
            .first()
            .expect("flat MCP prompt command should be suggested");
        assert_eq!(first.display_text, "/docs:summarize (MCP)");
        assert_eq!(first.command_text, "/mcp__docs__summarize");
        assert_eq!(first.description, "Summarize docs (arguments: path, style)");
        assert!(
            generate_command_suggestions("/mcp__docs__summarize now", &available_commands)
                .is_empty()
        );
    }

    #[test]
    fn mid_input_slash_command_matches_official_cursor_gate() {
        let found = find_mid_input_slash_command("explain /hel please", 11).unwrap();
        assert_eq!(found.start_pos, 8);
        assert_eq!(found.partial_command, "hel");
        assert_eq!(found.token, "/hel");
        assert!(find_mid_input_slash_command("explain /help now", 15).is_none());
        assert!(find_mid_input_slash_command("prefix/help", 11).is_none());
        assert!(find_mid_input_slash_command("/help", 5).is_none());
    }

    #[test]
    fn best_mid_input_match_returns_only_the_untyped_suffix() {
        let mut command = crate::commands::help::command();
        command.name = "hello".into();
        let (suffix, full) = get_best_command_match("hel", &[command]).unwrap();
        assert_eq!(suffix, "lo");
        assert_eq!(full, "hello");
        assert!(get_best_command_match("hello", &[]).is_none());
    }
}
