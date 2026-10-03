//! Read-only agent definition discovery.
//!
//! Maps to: CC `tools/AgentTool/loadAgentsDir.ts`.
//!
//! Safety boundary: this module only reads agent markdown metadata needed by
//! startup/status UI. It does not execute agent prompts, mutate the global
//! agent color map (see `agent_color_manager.rs`), log analytics, or write
//! settings/session files. Plugin loading is limited to explicit session-only
//! `--plugin-dir` directories via `utils/plugins/load_plugin_agents.rs`.
//!
//! The one write this module performs is CC's own: `initializeAgentMemorySnapshots`
//! (CC :262-294, called at :348-354) bootstraps user-scope agent memory from a
//! project snapshot when the `AGENT_MEMORY_SNAPSHOT` build feature and auto
//! memory are both on. Everything it writes lives under the agent memory dir
//! (`agent_memory_snapshot.rs`), and it is a no-op unless
//! `<cwd>/.claude/agent-memory-snapshots/<agentType>/snapshot.json` exists.

use super::agent_memory::AgentMemoryScope;
use super::built_in_agents::get_built_in_agents;
use crate::services::mcp::types::ScopedMcpServerConfig;
use crate::types::permissions::PermissionMode;
use crate::utils::status_notice_helpers::{AgentDefinitionSnapshot, AgentDefinitionsSnapshot};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AgentDefinitionSource {
    BuiltIn,
    Plugin,
    UserSettings,
    ProjectSettings,
    FlagSettings,
    PolicySettings,
}

/// Maps to CC `loadAgentsDir.ts#AgentMcpServerSpec`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentMcpServerSpec {
    /// Reference to an existing configured MCP server by name.
    Reference(String),
    /// Inline agent-specific MCP server definition. Official marks these as
    /// `scope: 'dynamic'` and cleans them up when the agent exits.
    Inline {
        name: String,
        config: ScopedMcpServerConfig,
    },
}

/// Maps to CC `loadAgentsDir.ts:127` `pendingSnapshotUpdate?: { snapshotTimestamp: string }`.
///
/// Written by `initialize_agent_memory_snapshots` on the `prompt-update` arm
/// (CC :283-290). CC's only reader is the `--agent` startup path in
/// `main.tsx:3277-3305`, which gates on
/// `feature('AGENT_MEMORY_SNAPSHOT') && mainThreadAgentDefinition &&
/// isCustomAgent(...) && .memory && .pendingSnapshotUpdate`, calls
/// `launchSnapshotUpdateDialog` (`dialogLaunchers.tsx:31-52`), prepends
/// `buildMergePrompt(...)` to `inputPrompt` on `'merge'`, and then clears the
/// field. Reader seam in this port: `components/agents/SnapshotUpdateDialog.ts`
/// is a `@generated-stub` in CC 2.1.88 (MODULE_MAP row `no-source`), so the
/// dialog, `buildMergePrompt`, and the `merge`/`keep`/`replace` choices have no
/// portable source. The field is therefore write-only here until that stub is
/// reversed; `replace_from_snapshot` / `mark_snapshot_synced`
/// (`agent_memory_snapshot.rs:194`/`:220`) are the leaves that dialog would drive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingSnapshotUpdate {
    /// Maps to CC `pendingSnapshotUpdate.snapshotTimestamp`.
    pub snapshot_timestamp: String,
}

