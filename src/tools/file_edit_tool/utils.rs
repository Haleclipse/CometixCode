//! Edit application helpers.
//!
//! Maps to: CC `tools/FileEditTool/utils.ts` — quote normalization, apply,
//! and structured-patch generation used by both execution and permission UI.

pub use super::types::{EditInput, FileEdit};
use crate::components::highlighted_code::fallback::convert_leading_tabs_to_spaces;
use crate::types::message::StructuredDiffHunk;
use crate::utils::diff::{
    DisplayEdit, get_patch_for_display, get_patch_from_contents, structured_diff_hunks,
};
use crate::utils::file::add_line_numbers;

/// Curly quote constants for normalization when applying model-generated edits.
/// Maps to: CC utils.ts quote constants.
pub const LEFT_SINGLE_CURLY_QUOTE: char = '‘';
pub const RIGHT_SINGLE_CURLY_QUOTE: char = '’';
pub const LEFT_DOUBLE_CURLY_QUOTE: char = '“';
pub const RIGHT_DOUBLE_CURLY_QUOTE: char = '”';

const SNIPPET_CONTEXT_LINES: usize = 4;
const DIFF_SNIPPET_MAX_BYTES: usize = 8192;

/// Contains replacements to de-sanitize model-generated strings.
/// Maps to: CC `utils.ts#DESANITIZATIONS`.
const DESANITIZATIONS: &[(&str, &str)] = &[
    ("<fnr>", "<function_results>"),
    ("<n>", "<name>"),
    ("</n>", "</name>"),
    ("<o>", "<output>"),
    ("</o>", "</output>"),
    ("<e>", "<error>"),
    ("</e>", "</error>"),
    ("<s>", "<system>"),
    ("</s>", "</system>"),
    ("<r>", "<result>"),
    ("</r>", "</result>"),
    ("< META_START >", "<META_START>"),
    ("< META_END >", "<META_END>"),
    ("< EOT >", "<EOT>"),
    ("< META >", "<META>"),
    ("< SOS >", "<SOS>"),
    ("\n\nH:", "\n\nHuman:"),
    ("\n\nA:", "\n\nAssistant:"),
];

/// Maps to: CC `tools/FileEditTool/utils.ts#normalizeQuotes`.
pub fn normalize_quotes(value: &str) -> String {
    value
        .replace([LEFT_SINGLE_CURLY_QUOTE, RIGHT_SINGLE_CURLY_QUOTE], "'")
        .replace([LEFT_DOUBLE_CURLY_QUOTE, RIGHT_DOUBLE_CURLY_QUOTE], "\"")
}

/// Maps to: CC `tools/FileEditTool/utils.ts#stripTrailingWhitespace`.
pub fn strip_trailing_whitespace(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
                result.push('\r');
                result.push('\n');
            } else {
                result.push('\r');
            }
            continue;
        }
        if ch == '\n' {
            result.push('\n');
            continue;
        }
        // Collect a line's content, then strip trailing whitespace.
        let mut line = String::new();
        line.push(ch);
        while let Some(&next) = chars.peek() {
            if next == '\n' || next == '\r' {
                break;
            }
            line.push(next);
            chars.next();
        }
        result.push_str(line.trim_end());
    }
    result
}

/// Maps to: CC `tools/FileEditTool/utils.ts#findActualString`.
pub fn find_actual_string(file_content: &str, search_string: &str) -> Option<String> {
    if file_content.contains(search_string) {
        return Some(search_string.to_string());
    }
    let normalized_search = normalize_quotes(search_string);
    let normalized_file = normalize_quotes(file_content);
    let search_index = normalized_file.find(&normalized_search)?;
    // CC slices `fileContent` with `substring`, i.e. UTF-16 code units taken
    // from the normalized copy. Quote normalization is unit-preserving, so the
    // offsets carry over unchanged.
    let units = file_content.encode_utf16().collect::<Vec<_>>();
    let start = normalized_file[..search_index]
        .encode_utf16()
        .count()
        .min(units.len());
    let end = start
        .saturating_add(search_string.encode_utf16().count())
        .min(units.len());
    Some(String::from_utf16_lossy(&units[start..end]))
}

