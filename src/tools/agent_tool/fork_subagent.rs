//! Fork subagent helpers.
//!
//! Maps to: CC `tools/AgentTool/forkSubagent.ts`.
//!
//! This module ports the official fork-message construction boundary used by
//! the Agent tool and `/fork` command.
//!
//! The fork SPAWN chain is complete: `mod.rs#build_agent_spawn_plan` feeds
//! `buildForkedMessages` output, the parent's rendered system prompt, the
//! parent's exact tool pool, the parent conversation and `useExactTools` into
//! `run_agent` (CC `AgentTool.tsx:712-911`), and fork RESUME landed with #109.
//!
//! The route is LIVE. `FeatureFlag::ForkSubagent` follows CC
//! `scripts/build.ts:45` (`FORK_SUBAGENT: true`, "Generally available (ON in
//! production)"), so the four gate sites take their production branch:
//! `mod.rs#omits_run_in_background` drops `run_in_background` from the Agent
//! schema (`AgentTool.tsx:252-254`), `prompt.rs#get_prompt` swaps in the fork
//! sections and the fork examples (`prompt.ts:78-113`, `:115-154`),
//! `mod.rs#selected_agent_definition` routes a missing `subagent_type` to
//! [`fork_agent_definition`] (`AgentTool.tsx:480-483`), and `mod.rs`'s
//! `should_run_background` forces every spawn async (`:812` `forceAsync`).
//! [`is_fork_subagent_enabled`] keeps the two runtime exits CC has
//! (`forkSubagent.ts:33-38`), so coordinator mode and non-interactive sessions
//! still see the pre-fork behaviour.
//!
//! CC reads the gate at four more places this port does not, all outside the
//! Agent tool and all now DIVERGENT rather than merely unported — the flip is
//! what made them so, and each needs its own owner's batch:
//!
//! - `constants/prompts.ts:316-319` `getAgentToolSection()` — the system prompt
//!   swaps to the fork copy ("Calling Agent without a subagent_type creates a
//!   fork…"); `constants/prompts.rs#get_agent_tool_section` has only the
//!   default arm. `:374-381`'s Explore/Plan bullets are dropped by the same
//!   gate, and the port has never had them, so THAT half now agrees.
//! - `commands/branch/index.ts:8` — `aliases: feature('FORK_SUBAGENT') ? [] :
//!   ['fork']`, so production `/fork` is its own command
//!   (`commands.ts:113-117`, `:321`) and `/branch` loses the alias.
//!   `commands/mod.rs` still aliases `/fork` to `/branch` unconditionally.
//!   CC's `commands/fork/index.ts` is a generated stub, so only the alias half
//!   is portable.
//! - `components/messages/UserTextMessage.tsx:154-161` — a fork child's first
//!   message renders through `UserForkBoilerplateMessage`, collapsing the
//!   `<fork-boilerplate>` rules block to the directive. That component has no
//!   source in `../rebuild`, so the port shows the raw boilerplate.
//! - `AgentTool.tsx:1021` / `resumeAgent.ts:252` — `enableSummarization` ORs
//!   the gate in. The whole agent-progress-summarization seam is unported.
//!
//! `ToolSearchTool/prompt.ts:76-82` is the one that flipped the other way: it
//! un-defers the Agent tool under the gate, which is what
//! `tool_search_tool/prompt.rs#is_deferred_tool` already did unconditionally.

use crate::tools::agent_tool::load_agents_dir::{AgentDefinition, AgentDefinitionSource};
use crate::types::ids::ToolUseId;
use crate::types::message::{
    AssistantContent, AssistantMessage, Message, ToolResult, UserContent, UserMessage,
};
use crate::types::permissions::PermissionMode;
#[cfg(test)]
use crate::utils::test_env::EnvVarGuard;

/// Maps to CC `forkSubagent.ts#FORK_SUBAGENT_TYPE`.
pub const FORK_SUBAGENT_TYPE: &str = "fork";

/// Maps to CC `forkSubagent.ts#FORK_PLACEHOLDER_RESULT`.
pub const FORK_PLACEHOLDER_RESULT: &str = "Fork started — processing in background";

