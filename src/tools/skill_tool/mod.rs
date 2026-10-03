//! Skill tool metadata and UI.
//!
//! Maps to:
//! - CC `tools/SkillTool/SkillTool.ts`
//! - CC `tools/SkillTool/constants.ts`
//! - CC `tools/SkillTool/prompt.ts`
//! - CC `tools/SkillTool/UI.tsx`
//!
//! The inline skill path loads local filesystem skills, materializes the prompt
//! (arguments/env substitutions plus `!` shell command expansion), and injects
//! it as `new_messages`. Forked skills (`context: fork`) run via `run_agent`
//! (CC `executeForkedSkill` / `prepareForkedCommandContext`).

pub mod constants;
pub mod prompt;
pub mod ui;

/// Maps to: CC `SkillTool.ts:291-298` `inputSchema`.
pub fn input_schema() -> &'static crate::utils::zod::Schema {
    static SCHEMA: std::sync::OnceLock<crate::utils::zod::Schema> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| {
        use crate::utils::zod;
        zod::object(vec![
            (
                "skill",
                zod::string()
                    .describe("The skill name. E.g., \"commit\", \"review-pr\", or \"pdf\""),
            ),
            (
                "args",
                zod::string()
                    .optional()
                    .describe("Optional arguments for the skill"),
            ),
        ])
    })
}

pub fn skill_tool_schema() -> crate::types::tools::Tool {
    crate::types::tools::Tool {
        name: constants::SKILL_TOOL_NAME.to_string(),
        description: prompt::get_prompt(),
        input_schema: crate::utils::zod_to_json_schema::zod_to_json_schema(input_schema()),
        ..Default::default()
    }
}

/// CC `tools/SkillTool/SkillTool.ts` `export type Output` (:329) — the
/// inline/forked union (outputSchema :301-326). Each `call()` construction
/// maps to exactly one branch with only that branch's fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Output {
    /// Inline skills (`SkillTool.ts:768-773`, `:1102`). `status` is the
    /// optional `'inline'` literal — present only on the MCP-prompt path.
    Inline {
        success: bool,
        command_name: String,
        allowed_tools: Option<Vec<String>>,
        model: Option<String>,
        status: Option<String>,
        /// Rust transport for CC's sibling `ToolResult.contextModifier`
        /// (`SkillTool.ts:775-839`). CC returns the closure next to `data`;
        /// this port cannot add a field to `crate::tool::ToolResult` without
        /// rewriting every tool's construction site, so the one tool that
        /// produces a modifier hands it out on its own typed output and
        /// `services/tools/tool_execution.rs` picks it up at CC's read
        /// position (`toolExecution.ts:1400`). Not part of the outputSchema:
        /// `ui::output_to_value` never serializes it and `ui::parse_output`
        /// (which reads the recorded `toolUseResult` back) always yields
        /// `None`, so the wire shape stays exactly CC's.
        context_modifier: Option<SkillContextModifier>,
    },
    /// Forked skills (`SkillTool.ts:277-283`).
    Forked {
        success: bool,
        command_name: String,
        agent_id: String,
        result: String,
    },
}

/// Maps to: CC `tools/SkillTool/SkillTool.ts:775-839` — the `contextModifier`
/// closure returned alongside an inline skill's `data`.
///
/// The three fields are exactly the closure's captures (`:651-653`):
/// `allowedTools` is `processedCommand.allowedTools`, i.e. the
/// `parseToolListFromCLI`-expanded specs (`processSlashCommand.tsx:1206-1208`);
/// `model` is the skill's `model:` frontmatter; `effort` is its `effort:`
/// frontmatter.
///
/// This is a THIRD widening rule, not either of the other two:
/// - fork path `createGetAppStateWithAllowedTools` (`forkedAgent.ts:147-171`):
///   early-return when the list is empty, then UNION — scoped to the forked
///   agent's own context.
/// - inline `!` blocks (`loadSkillsDir.ts:377-392`): unconditional REPLACE of
///   the `command` rule source, scoped to this skill's own shell expansion.
/// - this one: a UNION applied AFTER the skill's own execution, to the context
///   threaded into every subsequent tool use of the query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillContextModifier {
    pub allowed_tools: Vec<String>,
    pub model: Option<String>,
    pub effort: Option<crate::utils::effort::EffortValue>,
}

impl SkillContextModifier {
    /// CC `SkillTool.ts:766-773` returns `contextModifier` unconditionally, but
    /// with all three captures empty the closure is the identity function.
    /// Rust models that as `None` so the carrier stays absent instead of
    /// travelling as a no-op through the orchestration layer.
    pub(crate) fn new(
        allowed_tools: Vec<String>,
        model: Option<String>,
        effort: Option<crate::utils::effort::EffortValue>,
    ) -> Option<Self> {
        let modifier = Self {
            allowed_tools,
            model,
            effort,
        };
        (!modifier.is_identity()).then_some(modifier)
    }

    fn is_identity(&self) -> bool {
        self.allowed_tools.is_empty()
            && !self.model.as_deref().is_some_and(|model| !model.is_empty())
            && self.effort.is_none()
    }

    /// Maps to: CC `SkillTool.ts:776-838` — the closure body, in order.
    ///
    /// CC chains `getAppState` wrappers and spreads `options`; this port's
    /// `ToolUseContext` carries `tool_permission_context`, `main_loop_model`
    /// and `effort_value` as snapshot fields on the context itself, and the
    /// permission engine reads `context.tool_permission_context`
    /// (`hooks/use_can_use_tool.rs:372`) the way CC's reads
    /// `context.getAppState().toolPermissionContext`
    /// (`utils/permissions/permissions.ts:1167-1171`). So the wrapper chain
    /// becomes a direct write to those fields.
    ///
    /// The write deliberately does NOT go through
    /// `ToolUseContext::update_permission_context`: that also pushes the
    /// snapshot into the `AppStore`, and CC's modifier never calls
    /// `setAppState` — `REPL.tsx:3688-3696` states the rule for the sibling
    /// effort override ("wrapping getAppState keeps the override out of the
    /// global store so background agents and UI subscribers never see it").
    pub fn modify_context(&self, context: &mut crate::tool::ToolUseContext) {
        // `if (allowedTools.length > 0)` — union into `alwaysAllowRules.command`.
        // CC dedupes with `new Set([...existing, ...allowedTools])` over rule
        // STRINGS; this port stores parsed `PermissionRuleValue`s, so the same
        // dedupe runs over the parsed values.
        if !self.allowed_tools.is_empty() {
            let rules = self
                .allowed_tools
                .iter()
                .map(|spec| {
                    crate::utils::permissions::permission_rule_parser::permission_rule_value_from_string(
                        spec,
                    )
                })
                .collect::<Vec<_>>();
            let mut permission = context.tool_permission_context.clone();
            let entry = permission
                .always_allow_rules
                .entry(crate::types::permissions::PermissionRuleSource::Command)
                .or_default();
            for rule in rules {
                if !entry.contains(&rule) {
                    entry.push(rule);
                }
            }
            context.tool_permission_context = permission;
        }

        // `if (model)` — JS truthiness, so an empty `model:` is falsy and does
        // not override. CC resolves against `ctx.options.mainLoopModel`, the
        // modifier's own argument; the allowed-tools step above never touches
        // `options`, so reading the running context is the same value.
        if let Some(model) = self.model.as_deref().filter(|model| !model.is_empty()) {
            context.main_loop_model =
                Some(crate::utils::model::model::resolve_skill_model_override(
                    model,
                    context.main_loop_model.as_deref().unwrap_or_default(),
                ));
        }

        // `if (effort !== undefined)` — presence, not truthiness.
        if let Some(effort) = self.effort.clone() {
            context.effort_value = Some(effort);
        }
    }
}

/// Behavioral half of CC `SkillTool` — dispatched via `crate::tool::ToolCall`.
pub(crate) struct SkillTool;

