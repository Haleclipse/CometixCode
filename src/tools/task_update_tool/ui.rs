//! UI-only renderer contract for official `TaskUpdateTool`.

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TaskUpdateResultView {
    pub success: bool,
    pub task_id: String,
    pub updated_fields: Vec<String>,
    pub error: Option<String>,
    pub status_to: Option<String>,
    pub teammate_completion_nudge: bool,
    pub verification_nudge_needed: bool,
}

pub fn user_facing_name() -> &'static str {
    "TaskUpdate"
}

pub fn render_tool_use_message() -> Option<&'static str> {
    None
}

pub fn renders_success_result() -> bool {
    false
}

pub fn map_tool_result_content(result: &TaskUpdateResultView) -> String {
    if !result.success {
        return result
            .error
            .clone()
            .unwrap_or_else(|| format!("Task #{} not found", result.task_id));
    }

    let mut content = format!(
        "Updated task #{} {}",
        result.task_id,
        result.updated_fields.join(", ")
    );
    if result.status_to.as_deref() == Some("completed") && result.teammate_completion_nudge {
        content.push_str("\n\nTask completed. Call TaskList now to find your next available task or see if your work unblocked others.");
    }
    if result.verification_nudge_needed {
        // Maps to: CC `TaskUpdateTool.ts:396-398` — the verifier's agent type
        // is `review` in nyan (AgentTool/constants).
        content.push_str(&format!(
            "\n\nNOTE: You just closed out 3+ tasks and none of them was a verification step. Before writing your final summary, spawn the review agent (subagent_type=\"{}\"). You cannot self-assign PARTIAL by listing caveats in your summary — only the verifier issues a verdict.",
            crate::tools::agent_tool::constants::REVIEW_AGENT_TYPE
        ));
    }
    content
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_update_matches_official_null_renderer_and_content_mapping() {
        assert_eq!(user_facing_name(), "TaskUpdate");
        assert_eq!(render_tool_use_message(), None);
        assert!(!renders_success_result());
        assert_eq!(
            map_tool_result_content(&TaskUpdateResultView {
                success: false,
                task_id: "9".to_string(),
                ..TaskUpdateResultView::default()
            }),
            "Task #9 not found"
        );
        assert_eq!(
            map_tool_result_content(&TaskUpdateResultView {
                success: true,
                task_id: "9".to_string(),
                updated_fields: vec!["status".to_string(), "owner".to_string()],
                ..TaskUpdateResultView::default()
            }),
            "Updated task #9 status, owner"
        );
    }
}
