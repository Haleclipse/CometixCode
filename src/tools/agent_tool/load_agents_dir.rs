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
//! `<cwd>/.nyan/agent-memory-snapshots/<agentType>/snapshot.json` exists.

use super::agent_memory::AgentMemoryScope;
use super::built_in_agents::get_built_in_agents;
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
    /// Maps to CC `BaseAgentDefinition.hooks`.
    pub hooks: Option<crate::services::hooks::HooksConfig>,
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
    /// @nyan: the markdown file the agent was loaded from, so /agents opens
    /// and deletes that file instead of rebuilding a path from the name.
    pub file_path: Option<PathBuf>,
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
        if self.source != AgentDefinitionSource::BuiltIn
            && let Some(memory) = self.memory
            && crate::memdir::paths::is_auto_memory_enabled()
        {
            prompt.push_str("\n\n");
            prompt.push_str(&super::agent_memory::load_agent_memory_prompt(
                &self.agent_type,
                memory,
                &context.effective_cwd(),
            ));
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
            tools: None,
            disallowed_tools: None,
            skills: None,
            color: None,
            omit_claude_md: false,
            critical_system_reminder_experimental: None,
            hooks: None,
            max_turns: None,
            background: false,
            initial_prompt: None,
            memory: None,
            isolation: None,
            pending_snapshot_update: None,
            filename: None,
            base_dir: None,
            file_path: None,
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
    /// @nyan: `None` for a markdown file (`path` is that file); `Plugin` or
    /// `FlagSettings` for an agent dropped for an invalid `model:`, whose
    /// `path` then holds the agent name.
    pub source: Option<AgentDefinitionSource>,
}

