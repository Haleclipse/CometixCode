//! UI-only port of official `SkillTool/UI.tsx`.

use super::Output;
use crate::components::messages::user_tool_result_message::utils::{
    ToolRenderLine, ToolRenderTone,
};

pub fn render_tool_use_message(
    skill: Option<&str>,
    loaded_from_legacy_commands: bool,
) -> Option<String> {
    let skill = skill.map(str::trim).filter(|value| !value.is_empty())?;
    if loaded_from_legacy_commands && !skill.starts_with('/') {
        Some(format!("/{skill}"))
    } else {
        Some(skill.to_string())
    }
}

/// The Rust stand-in for CC's `outputSchema.safeParse(toolUseResult)`
/// (`UserToolSuccessMessage.tsx:80`): a union tried left to right — inline
/// `{success, commandName, allowedTools?, status?: 'inline'}` first,
/// then forked `{success, commandName, status: 'forked', agentId, result}`
/// (`SkillTool.ts:301-326`).
pub(crate) fn parse_output(value: &serde_json::Value) -> Option<Output> {
    let map = value.as_object()?;
    let success = map.get("success")?.as_bool()?;
    let command_name = map.get("commandName")?.as_str()?.to_string();

    let inline = || -> Option<Output> {
        let allowed_tools = match map.get("allowedTools") {
            None => None,
            Some(serde_json::Value::Array(items)) => Some(
                items
                    .iter()
                    .map(|item| item.as_str().map(ToOwned::to_owned))
                    .collect::<Option<Vec<_>>>()?,
            ),
            Some(_) => return None,
        };
        let status = match map.get("status") {
            None => None,
            Some(serde_json::Value::String(status)) if status == "inline" => Some(status.clone()),
            Some(_) => return None,
        };
        Some(Output::Inline {
            success,
            command_name: command_name.clone(),
            allowed_tools,
            status,
            // Not part of CC's outputSchema — the recorded `toolUseResult`
            // never carries the `contextModifier` captures back.
            context_modifier: None,
        })
    };
    let forked = || -> Option<Output> {
        if map.get("status")?.as_str()? != "forked" {
            return None;
        }
        Some(Output::Forked {
            success,
            command_name: command_name.clone(),
            agent_id: map.get("agentId")?.as_str()?.to_string(),
            result: map.get("result")?.as_str()?.to_string(),
        })
    };
    inline().or_else(forked)
}

/// Serializes [`Output`] to CC's exact `toolUseResult` wire shape — each
/// branch follows its `call()` construction (`SkillTool.ts:277-283`,
/// `:768-773`, `:1102`), optionals omitted.
pub(crate) fn output_to_value(output: &Output) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    match output {
        Output::Inline {
            success,
            command_name,
            allowed_tools,
            status,
            // Rust-only `contextModifier` transport; CC's `toolUseResult` is
            // the `data` object alone, so it is deliberately not serialized.
            context_modifier: _,
        } => {
            map.insert("success".to_string(), serde_json::Value::Bool(*success));
            map.insert(
                "commandName".to_string(),
                serde_json::Value::String(command_name.clone()),
            );
            if let Some(allowed_tools) = allowed_tools {
                map.insert(
                    "allowedTools".to_string(),
                    serde_json::Value::Array(
                        allowed_tools
                            .iter()
                            .map(|tool| serde_json::Value::String(tool.clone()))
                            .collect(),
                    ),
                );
            }
            if let Some(status) = status {
                map.insert(
                    "status".to_string(),
                    serde_json::Value::String(status.clone()),
                );
            }
        }
        Output::Forked {
            success,
            command_name,
            agent_id,
            result,
        } => {
            map.insert("success".to_string(), serde_json::Value::Bool(*success));
            map.insert(
                "commandName".to_string(),
                serde_json::Value::String(command_name.clone()),
            );
            map.insert(
                "status".to_string(),
                serde_json::Value::String("forked".to_string()),
            );
            map.insert(
                "agentId".to_string(),
                serde_json::Value::String(agent_id.clone()),
            );
            map.insert(
                "result".to_string(),
                serde_json::Value::String(result.clone()),
            );
        }
    }
    serde_json::Value::Object(map)
}

