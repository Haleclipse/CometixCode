//! Main-screen-safe subset of official `ExitPlanModeTool/UI.tsx`.

use super::Output;
use crate::components::messages::user_tool_result_message::utils::{
    ToolRenderLine, ToolRenderTone,
};
use crate::constants::figures::BLACK_CIRCLE;

/// The Rust stand-in for CC's `outputSchema.safeParse(toolUseResult)`
/// (`UserToolSuccessMessage.tsx:80`): `plan` is required but nullable,
/// `isAgent` required, the other five optional
/// (`ExitPlanModeV2Tool.ts:110-142`).
pub(crate) fn parse_output(value: &serde_json::Value) -> Option<Output> {
    let map = value.as_object()?;
    let optional_string = |key: &str| -> Option<Option<String>> {
        match map.get(key) {
            None => Some(None),
            Some(serde_json::Value::String(value)) => Some(Some(value.clone())),
            Some(_) => None,
        }
    };
    let optional_bool = |key: &str| -> Option<Option<bool>> {
        match map.get(key) {
            None => Some(None),
            Some(serde_json::Value::Bool(value)) => Some(Some(*value)),
            Some(_) => None,
        }
    };
    let plan = match map.get("plan")? {
        serde_json::Value::String(plan) => Some(plan.clone()),
        serde_json::Value::Null => None,
        _ => return None,
    };
    Some(Output {
        plan,
        is_agent: map.get("isAgent")?.as_bool()?,
        file_path: optional_string("filePath")?,
        has_task_tool: optional_bool("hasTaskTool")?,
        plan_was_edited: optional_bool("planWasEdited")?,
        awaiting_leader_approval: optional_bool("awaitingLeaderApproval")?,
        request_id: optional_string("requestId")?,
    })
}

/// Serializes [`Output`] to CC's exact `toolUseResult` wire shape — `plan`
/// is nullable-required and always written (null when absent), `isAgent`
/// always written, optionals omitted.
pub(crate) fn output_to_value(output: &Output) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    map.insert(
        "plan".to_string(),
        match output.plan.as_ref() {
            Some(plan) => serde_json::Value::String(plan.clone()),
            None => serde_json::Value::Null,
        },
    );
    map.insert(
        "isAgent".to_string(),
        serde_json::Value::Bool(output.is_agent),
    );
    if let Some(file_path) = output.file_path.as_ref() {
        map.insert(
            "filePath".to_string(),
            serde_json::Value::String(file_path.clone()),
        );
    }
    if let Some(has_task_tool) = output.has_task_tool {
        map.insert(
            "hasTaskTool".to_string(),
            serde_json::Value::Bool(has_task_tool),
        );
    }
    if let Some(plan_was_edited) = output.plan_was_edited {
        map.insert(
            "planWasEdited".to_string(),
            serde_json::Value::Bool(plan_was_edited),
        );
    }
    if let Some(awaiting) = output.awaiting_leader_approval {
        map.insert(
            "awaitingLeaderApproval".to_string(),
            serde_json::Value::Bool(awaiting),
        );
    }
    if let Some(request_id) = output.request_id.as_ref() {
        map.insert(
            "requestId".to_string(),
            serde_json::Value::String(request_id.clone()),
        );
    }
    serde_json::Value::Object(map)
}

/// Maps to: CC `ExitPlanModeTool/UI.tsx:19-75` `renderToolResultMessage` —
/// the three branches (empty plan, awaiting leader approval, approved).
pub(crate) fn render_tool_result_message(
    raw_output: Option<&serde_json::Value>,
) -> Vec<ToolRenderLine> {
    let Some(output) = raw_output.and_then(parse_output) else {
        return Vec::new();
    };
    render_result_lines(
        output.plan.as_deref(),
        output.file_path.as_deref(),
        output.awaiting_leader_approval.unwrap_or(false),
    )
}

pub fn render_result_lines(
    plan: Option<&str>,
    file_path: Option<&str>,
    awaiting_leader_approval: bool,
) -> Vec<ToolRenderLine> {
    let is_empty_plan = plan.is_none_or(|plan| plan.trim().is_empty());
    if is_empty_plan {
        return vec![ToolRenderLine::new(
            format!("{BLACK_CIRCLE} Exited plan mode"),
            ToolRenderTone::Normal,
        )];
    }

    if awaiting_leader_approval {
        let mut lines = vec![ToolRenderLine::new(
            format!("{BLACK_CIRCLE} Plan submitted for team lead approval"),
            ToolRenderTone::Normal,
        )];
        // CC `{filePath && …}` (UI.tsx:51) — JS falsy: only "" skips.
        if let Some(display_path) = file_path
            .filter(|path| !path.is_empty())
            .map(crate::utils::file::get_display_path)
        {
            lines.push(ToolRenderLine::new(
                format!("Plan file: {display_path}"),
                ToolRenderTone::Inactive,
            ));
        }
        lines.push(ToolRenderLine::new(
            "Waiting for team lead to review and approve...",
            ToolRenderTone::Inactive,
        ));
        return lines;
    }

    let mut lines = vec![ToolRenderLine::new(
        format!("{BLACK_CIRCLE} User approved Claude's plan"),
        ToolRenderTone::Normal,
    )];
    // CC `{filePath && …}` — JS falsy: only the empty string skips the line.
    if let Some(display_path) = file_path
        .filter(|path| !path.is_empty())
        .map(crate::utils::file::get_display_path)
    {
        lines.push(ToolRenderLine::new(
            format!("Plan saved to: {display_path} · /plan to edit"),
            ToolRenderTone::Inactive,
        ));
    }
    if let Some(plan) = plan.filter(|plan| !plan.trim().is_empty()) {
        lines.extend(
            plan.lines()
                .map(|line| ToolRenderLine::new(line, ToolRenderTone::Normal)),
        );
    }
    lines
}

/// Maps to: CC `RejectedPlanMessage.tsx` — the plan renders verbatim; the
/// `plan ?? getPlan() ?? 'No plan found'` fallback belongs to the reject
/// leaf entry (`ExitPlanModeTool/UI.tsx:81`), not this component.
pub fn render_rejected_result_lines(plan: &str) -> Vec<ToolRenderLine> {
    let mut lines = vec![ToolRenderLine::new(
        "User rejected Claude's plan:",
        ToolRenderTone::Inactive,
    )];
    lines.extend(
        plan.lines()
            .map(|line| ToolRenderLine::new(line, ToolRenderTone::Normal)),
    );
    lines
}
