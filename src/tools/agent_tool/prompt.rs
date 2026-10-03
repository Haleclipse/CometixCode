//! Maps to CC `tools/AgentTool/prompt.ts`.

use crate::tools::agent_tool::constants::AGENT_TOOL_NAME;
use crate::tools::agent_tool::load_agents_dir::AgentDefinition;
use crate::tools::file_read_tool::prompt::FILE_READ_TOOL_NAME;
use crate::tools::file_write_tool::prompt::FILE_WRITE_TOOL_NAME;
use crate::tools::glob_tool::prompt::GLOB_TOOL_NAME;
use crate::tools::send_message_tool::prompt::SEND_MESSAGE_TOOL_NAME;

/// Maps to CC `tools/AgentTool/prompt.ts:59-64`
/// `shouldInjectAgentListInMessages()`.
pub fn should_inject_agent_list_in_messages() -> bool {
    // The `isEnvDefinedFalsy` branch and the GrowthBook fallback both yield
    // `false`: GrowthBook delivery is intentionally absent; preserve the source
    // default rather than assigning this process to a fabricated cohort.
    crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_AGENT_LIST_IN_MESSAGES").as_deref(),
    )
}

/// Maps to CC `tools/AgentTool/prompt.ts:15-37` `getToolsDescription(agent)`.
fn tools_description(agent: &AgentDefinition) -> String {
    match (&agent.tools, &agent.disallowed_tools) {
        (Some(allowed), Some(denied)) if !allowed.is_empty() && !denied.is_empty() => {
            let effective = allowed
                .iter()
                .filter(|name| !denied.contains(name))
                .cloned()
                .collect::<Vec<_>>();
            if effective.is_empty() {
                "None".to_string()
            } else {
                effective.join(", ")
            }
        }
        (Some(allowed), _) if !allowed.is_empty() => allowed.join(", "),
        (_, Some(denied)) if !denied.is_empty() => {
            format!("All tools except {}", denied.join(", "))
        }
        _ => "All tools".to_string(),
    }
}

/// Maps to CC `tools/AgentTool/prompt.ts:43-46` `formatAgentLine(agent)`.
pub fn format_agent_line(agent: &AgentDefinition) -> String {
    format!(
        "- {}: {} (Tools: {})",
        agent.agent_type,
        agent.when_to_use,
        tools_description(agent)
    )
}

/// Maps to CC `tools/AgentTool/prompt.ts:80-97` fork-semantics section.
fn when_to_fork_section() -> String {
    format!(
        "\n\n## When to fork\n\nFork yourself (omit `subagent_type`) when the intermediate tool output isn't worth keeping in your context. The criterion is qualitative \u{2014} \"will I need this output again\" \u{2014} not task size.\n- **Research**: fork open-ended questions. If research can be broken into independent questions, launch parallel forks in one message. A fork beats a fresh subagent for this \u{2014} it inherits context and shares your cache.\n- **Implementation**: prefer to fork implementation work that requires more than a couple of edits. Do research before jumping to implementation.\n\nForks are cheap because they share your prompt cache. Don't set `model` on a fork \u{2014} a different model can't reuse the parent's cache. Pass a short `name` (one or two words, lowercase) so the user can see the fork in the teams panel and steer it mid-run.\n\n**Don't peek.** The tool result includes an `output_file` path — do not Read or tail it unless the user explicitly asks for a progress check. You get a completion notification; trust it. Reading the transcript mid-flight pulls the fork's tool noise into your context, which defeats the point of forking.\n\n**Don't race.** After launching, you know nothing about what the fork found. Never fabricate or predict fork results in any format — not as prose, summary, or structured output. The notification arrives as a user-role message in a later turn; it is never something you write yourself. If the user asks a follow-up before the notification lands, tell them the fork is still running — give status, not a guess.\n\n**Writing a fork prompt.** Since the fork inherits your context, the prompt is a *directive* — what to do, not what the situation is. Be specific about scope: what's in, what's out, what another agent is handling. Don't re-explain background.\n"
    )
}