fn skill_tool_error(content: impl Into<String>) -> crate::tool::ToolResult {
    crate::tool::ToolResult {
        data: crate::tool::ToolOutput::Composed {
            content: format!("<tool_use_error>{}</tool_use_error>", content.into()),
            status: crate::types::message::ToolResultStatus::Error,
        },
        new_messages: Vec::new(),
    }
}

fn normalize_skill_name(value: Option<&str>) -> Option<String> {
    let trimmed = value?.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.strip_prefix('/').unwrap_or(trimmed).to_string())
}

/// Maps to CC `SkillTool.ts:451-467` — the rule matcher used by both the deny
/// and allow loops. Both sides are normalized by stripping a leading slash.
fn skill_rule_matches(rule_content: &str, command_name: &str) -> bool {
    let normalized_rule = rule_content.strip_prefix('/').unwrap_or(rule_content);
    if normalized_rule == command_name {
        return true;
    }
    match normalized_rule.strip_suffix(":*") {
        Some(prefix) => command_name.starts_with(prefix),
        None => false,
    }
}

/// Maps to CC `SkillTool.ts:875-933` `SAFE_SKILL_PROPERTIES` /
/// `skillHasOnlySafeProperties(command)`.
///
/// CC enumerates the command object's own keys and asks whether any key
/// outside the allowlist carries a meaningful value. The Command projection's
/// represented unsafe prompt fields are the generic `Command.allowed_tools`
/// and hooks in its optional captured SkillCommand payload. Builtin prompt
/// callbacks do not require that payload, but their tool grants still count.
///
/// `allowedTools` (:320) and `hooks` (:342) really are own keys of CC's command
/// object that the allowlist deliberately omits — a skill that grants tools or
/// installs session hooks must be confirmed. CC's "meaningful value" test skips
/// empty containers (:920-929), so an empty list or `hooks: {}` stays safe.
///
/// `shell` is NOT one of them, and the earlier deviation that treated it as one
/// is withdrawn. CC captures `shell` in the `getPromptForCommand` closure and
/// never assigns it to the command object —
/// `ast-grep --lang ts -p 'return { $$$ } satisfies Command'
/// src/skills/loadSkillsDir.ts` yields the one literal at :317-400, whose keys
/// include `allowedTools` and `hooks` but not `shell` — so a `shell:`-only skill
/// is auto-allowed there and asking here was a visible 1:1 break. It also bought
/// nothing: `FrontmatterShell` is the closed pair `bash | powershell`
/// (`frontmatterParser.ts:339`), it names an interpreter rather than a program,
/// and every `!` block that interpreter runs is itself permission-checked as
/// Bash/PowerShell against the `command` source this skill REPLACED with its own
/// `allowed-tools` (`loadSkillsDir.ts:377-392`) — empty, for a skill that
/// declares only `shell:`. So the prompt could not gate any execution the `!`
/// block would not gate anyway, while `shell: powershell` — the ordinary Windows
/// configuration — paid a confirmation on every invocation.
fn skill_has_only_safe_properties(command: &crate::commands::Command) -> bool {
    command.availability.is_empty()
        && command.call.is_none()
        && !command.supports_non_interactive
        && command.allowed_tools.is_empty()
        && command
            .prompt_command
            .as_ref()
            .is_none_or(|skill| skill.hooks.as_ref().is_none_or(|hooks| hooks.is_empty()))
}

/// Maps to: CC `SkillTool.ts:81-95#getAllCommands`.
/// Execution resolves the full command catalog; the discovery listing has a
/// different filter and must not decide whether a command can be invoked.
fn get_all_commands(context: &crate::tool::ToolUseContext) -> Vec<crate::commands::Command> {
    let cwd = crate::bootstrap::state::get_original_cwd();
    let mut commands = crate::commands::get_commands(&cwd);
    let state = context.get_app_state();
    let mcp_commands = state
        .as_ref()
        .map_or(&context.mcp_state.commands, |state| &state.mcp.commands);
    for command in mcp_commands.iter().filter(|command| {
        command.kind == crate::commands::CommandKind::Prompt
            && command.loaded_from == Some(crate::skills::load_skills_dir::SkillLoadedFrom::Mcp)
    }) {
        if !commands
            .iter()
            .any(|existing| existing.name == command.name)
        {
            commands.push(command.clone());
        }
    }
    commands
}

/// Maps to CC `SkillTool.ts:209-214` command effort merge.
fn forked_skill_agent_definition(
    command: &crate::skills::load_skills_dir::SkillCommand,
    base_agent: &crate::tools::agent_tool::load_agents_dir::AgentDefinition,
) -> crate::tools::agent_tool::load_agents_dir::AgentDefinition {
    let mut agent = base_agent.clone();
    if let Some(effort) = command.effort.clone() {
        agent.effort = Some(effort);
    }
    agent
}

/// Maps to CC `executeForkedSkill` (`SkillTool.ts`:122).
///
/// #156: no `canUseTool`/`parentMessage` params — CC hands `canUseTool` to
/// `runAgent` (`SkillTool.ts:223-234`) only so `query()` can thread it to the
/// child's tool execution; this port's equivalent rides the cloned context
/// (`ToolUseContext.can_use_tool` + `interactive_permission_sink`), see the
/// RunAgentInput note in `run_agent.rs`.
async fn execute_forked_skill(
    command: &crate::skills::load_skills_dir::SkillCommand,
    command_name: &str,
    skill_args: Option<&str>,
    context: &crate::tool::ToolUseContext,
    on_progress: Option<crate::tool::ToolCallProgressFn<'_>>,
    parent_tool_use_id: Option<&str>,
) -> crate::tool::ToolResult {
    use crate::tools::agent_tool::run_agent::{self, RunAgentInput, RunAgentOutcome};
    use crate::utils::forked_agent::{extract_result_text, prepare_forked_command_context};

    let prepared = match prepare_forked_command_context(command, skill_args, context) {
        Ok(prepared) => prepared,
        Err(message) => return skill_tool_error(message),
    };

    let agent_definition = forked_skill_agent_definition(command, &prepared.base_agent);
    // Maps to: CC `tools/SkillTool/SkillTool.ts` passing `{ ...context, getAppState:
    // modifiedGetAppState }` into `runAgent`.
    let mut forked_context = context
        .clone()
        .with_get_app_state_override(prepared.modified_get_app_state.clone());
    if let Some(permission_context) = forked_context
        .get_app_state()
        .map(|state| (*state.tool_permission_context).clone())
    {
        // Rust runAgent reads its local permission carrier before constructing
        // the subagent context, so hydrate it from the canonical projection.
        forked_context.tool_permission_context = permission_context;
    }

    let agent_id = run_agent::create_agent_id(None);
    crate::bootstrap::state::add_invoked_skill(
        &command.name,
        command.file_path.display().to_string(),
        &prepared.skill_content,
        Some(&agent_id),
    );
    let run_result = run_agent::run_agent(RunAgentInput {
        agent_definition: &agent_definition,
        prompt: &prepared.skill_content,
        description: Some(command_name),
        model_override: command.model.as_deref(),
        context: &forked_context,
        query_source: crate::constants::query_source::QuerySource::AgentCustom,
        // Maps to CC `SkillTool.ts:223-234`: the forked skill agent runs
        // `isAsync: false` with the PARENT context's tool pool
        // (`availableTools: context.options.tools`), not a recomputed worker
        // pool.
        is_async: false,
        can_show_permission_prompts: None,
        available_tools: Some(forked_context.tools.clone()),
        // CC `SkillTool.ts:223-236` omits `forkContextMessages` entirely, so it
        // is `undefined` there — a skill agent gets no parent conversation.
        fork_context_messages: None,
        preserve_tool_use_results: false,
        transcript_subdir: None,
        // CC `SkillTool.ts:223-234` passes no `override.abortController`; the
        // sync arm then shares the parent's controller (runAgent.ts:524-528).
        r#override: run_agent::RunAgentOverride {
            agent_id: Some(agent_id.as_str()),
            ..Default::default()
        },
        use_exact_tools: false,
        allowed_tools: None,
        worktree_path: None,
        parent_tool_use_id,
        on_progress,
        background_task_id: None,
        content_replacement_state: None,
        background_signal: None,
        on_message: None,
        prompt_messages: Some(prepared.prompt_messages),
    })
    .await;
    // Forked-skill content is scoped to the ephemeral child agent and must not
    // leak after that execution ends.
    crate::bootstrap::state::clear_invoked_skills_for_agent(&agent_id);

    match run_result {
        Ok(RunAgentOutcome::Completed(completed)) => {
            let result_text = if completed.content.is_empty() {
                extract_result_text(&completed.messages, "Skill execution completed")
            } else {
                completed.content.join("\n")
            };
            crate::tool::ToolResult {
                data: crate::tool::ToolOutput::Skill(Output::Forked {
                    success: true,
                    command_name: command.name.clone(),
                    agent_id: completed.agent_id,
                    result: result_text,
                }),
                new_messages: Vec::new(),
            }
        }
        Ok(RunAgentOutcome::Backgrounded(_)) => skill_tool_error(format!(
            "Skill {command_name} forked execution was backgrounded unexpectedly"
        )),
        Err(error) => skill_tool_error(format!(
            "Skill {command_name} forked execution failed: {error}"
        )),
    }
}

