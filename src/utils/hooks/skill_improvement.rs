//! Skill improvement post-sampling helpers.
//! Maps to: CC `utils/hooks/skillImprovement.ts`.
//!
//! This module ports the pure decision/prompt/parsing portions of the official
//! skill-improvement hook. The production side-channel LLM registration/apply
//! path remains gated until Skill runtime and safe API hook orchestration are
//! fully wired; no hook is registered and no network call is made by this file.

use crate::constants::query_source::QuerySource;
use crate::types::message::{AssistantContent, Message, UserContent};
use crate::utils::messages::extract_tag;
use serde::{Deserialize, Serialize};

/// Maps to: CC `TURN_BATCH_SIZE`.
pub const TURN_BATCH_SIZE: usize = 5;

/// Maps to: CC `SkillUpdate`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillUpdate {
    pub section: String,
    pub change: String,
    pub reason: String,
}

/// Minimal invoked-skill projection used by the helper.
/// Maps to entries returned by CC `getInvokedSkillsForAgent(null)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvokedSkillInfo {
    pub skill_name: String,
    pub skill_path: String,
    pub content: String,
}

/// Stateful gate used by CC `createSkillImprovementHook()`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SkillImprovementState {
    last_analyzed_count: usize,
    last_analyzed_index: usize,
}

impl SkillImprovementState {
    /// Maps to the `shouldRun(context)` closure inside
    /// CC `createSkillImprovementHook()`.
    pub fn should_run(
        &mut self,
        query_source: Option<&QuerySource>,
        messages: &[Message],
        project_skill: Option<&InvokedSkillInfo>,
    ) -> bool {
        if query_source.map(QuerySource::as_api_source) != Some("repl_main_thread") {
            return false;
        }
        if project_skill.is_none() {
            return false;
        }
        let user_count = messages
            .iter()
            .filter(|message| matches!(message, Message::User(_)))
            .count();
        if user_count.saturating_sub(self.last_analyzed_count) < TURN_BATCH_SIZE {
            return false;
        }
        self.last_analyzed_count = user_count;
        true
    }

    /// Maps to `const newMessages = context.messages.slice(lastAnalyzedIndex)`
    /// and index update inside CC `buildMessages(context)`.
    pub fn take_new_messages<'a>(&mut self, messages: &'a [Message]) -> &'a [Message] {
        let start = self.last_analyzed_index.min(messages.len());
        self.last_analyzed_index = messages.len();
        &messages[start..]
    }
}

