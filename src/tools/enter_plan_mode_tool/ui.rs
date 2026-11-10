//! Main-screen-safe subset of official `EnterPlanModeTool/UI.tsx`.

use super::Output;
use crate::components::messages::user_tool_result_message::utils::{
    ToolRenderLine, ToolRenderTone,
};
use crate::constants::figures::BLACK_CIRCLE;

/// The Rust stand-in for CC's `outputSchema.safeParse(toolUseResult)`
/// (`UserToolSuccessMessage.tsx:80`): `message` is the only field, required
/// (`EnterPlanModeTool.ts:28-32`).
pub(crate) fn parse_output(value: &serde_json::Value) -> Option<Output> {
    let map = value.as_object()?;
    Some(Output {
        message: map.get("message")?.as_str()?.to_string(),
    })
}

/// Serializes [`Output`] to CC's exact `toolUseResult` wire shape.
pub(crate) fn output_to_value(output: &Output) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    map.insert(
        "message".to_string(),
        serde_json::Value::String(output.message.clone()),
    );
    serde_json::Value::Object(map)
}

/// Maps to: CC `EnterPlanModeTool/UI.tsx:14-32` `renderToolResultMessage` —
/// the constant two-line acknowledgement; the parsed output itself is unused,
/// but the safeParse gate still runs first.
pub(crate) fn render_tool_result_message(
    raw_output: Option<&serde_json::Value>,
) -> Vec<ToolRenderLine> {
    if raw_output.and_then(parse_output).is_none() {
        return Vec::new();
    }
    vec![
        ToolRenderLine::new(
            format!("{BLACK_CIRCLE} Entered plan mode"),
            ToolRenderTone::Normal,
        ),
        ToolRenderLine::new(
            "nyan is now exploring and designing an implementation approach.",
            ToolRenderTone::Inactive,
        ),
    ]
}

pub fn render_rejected_result_lines() -> Vec<ToolRenderLine> {
    vec![ToolRenderLine::new(
        format!("{BLACK_CIRCLE} User declined to enter plan mode"),
        ToolRenderTone::Normal,
    )]
}
