//! Maps to: CC `components/StatusNotices.tsx`.
//!
//! Safety boundary: official `StatusNotices` constructs context by reading
//! global config, memory files, and IDE plugin state. Nyan accepts
//! a prebuilt `StatusNoticeContext` snapshot and renders active notices only.

use crate::constants::figures::figures;
use crate::utils::format::format_number;
use crate::utils::status_notice_definitions::{
    JETBRAINS_MARKETPLACE_URL, MAX_MEMORY_CHARACTER_COUNT, MemoryFileInfo, StatusNoticeContext,
    StatusNoticeId, get_active_notices, get_large_memory_files, status_notice_memory_display_path,
};
use crate::utils::status_notice_helpers::{
    AGENT_DESCRIPTIONS_THRESHOLD, get_agent_descriptions_total_tokens,
};
use iocraft::prelude::*;

#[derive(Default, Props)]
pub struct StatusNoticesProps {
    pub context: StatusNoticeContext,
}

#[component]
pub fn StatusNotices(props: &StatusNoticesProps, hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<crate::utils::theme::Theme>();
    let active_notices = get_active_notices(&props.context);
    if active_notices.is_empty() {
        return element! { View {} }.into_any();
    }

    let children: Vec<AnyElement<'static>> = active_notices
        .into_iter()
        .map(|notice| render_status_notice(notice, &props.context, *theme))
        .collect();

    element! {
        View(flex_direction: FlexDirection::Column, padding_left: 1u32) {
            #(children)
        }
    }
    .into_any()
}

fn render_status_notice(
    notice: StatusNoticeId,
    context: &StatusNoticeContext,
    theme: crate::utils::theme::Theme,
) -> AnyElement<'static> {
    match notice {
        StatusNoticeId::LargeMemoryFiles => render_large_memory_files(context, theme),
        StatusNoticeId::LargeAgentDescriptions => render_large_agent_descriptions(context, theme),
        StatusNoticeId::JetbrainsPluginInstall => {
            let ide_name = context
                .ide_display_name
                .clone()
                .unwrap_or_else(|| "JetBrains".to_string());
            element! {
                View(flex_direction: FlexDirection::Row, gap: 1u32, margin_left: 1u32) {
                    Text(content: figures().arrow_up.to_string(), color: theme.ide)
                    View(flex_direction: FlexDirection::Row) {
                        Text(content: "Install the ".to_string())
                        Text(content: ide_name, color: theme.ide)
                        Text(content: " plugin from the JetBrains Marketplace: ".to_string())
                        Text(content: JETBRAINS_MARKETPLACE_URL.to_string(), weight: Weight::Bold)
                    }
                }
            }
            .into_any()
        }
    }
}

fn render_large_memory_files(
    context: &StatusNoticeContext,
    theme: crate::utils::theme::Theme,
) -> AnyElement<'static> {
    let rows: Vec<AnyElement<'static>> = get_large_memory_files(&context.memory_files)
        .into_iter()
        .map(|file| render_large_memory_file_row(file, context, theme))
        .collect();
    element! {
        View(flex_direction: FlexDirection::Column) {
            #(rows)
        }
    }
    .into_any()
}

fn render_large_memory_file_row(
    file: MemoryFileInfo,
    context: &StatusNoticeContext,
    theme: crate::utils::theme::Theme,
) -> AnyElement<'static> {
    let display_path = status_notice_memory_display_path(&file.path, &context.cwd);
    let content_len = file.content.chars().count() as u64;
    element! {
        View(flex_direction: FlexDirection::Row) {
            Text(content: figures().warning.to_string(), color: theme.warning)
            Text(content: "Large ".to_string(), color: theme.warning)
            Text(content: display_path, color: theme.warning, weight: Weight::Bold)
            Text(content: format!(" will impact performance ({} chars > {})", format_number(content_len), format_number(MAX_MEMORY_CHARACTER_COUNT as u64)), color: theme.warning)
            Text(content: " · /memory to edit".to_string(), dim: true)
        }
    }
    .into_any()
}

fn render_large_agent_descriptions(
    context: &StatusNoticeContext,
    theme: crate::utils::theme::Theme,
) -> AnyElement<'static> {
    let total_tokens = get_agent_descriptions_total_tokens(context.agent_definitions.as_ref());
    element! {
        View(flex_direction: FlexDirection::Row) {
            Text(content: figures().warning.to_string(), color: theme.warning)
            Text(content: format!("Large cumulative agent descriptions will impact performance (~{} tokens > {})", format_number(total_tokens), format_number(AGENT_DESCRIPTIONS_THRESHOLD)), color: theme.warning)
            Text(content: " · /agents to manage".to_string(), dim: true)
        }
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::status_notice_definitions::MemoryFileInfo;
    use crate::utils::status_notice_helpers::{AgentDefinitionSnapshot, AgentDefinitionsSnapshot};
    use crate::utils::theme;

    fn render_context(context: StatusNoticeContext) -> String {
        let current_theme = *theme::current();
        element! {
            ContextProvider(value: Context::owned(current_theme)) {
                StatusNotices(context: context)
            }
        }
        .render(Some(140))
        .to_string()
    }

    #[test]
    fn status_notices_empty_context_renders_nothing() {
        let text = render_context(StatusNoticeContext::default());
        assert!(text.trim().is_empty(), "canvas=\n{text}");
    }

    #[test]
    fn status_notices_render_large_memory_and_agents_copy() {
        let context = StatusNoticeContext {
            cwd: "/repo".to_string(),
            memory_files: vec![MemoryFileInfo {
                path: "/repo/AGENTS.md".to_string(),
                content: "x".repeat(MAX_MEMORY_CHARACTER_COUNT + 1),
            }],
            agent_definitions: Some(AgentDefinitionsSnapshot {
                active_agents: vec![AgentDefinitionSnapshot {
                    agent_type: "reviewer".to_string(),
                    when_to_use: "x".repeat((AGENT_DESCRIPTIONS_THRESHOLD as usize + 1) * 4),
                    source: "project".to_string(),
                }],
            }),
            ..StatusNoticeContext::default()
        };
        let text = render_context(context);

        assert!(
            text.contains("Large AGENTS.md will impact performance"),
            "canvas=\n{text}"
        );
        assert!(text.contains("/memory to edit"), "canvas=\n{text}");
        assert!(
            text.contains("Large cumulative agent descriptions"),
            "canvas=\n{text}"
        );
        assert!(text.contains("/agents to manage"), "canvas=\n{text}");
    }

    #[test]
    fn status_notices_render_jetbrains_copy_and_no_auth_conflict_notices() {
        let context = StatusNoticeContext {
            is_supported_jetbrains_terminal: true,
            is_jetbrains_plugin_installed: false,
            ide_display_name: Some("PyCharm".to_string()),
            ..StatusNoticeContext::default()
        };
        let text = render_context(context);

        assert!(!text.contains("Auth conflict"), "canvas=\n{text}");
        assert!(
            text.contains("Install the PyCharm plugin"),
            "canvas=\n{text}"
        );
        assert!(text.contains(JETBRAINS_MARKETPLACE_URL), "canvas=\n{text}");
    }
}
