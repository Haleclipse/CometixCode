//! Maps to: CC `utils/analyzeContext.ts`.
//!
//! Owns `/context` data collection: model-visible message accounting, system
//! prompt/tool/memory/agent/skill estimates, category construction, API-usage
//! reconciliation, and the fixed context grid. The count-tokens API adapter is
//! not yet used for every individual category; local UTF-16 estimates are the
//! explicit fallback at the same service boundary.

use crate::commands::CommandSource;
use crate::tool::ToolPermissionContext;
use crate::tools::agent_tool::load_agents_dir::{
    AgentDefinition, AgentDefinitionSource, AgentDefinitionsResult,
};
use crate::types::message::{AssistantContent, Message, TokenUsage, UserContent};
use crate::types::tools::Tool;
use crate::utils::system_prompt::CliSystemPromptOverrides;
use iocraft::Color;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

pub const TOOL_TOKEN_COUNT_OVERHEAD: u64 = 500;
const RESERVED_CATEGORY_NAME: &str = "Autocompact buffer";
const MANUAL_COMPACT_BUFFER_NAME: &str = "Compact buffer";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum ContextSource {
    Project,
    User,
    Local,
    Flag,
    Managed,
    Plugin,
    #[default]
    BuiltIn,
}

impl ContextSource {
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Project => "Project",
            Self::User => "User",
            Self::Local => "Local",
            Self::Flag => "Flag",
            Self::Managed => "Managed",
            Self::Plugin => "Plugin",
            Self::BuiltIn => "Built-in",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ContextCategory {
    pub name: String,
    pub tokens: u64,
    /// L1 projection of CC's `keyof Theme`; the analyzer resolves the key from
    /// the caller-provided theme before the iocraft component is mounted.
    pub color: Color,
    pub is_deferred: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ContextGridSquare {
    pub category_name: String,
    pub color: Color,
    pub square_fullness: f32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContextToolInfo {
    pub name: String,
    pub server_name: String,
    pub tokens: u64,
    pub is_loaded: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContextSystemPromptSectionInfo {
    pub name: String,
    pub tokens: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContextAgentInfo {
    pub source: ContextSource,
    pub agent_type: String,
    pub tokens: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContextMemoryFileInfo {
    pub path: String,
    pub file_type: String,
    pub tokens: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContextSkillFrontmatterInfo {
    pub source: ContextSource,
    pub name: String,
    pub tokens: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContextSkillsInfo {
    pub total_skills: usize,
    pub included_skills: usize,
    pub tokens: u64,
    pub skill_frontmatter: Vec<ContextSkillFrontmatterInfo>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContextSlashCommandInfo {
    pub total_commands: usize,
    pub included_commands: usize,
    pub tokens: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CollapseStatusSnapshot {
    pub enabled: bool,
    pub collapsed_spans: u64,
    pub collapsed_messages: u64,
    pub staged_spans: u64,
    pub total_spawns: u64,
    pub total_errors: u64,
    pub last_error: Option<String>,
    pub empty_spawn_warning_emitted: bool,
    pub total_empty_spawns: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContextToolBreakdown {
    pub name: String,
    pub call_tokens: u64,
    pub result_tokens: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContextAttachmentBreakdown {
    pub name: String,
    pub tokens: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContextMessageBreakdown {
    pub tool_call_tokens: u64,
    pub tool_result_tokens: u64,
    pub attachment_tokens: u64,
    pub assistant_message_tokens: u64,
    pub user_message_tokens: u64,
    pub tool_calls_by_type: Vec<ContextToolBreakdown>,
    pub attachments_by_type: Vec<ContextAttachmentBreakdown>,
}

/// Maps to: CC `utils/analyzeContext.ts#ContextData`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ContextData {
    pub categories: Vec<ContextCategory>,
    pub total_tokens: u64,
    pub max_tokens: u64,
    pub raw_max_tokens: u64,
    pub percentage: u64,
    pub grid_rows: Vec<Vec<ContextGridSquare>>,
    pub model: String,
    pub memory_files: Vec<ContextMemoryFileInfo>,
    pub mcp_tools: Vec<ContextToolInfo>,
    pub deferred_builtin_tools: Vec<ContextToolInfo>,
    pub system_tools: Vec<ContextToolInfo>,
    pub system_prompt_sections: Vec<ContextSystemPromptSectionInfo>,
    pub agents: Vec<ContextAgentInfo>,
    pub slash_commands: Option<ContextSlashCommandInfo>,
    pub skills: Option<ContextSkillsInfo>,
    pub auto_compact_threshold: Option<u64>,
    pub is_auto_compact_enabled: bool,
    pub message_breakdown: Option<ContextMessageBreakdown>,
    pub api_usage: Option<TokenUsage>,
    /// iocraft L1 snapshot for CC `CollapseStatus`, whose source component
    /// reads the context-collapse singleton directly.
    pub collapse_status: Option<CollapseStatusSnapshot>,
}

/// Input corresponding to `analyzeContextUsage(...)` arguments.
pub struct AnalyzeContextUsageInput<'a> {
    pub messages: &'a [Message],
    pub model: &'a str,
    pub tool_permission_context: &'a ToolPermissionContext,
    pub tools: &'a [Tool],
    pub agent_definitions: &'a AgentDefinitionsResult,
    pub terminal_width: Option<u16>,
    pub system_prompt_overrides: &'a CliSystemPromptOverrides,
    pub main_thread_agent_definition: Option<&'a AgentDefinition>,
    pub original_messages: Option<&'a [Message]>,
    pub cwd: &'a Path,
    pub theme: crate::utils::theme::Theme,
}

fn estimate_json(value: &serde_json::Value) -> u64 {
    serde_json::to_string(value)
        .map(|value| {
            (crate::services::token_estimation::rough_token_count_estimation(&value) as u64)
        })
        .unwrap_or_default()
}

fn extract_section_name(content: &str) -> String {
    if let Some(heading) = content.lines().find_map(|line| {
        let trimmed = line.trim();
        let rest = trimmed.trim_start_matches('#');
        (rest.len() < trimmed.len() && rest.starts_with(char::is_whitespace))
            .then(|| rest.trim().to_string())
    }) {
        return heading;
    }
    let first = content
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default()
        .trim();
    if first.chars().count() > 40 {
        format!("{}…", first.chars().take(40).collect::<String>())
    } else {
        first.to_string()
    }
}

fn memory_type_name(kind: crate::utils::claudemd::ClaudeMdKind) -> &'static str {
    use crate::utils::claudemd::ClaudeMdKind;
    match kind {
        ClaudeMdKind::Managed => "Managed",
        ClaudeMdKind::User => "User",
        ClaudeMdKind::Project => "Project",
        ClaudeMdKind::Local => "Local",
        ClaudeMdKind::AutoMem => "Auto memory",
        ClaudeMdKind::TeamMem => "Team memory",
    }
}

fn agent_source(source: AgentDefinitionSource) -> ContextSource {
    match source {
        AgentDefinitionSource::ProjectSettings => ContextSource::Project,
        AgentDefinitionSource::FlagSettings => ContextSource::Flag,
        AgentDefinitionSource::UserSettings => ContextSource::User,
        AgentDefinitionSource::PolicySettings => ContextSource::Managed,
        AgentDefinitionSource::Plugin => ContextSource::Plugin,
        AgentDefinitionSource::BuiltIn => ContextSource::BuiltIn,
    }
}

fn command_source(source: CommandSource) -> ContextSource {
    match source {
        CommandSource::ProjectSettings => ContextSource::Project,
        CommandSource::UserSettings => ContextSource::User,
        CommandSource::PolicySettings => ContextSource::Managed,
        CommandSource::Plugin | CommandSource::Mcp => ContextSource::Plugin,
        CommandSource::Builtin | CommandSource::Bundled => ContextSource::BuiltIn,
    }
}

/// Maps to: CC `analyzeContext.ts:645-660` — the local per-tool estimate,
/// `roughTokenCountEstimation(jsonStringify({ name, description: await
/// t.prompt({ getToolPermissionContext, tools, agents: agentInfo?.activeAgents
/// ?? [] }), input_schema }))`, whose comment states the intent: "Include name
/// + description + input schema to match what toolToAPISchema sends".
///
/// So the description must be the SAME lazy read the wire schema does
/// (`utils/api.ts:171-176`), resolved with this turn's LIVE permission context
/// and agent list — not the instance's eagerly rendered field, which for the
/// Agent tool is the unfiltered listing and would over-count a session whose
/// deny rules or `allowedAgentTypes` narrow it.
///
/// CC reaches this local estimate only for MCP tools (built-ins are counted by
/// the remote `countToolDefinitionTokens` → `toolToAPISchema` path, `:234-249`,
/// which the port has not adopted — see this module's header); the port routes
/// both families through it, so both now read the same source CC's schema
/// serialization does.
fn tool_definition_tokens(tool: &Tool, options: &crate::tool::ToolPromptOptions<'_>) -> u64 {
    estimate_json(&serde_json::json!({
        "name": tool.name,
        "description": tool.prompt(options),
        "input_schema": tool.input_schema,
    }))
}

fn count_message_breakdown(messages: &[Message]) -> ContextMessageBreakdown {
    let mut tool_use_names = HashMap::<String, String>::new();
    for message in messages {
        if let Message::Assistant(assistant) = message {
            for content in &assistant.content {
                if let AssistantContent::ToolUse(tool) | AssistantContent::ServerToolUse(tool) =
                    content
                {
                    tool_use_names.insert(tool.id.0.clone(), tool.name.clone());
                }
            }
        }
    }

    let mut breakdown = ContextMessageBreakdown::default();
    let mut calls = BTreeMap::<String, (u64, u64)>::new();
    let mut attachments = BTreeMap::<String, u64>::new();

    for message in messages {
        match message {
            Message::Assistant(assistant) => {
                for content in &assistant.content {
                    match content {
                        AssistantContent::ToolUse(tool) | AssistantContent::ServerToolUse(tool) => {
                            let tokens =
                                (crate::services::token_estimation::rough_token_count_estimation(
                                    &format!("{}{}", tool.name, tool.input),
                                ) as u64);
                            breakdown.tool_call_tokens += tokens;
                            calls.entry(tool.name.clone()).or_default().0 += tokens;
                        }
                        // Only the plain advisor result carries countable text;
                        // redacted and error results have none.
                        AssistantContent::Advisor { content, .. } => {
                            breakdown.assistant_message_tokens +=
                                content.text().map_or(0, |text| {
                                    crate::services::token_estimation::rough_token_count_estimation(
                                        text,
                                    ) as u64
                                });
                        }
                        AssistantContent::Text(text) | AssistantContent::Thinking { text, .. } => {
                            breakdown.assistant_message_tokens +=
                                (crate::services::token_estimation::rough_token_count_estimation(
                                    text,
                                ) as u64);
                        }
                        AssistantContent::RedactedThinking { data } => {
                            breakdown.assistant_message_tokens +=
                                (crate::services::token_estimation::rough_token_count_estimation(
                                    data,
                                ) as u64);
                        }
                        AssistantContent::WebSearchToolResult { content, .. } => {
                            breakdown.assistant_message_tokens += estimate_json(content);
                        }
                        AssistantContent::MessageIdentity(_) => {}
                    }
                }
            }
            Message::User(user) => {
                for content in &user.content {
                    match content {
                        UserContent::ToolResult(result) => {
                            let tokens =
                                (crate::services::token_estimation::rough_token_count_estimation(
                                    &result.content,
                                ) as u64)
                                    + result
                                        .content_blocks
                                        .iter()
                                        .map(|block| {
                                            serde_json::to_value(block)
                                                .ok()
                                                .as_ref()
                                                .map(estimate_json)
                                                .unwrap_or_default()
                                        })
                                        .sum::<u64>();
                            breakdown.tool_result_tokens += tokens;
                            let name = tool_use_names
                                .get(&result.tool_use_id.0)
                                .cloned()
                                .unwrap_or_else(|| "unknown".to_string());
                            calls.entry(name).or_default().1 += tokens;
                        }
                        UserContent::Text(text) | UserContent::MetaText(text) => {
                            breakdown.user_message_tokens +=
                                (crate::services::token_estimation::rough_token_count_estimation(
                                    text,
                                ) as u64);
                        }
                        UserContent::RawImage { block, .. } => {
                            breakdown.user_message_tokens +=
                                (crate::services::token_estimation::rough_token_count_estimation(
                                    block
                                        .pointer("/source/data")
                                        .and_then(serde_json::Value::as_str)
                                        .unwrap_or_default(),
                                ) as u64)
                                    .max(1_000);
                        }
                        UserContent::Image { data, .. }
                        | UserContent::MetaImage { data, .. }
                        | UserContent::Document { data, .. }
                        | UserContent::MetaDocument { data, .. } => {
                            breakdown.user_message_tokens +=
                                (crate::services::token_estimation::rough_token_count_estimation(
                                    data,
                                ) as u64)
                                    .max(1_000);
                        }
                    }
                }
            }
            Message::Attachment(attachment) => {
                let name = attachment.attachment_type().to_string();
                let tokens = estimate_json(&attachment.attachment.to_wire());
                breakdown.attachment_tokens += tokens;
                *attachments.entry(name).or_default() += tokens;
            }
            Message::System(system) => {
                breakdown.user_message_tokens +=
                    (crate::services::token_estimation::rough_token_count_estimation(
                        system.content().unwrap_or(""),
                    ) as u64);
            }
            Message::HookResult(hook) => {
                breakdown.user_message_tokens += estimate_json(&hook.attachment);
            }
            // Progress never reaches the model context (CC
            // tokenEstimation.ts:368 returns 0 for it).
            Message::Progress(_) => {}
        }
    }

    let mut tool_calls_by_type = calls
        .into_iter()
        .map(
            |(name, (call_tokens, result_tokens))| ContextToolBreakdown {
                name,
                call_tokens,
                result_tokens,
            },
        )
        .collect::<Vec<_>>();
    tool_calls_by_type
        .sort_by_key(|tool| std::cmp::Reverse(tool.call_tokens.saturating_add(tool.result_tokens)));
    breakdown.tool_calls_by_type = tool_calls_by_type;

    let mut attachments_by_type = attachments
        .into_iter()
        .map(|(name, tokens)| ContextAttachmentBreakdown { name, tokens })
        .collect::<Vec<_>>();
    attachments_by_type.sort_by_key(|item| std::cmp::Reverse(item.tokens));
    breakdown.attachments_by_type = attachments_by_type;
    breakdown
}

fn create_grid(
    categories: &[ContextCategory],
    context_window: u64,
    terminal_width: Option<u16>,
    theme: crate::utils::theme::Theme,
) -> Vec<Vec<ContextGridSquare>> {
    if context_window == 0 {
        return Vec::new();
    }
    let narrow = terminal_width.is_some_and(|width| width < 80);
    let (grid_width, grid_height) = if context_window >= 1_000_000 {
        (if narrow { 5 } else { 20 }, 10)
    } else if narrow {
        (5, 5)
    } else {
        (10, 10)
    };
    let total_squares = grid_width * grid_height;

    let make_squares = |category: &ContextCategory| {
        let exact = category.tokens as f64 / context_window as f64 * total_squares as f64;
        let count = if category.name == "Free space" {
            exact.round() as usize
        } else {
            1usize.max(exact.round() as usize)
        };
        let whole = exact.floor() as usize;
        let fraction = (exact - exact.floor()) as f32;
        (0..count)
            .map(|index| ContextGridSquare {
                category_name: category.name.clone(),
                color: category.color,
                square_fullness: if index == whole && fraction > 0.0 {
                    fraction
                } else {
                    1.0
                },
            })
            .collect::<Vec<_>>()
    };

    let reserved = categories.iter().find(|category| {
        category.name == RESERVED_CATEGORY_NAME || category.name == MANUAL_COMPACT_BUFFER_NAME
    });
    let mut squares = Vec::new();
    for category in categories.iter().filter(|category| {
        !category.is_deferred
            && category.name != "Free space"
            && category.name != RESERVED_CATEGORY_NAME
            && category.name != MANUAL_COMPACT_BUFFER_NAME
    }) {
        for square in make_squares(category) {
            if squares.len() < total_squares {
                squares.push(square);
            }
        }
    }

    let reserved_squares = reserved.map(&make_squares).unwrap_or_default();
    let free_target = total_squares.saturating_sub(reserved_squares.len());
    let free = categories
        .iter()
        .find(|category| category.name == "Free space");
    while squares.len() < free_target {
        squares.push(ContextGridSquare {
            category_name: "Free space".to_string(),
            color: theme.prompt_border,
            square_fullness: 1.0,
        });
    }
    for square in reserved_squares {
        if squares.len() < total_squares {
            squares.push(square);
        }
    }
    squares
        .chunks(grid_width)
        .take(grid_height)
        .map(<[ContextGridSquare]>::to_vec)
        .collect()
}

/// Maps to: CC `utils/analyzeContext.ts#analyzeContextUsage`.
pub fn analyze_context_usage(input: AnalyzeContextUsageInput<'_>) -> ContextData {
    let runtime_model = crate::utils::model::model::get_runtime_main_loop_model(
        input.tool_permission_context.mode,
        input.model.to_string(),
        false,
    );
    let betas = crate::utils::betas::get_model_betas(&runtime_model);
    let context_window =
        crate::utils::context::get_context_window_for_model(&runtime_model, &betas).max(1) as u64;

    // Maps to CC `utils/analyzeContext.ts:938` `getSystemPrompt(tools,
    // runtimeModel)` — the 2-arg call: CC passes nothing for
    // additionalWorkingDirectories/mcpClients here.
    let default_system_prompt =
        crate::constants::prompts::get_system_prompt(input.tools, &runtime_model, &[], &[]);
    let options = crate::utils::system_prompt::ToolUseContextOptions {
        main_loop_model: Some(runtime_model.clone()),
    };
    let effective_system_prompt = input.system_prompt_overrides.apply_with_agent(
        default_system_prompt,
        input.main_thread_agent_definition,
        None,
        Some(&options),
    );
    let system_prompt_sections = effective_system_prompt
        .iter()
        .filter(|section| {
            !section.is_empty()
                && section.as_str() != crate::constants::prompts::SYSTEM_PROMPT_DYNAMIC_BOUNDARY
        })
        .map(|section| ContextSystemPromptSectionInfo {
            name: extract_section_name(section),
            tokens: (crate::services::token_estimation::rough_token_count_estimation(section)
                as u64),
        })
        .collect::<Vec<_>>();
    let system_prompt_tokens = system_prompt_sections
        .iter()
        .map(|section| section.tokens)
        .sum::<u64>();

    let memory_files = crate::utils::claudemd::discover_claude_md_files()
        .into_iter()
        .map(|file| ContextMemoryFileInfo {
            path: file.path.display().to_string(),
            file_type: memory_type_name(file.kind).to_string(),
            tokens: (crate::services::token_estimation::rough_token_count_estimation(&file.content)
                as u64),
        })
        .collect::<Vec<_>>();
    let memory_tokens = memory_files.iter().map(|file| file.tokens).sum::<u64>();

    let mut system_tools = Vec::new();
    let mut mcp_tools = Vec::new();
    // Maps to CC `analyzeContext.ts:652-656`: `getToolPermissionContext` is the
    // caller's live accessor and `agents` is `agentInfo?.activeAgents ?? []`.
    // `allowedAgentTypes` has no key in that bag, so it stays `None`.
    let tool_prompt_options = crate::tool::ToolPromptOptions {
        tool_permission_context: input.tool_permission_context,
        tools: input.tools,
        agents: &input.agent_definitions.active_agents,
        allowed_agent_types: None,
    };
    for tool in input.tools {
        let tokens = tool_definition_tokens(tool, &tool_prompt_options);
        if tool.is_mcp {
            mcp_tools.push(ContextToolInfo {
                name: tool.name.clone(),
                server_name: tool
                    .name
                    .split("__")
                    .nth(1)
                    .unwrap_or("unknown")
                    .to_string(),
                tokens,
                // Deferred ToolSearch projection is not yet available locally;
                // active MCP tools are therefore treated as loaded.
                is_loaded: true,
            });
        } else {
            system_tools.push(ContextToolInfo {
                name: tool.name.clone(),
                server_name: String::new(),
                tokens,
                is_loaded: true,
            });
        }
    }
    system_tools.sort_by_key(|tool| std::cmp::Reverse(tool.tokens));
    mcp_tools.sort_by_key(|tool| std::cmp::Reverse(tool.tokens));
    let built_in_tool_tokens = system_tools.iter().map(|tool| tool.tokens).sum::<u64>();
    let mcp_tool_tokens = mcp_tools.iter().map(|tool| tool.tokens).sum::<u64>();

    let mut agents = input
        .agent_definitions
        .active_agents
        .iter()
        .filter(|agent| agent.source != AgentDefinitionSource::BuiltIn)
        .map(|agent| ContextAgentInfo {
            source: agent_source(agent.source),
            agent_type: agent.agent_type.clone(),
            tokens: (crate::services::token_estimation::rough_token_count_estimation(&format!(
                "{} {}",
                agent.agent_type, agent.when_to_use
            )) as u64),
        })
        .collect::<Vec<_>>();
    agents.sort_by_key(|agent| std::cmp::Reverse(agent.tokens));
    let agent_tokens = agents.iter().map(|agent| agent.tokens).sum::<u64>();

    // Maps to CC `getLimitedSkillToolCommands(getCwd())`: use the same
    // SkillTool owner predicate as the runtime registry. Filtering a complete
    // command catalog by `prompt_command.is_some()` would incorrectly include
    // `disable-model-invocation` skills and miss other valid SkillTool entries.
    let skill_commands = crate::commands::get_skill_tool_commands(input.cwd);
    let skill_frontmatter = skill_commands
        .iter()
        .map(|command| ContextSkillFrontmatterInfo {
            source: command_source(command.source),
            name: crate::commands::get_command_name(command).to_string(),
            tokens: (crate::services::token_estimation::rough_token_count_estimation(&format!(
                "{} {} {}",
                command.name,
                command.description,
                command.when_to_use.as_deref().unwrap_or_default()
            )) as u64),
        })
        .collect::<Vec<_>>();
    let skill_tokens = skill_frontmatter
        .iter()
        .map(|skill| skill.tokens)
        .sum::<u64>();
    let skills = (!skill_frontmatter.is_empty()).then_some(ContextSkillsInfo {
        total_skills: skill_frontmatter.len(),
        included_skills: skill_frontmatter.len(),
        tokens: skill_tokens,
        skill_frontmatter,
    });

    let message_breakdown = count_message_breakdown(input.messages);
    let message_tokens =
        crate::utils::tokens::token_count_with_estimation(input.messages).max(0) as u64;

    let theme = input.theme;
    let mut categories = Vec::new();
    if system_prompt_tokens > 0 {
        categories.push(ContextCategory {
            name: "System prompt".to_string(),
            tokens: system_prompt_tokens,
            color: theme.prompt_border,
            is_deferred: false,
        });
    }
    if built_in_tool_tokens > 0 {
        categories.push(ContextCategory {
            name: if crate::utils::build_profile::has_internal_capability(
                crate::utils::build_profile::InternalCapability::Context,
            ) {
                "[ANT-ONLY] System tools".to_string()
            } else {
                "System tools".to_string()
            },
            tokens: built_in_tool_tokens.saturating_sub(skill_tokens),
            color: theme.inactive,
            is_deferred: false,
        });
    }
    if mcp_tool_tokens > 0 {
        categories.push(ContextCategory {
            name: "MCP tools".to_string(),
            tokens: mcp_tool_tokens,
            color: theme.agent_cyan,
            is_deferred: false,
        });
    }
    if agent_tokens > 0 {
        categories.push(ContextCategory {
            name: "Custom agents".to_string(),
            tokens: agent_tokens,
            color: theme.permission,
            is_deferred: false,
        });
    }
    if memory_tokens > 0 {
        categories.push(ContextCategory {
            name: "Memory files".to_string(),
            tokens: memory_tokens,
            color: theme.claude,
            is_deferred: false,
        });
    }
    if skill_tokens > 0 {
        categories.push(ContextCategory {
            name: "Skills".to_string(),
            tokens: skill_tokens,
            color: theme.warning,
            is_deferred: false,
        });
    }
    if message_tokens > 0 {
        categories.push(ContextCategory {
            name: "Messages".to_string(),
            tokens: message_tokens,
            color: theme.agent_purple,
            is_deferred: false,
        });
    }

    let actual_usage = categories
        .iter()
        .filter(|category| !category.is_deferred)
        .map(|category| category.tokens)
        .sum::<u64>();
    let is_auto_compact_enabled = crate::services::compact::auto_compact::is_auto_compact_enabled();
    let auto_compact_threshold = is_auto_compact_enabled.then(|| {
        crate::services::compact::auto_compact::get_effective_context_window_size(&runtime_model)
            .saturating_sub(crate::services::compact::auto_compact::AUTOCOMPACT_BUFFER_TOKENS)
            .max(0) as u64
    });
    let reserved_tokens = if let Some(threshold) = auto_compact_threshold {
        let reserved = context_window.saturating_sub(threshold);
        categories.push(ContextCategory {
            name: RESERVED_CATEGORY_NAME.to_string(),
            tokens: reserved,
            color: theme.inactive,
            is_deferred: false,
        });
        reserved
    } else {
        let reserved =
            crate::services::compact::auto_compact::MANUAL_COMPACT_BUFFER_TOKENS.max(0) as u64;
        categories.push(ContextCategory {
            name: MANUAL_COMPACT_BUFFER_NAME.to_string(),
            tokens: reserved,
            color: theme.inactive,
            is_deferred: false,
        });
        reserved
    };
    categories.push(ContextCategory {
        name: "Free space".to_string(),
        tokens: context_window.saturating_sub(actual_usage.saturating_add(reserved_tokens)),
        color: theme.prompt_border,
        is_deferred: false,
    });

    let api_usage =
        crate::utils::tokens::get_current_usage(input.original_messages.unwrap_or(input.messages));
    let total_from_api = api_usage.as_ref().map(|usage| {
        usage
            .input_tokens
            .saturating_add(usage.cache_creation_input_tokens)
            .saturating_add(usage.cache_read_input_tokens)
    });
    let total_tokens = total_from_api.unwrap_or(actual_usage);
    let percentage = ((total_tokens as f64 / context_window as f64) * 100.0).round() as u64;
    let grid_rows = create_grid(&categories, context_window, input.terminal_width, theme);

    ContextData {
        categories,
        total_tokens,
        max_tokens: context_window,
        raw_max_tokens: context_window,
        percentage,
        grid_rows,
        model: runtime_model,
        memory_files,
        mcp_tools,
        deferred_builtin_tools: Vec::new(),
        system_tools,
        system_prompt_sections,
        agents,
        slash_commands: None,
        skills,
        auto_compact_threshold,
        is_auto_compact_enabled,
        message_breakdown: Some(message_breakdown),
        api_usage,
        collapse_status: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::TEST_ENV_LOCK;

    #[test]
    fn rough_context_estimation_uses_javascript_utf16_length() {
        assert_eq!(
            (crate::services::token_estimation::rough_token_count_estimation("abcdefgh") as u64),
            2
        );
        assert_eq!(
            (crate::services::token_estimation::rough_token_count_estimation("😀😀") as u64),
            1
        );
    }

    #[test]
    fn context_grid_matches_official_normal_and_narrow_dimensions() {
        let theme = *crate::utils::theme::current();
        let categories = vec![
            ContextCategory {
                name: "Messages".to_string(),
                tokens: 20_000,
                color: theme.agent_purple,
                is_deferred: false,
            },
            ContextCategory {
                name: "Free space".to_string(),
                tokens: 180_000,
                color: theme.prompt_border,
                is_deferred: false,
            },
        ];
        let wide = create_grid(&categories, 200_000, Some(100), theme);
        assert_eq!((wide.len(), wide[0].len()), (10, 10));
        let narrow = create_grid(&categories, 200_000, Some(79), theme);
        assert_eq!((narrow.len(), narrow[0].len()), (5, 5));
    }

    #[test]
    fn message_breakdown_pairs_tool_results_with_tool_names() {
        use crate::types::ids::ToolUseId;
        use crate::types::message::{AssistantMessage, ToolResult, UserMessage};
        let messages = vec![
            Message::Assistant(AssistantMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![AssistantContent::ToolUse(
                    crate::types::message::ToolUseBlock {
                        id: ToolUseId("tool-1".to_string()),
                        name: "Read".to_string(),
                        input: serde_json::json!({"file_path":"a.rs"}),
                    },
                )],
                model: None,
                stop_reason: None,
                usage: None,
            }),
            Message::User(UserMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![UserContent::ToolResult(ToolResult {
                    tool_use_id: ToolUseId("tool-1".to_string()),
                    content: "file contents".to_string(),
                    is_error: false,
                    content_blocks: Vec::new(),
                    tool_use_result: None,
                })],
                is_compact_summary: false,
                plan_content: None,
                image_paste_ids: None,
                is_visible_in_transcript_only: false,
                mcp_meta: None,
                source_tool_assistant_uuid: None,
                permission_mode: None,
                origin: None,
                summarize_metadata: None,
            }),
        ];
        let breakdown = count_message_breakdown(&messages);
        assert_eq!(breakdown.tool_calls_by_type[0].name, "Read");
        assert!(breakdown.tool_calls_by_type[0].call_tokens > 0);
        assert!(breakdown.tool_calls_by_type[0].result_tokens > 0);
    }

    /// CC `analyzeContext.ts:652-656` estimates each tool from `await
    /// t.prompt({ getToolPermissionContext, tools, agents })`, so the count
    /// tracks THIS turn's projection: an `Agent(<type>)` deny rule removes the
    /// type from the rendered listing (`AgentTool.tsx:359-363`) and the
    /// estimate must shrink with it.
    ///
    /// Old shape: `tool.description` — a field rendered once at schema
    /// construction with no permission context, so both contexts produced the
    /// same number and a tool built without that field (here: empty) counted
    /// as pure schema overhead.
    #[test]
    fn tool_definition_tokens_read_the_lazy_prompt_with_the_live_permission_context() {
        let _env_guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        crate::utils::process_env::remove("CLAUDE_CODE_AGENT_LIST_IN_MESSAGES");
        crate::utils::process_env::remove("CLAUDE_CODE_COORDINATOR_MODE");

        let agents = vec![
            AgentDefinition::new(
                "reviewer",
                "review code",
                AgentDefinitionSource::ProjectSettings,
            ),
            AgentDefinition::new(
                "test-runner",
                "run tests",
                AgentDefinitionSource::ProjectSettings,
            ),
        ];
        let tool = Tool {
            name: "Agent".to_string(),
            description: String::new(),
            input_schema: serde_json::json!({"type": "object"}),
            ..Default::default()
        };
        let tools = [tool.clone()];

        let open = ToolPermissionContext::default();
        let open_tokens = tool_definition_tokens(
            &tool,
            &crate::tool::ToolPromptOptions {
                tool_permission_context: &open,
                tools: &tools,
                agents: &agents,
                allowed_agent_types: None,
            },
        );

        let mut denied = ToolPermissionContext::default();
        denied.always_deny_rules.insert(
            crate::types::permissions::PermissionRuleSource::LocalSettings,
            vec![crate::types::permissions::PermissionRuleValue::new(
                "Agent",
                Some("reviewer".to_string()),
            )],
        );
        let denied_tokens = tool_definition_tokens(
            &tool,
            &crate::tool::ToolPromptOptions {
                tool_permission_context: &denied,
                tools: &tools,
                agents: &agents,
                allowed_agent_types: None,
            },
        );

        assert!(
            denied_tokens < open_tokens,
            "a denied agent type must shrink the estimate: {denied_tokens} vs {open_tokens}"
        );
        // The empty carried field is not what is being counted.
        let empty_field_only = estimate_json(&serde_json::json!({
            "name": tool.name,
            "description": tool.description,
            "input_schema": tool.input_schema,
        }));
        assert!(empty_field_only < denied_tokens);
    }
}