/// Maps to: CC `tools/FileEditTool/utils.ts#preserveQuoteStyle`.
pub fn preserve_quote_style(old_string: &str, actual_old_string: &str, new_string: &str) -> String {
    if old_string == actual_old_string {
        return new_string.to_string();
    }
    let has_double_quotes = actual_old_string.contains(LEFT_DOUBLE_CURLY_QUOTE)
        || actual_old_string.contains(RIGHT_DOUBLE_CURLY_QUOTE);
    let has_single_quotes = actual_old_string.contains(LEFT_SINGLE_CURLY_QUOTE)
        || actual_old_string.contains(RIGHT_SINGLE_CURLY_QUOTE);
    if !has_double_quotes && !has_single_quotes {
        return new_string.to_string();
    }

    let mut result = new_string.to_string();
    if has_double_quotes {
        result = apply_curly_double_quotes(&result);
    }
    if has_single_quotes {
        result = apply_curly_single_quotes(&result);
    }
    result
}

fn is_opening_context(chars: &[char], index: usize) -> bool {
    if index == 0 {
        return true;
    }
    matches!(
        chars[index - 1],
        ' ' | '\t' | '\n' | '\r' | '(' | '[' | '{' | '—' | '–'
    )
}

fn apply_curly_double_quotes(value: &str) -> String {
    let chars = value.chars().collect::<Vec<_>>();
    chars
        .iter()
        .enumerate()
        .map(|(index, ch)| {
            if *ch == '"' {
                if is_opening_context(&chars, index) {
                    LEFT_DOUBLE_CURLY_QUOTE
                } else {
                    RIGHT_DOUBLE_CURLY_QUOTE
                }
            } else {
                *ch
            }
        })
        .collect()
}

fn apply_curly_single_quotes(value: &str) -> String {
    let chars = value.chars().collect::<Vec<_>>();
    chars
        .iter()
        .enumerate()
        .map(|(index, ch)| {
            if *ch != '\'' {
                return *ch;
            }
            let prev_is_letter = index
                .checked_sub(1)
                .and_then(|idx| chars.get(idx))
                .is_some_and(|ch| ch.is_alphabetic());
            let next_is_letter = chars.get(index + 1).is_some_and(|ch| ch.is_alphabetic());
            if prev_is_letter && next_is_letter {
                RIGHT_SINGLE_CURLY_QUOTE
            } else if is_opening_context(&chars, index) {
                LEFT_SINGLE_CURLY_QUOTE
            } else {
                RIGHT_SINGLE_CURLY_QUOTE
            }
        })
        .collect()
}

/// Maps to: CC `tools/FileEditTool/utils.ts#applyEditToFile`.
pub fn apply_edit_to_file(
    original_content: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
) -> String {
    if !new_string.is_empty() {
        return if replace_all {
            original_content.replace(old_string, new_string)
        } else {
            original_content.replacen(old_string, new_string, 1)
        };
    }

    // Empty new_string: if old_string isn't newline-terminated but the file
    // has old_string + '\n', also consume the trailing newline so deletions
    // don't leave a blank line behind.
    let strip_trailing_newline =
        !old_string.ends_with('\n') && original_content.contains(&format!("{old_string}\n"));
    let search = if strip_trailing_newline {
        format!("{old_string}\n")
    } else {
        old_string.to_string()
    };
    if replace_all {
        original_content.replace(&search, new_string)
    } else {
        original_content.replacen(&search, new_string, 1)
    }
}

/// Maps to: CC `tools/FileEditTool/utils.ts#getPatchForEdit`.
pub fn get_patch_for_edit(
    file_contents: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
) -> Result<(Vec<StructuredDiffHunk>, String), String> {
    get_patch_for_edits(
        file_contents,
        &[FileEdit {
            old_string: old_string.to_string(),
            new_string: new_string.to_string(),
            replace_all,
        }],
    )
}