impl AgentDefinitionSource {
    pub fn official_name(self) -> &'static str {
        match self {
            Self::BuiltIn => "built-in",
            Self::Plugin => "plugin",
            Self::UserSettings => "userSettings",
            Self::ProjectSettings => "projectSettings",
            Self::FlagSettings => "flagSettings",
            Self::PolicySettings => "policySettings",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentDefinition {
    /// Maps to CC `AgentDefinition.agentType`.
    pub agent_type: String,
    /// Maps to CC `AgentDefinition.whenToUse`.
    pub when_to_use: String,
    /// Maps to CC `CustomAgentDefinition.getSystemPrompt()` / built-in
    /// `getSystemPrompt(...)` return value.
    pub system_prompt: Option<String>,
    /// Maps to CC `BaseAgentDefinition.model`.
    pub model: Option<String>,
    /// Maps to CC `BaseAgentDefinition.effort`.
    pub effort: Option<crate::utils::effort::EffortValue>,
    /// Maps to CC `BaseAgentDefinition.tools`.
    pub tools: Option<Vec<String>>,
    /// Maps to CC `BaseAgentDefinition.disallowedTools`.
    pub disallowed_tools: Option<Vec<String>>,
    /// Maps to CC `BaseAgentDefinition.skills`.
    pub skills: Option<Vec<String>>,
    /// Maps to CC `BaseAgentDefinition.color`.
    pub color: Option<String>,
    /// Maps to CC `BaseAgentDefinition.omitClaudeMd`.
    pub omit_claude_md: bool,
    /// Maps to CC `BaseAgentDefinition.criticalSystemReminder_EXPERIMENTAL`.
    pub critical_system_reminder_experimental: Option<String>,
    /// Maps to CC `BaseAgentDefinition.mcpServers`.
    pub mcp_servers: Option<Vec<AgentMcpServerSpec>>,
    /// Maps to CC `BaseAgentDefinition.hooks`.
    pub hooks: Option<crate::services::hooks::HooksConfig>,
    /// Maps to CC `BaseAgentDefinition.requiredMcpServers`.
    pub required_mcp_servers: Option<Vec<String>>,
    /// Maps to CC `BaseAgentDefinition.permissionMode`.
    pub permission_mode: Option<PermissionMode>,
    /// Maps to CC `BaseAgentDefinition.maxTurns`.
    pub max_turns: Option<u32>,
    /// Maps to CC `BaseAgentDefinition.background`.
    pub background: bool,
    /// Maps to CC `BaseAgentDefinition.initialPrompt`.
    pub initial_prompt: Option<String>,
    /// Maps to CC `BaseAgentDefinition.memory`.
    pub memory: Option<AgentMemoryScope>,
    /// Maps to CC `BaseAgentDefinition.isolation`.
    pub isolation: Option<String>,
    /// Maps to CC `BaseAgentDefinition.pendingSnapshotUpdate` (`loadAgentsDir.ts:127`).
    pub pending_snapshot_update: Option<PendingSnapshotUpdate>,
    /// Maps to CC `CustomAgentDefinition.filename` / `PluginAgentDefinition.filename`.
    pub filename: Option<String>,
    /// Maps to CC `CustomAgentDefinition.baseDir`.
    pub base_dir: Option<PathBuf>,
    /// Maps to CC `PluginAgentDefinition.plugin`.
    pub plugin: Option<String>,
    /// Maps to CC `AgentDefinition.source`.
    pub source: AgentDefinitionSource,
}

/// Maps to: CC `loadAgentsDir.ts#isBuiltInAgent`.
pub fn is_built_in_agent(agent: &AgentDefinition) -> bool {
    agent.source == AgentDefinitionSource::BuiltIn
}

/// Maps to: CC `loadAgentsDir.ts#isCustomAgent`.
pub fn is_custom_agent(agent: &AgentDefinition) -> bool {
    agent.source != AgentDefinitionSource::BuiltIn && agent.source != AgentDefinitionSource::Plugin
}

/// Maps to: CC `loadAgentsDir.ts#isPluginAgent`.
pub fn is_plugin_agent(agent: &AgentDefinition) -> bool {
    agent.source == AgentDefinitionSource::Plugin
}

impl AgentDefinition {
    /// Maps to: CC `loadAgentsDir.ts:481-487,726-732` custom-agent
    /// `getSystemPrompt` closures and `utils/plugins/loadPluginAgents.ts:205-211`.
    /// Built-ins already carry their defining
    /// getSystemPrompt result; the custom loader alone appends memory, inside
    /// the same string. `None` denotes an unavailable prompt provider in this
    /// port's existing Option carrier, not an empty successful prompt.
    pub fn get_system_prompt(&self, context: &crate::tool::ToolUseContext) -> Option<String> {
        let mut prompt = self.system_prompt.clone()?;
        if self.source != AgentDefinitionSource::BuiltIn {
            if let Some(memory) = self.memory {
                if crate::memdir::paths::is_auto_memory_enabled() {
                    prompt.push_str("\n\n");
                    prompt.push_str(&super::agent_memory::load_agent_memory_prompt(
                        &self.agent_type,
                        memory,
                        &context.effective_cwd(),
                    ));
                }
            }
        }
        Some(prompt)
    }

    pub fn new(
        agent_type: impl Into<String>,
        when_to_use: impl Into<String>,
        source: AgentDefinitionSource,
    ) -> Self {
        Self {
            agent_type: agent_type.into(),
            when_to_use: when_to_use.into(),
            system_prompt: None,
            model: None,
            effort: None,
            tools: None,
            disallowed_tools: None,
            skills: None,
            color: None,
            omit_claude_md: false,
            critical_system_reminder_experimental: None,
            mcp_servers: None,
            hooks: None,
            required_mcp_servers: None,
            permission_mode: None,
            max_turns: None,
            background: false,
            initial_prompt: None,
            memory: None,
            isolation: None,
            pending_snapshot_update: None,
            filename: None,
            base_dir: None,
            plugin: None,
            source,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailedAgentFile {
    /// Maps to CC `AgentDefinitionsResult.failedFiles[].path`.
    pub path: PathBuf,
    /// Maps to CC `AgentDefinitionsResult.failedFiles[].error`.
    pub error: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentDefinitionsResult {
    /// Maps to CC `AgentDefinitionsResult.activeAgents`.
    pub active_agents: Vec<AgentDefinition>,
    /// Maps to CC `AgentDefinitionsResult.allAgents`.
    pub all_agents: Vec<AgentDefinition>,
    /// Maps to CC `AgentDefinitionsResult.failedFiles`.
    pub failed_files: Vec<FailedAgentFile>,
    /// Maps to CC `loadAgentsDir.ts:190` `allowedAgentTypes?: string[]` — the
    /// `Agent(type1,type2)` narrowing a main-thread agent's frontmatter
    /// declares.
    ///
    /// Declared on the result type but never produced by the loader: CC's only
    /// producer is `agentToolUtils.ts:190-197` (`resolveAgentTools` parsing the
    /// tool spec), and the only site that puts it on an `agentDefinitions`
    /// object is `REPL.tsx:3206-3208`, which shallow-copies the memo's value
    /// onto the per-turn `toolUseContext.options.agentDefinitions`. So
    /// `AppState.agentDefinitions` never carries it (`AppStateStore.ts:505`) —
    /// only a per-turn context does.
    /// Structural census (no other producer exists):
    /// `ast-grep --lang ts -p '({ allowedAgentTypes: $V })' --selector pair` →
    /// `claude.ts:1241`, `api.ts:175`, `query.ts:683`; `--lang tsx` →
    /// `REPL.tsx:1208`, `:1219`; shorthand form
    /// `-p '({ allowedAgentTypes })' --selector shorthand_property_identifier`
    /// → `agentToolUtils.ts:223`, `REPL.tsx:3207`.
    pub allowed_agent_types: Option<Vec<String>>,
}

impl From<AgentDefinitionsResult> for AgentDefinitionsSnapshot {
    fn from(value: AgentDefinitionsResult) -> Self {
        Self {
            active_agents: value
                .active_agents
                .into_iter()
                .map(|agent| AgentDefinitionSnapshot {
                    agent_type: agent.agent_type,
                    when_to_use: agent.when_to_use,
                    source: agent.source.official_name().to_string(),
                })
                .collect(),
        }
    }
}

const SOURCE_PRIORITY: [AgentDefinitionSource; 6] = [
    AgentDefinitionSource::BuiltIn,
    AgentDefinitionSource::Plugin,
    AgentDefinitionSource::UserSettings,
    AgentDefinitionSource::ProjectSettings,
    AgentDefinitionSource::FlagSettings,
    AgentDefinitionSource::PolicySettings,
];

/// Maps to CC `getAgentDefinitionsWithOverrides(cwd)`.
pub fn get_agent_definitions_with_overrides(cwd: &Path) -> AgentDefinitionsResult {
    let built_in_agents = get_built_in_agents();

    // Maps to the official simple-mode branch: skip custom agents and return
    // only built-ins.
    if crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_SIMPLE").as_deref(),
    ) {
        let mut all_agents = built_in_agents;
        all_agents.extend(cli_flag_agents_from_bootstrap());
        let active_agents = get_active_agents_from_list(&all_agents);
        return AgentDefinitionsResult {
            active_agents,
            all_agents,
            failed_files: Vec::new(),
            // CC's loader returns object literals without `allowedAgentTypes`;
            // only `REPL.tsx:3206-3208` ever sets it.
            allowed_agent_types: None,
        };
    }

    // CC :308 `loadMarkdownFilesForSubdir('agents', cwd)`.
    let markdown_files =
        crate::utils::markdown_config_loader::load_markdown_files_for_subdir("agents", cwd);
    let audience = crate::utils::build_profile::build_audience();
    let mut custom_agents = Vec::new();
    let mut failed_files = Vec::new();
    for markdown_file in markdown_files {
        match parse_agent_from_markdown(&markdown_file, audience) {
            Some(agent) => custom_agents.push(agent),
            // CC :324 `if (!frontmatter['name']) return null` — JS truthy:
            // a falsy name (absent, "", 0, false) skips silently; only
            // truthy-name files record a parse failure.
            None if markdown_file
                .frontmatter
                .get("name")
                .is_some_and(frontmatter_value_truthy) =>
            {
                failed_files.push(FailedAgentFile {
                    path: markdown_file.file_path,
                    error: parse_error_for_frontmatter(&markdown_file.frontmatter),
                });
            }
            None => {}
        }
    }

    // Maps to CC `getAgentDefinitionsWithOverrides`: built-ins first,
    // enabled plugin agents next, filesystem custom agents after that. CLI/SDK
    // flag agents are merged last by `main.tsx`.
    let plugin_agents = crate::utils::plugins::load_plugin_agents::load_plugin_agents();
    // Maps to: CC `loadAgentsDir.ts:347-355`. CC kicks off `loadPluginAgents()`
    // first, then `Promise.all`s it with `initializeAgentMemorySnapshots` so
    // neither becomes a floating promise if the other throws (CC :344-346).
    // `load_plugin_agents` is a plain synchronous fn here, so plugin
    // loading has already finished by this line and the join has nothing left
    // to express — the faithful sequential form is the bare call. CC runs it on
    // `customAgents` only: built-ins, plugin agents, and CLI/SDK flag agents are
    // deliberately excluded.
    //
    // Run frequency differs, harmlessly: CC wraps this whole function in
    // `memoize(...)` keyed by cwd (`loadAgentsDir.ts:296`, cleared by
    // `clearAgentDefinitionsCache` :395-396), so the snapshot init runs once per
    // cwd per process. This port has no memo — all four callers
    // (`interactive_helpers.rs:173`, `commands/agents/agents.rs:20`,
    // `components/agents/agents_menu.rs:125`,
    // `components/memory/memory_file_selector.rs:265`) re-run the load. That is
    // safe because both write arms are self-cancelling: `initialize` writes the
    // `.md` payload plus `.snapshot-synced.json`, after which
    // `check_agent_memory_snapshot` sees local memory whose synced timestamp
    // equals the snapshot's and returns `None` (`agentMemorySnapshot.ts:124-143`);
    // `prompt-update` writes no files at all. Repeat calls therefore cost one
    // `snapshot.json` read per user-scope agent and change nothing.
    if crate::utils::feature_flags::feature_enabled(
        crate::utils::feature_flags::FeatureFlag::AgentMemorySnapshot,
    ) && crate::memdir::paths::is_auto_memory_enabled()
    {
        initialize_agent_memory_snapshots(&mut custom_agents, cwd);
    }
    let flag_agents = cli_flag_agents_from_bootstrap();
    let mut all_agents = built_in_agents;
    all_agents.extend(plugin_agents);
    all_agents.extend(custom_agents);
    all_agents.extend(flag_agents);
    let active_agents = get_active_agents_from_list(&all_agents);
    // Maps to CC `loadAgentsDir.ts#getAgentDefinitionsWithOverrides`: initialize
    // the canonical agent-color manager from all active definitions.
    for agent in &active_agents {
        if let Some(color) = agent
            .color
            .as_deref()
            .and_then(super::agent_color_manager::parse_agent_color_name)
        {
            super::agent_color_manager::set_agent_color(&agent.agent_type, Some(color));
        }
    }

    AgentDefinitionsResult {
        active_agents,
        all_agents,
        failed_files,
        // CC's loader returns object literals without `allowedAgentTypes`;
        // only `REPL.tsx:3206-3208` ever sets it.
        allowed_agent_types: None,
    }
}

/// Maps to CC `getActiveAgentsFromList(allAgents)`.
pub fn get_active_agents_from_list(all_agents: &[AgentDefinition]) -> Vec<AgentDefinition> {
    let mut order = Vec::new();
    let mut active: HashMap<String, AgentDefinition> = HashMap::new();

    for source in SOURCE_PRIORITY {
        for agent in all_agents.iter().filter(|agent| agent.source == source) {
            if !active.contains_key(&agent.agent_type) {
                order.push(agent.agent_type.clone());
            }
            active.insert(agent.agent_type.clone(), agent.clone());
        }
    }

    order
        .into_iter()
        .filter_map(|agent_type| active.remove(&agent_type))
        .collect()
}

/// Maps to CC `loadAgentsDir.ts#hasRequiredMcpServers`.
pub fn has_required_mcp_servers(agent: &AgentDefinition, available_servers: &[String]) -> bool {
    let Some(required) = agent.required_mcp_servers.as_ref() else {
        return true;
    };
    if required.is_empty() {
        return true;
    }
    required.iter().all(|pattern| {
        let needle = pattern.to_lowercase();
        available_servers
            .iter()
            .any(|server| server.to_lowercase().contains(&needle))
    })
}

/// Maps to CC `loadAgentsDir.ts#filterAgentsByMcpRequirements`.
pub fn filter_agents_by_mcp_requirements(
    agents: &[AgentDefinition],
    available_servers: &[String],
) -> Vec<AgentDefinition> {
    agents
        .iter()
        .filter(|agent| has_required_mcp_servers(agent, available_servers))
        .cloned()
        .collect()
}

/// Check for and initialize agent memory from project snapshots.
///
/// Maps to: CC `loadAgentsDir.ts:262-294` `initializeAgentMemorySnapshots`.
///
/// CC awaits a `Promise.all` over `agents.map(async …)`; the per-agent bodies
/// are independent, so the sequential loop is equivalent. Every other detail is
/// CC's: the `agent.memory !== 'user'` early return (CC :267) means project- and
/// local-scope memory is never snapshot-initialized, and the `switch` has only
/// `initialize` and `prompt-update` arms — no default, so `none` does nothing.
///
/// Deviation (pre-existing, MODULE_MAP `tools/AgentTool/agentMemorySnapshot.ts`):
/// the leaves take `cwd` explicitly where CC's `getSnapshotDirForAgent` reads
/// `getCwd()` (`agentMemorySnapshot.ts:32`). The loader's own `cwd` is passed
/// through — the same value CC's caller uses for `loadMarkdownFilesForSubdir`.
fn initialize_agent_memory_snapshots(agents: &mut [AgentDefinition], cwd: &Path) {
    for agent in agents.iter_mut() {
        // CC :267 `if (agent.memory !== 'user') return` — user scope only.
        if agent.memory != Some(AgentMemoryScope::User) {
            continue;
        }
        let result = super::agent_memory_snapshot::check_agent_memory_snapshot(
            &agent.agent_type,
            AgentMemoryScope::User,
            cwd,
        );
        match result.action {
            // CC :273-282.
            super::agent_memory_snapshot::AgentMemorySnapshotAction::Initialize => {
                crate::utils::debug::log_for_debugging(&format!(
                    "Initializing {} memory from project snapshot",
                    agent.agent_type
                ));
                let Some(snapshot_timestamp) = result.snapshot_timestamp.as_deref() else {
                    // CC asserts with `result.snapshotTimestamp!`; the check
                    // helper always sets it on this arm
                    // (`agentMemorySnapshot.ts:125`).
                    continue;
                };
                super::agent_memory_snapshot::initialize_from_snapshot(
                    &agent.agent_type,
                    AgentMemoryScope::User,
                    cwd,
                    snapshot_timestamp,
                );
            }
            // CC :283-290 — record the pending update and log; the local memory
            // is left untouched until the user answers the startup dialog.
            super::agent_memory_snapshot::AgentMemorySnapshotAction::PromptUpdate => {
                let Some(snapshot_timestamp) = result.snapshot_timestamp else {
                    continue;
                };
                // CC :284-286 sets the field, then :287-289 logs — same order.
                agent.pending_snapshot_update = Some(PendingSnapshotUpdate {
                    snapshot_timestamp: snapshot_timestamp.clone(),
                });
                crate::utils::debug::log_for_debugging(&format!(
                    "Newer snapshot available for {} memory (snapshot: {snapshot_timestamp})",
                    agent.agent_type
                ));
            }
            // CC's switch has no default arm.
            super::agent_memory_snapshot::AgentMemorySnapshotAction::None => {}
        }
    }
}

/// Maps to CC `loadAgentsDir.ts#parseAgentFromJson`.
pub fn parse_agent_from_json(
    name: &str,
    definition: &serde_json::Value,
    source: AgentDefinitionSource,
) -> Option<AgentDefinition> {
    // Maps to: CC `loadAgentsDir.ts:73-99` `AgentJsonSchema` +
    // `:445-516` `parseAgentFromJson` — `.parse()` is ALL-OR-NOTHING: any
    // present-but-invalid field rejects the whole agent (returns null); an
    // absent optional field is fine; unknown keys are ignored (no .strict()).
    // Zod's exact constraints: description/prompt are `min(1)` with NO trim
    // (a lone space passes), enums match exactly (no trim), initialPrompt is
    // a plain string (the empty string passes the schema; the construction
    // spread at :503 then drops it).
    let object = definition.as_object()?;
    let description = object.get("description")?.as_str()?;
    if description.is_empty() {
        return None;
    }
    let prompt = object.get("prompt")?.as_str()?;
    if prompt.is_empty() {
        return None;
    }

    // Every optional field: None when absent, Some(value) when valid,
    // early-return None when present but invalid.
    fn string_array(value: &serde_json::Value) -> Option<Vec<String>> {
        value
            .as_array()?
            .iter()
            .map(|item| item.as_str().map(ToOwned::to_owned))
            .collect()
    }

    let raw_tools = match object.get("tools") {
        None => None,
        Some(value) => Some(string_array(value)?),
    };
    let raw_disallowed = match object.get("disallowedTools") {
        None => None,
        Some(value) => Some(string_array(value)?),
    };
    let skills = match object.get("skills") {
        None => None,
        Some(value) => Some(string_array(value)?),
    };
    let model = match object.get("model") {
        None => None,
        Some(value) => Some(parse_model_json_value(value)?),
    };
    let effort = match object.get("effort") {
        None => None,
        Some(value) => Some(parse_effort_json_value(value)?),
    };
    let permission_mode = match object.get("permissionMode") {
        None => None,
        Some(value) => Some(match value.as_str()? {
            "default" => PermissionMode::Default,
            "acceptEdits" => PermissionMode::AcceptEdits,
            "plan" => PermissionMode::Plan,
            "dontAsk" => PermissionMode::DontAsk,
            "bypassPermissions" => PermissionMode::BypassPermissions,
            // CC PERMISSION_MODES includes 'auto' when TRANSCRIPT_CLASSIFIER
            // is on (types/permissions.ts:33-38) — ON in production builds.
            "auto" => PermissionMode::Auto,
            _ => return None,
        }),
    };
    let mcp_servers = match object.get("mcpServers") {
        None => None,
        Some(value) => Some(parse_agent_mcp_servers_from_json_value(value)?),
    };
    let hooks = match object.get("hooks") {
        None => None,
        Some(value) => Some(parse_hooks_config_json_value(value)?),
    };
    let max_turns = match object.get("maxTurns") {
        None => None,
        Some(value) => {
            let turns = value
                .as_u64()
                .and_then(|value| u32::try_from(value).ok())
                .filter(|value| *value > 0)?;
            Some(turns)
        }
    };
    let initial_prompt = match object.get("initialPrompt") {
        None => None,
        Some(value) => Some(value.as_str()?.to_string()),
    };
    let background = match object.get("background") {
        None => false,
        Some(value) => value.as_bool()?,
    };
    let memory = match object.get("memory") {
        None => None,
        Some(value) => Some(parse_agent_memory_scope(value.as_str()?)?),
    };
    let isolation = match object.get("isolation") {
        None => None,
        Some(value) => {
            // Zod enum, exact match: external builds accept only 'worktree';
            // 'remote' is ant-only — anything else rejects the agent.
            let value = value.as_str()?;
            let audience = crate::utils::build_profile::build_audience();
            let valid = value == "worktree"
                || (value == "remote"
                    && crate::utils::build_profile::audience_has_internal_capability(
                        audience,
                        crate::utils::build_profile::InternalCapability::Tools,
                    ));
            if !valid {
                return None;
            }
            Some(value.to_string())
        }
    };

    let mut agent = AgentDefinition::new(name, description, source);
    agent.system_prompt = Some(prompt.to_string());
    agent.tools = parse_agent_tools_from_validated(raw_tools);
    agent.disallowed_tools =
        raw_disallowed.and_then(|tools| parse_agent_tools_from_validated(Some(tools)));
    // CC construction spreads (:495-503): `mcpServers.length > 0` (:495),
    // `skills.length > 0` (:500), and truthy `initialPrompt` (:503) —
    // schema-valid empties pass zod but never reach the definition.
    agent.skills = skills.filter(|skills| !skills.is_empty());
    agent.model = model;
    agent.effort = effort;
    agent.permission_mode = permission_mode;
    agent.mcp_servers = mcp_servers.filter(|servers| !servers.is_empty());
    agent.hooks = hooks;
    agent.max_turns = max_turns;
    agent.initial_prompt = initial_prompt.filter(|prompt| !prompt.is_empty());
    agent.background = background;
    agent.memory = memory;
    // CC :453-458: inject memory tools only when memory is set AND tools were
    // explicitly declared (`tools !== undefined`).
    if agent.memory.is_some()
        && agent.tools.is_some()
        && crate::memdir::paths::is_auto_memory_enabled()
    {
        inject_agent_memory_tools(&mut agent.tools);
    }
    agent.isolation = isolation;

    Some(agent)
}

/// Maps to CC `loadAgentsDir.ts#parseAgentsFromJson`.
pub fn parse_agents_from_json(
    agents_json: &serde_json::Value,
    source: AgentDefinitionSource,
) -> Vec<AgentDefinition> {
    // Maps to: CC `loadAgentsDir.ts:521-536` `parseAgentsFromJson` —
    // `AgentsJsonSchema().parse()` validates the WHOLE record first
    // (`z.record(z.string(), AgentJsonSchema())`): one invalid definition
    // throws and the entire result is [].
    let Some(object) = agents_json.as_object() else {
        return Vec::new();
    };
    object
        .iter()
        .map(|(name, definition)| parse_agent_from_json(name, definition, source))
        .collect::<Option<Vec<_>>>()
        .unwrap_or_default()
}

/// Maps to CC `main.tsx` CLI `--agents` merge after `parseAgentsFromJson`.
fn cli_flag_agents_from_bootstrap() -> Vec<AgentDefinition> {
    crate::bootstrap::state::get_cli_agents_json()
        .as_ref()
        .map(|agents_json| parse_agents_from_json(agents_json, AgentDefinitionSource::FlagSettings))
        .unwrap_or_default()
}

/// Maps to: CC `tools/AgentTool/loadAgentsDir.ts#parseAgentFromMarkdown`.
fn parse_agent_from_markdown(
    markdown_file: &crate::utils::markdown_config_loader::MarkdownFile,
    audience: crate::utils::build_profile::BuildAudience,
) -> Option<AgentDefinition> {
    let frontmatter = &markdown_file.frontmatter;
    // CC :549-562: `!agentType || typeof agentType !== 'string'` — JS
    // truthiness with NO trim: the empty string is rejected, a lone space is
    // a valid agent type, and the raw (untrimmed) value is stored.
    let agent_type = frontmatter.get("name")?.as_str()?;
    if agent_type.is_empty() {
        return None;
    }
    let when_to_use = frontmatter.get("description")?.as_str()?;
    if when_to_use.is_empty() {
        return None;
    }
    let source = match markdown_file.source {
        crate::utils::markdown_config_loader::MarkdownConfigSource::PolicySettings => {
            AgentDefinitionSource::PolicySettings
        }
        crate::utils::markdown_config_loader::MarkdownConfigSource::UserSettings => {
            AgentDefinitionSource::UserSettings
        }
        crate::utils::markdown_config_loader::MarkdownConfigSource::ProjectSettings => {
            AgentDefinitionSource::ProjectSettings
        }
    };

    let mut agent = AgentDefinition::new(agent_type, when_to_use.replace("\\n", "\n"), source);
    agent.filename = markdown_file
        .file_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(ToOwned::to_owned);
    agent.base_dir = Some(markdown_file.base_dir.clone());
    // Maps to: CC `loadAgentsDir.ts:713` markdown-only `content.trim()`.
    // ECMAScript trim includes FEFF but excludes Unicode White_Space's 0085;
    // Rust str::trim would change successful prompt bytes at both boundaries.
    agent.system_prompt = Some(
        markdown_file
            .content
            .trim_matches(|c| {
                matches!(c,
                    '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}' |
                    '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' |
                    '\u{205f}' | '\u{3000}' | '\u{feff}'
                )
            })
            .to_string(),
    );
    agent.model = frontmatter.get("model").and_then(parse_model_json_value);
    // Maps to: CC `loadAgentsDir.ts:624-626` — the markdown path runs the
    // LENIENT `parseEffortValue` (effort.ts:71-87), NOT the strict JSON zod
    // union: `effort: HIGH` lowercases to a level, `effort: "3"` parseInts.
    agent.effort = frontmatter
        .get("effort")
        .and_then(parse_effort_frontmatter_value);
    agent.tools = crate::utils::markdown_config_loader::parse_agent_tools_from_frontmatter(
        frontmatter.get("tools"),
    );
    agent.disallowed_tools =
        crate::utils::markdown_config_loader::parse_agent_tools_from_frontmatter(
            frontmatter.get("disallowedTools"),
        );
    agent.skills = Some(
        crate::utils::markdown_config_loader::parse_slash_command_tools_from_frontmatter(
            frontmatter.get("skills"),
        ),
    );
    // CC loadAgentsDir.ts:735-737: `AGENT_COLORS.includes(color)` — an EXACT
    // match against the eight lowercase names (no trim, no case-fold); an
    // invalid color never enters the definition.
    agent.color = frontmatter
        .get("color")
        .and_then(serde_json::Value::as_str)
        .filter(|value| {
            crate::tools::agent_tool::agent_color_manager::is_valid_agent_color_name(value)
        })
        .map(ToOwned::to_owned);
    // CC `parseAgentFromMarkdown` never reads criticalSystemReminder from
    // frontmatter — the field exists on the TYPE (loadAgentsDir.ts:121) for
    // built-ins/SDK/forkedAgent overrides only; a frontmatter read was an
    // invented input surface custom markdown agents don't have in CC.
    // Maps to: CC `loadAgentsDir.ts:693-708` + :722-724 — the markdown path
    // is element-wise `safeParse`: an invalid item is logged and SKIPPED
    // (valid siblings and the agent survive), a non-array value leaves the
    // field undefined, and the construction spread keeps the field only when
    // `length > 0`. The JSON path's all-or-nothing aggregation
    // (`parse_agent_mcp_servers_from_json_value`) must NOT be used here.
    agent.mcp_servers = frontmatter
        .get("mcpServers")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(parse_agent_mcp_server_spec_json)
                .collect::<Vec<_>>()
        })
        .filter(|servers| !servers.is_empty());
    agent.hooks = frontmatter
        .get("hooks")
        .and_then(parse_hooks_config_json_value);
    agent.permission_mode = frontmatter
        .get("permissionMode")
        .and_then(serde_json::Value::as_str)
        // CC :638-640 includes-check is EXACT (no trim).
        .and_then(|value| match value {
            "default" => Some(PermissionMode::Default),
            "acceptEdits" => Some(PermissionMode::AcceptEdits),
            "plan" => Some(PermissionMode::Plan),
            "dontAsk" => Some(PermissionMode::DontAsk),
            "bypassPermissions" => Some(PermissionMode::BypassPermissions),
            // CC PERMISSION_MODES includes 'auto' when TRANSCRIPT_CLASSIFIER
            // is on (types/permissions.ts:33-38) — ON in production builds.
            "auto" => Some(PermissionMode::Auto),
            _ => None,
        });
    agent.max_turns = crate::utils::frontmatter_parser::parse_positive_int_from_frontmatter(
        frontmatter.get("maxTurns"),
    );
    agent.background = match frontmatter.get("background") {
        Some(serde_json::Value::Bool(true)) => true,
        Some(serde_json::Value::String(value)) if value == "true" => true,
        _ => false,
    };
    agent.memory = frontmatter
        .get("memory")
        .and_then(serde_json::Value::as_str)
        .and_then(parse_agent_memory_scope);
    if agent.memory.is_some() && crate::memdir::paths::is_auto_memory_enabled() {
        inject_agent_memory_tools(&mut agent.tools);
    }
    // CC :687-690: trim is only the emptiness TEST — the ORIGINAL untrimmed
    // value is stored.
    agent.initial_prompt = frontmatter
        .get("initialPrompt")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned);
    agent.isolation = frontmatter
        .get("isolation")
        .and_then(serde_json::Value::as_str)
        // CC :609-615 compares EXACT values (no trim).
        .and_then(|value| {
            (value == "worktree"
                || (value == "remote"
                    && crate::utils::build_profile::audience_has_internal_capability(
                        audience,
                        crate::utils::build_profile::InternalCapability::Tools,
                    )))
            .then(|| value.to_string())
        });
    Some(agent)
}

fn parse_error_for_frontmatter(
    frontmatter: &crate::utils::frontmatter_parser::FrontmatterData,
) -> String {
    // Maps to: CC `loadAgentsDir.ts:403-416` `getParseError` — truthy-string
    // checks (a truthy non-string `name`, e.g. a number, hits the name
    // branch), three exact strings.
    let truthy_string = |key: &str| {
        frontmatter
            .get(key)
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| !value.is_empty())
    };
    if !truthy_string("name") {
        return "Missing required \"name\" field in frontmatter".to_string();
    }
    if !truthy_string("description") {
        return "Missing required \"description\" field in frontmatter".to_string();
    }
    "Unknown parsing error".to_string()
}

/// JS truthiness over a frontmatter YAML value (CC `!frontmatter['name']`).
fn frontmatter_value_truthy(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => false,
        serde_json::Value::Bool(b) => *b,
        serde_json::Value::Number(n) => n.as_f64().is_some_and(|n| n != 0.0),
        serde_json::Value::String(s) => !s.is_empty(),
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => true,
    }
}