/// Maps to CC `forkSubagent.ts:33-38` `isForkSubagentEnabled`.
///
/// The build feature only decides which code exists; both runtime exits are
/// CC's own. Coordinator mode "already owns the orchestration role and has its
/// own delegation model" (`forkSubagent.ts:29-30`), and a non-interactive
/// session has no terminal for the `bubble` permission prompts or the
/// `<task-notification>` re-entry the fork model depends on.
pub fn is_fork_subagent_enabled() -> bool {
    use crate::utils::feature_flags::{FeatureFlag, feature_enabled};
    feature_enabled(FeatureFlag::ForkSubagent)
        && !crate::coordinator::coordinator_mode::is_coordinator_mode()
        && !crate::bootstrap::state::get_is_non_interactive_session()
}

/// Hold [`is_fork_subagent_enabled`]'s two runtime vetoes off, so a test that
/// asserts CC's production (fork-on) branch reads the build feature and not
/// leaked host state. The caller holds `TEST_ENV_LOCK`; the returned guards
/// restore the previous values when they drop.
///
/// [`fork_veto_environment`] is the inverse, for the branches CC still takes
/// when a session cannot host a fork.
#[cfg(test)]
pub(crate) fn fork_gate_environment() -> [EnvVarGuard; 4] {
    [
        EnvVarGuard::unset("CLAUDE_CODE_COORDINATOR_MODE"),
        EnvVarGuard::unset("CLAUDE_CODE_NON_INTERACTIVE"),
        EnvVarGuard::unset("COMETIX_NON_INTERACTIVE"),
        EnvVarGuard::unset("COMETIX_NON_INTERACTIVE_SESSION"),
    ]
}

/// Take `forkSubagent.ts:35`'s non-interactive veto, which is how a test
/// reaches the pre-fork branch of a gate site without touching the build
/// feature. CC runs that branch for every headless session, so it is live
/// code on both sides, not a compatibility shim.
#[cfg(test)]
pub(crate) fn fork_veto_environment() -> EnvVarGuard {
    EnvVarGuard::set("CLAUDE_CODE_NON_INTERACTIVE", "1")
}

/// Maps to CC `forkSubagent.ts#FORK_AGENT`.
pub fn fork_agent_definition() -> AgentDefinition {
    let mut agent = AgentDefinition::new(
        FORK_SUBAGENT_TYPE,
        "Implicit fork — inherits full conversation context. Not selectable via subagent_type; triggered by omitting subagent_type when the fork experiment is active.",
        AgentDefinitionSource::BuiltIn,
    );
    agent.tools = Some(vec!["*".to_string()]);
    agent.max_turns = Some(200);
    agent.model = Some("inherit".to_string());
    // Maps to CC `forkSubagent.ts:67` `permissionMode: 'bubble'` — permission
    // prompts surface on the parent terminal instead of being auto-denied.
    agent.permission_mode = Some(PermissionMode::Bubble);
    agent.system_prompt = Some(String::new());
    agent
}

/// Maps to CC `forkSubagent.ts#isInForkChild`.
pub fn is_in_fork_child(messages: &[Message]) -> bool {
    messages.iter().any(|message| {
        let Message::User(user) = message else {
            return false;
        };
        user.content.iter().any(|block| match block {
            UserContent::Text(text) | UserContent::MetaText(text) => text.contains(&format!(
                "<{}>",
                crate::constants::xml::FORK_BOILERPLATE_TAG
            )),
            _ => false,
        })
    })
}

