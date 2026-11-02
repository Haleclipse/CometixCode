//! Compaction prompts and summary formatting.
//! Maps to CC `services/compact/prompt.ts`.

const COMPACT_PROMPT_TEMPLATE: &str = include_str!("base_compact_prompt.txt");
const CC_COMPACT_PROMPT_TEMPLATE: &str = include_str!("cc_compact_prompt.txt");

fn configured_summary_style() -> crate::utils::settings::nyan::SummaryStyle {
    crate::utils::settings::get_initial_settings()
        .nyan
        .map(|settings| settings.compaction().summary_style())
        .unwrap_or(crate::utils::settings::nyan::SummaryStyle::Legacy)
}

/// `summaryStyle = "cc"` must stay byte-identical to upstream, including the
/// custom-instruction heading; the legacy format appends an `Additional focus`
/// line instead.
fn custom_focus(
    custom_instructions: Option<&str>,
    style: crate::utils::settings::nyan::SummaryStyle,
) -> String {
    custom_instructions
        .filter(|instructions| !instructions.trim().is_empty())
        .map(|instructions| match style {
            crate::utils::settings::nyan::SummaryStyle::Cc => {
                format!("\n\nAdditional Instructions:\n{instructions}")
            }
            crate::utils::settings::nyan::SummaryStyle::Legacy => {
                format!("\n\nAdditional focus: {instructions}")
            }
        })
        .unwrap_or_default()
}

/// Maps to CC `getCompactPrompt(...)`. `summaryStyle = "cc"` restores the
/// upstream prompt; nyan's default is the structured legacy format.
pub fn get_compact_prompt(custom_instructions: Option<&str>) -> String {
    get_compact_prompt_with_style(custom_instructions, configured_summary_style())
}

pub fn get_compact_prompt_with_style(
    custom_instructions: Option<&str>,
    style: crate::utils::settings::nyan::SummaryStyle,
) -> String {
    let template = match style {
        crate::utils::settings::nyan::SummaryStyle::Legacy => COMPACT_PROMPT_TEMPLATE,
        crate::utils::settings::nyan::SummaryStyle::Cc => CC_COMPACT_PROMPT_TEMPLATE,
    };
    template.replace(
        "{{CUSTOM_INSTRUCTIONS}}",
        &custom_focus(custom_instructions, style),
    )
}

/// Maps to CC prompt.ts#getPartialCompactPrompt.
pub fn get_partial_compact_prompt(
    custom_instructions: Option<&str>,
    direction: crate::types::message::PartialCompactDirection,
) -> String {
    get_partial_compact_prompt_with_style(
        custom_instructions,
        direction,
        configured_summary_style(),
    )
}

pub fn get_partial_compact_prompt_with_style(
    custom_instructions: Option<&str>,
    direction: crate::types::message::PartialCompactDirection,
    style: crate::utils::settings::nyan::SummaryStyle,
) -> String {
    let template = match (style, direction) {
        (
            crate::utils::settings::nyan::SummaryStyle::Legacy,
            crate::types::message::PartialCompactDirection::From,
        ) => include_str!("partial_compact_from_prompt.txt"),
        (
            crate::utils::settings::nyan::SummaryStyle::Legacy,
            crate::types::message::PartialCompactDirection::UpTo,
        ) => include_str!("partial_compact_up_to_prompt.txt"),
        (
            crate::utils::settings::nyan::SummaryStyle::Cc,
            crate::types::message::PartialCompactDirection::From,
        ) => include_str!("cc_partial_compact_from_prompt.txt"),
        (
            crate::utils::settings::nyan::SummaryStyle::Cc,
            crate::types::message::PartialCompactDirection::UpTo,
        ) => include_str!("cc_partial_compact_up_to_prompt.txt"),
    };
    template.replace(
        "{{CUSTOM_INSTRUCTIONS}}",
        &custom_focus(custom_instructions, style),
    )
}

/// Maps to CC `formatCompactSummary(...)`.
pub fn format_compact_summary(summary: &str) -> String {
    let without_analysis = strip_first_tagged_section(summary, "analysis");
    let formatted = replace_first_summary_section(&without_analysis);
    collapse_blank_lines(&formatted).trim().to_string()
}