fn parse_model_json_value(value: &serde_json::Value) -> Option<String> {
    let model = value.as_str()?.trim();
    if model.is_empty() {
        None
    } else if model.eq_ignore_ascii_case("inherit") {
        Some("inherit".to_string())
    } else {
        Some(model.to_string())
    }
}

/// Maps to: CC `effort.ts:71-87` `parseEffortValue` applied to a markdown
/// frontmatter value (loadAgentsDir.ts:624-626). Lenient by design: an
/// integer number passes directly (`typeof value === 'number'` fast path);
/// everything else is `String(value).toLowerCase()` → named level or
/// `parseInt` (so `HIGH` → "high", `"3"` → 3, `3.5` → "3.5" → 3).
/// Documented seam: CC's `String(value)` also stringifies arrays
/// (`[3]` → "3" → 3) and objects ("[object Object]" → undefined); Rust
/// returns `None` for both container shapes — only the single-element
/// numeric-array corner diverges.
fn parse_effort_frontmatter_value(
    value: &serde_json::Value,
) -> Option<crate::utils::effort::EffortValue> {
    match value {
        serde_json::Value::String(text) => crate::utils::effort::parse_effort_value(text),
        serde_json::Value::Number(number) => {
            if let Some(int) = number.as_i64() {
                return Some(crate::utils::effort::EffortValue::Numeric(int));
            }
            crate::utils::effort::parse_effort_value(&number.to_string())
        }
        serde_json::Value::Bool(flag) => {
            crate::utils::effort::parse_effort_value(if *flag { "true" } else { "false" })
        }
        _ => None,
    }
}

