//! Maps to: CC `utils/contextSuggestions.ts`.

use crate::utils::analyze_context::ContextData;
use crate::utils::file::get_display_path;
use crate::utils::format::format_tokens;

const LARGE_TOOL_RESULT_PERCENT: f64 = 15.0;
const LARGE_TOOL_RESULT_TOKENS: u64 = 10_000;
const READ_BLOAT_PERCENT: f64 = 5.0;
const NEAR_CAPACITY_PERCENT: u64 = 80;
const MEMORY_HIGH_PERCENT: f64 = 5.0;
const MEMORY_HIGH_TOKENS: u64 = 5_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SuggestionSeverity {
    Info,
    Warning,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextSuggestion {
    pub severity: SuggestionSeverity,
    pub title: String,
    pub detail: String,
    pub savings_tokens: Option<u64>,
}

/// Maps to: CC `generateContextSuggestions(data)`.
pub fn generate_context_suggestions(data: &ContextData) -> Vec<ContextSuggestion> {
    let mut suggestions = Vec::new();
    check_near_capacity(data, &mut suggestions);
    check_large_tool_results(data, &mut suggestions);
    check_read_result_bloat(data, &mut suggestions);
    check_memory_bloat(data, &mut suggestions);
    check_auto_compact_disabled(data, &mut suggestions);
    suggestions.sort_by(|left, right| {
        let left_rank = matches!(left.severity, SuggestionSeverity::Warning);
        let right_rank = matches!(right.severity, SuggestionSeverity::Warning);
        right_rank.cmp(&left_rank).then_with(|| {
            right
                .savings_tokens
                .unwrap_or(0)
                .cmp(&left.savings_tokens.unwrap_or(0))
        })
    });
    suggestions
}

fn check_near_capacity(data: &ContextData, suggestions: &mut Vec<ContextSuggestion>) {
    if data.percentage < NEAR_CAPACITY_PERCENT {
        return;
    }
    suggestions.push(ContextSuggestion {
        severity: SuggestionSeverity::Warning,
        title: format!("Context is {}% full", data.percentage),
        detail: if data.is_auto_compact_enabled {
            "Autocompact will trigger soon, which discards older messages. Use /compact now to control what gets kept."
                .to_string()
        } else {
            "Autocompact is disabled. Use /compact to free space, or enable autocompact in /config."
                .to_string()
        },
        savings_tokens: None,
    });
}

fn check_large_tool_results(data: &ContextData, suggestions: &mut Vec<ContextSuggestion>) {
    let Some(breakdown) = &data.message_breakdown else {
        return;
    };
    for tool in &breakdown.tool_calls_by_type {
        let tokens = tool.call_tokens.saturating_add(tool.result_tokens);
        let percent = percent(tokens, data.raw_max_tokens);
        if percent < LARGE_TOOL_RESULT_PERCENT || tokens < LARGE_TOOL_RESULT_TOKENS {
            continue;
        }
        if let Some(suggestion) = large_tool_suggestion(&tool.name, tokens, percent) {
            suggestions.push(suggestion);
        }
    }
}

fn large_tool_suggestion(tool_name: &str, tokens: u64, percent: f64) -> Option<ContextSuggestion> {
    let token_text = format_tokens(tokens);
    let rounded = percent.round() as u64;
    let (severity, title, detail, savings) = match tool_name {
        crate::tools::bash_tool::tool_name::BASH_TOOL_NAME => (
            SuggestionSeverity::Warning,
            format!("Bash results using {token_text} tokens ({rounded}%)"),
            "Pipe output through head, tail, or grep to reduce result size. Avoid cat on large files — use Read with offset/limit instead.",
            0.5,
        ),
        crate::tools::file_read_tool::prompt::FILE_READ_TOOL_NAME => (
            SuggestionSeverity::Info,
            format!("Read results using {token_text} tokens ({rounded}%)"),
            "Use offset and limit parameters to read only the sections you need. Avoid re-reading entire files when you only need a few lines.",
            0.3,
        ),
        crate::tools::grep_tool::prompt::GREP_TOOL_NAME => (
            SuggestionSeverity::Info,
            format!("Grep results using {token_text} tokens ({rounded}%)"),
            "Add more specific patterns or use the glob or type parameter to narrow file types. Consider Glob for file discovery instead of Grep.",
            0.3,
        ),
        crate::tools::web_fetch_tool::prompt::WEB_FETCH_TOOL_NAME => (
            SuggestionSeverity::Info,
            format!("WebFetch results using {token_text} tokens ({rounded}%)"),
            "Web page content can be very large. Consider extracting only the specific information needed.",
            0.4,
        ),
        _ if percent >= 20.0 => (
            SuggestionSeverity::Info,
            format!("{tool_name} using {token_text} tokens ({rounded}%)"),
            "This tool is consuming a significant portion of context.",
            0.2,
        ),
        _ => return None,
    };
    Some(ContextSuggestion {
        severity,
        title,
        detail: detail.to_string(),
        savings_tokens: Some((tokens as f64 * savings).floor() as u64),
    })
}

fn check_read_result_bloat(data: &ContextData, suggestions: &mut Vec<ContextSuggestion>) {
    let Some(read) = data.message_breakdown.as_ref().and_then(|breakdown| {
        breakdown
            .tool_calls_by_type
            .iter()
            .find(|tool| tool.name == crate::tools::file_read_tool::prompt::FILE_READ_TOOL_NAME)
    }) else {
        return;
    };
    let total = read.call_tokens.saturating_add(read.result_tokens);
    let total_percent = percent(total, data.raw_max_tokens);
    if total_percent >= LARGE_TOOL_RESULT_PERCENT && total >= LARGE_TOOL_RESULT_TOKENS {
        return;
    }
    let result_percent = percent(read.result_tokens, data.raw_max_tokens);
    if result_percent >= READ_BLOAT_PERCENT && read.result_tokens >= LARGE_TOOL_RESULT_TOKENS {
        suggestions.push(ContextSuggestion {
            severity: SuggestionSeverity::Info,
            title: format!(
                "File reads using {} tokens ({}%)",
                format_tokens(read.result_tokens),
                result_percent.round() as u64
            ),
            detail: "If you are re-reading files, consider referencing earlier reads. Use offset/limit for large files."
                .to_string(),
            savings_tokens: Some((read.result_tokens as f64 * 0.3).floor() as u64),
        });
    }
}

fn check_memory_bloat(data: &ContextData, suggestions: &mut Vec<ContextSuggestion>) {
    let total = data
        .memory_files
        .iter()
        .map(|file| file.tokens)
        .sum::<u64>();
    let memory_percent = percent(total, data.raw_max_tokens);
    if memory_percent < MEMORY_HIGH_PERCENT || total < MEMORY_HIGH_TOKENS {
        return;
    }
    let mut files = data.memory_files.iter().collect::<Vec<_>>();
    files.sort_by_key(|file| std::cmp::Reverse(file.tokens));
    let largest = files
        .into_iter()
        .take(3)
        .map(|file| {
            format!(
                "{} ({})",
                get_display_path(&file.path),
                format_tokens(file.tokens)
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    suggestions.push(ContextSuggestion {
        severity: SuggestionSeverity::Info,
        title: format!(
            "Memory files using {} tokens ({}%)",
            format_tokens(total),
            memory_percent.round() as u64
        ),
        detail: format!("Largest: {largest}. Use /memory to review and prune stale entries."),
        savings_tokens: Some((total as f64 * 0.3).floor() as u64),
    });
}

fn check_auto_compact_disabled(data: &ContextData, suggestions: &mut Vec<ContextSuggestion>) {
    if !data.is_auto_compact_enabled
        && data.percentage >= 50
        && data.percentage < NEAR_CAPACITY_PERCENT
    {
        suggestions.push(ContextSuggestion {
            severity: SuggestionSeverity::Info,
            title: "Autocompact is disabled".to_string(),
            detail: "Without autocompact, you will hit context limits and lose the conversation. Enable it in /config or use /compact manually."
                .to_string(),
            savings_tokens: None,
        });
    }
}

fn percent(tokens: u64, max: u64) -> f64 {
    if max == 0 {
        0.0
    } else {
        tokens as f64 / max as f64 * 100.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggestions_warn_near_capacity_and_sort_warnings_first() {
        let data = ContextData {
            percentage: 85,
            raw_max_tokens: 100_000,
            is_auto_compact_enabled: true,
            memory_files: vec![crate::utils::analyze_context::ContextMemoryFileInfo {
                path: "/repo/AGENTS.md".to_string(),
                file_type: "Project".to_string(),
                tokens: 6_000,
            }],
            ..ContextData::default()
        };
        let suggestions = generate_context_suggestions(&data);
        assert_eq!(suggestions[0].severity, SuggestionSeverity::Warning);
        assert_eq!(suggestions[0].title, "Context is 85% full");
        assert!(
            suggestions
                .iter()
                .any(|item| item.title.starts_with("Memory files using"))
        );
    }
}