impl FailedAgentFile {
    /// What to show in /doctor and /agents: the file path, or a labelled
    /// agent name for sources that have no file.
    pub fn label(&self) -> String {
        match self.source {
            Some(AgentDefinitionSource::Plugin) => format!("plugin agent {}", self.path.display()),
            Some(AgentDefinitionSource::FlagSettings) => {
                format!("--agents agent {}", self.path.display())
            }
            _ => self.path.display().to_string(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentDefinitionsResult {
    /// Maps to CC `AgentDefinitionsResult.activeAgents`.
    pub active_agents: Vec<AgentDefinition>,
    /// Maps to CC `AgentDefinitionsResult.allAgents`.
    pub all_agents: Vec<AgentDefinition>,
    /// Maps to CC `AgentDefinitionsResult.failedFiles`.
    pub failed_files: Vec<FailedAgentFile>,
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

const SOURCE_PRIORITY: [AgentDefinitionSource; 5] = [
    AgentDefinitionSource::BuiltIn,
    AgentDefinitionSource::Plugin,
    AgentDefinitionSource::UserSettings,
    AgentDefinitionSource::ProjectSettings,
    AgentDefinitionSource::FlagSettings,
];

fn normalize_path_for_comparison(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Discovers project agent directories: checks `.nyan/agents` walking up from cwd to home/git_root.
/// Strictly does NOT read `.claude/agents`.
fn get_project_nyan_agent_dirs(cwd: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let home = crate::utils::node_os::homedir();
    let home = home.canonicalize().unwrap_or(home);
    let git_root = crate::utils::git::find_git_root(cwd);
    let mut current = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());

    loop {
        if normalize_path_for_comparison(&current) == normalize_path_for_comparison(&home) {
            break;
        }

        let candidate = current.join(".nyan").join("agents");
        if candidate.is_dir() {
            dirs.push(candidate);
        }

        if let Some(git_root) = &git_root
            && normalize_path_for_comparison(&current) == normalize_path_for_comparison(git_root)
        {
            break;
        }

        if !current.pop() {
            break;
        }
    }

    dirs
}

/// Loads agent markdown files only from `~/.nyan/agents` (user) and
/// `.nyan/agents` (project).
/// Project agents require workspace trust (untrusted => do not load, print mode untrusted => skip).
fn load_agent_markdown_files(
    cwd: &Path,
) -> Vec<crate::utils::markdown_config_loader::MarkdownFile> {
    let user_dir = crate::utils::env_utils::get_config_home_dir().join("agents");

    // Project agents are gated by workspace trust: untrusted => do not load.
    let is_trusted = crate::utils::config::check_has_trust_dialog_accepted()
        || crate::utils::config::is_path_trusted(cwd);

    let mut project_dirs = if is_trusted {
        get_project_nyan_agent_dirs(cwd)
    } else {
        Vec::new()
    };

    if is_trusted
        && let (Some(git_root), Some(canonical_root)) = (
            crate::utils::git::find_git_root(cwd),
            crate::utils::git::find_canonical_git_root(cwd),
        )
        && normalize_path_for_comparison(&git_root)
            != normalize_path_for_comparison(&canonical_root)
    {
        let worktree_subdir = git_root.join(".nyan").join("agents");
        let worktree_has_subdir = project_dirs.iter().any(|directory| {
            normalize_path_for_comparison(directory)
                == normalize_path_for_comparison(&worktree_subdir)
        });
        if !worktree_has_subdir {
            let main_subdir = canonical_root.join(".nyan").join("agents");
            if !project_dirs.iter().any(|directory| {
                normalize_path_for_comparison(directory)
                    == normalize_path_for_comparison(&main_subdir)
            }) {
                project_dirs.push(main_subdir);
            }
        }
    }

    let mut files = Vec::new();
    if crate::utils::settings::constants::is_setting_source_enabled(
        crate::utils::settings::constants::SettingSource::User,
    ) {
        files.extend(
            crate::utils::markdown_config_loader::load_markdown_files_from_dir(
                &user_dir,
                crate::utils::markdown_config_loader::MarkdownConfigSource::UserSettings,
            ),
        );
    }
    if crate::utils::settings::constants::is_setting_source_enabled(
        crate::utils::settings::constants::SettingSource::Project,
    ) {
        for project_dir in project_dirs {
            files.extend(
                crate::utils::markdown_config_loader::load_markdown_files_from_dir(
                    &project_dir,
                    crate::utils::markdown_config_loader::MarkdownConfigSource::ProjectSettings,
                ),
            );
        }
    }

    let mut seen = std::collections::HashSet::new();
    files
        .into_iter()
        .filter(|file| {
            let key = file
                .file_path
                .canonicalize()
                .unwrap_or_else(|_| file.file_path.clone());
            seen.insert(normalize_path_for_comparison(&key))
        })
        .collect()
}

/// Maps to CC `getAgentDefinitionsWithOverrides(cwd)`.
pub fn get_agent_definitions_with_overrides(cwd: &Path) -> AgentDefinitionsResult {
    let built_in_agents = get_built_in_agents();

    // Maps to the official simple-mode branch: skip custom agents and return
    // only built-ins.
    if crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("NYAN_SIMPLE").as_deref(),
    ) {
        let nyan = crate::utils::settings::get_initial_settings().nyan;
        let mut failed_files = Vec::new();
        let mut all_agents = built_in_agents;
        all_agents.extend(retain_valid_models(
            cli_flag_agents_from_bootstrap(),
            nyan.as_deref(),
            &mut failed_files,
        ));
        let active_agents = get_active_agents_from_list(&all_agents);
        return AgentDefinitionsResult {
            active_agents,
            all_agents,
            failed_files,
        };
    }

    let markdown_files = load_agent_markdown_files(cwd);
    let settings = crate::utils::settings::get_initial_settings();
    let nyan = settings.nyan.as_deref();
    let mut custom_agents = Vec::new();
    let mut failed_files = Vec::new();
    for markdown_file in markdown_files {
        match parse_agent_from_markdown(&markdown_file) {
            Some(agent) => match validate_agent_model(&agent, nyan) {
                Ok(()) => custom_agents.push(agent),
                Err(error) => failed_files.push(FailedAgentFile {
                    path: markdown_file.file_path,
                    error,
                    source: None,
                }),
            },
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
                    source: None,
                });
            }
            None => {}
        }
    }

    // Maps to CC `getAgentDefinitionsWithOverrides`: built-ins first,
    // enabled plugin agents next, filesystem custom agents after that. CLI/SDK
    // flag agents are merged last by `main.tsx`.
    let plugin_agents = retain_valid_models(
        crate::utils::plugins::load_plugin_agents::load_plugin_agents(),
        nyan,
        &mut failed_files,
    );
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
    let flag_agents =
        retain_valid_models(cli_flag_agents_from_bootstrap(), nyan, &mut failed_files);
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
    }
}