// Existing L1 transport seam: MCP getPrompt is asynchronous while Command's
// getPromptForCommand is a synchronous function pointer. This separate path
// predates the shared inline repair; it is not CC's remote-canonical branch.
async fn execute_mcp_skill(
    command_name: &str,
    skill_args: Option<&str>,
    context: &crate::tool::ToolUseContext,
) -> Option<crate::tool::ToolResult> {
    let state = context.get_app_state();
    let mcp_state = state
        .as_ref()
        .map_or(&context.mcp_state, |state| &state.mcp);
    let registry_command = crate::commands::find_command(command_name, &mcp_state.commands)?;
    if registry_command.kind != crate::commands::CommandKind::Prompt
        || registry_command.loaded_from
            != Some(crate::skills::load_skills_dir::SkillLoadedFrom::Mcp)
    {
        return None;
    }
    if registry_command.disable_model_invocation {
        return Some(skill_tool_error(format!(
            "Skill {command_name} cannot be used with Skill tool due to disable-model-invocation"
        )));
    }
    let pair = mcp_state.clients.iter().find_map(|server| {
        server.prompts.iter().find_map(|prompt| {
            (crate::services::mcp::client::mcp_prompt_command_name(
                &server.client.name,
                &prompt.name,
            ) == registry_command.name)
                .then_some((server, prompt))
        })
    });
    let Some((server, prompt)) = pair else {
        return Some(skill_tool_error(format!(
            "MCP skill {command_name} is no longer available"
        )));
    };
    let blocks = match crate::services::mcp::client::get_mcp_prompt_for_command(
        &server.client.name,
        &prompt.name,
        &prompt.arg_names,
        skill_args.unwrap_or_default(),
    )
    .await
    {
        Ok(blocks) => blocks,
        Err(error) => {
            return Some(skill_tool_error(format!(
                "MCP skill {command_name} failed: {error}"
            )));
        }
    };
    let prompt_text = crate::services::mcp::client::mcp_prompt_content_blocks_summary(&blocks);
    crate::bootstrap::state::add_invoked_skill(
        registry_command.name.as_ref(),
        format!("mcp:{}:{}", server.client.name, prompt.name),
        &prompt_text,
        context.agent_id.as_deref(),
    );
    Some(crate::tool::ToolResult {
        // Existing MCP result seam, unchanged here. CC MCP skills use the
        // normal processPromptSlashCommand result at SkillTool.ts:768-839;
        // :1102 belongs to remote canonical skills, not this transport.
        data: crate::tool::ToolOutput::Skill(Output::Inline {
            success: true,
            command_name: registry_command.name.to_string(),
            allowed_tools: None,
            model: None,
            status: Some("inline".to_string()),
            context_modifier: None,
        }),
        new_messages: vec![crate::types::message::Message::User(
            crate::types::message::UserMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![crate::types::message::UserContent::MetaText(prompt_text)],
                is_compact_summary: false,
                plan_content: None,
                image_paste_ids: None,
                is_visible_in_transcript_only: false,
                mcp_meta: None,
                source_tool_assistant_uuid: None,
                permission_mode: None,
                origin: None,
                summarize_metadata: None,
            },
        )],
    })
}