/// Maps to: CC local `formatRecentMessages(messages)`.
pub fn format_recent_messages(messages: &[Message]) -> String {
    messages
        .iter()
        .filter_map(|message| match message {
            Message::User(user) => Some(("User", user_text_content(&user.content))),
            Message::Assistant(assistant) => {
                Some(("Assistant", assistant_text_content(&assistant.content)))
            }
            _ => None,
        })
        .map(|(role, content)| {
            let truncated = content.chars().take(500).collect::<String>();
            format!("{role}: {truncated}")
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn user_text_content(content: &[UserContent]) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            UserContent::Text(text) | UserContent::MetaText(text) => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn assistant_text_content(content: &[AssistantContent]) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            AssistantContent::Text(text) => Some(text.as_str()),
            AssistantContent::Advisor { content, .. } => content.text(),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Maps to: CC local `findProjectSkill()` path predicate.
pub fn find_project_skill(skills: &[InvokedSkillInfo]) -> Option<InvokedSkillInfo> {
    skills
        .iter()
        .find(|skill| skill.skill_path.starts_with("projectSettings:"))
        .cloned()
}

/// Maps to the user message built by CC `buildMessages(context)`.
pub fn build_skill_improvement_prompt(
    project_skill: &InvokedSkillInfo,
    recent_messages: &[Message],
) -> String {
    format!(
        "You are analyzing a conversation where a user is executing a skill (a repeatable process).\n\
Your job: identify if the user's recent messages contain preferences, requests, or corrections that should be permanently added to the skill definition for future runs.\n\n\
<skill_definition>\n{}\n</skill_definition>\n\n\
<recent_messages>\n{}\n</recent_messages>\n\n\
Look for:\n\
- Requests to add, change, or remove steps: \"can you also ask me X\", \"please do Y too\", \"don't do Z\"\n\
- Preferences about how steps should work: \"ask me about energy levels\", \"note the time\", \"use a casual tone\"\n\
- Corrections: \"no, do X instead\", \"always use Y\", \"make sure to...\"\n\n\
Ignore:\n\
- Routine conversation that doesn't generalize (one-time answers, chitchat)\n\
- Things the skill already does\n\n\
Output a JSON array inside <updates> tags. Each item: {{\"section\": \"which step/section to modify or 'new step'\", \"change\": \"what to add/modify\", \"reason\": \"which user message prompted this\"}}.\n\
Output <updates>[]</updates> if no updates are needed.",
        project_skill.content,
        format_recent_messages(recent_messages)
    )
}

/// Maps to: CC `parseResponse(content)` for skill improvement detection.
pub fn parse_skill_updates_response(content: &str) -> Vec<SkillUpdate> {
    let Some(updates) = extract_tag(content, "updates") else {
        return Vec::new();
    };
    serde_json::from_str::<Vec<SkillUpdate>>(&updates).unwrap_or_default()
}

/// Maps to: CC `applySkillImprovement(...)` file path construction.
pub fn skill_improvement_file_path(cwd: &std::path::Path, skill_name: &str) -> std::path::PathBuf {
    cwd.join(".claude")
        .join("skills")
        .join(skill_name)
        .join("SKILL.md")
}

/// Maps to the user message used by CC `applySkillImprovement(...)`.
pub fn build_apply_skill_improvement_prompt(
    current_content: &str,
    updates: &[SkillUpdate],
) -> String {
    let update_list = updates
        .iter()
        .map(|update| format!("- {}: {}", update.section, update.change))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        "You are editing a skill definition file. Apply the following improvements to the skill.\n\n\
<current_skill_file>\n{current_content}\n</current_skill_file>\n\n\
<improvements>\n{update_list}\n</improvements>\n\n\
Rules:\n\
- Integrate the improvements naturally into the existing structure\n\
- Preserve frontmatter (--- block) exactly as-is\n\
- Preserve the overall format and style\n\
- Do not remove existing content unless an improvement explicitly replaces it\n\
- Output the complete updated file inside <updated_file> tags"
    )
}

/// Safe file-write helper for the apply phase after a caller supplies an
/// already-generated updated file. This does not perform the side-channel LLM
/// query from CC `applySkillImprovement(...)`; that network path is deferred to
/// the Skill runtime/API-hook integration slice.
pub fn apply_skill_improvement_updated_file(
    cwd: &std::path::Path,
    skill_name: &str,
    response_text: &str,
) -> std::io::Result<Option<std::path::PathBuf>> {
    if skill_name.is_empty() {
        return Ok(None);
    }
    let Some(updated_content) = extract_tag(response_text, "updated_file") else {
        return Ok(None);
    };
    let path = skill_improvement_file_path(cwd, skill_name);
    std::fs::write(&path, updated_content)?;
    Ok(Some(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::message::{AssistantMessage, UserMessage};
    use chrono::Utc;

    fn user(text: &str) -> Message {
        Message::User(UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            content: vec![UserContent::Text(text.to_string())],
            is_compact_summary: false,
            plan_content: None,
            image_paste_ids: None,
            is_visible_in_transcript_only: false,
            mcp_meta: None,
            source_tool_assistant_uuid: None,
            permission_mode: None,
            origin: None,
            summarize_metadata: None,
        })
    }

    fn assistant(text: &str) -> Message {
        Message::Assistant(AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            content: vec![AssistantContent::Text(text.to_string())],
            model: None,
            stop_reason: None,
            usage: None,
        })
    }

    #[test]
    fn format_recent_messages_keeps_user_assistant_and_truncates_like_official() {
        let long = "x".repeat(600);
        let formatted = format_recent_messages(&[
            user(&long),
            assistant("done"),
            Message::System(crate::types::message::SystemMessage::informational(
                "ignored",
                crate::types::message::SystemMessageLevel::Info,
            )),
        ]);
        assert!(formatted.starts_with(&format!("User: {}", "x".repeat(500))));
        assert!(formatted.contains("\n\nAssistant: done"));
        assert!(!formatted.contains("ignored"));
    }

    #[test]
    fn project_skill_detection_matches_official_path_prefix() {
        let skills = vec![
            InvokedSkillInfo {
                skill_name: "global".to_string(),
                skill_path: "userSettings:/global".to_string(),
                content: "global".to_string(),
            },
            InvokedSkillInfo {
                skill_name: "project".to_string(),
                skill_path: "projectSettings:/repo/.claude/skills/project".to_string(),
                content: "project".to_string(),
            },
        ];
        assert_eq!(find_project_skill(&skills).unwrap().skill_name, "project");
    }

    #[test]
    fn state_should_run_every_five_user_messages_for_repl_project_skill() {
        let project_skill = InvokedSkillInfo {
            skill_name: "project".to_string(),
            skill_path: "projectSettings:/repo/skill".to_string(),
            content: "skill".to_string(),
        };
        let mut state = SkillImprovementState::default();
        let messages = vec![user("1"), user("2"), user("3"), user("4")];
        assert!(!state.should_run(Some(&QuerySource::Prompt), &messages, Some(&project_skill)));
        let messages = vec![user("1"), user("2"), user("3"), user("4"), user("5")];
        assert!(state.should_run(Some(&QuerySource::Prompt), &messages, Some(&project_skill)));
        assert!(!state.should_run(Some(&QuerySource::Prompt), &messages, Some(&project_skill)));
        assert!(!state.should_run(Some(&QuerySource::Compact), &messages, Some(&project_skill)));
        assert!(!state.should_run(Some(&QuerySource::Prompt), &messages, None));
    }

    #[test]
    fn prompt_and_parse_response_match_official_tags() {
        let project_skill = InvokedSkillInfo {
            skill_name: "daily".to_string(),
            skill_path: "projectSettings:/repo/daily".to_string(),
            content: "## Steps\nAsk questions.".to_string(),
        };
        let prompt = build_skill_improvement_prompt(&project_skill, &[user("please ask energy")]);
        assert!(prompt.contains("<skill_definition>\n## Steps"));
        assert!(prompt.contains("<recent_messages>\nUser: please ask energy"));
        let updates = parse_skill_updates_response(
            r#"thinking<updates>[{"section":"new step","change":"Ask energy","reason":"please ask energy"}]</updates>"#,
        );
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].change, "Ask energy");
        assert!(parse_skill_updates_response("no tags").is_empty());
        assert!(parse_skill_updates_response("<updates>not json</updates>").is_empty());
    }

    #[test]
    fn build_apply_prompt_and_file_path_match_official_apply_shape() {
        let updates = vec![SkillUpdate {
            section: "new step".to_string(),
            change: "Ask energy".to_string(),
            reason: "user asked".to_string(),
        }];
        let prompt = build_apply_skill_improvement_prompt("---\nname: daily\n---", &updates);
        assert!(prompt.contains("<current_skill_file>\n---\nname: daily"));
        assert!(prompt.contains("<improvements>\n- new step: Ask energy"));
        assert_eq!(
            skill_improvement_file_path(std::path::Path::new("/repo"), "daily"),
            std::path::Path::new("/repo/.claude/skills/daily/SKILL.md")
        );
    }

    #[test]
    fn safe_apply_writes_only_supplied_updated_file_tag() {
        let dir =
            std::env::temp_dir().join(format!("nyan-skill-improvement-{}", uuid::Uuid::new_v4()));
        let skill_dir = dir.join(".claude/skills/daily");
        std::fs::create_dir_all(&skill_dir).unwrap();
        let path = apply_skill_improvement_updated_file(
            &dir,
            "daily",
            "<updated_file>new skill content</updated_file>",
        )
        .unwrap()
        .expect("updated path");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new skill content");
        assert!(
            apply_skill_improvement_updated_file(&dir, "daily", "no tag")
                .unwrap()
                .is_none()
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
