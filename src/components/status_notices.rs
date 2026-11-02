//! Maps to: CC `components/StatusNotices.tsx`.
//!
//! Safety boundary: official `StatusNotices` constructs context by reading
//! global config, memory files, auth state, and IDE plugin state. Nyan accepts
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
        StatusNoticeId::ClaudeAiExternalToken => {
            let source = context.auth_token_source.clone();
            element! {
                View(flex_direction: FlexDirection::Row, margin_top: 1u32) {
                    Text(content: figures().warning.to_string(), color: theme.warning)
                    Text(content: format!("Auth conflict: Using {source} instead of Claude account subscription token. Either unset {source}, or run `claude /logout`."), color: theme.warning, wrap: TextWrap::Wrap)
                }
            }
            .into_any()
        }
        StatusNoticeId::ApiKeyConflict => {
            let source = context.api_key_source.clone();
            element! {
                View(flex_direction: FlexDirection::Row, margin_top: 1u32) {
                    Text(content: figures().warning.to_string(), color: theme.warning)
                    Text(content: format!("Auth conflict: Using {source} instead of Anthropic Console key. Either unset {source}, or run `claude /logout`."), color: theme.warning, wrap: TextWrap::Wrap)
                }
            }
            .into_any()
        }
        StatusNoticeId::BothAuthMethods => render_both_auth_methods(context, theme),
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

fn render_both_auth_methods(
    context: &StatusNoticeContext,
    theme: crate::utils::theme::Theme,
) -> AnyElement<'static> {
    let api_key_source = context.api_key_source.clone();
    let auth_token_source = context.auth_token_source.clone();
    let auth_source_label = if auth_token_source == "claude.ai" {
        "claude.ai".to_string()
    } else {
        auth_token_source.clone()
    };
    let auth_action = if api_key_source == "ANTHROPIC_API_KEY" {
        "Unset the ANTHROPIC_API_KEY environment variable, or claude /logout then say \"No\" to the API key approval before login.".to_string()
    } else if api_key_source == "apiKeyHelper" {
        "Unset the apiKeyHelper setting.".to_string()
    } else {
        "claude /logout".to_string()
    };
    let api_action = if auth_token_source == "claude.ai" {
        "claude /logout to sign out of claude.ai.".to_string()
    } else {
        format!("Unset the {auth_token_source} environment variable.")
    };

    element! {
        View(flex_direction: FlexDirection::Column, margin_top: 1u32) {
            View(flex_direction: FlexDirection::Row) {
                Text(content: figures().warning.to_string(), color: theme.warning)
                Text(content: format!("Auth conflict: Both a token ({auth_token_source}) and an API key ({api_key_source}) are set. This may lead to unexpected behavior."), color: theme.warning, wrap: TextWrap::Wrap)
            }
            View(flex_direction: FlexDirection::Column, margin_left: 3u32) {
                Text(content: format!("· Trying to use {auth_source_label}? {auth_action}"), color: theme.warning, wrap: TextWrap::Wrap)
                Text(content: format!("· Trying to use {api_key_source}? {api_action}"), color: theme.warning, wrap: TextWrap::Wrap)
            }
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
    fn status_notices_render_auth_and_jetbrains_copy() {
        let context = StatusNoticeContext {
            is_claude_ai_subscriber: true,
            auth_token_source: "ANTHROPIC_AUTH_TOKEN".to_string(),
            has_console_api_key: true,
            api_key_source: "ANTHROPIC_API_KEY".to_string(),
            is_supported_jetbrains_terminal: true,
            is_jetbrains_plugin_installed: false,
            ide_display_name: Some("PyCharm".to_string()),
            ..StatusNoticeContext::default()
        };
        let text = render_context(context);

        assert!(
            text.contains("instead of Claude account subscription token"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("instead of Anthropic Console key"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("Both a token (ANTHROPIC_AUTH_TOKEN) and an API key"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("Install the PyCharm plugin"),
            "canvas=\n{text}"
        );
        assert!(text.contains(JETBRAINS_MARKETPLACE_URL), "canvas=\n{text}");
    }
}