impl crate::tool::ToolCall for SkillTool {
    fn name(&self) -> &'static str {
        "Skill"
    }

    /// Maps to: CC `SkillTool.ts:344` `prompt: async () =>
    /// getPrompt(getProjectRoot())` — the memoized `getPrompt`
    /// (`SkillTool/prompt.ts:173`) ignores its `_cwd` argument and returns a
    /// constant, so the zero-arg Rust builder is the same value.
    fn prompt(
        &self,
        _tool: &crate::types::tools::Tool,
        _options: &crate::tool::ToolPromptOptions<'_>,
    ) -> String {
        prompt::get_prompt()
    }

    /// Maps to: CC `SkillTool.ts:342`
    /// ``description: async ({ skill }) => `Execute skill: ${skill}` ``.
    ///
    /// This is the TOOL describing itself; `useCanUseTool.tsx:138-143` stamps it
    /// onto `ToolUseConfirm.description`. It is deliberately NOT the string the
    /// Skill dialog's dim line shows — that one is the SKILL's own description,
    /// carried by `PermissionAskDecision.metadata.command`
    /// (`SkillPermissionRequest.tsx:47-52,236`). CC uses the raw `skill` here,
    /// leading slash and all, not the normalized command name.
    fn description(&self, args: &serde_json::Value) -> String {
        format!(
            "Execute skill: {}",
            args.get("skill")
                .and_then(|value| value.as_str())
                .unwrap_or_default()
        )
    }

    /// Maps to: CC `SkillTool.ts:352` `toAutoClassifierInput({ skill })`.
    fn to_auto_classifier_input(&self, args: &serde_json::Value) -> String {
        args.get("skill")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string()
    }

    /// Maps to: CC `SkillTool.ts:354-430` `validateInput({ skill }, context)`.
    /// The remote-canonical branch (:377-396) is ant-only experimental and has
    /// no counterpart here.
    fn validate_input(
        &self,
        args: &serde_json::Value,
        context: &crate::tool::ToolUseContext,
    ) -> crate::tool::ValidationResult {
        use crate::tool::ValidationResult;

        let skill = args
            .get("skill")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        let Some(command_name) = normalize_skill_name(Some(skill)) else {
            return ValidationResult::error(format!("Invalid skill format: {skill}"), 1);
        };

        let commands = get_all_commands(context);
        let Some(command) = crate::commands::find_command(&command_name, &commands) else {
            return ValidationResult::error(format!("Unknown skill: {command_name}"), 2);
        };
        if command.disable_model_invocation {
            return ValidationResult::error(
                format!(
                    "Skill {command_name} cannot be used with {} tool due to disable-model-invocation",
                    constants::SKILL_TOOL_NAME
                ),
                4,
            );
        }
        if command.kind != crate::commands::CommandKind::Prompt {
            return ValidationResult::error(
                format!("Skill {command_name} is not a prompt-based skill"),
                5,
            );
        }
        ValidationResult::Ok
    }

    /// Maps to: CC `SkillTool.ts:432-578` `checkPermissions({ skill, args })`.
    /// Deny rules win, then allow rules, then the safe-properties auto-allow,
    /// and anything left prompts with exact + prefix rule suggestions.
    fn check_permissions(
        &self,
        args: &serde_json::Value,
        context: &crate::tool::ToolUseContext,
    ) -> crate::utils::permissions::permission_result::PermissionResult {
        use crate::types::permissions::{
            PermissionBehavior, PermissionRuleValue, PermissionUpdate, PermissionUpdateDestination,
        };
        use crate::utils::permissions::permission_result::{
            PermissionDecisionReason, PermissionResult,
        };

        let command_name = normalize_skill_name(args.get("skill").and_then(|value| value.as_str()))
            .unwrap_or_default();
        let commands = get_all_commands(context);
        let command = crate::commands::find_command(&command_name, &commands);

        for behavior in [PermissionBehavior::Deny, PermissionBehavior::Allow] {
            let rules = crate::utils::permissions::permissions::get_rule_by_contents_for_tool_name(
                &context.tool_permission_context,
                constants::SKILL_TOOL_NAME,
                behavior,
            );
            for (rule_content, rule) in rules {
                if !skill_rule_matches(&rule_content, &command_name) {
                    continue;
                }
                return match behavior {
                    PermissionBehavior::Deny => PermissionResult::Deny {
                        message: "Skill execution blocked by permission rules".to_string(),
                        decision_reason: PermissionDecisionReason::Rule { rule },
                        tool_use_id: None,
                    },
                    _ => PermissionResult::Allow {
                        updated_input: Some(args.clone()),
                        user_modified: None,
                        decision_reason: Some(PermissionDecisionReason::Rule { rule }),
                        tool_use_id: None,
                        accept_feedback: None,
                        content_blocks: Vec::new(),
                    },
                };
            }
        }

        if command.is_some_and(|command| {
            command.kind == crate::commands::CommandKind::Prompt
                && skill_has_only_safe_properties(command)
        }) {
            return PermissionResult::Allow {
                updated_input: Some(args.clone()),
                user_modified: None,
                decision_reason: None,
                tool_use_id: None,
                accept_feedback: None,
                content_blocks: Vec::new(),
            };
        }

        let suggestions = [command_name.clone(), format!("{command_name}:*")]
            .into_iter()
            .map(|rule_content| PermissionUpdate::AddRules {
                destination: PermissionUpdateDestination::LocalSettings,
                behavior: PermissionBehavior::Allow,
                rules: vec![PermissionRuleValue::new(
                    constants::SKILL_TOOL_NAME,
                    Some(rule_content),
                )],
            })
            .collect();
        PermissionResult::Ask {
            message: format!("Execute skill: {command_name}"),
            updated_input: None,
            decision_reason: None,
            suggestions,
            blocked_path: None,
            // Maps to: CC `SkillTool.ts:576`
            // `metadata: commandObj ? { command: commandObj } : undefined` —
            // `commandObj` is the resolved command looked up at `:446-447`
            // (`findCommand(commandName, await getAllCommands(context))`), i.e.
            // the same `command` this function already holds. CC hands over the
            // WHOLE Command object; `PermissionCommandMetadata`
            // (`types/permissions.ts:157-162`) names only `name`/`description`
            // and keeps the rest behind an index signature.
            //
            // `SkillPermissionRequest.tsx:236` renders `commandObj?.description`,
            // which is the SKILL's own description — not
            // `toolUseConfirm.description` (`SkillTool.ts:342`
            // `Execute skill: ${skill}`) and not the decision's `message`.
            metadata: command.as_ref().map(|command| {
                crate::utils::permissions::permission_result::PermissionMetadata::Command {
                    command:
                        crate::utils::permissions::permission_result::PermissionCommandMetadata {
                            name: command.name.to_string(),
                            description: Some(
                                crate::commands::command_description(command).into_owned(),
                            ),
                            extra: std::collections::BTreeMap::new(),
                        },
                }
            }),
            is_bash_security_check_for_misparsing: false,
            pending_classifier_check: None,
            content_blocks: Vec::new(),
        }
    }

    fn call<'a>(
        &'a self,
        args: &'a serde_json::Value,
        request: &'a crate::types::permissions::PermissionRequest,
        context: &'a crate::tool::ToolUseContext,
        _can_use_tool: Option<crate::tool::CanUseToolFn<'a>>,
        _parent_message: Option<&'a crate::types::message::AssistantMessage>,
        on_progress: Option<crate::tool::ToolCallProgressFn<'a>>,
    ) -> futures::future::BoxFuture<'a, crate::tool::ToolResult> {
        Box::pin(async move {
            let Some(command_name) =
                normalize_skill_name(args.get("skill").and_then(|value| value.as_str()))
            else {
                return skill_tool_error("Invalid skill format");
            };
            let skill_args = args.get("args").and_then(|value| value.as_str());
            let commands = get_all_commands(context);
            let Some(command) = crate::commands::find_command(&command_name, &commands) else {
                return skill_tool_error(format!("Unknown skill: {command_name}"));
            };
            if command.disable_model_invocation {
                return skill_tool_error(format!(
                    "Skill {command_name} cannot be used with Skill tool due to disable-model-invocation"
                ));
            }
            if let Some(skill) = command.prompt_command.as_deref() {
                if skill.execution_context
                    == crate::skills::load_skills_dir::SkillExecutionContext::Fork
                {
                    return execute_forked_skill(
                        skill,
                        &command_name,
                        skill_args,
                        context,
                        on_progress,
                        Some(request.tool_use_id.as_str()),
                    )
                    .await;
                }
            }
            // Existing MCP async transport seam: its descriptor has no sync
            // getPromptForCommand callback. Candidate eligibility still comes
            // from getAllCommands, never from plain MCP prompts or discovery.
            if command.loaded_from == Some(crate::skills::load_skills_dir::SkillLoadedFrom::Mcp)
                && command.get_prompt_for_command.is_none()
            {
                return execute_mcp_skill(&command_name, skill_args, context)
                    .await
                    .unwrap_or_else(|| {
                        skill_tool_error(format!("MCP skill {command_name} is no longer available"))
                    });
            }
            // CC SkillTool.ts:633-644,754-759: this shared owner performs
            // callback -> hooks -> invocation -> metadata/meta/permissions.
            // Do not independently repeat any of those side effects here.
            let processed = match crate::utils::process_user_input::process_slash_command::process_prompt_slash_command(
                &command_name, skill_args.unwrap_or_default(), &commands, context, Vec::new(),
            ) {
                Ok(processed) => processed,
                Err(error) => return skill_tool_error(error.to_string()),
            };
            if !processed.should_query {
                return skill_tool_error("Command processing failed");
            }
            let allowed_tools = processed.allowed_tools.unwrap_or_default();
            let model = command
                .prompt_command
                .as_ref()
                .and_then(|skill| skill.model.clone());
            let effort = command
                .prompt_command
                .as_ref()
                .and_then(|skill| skill.effort.clone());
            // CC SkillTool.ts:734-752 drops only the command display row.
            // Rust's existing Meta* carrier distinguishes callback content
            // from that plain string metadata; attachments remain model-visible.
            let new_messages = processed
                .messages
                .into_iter()
                .filter_map(|row| {
                    use crate::types::message::{Message, RenderableMessageKind, UserContent};
                    match row.kind {
                        RenderableMessageKind::User { message } => {
                            if matches!(message.content.as_slice(), [UserContent::Text(text)]
                            if text.contains("<command-message>"))
                            {
                                None
                            } else {
                                Some(Message::User(message))
                            }
                        }
                        RenderableMessageKind::Attachment(attachment) => Some(Message::Attachment(
                            crate::types::message::AttachmentMessage {
                                uuid: row.uuid,
                                timestamp: chrono::Utc::now(),
                                attachment,
                                wire_payload: None,
                            },
                        )),
                        RenderableMessageKind::System(message) => Some(Message::System(message)),
                        _ => None,
                    }
                })
                .collect();
            crate::tool::ToolResult {
                data: crate::tool::ToolOutput::Skill(Output::Inline {
                    success: true,
                    command_name,
                    allowed_tools: (!allowed_tools.is_empty()).then(|| allowed_tools.clone()),
                    model: model.clone(),
                    status: None,
                    context_modifier: SkillContextModifier::new(allowed_tools, model, effort),
                }),
                new_messages,
            }
        })
    }

    fn map_tool_result_to_tool_result_block_param(
        &self,
        data: &crate::tool::ToolOutput,
        _tool_use_id: &str,
    ) -> (String, crate::types::message::ToolResultStatus) {
        match data {
            crate::tool::ToolOutput::Skill(Output::Forked {
                command_name,
                result,
                ..
            }) => (
                format!(
                    "Skill \"{command_name}\" completed (forked execution).\n\nResult:\n{result}"
                ),
                crate::types::message::ToolResultStatus::Success,
            ),
            crate::tool::ToolOutput::Skill(Output::Inline { command_name, .. }) => (
                format!("Launching skill: {command_name}"),
                crate::types::message::ToolResultStatus::Success,
            ),
            crate::tool::ToolOutput::Composed {
                content, status, ..
            } => (content.clone(), *status),
            _ => (
                "<tool_use_error>tool output variant not handled by Skill result mapper</tool_use_error>"
                    .to_string(),
                crate::types::message::ToolResultStatus::Error,
            ),
        }
    }

    /// Maps to: CC recording SkillTool's `Output` union as the message's
    /// `toolUseResult`.
    fn tool_use_result(&self, data: &crate::tool::ToolOutput) -> Option<serde_json::Value> {
        match data {
            crate::tool::ToolOutput::Skill(output) => Some(ui::output_to_value(output)),
            crate::tool::ToolOutput::Composed {
                content,
                status: crate::types::message::ToolResultStatus::Error,
                ..
            } => {
                let message = crate::utils::messages::extract_tag(content, "tool_use_error")
                    .unwrap_or_else(|| content.clone());
                Some(serde_json::Value::String(message))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    fn context_with_general_purpose_agent() -> crate::tool::ToolUseContext {
        let agent = crate::tools::agent_tool::load_agents_dir::AgentDefinition::new(
            "general-purpose",
            "General agent",
            crate::tools::agent_tool::load_agents_dir::AgentDefinitionSource::BuiltIn,
        );
        crate::tool::ToolUseContext::default().with_agent_definitions(std::sync::Arc::new(
            crate::tools::agent_tool::load_agents_dir::AgentDefinitionsResult {
                active_agents: vec![agent.clone()],
                all_agents: vec![agent],
                failed_files: Vec::new(),
                allowed_agent_types: None,
            },
        ))
    }

    #[test]
    fn skill_tool_schema_matches_official_input_shape() {
        let schema = skill_tool_schema();
        assert_eq!(schema.name, "Skill");
        assert_eq!(
            schema.input_schema["required"],
            serde_json::json!(["skill"])
        );
        assert!(schema.input_schema["properties"].get("args").is_some());
        assert!(
            schema.input_schema["properties"]
                .get("command_name")
                .is_none()
        );
        assert!(schema.description.contains("How to invoke"));
    }

    #[test]
    fn skill_execution_matches_official_builtin_prompt_pipeline() {
        use crate::tool::ToolCall;
        use crate::types::message::{Message, UserContent};
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "0");
        let mut context = crate::tool::ToolUseContext::default();
        context.agent_id = Some(format!("skill-builtin-{}", uuid::Uuid::new_v4()));
        let args = serde_json::json!({"skill": "/init"});
        assert!(SkillTool.validate_input(&args, &context).is_ok());
        assert!(matches!(
            SkillTool.check_permissions(&args, &context),
            crate::utils::permissions::permission_result::PermissionResult::Allow { .. }
        ));
        let listed =
            crate::commands::get_skill_tool_commands(&crate::bootstrap::state::get_original_cwd());
        assert!(!listed.iter().any(|command| command.name == "init"
            && command.source == crate::commands::CommandSource::Builtin));
        let request = crate::utils::permissions::permissions::mock_permission_request_with_input(
            "perm-builtin",
            "toolu_builtin",
            "Skill",
            "init",
            args.clone(),
            crate::types::permissions::PermissionMode::Default,
        );
        let result = futures::executor::block_on(
            SkillTool.call(&args, &request, &context, None, None, None),
        );
        assert!(
            matches!(&result.data, crate::tool::ToolOutput::Skill(Output::Inline {
            success: true, command_name, allowed_tools: None, model: None, status: None, ..
        }) if command_name == "init")
        );
        let expected = crate::commands::init::get_prompt_for_command();
        assert_eq!(result.new_messages.len(), 2);
        assert!(matches!(&result.new_messages[0], Message::User(user)
            if matches!(user.content.as_slice(), [UserContent::MetaText(text)] if text == expected)));
        assert!(
            matches!(&result.new_messages[1], Message::Attachment(attachment)
            if matches!(&attachment.attachment, crate::utils::attachments::Attachment::CommandPermissions {
                allowed_tools, model: None,
            } if allowed_tools.is_empty()))
        );
        let invoked =
            crate::bootstrap::state::get_invoked_skills_for_agent(context.agent_id.as_deref());
        assert_eq!(invoked.len(), 1);
        assert_eq!(invoked[0].skill_path, "builtin:init");
        assert_eq!(invoked[0].content, expected);
        crate::bootstrap::state::clear_invoked_skills_for_agent(
            context.agent_id.as_deref().unwrap(),
        );
    }

    #[test]
    fn skill_execution_matches_official_catalog_validation_boundaries() {
        use crate::tool::{ToolCall, ValidationResult};
        let mut context = crate::tool::ToolUseContext::default();
        let mut disabled = crate::commands::Command::from_skill(skill_fixture(None));
        disabled.name = "skill-disabled-probe".into();
        disabled.loaded_from = Some(crate::skills::load_skills_dir::SkillLoadedFrom::Mcp);
        disabled.disable_model_invocation = true;
        context.mcp_state.commands.push(disabled);
        let mut plain = crate::commands::Command::from_skill(skill_fixture(None));
        plain.name = "plain-mcp-prompt-probe".into();
        plain.loaded_from = None;
        context.mcp_state.commands.push(plain);
        for (name, code, text) in [
            ("skill-disabled-probe", 4, "disable-model-invocation"),
            ("plain-mcp-prompt-probe", 2, "Unknown skill"),
            ("help", 5, "not a prompt-based skill"),
        ] {
            assert!(
                matches!(SkillTool.validate_input(&serde_json::json!({"skill": name}), &context),
                ValidationResult::Error { message, error_code } if error_code == code && message.contains(text)),
                "{name}"
            );
        }
        let mut duplicate = crate::commands::Command::from_skill(skill_fixture(None));
        duplicate.name = "init".into();
        duplicate.loaded_from = Some(crate::skills::load_skills_dir::SkillLoadedFrom::Mcp);
        context.mcp_state.commands.push(duplicate);
        let catalog = get_all_commands(&context);
        assert_eq!(
            catalog
                .iter()
                .filter(|command| command.name == "init")
                .count(),
            1
        );
        assert_eq!(
            crate::commands::find_command("init", &catalog)
                .unwrap()
                .source,
            crate::commands::CommandSource::Builtin
        );
    }

    #[test]
    fn skill_tool_call_loads_inline_skill_and_injects_prompt_message() {
        use crate::tool::ToolCall;
        use std::io::Write;

        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let root =
            std::env::temp_dir().join(format!("cometix-skill-tool-call-{}", uuid::Uuid::new_v4()));
        let skill_file = root
            .join(".claude")
            .join("skills")
            .join("commit")
            .join("SKILL.md");
        std::fs::create_dir_all(skill_file.parent().unwrap()).unwrap();
        let mut file = std::fs::File::create(&skill_file).unwrap();
        file.write_all(
            b"---\ndescription: Commit changes\nallowed-tools: Bash, Read\narguments: message\nmodel: opus\n---\nCreate commit: $message\nDir: ${CLAUDE_SKILL_DIR}\nSession: ${CLAUDE_SESSION_ID}",
        )
        .unwrap();

        let old_cwd = crate::bootstrap::state::get_original_cwd();
        let old_session = crate::bootstrap::state::get_session_id();
        crate::bootstrap::state::set_original_cwd(&root);
        crate::bootstrap::state::set_session_id("session-skill-test");

        let request = crate::utils::permissions::permissions::mock_permission_request_with_input(
            "perm-skill".to_string(),
            "toolu_skill".to_string(),
            "Skill".to_string(),
            "commit".to_string(),
            serde_json::json!({"skill": "/commit", "args": "fix"}),
            crate::types::permissions::PermissionMode::Default,
        );

        let result = futures::executor::block_on(SkillTool.call(
            &request.input,
            &request,
            &crate::tool::ToolUseContext::default(),
            None,
            None,
            None,
        ));

        crate::bootstrap::state::set_original_cwd(old_cwd);
        crate::bootstrap::state::set_session_id(old_session);

        let crate::tool::ToolOutput::Skill(Output::Inline {
            command_name,
            allowed_tools,
            model,
            status,
            ..
        }) = result.data
        else {
            panic!("expected inline Skill output");
        };
        assert_eq!(command_name, "commit");
        assert_eq!(
            allowed_tools.as_deref(),
            Some(&["Bash".to_string(), "Read".to_string()][..])
        );
        assert_eq!(model.as_deref(), Some("opus"));
        // The inline main shape carries no status (SkillTool.ts:768-773).
        assert_eq!(status, None);
        assert_eq!(result.new_messages.len(), 2);
        let crate::types::message::Message::User(message) = &result.new_messages[0] else {
            panic!("expected user message");
        };
        let crate::types::message::UserContent::MetaText(text) = &message.content[0] else {
            panic!("expected text content");
        };
        assert!(text.contains("Create commit: fix"));
        assert!(text.contains("Session: session-skill-test"));
        assert!(text.contains("Base directory for this skill:"));
        assert!(
            matches!(&result.new_messages[1], crate::types::message::Message::Attachment(attachment)
            if matches!(&attachment.attachment, crate::utils::attachments::Attachment::CommandPermissions { allowed_tools, model }
                if allowed_tools == &vec!["Bash".to_string(), "Read".to_string()] && model.as_deref() == Some("opus")))
        );
    }

    #[test]
    fn skill_tool_call_expands_allowed_prompt_shell_commands() {
        use crate::tool::ToolCall;
        use std::io::Write;

        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let root =
            std::env::temp_dir().join(format!("cometix-skill-tool-shell-{}", uuid::Uuid::new_v4()));
        let skill_file = root
            .join(".claude")
            .join("skills")
            .join("shell")
            .join("SKILL.md");
        std::fs::create_dir_all(skill_file.parent().unwrap()).unwrap();
        let mut file = std::fs::File::create(&skill_file).unwrap();
        // `printf`, not `echo`: `echo` is read-only classified
        // (bash_tool/read_only_validation.rs:1370) and therefore auto-allowed,
        // so an echo fixture would pass without the frontmatter grant.
        file.write_all(
            b"---\ndescription: Shell skill\nallowed-tools: Bash(printf *)\n---\nResult: !`printf skill-shell-output`",
        )
        .unwrap();

        let old_cwd = crate::bootstrap::state::get_original_cwd();
        crate::bootstrap::state::set_original_cwd(&root);

        let request = crate::utils::permissions::permissions::mock_permission_request_with_input(
            "perm-skill".to_string(),
            "toolu_skill".to_string(),
            "Skill".to_string(),
            "shell".to_string(),
            serde_json::json!({"skill": "shell"}),
            crate::types::permissions::PermissionMode::Default,
        );
        let result = futures::executor::block_on(SkillTool.call(
            &request.input,
            &request,
            &crate::tool::ToolUseContext::default(),
            None,
            None,
            None,
        ));
        crate::bootstrap::state::set_original_cwd(old_cwd);

        let crate::tool::ToolOutput::Skill(_) = result.data else {
            panic!("expected Skill output");
        };
        let crate::types::message::Message::User(message) = &result.new_messages[0] else {
            panic!("expected user message");
        };
        let crate::types::message::UserContent::MetaText(text) = &message.content[0] else {
            panic!("expected text content");
        };
        assert!(text.contains("Result: skill-shell-output"));
        assert!(!text.contains("!`"));
    }

    #[test]
    fn skill_declaring_hooks_is_not_auto_allowed_by_safe_properties() {
        // Maps to CC `SAFE_SKILL_PROPERTIES` (SkillTool.ts:875-908): `hooks` is
        // an own key of the command object (:342) that the allowlist omits, so
        // `skillHasOnlySafeProperties` returns false and the dialog opens. CC's
        // meaningful-value test skips empty containers (:920-929), so `hooks: {}`
        // stays safe. Old shape: `SkillCommand` had no `hooks` field, so a
        // hook-installing skill was silently auto-allowed.
        let plain = skill_fixture(None);
        assert!(skill_has_only_safe_properties(
            &crate::commands::Command::from_skill(plain)
        ));

        let empty = skill_fixture(Some(crate::services::hooks::HooksConfig::new()));
        assert!(skill_has_only_safe_properties(
            &crate::commands::Command::from_skill(empty)
        ));

        let with_hooks = skill_fixture(Some(
            serde_json::from_value(serde_json::json!({
                "Stop": [{"hooks": [{"type": "command", "command": "echo hi"}]}]
            }))
            .unwrap(),
        ));
        assert!(!skill_has_only_safe_properties(
            &crate::commands::Command::from_skill(with_hooks)
        ));
    }

    #[test]
    fn builtin_allowed_tools_matches_official_safe_properties() {
        // CC SkillTool.ts:913-934 checks own command properties, including
        // builtin allowedTools; a filesystem SkillCommand is not required.
        let mut command = crate::commands::statusline::command();
        assert!(command.prompt_command.is_none());
        assert!(!skill_has_only_safe_properties(&command));
        command.allowed_tools.clear();
        assert!(skill_has_only_safe_properties(&command));
        command.allowed_tools.push(String::new());
        // Source tests array length, not the truthiness of each tool string.
        assert!(!skill_has_only_safe_properties(&command));
    }

    #[test]
    fn shell_only_skill_is_auto_allowed_like_official() {
        // Maps to CC `SAFE_SKILL_PROPERTIES` / `skillHasOnlySafeProperties`
        // (SkillTool.ts:875-933). `shell` is not an own key of the command
        // object CC builds (`loadSkillsDir.ts:317-400` — `createSkillCommand`
        // closes over it), so `Object.keys(command)` never sees it and a
        // `shell:`-only skill needs no confirmation. Old shape: the port stored
        // `shell` as a field and treated it like `allowed-tools`, so every
        // `shell: powershell` skill — the ordinary Windows configuration —
        // opened a permission dialog CC does not open. The assert fails; it
        // does not hang.
        let mut shell_only = skill_fixture(None);
        shell_only.shell = Some(crate::utils::frontmatter_parser::FrontmatterShell::PowerShell);
        assert!(skill_has_only_safe_properties(
            &crate::commands::Command::from_skill(shell_only)
        ));

        // The two keys CC's allowlist really does omit still ask.
        let mut with_tools = skill_fixture(None);
        with_tools.allowed_tools = vec!["Bash(rm *)".to_string()];
        assert!(!skill_has_only_safe_properties(
            &crate::commands::Command::from_skill(with_tools)
        ));
    }

    fn skill_fixture(
        hooks: Option<crate::services::hooks::HooksConfig>,
    ) -> crate::skills::load_skills_dir::SkillCommand {
        use crate::skills::load_skills_dir::{
            SkillCommand, SkillExecutionContext, SkillLoadedFrom, SkillSource,
        };
        SkillCommand {
            name: "fixture".to_string(),
            display_name: None,
            description: "Fixture".to_string(),
            has_user_specified_description: true,
            markdown_content: "body".to_string(),
            content_length: 4,
            allowed_tools: Vec::new(),
            argument_hint: None,
            argument_names: Vec::new(),
            when_to_use: None,
            version: None,
            model: None,
            effort: None,
            disable_model_invocation: false,
            user_invocable: true,
            execution_context: SkillExecutionContext::Inline,
            agent: None,
            shell: None,
            hooks,
            paths: None,
            source: SkillSource::ProjectSettings,
            loaded_from: SkillLoadedFrom::Skills,
            skill_root: None,
            file_path: std::path::PathBuf::from("SKILL.md"),
        }
    }

    fn command_allow_rules(
        context: &crate::tool::ToolUseContext,
    ) -> Vec<crate::types::permissions::PermissionRuleValue> {
        context
            .tool_permission_context
            .always_allow_rules
            .get(&crate::types::permissions::PermissionRuleSource::Command)
            .cloned()
            .unwrap_or_default()
    }

    #[test]
    fn skill_context_modifier_applies_all_three_frontmatter_overrides() {
        // Maps to CC `SkillTool.ts:775-838`. Old shape: the port produced no
        // `contextModifier` at all, so a skill's `allowed-tools`, `model:` and
        // `effort:` reached only the skill's own `!` blocks and were invisible
        // to every tool use the model made afterwards.
        let modifier = SkillContextModifier::new(
            vec!["Bash(printf *)".to_string()],
            Some("opus".to_string()),
            Some(crate::utils::effort::EffortValue::Named("high".to_string())),
        )
        .expect("frontmatter carries all three captures");

        let mut context = crate::tool::ToolUseContext::default();
        context.main_loop_model = Some("claude-sonnet-4-5".to_string());
        modifier.modify_context(&mut context);

        assert_eq!(
            command_allow_rules(&context),
            vec![crate::types::permissions::PermissionRuleValue::new(
                "Bash",
                Some("printf *".to_string())
            )]
        );
        assert_eq!(context.main_loop_model.as_deref(), Some("opus"));
        assert_eq!(
            context.effort_value,
            Some(crate::utils::effort::EffortValue::Named("high".to_string()))
        );
    }

    #[test]
    fn skill_context_modifier_unions_command_rules_instead_of_replacing_them() {
        // CC `SkillTool.ts:788-803` spreads the existing `command` rules into a
        // `new Set` before appending — a UNION. That is a THIRD rule, distinct
        // from the two the port already had: the fork path
        // (`forkedAgent.ts:147-171`) also unions but only for the forked
        // agent's own context, and the inline `!` block override
        // (`loadSkillsDir.ts:377-392`) unconditionally REPLACES `command`.
        let mut context = crate::tool::ToolUseContext::default();
        context.tool_permission_context.always_allow_rules.insert(
            crate::types::permissions::PermissionRuleSource::Command,
            vec![crate::types::permissions::PermissionRuleValue::new(
                "Read", None,
            )],
        );

        let modifier = SkillContextModifier::new(
            vec!["Bash(printf *)".to_string(), "Read".to_string()],
            None,
            None,
        )
        .expect("allowed-tools alone is enough");
        modifier.modify_context(&mut context);

        // The pre-existing rule survives (union, not replace) and the duplicate
        // `Read` is deduped, matching CC's `new Set([...])`.
        assert_eq!(
            command_allow_rules(&context),
            vec![
                crate::types::permissions::PermissionRuleValue::new("Read", None),
                crate::types::permissions::PermissionRuleValue::new(
                    "Bash",
                    Some("printf *".to_string())
                ),
            ]
        );
    }

    #[test]
    fn skill_context_modifier_stays_out_of_the_app_store() {
        // CC's modifier only wraps `getAppState`; it never calls `setAppState`.
        // `REPL.tsx:3688-3696` states the rule for the sibling effort override:
        // "wrapping getAppState keeps the override out of the global store so
        // background agents and UI subscribers (Spinner, LogoV2) never see it."
        // So the effect is scoped to this query's context chain and is gone
        // once the next user turn builds a fresh `getToolUseContext(...)`.
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        let mut context = crate::tool::ToolUseContext::default().with_app_store(store.clone());

        let modifier =
            SkillContextModifier::new(vec!["Bash(printf *)".to_string()], None, None).unwrap();
        modifier.modify_context(&mut context);

        assert_eq!(command_allow_rules(&context).len(), 1);
        assert!(
            store
                .tool_permission_context()
                .always_allow_rules
                .get(&crate::types::permissions::PermissionRuleSource::Command)
                .is_none()
        );
    }

    #[test]
    fn skill_model_override_carries_the_1m_suffix() {
        // Maps to CC `SkillTool.ts:806-820` calling `resolveSkillModelOverride`
        // (`utils/model/model.ts:508-536`): a skill declaring `model: opus` on
        // an `opus[1m]` session must not silently drop the window to 200K.
        // `haiku` has no 1M variant, so it still downgrades.
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _context = EnvVarGuard::unset("CLAUDE_CODE_DISABLE_1M_CONTEXT");

        let mut context = crate::tool::ToolUseContext::default();
        context.main_loop_model = Some("opus[1m]".to_string());
        SkillContextModifier::new(Vec::new(), Some("opus".to_string()), None)
            .unwrap()
            .modify_context(&mut context);
        assert_eq!(context.main_loop_model.as_deref(), Some("opus[1m]"));

        let mut context = crate::tool::ToolUseContext::default();
        context.main_loop_model = Some("opus[1m]".to_string());
        SkillContextModifier::new(Vec::new(), Some("haiku".to_string()), None)
            .unwrap()
            .modify_context(&mut context);
        assert_eq!(context.main_loop_model.as_deref(), Some("haiku"));

        // A non-1M session leaves the skill's alias untouched.
        let mut context = crate::tool::ToolUseContext::default();
        context.main_loop_model = Some("claude-sonnet-4-5".to_string());
        SkillContextModifier::new(Vec::new(), Some("opus".to_string()), None)
            .unwrap()
            .modify_context(&mut context);
        assert_eq!(context.main_loop_model.as_deref(), Some("opus"));
    }

    #[test]
    fn skill_context_modifier_absent_when_every_capture_is_empty() {
        // CC's closure is the identity function with no allowedTools, a falsy
        // `model` (empty string is falsy in JS) and `effort === undefined`.
        assert!(SkillContextModifier::new(Vec::new(), None, None).is_none());
        assert!(SkillContextModifier::new(Vec::new(), Some(String::new()), None).is_none());
        assert!(SkillContextModifier::new(Vec::new(), Some("opus".to_string()), None).is_some());

        // A falsy `model` alone does not override, matching `if (model)`.
        let mut context = crate::tool::ToolUseContext::default();
        context.main_loop_model = Some("claude-sonnet-4-5".to_string());
        SkillContextModifier::new(vec!["Read".to_string()], Some(String::new()), None)
            .unwrap()
            .modify_context(&mut context);
        assert_eq!(
            context.main_loop_model.as_deref(),
            Some("claude-sonnet-4-5")
        );
    }

    #[test]
    fn skill_tool_call_returns_the_context_modifier_from_frontmatter() {
        use crate::tool::ToolCall;
        use std::io::Write;

        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-skill-tool-modifier-{}",
            uuid::Uuid::new_v4()
        ));
        let skill_file = root
            .join(".claude")
            .join("skills")
            .join("review")
            .join("SKILL.md");
        std::fs::create_dir_all(skill_file.parent().unwrap()).unwrap();
        let mut file = std::fs::File::create(&skill_file).unwrap();
        file.write_all(
            b"---\ndescription: Review\nallowed-tools: Bash(printf *), Read\nmodel: opus\neffort: high\n---\nReview body",
        )
        .unwrap();

        let old_cwd = crate::bootstrap::state::get_original_cwd();
        crate::bootstrap::state::set_original_cwd(&root);
        let request = crate::utils::permissions::permissions::mock_permission_request_with_input(
            "perm-skill".to_string(),
            "toolu_skill".to_string(),
            "Skill".to_string(),
            "review".to_string(),
            serde_json::json!({"skill": "review"}),
            crate::types::permissions::PermissionMode::Default,
        );
        let result = futures::executor::block_on(SkillTool.call(
            &request.input,
            &request,
            &crate::tool::ToolUseContext::default(),
            None,
            None,
            None,
        ));
        crate::bootstrap::state::set_original_cwd(old_cwd);
        let _ = std::fs::remove_dir_all(&root);

        let crate::tool::ToolOutput::Skill(Output::Inline {
            context_modifier, ..
        }) = &result.data
        else {
            panic!("expected inline Skill output");
        };
        let modifier = context_modifier
            .as_ref()
            .expect("inline skills return a contextModifier");
        assert_eq!(
            modifier.allowed_tools,
            vec!["Bash(printf *)".to_string(), "Read".to_string()]
        );
        assert_eq!(modifier.model.as_deref(), Some("opus"));
        assert_eq!(
            modifier.effort,
            Some(crate::utils::effort::EffortValue::Named("high".to_string()))
        );

        // The carrier never reaches the recorded `toolUseResult`: CC's wire
        // shape is the `data` object alone (`SkillTool.ts:768-773`).
        let recorded = SkillTool.tool_use_result(&result.data).unwrap();
        assert!(recorded.get("effort").is_none());
        assert!(recorded.get("contextModifier").is_none());
    }

    #[test]
    fn listed_mcp_skill_uses_the_same_invocation_registry() {
        let server_name = "docs";
        let prompt_name = "summarize";
        let command_name =
            crate::services::mcp::client::mcp_prompt_command_name(server_name, prompt_name);
        let command = crate::commands::Command::from_mcp_prompt(
            crate::services::mcp::client::McpPromptCommandSnapshot {
                name: command_name.clone(),
                description: "Summarize docs".to_string(),
                has_user_specified_description: true,
                user_facing_name: "docs:summarize (MCP)".to_string(),
                arg_names: vec!["path".to_string()],
                source: "mcp",
            },
        );
        let mut context = crate::tool::ToolUseContext::default();
        context.mcp_state.commands.push(command);
        context
            .mcp_state
            .clients
            .push(crate::services::mcp::types::McpServerSnapshot {
                connection_id: None,
                client: crate::services::mcp::types::McpClientSnapshot {
                    name: server_name.to_string(),
                    status: crate::services::mcp::types::McpServerConnectionType::Connected,
                    reconnect_attempt: None,
                    max_reconnect_attempts: None,
                    ide_name: None,
                    server_version: None,
                    error: None,
                },
                config: None,
                supports_resources: false,
                tools: Vec::new(),
                prompts: vec![crate::services::mcp::types::McpPromptSnapshot {
                    name: prompt_name.to_string(),
                    description: Some("Summarize docs".to_string()),
                    arg_names: vec!["path".to_string()],
                }],
                resources: Vec::new(),
            });
        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(execute_mcp_skill(
                &command_name,
                Some("README.md"),
                &context,
            ))
            .expect("MCP registry command should be recognized");
        let crate::tool::ToolOutput::Composed { content, .. } = result.data else {
            panic!("unconnected test MCP should return a composed runtime error");
        };
        assert!(content.contains("MCP skill"));
        assert!(!content.contains("Unknown skill"));
    }

    #[test]
    fn prepare_forked_skill_resolves_general_purpose_and_prompt() {
        use std::io::Write;

        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let root =
            std::env::temp_dir().join(format!("cometix-skill-tool-fork-{}", uuid::Uuid::new_v4()));
        let skill_file = root
            .join(".claude")
            .join("skills")
            .join("deep")
            .join("SKILL.md");
        std::fs::create_dir_all(skill_file.parent().unwrap()).unwrap();
        let mut file = std::fs::File::create(&skill_file).unwrap();
        file.write_all(
            b"---\ndescription: Deep dive\ncontext: fork\nallowed-tools: Read, Grep\neffort: high\n---\nDeep body",
        )
        .unwrap();
        let old_cwd = crate::bootstrap::state::get_original_cwd();
        crate::bootstrap::state::set_original_cwd(&root);

        let commands = crate::skills::load_skills_dir::get_skill_dir_commands(&root);
        let command = crate::skills::load_skills_dir::find_skill_command("deep", &commands)
            .expect("fork skill");
        let prepared = crate::utils::forked_agent::prepare_forked_command_context(
            &command,
            None,
            &context_with_general_purpose_agent(),
        )
        .expect("prepare forked context");
        crate::bootstrap::state::set_original_cwd(old_cwd);
        let _ = std::fs::remove_dir_all(&root);

        assert_eq!(prepared.base_agent.agent_type, "general-purpose");
        assert_eq!(
            forked_skill_agent_definition(&command, &prepared.base_agent).effort,
            Some(crate::utils::effort::EffortValue::Named("high".to_string()))
        );
        assert!(prepared.skill_content.contains("Deep body"));
        assert!(
            !crate::tool::ToolUseContext::default()
                .with_get_app_state_override(prepared.modified_get_app_state)
                .get_app_state()
                .expect("projected app state")
                .tool_permission_context
                .always_allow_rules
                .is_empty()
        );
    }

    #[test]
    fn skill_tool_fork_call_no_longer_returns_not_ported_stub() {
        use crate::tool::ToolCall;
        use std::io::Write;

        // Production tool calls run inside a Tokio query actor and may await
        // plugin work on the process executor during agent preparation.
        crate::utils::process_runtime::initialize_test_process_runtime();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let root =
            std::env::temp_dir().join(format!("cometix-skill-tool-fork-{}", uuid::Uuid::new_v4()));
        let skill_file = root
            .join(".claude")
            .join("skills")
            .join("deep")
            .join("SKILL.md");
        std::fs::create_dir_all(skill_file.parent().unwrap()).unwrap();
        let mut file = std::fs::File::create(&skill_file).unwrap();
        file.write_all(b"---\ndescription: Deep dive\ncontext: fork\n---\nDeep body")
            .unwrap();
        let old_cwd = crate::bootstrap::state::get_original_cwd();
        crate::bootstrap::state::set_original_cwd(&root);

        let request = crate::utils::permissions::permissions::mock_permission_request_with_input(
            "perm-skill".to_string(),
            "toolu_skill".to_string(),
            "Skill".to_string(),
            "deep".to_string(),
            serde_json::json!({"skill": "deep"}),
            crate::types::permissions::PermissionMode::Default,
        );
        // Abort before run_agent so the test does not need a live model.
        let mut context = context_with_general_purpose_agent();
        context.abort_controller.abort();
        let result =
            runtime.block_on(SkillTool.call(&request.input, &request, &context, None, None, None));
        crate::bootstrap::state::set_original_cwd(old_cwd);
        let _ = std::fs::remove_dir_all(&root);

        match result.data {
            crate::tool::ToolOutput::Skill(output) => {
                assert!(matches!(output, Output::Forked { .. }));
            }
            crate::tool::ToolOutput::Composed { content, .. } => {
                assert!(
                    !content.contains("not ported"),
                    "fork path should reach run_agent, got {content}"
                );
                assert!(
                    content.contains("forked execution failed") || content.contains("aborted"),
                    "expected aborted/failed fork error, got {content}"
                );
            }
            _ => panic!("unexpected skill fork result variant"),
        }
    }

    #[test]
    fn extract_result_text_uses_last_assistant_message() {
        use crate::types::message::{AssistantContent, AssistantMessage, Message, UserContent};

        let messages = vec![
            Message::User(crate::types::message::UserMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![UserContent::Text("prompt".into())],
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
            Message::Assistant(AssistantMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![AssistantContent::Text("final answer".into())],
                model: None,
                stop_reason: None,
                usage: None,
            }),
        ];
        assert_eq!(
            crate::utils::forked_agent::extract_result_text(&messages, "default"),
            "final answer"
        );
        assert_eq!(
            crate::utils::forked_agent::extract_result_text(&[], "Skill execution completed"),
            "Skill execution completed"
        );
    }
}