/// Maps to CC `tools/AgentTool/prompt.ts:99-113` `writingThePromptSection`.
fn writing_the_prompt_section(fork_enabled: bool) -> String {
    let fresh_agent_lead_in = if fork_enabled {
        "When spawning a fresh agent (with a `subagent_type`), it starts with zero context. "
    } else {
        ""
    };
    let terse_lead_in = if fork_enabled {
        "For fresh agents, terse"
    } else {
        "Terse"
    };
    format!(
        "\n\n## Writing the prompt\n\n{fresh_agent_lead_in}Brief the agent like a smart colleague who just walked into the room — it hasn't seen this conversation, doesn't know what you've tried, doesn't understand why this task matters.\n- Explain what you're trying to accomplish and why.\n- Describe what you've already learned or ruled out.\n- Give enough context about the surrounding problem that the agent can make judgment calls rather than just following a narrow instruction.\n- If you need a short response, say so (\"report in under 200 words\").\n- Lookups: hand over the exact command. Investigations: hand over the question — prescribed steps become dead weight when the premise is wrong.\n\n{terse_lead_in} command-style prompts produce shallow, generic work.\n\n**Never delegate understanding.** Don't write \"based on your findings, fix the bug\" or \"based on the research, implement it.\" Those phrases push synthesis onto the agent instead of doing it yourself. Write prompts that prove you understood: include file paths, line numbers, what specifically to change.\n"
    )
}

/// Maps to CC `tools/AgentTool/prompt.ts:115-154` `forkExamples`.
fn fork_examples() -> String {
    format!(
        "Example usage:\n\n<example>\nuser: \"What's left on this branch before we can ship?\"\nassistant: <thinking>Forking this \u{2014} it's a survey question. I want the punch list, not the git output in my context.</thinking>\n{AGENT_TOOL_NAME}({{\n  name: \"ship-audit\",\n  description: \"Branch ship-readiness audit\",\n  prompt: \"Audit what's left before this branch can ship. Check: uncommitted changes, commits ahead of main, whether tests exist, whether the GrowthBook gate is wired up, whether CI-relevant files changed. Report a punch list \u{2014} done vs. missing. Under 200 words.\"\n}})\nassistant: Ship-readiness audit running.\n<commentary>\nTurn ends here. The coordinator knows nothing about the findings yet. What follows is a SEPARATE turn \u{2014} the notification arrives from outside, as a user-role message. It is not something the coordinator writes.\n</commentary>\n[later turn \u{2014} notification arrives as user message]\nassistant: Audit's back. Three blockers: no tests for the new prompt path, GrowthBook gate wired but not in build_flags.yaml, and one uncommitted file.\n</example>\n\n<example>\nuser: \"so is the gate wired up or not\"\n<commentary>\nUser asks mid-wait. The audit fork was launched to answer exactly this, and it hasn't returned. The coordinator does not have this answer. Give status, not a fabricated result.\n</commentary>\nassistant: Still waiting on the audit \u{2014} that's one of the things it's checking. Should land shortly.\n</example>\n\n<example>\nuser: \"Can you get a second opinion on whether this migration is safe?\"\nassistant: <thinking>I'll ask the code-reviewer agent — it won't see my analysis, so it can give an independent read.</thinking>\n<commentary>\nA subagent_type is specified, so the agent starts fresh. It needs full context in the prompt. The briefing explains what to assess and why.\n</commentary>\n{AGENT_TOOL_NAME}({{\n  name: \"migration-review\",\n  description: \"Independent migration review\",\n  subagent_type: \"code-reviewer\",\n  prompt: \"Review migration 0042_user_schema.sql for safety. Context: we're adding a NOT NULL column to a 50M-row table. Existing rows get a backfill default. I want a second opinion on whether the backfill approach is safe under concurrent writes — I've checked locking behavior but want independent verification. Report: is this safe, and if not, what specifically breaks?\"\n}})\n</example>\n"
    )
}