/// CC JSON zod: `z.union([z.enum(EFFORT_LEVELS), z.number().int()])` — the
/// string arm is the EXACT four lowercase levels ("HIGH"/"3" reject the
/// agent); the lenient `parseEffortValue` coercion belongs to the markdown
/// path only (see [`parse_effort_frontmatter_value`]).
fn parse_effort_json_value(value: &serde_json::Value) -> Option<crate::utils::effort::EffortValue> {
    if let Some(level) = value.as_str() {
        return match level {
            "low" | "medium" | "high" | "xhigh" | "max" => {
                crate::utils::effort::parse_effort_value(level)
            }
            _ => None,
        };
    }
    // z.number().int(): a float rejects.
    if value.is_i64() || value.is_u64() {
        return value
            .as_i64()
            .map(crate::utils::effort::EffortValue::Numeric);
    }
    None
}

/// Maps to: CC `parseAgentToolsFromFrontmatter` applied to an
/// already-schema-validated string array (`parseAgentFromJson` runs zod
/// first, then this projection — a `*` entry means "all tools" and yields
/// undefined).
fn parse_agent_tools_from_validated(tools: Option<Vec<String>>) -> Option<Vec<String>> {
    let tools = tools?;
    let parsed = crate::utils::permissions::permission_setup::parse_tool_list_from_cli(&tools);
    if parsed.iter().any(|tool| tool == "*") {
        None
    } else {
        Some(parsed)
    }
}

/// CC `HooksSchema` is a partialRecord — `{}` is schema-valid and the object
/// spread keeps it on the agent (an object is always truthy). No emptiness
/// filter.
fn parse_hooks_config_json_value(
    value: &serde_json::Value,
) -> Option<crate::services::hooks::HooksConfig> {
    serde_json::from_value::<crate::services::hooks::HooksConfig>(value.clone()).ok()
}

/// CC `z.array(AgentMcpServerSpecSchema())` — any invalid element fails the
/// whole array (and with it the agent); an EMPTY array is schema-valid (the
/// spread's `length > 0` then leaves the field undefined — handled at the
/// construction site). References are plain `z.string()` (no trim, no
/// emptiness constraint).
fn parse_agent_mcp_servers_from_json_value(
    value: &serde_json::Value,
) -> Option<Vec<AgentMcpServerSpec>> {
    let items = value.as_array()?;
    items.iter().map(parse_agent_mcp_server_spec_json).collect()
}

