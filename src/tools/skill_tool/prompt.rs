//! Maps to CC `tools/SkillTool/prompt.ts`.

pub const SKILL_BUDGET_CONTEXT_PERCENT: f64 = 0.01;
pub const CHARS_PER_TOKEN: usize = 4;
pub const DEFAULT_CHAR_BUDGET: usize = 8_000;
pub const MAX_LISTING_DESC_CHARS: usize = 250;

const MIN_DESC_LENGTH: usize = 20;

/// Maps to CC `tools/SkillTool/prompt.ts:getCharBudget`.
pub(crate) fn get_char_budget(context_window_tokens: i64) -> usize {
    let env_budget = crate::utils::process_env::var("SLASH_COMMAND_TOOL_CHAR_BUDGET")
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| *value != 0.0)
        .map(|value| value.max(0.0) as usize);
    env_budget.unwrap_or_else(|| {
        if context_window_tokens > 0 {
            ((context_window_tokens as f64) * CHARS_PER_TOKEN as f64 * SKILL_BUDGET_CONTEXT_PERCENT)
                .floor() as usize
        } else {
            DEFAULT_CHAR_BUDGET
        }
    })
}

/// Maps to CC `tools/SkillTool/prompt.ts:getCommandDescription`.
fn get_command_description(command: &crate::commands::Command) -> String {
    // JS `.length` counts UTF-16 code units. Rust cannot construct an invalid
    // lone surrogate, so truncation keeps complete scalar values while using
    // the same UTF-16 budget for the normal (and all valid) text path.
    let description = match command
        .when_to_use
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        Some(when_to_use) => format!("{} - {when_to_use}", command.description),
        None => command.description.to_string(),
    };
    if description.encode_utf16().count() <= MAX_LISTING_DESC_CHARS {
        return description;
    }

    let mut units = 0usize;
    let mut truncated = String::new();
    for character in description.chars() {
        let character_units = character.len_utf16();
        if units + character_units > MAX_LISTING_DESC_CHARS - 1 {
            break;
        }
        units += character_units;
        truncated.push(character);
    }
    format!("{truncated}…")
}

/// Maps to CC `tools/SkillTool/prompt.ts:formatCommandDescription` and
/// `formatCommandsWithinBudget`.
pub(crate) fn format_commands_within_budget(
    commands: &[crate::commands::Command],
    context_window_tokens: i64,
) -> String {
    use unicode_width::UnicodeWidthStr;

    if commands.is_empty() {
        return String::new();
    }

    let budget = get_char_budget(context_window_tokens);
    let full = commands
        .iter()
        .map(|command| format!("- {}: {}", command.name, get_command_description(command)))
        .collect::<Vec<_>>();
    let full_width = full
        .iter()
        .map(|line| UnicodeWidthStr::width(line.as_str()))
        .sum::<usize>()
        .saturating_add(full.len().saturating_sub(1));
    if full_width <= budget {
        return full.join("\n");
    }

    let bundled = commands
        .iter()
        .enumerate()
        .filter_map(|(index, command)| {
            (command.source == crate::commands::CommandSource::Bundled).then_some(index)
        })
        .collect::<std::collections::HashSet<_>>();
    let bundled_width = full
        .iter()
        .enumerate()
        .filter(|(index, _)| bundled.contains(index))
        .map(|(_, line)| UnicodeWidthStr::width(line.as_str()).saturating_add(1))
        .sum::<usize>();
    let rest_count = commands.len().saturating_sub(bundled.len());
    if rest_count == 0 {
        return full.join("\n");
    }

    let name_overhead = commands
        .iter()
        .enumerate()
        .filter(|(index, _)| !bundled.contains(index))
        .map(|(_, command)| UnicodeWidthStr::width(command.name.as_ref()).saturating_add(4))
        .sum::<usize>()
        .saturating_add(rest_count.saturating_sub(1));
    let max_description_width = budget
        .saturating_sub(bundled_width)
        .saturating_sub(name_overhead)
        / rest_count;
    commands
        .iter()
        .enumerate()
        .map(|(index, command)| {
            if bundled.contains(&index) {
                return full[index].clone();
            }
            if max_description_width < MIN_DESC_LENGTH {
                return format!("- {}", command.name);
            }
            let description = get_command_description(command);
            if UnicodeWidthStr::width(description.as_str()) <= max_description_width {
                format!("- {}: {description}", command.name)
            } else {
                format!(
                    "- {}: {}",
                    command.name,
                    crate::utils::truncate::truncate(&description, max_description_width, false)
                )
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn get_prompt() -> String {
    r#"Execute a skill within the main conversation

When users ask you to perform tasks, check if any of the available skills match. Skills provide specialized capabilities and domain knowledge.

When users reference a "slash command" or "/<something>" (e.g., "/commit", "/review-pr"), they are referring to a skill. Use this tool to invoke it.

How to invoke:
- Use this tool with the skill name and optional arguments
- Examples:
  - `skill: "pdf"` - invoke the pdf skill
  - `skill: "commit", args: "-m 'Fix bug'"` - invoke with arguments
  - `skill: "review-pr", args: "123"` - invoke with arguments
  - `skill: "ms-office-suite:pdf"` - invoke using fully qualified name

Important:
- Available skills are listed in system-reminder messages in the conversation
- When a skill matches the user's request, this is a BLOCKING REQUIREMENT: invoke the relevant Skill tool BEFORE generating any other response about the task
- NEVER mention a skill without actually calling this tool
- Do not invoke a skill that is already running
- Do not use this tool for built-in CLI commands (like /help, /clear, etc.)
- If you see a <command-name> tag in the current conversation turn, the skill has ALREADY been loaded - follow the instructions directly instead of calling this tool again
"#.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(description: &str) -> crate::commands::Command {
        crate::commands::Command::from_mcp_prompt(
            crate::services::mcp::client::McpPromptCommandSnapshot {
                name: "fixture-skill".to_string(),
                description: description.to_string(),
                has_user_specified_description: true,
                user_facing_name: "fixture-skill".to_string(),
                arg_names: Vec::new(),
                source: "fixture",
            },
        )
    }

    #[test]
    fn description_does_not_append_separator_for_empty_when_to_use() {
        let mut fixture = command("Use this skill");
        fixture.when_to_use = Some("".into());
        assert_eq!(get_command_description(&fixture), "Use this skill");
    }

    #[test]
    fn description_cap_uses_utf16_budget_without_splitting_scalar_values() {
        let mut fixture = command(&format!("{}😀", "a".repeat(249)));
        let description = get_command_description(&fixture);
        assert_eq!(description.encode_utf16().count(), MAX_LISTING_DESC_CHARS);
        assert!(description.ends_with('…'));
        assert_eq!(description.chars().count(), 250);

        fixture.when_to_use = Some("Use it".into());
        let with_when = get_command_description(&fixture);
        assert!(with_when.ends_with('…'));
        assert_eq!(with_when.encode_utf16().count(), MAX_LISTING_DESC_CHARS);
    }

    #[test]
    fn empty_command_listing_is_empty() {
        assert_eq!(format_commands_within_budget(&[], 200_000), "");
    }
}