/// Maps to: CC `tools/FileEditTool/utils.ts#getPatchForEdits`.
///
/// The returned patch is display-oriented (leading tabs converted to spaces),
/// matching upstream `getPatchFromContents` after `convertLeadingTabsToSpaces`.
pub fn get_patch_for_edits(
    file_contents: &str,
    edits: &[FileEdit],
) -> Result<(Vec<StructuredDiffHunk>, String), String> {
    let mut updated_file = file_contents.to_string();
    let mut applied_new_strings: Vec<String> = Vec::new();

    // Special case for empty files (CC utils.ts:274-294): the source routes
    // this through getPatchForDisplay with the whole-content pseudo-edit.
    if file_contents.is_empty()
        && edits.len() == 1
        && edits[0].old_string.is_empty()
        && edits[0].new_string.is_empty()
    {
        return Ok((
            get_patch_for_display(
                file_contents,
                &[DisplayEdit {
                    old_string: file_contents,
                    new_string: &updated_file,
                    replace_all: false,
                }],
            ),
            String::new(),
        ));
    }

    for edit in edits {
        let old_string_to_check = edit.old_string.trim_end_matches('\n');
        for previous_new in &applied_new_strings {
            if !old_string_to_check.is_empty() && previous_new.contains(old_string_to_check) {
                return Err(
                    "Cannot edit file: old_string is a substring of a new_string from a previous edit."
                        .to_string(),
                );
            }
        }

        let previous_content = updated_file.clone();
        updated_file = if edit.old_string.is_empty() {
            edit.new_string.clone()
        } else {
            apply_edit_to_file(
                &updated_file,
                &edit.old_string,
                &edit.new_string,
                edit.replace_all,
            )
        };

        if updated_file == previous_content {
            return Err("String not found in file. Failed to apply edit.".to_string());
        }
        applied_new_strings.push(edit.new_string.clone());
    }

    if updated_file == file_contents {
        return Err("Original and edited file match exactly. Failed to apply edit.".to_string());
    }

    // We already have before/after content, so call getPatchFromContents
    // directly (CC utils.ts:339-347): getPatchForDisplay would transform
    // fileContents twice and run a no-op full-content replace, ~20% slower
    // on large files.
    let patch = get_patch_from_contents(
        &convert_leading_tabs_to_spaces(file_contents),
        &convert_leading_tabs_to_spaces(&updated_file),
        false,
    );
    Ok((patch, updated_file))
}

/// Maps to: CC `utils.ts#getSnippet`.
pub fn get_snippet(
    original_file: &str,
    old_string: &str,
    new_string: &str,
    context_lines: usize,
) -> (String, usize) {
    let before = original_file.split(old_string).next().unwrap_or("");
    let replacement_line = before
        .replace("\r\n", "\n")
        .split('\n')
        .count()
        .saturating_sub(1);
    let new_file = apply_edit_to_file(original_file, old_string, new_string, false);
    let normalized_new_file = new_file.replace("\r\n", "\n");
    let new_file_lines: Vec<&str> = normalized_new_file.split('\n').collect();
    let start_line = replacement_line.saturating_sub(context_lines);
    let end_line = (replacement_line + context_lines + new_string.split('\n').count())
        .min(new_file_lines.len());
    let snippet = new_file_lines
        .get(start_line..end_line)
        .unwrap_or(&[])
        .join("\n");
    (snippet, start_line + 1)
}

/// Maps to: CC `utils.ts#getSnippetForPatch`.
pub fn get_snippet_for_patch(patch: &[StructuredDiffHunk], new_file: &str) -> (String, usize) {
    if patch.is_empty() {
        return (String::new(), 1);
    }
    let mut min_line = usize::MAX;
    let mut max_line = 0usize;
    for hunk in patch {
        min_line = min_line.min(hunk.old_start);
        let hunk_end = hunk
            .old_start
            .saturating_add(hunk.new_lines)
            .saturating_sub(1);
        max_line = max_line.max(hunk_end);
    }
    let start_line = min_line.saturating_sub(SNIPPET_CONTEXT_LINES).max(1);
    let end_line = max_line + SNIPPET_CONTEXT_LINES;
    let normalized_new_file = new_file.replace("\r\n", "\n");
    let file_lines: Vec<&str> = normalized_new_file.split('\n').collect();
    let end_idx = end_line.min(file_lines.len());
    let start_idx = start_line.saturating_sub(1).min(end_idx);
    let snippet = file_lines[start_idx..end_idx].join("\n");
    (
        add_line_numbers(&snippet, &serde_json::json!(start_line)),
        start_line,
    )
}