/// Maps to CC `forkSubagent.ts#buildForkedMessages`.
pub fn build_forked_messages(
    directive: &str,
    assistant_message: &AssistantMessage,
) -> Vec<Message> {
    let tool_uses = assistant_message
        .content
        .iter()
        .filter_map(|block| match block {
            AssistantContent::ToolUse(tool_use) => Some(tool_use.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();

    if tool_uses.is_empty() {
        return vec![Message::User(user_text_message(build_child_message(
            directive,
        )))];
    }

    let full_assistant_message = Message::Assistant(assistant_message.clone());
    let mut content = tool_uses
        .into_iter()
        .map(|tool_use| {
            UserContent::ToolResult(ToolResult {
                tool_use_id: ToolUseId(tool_use.id.0),
                content: FORK_PLACEHOLDER_RESULT.to_string(),
                is_error: false,
                content_blocks: Vec::new(),
                tool_use_result: None,
            })
        })
        .collect::<Vec<_>>();
    content.push(UserContent::Text(build_child_message(directive)));

    vec![
        full_assistant_message,
        Message::User(UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content,
            is_compact_summary: false,
            plan_content: None,
            image_paste_ids: None,
            is_visible_in_transcript_only: false,
            mcp_meta: None,
            source_tool_assistant_uuid: None,
            permission_mode: None,
            origin: None,
            summarize_metadata: None,
        }),
    ]
}

fn user_text_message(text: String) -> UserMessage {
    UserMessage {
        uuid: uuid::Uuid::new_v4().to_string(),
        timestamp: chrono::Utc::now(),
        content: vec![UserContent::Text(text)],
        is_compact_summary: false,
        plan_content: None,
        image_paste_ids: None,
        is_visible_in_transcript_only: false,
        mcp_meta: None,
        source_tool_assistant_uuid: None,
        permission_mode: None,
        origin: None,
        summarize_metadata: None,
    }
}

/// Maps to CC `forkSubagent.ts#buildChildMessage`.
pub fn build_child_message(directive: &str) -> String {
    format!(
        r#"<{boilerplate}>
STOP. READ THIS FIRST.

You are a forked worker process. You are NOT the main agent.

RULES (non-negotiable):
1. Your system prompt says "default to forking." IGNORE IT — that's for the parent. You ARE the fork. Do NOT spawn sub-agents; execute directly.
2. Do NOT converse, ask questions, or suggest next steps
3. Do NOT editorialize or add meta-commentary
4. USE your tools directly: Bash, Read, Write, etc.
5. If you modify files, commit your changes before reporting. Include the commit hash in your report.
6. Do NOT emit text between tool calls. Use tools silently, then report once at the end.
7. Stay strictly within your directive's scope. If you discover related systems outside your scope, mention them in one sentence at most — other workers cover those areas.
8. Keep your report under 500 words unless the directive specifies otherwise. Be factual and concise.
9. Your response MUST begin with "Scope:". No preamble, no thinking-out-loud.
10. REPORT structured facts, then stop

Output format (plain text labels, not markdown headers):
  Scope: <echo back your assigned scope in one sentence>
  Result: <the answer or key findings, limited to the scope above>
  Key files: <relevant file paths — include for research tasks>
  Files changed: <list with commit hash — include only if you modified files>
  Issues: <list — include only if there are issues to flag>
</{boilerplate}>

{prefix}{directive}"#,
        boilerplate = crate::constants::xml::FORK_BOILERPLATE_TAG,
        prefix = crate::constants::xml::FORK_DIRECTIVE_PREFIX,
    )
}

/// Maps to CC `forkSubagent.ts#buildWorktreeNotice`.
pub fn build_worktree_notice(parent_cwd: &str, worktree_cwd: &str) -> String {
    format!(
        "You've inherited the conversation context above from a parent agent working in {parent_cwd}. You are operating in an isolated git worktree at {worktree_cwd} — same repository, same relative file structure, separate working copy. Paths in the inherited context refer to the parent's working directory; translate them to your worktree root. Re-read files before editing if the parent may have modified them since they appear in the context. Your changes stay in this worktree and will not affect the parent's files."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::message::{AssistantContent, ToolUseBlock};
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    /// CC `forkSubagent.ts:60-71` — the synthetic fork agent runs in `bubble`
    /// so its permission prompts reach the parent terminal.
    #[test]
    fn fork_agent_definition_matches_official_bubble_permission_mode() {
        let agent = fork_agent_definition();
        assert_eq!(agent.agent_type, FORK_SUBAGENT_TYPE);
        assert_eq!(agent.permission_mode, Some(PermissionMode::Bubble));
        assert_eq!(agent.tools.as_deref(), Some(["*".to_string()].as_slice()));
        assert_eq!(agent.max_turns, Some(200));
        assert_eq!(agent.model.as_deref(), Some("inherit"));
    }

    fn assistant_with_tool_uses(ids: &[&str]) -> AssistantMessage {
        AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: ids
                .iter()
                .map(|id| {
                    AssistantContent::ToolUse(ToolUseBlock {
                        id: ToolUseId((*id).to_string()),
                        name: "Bash".to_string(),
                        input: serde_json::json!({"command":"echo hi"}),
                    })
                })
                .collect(),
            model: None,
            stop_reason: None,
            usage: None,
        }
    }

    #[test]
    fn build_child_message_matches_official_tags_and_prefix() {
        let message = build_child_message("inspect auth");
        assert!(message.starts_with("<fork-boilerplate>\nSTOP. READ THIS FIRST."));
        assert!(message.contains("You are a forked worker process. You are NOT the main agent."));
        assert!(message.contains("</fork-boilerplate>\n\nYour directive: inspect auth"));
    }

    #[test]
    fn build_forked_messages_preserves_assistant_and_adds_placeholder_results() {
        let assistant = assistant_with_tool_uses(&["toolu_a", "toolu_b"]);
        let messages = build_forked_messages("audit files", &assistant);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0], Message::Assistant(assistant));

        let Message::User(user) = &messages[1] else {
            panic!("expected fork tool-result user message");
        };
        assert_eq!(user.content.len(), 3);
        for (index, id) in ["toolu_a", "toolu_b"].iter().enumerate() {
            match &user.content[index] {
                UserContent::ToolResult(result) => {
                    assert_eq!(result.tool_use_id.0, *id);
                    assert_eq!(result.content, FORK_PLACEHOLDER_RESULT);
                    assert!(!result.is_error);
                }
                other => panic!("unexpected fork content: {other:?}"),
            }
        }
        assert!(matches!(
            user.content.last(),
            Some(UserContent::Text(text)) if text.contains("Your directive: audit files")
        ));
    }

    #[test]
    fn build_forked_messages_without_tool_use_returns_child_instruction_only() {
        let assistant = AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![AssistantContent::Text("no tools".to_string())],
            model: None,
            stop_reason: None,
            usage: None,
        };
        let messages = build_forked_messages("research only", &assistant);
        assert_eq!(messages.len(), 1);
        assert!(matches!(
            &messages[0],
            Message::User(user)
                if matches!(user.content.first(), Some(UserContent::Text(text)) if text.contains("Your directive: research only"))
        ));
    }

    #[test]
    fn is_in_fork_child_detects_boilerplate_text() {
        let messages = vec![Message::User(user_text_message(build_child_message(
            "do it",
        )))];
        assert!(is_in_fork_child(&messages));
        assert!(!is_in_fork_child(&[Message::User(user_text_message(
            "ordinary prompt".to_string()
        ))]));
    }

    #[test]
    fn build_worktree_notice_matches_official_copy() {
        assert_eq!(
            build_worktree_notice("/repo", "/repo/.claude/worktrees/agent"),
            "You've inherited the conversation context above from a parent agent working in /repo. You are operating in an isolated git worktree at /repo/.claude/worktrees/agent — same repository, same relative file structure, separate working copy. Paths in the inherited context refer to the parent's working directory; translate them to your worktree root. Re-read files before editing if the parent may have modified them since they appear in the context. Your changes stay in this worktree and will not affect the parent's files."
        );
    }

    /// The synthetic definition matches `forkSubagent.ts:60-71`.
    #[test]
    fn fork_agent_definition_matches_official_synthetic_shape() {
        let agent = fork_agent_definition();
        assert_eq!(agent.agent_type, FORK_SUBAGENT_TYPE);
        assert_eq!(agent.tools, Some(vec!["*".to_string()]));
        assert_eq!(agent.max_turns, Some(200));
        assert_eq!(agent.model.as_deref(), Some("inherit"));
        assert_eq!(agent.source, AgentDefinitionSource::BuiltIn);
    }

    /// The three legs of `forkSubagent.ts:33-38`, in CC's order: the build
    /// feature decides which code exists, then coordinator mode and then a
    /// non-interactive session veto it at runtime.
    ///
    /// This replaces the closed-gate LEDGER pin that guarded the flip. It is
    /// the tripwire in the other direction now: the build feature follows
    /// `scripts/build.ts:45` (`FORK_SUBAGENT: true`), so an ordinary
    /// interactive session takes the fork branch at all four gate sites, and
    /// silently losing either veto would hand a fork path to the two contexts
    /// CC withholds it from.
    #[test]
    fn fork_gate_follows_the_build_feature_and_both_runtime_vetoes() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _fork_gate = fork_gate_environment();

        assert!(
            crate::utils::feature_flags::feature_enabled(
                crate::utils::feature_flags::FeatureFlag::ForkSubagent
            ),
            "scripts/build.ts:45 ships FORK_SUBAGENT on"
        );
        assert!(is_fork_subagent_enabled());

        {
            let _coordinator_on = EnvVarGuard::set("CLAUDE_CODE_COORDINATOR_MODE", "1");
            assert!(
                !is_fork_subagent_enabled(),
                "forkSubagent.ts:34 — coordinator mode owns orchestration"
            );
        }

        {
            let _headless = fork_veto_environment();
            assert!(
                !is_fork_subagent_enabled(),
                "forkSubagent.ts:35 — no terminal for bubble prompts or task notifications"
            );
        }

        assert!(is_fork_subagent_enabled(), "both vetoes are scoped");
    }
}