/// Maps to CC `getCompactUserSummaryMessage(...)`.
pub fn get_compact_user_summary_message(
    summary: &str,
    suppress_follow_up_questions: bool,
    transcript_path: Option<&str>,
    recent_messages_preserved: bool,
) -> String {
    let mut base = format!(
        "This session is being continued from a previous conversation that ran out of context. The summary below covers the earlier portion of the conversation.\n\n{}",
        format_compact_summary(summary)
    );
    if let Some(path) = transcript_path.filter(|path| !path.is_empty()) {
        base.push_str("\n\nIf you need specific details from before compaction (like exact code snippets, error messages, or content you generated), read the full transcript at: ");
        base.push_str(path);
    }
    if recent_messages_preserved {
        base.push_str("\n\nRecent messages are preserved verbatim.");
    }
    if suppress_follow_up_questions {
        base.push_str(
            "\nContinue the conversation from where it left off without asking the user any further questions. Resume directly — do not acknowledge the summary, do not recap what was happening, do not preface with \"I'll continue\" or similar. Pick up the last task as if the break never happened.",
        );
    }
    base
}

fn strip_first_tagged_section(input: &str, tag: &str) -> String {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let Some(start) = input.find(&open) else {
        return input.to_string();
    };
    let Some(relative_end) = input[start + open.len()..].find(&close) else {
        return input.to_string();
    };
    let end = start + open.len() + relative_end + close.len();
    format!("{}{}", &input[..start], &input[end..])
}

fn replace_first_summary_section(input: &str) -> String {
    let Some(start) = input.find("<summary>") else {
        return input.to_string();
    };
    let body_start = start + "<summary>".len();
    let Some(relative_end) = input[body_start..].find("</summary>") else {
        return input.to_string();
    };
    let body_end = body_start + relative_end;
    let end = body_end + "</summary>".len();
    format!(
        "{}Summary:\n{}{}",
        &input[..start],
        input[body_start..body_end].trim(),
        &input[end..]
    )
}

fn collapse_blank_lines(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut consecutive_newlines = 0usize;
    for ch in input.chars() {
        if ch == '\n' {
            consecutive_newlines += 1;
            if consecutive_newlines <= 2 {
                out.push(ch);
            }
        } else {
            consecutive_newlines = 0;
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_prompt_matches_official_no_tools_and_custom_instruction_shape() {
        let prompt = get_compact_prompt(Some("Focus on Rust changes"));
        assert!(prompt.starts_with("You are summarizing a coding conversation"));
        assert!(prompt.contains("Additional focus: Focus on Rust changes"));
        assert!(prompt.contains("Goal:"));
        assert!(prompt.contains("Critical Context:"));
    }

    #[test]
    fn compact_summary_strips_analysis_and_formats_summary_like_official() {
        assert_eq!(
            format_compact_summary(
                "<analysis>draft</analysis>\n\n<summary>\nUseful context\n</summary>"
            ),
            "Summary:\nUseful context"
        );
    }
    #[test]
    fn partial_prompt_matches_official_direction_and_instruction_order() {
        use crate::types::message::PartialCompactDirection;
        // CC prompt.ts:274-294, external text is copied verbatim to the two templates.
        let from =
            get_partial_compact_prompt(Some("  keep decisions  "), PartialCompactDirection::From);
        assert!(from.starts_with("You are summarizing the RECENT portion"));
        assert!(from.contains("RECENT portion of the conversation"));
        assert!(from.contains("Additional focus:   keep decisions  "));
        let up = get_partial_compact_prompt(Some(" "), PartialCompactDirection::UpTo);
        assert!(up.contains("newer messages that build on this context will follow"));
        assert!(!up.contains("Additional focus:"));
        assert!(up.contains("portion before the selected boundary"));
    }

    #[test]
    fn cc_style_restores_the_upstream_prompt_byte_for_byte() {
        let prompt = get_compact_prompt_with_style(
            Some("Focus on Rust changes"),
            crate::utils::settings::nyan::SummaryStyle::Cc,
        );
        assert!(prompt.starts_with("CRITICAL: Respond with TEXT ONLY."));
        assert!(prompt.contains("Additional Instructions:\nFocus on Rust changes"));
        assert!(prompt.ends_with("Tool calls will be rejected and you will fail the task."));
        // Placeholder consumed exactly once; no `Additional focus` leak.
        assert!(!prompt.contains("{{CUSTOM_INSTRUCTIONS}}"));
        assert!(!prompt.contains("Additional focus:"));
    }

    #[test]
    fn cc_partial_styles_restore_the_upstream_text() {
        for direction in [
            crate::types::message::PartialCompactDirection::From,
            crate::types::message::PartialCompactDirection::UpTo,
        ] {
            let prompt = get_partial_compact_prompt_with_style(
                Some("keep decisions"),
                direction,
                crate::utils::settings::nyan::SummaryStyle::Cc,
            );
            assert!(prompt.starts_with("CRITICAL: Respond with TEXT ONLY."));
            assert!(prompt.contains("Additional Instructions:\nkeep decisions"));
            assert!(prompt.ends_with("Tool calls will be rejected and you will fail the task."));
        }
    }
}