/// Maps to CC `tools/AgentTool/prompt.ts:156-188` `currentExamples`.
fn current_examples() -> String {
    format!(
        "Example usage:\n\n<example_agent_descriptions>\n\"test-runner\": use this agent after you are done writing code to run tests\n\"greeting-responder\": use this agent to respond to user greetings with a friendly joke\n</example_agent_descriptions>\n\n<example>\nuser: \"Please write a function that checks if a number is prime\"\nassistant: I'm going to use the {FILE_WRITE_TOOL_NAME} tool to write the following code:\n<code>\nfunction isPrime(n) {{\n  if (n <= 1) return false\n  for (let i = 2; i * i <= n; i++) {{\n    if (n % i === 0) return false\n  }}\n  return true\n}}\n</code>\n<commentary>\nSince a significant piece of code was written and the task was completed, now use the test-runner agent to run the tests\n</commentary>\nassistant: Uses the {AGENT_TOOL_NAME} tool to launch the test-runner agent\n</example>\n\n<example>\nuser: \"Hello\"\n<commentary>\nSince the user is greeting, use the greeting-responder agent to respond with a friendly joke\n</commentary>\nassistant: \"I'm going to use the {AGENT_TOOL_NAME} tool to launch the greeting-responder agent\"\n</example>\n"
    )
}

