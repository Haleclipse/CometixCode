//! Maps to: CC `components/ContextVisualization.tsx`.
//!
//! Context analysis and suggestion generation remain in the analyzer/service
//! slices. This component owns the official context-usage visualization and
//! legend rendering from a caller-provided `ContextData` snapshot.

use crate::components::context_suggestions::ContextSuggestions;
use crate::utils::analyze_context::{
    CollapseStatusSnapshot, ContextAgentInfo, ContextCategory, ContextData, ContextGridSquare,
    ContextMemoryFileInfo, ContextSkillFrontmatterInfo, ContextSkillsInfo, ContextSource,
    ContextToolInfo,
};
use crate::utils::context_suggestions::generate_context_suggestions;
use crate::utils::file::get_display_path;
use crate::utils::format::format_tokens;
use crate::utils::string_utils::plural;
use crate::utils::theme::Theme;
use iocraft::prelude::*;
use std::collections::BTreeMap;

const RESERVED_CATEGORY_NAME: &str = "Autocompact buffer";
const SOURCE_DISPLAY_ORDER: &[&str] = &["Project", "User", "Plugin", "Built-in"];

#[derive(Clone, Debug, Default, Props)]
pub struct ContextVisualizationProps {
    pub data: ContextData,
}

pub trait ContextSourceItem: Clone {
    fn source(&self) -> ContextSource;
    fn tokens(&self) -> u64;
}

impl ContextSourceItem for ContextAgentInfo {
    fn source(&self) -> ContextSource {
        self.source
    }

    fn tokens(&self) -> u64 {
        self.tokens
    }
}

impl ContextSourceItem for ContextSkillFrontmatterInfo {
    fn source(&self) -> ContextSource {
        self.source
    }

    fn tokens(&self) -> u64 {
        self.tokens
    }
}

/// Maps to: CC `ContextVisualization.tsx#groupBySource`.
pub fn group_by_source<T: ContextSourceItem>(items: &[T]) -> Vec<(String, Vec<T>)> {
    let mut groups: BTreeMap<String, Vec<T>> = BTreeMap::new();
    for item in items {
        groups
            .entry(item.source().display_name().to_string())
            .or_default()
            .push(item.clone());
    }
    for group in groups.values_mut() {
        group.sort_by_key(|item| std::cmp::Reverse(item.tokens()));
    }
    SOURCE_DISPLAY_ORDER
        .iter()
        .filter_map(|source| {
            groups
                .remove(*source)
                .map(|items| ((*source).to_string(), items))
        })
        .collect()
}

/// Maps to: CC `ContextVisualization.tsx` visible-category filter.
pub fn visible_context_categories(categories: &[ContextCategory]) -> Vec<ContextCategory> {
    categories
        .iter()
        .filter(|cat| {
            cat.tokens > 0
                && cat.name != "Free space"
                && cat.name != RESERVED_CATEGORY_NAME
                && !cat.is_deferred
        })
        .cloned()
        .collect()
}

/// Maps to: CC `ContextVisualization.tsx#CollapseStatus` summary copy.
pub fn collapse_status_lines(status: Option<&CollapseStatusSnapshot>) -> Vec<String> {
    let Some(s) = status else {
        return Vec::new();
    };
    if !s.enabled {
        return Vec::new();
    }
    let mut parts = Vec::new();
    if s.collapsed_spans > 0 {
        parts.push(format!(
            "{} {} summarized ({} msgs)",
            s.collapsed_spans,
            plural(s.collapsed_spans as usize, "span", None),
            s.collapsed_messages
        ));
    }
    if s.staged_spans > 0 {
        parts.push(format!("{} staged", s.staged_spans));
    }
    let summary = if !parts.is_empty() {
        parts.join(", ")
    } else if s.total_spawns > 0 {
        format!(
            "{} {}, nothing staged yet",
            s.total_spawns,
            plural(s.total_spawns as usize, "spawn", None)
        )
    } else {
        "waiting for first trigger".to_string()
    };
    let mut lines = vec![format!("Context strategy: collapse ({summary})")];
    if s.total_errors > 0 {
        let last = s
            .last_error
            .as_ref()
            .map(|err| format!(" (last: {})", err.chars().take(60).collect::<String>()))
            .unwrap_or_default();
        lines.push(format!(
            "Collapse errors: {}/{} spawns failed{}",
            s.total_errors, s.total_spawns, last
        ));
    } else if s.empty_spawn_warning_emitted {
        lines.push(format!(
            "Collapse idle: {} consecutive empty runs",
            s.total_empty_spawns
        ));
    }
    lines
}