fn parse_agent_mcp_server_spec_json(value: &serde_json::Value) -> Option<AgentMcpServerSpec> {
    if let Some(name) = value.as_str() {
        return Some(AgentMcpServerSpec::Reference(name.to_string()));
    }
    let object = value.as_object()?;
    // Carrier limit: CC's inline member is `z.record(name, config)` (0..N
    // keys pass the schema; the type comment says single-name). The Rust
    // enum carries exactly one name — multi-key/empty inline objects are a
    // documented carrier seam, kept as the single-key projection.
    if object.len() != 1 {
        return None;
    }
    let (name, config_value) = object.iter().next()?;
    let config =
        serde_json::from_value::<crate::utils::config::McpServerConfig>(config_value.clone())
            .ok()?;
    Some(AgentMcpServerSpec::Inline {
        name: name.to_string(),
        config: ScopedMcpServerConfig::from_config(
            crate::services::mcp::types::ConfigScope::Dynamic,
            &config,
        ),
    })
}

/// Maps to CC `loadAgentsDir.ts` memory scope validation against
/// `['user', 'project', 'local']`.
fn parse_agent_memory_scope(value: &str) -> Option<AgentMemoryScope> {
    // CC: JSON `z.enum(['user','project','local'])` and the markdown
    // includes-check are both EXACT — no trim on either path.
    match value {
        "user" => Some(AgentMemoryScope::User),
        "project" => Some(AgentMemoryScope::Project),
        "local" => Some(AgentMemoryScope::Local),
        _ => None,
    }
}

