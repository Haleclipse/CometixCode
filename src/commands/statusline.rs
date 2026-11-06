//! Maps to: CC `commands/statusline.tsx`.

use crate::tools::agent_tool::constants::AGENT_TOOL_NAME;
use crate::types::message::UserContent;

/// Maps to: CC `commands/statusline.tsx:19-28#statusline.getPromptForCommand`.
/// Uses the established `Typed command executable pointer` carrier.
pub fn get_prompt_for_command(
    _command: &super::Command,
    args: &str,
    _context: &crate::tool::ToolUseContext,
) -> anyhow::Result<Vec<UserContent>> {
    // CC uses ECMAScript trim: FEFF is whitespace, U+0085 is not.
    let prompt =
        args.trim_matches(|ch: char| (ch.is_whitespace() && ch != '\u{85}') || ch == '\u{feff}');
    let prompt = if prompt.is_empty() {
        "Configure my statusLine from my shell PS1 configuration"
    } else {
        prompt
    };
    Ok(vec![UserContent::Text(format!(
        "Create an {AGENT_TOOL_NAME} with subagent_type \"general-purpose\" and the prompt \"{prompt}\""
    ))])
}

/// Maps to: CC `commands/statusline.tsx:5-29#statusline` descriptor.
pub fn command() -> super::Command {
    let mut command = super::Command::prompt("statusline", "Set up nyan's status line UI")
        .prompt_metadata("setting up statusLine", 0)
        .disable_non_interactive()
        .prompt_executable(get_prompt_for_command);
    command.allowed_tools = [
        AGENT_TOOL_NAME,
        "Read(~/**)",
        "Edit(~/.claude/settings.json)",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_matches_official_statusline_command() {
        let command = command();
        assert_eq!(command.name, "statusline");
        assert_eq!(command.description, "Set up nyan's status line UI");
        assert_eq!(command.kind, super::super::CommandKind::Prompt);
        assert_eq!(command.source, super::super::CommandSource::Builtin);
        assert_eq!(command.content_length, Some(0));
        assert_eq!(
            command.progress_message.as_deref(),
            Some("setting up statusLine")
        );
        assert!(command.aliases.is_empty());
        assert!(command.disable_non_interactive);
        assert!(command.get_prompt_for_command.is_some());
        assert!(command.prompt_command.is_none());
        assert_eq!(
            command.allowed_tools,
            ["Agent", "Read(~/**)", "Edit(~/.claude/settings.json)"]
        );
    }

    #[test]
    fn get_prompt_for_command_matches_official_bun_arguments() {
        // Bun direct import oracle: research/proof/statusline-0913/bun-oracle.json.
        // Deliberately retain quotes/newlines: source template interpolation
        // does not JSON-escape user input.
        let command = command();
        let context = crate::tool::ToolUseContext::default();
        for (args, expected) in [
            (
                "",
                "Create an Agent with subagent_type \"general-purpose\" and the prompt \"Configure my statusLine from my shell PS1 configuration\"",
            ),
            (
                "   ",
                "Create an Agent with subagent_type \"general-purpose\" and the prompt \"Configure my statusLine from my shell PS1 configuration\"",
            ),
            (
                " use model and cwd ",
                "Create an Agent with subagent_type \"general-purpose\" and the prompt \"use model and cwd\"",
            ),
            (
                "\t\r\n",
                "Create an Agent with subagent_type \"general-purpose\" and the prompt \"Configure my statusLine from my shell PS1 configuration\"",
            ),
            (
                "\u{feff}",
                "Create an Agent with subagent_type \"general-purpose\" and the prompt \"Configure my statusLine from my shell PS1 configuration\"",
            ),
            (
                "\u{85}",
                "Create an Agent with subagent_type \"general-purpose\" and the prompt \"\u{85}\"",
            ),
            (
                "\u{feff} 中文 🌙 \u{feff}",
                "Create an Agent with subagent_type \"general-purpose\" and the prompt \"中文 🌙\"",
            ),
            (
                "say \"hello\"\nnext",
                "Create an Agent with subagent_type \"general-purpose\" and the prompt \"say \"hello\"\nnext\"",
            ),
            (
                "\u{2028}\u{2029}",
                "Create an Agent with subagent_type \"general-purpose\" and the prompt \"Configure my statusLine from my shell PS1 configuration\"",
            ),
        ] {
            assert_eq!(
                get_prompt_for_command(&command, args, &context).unwrap(),
                vec![UserContent::Text(expected.to_string())],
                "args={args:?}"
            );
        }
    }
}