fn percent(tokens: u64, raw_max_tokens: u64) -> String {
    if raw_max_tokens == 0 {
        "0.0%".to_string()
    } else {
        format!("{:.1}%", (tokens as f64 / raw_max_tokens as f64) * 100.0)
    }
}

fn category_symbol(category_name: &str, fullness: f32) -> &'static str {
    if category_name == "Free space" {
        "⛶ "
    } else if category_name == RESERVED_CATEGORY_NAME {
        "⛝ "
    } else if fullness >= 0.7 {
        "⛁ "
    } else {
        "⛀ "
    }
}

/// Maps to: CC `components/ContextVisualization.tsx#ContextVisualization`.
#[component]
pub fn ContextVisualization(
    props: &ContextVisualizationProps,
    hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let data = props.data.clone();
    let visible_categories = visible_context_categories(&data.categories);
    let has_deferred_mcp_tools = data
        .categories
        .iter()
        .any(|cat| cat.is_deferred && cat.name.contains("MCP"));
    let autocompact_category = data
        .categories
        .iter()
        .find(|cat| cat.name == RESERVED_CATEGORY_NAME && cat.tokens > 0)
        .cloned();
    let free_space_tokens = data
        .categories
        .iter()
        .find(|cat| cat.name == "Free space")
        .map(|cat| cat.tokens)
        .unwrap_or(0);
    let collapse_lines = collapse_status_lines(data.collapse_status.as_ref());
    let suggestions = generate_context_suggestions(&data);

    element! {
        View(flex_direction: FlexDirection::Column, padding_left: 1u32) {
            Text(content: "Context Usage".to_string(), weight: Weight::Bold, wrap: TextWrap::NoWrap)
            View(flex_direction: FlexDirection::Row, column_gap: 2u32) {
                View(flex_direction: FlexDirection::Column, flex_shrink: 0.0f32) {
                    #(data.grid_rows.iter().map(|row| element! {
                        View(flex_direction: FlexDirection::Row, margin_left: -1i32) {
                            #(row.iter().map(|square| {
                                let dim = square.category_name == "Free space";
                                element! {
                                    Text(
                                        content: category_symbol(&square.category_name, square.square_fullness).to_string(),
                                        color: if dim { None } else { Some(square.color) },
                                        dim: dim,
                                        wrap: TextWrap::NoWrap,
                                    )
                                }
                            }).collect::<Vec<_>>())
                        }
                    }).collect::<Vec<_>>())
                }
                View(flex_direction: FlexDirection::Column, flex_shrink: 0.0f32) {
                    Text(content: format!("{} · {}/{} tokens ({}%)", data.model, format_tokens(data.total_tokens), format_tokens(data.raw_max_tokens), data.percentage), dim: true, wrap: TextWrap::NoWrap)
                    #(collapse_lines.into_iter().enumerate().map(|(idx, line)| element! {
                        Text(content: line, dim: idx == 0, color: if idx == 0 { None } else { Some(theme.warning) }, wrap: TextWrap::Wrap)
                    }).collect::<Vec<_>>())
                    Text(content: " ".to_string(), wrap: TextWrap::NoWrap)
                    Text(content: "Estimated usage by category".to_string(), dim: true, italic: true, wrap: TextWrap::NoWrap)
                    #(visible_categories.into_iter().map(|cat| {
                        let token_display = format_tokens(cat.tokens);
                        let percent_display = percent(cat.tokens, data.raw_max_tokens);
                        element! {
                            View(flex_direction: FlexDirection::Row) {
                                Text(content: "⛁".to_string(), color: cat.color, wrap: TextWrap::NoWrap)
                                Text(content: format!(" {}: ", cat.name), wrap: TextWrap::NoWrap)
                                Text(content: format!("{token_display} tokens ({percent_display})"), dim: true, wrap: TextWrap::NoWrap)
                            }
                        }
                    }).collect::<Vec<_>>())
                    #(if free_space_tokens > 0 {
                        Some(element! {
                            View(flex_direction: FlexDirection::Row) {
                                Text(content: "⛶".to_string(), dim: true, wrap: TextWrap::NoWrap)
                                Text(content: " Free space: ".to_string(), wrap: TextWrap::NoWrap)
                                Text(content: format!("{} ({})", format_tokens(free_space_tokens), percent(free_space_tokens, data.raw_max_tokens)), dim: true, wrap: TextWrap::NoWrap)
                            }
                        })
                    } else { None })
                    #(autocompact_category.map(|cat| element! {
                        View(flex_direction: FlexDirection::Row) {
                            Text(content: "⛝".to_string(), color: cat.color, wrap: TextWrap::NoWrap)
                            Text(content: format!(" {}: ", cat.name), dim: true, wrap: TextWrap::NoWrap)
                            Text(content: format!("{} tokens ({})", format_tokens(cat.tokens), percent(cat.tokens, data.raw_max_tokens)), dim: true, wrap: TextWrap::NoWrap)
                        }
                    }))
                }
            }

            View(flex_direction: FlexDirection::Column, margin_left: -1i32) {
                #(if data.mcp_tools.is_empty() { None } else {
                    let loaded = data.mcp_tools.iter().filter(|tool| tool.is_loaded).cloned().collect::<Vec<_>>();
                    let available = data.mcp_tools.iter().filter(|tool| !tool.is_loaded).cloned().collect::<Vec<_>>();
                    Some(element! {
                        View(flex_direction: FlexDirection::Column, margin_top: 1u32) {
                            View(flex_direction: FlexDirection::Row) {
                                Text(content: "MCP tools".to_string(), weight: Weight::Bold, wrap: TextWrap::NoWrap)
                                Text(content: format!(" · /mcp{}", if has_deferred_mcp_tools { " (loaded on-demand)" } else { "" }), dim: true, wrap: TextWrap::NoWrap)
                            }
                            #(if has_deferred_mcp_tools {
                                let mut blocks = Vec::<AnyElement<'static>>::new();
                                if !loaded.is_empty() {
                                    blocks.push(element! {
                                        View(flex_direction: FlexDirection::Column, margin_top: 1u32) {
                                            Text(content: "Loaded".to_string(), dim: true, wrap: TextWrap::NoWrap)
                                            #(loaded.iter().map(|tool| element! {
                                                View(flex_direction: FlexDirection::Row) {
                                                    Text(content: format!("└ {}: ", tool.name), wrap: TextWrap::NoWrap)
                                                    Text(content: format!("{} tokens", format_tokens(tool.tokens)), dim: true, wrap: TextWrap::NoWrap)
                                                }
                                            }).collect::<Vec<_>>())
                                        }
                                    }.into_any());
                                }
                                if !available.is_empty() {
                                    blocks.push(element! {
                                        View(flex_direction: FlexDirection::Column, margin_top: 1u32) {
                                            Text(content: "Available".to_string(), dim: true, wrap: TextWrap::NoWrap)
                                            #(available.iter().map(|tool| element! {
                                                Text(content: format!("└ {}", tool.name), dim: true, wrap: TextWrap::NoWrap)
                                            }).collect::<Vec<_>>())
                                        }
                                    }.into_any());
                                }
                                blocks
                            } else {
                                data.mcp_tools.iter().map(|tool| element! {
                                    View(flex_direction: FlexDirection::Row) {
                                        Text(content: format!("└ {}: ", tool.name), wrap: TextWrap::NoWrap)
                                        Text(content: format!("{} tokens", format_tokens(tool.tokens)), dim: true, wrap: TextWrap::NoWrap)
                                    }
                                }.into_any()).collect::<Vec<_>>()
                            })
                        }
                    })
                })

                #(if data.agents.is_empty() { None } else {
                    Some(element! {
                        View(flex_direction: FlexDirection::Column, margin_top: 1u32) {
                            View(flex_direction: FlexDirection::Row) {
                                Text(content: "Custom agents".to_string(), weight: Weight::Bold, wrap: TextWrap::NoWrap)
                                Text(content: " · /agents".to_string(), dim: true, wrap: TextWrap::NoWrap)
                            }
                            #(group_by_source(&data.agents).into_iter().map(|(source, agents)| element! {
                                View(flex_direction: FlexDirection::Column, margin_top: 1u32) {
                                    Text(content: source, dim: true, wrap: TextWrap::NoWrap)
                                    #(agents.into_iter().map(|agent| element! {
                                        View(flex_direction: FlexDirection::Row) {
                                            Text(content: format!("└ {}: ", agent.agent_type), wrap: TextWrap::NoWrap)
                                            Text(content: format!("{} tokens", format_tokens(agent.tokens)), dim: true, wrap: TextWrap::NoWrap)
                                        }
                                    }).collect::<Vec<_>>())
                                }
                            }).collect::<Vec<_>>())
                        }
                    })
                })

                #(if data.memory_files.is_empty() { None } else {
                    Some(element! {
                        View(flex_direction: FlexDirection::Column, margin_top: 1u32) {
                            View(flex_direction: FlexDirection::Row) {
                                Text(content: "Memory files".to_string(), weight: Weight::Bold, wrap: TextWrap::NoWrap)
                                Text(content: " · /memory".to_string(), dim: true, wrap: TextWrap::NoWrap)
                            }
                            #(data.memory_files.iter().map(|file| element! {
                                View(flex_direction: FlexDirection::Row) {
                                    Text(content: format!("└ {}: ", get_display_path(&file.path)), wrap: TextWrap::NoWrap)
                                    Text(content: format!("{} tokens", format_tokens(file.tokens)), dim: true, wrap: TextWrap::NoWrap)
                                }
                            }).collect::<Vec<_>>())
                        }
                    })
                })

                #(if let Some(skills) = data.skills.clone() {
                    if skills.tokens > 0 {
                        Some(element! {
                            View(flex_direction: FlexDirection::Column, margin_top: 1u32) {
                                View(flex_direction: FlexDirection::Row) {
                                    Text(content: "Skills".to_string(), weight: Weight::Bold, wrap: TextWrap::NoWrap)
                                    Text(content: " · /skills".to_string(), dim: true, wrap: TextWrap::NoWrap)
                                }
                                #(group_by_source(&skills.skill_frontmatter).into_iter().map(|(source, skills)| element! {
                                    View(flex_direction: FlexDirection::Column, margin_top: 1u32) {
                                        Text(content: source, dim: true, wrap: TextWrap::NoWrap)
                                        #(skills.into_iter().map(|skill| element! {
                                            View(flex_direction: FlexDirection::Row) {
                                                Text(content: format!("└ {}: ", skill.name), wrap: TextWrap::NoWrap)
                                                Text(content: format!("{} tokens", format_tokens(skill.tokens)), dim: true, wrap: TextWrap::NoWrap)
                                            }
                                        }).collect::<Vec<_>>())
                                    }
                                }).collect::<Vec<_>>())
                            }
                        })
                    } else { None }
                } else { None })
            }
            ContextSuggestions(suggestions: suggestions)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;

    #[test]
    fn context_visualization_filters_visible_categories_like_official() {
        let cats = vec![
            ContextCategory {
                name: "Messages".to_string(),
                tokens: 10,
                color: Color::Blue,
                is_deferred: false,
            },
            ContextCategory {
                name: "Free space".to_string(),
                tokens: 90,
                color: Color::Grey,
                is_deferred: false,
            },
            ContextCategory {
                name: RESERVED_CATEGORY_NAME.to_string(),
                tokens: 5,
                color: Color::Yellow,
                is_deferred: false,
            },
            ContextCategory {
                name: "MCP tools".to_string(),
                tokens: 1,
                color: Color::Green,
                is_deferred: true,
            },
            ContextCategory {
                name: "Zero".to_string(),
                tokens: 0,
                color: Color::Red,
                is_deferred: false,
            },
        ];
        let visible = visible_context_categories(&cats);
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].name, "Messages");
    }

    #[test]
    fn context_visualization_group_by_source_orders_and_sorts_by_tokens() {
        let agents = vec![
            ContextAgentInfo {
                source: ContextSource::Plugin,
                agent_type: "plugin-small".to_string(),
                tokens: 1,
            },
            ContextAgentInfo {
                source: ContextSource::Project,
                agent_type: "project".to_string(),
                tokens: 5,
            },
            ContextAgentInfo {
                source: ContextSource::Plugin,
                agent_type: "plugin-big".to_string(),
                tokens: 9,
            },
            ContextAgentInfo {
                source: ContextSource::User,
                agent_type: "user".to_string(),
                tokens: 2,
            },
        ];
        let grouped = group_by_source(&agents);
        assert_eq!(grouped[0].0, "Project");
        assert_eq!(grouped[1].0, "User");
        assert_eq!(grouped[2].0, "Plugin");
        assert_eq!(grouped[2].1[0].agent_type, "plugin-big");
    }

    #[test]
    fn context_visualization_collapse_status_copy_matches_official() {
        let lines = collapse_status_lines(Some(&CollapseStatusSnapshot {
            enabled: true,
            collapsed_spans: 2,
            collapsed_messages: 7,
            staged_spans: 1,
            total_spawns: 3,
            total_errors: 1,
            last_error: Some("worker failed".to_string()),
            ..CollapseStatusSnapshot::default()
        }));
        assert_eq!(
            lines[0],
            "Context strategy: collapse (2 spans summarized (7 msgs), 1 staged)"
        );
        assert!(lines[1].contains("Collapse errors: 1/3 spawns failed"));
    }

    #[test]
    fn context_visualization_renders_grid_legend_and_sections() {
        let data = ContextData {
            categories: vec![
                ContextCategory {
                    name: "Messages".to_string(),
                    tokens: 1_200,
                    color: Color::Blue,
                    is_deferred: false,
                },
                ContextCategory {
                    name: "Free space".to_string(),
                    tokens: 8_800,
                    color: Color::Grey,
                    is_deferred: false,
                },
                ContextCategory {
                    name: "MCP tools".to_string(),
                    tokens: 0,
                    color: Color::Green,
                    is_deferred: true,
                },
            ],
            total_tokens: 1_200,
            raw_max_tokens: 10_000,
            percentage: 12,
            grid_rows: vec![vec![
                ContextGridSquare {
                    category_name: "Messages".to_string(),
                    color: Color::Blue,
                    square_fullness: 0.8,
                },
                ContextGridSquare {
                    category_name: "Free space".to_string(),
                    color: Color::Grey,
                    square_fullness: 0.0,
                },
            ]],
            model: "opus".to_string(),
            memory_files: vec![ContextMemoryFileInfo {
                path: "/repo/AGENTS.md".to_string(),
                file_type: "Project".to_string(),
                tokens: 6_000,
            }],
            mcp_tools: vec![
                ContextToolInfo {
                    name: "read_docs".to_string(),
                    server_name: "docs".to_string(),
                    tokens: 100,
                    is_loaded: true,
                },
                ContextToolInfo {
                    name: "search_docs".to_string(),
                    server_name: "docs".to_string(),
                    tokens: 0,
                    is_loaded: false,
                },
            ],
            agents: vec![ContextAgentInfo {
                source: ContextSource::Project,
                agent_type: "reviewer".to_string(),
                tokens: 250,
            }],
            skills: Some(ContextSkillsInfo {
                total_skills: 1,
                included_skills: 1,
                tokens: 80,
                skill_frontmatter: vec![ContextSkillFrontmatterInfo {
                    source: ContextSource::User,
                    name: "rust".to_string(),
                    tokens: 80,
                }],
            }),
            ..ContextData::default()
        };
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                ContextVisualization(data: data)
            }
        }
        .render(Some(160))
        .to_string();

        assert!(text.contains("Context Usage"), "canvas=\n{text}");
        assert!(
            text.contains("opus · 1.2k/10k tokens (12%)"),
            "canvas=\n{text}"
        );
        assert!(text.contains("Messages:"), "canvas=\n{text}");
        assert!(text.contains("Free space:"), "canvas=\n{text}");
        assert!(
            text.contains("MCP tools · /mcp (loaded on-demand)"),
            "canvas=\n{text}"
        );
        assert!(text.contains("Loaded"), "canvas=\n{text}");
        assert!(text.contains("Available"), "canvas=\n{text}");
        assert!(text.contains("Custom agents"), "canvas=\n{text}");
        assert!(text.contains("Memory files"), "canvas=\n{text}");
        assert!(text.contains("Skills"), "canvas=\n{text}");
        assert!(text.contains("Suggestions"), "canvas=\n{text}");
    }
}