/// @nyan: an agent's `model:` must be a profile ref (profile name, `@main`,
/// `@light`, `@heavy` or `inherit`, optionally `:effort`). An agent naming
/// anything else is not loaded; the error names the bad ref.
fn validate_agent_model(
    agent: &AgentDefinition,
    nyan: Option<&crate::utils::settings::nyan::NyanSettings>,
) -> Result<(), String> {
    match agent.model.as_deref() {
        Some(model) => crate::utils::model::profile::resolve_model_ref(nyan, model)
            .map(drop)
            .map_err(|error| format!("Invalid \"model\" field: {error}")),
        None => Ok(()),
    }
}

/// Drops plugin and `--agents` agents whose `model:` is invalid, recording
/// each in `failed_files` under the agent name and its source (the loaders do
/// not carry a file path for them).
fn retain_valid_models(
    agents: Vec<AgentDefinition>,
    nyan: Option<&crate::utils::settings::nyan::NyanSettings>,
    failed_files: &mut Vec<FailedAgentFile>,
) -> Vec<AgentDefinition> {
    agents
        .into_iter()
        .filter(|agent| match validate_agent_model(agent, nyan) {
            Ok(()) => true,
            Err(error) => {
                failed_files.push(FailedAgentFile {
                    path: PathBuf::from(&agent.agent_type),
                    error,
                    source: Some(agent.source),
                });
                false
            }
        })
        .collect()
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
            // anything else rejects the agent.
            let value = value.as_str()?;
            if value != "worktree" {
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
    // CC construction spreads (:500-503): `skills.length > 0` (:500) and
    // truthy `initialPrompt` (:503) — schema-valid empties pass zod but never
    // reach the definition.
    agent.skills = skills.filter(|skills| !skills.is_empty());
    agent.model = model;
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
    agent.file_path = Some(markdown_file.file_path.clone());
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
    agent.hooks = frontmatter
        .get("hooks")
        .and_then(parse_hooks_config_json_value);
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
        .filter(|value| *value == "worktree")
        .map(ToOwned::to_owned);
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
            "nyan-agent-definitions-{name}-{}",
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
    /// `config_home`, the project walk stops at `home`, and a host's
    /// `NYAN_SIMPLE` (CC `--bare` exports it) is cleared. The caller
    /// holds `TEST_ENV_LOCK`.
    fn pin_loader_env(home: &Path, config_home: &Path) -> [EnvVarGuard; 3] {
        [
            EnvVarGuard::set("NYAN_CONFIG_DIR", config_home),
            EnvVarGuard::set(HOME_VAR, home),
            EnvVarGuard::unset("NYAN_SIMPLE"),
        ]
    }

    /// Stops the project walk at `homedir()`: an agents directory above the home directory is never read.
    /// In nyan, project agents live in `.nyan/agents` and require workspace trust.
    #[test]
    fn project_walk_stops_at_homedir() {
        let root = temp_dir("home-stop");
        let home = root.join("home");
        let cwd = home.join("project");
        let agent = |name: &str| format!("---\nname: {name}\ndescription: d\n---\nBody");
        write_file(&cwd.join(".nyan/agents/inside.md"), &agent("inside"));
        write_file(&root.join(".nyan/agents/above.md"), &agent("above"));
        // Also verify .claude/agents is NOT read
        write_file(
            &cwd.join(".claude/agents/claude_inside.md"),
            &agent("claude_inside"),
        );

        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _trust = crate::services::hooks::test_support::SessionTrustGuard::accepted();
        let _env = pin_loader_env(&home, &root.join("config"));
        let result = get_agent_definitions_with_overrides(&cwd);

        let names = result
            .active_agents
            .iter()
            .map(|agent| agent.agent_type.as_str())
            .collect::<Vec<_>>();
        assert!(names.contains(&"inside"), "{names:?}");
        assert!(!names.contains(&"above"), "{names:?}");
        assert!(!names.contains(&"claude_inside"), "{names:?}");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn project_agents_require_workspace_trust() {
        let root = temp_dir("trust-gate");
        let home = root.join("home");
        let cwd = home.join("project");
        let agent = |name: &str| format!("---\nname: {name}\ndescription: d\n---\nBody");
        write_file(
            &cwd.join(".nyan/agents/project_agent.md"),
            &agent("project_agent"),
        );
        let config_home = root.join("config");
        write_file(
            &config_home.join("agents/user_agent.md"),
            &agent("user_agent"),
        );

        let _lock = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::config::reset_trust_dialog_accepted_cache_for_testing();
        crate::bootstrap::state::set_session_trust_accepted(false);
        let _env = pin_loader_env(&home, &config_home);

        // When untrusted, project agents are NOT loaded, but user agents ARE loaded.
        let result = get_agent_definitions_with_overrides(&cwd);
        let names = result
            .active_agents
            .iter()
            .map(|agent| agent.agent_type.as_str())
            .collect::<Vec<_>>();
        assert!(!names.contains(&"project_agent"), "{names:?}");
        assert!(names.contains(&"user_agent"), "{names:?}");

        // When trusted, project agents ARE loaded.
        let _trust = crate::services::hooks::test_support::SessionTrustGuard::accepted();
        let result = get_agent_definitions_with_overrides(&cwd);
        let names = result
            .active_agents
            .iter()
            .map(|agent| agent.agent_type.as_str())
            .collect::<Vec<_>>();
        assert!(names.contains(&"project_agent"), "{names:?}");
        assert!(names.contains(&"user_agent"), "{names:?}");

        let _ = fs::remove_dir_all(root);
    }

    /// A sparse worktree (no `.nyan/agents` of its own) falls back to the main
    /// checkout's directory, but only for a trusted workspace.
    #[test]
    fn sparse_worktree_falls_back_to_the_main_checkouts_project_agents() {
        let root = temp_dir("worktree-fallback");
        let main = root.join("main");
        let worktree = root.join("worktree");
        let worktree_git_dir = main.join(".git/worktrees/worktree");
        fs::create_dir_all(&worktree_git_dir).expect("worktree git dir");
        fs::create_dir_all(&worktree).expect("worktree");
        write_file(
            &main.join(".nyan/agents/shared.md"),
            "---\nname: shared\ndescription: d\n---\nBody",
        );
        write_file(
            &main.join(".claude/agents/legacy.md"),
            "---\nname: legacy\ndescription: d\n---\nBody",
        );
        write_file(
            &worktree.join(".git"),
            &format!("gitdir: {}\n", worktree_git_dir.display()),
        );
        write_file(&worktree_git_dir.join("commondir"), "../..\n");
        write_file(
            &worktree_git_dir.join("gitdir"),
            &format!("{}\n", worktree.join(".git").display()),
        );

        let _lock = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::config::reset_trust_dialog_accepted_cache_for_testing();
        crate::bootstrap::state::set_session_trust_accepted(false);
        let _env = pin_loader_env(&root, &root.join("config"));
        let names = |result: AgentDefinitionsResult| {
            result
                .active_agents
                .into_iter()
                .map(|agent| agent.agent_type)
                .collect::<Vec<_>>()
        };

        let untrusted = names(get_agent_definitions_with_overrides(&worktree));
        assert!(!untrusted.contains(&"shared".to_string()), "{untrusted:?}");

        let _trust = crate::services::hooks::test_support::SessionTrustGuard::accepted();
        let trusted = names(get_agent_definitions_with_overrides(&worktree));
        assert!(trusted.contains(&"shared".to_string()), "{trusted:?}");
        assert!(!trusted.contains(&"legacy".to_string()), "{trusted:?}");
        let _ = fs::remove_dir_all(root);
    }

    /// An agent's depth lives in its `model:` as `ref[:effort]`; a separate
    /// `effort:` key is not a field, so a file carrying one still loads and the
    /// key changes nothing, while the depth in `model:` is kept verbatim for the
    /// selection chain to split.
    #[test]
    fn effort_frontmatter_is_not_a_field_and_model_keeps_its_effort_suffix() {
        let root = temp_dir("markdown-effort");
        let config_home = root.join("config");
        write_file(
            &config_home.join("agents/separate.md"),
            "---\nname: separate\ndescription: Separate effort key\nmodel: inherit\neffort: HIGH\n---\nBody",
        );
        write_file(
            &config_home.join("agents/suffixed.md"),
            "---\nname: suffixed\ndescription: Effort on the model\nmodel: \"@heavy:max\"\n---\nBody",
        );

        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _env = pin_loader_env(&root, &config_home);
        let result = get_agent_definitions_with_overrides(&root);

        assert!(result.failed_files.is_empty(), "{:?}", result.failed_files);
        let model_of = |name: &str| {
            result
                .active_agents
                .iter()
                .find(|agent| agent.agent_type == name)
                .unwrap_or_else(|| panic!("{name} agent survives"))
                .model
                .clone()
        };
        assert_eq!(model_of("separate").as_deref(), Some("inherit"));
        assert_eq!(model_of("suffixed").as_deref(), Some("@heavy:max"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn parses_custom_agent_markdown_for_status_notice_snapshot() {
        let root = temp_dir("parse");
        let config_home = root.join("config");
        write_file(
            &config_home.join("agents/reviewer.md"),
            "---\nname: reviewer\ndescription: Review code changes\\nwith tests\ncolor: blue\nmodel: INHERIT\ntools: Read, Grep Bash\ndisallowedTools: Bash\nskills: audit, docs\nmemory: project\nmcpServers:\n  - docs\n  - local-agent:\n      type: stdio\n      command: node\n      args:\n        - server.js\nhooks:\n  Stop:\n    - matcher: \"\"\n      hooks:\n        - command: echo stop\n          timeout: 5\npermissionMode: plan\nmaxTurns: 4\nbackground: true\ninitialPrompt: Start here\nisolation: worktree\n---\nPrompt body is not executed.",
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
        let _auto_memory = EnvVarGuard::set("NYAN_DISABLE_AUTO_MEMORY", "false");
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
        let hooks = reviewer.hooks.as_ref().expect("frontmatter hooks");
        assert_eq!(hooks["Stop"][0].hooks[0].command, "echo stop");
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

    /// `permissionMode`, `mcpServers` and `requiredMcpServers` are not agent
    /// fields in nyan: a file that still carries them loads normally and the
    /// keys change nothing, on both the markdown and the `--agents` JSON path.
    #[test]
    fn removed_frontmatter_fields_are_ignored_not_rejected() {
        let root = temp_dir("removed-fields");
        let config_home = root.join("config");
        write_file(
            &config_home.join("agents/legacy.md"),
            "---\nname: legacy\ndescription: Old definition\npermissionMode: bypassPermissions\nrequiredMcpServers:\n  - docs\nmcpServers:\n  - docs\ntools: Read, Agent(worker)\n---\nBody",
        );

        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _env = pin_loader_env(&root, &config_home);
        let result = get_agent_definitions_with_overrides(&root);

        assert!(result.failed_files.is_empty(), "{:?}", result.failed_files);
        let legacy = result
            .active_agents
            .iter()
            .find(|agent| agent.agent_type == "legacy")
            .expect("legacy agent still loads");
        // `Agent(worker)` stays a plain tool entry; it no longer narrows which
        // agent types may be spawned.
        assert_eq!(
            legacy.tools.as_deref(),
            Some(&["Read".to_string(), "Agent(worker)".to_string()][..])
        );

        let json = serde_json::json!({
            "description": "From json",
            "prompt": "Body",
            "permissionMode": "bypassPermissions",
            "mcpServers": ["docs"],
        });
        let from_json =
            parse_agent_from_json("json-agent", &json, AgentDefinitionSource::FlagSettings)
                .expect("json agent still parses");
        assert_eq!(from_json.when_to_use, "From json");
        let _ = fs::remove_dir_all(root);
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
        let _auto_memory = EnvVarGuard::set("NYAN_DISABLE_AUTO_MEMORY", "false");

        let value = serde_json::json!({
            "description": "Review code",
            "prompt": "You review code.",
            "tools": ["Read, Bash(git commit, git status)"],
            "disallowedTools": ["WebFetch"],
            "skills": ["audit"],
            "model": "INHERIT",
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

        // CC AgentJsonSchema `.parse()` is ALL-OR-NOTHING: a present-but-
        // invalid field rejects the entire agent, not just the field.
        for (key, bad) in [
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
        // The isolation enum (AgentTool.tsx:215-218) accepts only 'worktree'.
        let mut remote = value.clone();
        remote["isolation"] = serde_json::json!("remote");
        let parsed =
            parse_agent_from_json("reviewer", &remote, AgentDefinitionSource::FlagSettings);
        assert!(parsed.is_none(), "isolation: remote is rejected");
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
        empties["hooks"] = serde_json::json!({});
        let empties_agent =
            parse_agent_from_json("reviewer", &empties, AgentDefinitionSource::FlagSettings)
                .expect("schema-valid empties keep the agent");
        assert_eq!(empties_agent.skills, None);
        assert!(empties_agent.hooks.is_some());
    }

    #[test]
    fn unknown_model_refs_are_not_loaded_and_recorded_as_failed() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                crate::bootstrap::state::set_inline_plugins(Vec::new());
                crate::bootstrap::state::set_cli_agents_json(None);
                crate::utils::settings::settings_cache::reset_settings_cache();
            }
        }
        let _reset = Reset;
        crate::utils::settings::settings_cache::install_test_settings(
            serde_json::from_value(serde_json::json!({
                "nyan": {"models": {"ds-flash": {"model": "deepseek-chat"}},
                    "selection": {"main": "ds-flash"}}
            }))
            .unwrap(),
        );
        let root = temp_dir("model-validation");
        let config_home = root.join("config");
        for (name, model) in [
            ("profile", "ds-flash"),
            ("tier", "\"@light\""),
            ("inherit", "inherit"),
            ("effort", "\"ds-flash:high\""),
            ("bad", "sonnet"),
        ] {
            write_file(
                &config_home.join(format!("agents/{name}.md")),
                &format!("---\nname: {name}\ndescription: d\nmodel: {model}\n---\nBody"),
            );
        }
        let plugin_root = root.join("plugin");
        write_file(
            &plugin_root.join(".claude-plugin/plugin.json"),
            r#"{"name":"pack"}"#,
        );
        write_file(
            &plugin_root.join("agents/ok.md"),
            "---\nname: ok\ndescription: d\nmodel: \"@heavy\"\n---\nBody",
        );
        write_file(
            &plugin_root.join("agents/broken.md"),
            "---\nname: broken\ndescription: d\nmodel: opus\n---\nBody",
        );
        crate::bootstrap::state::set_inline_plugins(vec![plugin_root]);
        crate::bootstrap::state::set_cli_agents_json(Some(serde_json::json!({
            "flag-ok": {"description": "d", "prompt": "p", "model": "@main:off"},
            "flag-bad": {"description": "d", "prompt": "p", "model": "openai/gpt-5"}
        })));

        let _env = pin_loader_env(&root, &config_home);
        let result = get_agent_definitions_with_overrides(&root);

        let loaded = |name: &str| {
            result
                .all_agents
                .iter()
                .any(|agent| agent.agent_type == name)
        };
        for name in ["profile", "tier", "inherit", "effort", "pack:ok", "flag-ok"] {
            assert!(loaded(name), "{name} loads");
        }
        for name in ["bad", "pack:broken", "flag-bad"] {
            assert!(!loaded(name), "{name} is not loaded");
        }
        let failed = result
            .failed_files
            .iter()
            .map(|file| (file.label(), file.error.clone()))
            .collect::<Vec<_>>();
        assert_eq!(failed.len(), 3, "{failed:?}");
        assert_eq!(
            failed[0].0,
            config_home.join("agents/bad.md").display().to_string()
        );
        assert!(failed[0].1.contains("\"sonnet\""), "{}", failed[0].1);
        assert_eq!(failed[1].0, "plugin agent pack:broken");
        assert!(failed[1].1.contains("\"opus\""));
        assert_eq!(failed[2].0, "--agents agent flag-bad");
        assert!(
            failed[2]
                .1
                .starts_with("Invalid \"model\" field: Unknown model \"openai/gpt-5\"")
        );
        let _ = fs::remove_dir_all(root);
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

    #[test]
    fn isolation_frontmatter_honors_official_external_remote_gate() {
        use crate::utils::frontmatter_parser::FrontmatterData;
        use crate::utils::markdown_config_loader::{MarkdownConfigSource, MarkdownFile};

        let parse = |isolation: &str| {
            let mut frontmatter = FrontmatterData::new();
            frontmatter.insert("name".to_string(), serde_json::json!("reviewer"));
            frontmatter.insert(
                "description".to_string(),
                serde_json::json!("Review changes"),
            );
            frontmatter.insert("isolation".to_string(), serde_json::json!(isolation));
            parse_agent_from_markdown(&MarkdownFile {
                file_path: PathBuf::from("reviewer.md"),
                base_dir: PathBuf::from("."),
                frontmatter,
                content: "Review carefully".to_string(),
                source: MarkdownConfigSource::ProjectSettings,
            })
            .and_then(|agent| agent.isolation)
        };

        assert_eq!(parse("worktree").as_deref(), Some("worktree"));
        assert_eq!(parse("remote"), None);
        assert_eq!(parse("invalid"), None);
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
            AgentDefinition::new("reviewer", "flag", AgentDefinitionSource::FlagSettings),
            AgentDefinition::new("helper", "user", AgentDefinitionSource::UserSettings),
        ];

        let active = get_active_agents_from_list(&all_agents);

        let reviewer = active
            .iter()
            .find(|agent| agent.agent_type == "reviewer")
            .expect("reviewer active agent");
        assert_eq!(reviewer.when_to_use, "flag");
        assert_eq!(
            active
                .iter()
                .map(|agent| agent.agent_type.as_str())
                .collect::<Vec<_>>(),
            vec!["reviewer", "helper"]
        );
    }

    #[test]
    fn filesystem_loader_uses_user_project_order_and_simple_mode() {
        let root = temp_dir("dirs");
        let config_home = root.join("config");
        let cwd = root.join("repo/nested");
        fs::create_dir_all(&cwd).expect("cwd");
        fs::create_dir_all(root.join("repo/.git")).expect("git root");
        // `.claude/agents` is not a project agents directory.
        write_file(
            &cwd.join(".claude/agents/legacy.md"),
            "---\nname: legacy\ndescription: Legacy\n---\nLegacy",
        );
        write_file(
            &config_home.join("agents/reviewer.md"),
            "---\nname: reviewer\ndescription: User description\n---\nUser",
        );
        write_file(
            &cwd.join(".nyan/agents/reviewer.md"),
            "---\nname: reviewer\ndescription: Nested project description\n---\nNested",
        );
        write_file(
            &root.join("repo/.nyan/agents/reviewer.md"),
            "---\nname: reviewer\ndescription: Root project description\n---\nRoot",
        );

        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _trust = crate::services::hooks::test_support::SessionTrustGuard::accepted();
        let _env = pin_loader_env(&root, &config_home);
        let result = get_agent_definitions_with_overrides(&cwd);
        let reviewer = result
            .active_agents
            .iter()
            .find(|agent| agent.agent_type == "reviewer")
            .expect("reviewer active agent");
        assert_eq!(reviewer.when_to_use, "Root project description");
        assert_eq!(reviewer.source, AgentDefinitionSource::ProjectSettings);
        assert!(
            result
                .all_agents
                .iter()
                .all(|agent| agent.agent_type != "legacy")
        );
        // Every tier is loaded, in user < nested < root order.
        let descriptions = result
            .all_agents
            .iter()
            .filter(|agent| agent.agent_type == "reviewer")
            .map(|agent| (agent.source, agent.when_to_use.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(
            descriptions,
            vec![
                (AgentDefinitionSource::UserSettings, "User description"),
                (
                    AgentDefinitionSource::ProjectSettings,
                    "Nested project description"
                ),
                (
                    AgentDefinitionSource::ProjectSettings,
                    "Root project description"
                ),
            ]
        );

        let _simple = EnvVarGuard::set("NYAN_SIMPLE", "true");
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
        _loader: [EnvVarGuard; 3],
        _remote_memory: EnvVarGuard,
        _auto_memory: EnvVarGuard,
        _lock: TestEnvGuard<'static>,
    }

    fn pin_snapshot_env(home: &Path, config_home: &Path) -> SnapshotEnv {
        let lock = TEST_ENV_LOCK.lock().unwrap();
        SnapshotEnv {
            _loader: pin_loader_env(home, config_home),
            _remote_memory: EnvVarGuard::unset("NYAN_REMOTE_MEMORY_DIR"),
            _auto_memory: EnvVarGuard::set("NYAN_DISABLE_AUTO_MEMORY", "false"),
            _lock: lock,
        }
    }

    fn write_snapshot(cwd: &Path, agent_type: &str, updated_at: &str, payload: &str) {
        let snapshot_dir = cwd
            .join(".nyan")
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
        // saveSyncedMeta, into the USER memory dir (`$NYAN_CONFIG_DIR/agent-memory`).
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
        assert!(!root.join(".nyan/agent-memory/projectmem").exists());
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
            let agent = parse_agent_from_markdown(&markdown).unwrap();
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
