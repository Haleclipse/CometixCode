//! Width-aware truncation and wrapping.
//! Maps to: CC `utils/truncate.ts`.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Maps to: CC `utils/truncate.ts:16-53` `truncatePathMiddle`.
pub fn truncate_path_middle(path: &str, max_length: usize) -> String {
    if UnicodeWidthStr::width(path) <= max_length {
        return path.to_string();
    }
    if max_length == 0 {
        return "…".to_string();
    }
    if max_length < 5 {
        return truncate_to_width(path, max_length);
    }

    let last_slash = path.rfind('/');
    let filename = last_slash.map(|index| &path[index..]).unwrap_or(path);
    let directory = last_slash.map(|index| &path[..index]).unwrap_or("");
    let filename_width = UnicodeWidthStr::width(filename);
    if filename_width >= max_length.saturating_sub(1) {
        return truncate_start_to_width(path, max_length);
    }

    let available_for_dir = max_length.saturating_sub(1 + filename_width);
    if available_for_dir == 0 {
        return truncate_start_to_width(filename, max_length);
    }

    format!(
        "{}…{}",
        truncate_to_width_no_ellipsis(directory, available_for_dir),
        filename
    )
}

/// Maps to: CC `utils/truncate.ts:63-75` `truncateToWidth`.
pub fn truncate_to_width(text: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_string();
    }
    if max_width <= 1 {
        return "…".to_string();
    }

    let mut width = 0usize;
    let mut result = String::new();
    for grapheme in UnicodeSegmentation::graphemes(text, true) {
        let grapheme_width = UnicodeWidthStr::width(grapheme);
        if width + grapheme_width > max_width - 1 {
            break;
        }
        result.push_str(grapheme);
        width += grapheme_width;
    }
    result.push('…');
    result
}

/// Maps to: CC `utils/truncate.ts:82-102` `truncateStartToWidth`.
pub fn truncate_start_to_width(text: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_string();
    }
    if max_width <= 1 {
        return "…".to_string();
    }

    let mut width = 0usize;
    let mut kept = Vec::new();
    for grapheme in UnicodeSegmentation::graphemes(text, true).rev() {
        let grapheme_width = UnicodeWidthStr::width(grapheme);
        if width + grapheme_width > max_width - 1 {
            break;
        }
        kept.push(grapheme);
        width += grapheme_width;
    }
    kept.reverse();
    format!("…{}", kept.concat())
}

/// Maps to: CC `utils/truncate.ts:108-128` `truncateToWidthNoEllipsis`.
pub fn truncate_to_width_no_ellipsis(text: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_string();
    }
    if max_width == 0 {
        return String::new();
    }

    let mut width = 0usize;
    let mut result = String::new();
    for grapheme in UnicodeSegmentation::graphemes(text, true) {
        let grapheme_width = UnicodeWidthStr::width(grapheme);
        if width + grapheme_width > max_width {
            break;
        }
        result.push_str(grapheme);
        width += grapheme_width;
    }
    result
}

/// Maps to: CC `utils/truncate.ts:134-160` `truncate`.
pub fn truncate(str_value: &str, max_width: usize, single_line: bool) -> String {
    let mut result = str_value;
    if single_line && let Some(first_newline) = str_value.find('\n') {
        result = &str_value[..first_newline];
        if UnicodeWidthStr::width(result) + 1 > max_width {
            return truncate_to_width(result, max_width);
        }
        return format!("{result}…");
    }

    if UnicodeWidthStr::width(result) <= max_width {
        result.to_string()
    } else {
        truncate_to_width(result, max_width)
    }
}

/// Maps to: CC `utils/truncate.ts:162-181` `wrapText`.
pub fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current_line = String::new();
    let mut current_width = 0usize;

    for grapheme in UnicodeSegmentation::graphemes(text, true) {
        let grapheme_width = UnicodeWidthStr::width(grapheme);
        if current_width + grapheme_width <= width {
            current_line.push_str(grapheme);
            current_width += grapheme_width;
        } else {
            if !current_line.is_empty() {
                lines.push(current_line);
            }
            current_line = grapheme.to_string();
            current_width = grapheme_width;
        }
    }
    if !current_line.is_empty() {
        lines.push(current_line);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn width_helpers_match_official_middle_path_shape() {
        assert_eq!(truncate_to_width("abcdef", 4), "abc…");
        assert_eq!(truncate_to_width("abcdef", 1), "…");
        assert_eq!(truncate_start_to_width("abcdef", 4), "…def");
        assert_eq!(truncate_to_width_no_ellipsis("abcdef", 4), "abcd");
        assert_eq!(
            truncate_path_middle("src/components/deep/MyComponent.tsx", 30),
            "src/component…/MyComponent.tsx"
        );
        assert_eq!(truncate_path_middle("abcdef", 4), "abc…");
    }

    #[test]
    fn truncate_preserves_official_width_and_single_line_contract() {
        assert_eq!(truncate("abcdef", 4, false), "abc…");
        assert_eq!(truncate("ab\ncd", 8, true), "ab…");
        assert_eq!(truncate("abcdef\ncd", 4, true), "abc…");
        assert_eq!(wrap_text("abcdef", 3), vec!["abc", "def"]);
    }
}