/// Maps to: CC `SkillTool/UI.tsx:23-59` `renderToolResultMessage` — forked
/// renders the "Done" byline; inline joins "Successfully loaded skill" and
/// the tool count with " · ". @nyan: CC also appends the skill's `model:`;
/// skills no longer choose a model (see `SkillContextModifier`).
pub(crate) fn render_tool_result_message(
    raw_output: Option<&serde_json::Value>,
) -> Vec<ToolRenderLine> {
    let Some(output) = raw_output.and_then(parse_output) else {
        return Vec::new();
    };
    match output {
        Output::Forked { .. } => vec![ToolRenderLine::new("Done", ToolRenderTone::Normal)],
        Output::Inline { allowed_tools, .. } => {
            let mut parts = vec!["Successfully loaded skill".to_string()];
            let count = allowed_tools.as_deref().map_or(0, <[_]>::len);
            if count > 0 {
                let noun = if count == 1 { "tool" } else { "tools" };
                parts.push(format!("{count} {noun} allowed"));
            }
            vec![ToolRenderLine::new(
                parts.join(" · "),
                ToolRenderTone::Normal,
            )]
        }
    }
}

pub(crate) fn skill_tool_use_summary(input: &serde_json::Value) -> Option<String> {
    let skill = crate::components::messages::user_tool_result_message::utils::first_string(
        input,
        &["skill"],
    );
    let loaded_from_legacy_commands = input
        .get("loadedFrom")
        .or_else(|| input.get("loaded_from"))
        .and_then(|value| value.as_str())
        == Some("commands_DEPRECATED");
    crate::tools::skill_tool::ui::render_tool_use_message(
        skill.as_deref(),
        loaded_from_legacy_commands,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_tool_use_message_matches_official_display_subset() {
        assert_eq!(
            render_tool_use_message(Some("review-pr"), false),
            Some("review-pr".to_string())
        );
        assert_eq!(
            render_tool_use_message(Some("review-pr"), true),
            Some("/review-pr".to_string())
        );
        assert_eq!(render_tool_use_message(Some(""), false), None);
    }

    #[test]
    fn skill_result_lines_match_official_inline_and_forked_shapes() {
        // The raw `toolUseResult` on the row drives the renderer.
        let inline_raw = serde_json::json!({
            "success": true,
            "commandName": "review-pr",
            "allowedTools": ["Bash", "Read"],
        });
        let inline = render_tool_result_message(Some(&inline_raw));
        assert_eq!(
            inline[0].text,
            "Successfully loaded skill · 2 tools allowed"
        );
        assert_eq!(inline[0].tone, ToolRenderTone::Normal);

        let forked_raw = serde_json::json!({
            "success": true,
            "commandName": "review-pr",
            "status": "forked",
            "agentId": "agent-1",
            "result": "All good",
        });
        let forked = render_tool_result_message(Some(&forked_raw));
        assert_eq!(forked[0].text, "Done");
    }

    #[test]
    fn skill_parse_follows_official_union_order_and_round_trips() {
        // The MCP-prompt inline shape carries `status: 'inline'`.
        let mcp_inline = serde_json::json!({
            "success": true,
            "commandName": "mcp-prompt",
            "status": "inline",
        });
        let output = parse_output(&mcp_inline).unwrap();
        assert!(
            matches!(output, Output::Inline { ref status, .. } if status.as_deref() == Some("inline"))
        );
        assert_eq!(output_to_value(&output), mcp_inline);

        let forked_raw = serde_json::json!({
            "success": true,
            "commandName": "review-pr",
            "status": "forked",
            "agentId": "agent-1",
            "result": "All good",
        });
        let output = parse_output(&forked_raw).unwrap();
        assert!(matches!(output, Output::Forked { .. }));
        assert_eq!(output_to_value(&output), forked_raw);

        // A forked shape missing its required fields fails both branches.
        assert!(
            parse_output(&serde_json::json!({
                "success": true,
                "commandName": "x",
                "status": "forked",
            }))
            .is_none()
        );
    }
}
