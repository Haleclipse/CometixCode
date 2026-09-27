//! Maps to: CC `commands/context/context.tsx`.

use super::ContextCommandRequest;
use crate::components::context_visualization::ContextVisualization;
use crate::constants::query_source::QuerySource;
use crate::utils::analyze_context::{AnalyzeContextUsageInput, ContextData, analyze_context_usage};
use iocraft::prelude::*;

/// Maps to: CC `commands/context/context.tsx#toApiView` followed by
/// `microcompactMessages(apiView)` and `analyzeContextUsage(...)`.
pub fn collect_context_data(request: &ContextCommandRequest) -> ContextData {
    let api_view =
        crate::utils::messages::get_messages_after_compact_boundary(&request.context.messages);
    // CONTEXT_COLLAPSE `projectView` remains an explicit service seam: the
    // rebuild feature has no local projection operation yet.
    let compacted = crate::services::compact::micro_compact::microcompact_messages(
        api_view.clone(),
        &request.context,
        &QuerySource::Prompt,
    )
    .messages;

    let app_state = request
        .context
        .app_store
        .store
        .as_ref()
        .map(crate::state::store::AppStore::get)
        .unwrap_or_else(|| std::sync::Arc::new(crate::state::app_state_store::AppState::default()));
    let model = request
        .context
        .main_loop_model
        .clone()
        .unwrap_or_else(crate::utils::model::model::get_main_loop_model);
    let cwd = request
        .context
        .cwd_override
        .clone()
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| std::path::PathBuf::from("."));

    analyze_context_usage(AnalyzeContextUsageInput {
        messages: &compacted,
        model: &model,
        tool_permission_context: &request.context.tool_permission_context,
        tools: &request.context.tools,
        agent_definitions: &app_state.agent_definitions,
        terminal_width: request.terminal_width,
        system_prompt_overrides: &request.system_prompt_overrides,
        main_thread_agent_definition: request.main_thread_agent_definition.as_ref(),
        original_messages: Some(&api_view),
        cwd: &cwd,
        // CC keeps theme keys in the data and resolves them under the
        // ThemeProvider `renderToAnsiString` mounts; the port resolves them
        // here, against the theme that provider starts with.
        theme: *crate::utils::theme::get_theme(
            crate::components::design_system::theme_provider::initial_theme_name(),
        ),
    })
}

/// Maps to CC `renderToAnsiString(<ContextVisualization data={data} />)`,
/// whose render wraps the tree in ThemeProvider (`ink.ts:12-23`).
pub fn render_context_to_ansi(data: ContextData, columns: u16) -> Result<String, String> {
    let mut element = element! {
        crate::components::design_system::theme_provider::ThemeProvider {
            ContextVisualization(data: data)
        }
    };
    let canvas = element.render(Some(columns.max(1) as usize));
    let mut bytes = Vec::new();
    canvas
        .write_ansi(&mut bytes)
        .map_err(|error| format!("failed to encode context visualization: {error}"))?;
    let mut output = String::from_utf8(bytes)
        .map_err(|error| format!("context visualization was not UTF-8: {error}"))?;
    while output.ends_with('\n') || output.ends_with('\r') {
        output.pop();
    }
    Ok(output)
}

/// Maps to: CC `commands/context/context.tsx#call`.
///
/// The REPL invokes this on its command worker, so memory/config discovery and
/// static rendering never execute in the retained update frame.
pub fn call(request: &ContextCommandRequest) -> Result<String, String> {
    let width = request.terminal_width.unwrap_or(80);
    render_context_to_ansi(collect_context_data(request), width)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::message::{Message, UserContent, UserMessage};

    #[test]
    fn context_call_projects_compact_boundary_view_and_renders_official_header() {
        let mut context = crate::tool::ToolUseContext::default();
        context.main_loop_model = Some("claude-sonnet-4-20250514".to_string());
        context.messages = vec![Message::User(UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![UserContent::Text("hello context".to_string())],
            is_compact_summary: false,
            plan_content: None,
            image_paste_ids: None,
            is_visible_in_transcript_only: false,
            mcp_meta: None,
            source_tool_assistant_uuid: None,
            permission_mode: None,
            origin: None,
            summarize_metadata: None,
        })];
        let mut request = ContextCommandRequest::new(&context);
        request.terminal_width = Some(100);
        let output = call(&request).expect("context output");
        assert!(output.contains("Context Usage"), "output=\n{output}");
        assert!(
            output.contains("Estimated usage by category"),
            "output=\n{output}"
        );
        assert!(output.contains("Messages:"), "output=\n{output}");
        assert!(output.contains("Free space:"), "output=\n{output}");
    }
}
