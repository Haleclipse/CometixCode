//! Maps to: CC `commands/context/context-noninteractive.ts`.

use super::ContextCommandRequest;
use crate::utils::analyze_context::ContextData;
use crate::utils::format::format_tokens;

/// Maps to CC `formatContextAsMarkdownTable(data)`.
pub fn format_context_as_markdown_table(data: &ContextData) -> String {
    let mut output = "## Context Usage\n\n".to_string();
    output.push_str(&format!("**Model:** {}  \n", data.model));
    output.push_str(&format!(
        "**Tokens:** {} / {} ({}%)\n",
        format_tokens(data.total_tokens),
        format_tokens(data.raw_max_tokens),
        data.percentage
    ));
    if let Some(status) = data
        .collapse_status
        .as_ref()
        .filter(|status| status.enabled)
    {
        let mut parts = Vec::new();
        if status.collapsed_spans > 0 {
            parts.push(format!(
                "{} {} summarized ({} messages)",
                status.collapsed_spans,
                if status.collapsed_spans == 1 {
                    "span"
                } else {
                    "spans"
                },
                status.collapsed_messages
            ));
        }
        if status.staged_spans > 0 {
            parts.push(format!("{} staged", status.staged_spans));
        }
        let summary = if !parts.is_empty() {
            parts.join(", ")
        } else if status.total_spawns > 0 {
            format!(
                "{} {}, nothing staged yet",
                status.total_spawns,
                if status.total_spawns == 1 {
                    "spawn"
                } else {
                    "spawns"
                }
            )
        } else {
            "waiting for first trigger".to_string()
        };
        output.push_str(&format!("**Context strategy:** collapse ({summary})\n"));
        if status.total_errors > 0 {
            output.push_str(&format!(
                "**Collapse errors:** {}/{} spawns failed",
                status.total_errors, status.total_spawns
            ));
            if let Some(last_error) = &status.last_error {
                output.push_str(&format!(
                    " (last: {})",
                    last_error.chars().take(80).collect::<String>()
                ));
            }
            output.push('\n');
        } else if status.empty_spawn_warning_emitted {
            output.push_str(&format!(
                "**Collapse idle:** {} consecutive empty runs\n",
                status.total_empty_spawns
            ));
        }
    }
    output.push('\n');

    let visible = data.categories.iter().filter(|category| {
        category.tokens > 0
            && category.name != "Free space"
            && category.name != "Autocompact buffer"
    });
    let visible = visible.collect::<Vec<_>>();
    if !visible.is_empty() {
        output.push_str("### Estimated usage by category\n\n");
        output.push_str("| Category | Tokens | Percentage |\n");
        output.push_str("|----------|--------|------------|\n");
        for category in visible {
            output.push_str(&format!(
                "| {} | {} | {:.1}% |\n",
                category.name,
                format_tokens(category.tokens),
                percentage(category.tokens, data.raw_max_tokens)
            ));
        }
        if let Some(category) = data
            .categories
            .iter()
            .find(|category| category.name == "Free space" && category.tokens > 0)
        {
            output.push_str(&format!(
                "| Free space | {} | {:.1}% |\n",
                format_tokens(category.tokens),
                percentage(category.tokens, data.raw_max_tokens)
            ));
        }
        if let Some(category) = data
            .categories
            .iter()
            .find(|category| category.name == "Autocompact buffer" && category.tokens > 0)
        {
            output.push_str(&format!(
                "| Autocompact buffer | {} | {:.1}% |\n",
                format_tokens(category.tokens),
                percentage(category.tokens, data.raw_max_tokens)
            ));
        }
        output.push('\n');
    }

    if !data.mcp_tools.is_empty() {
        output.push_str("### MCP Tools\n\n");
        output.push_str("| Tool | Server | Tokens |\n");
        output.push_str("|------|--------|--------|\n");
        for tool in &data.mcp_tools {
            output.push_str(&format!(
                "| {} | {} | {} |\n",
                tool.name,
                tool.server_name,
                format_tokens(tool.tokens)
            ));
        }
        output.push('\n');
    }

    if !data.agents.is_empty() {
        output.push_str("### Custom Agents\n\n");
        output.push_str("| Agent Type | Source | Tokens |\n");
        output.push_str("|------------|--------|--------|\n");
        for agent in &data.agents {
            output.push_str(&format!(
                "| {} | {} | {} |\n",
                agent.agent_type,
                agent.source.display_name(),
                format_tokens(agent.tokens)
            ));
        }
        output.push('\n');
    }

    if !data.memory_files.is_empty() {
        output.push_str("### Memory Files\n\n");
        output.push_str("| Type | Path | Tokens |\n");
        output.push_str("|------|------|--------|\n");
        for file in &data.memory_files {
            output.push_str(&format!(
                "| {} | {} | {} |\n",
                file.file_type,
                file.path,
                format_tokens(file.tokens)
            ));
        }
        output.push('\n');
    }

    if let Some(skills) = data
        .skills
        .as_ref()
        .filter(|skills| skills.tokens > 0 && !skills.skill_frontmatter.is_empty())
    {
        output.push_str("### Skills\n\n");
        output.push_str("| Skill | Source | Tokens |\n");
        output.push_str("|-------|--------|--------|\n");
        for skill in &skills.skill_frontmatter {
            output.push_str(&format!(
                "| {} | {} | {} |\n",
                skill.name,
                skill.source.display_name(),
                format_tokens(skill.tokens)
            ));
        }
        output.push('\n');
    }

    output
}

pub fn call(request: &ContextCommandRequest) -> String {
    format_context_as_markdown_table(&super::context::collect_context_data(request))
}

fn percentage(tokens: u64, max: u64) -> f64 {
    if max == 0 {
        0.0
    } else {
        tokens as f64 / max as f64 * 100.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::analyze_context::{ContextCategory, ContextMemoryFileInfo};

    #[test]
    fn context_noninteractive_formats_official_markdown_tables() {
        let theme = *crate::utils::theme::current();
        let data = ContextData {
            categories: vec![
                ContextCategory {
                    name: "Messages".to_string(),
                    tokens: 1_000,
                    color: theme.agent_purple,
                    is_deferred: false,
                },
                ContextCategory {
                    name: "Free space".to_string(),
                    tokens: 9_000,
                    color: theme.prompt_border,
                    is_deferred: false,
                },
            ],
            total_tokens: 1_000,
            raw_max_tokens: 10_000,
            percentage: 10,
            model: "sonnet".to_string(),
            memory_files: vec![ContextMemoryFileInfo {
                path: "/repo/AGENTS.md".to_string(),
                file_type: "Project".to_string(),
                tokens: 200,
            }],
            ..ContextData::default()
        };
        let output = format_context_as_markdown_table(&data);
        assert!(output.starts_with("## Context Usage\n\n**Model:** sonnet  \n"));
        assert!(output.contains("| Messages | 1k | 10.0% |"));
        assert!(output.contains("| Free space | 9k | 90.0% |"));
        assert!(output.contains("| Project | /repo/AGENTS.md | 200 |"));
    }
}