/// Maps to CC `loadAgentsDir.ts` memory-enabled tool injection for
/// `Write`/`Edit`/`Read` when an agent has a constrained tool set.
fn inject_agent_memory_tools(tools: &mut Option<Vec<String>>) {
    let Some(tools) = tools.as_mut() else {
        return;
    };
    for tool in [
        crate::tools::file_write_tool::prompt::FILE_WRITE_TOOL_NAME,
        crate::tools::file_edit_tool::constants::FILE_EDIT_TOOL_NAME,
        crate::tools::file_read_tool::prompt::FILE_READ_TOOL_NAME,
    ] {
        if !tools.iter().any(|existing| existing == tool) {
            tools.push(tool.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, HOME_VAR, TEST_ENV_LOCK, TestEnvGuard};
    use std::fs;
    use std::io::Write;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cometix-agent-definitions-{name}-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    fn write_file(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("parent dir");
        }
        let mut file = fs::File::create(path).expect("create file");
        file.write_all(content.as_bytes()).expect("write file");
    }

    /// Pins the loader's environment inputs: user agents come from
    /// `config_home`, the project walk stops at `home`, a host's
    /// `CLAUDE_CODE_SIMPLE` (CC `--bare` exports it) is cleared, and the
    /// managed root points at a directory that does not exist. Unsetting the
    /// managed override would fall back to the machine's real managed root
    /// (`/Library/Application Support/ClaudeCode` on macOS), whose agents and
    /// `managed-settings.json` would then reach the test. The caller holds
    /// `TEST_ENV_LOCK`.
    fn pin_loader_env(home: &Path, config_home: &Path) -> [EnvVarGuard; 4] {
        [
            EnvVarGuard::set("CLAUDE_CONFIG_DIR", config_home),
            EnvVarGuard::set(HOME_VAR, home),
            EnvVarGuard::unset("CLAUDE_CODE_SIMPLE"),
            EnvVarGuard::set(
                "CLAUDE_CODE_MANAGED_SETTINGS_PATH",
                config_home.join("missing-managed-root"),
            ),
        ]
    }

    /// CC `markdownConfigLoader.ts:238` stops the project walk at
    /// `homedir()`: an agents directory above the home directory is never
    /// read.
    #[test]
    fn project_walk_stops_at_homedir() {
        let root = temp_dir("home-stop");
        let home = root.join("home");
        let cwd = home.join("project");
        let agent = |name: &str| format!("---\nname: {name}\ndescription: d\n---\nBody");
        write_file(&cwd.join(".claude/agents/inside.md"), &agent("inside"));
        write_file(&root.join(".claude/agents/above.md"), &agent("above"));

        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _env = pin_loader_env(&home, &root.join("config"));
        let result = get_agent_definitions_with_overrides(&cwd);

        let names = result
            .active_agents
            .iter()
            .map(|agent| agent.agent_type.as_str())
            .collect::<Vec<_>>();
        assert!(names.contains(&"inside"), "{names:?}");
        assert!(!names.contains(&"above"), "{names:?}");
        let _ = fs::remove_dir_all(root);
    }

    /// CC `loadAgentsDir.ts:624-626` markdown effort is the LENIENT
    /// `parseEffortValue` (effort.ts:71-87), and :693-708/:722-724 markdown
    /// mcpServers is element-wise safeParse-skip with a `length > 0` spread —
    /// exercised through the production markdown load path (the JSON path's
    /// strict rules must not leak in here).
    #[test]
    fn markdown_path_keeps_lenient_effort_and_skips_bad_mcp_server_elements() {
        let root = temp_dir("markdown-lenient");
        let config_home = root.join("config");
        write_file(
            &config_home.join("agents/shouty.md"),
            "---\nname: shouty\ndescription: Uppercase effort\neffort: HIGH\nmcpServers:\n  - docs\n  - 7\n  - other\n---\nBody",
        );
        write_file(
            &config_home.join("agents/numeric.md"),
            "---\nname: numeric\ndescription: Numeric-string effort\neffort: \"3\"\nmcpServers:\n  - 1\n  - 2\n---\nBody",
        );

        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _env = pin_loader_env(&root, &config_home);
        let _auto_memory = EnvVarGuard::set("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "false");
        let result = get_agent_definitions_with_overrides(&root);

        let shouty = result
            .active_agents
            .iter()
            .find(|agent| agent.agent_type == "shouty")
            .expect("shouty agent survives");
        // `HIGH` lowercases to a level; the strict JSON arm would drop it.
        assert_eq!(
            shouty.effort,
            Some(crate::utils::effort::EffortValue::Named("high".to_string()))
        );
        // The invalid element `7` is skipped; valid siblings survive.
        assert_eq!(
            shouty.mcp_servers.as_ref().map(|servers| servers
                .iter()
                .map(|spec| match spec {
                    AgentMcpServerSpec::Reference(name) => name.as_str(),
                    AgentMcpServerSpec::Inline { name, .. } => name.as_str(),
                })
                .collect::<Vec<_>>()),
            Some(vec!["docs", "other"])
        );

        let numeric = result
            .active_agents
            .iter()
            .find(|agent| agent.agent_type == "numeric")
            .expect("numeric agent survives");
        // `"3"` parseInts on the markdown path (JSON rejects the string arm).
        assert_eq!(
            numeric.effort,
            Some(crate::utils::effort::EffortValue::Numeric(3))
        );
        // All elements invalid → empty result → `length > 0` omits the field.
        assert_eq!(numeric.mcp_servers, None);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn parses_custom_agent_markdown_for_status_notice_snapshot() {
        let root = temp_dir("parse");
        let config_home = root.join("config");
        write_file(
            &config_home.join("agents/reviewer.md"),
            "---\nname: reviewer\ndescription: Review code changes\\nwith tests\ncolor: blue\nmodel: INHERIT\ntools: Read, Grep Bash\ndisallowedTools: Bash\nskills: audit, docs\nmemory: project\nmcpServers:\n  - docs\n  - local-agent:\n      type: stdio\n      command: node\n      args:\n        - server.js\nhooks:\n  Stop:\n    - matcher: \"\"\n      hooks:\n        - command: echo stop\n          timeout: 5\npermissionMode: plan\neffort: high\nmaxTurns: 4\nbackground: true\ninitialPrompt: Start here\nisolation: worktree\n---\nPrompt body is not executed.",
        );
        write_file(
            &config_home.join("agents/notes.md"),
            "# Reference doc without agent frontmatter",
        );
        write_file(
            &config_home.join("agents/broken.md"),
            "---\nname: broken\n---\nMissing description",
        );

        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _env = pin_loader_env(&root, &config_home);
        let _auto_memory = EnvVarGuard::set("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "false");
        let result = get_agent_definitions_with_overrides(&root);

        let reviewer = result
            .active_agents
            .iter()
            .find(|agent| agent.agent_type == "reviewer")
            .expect("custom reviewer agent");
        assert_eq!(reviewer.when_to_use, "Review code changes\nwith tests");
        assert_eq!(
            reviewer.system_prompt.as_deref(),
            Some("Prompt body is not executed.")
        );
        assert_eq!(reviewer.source, AgentDefinitionSource::UserSettings);
        assert_eq!(reviewer.model.as_deref(), Some("inherit"));
        assert_eq!(
            reviewer
                .tools
                .as_ref()
                .map(|items| items.iter().map(String::as_str).collect::<Vec<_>>()),
            Some(vec!["Read", "Grep", "Bash", "Write", "Edit"])
        );
        assert_eq!(
            reviewer
                .disallowed_tools
                .as_ref()
                .map(|items| items.iter().map(String::as_str).collect::<Vec<_>>()),
            Some(vec!["Bash"])
        );
        assert_eq!(
            reviewer
                .skills
                .as_ref()
                .map(|items| items.iter().map(String::as_str).collect::<Vec<_>>()),
            Some(vec!["audit", "docs"])
        );
        let mcp_servers = reviewer.mcp_servers.as_ref().expect("mcp servers");
        assert!(matches!(
            &mcp_servers[0],
            AgentMcpServerSpec::Reference(name) if name == "docs"
        ));
        assert!(matches!(
            &mcp_servers[1],
            AgentMcpServerSpec::Inline { name, config }
                if name == "local-agent"
                    && config.scope == crate::services::mcp::types::ConfigScope::Dynamic
                    && config.command.as_deref() == Some("node")
                    && config.args == vec!["server.js"]
        ));
        let hooks = reviewer.hooks.as_ref().expect("frontmatter hooks");
        assert_eq!(hooks["Stop"][0].hooks[0].command, "echo stop");
        assert_eq!(reviewer.permission_mode, Some(PermissionMode::Plan));
        assert_eq!(
            reviewer.effort,
            Some(crate::utils::effort::EffortValue::Named("high".to_string()))
        );
        assert_eq!(reviewer.max_turns, Some(4));
        assert!(reviewer.background);
        assert_eq!(reviewer.initial_prompt.as_deref(), Some("Start here"));
        assert_eq!(reviewer.memory, Some(AgentMemoryScope::Project));
        assert_eq!(reviewer.isolation.as_deref(), Some("worktree"));
        assert_eq!(result.failed_files.len(), 1);
        // CC getParseError exact copy (loadAgentsDir.ts:410-412).
        assert_eq!(
            result.failed_files[0].error,
            "Missing required \"description\" field in frontmatter"
        );
    }

    #[test]
    fn agent_tools_frontmatter_matches_official_cli_list_parser() {
        assert_eq!(
            crate::utils::markdown_config_loader::parse_agent_tools_from_frontmatter(Some(
                &serde_json::json!("Read, Bash(git commit, git status)"),
            )),
            Some(vec![
                "Read".to_string(),
                "Bash(git commit, git status)".to_string(),
            ])
        );
        assert_eq!(
            crate::utils::markdown_config_loader::parse_agent_tools_from_frontmatter(Some(
                &serde_json::json!(["Read Grep", "Bash(npm test, cargo test)"]),
            )),
            Some(vec![
                "Read".to_string(),
                "Grep".to_string(),
                "Bash(npm test, cargo test)".to_string(),
            ])
        );
        assert_eq!(
            crate::utils::markdown_config_loader::parse_agent_tools_from_frontmatter(Some(
                &serde_json::json!("*"),
            )),
            None
        );
        assert_eq!(
            crate::utils::markdown_config_loader::parse_slash_command_tools_from_frontmatter(None),
            Vec::<String>::new()
        );
    }

    #[test]
    fn parse_agent_from_json_matches_official_schema_and_memory_tool_injection() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _auto_memory = EnvVarGuard::set("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "false");

        let value = serde_json::json!({
            "description": "Review code",
            "prompt": "You review code.",
            "tools": ["Read, Bash(git commit, git status)"],
            "disallowedTools": ["WebFetch"],
            "skills": ["audit"],
            "model": "INHERIT",
            "effort": 77,
            "permissionMode": "plan",
            "mcpServers": [
                "docs",
                {"local-agent": {"type": "stdio", "command": "node", "args": ["server.js"]}}
            ],
            "hooks": {
                "PreToolUse": [{"matcher": "Bash", "hooks": [{"command": "echo pre", "timeout": 5}]}]
            },
            "maxTurns": 5,
            "initialPrompt": "Start",
            "background": true,
            "memory": "user",
            "isolation": "worktree"
        });

        let agent = parse_agent_from_json("reviewer", &value, AgentDefinitionSource::FlagSettings)
            .expect("json agent");
        assert_eq!(agent.agent_type, "reviewer");
        assert_eq!(agent.when_to_use, "Review code");
        assert_eq!(agent.system_prompt.as_deref(), Some("You review code."));
        assert_eq!(agent.source, AgentDefinitionSource::FlagSettings);
        assert_eq!(agent.model.as_deref(), Some("inherit"));
        assert_eq!(
            agent.effort,
            Some(crate::utils::effort::EffortValue::Numeric(77))
        );
        assert_eq!(agent.permission_mode, Some(PermissionMode::Plan));
        let hooks = agent.hooks.as_ref().expect("json hooks");
        assert_eq!(hooks["PreToolUse"][0].matcher.as_deref(), Some("Bash"));
        assert_eq!(hooks["PreToolUse"][0].hooks[0].command, "echo pre");
        assert_eq!(agent.max_turns, Some(5));
        assert!(agent.background);
        assert_eq!(agent.initial_prompt.as_deref(), Some("Start"));
        assert_eq!(agent.memory, Some(AgentMemoryScope::User));
        assert_eq!(agent.isolation.as_deref(), Some("worktree"));
        assert_eq!(
            agent.tools.as_ref().unwrap(),
            &vec![
                "Read".to_string(),
                "Bash(git commit, git status)".to_string(),
                "Write".to_string(),
                "Edit".to_string(),
            ]
        );
        assert_eq!(
            agent.disallowed_tools.as_deref(),
            Some(&["WebFetch".to_string()][..])
        );
        assert_eq!(agent.skills.as_deref(), Some(&["audit".to_string()][..]));
        let mcp_servers = agent.mcp_servers.as_ref().expect("mcp servers");
        assert!(matches!(&mcp_servers[0], AgentMcpServerSpec::Reference(name) if name == "docs"));
        assert!(matches!(
            &mcp_servers[1],
            AgentMcpServerSpec::Inline { name, config }
                if name == "local-agent"
                    && config.scope == crate::services::mcp::types::ConfigScope::Dynamic
                    && config.command.as_deref() == Some("node")
        ));

        // CC AgentJsonSchema `.parse()` is ALL-OR-NOTHING: a present-but-
        // invalid field rejects the entire agent, not just the field.
        for (key, bad) in [
            ("permissionMode", serde_json::json!("yolo")),
            ("maxTurns", serde_json::json!(0)),
            ("background", serde_json::json!("yes")),
            ("memory", serde_json::json!("global")),
            ("tools", serde_json::json!(["Read", 42])),
            ("isolation", serde_json::json!("submarine")),
        ] {
            let mut invalid = value.clone();
            invalid[key] = bad;
            assert!(
                parse_agent_from_json("reviewer", &invalid, AgentDefinitionSource::FlagSettings)
                    .is_none(),
                "invalid {key} must reject the whole agent"
            );
        }
        // The isolation enum is audience-shaped at BUILD time
        // (AgentTool.tsx:215-218 — the `"external" === 'ant'` residue is the
        // bundler's USER_TYPE --define): ant builds accept
        // ['worktree','remote'], external builds only ['worktree']. This row
        // used to sit in the all-or-nothing loop above, which pinned the
        // external arm as if it were audience-free.
        let mut remote = value.clone();
        remote["isolation"] = serde_json::json!("remote");
        let parsed =
            parse_agent_from_json("reviewer", &remote, AgentDefinitionSource::FlagSettings);
        if crate::utils::build_profile::has_internal_capability(
            crate::utils::build_profile::InternalCapability::Tools,
        ) {
            assert_eq!(
                parsed
                    .expect("ant builds accept isolation: remote")
                    .isolation
                    .as_deref(),
                Some("remote")
            );
        } else {
            assert!(parsed.is_none(), "external builds reject isolation: remote");
        }
        // zod `min(1)` has no trim — a lone-space description passes; the
        // empty string does not. initialPrompt is a plain string: "" passes
        // the schema (the construction spread drops it below).
        let mut spacey = value.clone();
        spacey["description"] = serde_json::json!(" ");
        spacey["initialPrompt"] = serde_json::json!("");
        let spacey_agent =
            parse_agent_from_json("reviewer", &spacey, AgentDefinitionSource::FlagSettings)
                .expect("space description passes min(1)");
        assert_eq!(spacey_agent.when_to_use, " ");
        // "" passes zod but the construction spread is truthy (:503) — the
        // field never reaches the definition.
        assert_eq!(spacey_agent.initial_prompt, None);
        let mut empty_desc = value.clone();
        empty_desc["description"] = serde_json::json!("");
        assert!(
            parse_agent_from_json("reviewer", &empty_desc, AgentDefinitionSource::FlagSettings)
                .is_none()
        );

        // CC :521-536: AgentsJsonSchema().parse() validates the WHOLE record
        // — one invalid definition empties the entire result.
        let parsed = parse_agents_from_json(
            &serde_json::json!({"reviewer": value, "broken": {"prompt": "missing desc"}}),
            AgentDefinitionSource::FlagSettings,
        );
        assert_eq!(parsed.len(), 0);
        let all_valid = parse_agents_from_json(
            &serde_json::json!({"reviewer": value}),
            AgentDefinitionSource::FlagSettings,
        );
        assert_eq!(all_valid.len(), 1);
        // Schema-valid empties pass zod but the construction spreads drop
        // them (hooks {} survives — objects are always truthy).
        let mut empties = value.clone();
        empties["skills"] = serde_json::json!([]);
        empties["mcpServers"] = serde_json::json!([]);
        empties["hooks"] = serde_json::json!({});
        let empties_agent =
            parse_agent_from_json("reviewer", &empties, AgentDefinitionSource::FlagSettings)
                .expect("schema-valid empties keep the agent");
        assert_eq!(empties_agent.skills, None);
        assert!(empties_agent.mcp_servers.is_none());
        assert!(empties_agent.hooks.is_some());
    }

    #[test]
    fn cli_flag_agents_merge_after_filesystem_agents_like_official_main() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::bootstrap::state::set_cli_agents_json(None);
        crate::bootstrap::state::set_inline_plugins(Vec::new());
        let root = temp_dir("cli-agents");
        let config_home = root.join("config");
        write_file(
            &config_home.join("agents/reviewer.md"),
            "---\nname: reviewer\ndescription: Filesystem reviewer\n---\nFilesystem prompt",
        );
        crate::bootstrap::state::set_cli_agents_json(Some(serde_json::json!({
            "reviewer": {
                "description": "CLI reviewer",
                "prompt": "CLI prompt"
            }
        })));

        let _env = pin_loader_env(&root, &config_home);
        let result = get_agent_definitions_with_overrides(&root);

        let active = result
            .active_agents
            .iter()
            .find(|agent| agent.agent_type == "reviewer")
            .expect("active reviewer");
        assert_eq!(active.source, AgentDefinitionSource::FlagSettings);
        assert_eq!(active.when_to_use, "CLI reviewer");
        assert!(
            result
                .all_agents
                .iter()
                .any(|agent| agent.agent_type == "reviewer"
                    && agent.source == AgentDefinitionSource::UserSettings)
        );
        assert!(
            result
                .all_agents
                .iter()
                .any(|agent| agent.agent_type == "reviewer"
                    && agent.source == AgentDefinitionSource::FlagSettings)
        );

        crate::bootstrap::state::set_cli_agents_json(None);
    }

    #[test]
    fn inline_plugin_agents_merge_before_filesystem_agents_like_official_loader() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        struct InlinePluginStateGuard;
        impl Drop for InlinePluginStateGuard {
            fn drop(&mut self) {
                crate::bootstrap::state::set_inline_plugins(Vec::new());
                crate::bootstrap::state::set_cli_agents_json(None);
            }
        }
        let _guard = InlinePluginStateGuard;
        crate::bootstrap::state::set_cli_agents_json(None);

        let root = temp_dir("inline-plugin-agents");
        let config_home = root.join("config");
        let plugin_root = root.join("plugin");
        write_file(
            &plugin_root.join(".claude-plugin/plugin.json"),
            r#"{"name":"review-pack"}"#,
        );
        write_file(
            &plugin_root.join("agents/reviewer.md"),
            "---\nname: reviewer\ndescription: Plugin reviewer\n---\nPlugin prompt",
        );
        write_file(
            &config_home.join("agents/reviewer.md"),
            "---\nname: review-pack:reviewer\ndescription: Filesystem reviewer\n---\nFilesystem prompt",
        );
        crate::bootstrap::state::set_inline_plugins(vec![plugin_root.clone()]);

        let _env = pin_loader_env(&root, &config_home);
        let result = get_agent_definitions_with_overrides(&root);

        let active = result
            .active_agents
            .iter()
            .find(|agent| agent.agent_type == "review-pack:reviewer")
            .expect("active reviewer");
        assert_eq!(active.source, AgentDefinitionSource::UserSettings);
        assert_eq!(active.when_to_use, "Filesystem reviewer");
        assert!(
            result
                .all_agents
                .iter()
                .any(|agent| agent.agent_type == "review-pack:reviewer"
                    && agent.source == AgentDefinitionSource::Plugin
                    && agent.plugin.as_deref() == Some("review-pack@inline"))
        );
        assert!(
            result
                .all_agents
                .iter()
                .any(|agent| agent.agent_type == "review-pack:reviewer"
                    && agent.source == AgentDefinitionSource::UserSettings)
        );
    }

    /// Unit test of the shared LENIENT parser only — the production markdown
    /// path is covered by
    /// `markdown_path_keeps_lenient_effort_and_skips_bad_mcp_server_elements`,
    /// which feeds `parse_agent_from_markdown` end to end.
    #[test]
    fn lenient_effort_parser_accepts_official_levels_and_integers() {
        assert_eq!(
            crate::utils::effort::parse_effort_value("MEDIUM"),
            Some(crate::utils::effort::EffortValue::Named(
                "medium".to_string()
            ))
        );
        assert_eq!(
            crate::utils::effort::parse_effort_value("101"),
            Some(crate::utils::effort::EffortValue::Numeric(101))
        );
        assert_eq!(crate::utils::effort::parse_effort_value(""), None);
        assert_eq!(crate::utils::effort::parse_effort_value("invalid"), None);
    }

    #[test]
    fn isolation_frontmatter_honors_official_external_and_internal_remote_gate() {
        use crate::utils::build_profile::BuildAudience;
        use crate::utils::frontmatter_parser::FrontmatterData;
        use crate::utils::markdown_config_loader::{MarkdownConfigSource, MarkdownFile};

        let parse = |isolation: &str, audience| {
            let mut frontmatter = FrontmatterData::new();
            frontmatter.insert("name".to_string(), serde_json::json!("reviewer"));
            frontmatter.insert(
                "description".to_string(),
                serde_json::json!("Review changes"),
            );
            frontmatter.insert("isolation".to_string(), serde_json::json!(isolation));
            parse_agent_from_markdown(
                &MarkdownFile {
                    file_path: PathBuf::from("reviewer.md"),
                    base_dir: PathBuf::from("."),
                    frontmatter,
                    content: "Review carefully".to_string(),
                    source: MarkdownConfigSource::ProjectSettings,
                },
                audience,
            )
            .and_then(|agent| agent.isolation)
        };

        assert_eq!(
            parse("worktree", BuildAudience::External).as_deref(),
            Some("worktree")
        );
        assert_eq!(parse("remote", BuildAudience::External), None);
        assert_eq!(
            parse("remote", BuildAudience::AnthropicInternal).as_deref(),
            Some("remote")
        );
        assert_eq!(parse("invalid", BuildAudience::AnthropicInternal), None);
    }

    #[test]
    fn required_mcp_server_filter_matches_official_case_insensitive_patterns() {
        let mut docs_agent = AgentDefinition::new(
            "docs-reader",
            "Requires docs MCP",
            AgentDefinitionSource::ProjectSettings,
        );
        docs_agent.required_mcp_servers = Some(vec!["Docs".to_string(), "jira".to_string()]);
        let plain_agent = AgentDefinition::new(
            "plain",
            "No MCP requirements",
            AgentDefinitionSource::BuiltIn,
        );
        let available = vec!["company-docs-prod".to_string(), "jira-cloud".to_string()];

        assert!(has_required_mcp_servers(&docs_agent, &available));
        let filtered = filter_agents_by_mcp_requirements(
            &[docs_agent.clone(), plain_agent.clone()],
            &available,
        );
        assert_eq!(
            filtered
                .iter()
                .map(|agent| agent.agent_type.as_str())
                .collect::<Vec<_>>(),
            vec!["docs-reader", "plain"]
        );

        let missing = vec!["company-docs-prod".to_string()];
        assert!(!has_required_mcp_servers(&docs_agent, &missing));
        let filtered = filter_agents_by_mcp_requirements(&[docs_agent, plain_agent], &missing);
        assert_eq!(
            filtered
                .iter()
                .map(|agent| agent.agent_type.as_str())
                .collect::<Vec<_>>(),
            vec!["plain"]
        );
    }

    #[test]
    fn active_agents_preserve_official_source_override_order() {
        let all_agents = vec![
            AgentDefinition::new("reviewer", "built in", AgentDefinitionSource::BuiltIn),
            AgentDefinition::new("reviewer", "user", AgentDefinitionSource::UserSettings),
            AgentDefinition::new(
                "reviewer",
                "project",
                AgentDefinitionSource::ProjectSettings,
            ),
            AgentDefinition::new("reviewer", "managed", AgentDefinitionSource::PolicySettings),
            AgentDefinition::new("helper", "user", AgentDefinitionSource::UserSettings),
        ];

        let active = get_active_agents_from_list(&all_agents);

        let reviewer = active
            .iter()
            .find(|agent| agent.agent_type == "reviewer")
            .expect("reviewer active agent");
        assert_eq!(reviewer.when_to_use, "managed");
        assert_eq!(
            active
                .iter()
                .map(|agent| agent.agent_type.as_str())
                .collect::<Vec<_>>(),
            vec!["reviewer", "helper"]
        );
    }

    #[test]
    fn filesystem_loader_uses_managed_user_project_order_and_simple_mode() {
        let root = temp_dir("dirs");
        let config_home = root.join("config");
        let managed_root = root.join("managed");
        let cwd = root.join("repo/nested");
        fs::create_dir_all(&cwd).expect("cwd");
        fs::create_dir_all(root.join("repo/.git")).expect("git root");
        write_file(
            &managed_root.join(".claude/agents/reviewer.md"),
            "---\nname: reviewer\ndescription: Managed description\n---\nManaged",
        );
        write_file(
            &config_home.join("agents/reviewer.md"),
            "---\nname: reviewer\ndescription: User description\n---\nUser",
        );
        write_file(
            &cwd.join(".claude/agents/reviewer.md"),
            "---\nname: reviewer\ndescription: Nested project description\n---\nNested",
        );
        write_file(
            &root.join("repo/.claude/agents/reviewer.md"),
            "---\nname: reviewer\ndescription: Root project description\n---\nRoot",
        );

        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _env = pin_loader_env(&root, &config_home);
        // `get_managed_file_path` honours the override in every test build.
        let _managed = EnvVarGuard::set("CLAUDE_CODE_MANAGED_SETTINGS_PATH", &managed_root);
        let result = get_agent_definitions_with_overrides(&cwd);
        let reviewer = result
            .active_agents
            .iter()
            .find(|agent| agent.agent_type == "reviewer")
            .expect("reviewer active agent");
        assert_eq!(reviewer.when_to_use, "Managed description");
        assert_eq!(reviewer.source, AgentDefinitionSource::PolicySettings);

        let _simple = EnvVarGuard::set("CLAUDE_CODE_SIMPLE", "true");
        let simple = get_agent_definitions_with_overrides(&cwd);
        assert!(
            simple
                .active_agents
                .iter()
                .all(|agent| agent.source == AgentDefinitionSource::BuiltIn)
        );
    }

    #[test]
    fn frontmatter_parser_supports_quoted_and_block_descriptions() {
        let fields = crate::utils::frontmatter_parser::parse_frontmatter(
            "---\nname: 'planner'\ndescription: >\n  Plan: carefully\n  across files\n---\nBody",
        )
        .frontmatter;
        assert_eq!(
            fields.get("name").and_then(serde_json::Value::as_str),
            Some("planner")
        );
        assert_eq!(
            fields
                .get("description")
                .and_then(serde_json::Value::as_str),
            Some("Plan: carefully across files\n")
        );
    }

    /// Pins what the snapshot tests load through: the project walk stops at
    /// `home`, auto memory is forced on, and user-scope `get_agent_memory_dir`
    /// resolves under `config_home` (`agent_memory.rs:41`, `:170-174`).
    /// Without the last two the snapshot copy would land in whatever config
    /// home the run inherited.
    struct SnapshotEnv {
        _loader: [EnvVarGuard; 4],
        _remote_memory: EnvVarGuard,
        _auto_memory: EnvVarGuard,
        _lock: TestEnvGuard<'static>,
    }

    fn pin_snapshot_env(home: &Path, config_home: &Path) -> SnapshotEnv {
        let lock = TEST_ENV_LOCK.lock().unwrap();
        SnapshotEnv {
            _loader: pin_loader_env(home, config_home),
            _remote_memory: EnvVarGuard::unset("CLAUDE_CODE_REMOTE_MEMORY_DIR"),
            _auto_memory: EnvVarGuard::set("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "false"),
            _lock: lock,
        }
    }

    fn write_snapshot(cwd: &Path, agent_type: &str, updated_at: &str, payload: &str) {
        let snapshot_dir = cwd
            .join(".claude")
            .join("agent-memory-snapshots")
            .join(agent_type);
        write_file(
            &snapshot_dir.join("snapshot.json"),
            &format!("{{\"updatedAt\":\"{updated_at}\"}}"),
        );
        write_file(&snapshot_dir.join("MEMORY.md"), payload);
    }

    /// Regression: the definitions load must run CC's
    /// `initializeAgentMemorySnapshots` (`loadAgentsDir.ts:348-354` calling
    /// `:262-294`). Reverting the call site leaves the user-scope memory dir
    /// empty and this test fails on the first assert.
    ///
    /// Also pins CC :267 `if (agent.memory !== 'user') return`: the project-scope
    /// agent has an equally valid snapshot on disk and must be skipped.
    #[test]
    fn agent_definitions_load_initializes_user_scope_memory_from_project_snapshot() {
        let root = temp_dir("snapshot-initialize");
        let config_home = root.join("config");
        let _env = pin_snapshot_env(&root, &config_home);

        write_file(
            &config_home.join("agents/usermem.md"),
            "---\nname: usermem\ndescription: User-scope memory agent\nmemory: user\n---\nBody",
        );
        write_file(
            &config_home.join("agents/projectmem.md"),
            "---\nname: projectmem\ndescription: Project-scope memory agent\nmemory: project\n---\nBody",
        );
        write_snapshot(
            &root,
            "usermem",
            "2026-02-01T00:00:00Z",
            "- snapshot note\n",
        );
        write_snapshot(&root, "projectmem", "2026-02-01T00:00:00Z", "- skipped\n");

        let result = get_agent_definitions_with_overrides(&root);

        // CC :277-281 `initializeFromSnapshot` → copySnapshotToLocal +
        // saveSyncedMeta, into the USER memory dir (`$CLAUDE_CONFIG_DIR/agent-memory`).
        let user_memory = config_home.join("agent-memory/usermem");
        assert_eq!(
            fs::read_to_string(user_memory.join("MEMORY.md")).ok(),
            Some("- snapshot note\n".to_string()),
            "user-scope memory should be seeded from the project snapshot"
        );
        assert_eq!(
            fs::read_to_string(user_memory.join(".snapshot-synced.json")).ok(),
            Some("{\"syncedFrom\":\"2026-02-01T00:00:00Z\"}".to_string())
        );
        // `snapshot.json` itself is never copied (`agentMemorySnapshot.ts:68`).
        assert!(!user_memory.join("snapshot.json").exists());

        // The `initialize` arm does not set pendingSnapshotUpdate (CC :273-282).
        let usermem = result
            .active_agents
            .iter()
            .find(|agent| agent.agent_type == "usermem")
            .expect("usermem agent");
        assert_eq!(usermem.pending_snapshot_update, None);

        // CC :267 — project scope is filtered out before the check runs.
        assert!(!config_home.join("agent-memory/projectmem").exists());
        assert!(!root.join(".claude/agent-memory/projectmem").exists());
        let projectmem = result
            .active_agents
            .iter()
            .find(|agent| agent.agent_type == "projectmem")
            .expect("projectmem agent");
        assert_eq!(projectmem.pending_snapshot_update, None);

        let _ = fs::remove_dir_all(root);
    }

    /// Backs the safety argument for this port lacking CC's `memoize`
    /// (`loadAgentsDir.ts:296`): because every caller re-runs the load, the
    /// snapshot init must be self-cancelling. After the first initialize writes
    /// `.snapshot-synced.json`, `checkAgentMemorySnapshot` returns `none`
    /// (`agentMemorySnapshot.ts:124-143`), so a second load must not overwrite
    /// memory the user has since edited.
    #[test]
    fn repeated_definitions_load_does_not_re_copy_over_edited_agent_memory() {
        let root = temp_dir("snapshot-idempotent");
        let config_home = root.join("config");
        let _env = pin_snapshot_env(&root, &config_home);

        write_file(
            &config_home.join("agents/usermem.md"),
            "---\nname: usermem\ndescription: User-scope memory agent\nmemory: user\n---\nBody",
        );
        write_snapshot(
            &root,
            "usermem",
            "2026-02-01T00:00:00Z",
            "- snapshot note\n",
        );

        let user_memory = config_home.join("agent-memory/usermem");
        let _ = get_agent_definitions_with_overrides(&root);
        assert_eq!(
            fs::read_to_string(user_memory.join("MEMORY.md")).ok(),
            Some("- snapshot note\n".to_string()),
            "first load seeds from the snapshot"
        );

        // The user then edits their own memory. The snapshot on disk is unchanged.
        write_file(&user_memory.join("MEMORY.md"), "- hand-edited note\n");

        let second = get_agent_definitions_with_overrides(&root);
        assert_eq!(
            fs::read_to_string(user_memory.join("MEMORY.md")).ok(),
            Some("- hand-edited note\n".to_string()),
            "a repeat load must not clobber edited agent memory"
        );
        let usermem = second
            .active_agents
            .iter()
            .find(|agent| agent.agent_type == "usermem")
            .expect("usermem agent");
        assert_eq!(usermem.pending_snapshot_update, None);

        let _ = fs::remove_dir_all(root);
    }

    /// Regression for the other arm: CC :283-290 records
    /// `pendingSnapshotUpdate` and leaves local memory alone. Reverting the call
    /// site makes `pending_snapshot_update` stay `None` and this test fails.
    #[test]
    fn agent_definitions_load_records_pending_snapshot_update_without_touching_memory() {
        let root = temp_dir("snapshot-prompt-update");
        let config_home = root.join("config");
        let _env = pin_snapshot_env(&root, &config_home);

        write_file(
            &config_home.join("agents/usermem.md"),
            "---\nname: usermem\ndescription: User-scope memory agent\nmemory: user\n---\nBody",
        );
        write_snapshot(
            &root,
            "usermem",
            "2026-02-01T00:00:00Z",
            "- snapshot note\n",
        );
        // Existing local memory with no `.snapshot-synced.json` →
        // `checkAgentMemorySnapshot` returns `prompt-update`
        // (`agentMemorySnapshot.ts:124-141`).
        let user_memory = config_home.join("agent-memory/usermem");
        write_file(&user_memory.join("MEMORY.md"), "- local note\n");

        let result = get_agent_definitions_with_overrides(&root);

        let expected = Some(PendingSnapshotUpdate {
            snapshot_timestamp: "2026-02-01T00:00:00Z".to_string(),
        });
        let usermem = result
            .active_agents
            .iter()
            .find(|agent| agent.agent_type == "usermem")
            .expect("usermem agent");
        assert_eq!(usermem.pending_snapshot_update, expected);
        // CC mutates the `customAgents` element in place, so the same value is
        // visible through `allAgents` (the array is spread, not copied).
        let usermem_all = result
            .all_agents
            .iter()
            .find(|agent| agent.agent_type == "usermem")
            .expect("usermem in allAgents");
        assert_eq!(usermem_all.pending_snapshot_update, expected);

        // The prompt-update arm must not copy or mark anything.
        assert_eq!(
            fs::read_to_string(user_memory.join("MEMORY.md")).ok(),
            Some("- local note\n".to_string())
        );
        assert!(!user_memory.join(".snapshot-synced.json").exists());

        let _ = fs::remove_dir_all(root);
    }
}

#[cfg(test)]
mod prompt_contract_tests {
    //! Prompt-provider loading contracts from CC `tools/AgentTool/loadAgentsDir.ts`.

    use super::*;

    /// CC `loadAgentsDir.ts:473,481-487` preserves JSON prompt bytes, while
    /// `:713,726-732` trims markdown body bytes before storing its closure.
    #[test]
    fn loaded_system_prompt_matches_official_json_vs_markdown_whitespace() {
        let original = " \n\tReview carefully.\n ";
        for (body, expected) in [
            (original, "Review carefully."),
            (" \n\t", ""),
            ("\u{feff}Review\u{feff}", "Review"),
            ("\u{0085}Review\u{0085}", "\u{0085}Review\u{0085}"),
        ] {
            let json_agent = parse_agent_from_json(
                "reviewer",
                &serde_json::json!({"description": "Review", "prompt": body}),
                AgentDefinitionSource::FlagSettings,
            )
            .unwrap();
            // CC loadAgentsDir.ts:473 preserves JSON strings; :713 applies
            // ECMAScript trim only to markdown, including FEFF but not 0085.
            assert_eq!(json_agent.system_prompt.as_deref(), Some(body));
            let markdown = crate::utils::markdown_config_loader::MarkdownFile {
                file_path: "reviewer.md".into(),
                base_dir: ".".into(),
                frontmatter: [
                    ("name".to_string(), serde_json::json!("reviewer")),
                    ("description".to_string(), serde_json::json!("Review")),
                ]
                .into_iter()
                .collect(),
                content: body.to_string(),
                source: crate::utils::markdown_config_loader::MarkdownConfigSource::ProjectSettings,
            };
            let agent = parse_agent_from_markdown(
                &markdown,
                crate::utils::build_profile::BuildAudience::External,
            )
            .unwrap();
            assert_eq!(agent.system_prompt.as_deref(), Some(expected));
            assert_eq!(
                agent
                    .get_system_prompt(&crate::tool::ToolUseContext::default())
                    .as_deref(),
                Some(expected)
            );
        }
    }
}