/// Maps to CC `tools/AgentTool/prompt.ts:66-287` `getPrompt(...)`.
///
/// CC also branches on `isInProcessTeammate()` for the trailing spawn-parameter
/// note. That identity lives in an AsyncLocalStorage store CC reads ambiently;
/// this port carries it on `ToolUseContext.agent_id`, which the schema builder
/// has no access to, so only the `isTeammate()` branch is reachable here.
pub fn get_prompt(
    agent_definitions: &[AgentDefinition],
    is_coordinator: bool,
    allowed_agent_types: Option<&[String]>,
) -> String {
    let effective_agents = match allowed_agent_types {
        Some(allowed) => agent_definitions
            .iter()
            .filter(|agent| allowed.contains(&agent.agent_type))
            .collect::<Vec<_>>(),
        None => agent_definitions.iter().collect::<Vec<_>>(),
    };

    let fork_enabled = crate::tools::agent_tool::fork_subagent::is_fork_subagent_enabled();
    let when_to_fork = if fork_enabled {
        when_to_fork_section()
    } else {
        String::new()
    };

    // When the gate is on, the agent list lives in an `agent_listing_delta`
    // attachment (see `utils/attachments.rs`) instead of inline, keeping the
    // tool description static so the tools-block prompt cache survives
    // MCP/plugin/permission changes.
    let list_via_attachment = should_inject_agent_list_in_messages();
    let agent_list_section = if list_via_attachment {
        "Available agent types are listed in <system-reminder> messages in the conversation."
            .to_string()
    } else {
        format!(
            "Available agent types and the tools they have access to:\n{}",
            effective_agents
                .iter()
                .map(|agent| format_agent_line(agent))
                .collect::<Vec<_>>()
                .join("\n")
        )
    };

    let subagent_type_note = if fork_enabled {
        format!(
            "When using the {AGENT_TOOL_NAME} tool, specify a subagent_type to use a specialized agent, or omit it to fork yourself — a fork inherits your full conversation context."
        )
    } else {
        format!(
            "When using the {AGENT_TOOL_NAME} tool, specify a subagent_type parameter to select which agent type to use. If omitted, the general-purpose agent is used."
        )
    };

    let shared = format!(
        "Launch a new agent to handle complex, multi-step tasks autonomously.\n\nThe {AGENT_TOOL_NAME} tool launches specialized agents (subprocesses) that autonomously handle complex tasks. Each agent type has specific capabilities and tools available to it.\n\n{agent_list_section}\n\n{subagent_type_note}"
    );

    // The coordinator system prompt already covers usage notes, examples, and
    // when-not-to-use guidance, so coordinator mode gets the slim prompt.
    if is_coordinator {
        return shared;
    }

    // Ant-native builds alias find/grep to embedded bfs/ugrep and drop the
    // dedicated Glob/Grep tools, so point at find via Bash instead.
    let embedded = crate::utils::embedded_tools::has_embedded_search_tools();
    let file_search_hint = if embedded {
        "`find` via the Bash tool".to_string()
    } else {
        format!("the {GLOB_TOOL_NAME} tool")
    };
    let content_search_hint = if embedded {
        "`grep` via the Bash tool".to_string()
    } else {
        format!("the {GLOB_TOOL_NAME} tool")
    };
    let when_not_to_use_section = if fork_enabled {
        String::new()
    } else {
        format!(
            "\nWhen NOT to use the {AGENT_TOOL_NAME} tool:\n- If you want to read a specific file path, use the {FILE_READ_TOOL_NAME} tool or {file_search_hint} instead of the {AGENT_TOOL_NAME} tool, to find the match more quickly\n- If you are searching for a specific class definition like \"class Foo\", use {content_search_hint} instead, to find the match more quickly\n- If you are searching for code within a specific file or set of 2-3 files, use the {FILE_READ_TOOL_NAME} tool instead of the {AGENT_TOOL_NAME} tool, to find the match more quickly\n- Other tasks that are not related to the agent descriptions above\n"
        )
    };

    // When listing via attachment, the "launch multiple agents" note ships in
    // the attachment message (conditioned on subscription there).
    let concurrency_note = if !list_via_attachment
        && crate::utils::auth::get_subscription_type().as_deref() != Some("pro")
    {
        "\n- Launch multiple agents concurrently whenever possible, to maximize performance; to do that, use a single message with multiple tool uses"
    } else {
        ""
    };

    let background_notes = if !crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS")
            .as_deref(),
    ) && !fork_enabled
    {
        "\n- You can optionally run agents in the background using the run_in_background parameter. When an agent runs in the background, you will be automatically notified when it completes — do NOT sleep, poll, or proactively check on its progress. Continue with other work or respond to the user instead.\n- **Foreground vs background**: Use foreground (default) when you need the agent's results before you can proceed — e.g., research agents whose findings inform your next steps. Use background when you have genuinely independent work to do in parallel."
    } else {
        ""
    };

    let resume_note = if fork_enabled {
        "Each fresh Agent invocation with a subagent_type starts without context — provide a complete task description."
    } else {
        "Each Agent invocation starts fresh — provide a complete task description."
    };
    let research_note = if fork_enabled {
        ""
    } else {
        ", since it is not aware of the user's intent"
    };

    let remote_isolation_note = if crate::utils::build_profile::has_internal_capability(
        crate::utils::build_profile::InternalCapability::Prompts,
    ) {
        "\n- You can set `isolation: \"remote\"` to run the agent in a remote CCR environment. This is always a background task; you'll be notified when it completes. Use for long-running tasks that need a fresh sandbox."
    } else {
        ""
    };

    let teammate_note = if crate::utils::teammate::is_teammate() {
        "\n- The name, team_name, and mode parameters are not available in this context — teammates cannot spawn other teammates. Omit them to spawn a subagent."
    } else {
        ""
    };

    let examples = if fork_enabled {
        fork_examples()
    } else {
        current_examples()
    };

    format!(
        "{shared}\n{when_not_to_use_section}\n\nUsage notes:\n- Always include a short description (3-5 words) summarizing what the agent will do{concurrency_note}\n- When the agent is done, it will return a single message back to you. The result returned by the agent is not visible to the user. To show the user the result, you should send a text message back to the user with a concise summary of the result.{background_notes}\n- To continue a previously spawned agent, use {SEND_MESSAGE_TOOL_NAME} with the agent's ID or name as the `to` field. The agent resumes with its full context preserved. {resume_note}\n- The agent's outputs should generally be trusted\n- Clearly tell the agent whether you expect it to write code or just to do research (search, file reads, web fetches, etc.){research_note}\n- If the agent description mentions that it should be used proactively, then you should try your best to use it without the user having to ask for it first. Use your judgement.\n- If the user specifies that they want you to run agents \"in parallel\", you MUST send a single message with multiple {AGENT_TOOL_NAME} tool use content blocks. For example, if you need to launch both a build-validator agent and a test-runner agent in parallel, send a single message with both tool calls.\n- You can optionally set `isolation: \"worktree\"` to run the agent in a temporary git worktree, giving it an isolated copy of the repository. The worktree is automatically cleaned up if the agent makes no changes; if changes are made, the worktree path and branch are returned in the result.{remote_isolation_note}{teammate_note}{when_to_fork}{writing_the_prompt}\n\n{examples}",
        writing_the_prompt = writing_the_prompt_section(fork_enabled),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::agent_tool::load_agents_dir::AgentDefinitionSource;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    fn agent(agent_type: &str, when_to_use: &str) -> AgentDefinition {
        AgentDefinition::new(agent_type, when_to_use, AgentDefinitionSource::BuiltIn)
    }

    #[test]
    fn format_agent_line_matches_official_tool_description_branches() {
        let mut plain = agent("general-purpose", "Use for general tasks");
        assert_eq!(
            format_agent_line(&plain),
            "- general-purpose: Use for general tasks (Tools: All tools)"
        );

        plain.disallowed_tools = Some(vec!["Bash".to_string(), "Write".to_string()]);
        assert_eq!(
            format_agent_line(&plain),
            "- general-purpose: Use for general tasks (Tools: All tools except Bash, Write)"
        );

        plain.tools = Some(vec!["Read".to_string(), "Bash".to_string()]);
        assert_eq!(
            format_agent_line(&plain),
            "- general-purpose: Use for general tasks (Tools: Read)"
        );

        plain.tools = Some(vec!["Bash".to_string()]);
        assert_eq!(
            format_agent_line(&plain),
            "- general-purpose: Use for general tasks (Tools: None)"
        );
    }

    #[test]
    fn prompt_lists_available_agent_types_inline_when_attachment_gate_is_off() {
        let _guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _list_in_messages = EnvVarGuard::unset("CLAUDE_CODE_AGENT_LIST_IN_MESSAGES");

        let prompt = get_prompt(
            &[
                agent("general-purpose", "Use for general tasks"),
                agent("code-reviewer", "Use after writing code"),
            ],
            false,
            None,
        );

        assert!(prompt.contains("Available agent types and the tools they have access to:"));
        assert!(prompt.contains("- general-purpose: Use for general tasks (Tools: All tools)"));
        assert!(prompt.contains("- code-reviewer: Use after writing code (Tools: All tools)"));
    }

    #[test]
    fn prompt_defers_the_agent_list_to_attachments_when_the_gate_is_on() {
        let _guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let prompt = {
            let _list_in_messages = EnvVarGuard::set("CLAUDE_CODE_AGENT_LIST_IN_MESSAGES", "true");
            get_prompt(
                &[agent("general-purpose", "Use for general tasks")],
                false,
                None,
            )
        };

        assert!(prompt.contains(
            "Available agent types are listed in <system-reminder> messages in the conversation."
        ));
        assert!(!prompt.contains("- general-purpose:"));
        assert!(!prompt.contains("Launch multiple agents concurrently"));
    }

    #[test]
    fn allowed_agent_types_restrict_the_inline_listing_like_official() {
        let _guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _list_in_messages = EnvVarGuard::unset("CLAUDE_CODE_AGENT_LIST_IN_MESSAGES");

        let prompt = get_prompt(
            &[
                agent("general-purpose", "Use for general tasks"),
                agent("code-reviewer", "Use after writing code"),
            ],
            false,
            Some(&["code-reviewer".to_string()]),
        );

        assert!(prompt.contains("- code-reviewer:"));
        assert!(!prompt.contains("- general-purpose:"));
    }

    /// `prompt.ts:78` reads the fork gate once and five sections branch on it.
    /// `scripts/build.ts:45` ships it on, so this is the description the model
    /// actually receives.
    #[test]
    fn non_coordinator_prompt_carries_the_official_sections() {
        let _guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _list_in_messages = EnvVarGuard::unset("CLAUDE_CODE_AGENT_LIST_IN_MESSAGES");
        let _background = EnvVarGuard::unset("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS");
        let _fork_gate = crate::tools::agent_tool::fork_subagent::fork_gate_environment();

        let prompt = get_prompt(
            &[agent("general-purpose", "Use for general tasks")],
            false,
            None,
        );

        assert!(prompt.contains("\nUsage notes:\n"));
        assert!(prompt.contains("## Writing the prompt"));
        assert!(prompt.contains("Example usage:"));
        // `prompt.ts:80` and `:115` — the fork section and the fork examples.
        assert!(prompt.contains("## When to fork"));
        assert!(prompt.contains("Fork yourself (omit `subagent_type`)"));
        assert!(prompt.contains("name: \"ship-audit\","));
        // `prompt.ts:210` — the header sends a missing subagent_type to a fork.
        assert!(prompt.contains(
            "specify a subagent_type to use a specialized agent, or omit it to fork yourself"
        ));
        // `prompt.ts:99-113` — the two fresh-agent lead-ins the gate swaps in.
        assert!(prompt.contains(
            "When spawning a fresh agent (with a `subagent_type`), it starts with zero context."
        ));
        assert!(prompt.contains("For fresh agents, terse command-style prompts"));
        // `prompt.ts:232-233` — the when-NOT-to-use list goes away with the
        // gate on.
        assert!(!prompt.contains("When NOT to use the Agent tool:"));
        // `prompt.ts:258-266` — so does the background note, since `forceAsync`
        // (`AgentTool.tsx:812`) leaves nothing for run_in_background to do.
        assert!(!prompt.contains("run_in_background parameter"));
        // `prompt.ts:286` — fork examples REPLACE the current ones.
        assert!(!prompt.contains("<example_agent_descriptions>"));
        // Cometix invented this heading; the official prompt has no such section.
        assert!(!prompt.contains("## When to use the Agent tool"));
    }

    /// The other side of the same five branches, which CC serves to every
    /// session `forkSubagent.ts:34-35` vetoes. Both arms are live code in CC,
    /// so both stay under test.
    #[test]
    fn a_vetoed_fork_gate_restores_the_pre_fork_sections() {
        let _guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _list_in_messages = EnvVarGuard::unset("CLAUDE_CODE_AGENT_LIST_IN_MESSAGES");
        let _background = EnvVarGuard::unset("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS");
        let _fork_vetoed = crate::tools::agent_tool::fork_subagent::fork_veto_environment();

        let prompt = get_prompt(
            &[agent("general-purpose", "Use for general tasks")],
            false,
            None,
        );

        assert!(prompt.contains("When NOT to use the Agent tool:"));
        assert!(prompt.contains("\nUsage notes:\n"));
        assert!(prompt.contains("## Writing the prompt"));
        assert!(prompt.contains("Example usage:"));
        assert!(prompt.contains("<example_agent_descriptions>"));
        assert!(prompt.contains(
            "- You can optionally run agents in the background using the run_in_background parameter."
        ));
        assert!(prompt.contains(
            "specify a subagent_type parameter to select which agent type to use. If omitted, the general-purpose agent is used."
        ));
        assert!(!prompt.contains("## When to fork"));
        assert!(!prompt.contains("name: \"ship-audit\","));
        // Cometix invented this heading; the official prompt has no such section.
        assert!(!prompt.contains("## When to use the Agent tool"));
    }

    #[test]
    fn coordinator_prompt_stops_after_the_shared_header() {
        let _guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _list_in_messages = EnvVarGuard::unset("CLAUDE_CODE_AGENT_LIST_IN_MESSAGES");

        let prompt = get_prompt(
            &[agent("general-purpose", "Use for general tasks")],
            true,
            None,
        );

        assert!(
            prompt.starts_with(
                "Launch a new agent to handle complex, multi-step tasks autonomously."
            )
        );
        assert!(!prompt.contains("Usage notes:"));
        assert!(!prompt.contains("Example usage:"));
    }

    /// The fork veto is held on purpose: `prompt.ts:258-261` keeps the note
    /// only when `!disabled && !isInProcessTeammate() && !forkEnabled`, and
    /// with the fork leg live this test would pass on either side of the kill
    /// switch and prove nothing about it.
    #[test]
    fn background_notes_drop_when_background_tasks_are_disabled() {
        let _guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _list_in_messages = EnvVarGuard::unset("CLAUDE_CODE_AGENT_LIST_IN_MESSAGES");
        let _fork_vetoed = crate::tools::agent_tool::fork_subagent::fork_veto_environment();
        let prompt = {
            let _background = EnvVarGuard::set("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS", "1");
            get_prompt(
                &[agent("general-purpose", "Use for general tasks")],
                false,
                None,
            )
        };

        assert!(!prompt.contains("run_in_background parameter"));
        assert!(!prompt.contains("**Foreground vs background**"));
    }
}