/// Maps to: CC `utils.ts#getSnippetForTwoFileDiff`.
pub fn get_snippet_for_two_file_diff(file_a: &str, file_b: &str) -> String {
    let hunks = structured_diff_hunks(file_a, file_b, 8);
    if hunks.is_empty() {
        return String::new();
    }
    let full = hunks
        .iter()
        .map(|hunk| {
            let content = hunk
                .lines
                .iter()
                .filter(|line| !line.starts_with('-') && !line.starts_with('\\'))
                .map(|line| {
                    if line.starts_with('+') || line.starts_with(' ') {
                        &line[1..]
                    } else {
                        line.as_str()
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            add_line_numbers(&content, &serde_json::json!(hunk.old_start))
        })
        .collect::<Vec<_>>()
        .join("\n...\n");

    if full.encode_utf16().count() <= DIFF_SNIPPET_MAX_BYTES {
        return full;
    }
    let max_byte = byte_index_at_utf16_units(&full, DIFF_SNIPPET_MAX_BYTES);
    let cutoff = full[..max_byte].rfind('\n').unwrap_or(max_byte);
    let kept = &full[..cutoff];
    let remaining = full[cutoff..].matches('\n').count() + 1;
    format!("{kept}\n\n... [{remaining} lines truncated] ...")
}

/// Maps to: CC `utils.ts#getEditsForPatch`.
pub fn get_edits_for_patch(patch: &[StructuredDiffHunk]) -> Vec<FileEdit> {
    patch
        .iter()
        .map(|hunk| {
            let mut old_lines = Vec::new();
            let mut new_lines = Vec::new();
            for line in &hunk.lines {
                if let Some(rest) = line.strip_prefix(' ') {
                    old_lines.push(rest);
                    new_lines.push(rest);
                } else if let Some(rest) = line.strip_prefix('-') {
                    old_lines.push(rest);
                } else if let Some(rest) = line.strip_prefix('+') {
                    new_lines.push(rest);
                }
            }
            FileEdit {
                old_string: old_lines.join("\n"),
                new_string: new_lines.join("\n"),
                replace_all: false,
            }
        })
        .collect()
}

fn desanitize_match_string(match_string: &str) -> (String, Vec<(String, String)>) {
    let mut result = match_string.to_string();
    let mut applied = Vec::new();
    for &(from, to) in DESANITIZATIONS {
        if result.contains(from) {
            result = result.replace(from, to);
            applied.push((from.to_string(), to.to_string()));
        }
    }
    (result, applied)
}

fn byte_index_at_utf16_units(value: &str, max_units: usize) -> usize {
    let mut units = 0usize;
    for (index, character) in value.char_indices() {
        let next = units + character.len_utf16();
        if next > max_units {
            return index;
        }
        units = next;
    }
    value.len()
}

/// Maps to: CC `utils.ts#normalizeFileEditInput`.
pub fn normalize_file_edit_input(
    file_path: &str,
    edits: &[EditInput],
    cwd: &std::path::Path,
) -> (String, Vec<EditInput>) {
    if edits.is_empty() {
        return (file_path.to_string(), edits.to_vec());
    }
    let is_markdown = std::path::Path::new(file_path)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("mdx"));
    let full_path = crate::utils::path::expand_path(file_path, Some(cwd))
        .unwrap_or_else(|_| std::path::PathBuf::from(file_path));
    let file_content = match crate::utils::file_read::read_file_sync(&full_path) {
        Ok(content) => content,
        Err(error) => {
            if error.kind() != std::io::ErrorKind::NotFound {
                crate::utils::debug::log_for_debugging(&format!(
                    "Unable to normalize Edit input from {}: {error}",
                    full_path.display()
                ));
            }
            return (file_path.to_string(), edits.to_vec());
        }
    };

    let normalized = edits
        .iter()
        .map(|edit| {
            let normalized_new = if is_markdown {
                edit.new_string.clone()
            } else {
                strip_trailing_whitespace(&edit.new_string)
            };
            if file_content.contains(&edit.old_string) {
                return EditInput {
                    old_string: edit.old_string.clone(),
                    new_string: normalized_new,
                    replace_all: edit.replace_all,
                };
            }
            let (desanitized_old, applied) = desanitize_match_string(&edit.old_string);
            if file_content.contains(&desanitized_old) {
                let mut desanitized_new = normalized_new;
                for (from, to) in applied {
                    desanitized_new = desanitized_new.replace(&from, &to);
                }
                return EditInput {
                    old_string: desanitized_old,
                    new_string: desanitized_new,
                    replace_all: edit.replace_all,
                };
            }
            EditInput {
                old_string: edit.old_string.clone(),
                new_string: normalized_new,
                replace_all: edit.replace_all,
            }
        })
        .collect();
    (file_path.to_string(), normalized)
}

/// Maps to: CC `utils.ts#areFileEditsEquivalent`.
pub fn are_file_edits_equivalent(
    edits1: &[FileEdit],
    edits2: &[FileEdit],
    original_content: &str,
) -> bool {
    if edits1.len() == edits2.len() && edits1.iter().zip(edits2.iter()).all(|(a, b)| a == b) {
        return true;
    }
    match (
        get_patch_for_edits(original_content, edits1),
        get_patch_for_edits(original_content, edits2),
    ) {
        (Err(e1), Err(e2)) => e1 == e2,
        (Ok((_, u1)), Ok((_, u2))) => u1 == u2,
        _ => false,
    }
}

/// Maps to: CC `utils.ts#areFileEditsInputsEquivalent`.
pub fn are_file_edits_inputs_equivalent(
    path1: &str,
    edits1: &[FileEdit],
    path2: &str,
    edits2: &[FileEdit],
    cwd: &std::path::Path,
) -> std::io::Result<bool> {
    if path1 != path2 {
        return Ok(false);
    }
    if edits1.len() == edits2.len() && edits1.iter().zip(edits2.iter()).all(|(a, b)| a == b) {
        return Ok(true);
    }
    let full_path = crate::utils::path::expand_path(path1, Some(cwd))
        .unwrap_or_else(|_| std::path::PathBuf::from(path1));
    let file_content = match crate::utils::file_read::read_file_sync(&full_path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error),
    };
    Ok(are_file_edits_equivalent(edits1, edits2, &file_content))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_actual_string_matches_curly_quotes() {
        let actual = find_actual_string("let x = “old”;", "let x = \"old\";");
        assert_eq!(actual.as_deref(), Some("let x = “old”;"));
    }

    #[test]
    fn preserve_quote_style_rewrites_new_string() {
        let actual_old = "let x = “old”;";
        let new = preserve_quote_style("let x = \"old\";", actual_old, "let x = \"new\";");
        assert_eq!(new, "let x = “new”;");
    }

    #[test]
    fn apply_edit_strips_trailing_newline_on_empty_replacement() {
        let updated = apply_edit_to_file("a\nold\nb\n", "old", "", false);
        assert_eq!(updated, "a\nb\n");
    }

    #[test]
    fn get_patch_for_edit_returns_structured_hunks() {
        let (patch, updated) =
            get_patch_for_edit("a\nold\nz\n", "old", "new", false).expect("edit applies");
        assert_eq!(updated, "a\nnew\nz\n");
        assert!(!patch.is_empty());
        assert!(patch[0].lines.iter().any(|line| line == "-old"));
        assert!(patch[0].lines.iter().any(|line| line == "+new"));
    }

    #[test]
    fn get_edits_for_patch_round_trips_simple_hunk() {
        let (patch, _) =
            get_patch_for_edit("a\nold\nz\n", "old", "new", false).expect("edit applies");
        let edits = get_edits_for_patch(&patch);
        assert_eq!(edits.len(), 1);
        assert!(edits[0].old_string.contains("old"));
        assert!(edits[0].new_string.contains("new"));
    }

    #[test]
    fn normalize_file_edit_input_desanitizes_and_respects_markdown_hard_breaks() {
        let root = std::env::temp_dir().join(format!(
            "nyan-edit-normalize-utils-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("code.txt"), "<function_results>old").unwrap();
        let (_, normalized) = normalize_file_edit_input(
            "code.txt",
            &[EditInput {
                old_string: "<fnr>old".to_string(),
                new_string: "<fnr>new   ".to_string(),
                replace_all: false,
            }],
            &root,
        );
        assert_eq!(normalized[0].old_string, "<function_results>old");
        assert_eq!(normalized[0].new_string, "<function_results>new");

        std::fs::write(root.join("README.md"), "old").unwrap();
        let (_, markdown) = normalize_file_edit_input(
            "README.md",
            &[EditInput {
                old_string: "old".to_string(),
                new_string: "new  \n".to_string(),
                replace_all: false,
            }],
            &root,
        );
        assert_eq!(markdown[0].new_string, "new  \n");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn are_file_edits_equivalent_compares_outcomes() {
        let content = "hello world\n";
        let a = [FileEdit {
            old_string: "world".into(),
            new_string: "there".into(),
            replace_all: false,
        }];
        let b = [FileEdit {
            old_string: "hello world".into(),
            new_string: "hello there".into(),
            replace_all: false,
        }];
        assert!(are_file_edits_equivalent(&a, &b, content));
    }
}
