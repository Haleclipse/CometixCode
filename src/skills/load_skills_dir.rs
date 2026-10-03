//! Filesystem skill discovery and prompt materialization.
//!
//! Maps to: CC `skills/loadSkillsDir.ts`.
//!
//! This module owns the local `/skills/` and legacy `/commands/` skill loader
//! boundary. It intentionally keeps command execution out except for the
//! official prompt-shell expansion seam (`!` blocks). Hooks, MCP skills,
//! remote/canonical skills, and forked-agent execution are handled by later
//! runtime slices.

use crate::utils::argument_substitution::{parse_argument_names, substitute_arguments};
use crate::utils::frontmatter_parser::{
    FrontmatterShell, coerce_description_to_string, parse_frontmatter, parse_shell_frontmatter,
};
use crate::utils::markdown_config_loader::extract_description_from_markdown;
use indexmap::IndexMap;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SkillSource {
    PolicySettings,
    UserSettings,
    ProjectSettings,
}

impl SkillSource {
    pub fn official_name(self) -> &'static str {
        match self {
            Self::PolicySettings => "policySettings",
            Self::UserSettings => "userSettings",
            Self::ProjectSettings => "projectSettings",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SkillLoadedFrom {
    Skills,
    CommandsDeprecated,
    Plugin,
    Bundled,
    Mcp,
}

impl SkillLoadedFrom {
    pub fn official_name(self) -> &'static str {
        match self {
            Self::Skills => "skills",
            Self::CommandsDeprecated => "commands_DEPRECATED",
            Self::Plugin => "plugin",
            Self::Bundled => "bundled",
            Self::Mcp => "mcp",
        }
    }
}

/// Maps to: CC `SettingSource | 'plugin'`, the parameter type of
/// `getSkillsPath` (`:78-81`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SkillsPathSource {
    Setting(crate::utils::settings::constants::SettingSource),
    Plugin,
}

/// Maps to: CC `getSkillsPath(source, dir)` (`:78-94`).
///
/// Lives here, not in the menu that renders it: CC exports it from
/// `loadSkillsDir.ts` and `SkillsMenu.tsx` imports it. `localSettings` and
/// `flagSettings` fall through CC's `default` arm to the empty string — they
/// have no skills directory of their own.
pub fn get_skills_path(source: SkillsPathSource, dir: &str) -> PathBuf {
    use crate::utils::settings::constants::SettingSource;

    match source {
        SkillsPathSource::Setting(SettingSource::Policy) => {
            crate::utils::settings::managed_path::get_managed_file_path()
                .join(".claude")
                .join(dir)
        }
        SkillsPathSource::Setting(SettingSource::User) => {
            crate::utils::env_utils::get_claude_config_home_dir().join(dir)
        }
        SkillsPathSource::Setting(SettingSource::Project) => {
            PathBuf::from(format!(".claude/{dir}"))
        }
        SkillsPathSource::Plugin => PathBuf::from("plugin"),
        SkillsPathSource::Setting(SettingSource::Local | SettingSource::Flag) => PathBuf::new(),
    }
}

/// Maps to: CC `estimateSkillFrontmatterTokens(skill)` (`:100-105`).
///
/// CC reads the three fields off a `Command`; the caller here is the skills
/// menu, whose row type is its own projection, so the fields arrive directly.
/// `filter(Boolean)` is the empty-string drop.
pub fn estimate_skill_frontmatter_tokens(
    name: &str,
    description: &str,
    when_to_use: Option<&str>,
) -> i64 {
    let frontmatter_text = [Some(name), Some(description), when_to_use]
        .into_iter()
        .flatten()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    crate::services::token_estimation::rough_token_count_estimation(&frontmatter_text)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SkillExecutionContext {
    Inline,
    Fork,
}

impl SkillExecutionContext {
    pub fn official_name(self) -> &'static str {
        match self {
            Self::Inline => "inline",
            Self::Fork => "fork",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillCommand {
    /// Maps to CC `Command.type === 'prompt'`.
    pub name: String,
    pub display_name: Option<String>,
    pub description: String,
    pub has_user_specified_description: bool,
    pub markdown_content: String,
    pub content_length: usize,
    pub allowed_tools: Vec<String>,
    pub argument_hint: Option<String>,
    pub argument_names: Vec<String>,
    pub when_to_use: Option<String>,
    pub version: Option<String>,
    pub model: Option<String>,
    /// Maps to CC prompt-command frontmatter `effort`.
    pub effort: Option<crate::utils::effort::EffortValue>,
    pub disable_model_invocation: bool,
    pub user_invocable: bool,
    pub execution_context: SkillExecutionContext,
    pub agent: Option<String>,
    pub shell: Option<FrontmatterShell>,
    /// Maps to CC `createSkillCommand(...).hooks` (`loadSkillsDir.ts:342`),
    /// parsed by `parseHooksFromFrontmatter` (`:136-153`).
    pub hooks: Option<crate::services::hooks::HooksConfig>,
    pub paths: Option<Vec<String>>,
    pub source: SkillSource,
    pub loaded_from: SkillLoadedFrom,
    pub skill_root: Option<PathBuf>,
    pub file_path: PathBuf,
}

impl SkillCommand {
    /// Maps to CC `createSkillCommand(...).userFacingName()`.
    pub fn user_facing_name(&self) -> &str {
        self.display_name.as_deref().unwrap_or(&self.name)
    }

    /// Maps to CC `createSkillCommand(...).getPromptForCommand(args, ctx)`
    /// (`loadSkillsDir.ts:344-399`).
    pub fn get_prompt_for_command(
        &self,
        args: Option<&str>,
        session_id: &str,
        tool_use_context: &crate::tool::ToolUseContext,
    ) -> anyhow::Result<String> {
        let mut final_content = self.get_prompt_for_command_without_shell(args, session_id)?;

        // Security: official MCP skills are remote and untrusted, so
        // `executeShellCommandsInPrompt(...)` is skipped for
        // `loadedFrom === 'mcp'` (`loadSkillsDir.ts:371-374`).
        if self.loaded_from != SkillLoadedFrom::Mcp {
            let shell_context = tool_use_context
                .clone()
                .with_get_app_state_override(self.command_scoped_get_app_state(tool_use_context));
            final_content = crate::utils::prompt_shell_execution::execute_shell_commands_in_prompt(
                &final_content,
                &shell_context,
                &format!("/{}", self.name),
                self.shell,
            )?;
        }

        Ok(final_content)
    }

    /// Maps to CC `loadSkillsDir.ts:377-392` — the `getAppState` override the
    /// skill wraps its own context in before `executeShellCommandsInPrompt`.
    ///
    /// CC writes `command: allowedTools`, an unconditional REPLACE of the
    /// `command` rule source with exactly this skill's `allowed-tools`. That is
    /// the scoping: a `!` block runs with THIS skill's grants and nothing else,
    /// including when `allowed-tools` is absent (the source is emptied).
    ///
    /// It is deliberately not the union in `createGetAppStateWithAllowedTools`
    /// (`forkedAgent.ts:147-171`), which is the FORK path and does merge with
    /// whatever `SkillTool`'s `contextModifier` has already accumulated on the
    /// turn. Reusing the fork helper here let a previously invoked skill's
    /// grants apply to a later skill's `!` blocks.
    fn command_scoped_get_app_state(
        &self,
        parent: &crate::tool::ToolUseContext,
    ) -> crate::tool::GetAppStateCallback {
        use crate::types::permissions::PermissionRuleSource;
        use crate::utils::permissions::permission_rule_parser::permission_rule_value_from_string;
        use crate::utils::permissions::permission_setup::parse_tool_list_from_cli;

        let parent = parent.clone();
        let rules = parse_tool_list_from_cli(&self.allowed_tools)
            .iter()
            .map(|spec| permission_rule_value_from_string(spec))
            .collect::<Vec<_>>();
        crate::tool::GetAppStateCallback::new(move || {
            let state = parent.get_app_state().unwrap_or_else(|| {
                // Rust headless/test contexts may carry only the local snapshot;
                // project it through the canonical getAppState boundary too.
                let mut state = crate::state::app_state_store::AppState::default();
                state.tool_permission_context =
                    std::sync::Arc::new(parent.tool_permission_context.clone());
                std::sync::Arc::new(state)
            });
            let mut projected = (*state).clone();
            let mut permission = (*projected.tool_permission_context).clone();
            permission
                .always_allow_rules
                .insert(PermissionRuleSource::Command, rules.clone());
            projected.tool_permission_context = std::sync::Arc::new(permission);
            Some(std::sync::Arc::new(projected))
        })
    }

    /// Maps to CC `processSlashCommand.tsx:1172-1186` — the frontmatter-hook
    /// registration that runs right after `getPromptForCommand`, gated on the
    /// same `["hooks"]`-lock rule as agent frontmatter hooks
    /// (`run_agent.rs:1311-1318`, CC `runAgent.ts`): under a plugin-only
    /// `hooks` policy, only admin-trusted sources may register.
    ///
    /// CC has one call site because everything funnels through
    /// `getMessagesForPromptSlashCommand`. This port materializes skill prompts
    /// in two places — the slash-command transport and `SkillTool::call` — so
    /// the gate lives here, once, instead of being copied into both.
    ///
    /// Returns the number of hooks registered.
    ///
    /// Named for CC `registerSkillHooks`, not `registerFrontmatterHooks`: the
    /// skill path uses the former, which registers `Stop` as `Stop`. The agent
    /// path (`register_agent_frontmatter_hooks_for_run`) uses the latter, which
    /// rewrites `Stop` to `SubagentStop`.
    pub fn register_skill_hooks(&self, session_id: &str) -> usize {
        let Some(hooks) = self.hooks.as_ref() else {
            return 0;
        };
        let hooks_allowed_for_this_skill =
            !crate::utils::settings::plugin_only_policy::is_restricted_to_plugin_only("hooks")
                || crate::utils::settings::plugin_only_policy::is_source_admin_trusted(
                    self.source.official_name(),
                );
        if !hooks_allowed_for_this_skill {
            return 0;
        }
        crate::utils::hooks::register_skill_hooks::register_skill_hooks(
            session_id,
            hooks,
            &self.name,
            self.skill_root
                .as_ref()
                .map(|root| root.display().to_string()),
        )
        .registered_count
    }

    /// Test/helper seam for the argument/env substitution half before official
    /// `executeShellCommandsInPrompt(...)` runs.
    pub fn get_prompt_for_command_without_shell(
        &self,
        args: Option<&str>,
        session_id: &str,
    ) -> anyhow::Result<String> {
        let mut final_content = if let Some(base_dir) = &self.skill_root {
            format!(
                "Base directory for this skill: {}\n\n{}",
                display_path_for_prompt(base_dir),
                self.markdown_content
            )
        } else {
            self.markdown_content.clone()
        };

        final_content =
            substitute_arguments(&final_content, args, true, self.argument_names.as_slice())?;

        if let Some(base_dir) = &self.skill_root {
            final_content =
                final_content.replace("${CLAUDE_SKILL_DIR}", &display_path_for_prompt(base_dir));
        }
        Ok(final_content.replace("${CLAUDE_SESSION_ID}", session_id))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillWithPath {
    pub skill: SkillCommand,
    pub file_path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillDir {
    pub path: PathBuf,
    pub source: SkillSource,
}

// Maps to CC `dynamicSkillDirs`, `dynamicSkills`, `conditionalSkills`, and
// `activatedConditionalSkillNames`. Process-global state is session-global in
// the official runtime as well.
static DYNAMIC_SKILL_DIRS: LazyLock<Mutex<HashSet<PathBuf>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));
static DYNAMIC_SKILLS: LazyLock<Mutex<IndexMap<String, SkillCommand>>> =
    LazyLock::new(|| Mutex::new(IndexMap::new()));
static CONDITIONAL_SKILLS: LazyLock<Mutex<IndexMap<String, SkillCommand>>> =
    LazyLock::new(|| Mutex::new(IndexMap::new()));
static ACTIVATED_CONDITIONAL_SKILL_NAMES: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));
static DYNAMIC_SKILLS_GENERATION: AtomicU64 = AtomicU64::new(0);

/// Maps to: CC `memoize(...)` around `getSkillDirCommands` (`:638`), keyed by
/// `cwd` exactly as lodash keys on the first argument. Cleared by
/// `clear_skill_caches()` (`:806-811`) and by nothing else — in particular NOT
/// by `clear_command_memoization_caches()`, which CC documents at
/// `commands.ts:519-522` as clearing command memos *without* touching skill
/// caches.
static SKILL_DIR_COMMANDS_CACHE: LazyLock<Mutex<HashMap<PathBuf, Vec<SkillCommand>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// RAII snapshot for tests that exercise dynamic discovery/activation while
/// preserving all process-global skill registries and generation state.
#[cfg(test)]
pub(crate) struct DynamicSkillsTestSnapshot {
    directories: HashSet<PathBuf>,
    dynamic: IndexMap<String, SkillCommand>,
    conditional: IndexMap<String, SkillCommand>,
    activated: HashSet<String>,
    generation: u64,
}

#[cfg(test)]
impl DynamicSkillsTestSnapshot {
    pub(crate) fn capture() -> Self {
        Self {
            directories: DYNAMIC_SKILL_DIRS
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone(),
            dynamic: DYNAMIC_SKILLS
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone(),
            conditional: CONDITIONAL_SKILLS
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone(),
            activated: ACTIVATED_CONDITIONAL_SKILL_NAMES
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone(),
            generation: DYNAMIC_SKILLS_GENERATION.load(Ordering::Acquire),
        }
    }
}

#[cfg(test)]
impl Drop for DynamicSkillsTestSnapshot {
    fn drop(&mut self) {
        *DYNAMIC_SKILL_DIRS
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = self.directories.clone();
        *DYNAMIC_SKILLS
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = self.dynamic.clone();
        *CONDITIONAL_SKILLS
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = self.conditional.clone();
        *ACTIVATED_CONDITIONAL_SKILL_NAMES
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = self.activated.clone();
        DYNAMIC_SKILLS_GENERATION.store(self.generation, Ordering::Release);
    }
}

/// Maps to CC `getSkillDirCommands(cwd)` — the memoized wrapper (`:638`).
pub fn get_skill_dir_commands(cwd: &Path) -> Vec<SkillCommand> {
    let key = cwd.to_path_buf();
    if let Some(cached) = SKILL_DIR_COMMANDS_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&key)
    {
        return cached.clone();
    }
    let loaded = load_skill_dir_commands(cwd);
    SKILL_DIR_COMMANDS_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(key, loaded.clone());
    loaded
}

/// Maps to CC the function body lodash memoizes (`:639-803`).
fn load_skill_dir_commands(cwd: &Path) -> Vec<SkillCommand> {
    use crate::utils::settings::constants::{SettingSource, is_setting_source_enabled};

    let mut skill_dirs = Vec::new();
    let mut legacy_command_dirs = Vec::new();
    let skills_locked =
        crate::utils::settings::plugin_only_policy::is_restricted_to_plugin_only("skills");
    let user_enabled = is_setting_source_enabled(SettingSource::User) && !skills_locked;
    let project_enabled = is_setting_source_enabled(SettingSource::Project) && !skills_locked;
    let additional_dirs = crate::bootstrap::state::get_additional_directories_for_claude_md();

    // CC --bare skips every auto-discovered source and all legacy commands;
    // only explicit --add-dir skill roots survive the project/policy gate.
    if crate::utils::env_utils::is_bare_mode() {
        if project_enabled {
            for add_dir in additional_dirs {
                let candidate = add_dir.join(".claude").join("skills");
                if candidate.is_dir() {
                    skill_dirs.push(SkillDir {
                        path: candidate,
                        source: SkillSource::ProjectSettings,
                    });
                }
            }
        }
    } else {
        if !crate::utils::env_utils::is_env_truthy(
            crate::utils::process_env::var("CLAUDE_CODE_DISABLE_POLICY_SKILLS")
                .as_deref(),
        ) {
            let managed = crate::utils::settings::managed_path::get_managed_file_path()
                .join(".claude")
                .join("skills");
            if managed.is_dir() {
                skill_dirs.push(SkillDir {
                    path: managed,
                    source: SkillSource::PolicySettings,
                });
            }
        }

        if user_enabled {
            let user = crate::utils::env_utils::get_claude_config_home_dir().join("skills");
            if user.is_dir() {
                skill_dirs.push(SkillDir {
                    path: user,
                    source: SkillSource::UserSettings,
                });
            }
        }

        if project_enabled {
            for dir in
                crate::utils::markdown_config_loader::get_project_dirs_up_to_home("skills", cwd)
            {
                skill_dirs.push(SkillDir {
                    path: dir,
                    source: SkillSource::ProjectSettings,
                });
            }
            for add_dir in additional_dirs {
                let candidate = add_dir.join(".claude").join("skills");
                if candidate.is_dir() {
                    skill_dirs.push(SkillDir {
                        path: candidate,
                        source: SkillSource::ProjectSettings,
                    });
                }
            }
        }

        // Legacy commands are skills regardless of source, so strict
        // plugin-only policy disables the entire legacy loader, including
        // managed commands.
        if !skills_locked {
            let managed_commands = crate::utils::settings::managed_path::get_managed_file_path()
                .join(".claude")
                .join("commands");
            if managed_commands.is_dir() {
                legacy_command_dirs.push(SkillDir {
                    path: managed_commands,
                    source: SkillSource::PolicySettings,
                });
            }
            if user_enabled {
                let user_commands =
                    crate::utils::env_utils::get_claude_config_home_dir().join("commands");
                if user_commands.is_dir() {
                    legacy_command_dirs.push(SkillDir {
                        path: user_commands,
                        source: SkillSource::UserSettings,
                    });
                }
            }
            if project_enabled {
                for dir in crate::utils::markdown_config_loader::get_project_dirs_up_to_home(
                    "commands", cwd,
                ) {
                    legacy_command_dirs.push(SkillDir {
                        path: dir,
                        source: SkillSource::ProjectSettings,
                    });
                }
            }
        }
    }

    let mut loaded_with_paths = Vec::new();
    for dir in skill_dirs {
        loaded_with_paths.extend(load_skills_from_skills_dir(&dir.path, dir.source));
    }
    for dir in legacy_command_dirs {
        loaded_with_paths.extend(load_skills_from_commands_dir(&dir.path, dir.source));
    }
    let loaded = deduplicate_skills_by_file_identity(loaded_with_paths)
        .into_iter()
        .map(|entry| entry.skill)
        .collect::<Vec<_>>();
    let activated = ACTIVATED_CONDITIONAL_SKILL_NAMES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    let mut conditional = CONDITIONAL_SKILLS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut unconditional = Vec::new();
    for skill in loaded {
        if skill.paths.as_ref().is_some_and(|paths| !paths.is_empty())
            && !activated.contains(&skill.name)
        {
            conditional.insert(skill.name.clone(), skill);
        } else {
            unconditional.push(skill);
        }
    }
    drop(conditional);

    // CC returns `unconditionalSkills` here and nothing else (`:802`). Dynamic
    // skills are merged one layer up, in `getCommands` (`commands.ts:476-517`),
    // where the dedup set covers every enabled command — builtins and plugin
    // commands included — and the insertion point is immediately before the
    // first builtin. Merging them here instead deduped against skill-dir
    // commands only, so a dynamically discovered skill named after a builtin
    // (`review`, `init`, …) survived as a SECOND entry and, being ahead of the
    // builtins, won `find_command`.
    unconditional
}

/// Maps to CC `findCommand(name, commands)` for skill commands.
pub fn find_skill_command<'a>(
    name: &str,
    commands: &'a [SkillCommand],
) -> Option<&'a SkillCommand> {
    let normalized = name.trim().strip_prefix('/').unwrap_or(name.trim());
    commands.iter().find(|command| command.name == normalized)
}

/// Maps to CC `getDynamicSkills()`.
pub fn get_dynamic_skills() -> Vec<SkillCommand> {
    DYNAMIC_SKILLS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .values()
        .cloned()
        .collect()
}

/// Rust cache-key projection of CC's `skillsLoaded` signal.
pub fn dynamic_skills_generation() -> u64 {
    DYNAMIC_SKILLS_GENERATION.load(Ordering::Acquire)
}

/// Maps to CC `discoverSkillDirsForPaths(filePaths, cwd)`.
pub fn discover_skill_dirs_for_paths(file_paths: &[PathBuf], cwd: &Path) -> Vec<PathBuf> {
    let resolved_cwd = crate::utils::path::expand_path(&cwd.display().to_string(), None)
        .unwrap_or_else(|_| cwd.to_path_buf());
    let mut discovered = Vec::new();
    let mut checked = DYNAMIC_SKILL_DIRS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    for file_path in file_paths {
        let absolute =
            crate::utils::path::expand_path(&file_path.display().to_string(), Some(&resolved_cwd))
                .unwrap_or_else(|_| file_path.to_path_buf());
        let mut current = absolute.parent().map(Path::to_path_buf);
        while let Some(directory) = current {
            if directory == resolved_cwd || !directory.starts_with(&resolved_cwd) {
                break;
            }
            let skill_dir = directory.join(".claude").join("skills");
            if checked.insert(skill_dir.clone())
                && std::fs::metadata(&skill_dir).is_ok()
                && !crate::utils::git::gitignore::is_path_gitignored(&directory, &resolved_cwd)
            {
                discovered.push(skill_dir);
            }
            current = directory.parent().map(Path::to_path_buf);
        }
    }
    drop(checked);
    discovered.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    discovered
}

/// Maps to CC `addSkillDirectories(dirs)`.
pub fn add_skill_directories(dirs: &[PathBuf]) {
    if !crate::utils::settings::constants::is_setting_source_enabled(
        crate::utils::settings::constants::SettingSource::Project,
    ) || crate::utils::settings::plugin_only_policy::is_restricted_to_plugin_only("skills")
        || dirs.is_empty()
    {
        return;
    }
    let loaded = dirs
        .iter()
        .map(|directory| load_skills_from_skills_dir(directory, SkillSource::ProjectSettings))
        .collect::<Vec<_>>();
    {
        let mut dynamic = DYNAMIC_SKILLS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for skills in loaded.iter().rev() {
            for entry in skills {
                dynamic.insert(entry.skill.name.clone(), entry.skill.clone());
            }
        }
    }
    // Maps to: CC `:974` `skillsLoaded.emit()` — outside the
    // `if (newSkillCount > 0)` block at `:954-971`, which only guards logging
    // and analytics. Bumping the generation only on a non-empty load left the
    // command memo holding a list assembled before these directories existed,
    // so a directory that had just appeared with no loadable SKILL.md never
    // invalidated anything.
    DYNAMIC_SKILLS_GENERATION.fetch_add(1, Ordering::AcqRel);
}

/// Maps to: CC `skills/loadSkillsDir.ts#activateConditionalSkillsForPaths`.
pub fn activate_conditional_skills_for_paths(file_paths: &[PathBuf], cwd: &Path) -> Vec<String> {
    let mut conditional = CONDITIONAL_SKILLS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if conditional.is_empty() {
        return Vec::new();
    }
    let mut activated = Vec::new();
    for (name, skill) in conditional.iter() {
        let Some(patterns) = skill.paths.as_deref().filter(|paths| !paths.is_empty()) else {
            continue;
        };
        // Rust representation of `ignore().add(skill.paths)` at CC `:1012` —
        // one matcher per SKILL, built outside the file loop as CC builds it.
        // Building it per file re-parsed every pattern once per touched path.
        let mut builder = ignore::gitignore::GitignoreBuilder::new(".");
        let mut patterns_ok = true;
        for pattern in patterns {
            if builder.add_line(None, pattern).is_err() {
                patterns_ok = false;
                break;
            }
        }
        if !patterns_ok {
            continue;
        }
        let Ok(matcher) = builder.build() else {
            continue;
        };
        if file_paths.iter().any(|file_path| {
            let absolute =
                crate::utils::path::expand_path(&file_path.display().to_string(), Some(cwd))
                    .unwrap_or_else(|_| file_path.to_path_buf());
            let Ok(relative) = absolute.strip_prefix(cwd) else {
                return false;
            };
            if relative.as_os_str().is_empty() {
                return false;
            }
            matcher
                .matched_path_or_any_parents(relative, false)
                .is_ignore()
        }) {
            activated.push(name.clone());
        }
    }
    if activated.is_empty() {
        return activated;
    }
    let activated_skills = activated
        .iter()
        .filter_map(|name| {
            conditional
                .shift_remove(name)
                .map(|skill| (name.clone(), skill))
        })
        .collect::<Vec<_>>();
    drop(conditional);
    {
        let mut dynamic = DYNAMIC_SKILLS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for (name, skill) in &activated_skills {
            dynamic.insert(name.clone(), skill.clone());
        }
    }
    {
        let mut activated_names = ACTIVATED_CONDITIONAL_SKILL_NAMES
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        activated_names.extend(activated_skills.into_iter().map(|(name, _)| name));
    }
    DYNAMIC_SKILLS_GENERATION.fetch_add(1, Ordering::AcqRel);
    activated
}

/// Maps to CC `getConditionalSkillCount()`.
pub fn get_conditional_skill_count() -> usize {
    CONDITIONAL_SKILLS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .len()
}

/// Maps to CC `clearSkillCaches()` (`:806-811`).
///
/// CC also clears `loadMarkdownFilesForSubdir.cache`; this port's
/// `load_markdown_files_for_subdir` walks the disk on every call and has no
/// cache to clear, so that line has no counterpart yet. Everything else is 1:1.
pub fn clear_skill_caches() {
    SKILL_DIR_COMMANDS_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
    CONDITIONAL_SKILLS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
    ACTIVATED_CONDITIONAL_SKILL_NAMES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
}

/// Maps to CC `clearDynamicSkills()`.
pub fn clear_dynamic_skills() {
    DYNAMIC_SKILL_DIRS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
    DYNAMIC_SKILLS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
    CONDITIONAL_SKILLS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
    ACTIVATED_CONDITIONAL_SKILL_NAMES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
    DYNAMIC_SKILLS_GENERATION.fetch_add(1, Ordering::AcqRel);
}

/// Maps to CC `loadSkillsFromSkillsDir(basePath, source)`.
pub fn load_skills_from_skills_dir(base_path: &Path, source: SkillSource) -> Vec<SkillWithPath> {
    let entries = match fs::read_dir(base_path) {
        Ok(entries) => entries,
        Err(error) => {
            // Maps to: CC `:416-419` — `if (!isFsInaccessible(e)) logError(e)`.
            // A missing/denied/not-a-directory skills root is the ordinary case
            // and stays quiet; anything else is a real fault worth surfacing.
            if !is_fs_inaccessible(&error) {
                crate::utils::debug::log_for_debugging_with_level(
                    &format!("[skills] failed to read {}: {error}", base_path.display()),
                    crate::utils::debug::DebugLogLevel::Error,
                );
            }
            return Vec::new();
        }
    };

    let mut entries = entries.flatten().collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.file_name());

    let mut out = Vec::new();
    for entry in entries {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if !(file_type.is_dir() || file_type.is_symlink() && path.is_dir()) {
            continue;
        }
        let skill_name = entry.file_name().to_string_lossy().to_string();
        let skill_file_path = path.join("SKILL.md");
        let raw = match fs::read_to_string(&skill_file_path) {
            Ok(raw) => raw,
            Err(error) => {
                // Maps to: CC `:436-445`. A directory without SKILL.md is the
                // normal "not a skill" case and stays silent; EACCES/EPERM/EIO
                // are configuration faults and were previously indistinguishable
                // from "no such skill" — the directory just never appeared.
                if error.kind() != std::io::ErrorKind::NotFound {
                    crate::utils::debug::log_for_debugging_with_level(
                        &format!(
                            "[skills] failed to read {}: {error}",
                            skill_file_path.display()
                        ),
                        crate::utils::debug::DebugLogLevel::Warn,
                    );
                }
                continue;
            }
        };
        let parsed = parse_frontmatter(raw.strip_prefix('\u{feff}').unwrap_or(&raw));
        let Some(skill) = create_skill_command(CreateSkillCommandInput {
            skill_name,
            markdown_content: parsed.content,
            frontmatter: parsed.frontmatter,
            source,
            base_dir: Some(path.clone()),
            loaded_from: SkillLoadedFrom::Skills,
            file_path: skill_file_path.clone(),
            description_fallback_label: "Skill",
        }) else {
            continue;
        };
        out.push(SkillWithPath {
            skill,
            file_path: skill_file_path,
        });
    }
    out
}

/// Maps to CC `loadSkillsFromCommandsDir(cwd)` after
/// `loadMarkdownFilesForSubdir('commands', cwd)` has produced one base dir.
pub fn load_skills_from_commands_dir(base_dir: &Path, source: SkillSource) -> Vec<SkillWithPath> {
    let base_dir = base_dir
        .canonicalize()
        .unwrap_or_else(|_| base_dir.to_path_buf());
    let mut files = markdown_files(&base_dir);
    files.sort();
    let files = transform_skill_files(files);

    let mut out = Vec::new();
    let mut seen_paths = HashSet::new();
    for file_path in files {
        let canonical = file_path
            .canonicalize()
            .unwrap_or_else(|_| file_path.clone());
        if !seen_paths.insert(canonical) {
            continue;
        }
        let Ok(raw) = fs::read_to_string(&file_path) else {
            continue;
        };
        let parsed = parse_frontmatter(raw.strip_prefix('\u{feff}').unwrap_or(&raw));
        let skill_name = command_name_for_legacy_file(&file_path, &base_dir);
        if skill_name.is_empty() {
            continue;
        }
        let Some(skill) = create_skill_command(CreateSkillCommandInput {
            skill_name,
            markdown_content: parsed.content,
            frontmatter: parsed.frontmatter,
            source,
            base_dir: is_skill_file(&file_path)
                .then(|| file_path.parent().map(Path::to_path_buf))
                .flatten(),
            loaded_from: SkillLoadedFrom::CommandsDeprecated,
            file_path: file_path.clone(),
            description_fallback_label: "Custom command",
        }) else {
            continue;
        };
        out.push(SkillWithPath { skill, file_path });
    }
    out
}

struct CreateSkillCommandInput {
    skill_name: String,
    markdown_content: String,
    frontmatter: BTreeMap<String, Value>,
    source: SkillSource,
    base_dir: Option<PathBuf>,
    loaded_from: SkillLoadedFrom,
    file_path: PathBuf,
    description_fallback_label: &'static str,
}

/// Maps to CC `parseSkillFrontmatterFields(...)` + `createSkillCommand(...)`.
fn create_skill_command(input: CreateSkillCommandInput) -> Option<SkillCommand> {
    let parsed = parse_skill_frontmatter_fields(
        &input.frontmatter,
        &input.markdown_content,
        &input.skill_name,
        input.description_fallback_label,
    );

    Some(SkillCommand {
        name: input.skill_name,
        display_name: parsed.display_name,
        description: parsed.description,
        has_user_specified_description: parsed.has_user_specified_description,
        content_length: input.markdown_content.len(),
        markdown_content: input.markdown_content,
        allowed_tools: parsed.allowed_tools,
        argument_hint: parsed.argument_hint,
        argument_names: parsed.argument_names,
        when_to_use: parsed.when_to_use,
        version: parsed.version,
        model: parsed.model,
        effort: parsed.effort,
        disable_model_invocation: parsed.disable_model_invocation,
        user_invocable: parsed.user_invocable,
        execution_context: parsed
            .execution_context
            .unwrap_or(SkillExecutionContext::Inline),
        agent: parsed.agent,
        shell: parsed.shell,
        hooks: parsed.hooks,
        paths: parse_skill_paths(&input.frontmatter),
        source: input.source,
        loaded_from: input.loaded_from,
        skill_root: input.base_dir,
        file_path: input.file_path,
    })
}

#[derive(Debug, PartialEq, Eq)]
struct ParsedSkillFrontmatterFields {
    display_name: Option<String>,
    description: String,
    has_user_specified_description: bool,
    allowed_tools: Vec<String>,
    argument_hint: Option<String>,
    argument_names: Vec<String>,
    when_to_use: Option<String>,
    version: Option<String>,
    model: Option<String>,
    effort: Option<crate::utils::effort::EffortValue>,
    disable_model_invocation: bool,
    user_invocable: bool,
    execution_context: Option<SkillExecutionContext>,
    agent: Option<String>,
    shell: Option<FrontmatterShell>,
    hooks: Option<crate::services::hooks::HooksConfig>,
}

/// Maps to CC `parseHooksFromFrontmatter(frontmatter, skillName)`
/// (`loadSkillsDir.ts:136-153`).
///
/// CC runs `HooksSchema().safeParse` and returns `undefined` on failure, which
/// serde's `from_value` reproduces: `HooksSchema` is
/// `z.partialRecord(z.enum(HOOK_EVENTS), z.array(HookMatcherSchema()))`, so a
/// malformed matcher list rejects the whole value rather than being repaired.
fn parse_hooks_from_frontmatter(
    frontmatter: &BTreeMap<String, Value>,
    skill_name: &str,
) -> Option<crate::services::hooks::HooksConfig> {
    let raw = frontmatter.get("hooks")?;
    if raw.is_null() {
        return None;
    }
    match serde_json::from_value::<crate::services::hooks::HooksConfig>(raw.clone()) {
        Ok(hooks) => Some(hooks),
        Err(error) => {
            tracing::debug!(skill = skill_name, %error, "invalid hooks in skill");
            None
        }
    }
}

fn parse_skill_frontmatter_fields(
    frontmatter: &BTreeMap<String, Value>,
    markdown_content: &str,
    resolved_name: &str,
    description_fallback_label: &str,
) -> ParsedSkillFrontmatterFields {
    let validated_description = coerce_description_to_string(frontmatter.get("description"));
    let description = validated_description.clone().unwrap_or_else(|| {
        extract_description_from_markdown(markdown_content, description_fallback_label)
    });

    let model = string_field(frontmatter, "model").and_then(|model| {
        let trimmed = model.trim();
        (!trimmed.is_empty() && trimmed != "inherit").then(|| trimmed.to_string())
    });
    let effort_raw = string_field(frontmatter, "effort");
    let effort = effort_raw
        .as_deref()
        .and_then(crate::utils::effort::parse_effort_value);
    if effort_raw.is_some() && effort.is_none() {
        tracing::debug!(
            skill = resolved_name,
            effort = effort_raw.as_deref().unwrap_or_default(),
            "skill has invalid effort frontmatter"
        );
    }

    ParsedSkillFrontmatterFields {
        display_name: string_field(frontmatter, "name"),
        description,
        has_user_specified_description: validated_description.is_some(),
        allowed_tools:
            crate::utils::markdown_config_loader::parse_slash_command_tools_from_frontmatter(
                frontmatter.get("allowed-tools"),
            ),
        argument_hint: string_field(frontmatter, "argument-hint"),
        argument_names: parse_argument_names(frontmatter.get("arguments")),
        when_to_use: string_field(frontmatter, "when_to_use"),
        version: string_field(frontmatter, "version"),
        model,
        effort,
        disable_model_invocation: parse_bool(frontmatter.get("disable-model-invocation")),
        user_invocable: frontmatter
            .get("user-invocable")
            .map(|value| parse_bool(Some(value)))
            .unwrap_or(true),
        execution_context: (string_field(frontmatter, "context").as_deref() == Some("fork"))
            .then_some(SkillExecutionContext::Fork),
        agent: string_field(frontmatter, "agent"),
        shell: parse_shell_frontmatter(frontmatter.get("shell"), resolved_name),
        hooks: parse_hooks_from_frontmatter(frontmatter, resolved_name),
    }
}

fn string_field(frontmatter: &BTreeMap<String, Value>, key: &str) -> Option<String> {
    match frontmatter.get(key)? {
        Value::String(value) => {
            let trimmed = value.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        }
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        Value::Null | Value::Array(_) | Value::Object(_) => None,
    }
}

fn parse_bool(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Bool(value)) => *value,
        Some(Value::String(value)) => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "true" | "1" | "yes" | "on"
        ),
        Some(Value::Number(value)) => value.as_i64().unwrap_or(0) != 0,
        _ => false,
    }
}

/// Maps to CC `skills/loadSkillsDir.ts:159-178#parseSkillPaths`.
///
/// The stages are ordered as CC orders them: `splitPathInFrontmatter` splits on
/// top-level commas AND expands brace patterns, THEN `/**` is stripped, THEN
/// empties are filtered. Expanding before stripping is what makes
/// `src/{a,b}/**` become `["src/a", "src/b"]` instead of a pair of patterns
/// that still carry `/**`.
fn parse_skill_paths(frontmatter: &BTreeMap<String, Value>) -> Option<Vec<String>> {
    let patterns =
        crate::utils::frontmatter_parser::split_path_in_frontmatter(frontmatter.get("paths")?)
            .into_iter()
            .map(|pattern| {
                // Remove /** suffix - the ignore library treats 'path' as matching both
                // the path itself and everything inside it.
                pattern
                    .strip_suffix("/**")
                    .map(ToOwned::to_owned)
                    .unwrap_or(pattern)
            })
            .filter(|pattern| !pattern.is_empty())
            .collect::<Vec<_>>();

    (!patterns.is_empty() && !patterns.iter().all(|pattern| pattern == "**")).then_some(patterns)
}

fn deduplicate_skills_by_file_identity(skills: Vec<SkillWithPath>) -> Vec<SkillWithPath> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for entry in skills {
        let identity = get_file_identity(&entry.file_path);
        if let Some(identity) = identity {
            if !seen.insert(identity) {
                continue;
            }
        }
        out.push(entry);
    }
    out
}

/// Maps to: CC `skills/loadSkillsDir.ts#getFileIdentity:118-124`.
///
/// `realpath`, not `dev:ino`. CC moved off stat identity deliberately and says
/// why at `:107-117`: filesystems that report inode 0 (virtual/container/NFS) or
/// lose precision (ExFAT) made unrelated skills collide and silently disappear —
/// anthropics/claude-code#13893. The trade is intentional: two hard links to one
/// SKILL.md are two identities under realpath and one under `dev:ino`, and CC
/// takes the symlink-correct side.
fn get_file_identity(path: &Path) -> Option<String> {
    fs::canonicalize(path)
        .ok()
        .map(|path| path.to_string_lossy().to_string())
}

/// Maps to: CC `isFsInaccessible(e)` (`utils/errors.ts:186-195`) — the errno set
/// `loadSkillsFromSkillsDir` treats as "this root is simply not there".
/// `ENOTDIR`/`ELOOP` have no stable `io::ErrorKind`, so they go by raw errno.
fn is_fs_inaccessible(error: &std::io::Error) -> bool {
    if matches!(
        error.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
    ) {
        return true;
    }
    #[cfg(unix)]
    {
        matches!(
            error.raw_os_error(),
            Some(code) if code == libc::ENOTDIR || code == libc::ELOOP
        )
    }
    #[cfg(not(unix))]
    {
        false
    }
}

fn markdown_files(base_dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect_markdown_files(base_dir, &mut out, &mut HashSet::new());
    out
}

fn collect_markdown_files(dir: &Path, out: &mut Vec<PathBuf>, seen_dirs: &mut HashSet<PathBuf>) {
    let canonical = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    if !seen_dirs.insert(canonical) {
        return;
    }

    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() || file_type.is_symlink() && path.is_dir() {
            collect_markdown_files(&path, out, seen_dirs);
        } else if path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
        {
            out.push(path);
        }
    }
}

/// Maps to CC `transformSkillFiles(files)`.
fn transform_skill_files(files: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut by_dir: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
    for file in files {
        let dir = file.parent().map(Path::to_path_buf).unwrap_or_default();
        by_dir.entry(dir).or_default().push(file);
    }

    let mut dirs = by_dir.into_iter().collect::<Vec<_>>();
    dirs.sort_by(|(left, _), (right, _)| left.cmp(right));

    let mut out = Vec::new();
    for (_, mut dir_files) in dirs {
        dir_files.sort();
        if let Some(skill_file) = dir_files.iter().find(|path| is_skill_file(path)).cloned() {
            out.push(skill_file);
        } else {
            out.extend(dir_files);
        }
    }
    out
}

fn is_skill_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("SKILL.md"))
}

fn command_name_for_legacy_file(file_path: &Path, base_dir: &Path) -> String {
    if is_skill_file(file_path) {
        let Some(skill_dir) = file_path.parent() else {
            return String::new();
        };
        let Some(parent_of_skill_dir) = skill_dir.parent() else {
            return String::new();
        };
        let Some(base_name) = skill_dir.file_name().and_then(|name| name.to_str()) else {
            return String::new();
        };
        let namespace = namespace_for(parent_of_skill_dir, base_dir);
        if namespace.is_empty() {
            base_name.to_string()
        } else {
            format!("{namespace}:{base_name}")
        }
    } else {
        let Some(file_dir) = file_path.parent() else {
            return String::new();
        };
        let Some(stem) = file_path.file_stem().and_then(|stem| stem.to_str()) else {
            return String::new();
        };
        let namespace = namespace_for(file_dir, base_dir);
        if namespace.is_empty() {
            stem.to_string()
        } else {
            format!("{namespace}:{stem}")
        }
    }
}

fn namespace_for(target_dir: &Path, base_dir: &Path) -> String {
    let relative = target_dir.strip_prefix(base_dir).unwrap_or(target_dir);
    relative
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(part) => part.to_str(),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(":")
}

fn display_path_for_prompt(path: &Path) -> String {
    let value = path.to_string_lossy().to_string();
    #[cfg(target_os = "windows")]
    {
        value.replace('\\', "/")
    }
    #[cfg(not(target_os = "windows"))]
    {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};
    use std::io::Write;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("cometix-skills-{name}-{}", uuid::Uuid::new_v4()));
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

    /// A minimal loaded skill: every optional frontmatter field absent, which
    /// is the shape `createSkillCommand` produces for a SKILL.md carrying only
    /// a description.
    fn skill_command_fixture(name: &str) -> SkillCommand {
        SkillCommand {
            name: name.to_string(),
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
            hooks: None,
            paths: None,
            source: SkillSource::ProjectSettings,
            loaded_from: SkillLoadedFrom::Skills,
            skill_root: None,
            file_path: PathBuf::from("SKILL.md"),
        }
    }

    struct SkillDiscoveryStateGuard {
        sources: Vec<String>,
        additional_dirs: Vec<PathBuf>,
    }

    impl SkillDiscoveryStateGuard {
        fn capture() -> Self {
            Self {
                sources: crate::bootstrap::state::get_allowed_setting_sources(),
                additional_dirs: crate::bootstrap::state::get_additional_directories_for_claude_md(
                ),
            }
        }
    }

    impl Drop for SkillDiscoveryStateGuard {
        fn drop(&mut self) {
            crate::bootstrap::state::set_allowed_setting_sources(self.sources.clone());
            crate::bootstrap::state::set_additional_directories_for_claude_md(
                self.additional_dirs.clone(),
            );
        }
    }

    #[test]
    fn initial_skill_dirs_apply_source_plugin_only_and_bare_mode_gates() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _state = SkillDiscoveryStateGuard::capture();
        let _dynamic = DynamicSkillsTestSnapshot::capture();
        clear_dynamic_skills();
        let root = temp_dir("initial-gates");
        let managed = root.join("managed");
        let config = root.join("config");
        let project = root.join("project");
        let additional = root.join("additional");
        std::fs::create_dir_all(project.join(".git")).unwrap();
        for (path, description) in [
            (
                managed.join(".claude/skills/managed-skill/SKILL.md"),
                "Managed skill",
            ),
            (config.join("skills/user-skill/SKILL.md"), "User skill"),
            (
                project.join(".claude/skills/project-skill/SKILL.md"),
                "Project skill",
            ),
            (
                additional.join(".claude/skills/additional-skill/SKILL.md"),
                "Additional skill",
            ),
        ] {
            write_file(
                &path,
                &format!("---\ndescription: {description}\n---\nBody"),
            );
        }
        for (path, body) in [
            (
                managed.join(".claude/commands/managed-command.md"),
                "Managed command",
            ),
            (config.join("commands/user-command.md"), "User command"),
            (
                project.join(".claude/commands/project-command.md"),
                "Project command",
            ),
        ] {
            write_file(&path, body);
        }
        let _managed = EnvVarGuard::set("CLAUDE_CODE_MANAGED_SETTINGS_PATH", &managed);
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config);
        let _simple = EnvVarGuard::set("CLAUDE_CODE_SIMPLE", "0");
        let _policy_disabled = EnvVarGuard::set("CLAUDE_CODE_DISABLE_POLICY_SKILLS", "0");
        crate::bootstrap::state::set_allowed_setting_sources(vec!["projectSettings".to_string()]);
        crate::bootstrap::state::set_additional_directories_for_claude_md(vec![additional.clone()]);

        // `get_skill_dir_commands` is memoized by cwd (CC `:638`), and this test
        // re-reads the same cwd under three different policies, so each round
        // clears the skill caches the way CC's `clearSkillCaches()` does.
        clear_skill_caches();
        let names = get_skill_dir_commands(&project)
            .into_iter()
            .map(|skill| skill.name)
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(
            names,
            std::collections::HashSet::from([
                "managed-skill".to_string(),
                "project-skill".to_string(),
                "additional-skill".to_string(),
                "managed-command".to_string(),
                "project-command".to_string(),
            ])
        );

        std::fs::write(
            managed.join("managed-settings.json"),
            r#"{"strictPluginOnlyCustomization":["skills"]}"#,
        )
        .unwrap();
        // Writing the file directly bypasses `update_settings_for_source`, the
        // only writer that invalidates the merged-settings cache
        // (settings.ts:505-506). The read above already populated it.
        crate::utils::settings::settings_cache::reset_settings_cache();
        clear_skill_caches();
        let names = get_skill_dir_commands(&project)
            .into_iter()
            .map(|skill| skill.name)
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["managed-skill"]);

        std::fs::remove_file(managed.join("managed-settings.json")).unwrap();
        crate::utils::settings::settings_cache::reset_settings_cache();
        crate::utils::process_env::set("CLAUDE_CODE_SIMPLE", "1");
        clear_skill_caches();
        let names = get_skill_dir_commands(&project)
            .into_iter()
            .map(|skill| skill.name)
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["additional-skill"]);

        clear_skill_caches();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn skill_allowed_tools_use_canonical_parenthesis_aware_frontmatter_parser() {
        let frontmatter = BTreeMap::from([(
            "allowed-tools".to_string(),
            Value::String("Bash(echo one, echo two), Read".to_string()),
        )]);

        let parsed = parse_skill_frontmatter_fields(&frontmatter, "body", "review", "Skill");

        assert_eq!(
            parsed.allowed_tools,
            vec!["Bash(echo one, echo two)".to_string(), "Read".to_string()]
        );
    }

    #[test]
    fn loads_skills_directory_format_only_like_official() {
        let root = temp_dir("skills-dir");
        let skills = root.join("skills");
        write_file(
            &skills.join("review").join("SKILL.md"),
            "---\nname: Review Display\ndescription: Review code\nallowed-tools: Read, Grep\nargument-hint: [scope]\narguments: scope mode\nmodel: opus\npaths: src/**, tests/**\n---\nUse $scope in $mode. Dir=${CLAUDE_SKILL_DIR} Session=${CLAUDE_SESSION_ID}",
        );
        write_file(&skills.join("ignored.md"), "# ignored");

        let loaded = load_skills_from_skills_dir(&skills, SkillSource::ProjectSettings);
        assert_eq!(loaded.len(), 1);
        let skill = &loaded[0].skill;
        assert_eq!(skill.name, "review");
        assert_eq!(skill.user_facing_name(), "Review Display");
        assert_eq!(skill.allowed_tools, vec!["Read", "Grep"]);
        assert_eq!(skill.argument_names, vec!["scope", "mode"]);
        assert_eq!(
            skill.paths.as_deref(),
            Some(&["src".to_string(), "tests".to_string()][..])
        );
        assert_eq!(skill.model.as_deref(), Some("opus"));
        let prompt = skill
            .get_prompt_for_command_without_shell(Some("core quick"), "session-1")
            .unwrap();
        assert!(prompt.contains("Base directory for this skill:"));
        assert!(prompt.contains("Use core in quick."));
        assert!(prompt.contains("Session=session-1"));
    }

    #[test]
    fn braced_skill_paths_expand_before_the_glob_suffix_is_stripped() {
        // Maps to CC `parseSkillPaths` (loadSkillsDir.ts:159-178) over
        // `splitPathInFrontmatter` (frontmatterParser.ts:189-232).
        let root = temp_dir("skills-braces");
        let skills = root.join("skills");
        write_file(
            &skills.join("web").join("SKILL.md"),
            "---\ndescription: Web skill\npaths: src/**/*.{ts,tsx}, packages/{app,lib}/**\n---\nBody",
        );

        let loaded = load_skills_from_skills_dir(&skills, SkillSource::ProjectSettings);
        assert_eq!(loaded.len(), 1);
        // Before brace expansion landed this was
        // ["src/**/*.{ts", "tsx}", "packages/{app", "lib}"] — four patterns that
        // can never match a real file, so the skill stayed conditional forever.
        assert_eq!(
            loaded[0].skill.paths.as_deref(),
            Some(
                &[
                    "src/**/*.ts".to_string(),
                    "src/**/*.tsx".to_string(),
                    "packages/app".to_string(),
                    "packages/lib".to_string(),
                ][..]
            )
        );
    }

    #[test]
    fn conditional_skill_with_braced_paths_activates_instead_of_staying_invisible() {
        // H1 end to end: a `paths:` brace pattern must reach
        // `activateConditionalSkillsForPaths` as expanded globs. Old shape: the
        // unexpanded patterns matched nothing, so the skill was never activated
        // and never appeared in `get_skill_dir_commands` — silently invisible,
        // no error. The assertion fails (it does not hang).
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let _snapshot = DynamicSkillsTestSnapshot::capture();
        clear_dynamic_skills();
        let root = std::env::temp_dir().join(format!(
            "cometix-braced-conditional-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let skill_dir = root.join(".claude/skills/typescript-only");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: typescript-only\ndescription: TS skill\npaths: src/**/*.{ts,tsx}\n---\nTS body",
        )
        .unwrap();

        let initially_visible = get_skill_dir_commands(&root);
        assert!(
            !initially_visible
                .iter()
                .any(|skill| skill.name == "typescript-only")
        );
        assert_eq!(get_conditional_skill_count(), 1);

        let activated = activate_conditional_skills_for_paths(&[root.join("src/app.tsx")], &root);
        assert_eq!(activated, vec!["typescript-only"]);
        // Activation moves the skill into the DYNAMIC map (CC `:1031`), which
        // `getCommands` merges — `getSkillDirCommands` itself returns only
        // unconditional static skills and is memoized besides.
        assert!(
            get_dynamic_skills()
                .iter()
                .any(|skill| skill.name == "typescript-only")
        );

        clear_skill_caches();
        clear_dynamic_skills();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn legacy_commands_transform_skill_file_and_regular_markdown_namespaces() {
        let root = temp_dir("legacy");
        let commands = root.join("commands");
        write_file(
            &commands.join("agents").join("fixer").join("SKILL.md"),
            "---\ndescription: Fix things\ncontext: fork\n---\nFix body",
        );
        write_file(
            &commands.join("agents").join("fixer").join("ignored.md"),
            "# ignored sibling",
        );
        write_file(
            &commands.join("qa").join("smoke.md"),
            "# Smoke\nRun $ARGUMENTS",
        );

        let loaded = load_skills_from_commands_dir(&commands, SkillSource::ProjectSettings);
        assert_eq!(loaded.len(), 2);
        let names = loaded
            .iter()
            .map(|entry| entry.skill.name.as_str())
            .collect::<Vec<_>>();
        assert!(names.contains(&"agents:fixer"));
        assert!(names.contains(&"qa:smoke"));
        let fixer = loaded
            .iter()
            .find(|entry| entry.skill.name == "agents:fixer")
            .unwrap();
        assert_eq!(fixer.skill.execution_context, SkillExecutionContext::Fork);
        assert_eq!(fixer.skill.loaded_from, SkillLoadedFrom::CommandsDeprecated);
    }

    #[test]
    fn get_prompt_for_command_expands_allowed_shell_commands() {
        // `printf`, not `echo`: `echo` is classified read-only
        // (bash_tool/read_only_validation.rs:1370) and auto-allowed, so an
        // echo-based fixture passes with no `allowed-tools` grant at all and
        // proves nothing about the widening this test is named for.
        let mut command = skill_command_fixture("shell");
        command.description = "Shell".to_string();
        command.markdown_content =
            "Before !`printf skill-inline`\n```!\nprintf skill-block\n```".to_string();
        command.content_length = 0;
        command.allowed_tools = vec!["Bash(printf *)".to_string()];

        let prompt = command
            .get_prompt_for_command(None, "session-1", &crate::tool::ToolUseContext::default())
            .expect("allowed shell expansion succeeds");
        assert!(prompt.contains("Before skill-inline"));
        assert!(prompt.contains("skill-block"));
        assert!(!prompt.contains("!`"));
    }

    #[test]
    fn skill_frontmatter_hooks_parse_and_reach_the_session_hook_registry() {
        // Maps to CC `parseHooksFromFrontmatter` (loadSkillsDir.ts:136-153),
        // `createSkillCommand(...).hooks` (:342) and the registration at
        // `processSlashCommand.tsx:1172-1186`. Old shape: `SkillCommand` had no
        // `hooks` field at all, so the frontmatter was dropped on the floor and
        // this assert_eq on registered_count saw 0.
        use crate::services::hooks::HookEvent;
        use crate::utils::hooks::session_hooks::{
            clear_all_session_hooks, get_session_hook_matchers,
        };

        let root = temp_dir("skills-hooks");
        let skills = root.join("skills");
        write_file(
            &skills.join("guard").join("SKILL.md"),
            "---\ndescription: Guard skill\nhooks:\n  PreToolUse:\n    - matcher: Bash\n      hooks:\n        - type: command\n          command: echo guard\n---\nBody",
        );

        let loaded = load_skills_from_skills_dir(&skills, SkillSource::ProjectSettings);
        let skill = &loaded[0].skill;
        assert!(skill.hooks.is_some());

        let session = format!("skill-hooks-{}", uuid::Uuid::new_v4().simple());
        assert_eq!(skill.register_skill_hooks(&session), 1);
        let matchers = get_session_hook_matchers(&session, Some(HookEvent::PreToolUse));
        let pre = matchers
            .get(&HookEvent::PreToolUse)
            .expect("PreToolUse hooks");
        assert_eq!(pre[0].matcher, "Bash");
        assert_eq!(pre[0].hooks[0].command, "echo guard");
        // skillRoot rides along (CC registerSkillHooks.ts:52) so hook commands
        // can resolve CLAUDE_PLUGIN_ROOT.
        assert_eq!(
            pre[0].skill_root.as_deref(),
            skill.skill_root.as_ref().map(|root| root.to_str().unwrap())
        );
        assert!(
            pre[0]
                .skill_root
                .as_deref()
                .is_some_and(|root| root.ends_with("guard"))
        );

        clear_all_session_hooks();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn plugin_only_hooks_policy_blocks_untrusted_skill_hook_registration() {
        // Maps to CC `processSlashCommand.tsx:1175-1177`
        // `!isRestrictedToPluginOnly('hooks') || isSourceAdminTrusted(source)`.
        // Same gate as agent frontmatter hooks (run_agent.rs:1311-1318). The
        // policy is read from managed settings on disk, not a memory seed, so
        // this exercises the real predicate.
        use crate::utils::hooks::session_hooks::clear_all_session_hooks;

        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let root = temp_dir("skill-hooks-policy");
        let managed = root.join("managed");
        write_file(
            &managed.join("managed-settings.json"),
            r#"{"strictPluginOnlyCustomization":["hooks"]}"#,
        );
        let _managed_env = EnvVarGuard::set("CLAUDE_CODE_MANAGED_SETTINGS_PATH", &managed);
        crate::utils::settings::settings_cache::reset_settings_cache();

        let hooks: crate::services::hooks::HooksConfig = serde_json::from_value(
            serde_json::json!({"Stop": [{"hooks": [{"type": "command", "command": "echo locked"}]}]}),
        )
        .unwrap();

        let mut project_skill = skill_command_fixture("project-hooks");
        project_skill.hooks = Some(hooks.clone());
        let session = format!("policy-{}", uuid::Uuid::new_v4().simple());
        assert_eq!(project_skill.register_skill_hooks(&session), 0);

        // policySettings is admin-trusted, so the same skill from managed
        // policy still registers.
        let mut policy_skill = skill_command_fixture("policy-hooks");
        policy_skill.hooks = Some(hooks);
        policy_skill.source = SkillSource::PolicySettings;
        assert_eq!(policy_skill.register_skill_hooks(&session), 1);

        clear_all_session_hooks();
        crate::utils::settings::settings_cache::reset_settings_cache();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn malformed_skill_hooks_frontmatter_registers_nothing() {
        // CC's `HooksSchema().safeParse` failure arm returns undefined
        // (loadSkillsDir.ts:144-150) — an unusable hooks block must not
        // half-register.
        let root = temp_dir("skills-bad-hooks");
        let skills = root.join("skills");
        write_file(
            &skills.join("broken").join("SKILL.md"),
            "---\ndescription: Broken\nhooks: not-a-record\n---\nBody",
        );

        let loaded = load_skills_from_skills_dir(&skills, SkillSource::ProjectSettings);
        assert_eq!(loaded[0].skill.hooks, None);
        assert_eq!(loaded[0].skill.register_skill_hooks("unused"), 0);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn shell_expansion_replaces_the_command_rule_source_instead_of_merging() {
        // Maps to CC `loadSkillsDir.ts:377-392` `command: allowedTools`.
        //
        // A skill with no `allowed-tools` must run its `!` blocks with the
        // `command` source EMPTIED, not with whatever another skill already put
        // there this turn. Old shape: the fork-path union helper short-circuits
        // on an empty allow list and returns the parent state untouched, so the
        // inherited grant applied and the command RAN — this expect_err failed.
        use crate::types::permissions::PermissionRuleSource;
        use crate::utils::permissions::permission_rule_parser::permission_rule_value_from_string;

        let mut context = crate::tool::ToolUseContext::default();
        context.tool_permission_context.always_allow_rules.insert(
            PermissionRuleSource::Command,
            vec![permission_rule_value_from_string("Bash(printf *)")],
        );

        let mut command = skill_command_fixture("no-tools");
        command.markdown_content = "Leaked !`printf leaked-grant`".to_string();
        let error = command
            .get_prompt_for_command(None, "session-1", &context)
            .expect_err("an inherited command-source grant must not reach this skill");
        assert!(
            error
                .to_string()
                .contains("Shell command permission check failed")
        );

        // Its own `allowed-tools` is what does reach it.
        let mut allowed = skill_command_fixture("own-tools");
        allowed.markdown_content = "Own !`printf own-grant`".to_string();
        allowed.allowed_tools = vec!["Bash(printf *)".to_string()];
        let prompt = allowed
            .get_prompt_for_command(None, "session-1", &context)
            .expect("the skill's own allowed-tools grant applies");
        assert!(prompt.contains("Own own-grant"));
    }

    #[test]
    fn mcp_loaded_skills_never_run_inline_shell_commands() {
        // Maps to CC `loadSkillsDir.ts:371-374` — the `loadedFrom !== 'mcp'`
        // guard. Remote markdown must not execute `!` blocks.
        let mut command = skill_command_fixture("remote");
        command.loaded_from = SkillLoadedFrom::Mcp;
        command.allowed_tools = vec!["Bash(echo *)".to_string()];
        command.markdown_content = "Remote !`echo remote-output`".to_string();

        let prompt = command
            .get_prompt_for_command(None, "session-1", &crate::tool::ToolUseContext::default())
            .expect("mcp skills skip shell expansion instead of failing");
        assert!(prompt.contains("!`echo remote-output`"));
        assert!(!prompt.contains("remote-output\n"));
    }

    #[test]
    fn dynamic_nested_and_conditional_skills_follow_file_paths() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let _snapshot = DynamicSkillsTestSnapshot::capture();
        clear_dynamic_skills();
        let root = std::env::temp_dir().join(format!(
            "cometix-dynamic-skills-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let nested_skills = root.join("packages/pkg/.claude/skills");
        let dynamic_skill = nested_skills.join("nested");
        let conditional_skill = root.join(".claude/skills/rust-only");
        std::fs::create_dir_all(&dynamic_skill).unwrap();
        std::fs::create_dir_all(&conditional_skill).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            dynamic_skill.join("SKILL.md"),
            "---\nname: nested\ndescription: Nested skill\n---\nNested body",
        )
        .unwrap();
        std::fs::write(
            conditional_skill.join("SKILL.md"),
            "---\nname: rust-only\ndescription: Rust skill\npaths: src/**/*.rs\n---\nRust body",
        )
        .unwrap();
        let _ = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&root)
            .status();

        let touched = root.join("packages/pkg/src/lib.rs");
        let dirs = discover_skill_dirs_for_paths(std::slice::from_ref(&touched), &root);
        assert_eq!(dirs, vec![nested_skills]);
        add_skill_directories(&dirs);
        assert!(
            get_dynamic_skills()
                .iter()
                .any(|skill| skill.name == "nested")
        );

        let initially_visible = get_skill_dir_commands(&root);
        assert!(
            !initially_visible
                .iter()
                .any(|skill| skill.name == "rust-only")
        );
        assert_eq!(get_conditional_skill_count(), 1);
        let activated = activate_conditional_skills_for_paths(&[root.join("src/lib.rs")], &root);
        assert_eq!(activated, vec!["rust-only"]);
        assert!(
            get_dynamic_skills()
                .iter()
                .any(|skill| skill.name == "rust-only")
        );

        clear_skill_caches();
        clear_dynamic_skills();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn visible_skill_keeps_precedence_over_same_named_dynamic_skill() {
        // The dedup lives in `getCommands` (`commands.ts:492-498`), so it is
        // asserted there: `getSkillDirCommands` no longer sees dynamic skills
        // at all. The claim is unchanged — the base command wins the name.
        struct AllowedSourcesRestore(Vec<String>);
        impl Drop for AllowedSourcesRestore {
            fn drop(&mut self) {
                crate::bootstrap::state::set_allowed_setting_sources(self.0.clone());
            }
        }

        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _dynamic = DynamicSkillsTestSnapshot::capture();
        let _sources =
            AllowedSourcesRestore(crate::bootstrap::state::get_allowed_setting_sources());
        crate::commands::clear_commands_cache();
        clear_dynamic_skills();
        crate::bootstrap::state::set_allowed_setting_sources(vec!["projectSettings".to_string()]);
        let root = temp_dir("dynamic-precedence");
        let name = format!("collision-{}", uuid::Uuid::new_v4().simple());
        write_file(
            &root.join(".claude/skills").join(&name).join("SKILL.md"),
            &format!("---\nname: {name}\ndescription: Visible project skill\n---\nVisible"),
        );
        let dynamic_root = root.join("discovered");
        write_file(
            &dynamic_root.join(&name).join("SKILL.md"),
            &format!("---\nname: {name}\ndescription: Dynamic discovered skill\n---\nDynamic"),
        );
        add_skill_directories(std::slice::from_ref(&dynamic_root));

        let visible = crate::commands::get_commands(&root);
        let matches = visible
            .iter()
            .filter(|command| command.name == name)
            .collect::<Vec<_>>();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].description, "Visible project skill");

        crate::commands::clear_commands_cache();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn dynamic_skills_preserve_map_insertion_order() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let _snapshot = DynamicSkillsTestSnapshot::capture();
        clear_dynamic_skills();
        let root = std::env::temp_dir().join(format!(
            "cometix-dynamic-skill-order-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let first_dir = root.join("first");
        let second_dir = root.join("second");
        std::fs::create_dir_all(first_dir.join("zeta")).unwrap();
        std::fs::create_dir_all(second_dir.join("alpha")).unwrap();
        std::fs::write(
            first_dir.join("zeta/SKILL.md"),
            "---\nname: zeta\ndescription: Zeta\n---\nBody",
        )
        .unwrap();
        std::fs::write(
            second_dir.join("alpha/SKILL.md"),
            "---\nname: alpha\ndescription: Alpha\n---\nBody",
        )
        .unwrap();

        add_skill_directories(&[first_dir, second_dir]);
        assert_eq!(
            get_dynamic_skills()
                .into_iter()
                .map(|skill| skill.name)
                .collect::<Vec<_>>(),
            vec!["alpha", "zeta"]
        );

        clear_dynamic_skills();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn find_skill_command_strips_leading_slash() {
        let commands = vec![skill_command_fixture("commit")];
        assert!(find_skill_command("/commit", &commands).is_some());
    }

    #[test]
    fn empty_skill_directory_still_invalidates_downstream_caches() {
        // Maps to CC `:974` — `skillsLoaded.emit()` sits AFTER the
        // `if (newSkillCount > 0)` block at `:954-971` and is therefore
        // unconditional. Old shape: the generation only advanced when a skill
        // was actually loaded, so discovering a directory that turned out to
        // hold nothing loadable left every generation-keyed cache holding a
        // list built before the directory existed. The assert fails (equal
        // generations); it does not hang.
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let _snapshot = DynamicSkillsTestSnapshot::capture();
        let _sources = SkillDiscoveryStateGuard::capture();
        crate::bootstrap::state::set_allowed_setting_sources(vec!["projectSettings".to_string()]);
        clear_dynamic_skills();

        let root = temp_dir("empty-dynamic-dir");
        let empty_dir = root.join(".claude/skills");
        fs::create_dir_all(&empty_dir).unwrap();

        let before = dynamic_skills_generation();
        add_skill_directories(std::slice::from_ref(&empty_dir));
        assert!(get_dynamic_skills().is_empty());
        assert_ne!(dynamic_skills_generation(), before);

        clear_dynamic_skills();
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_skill_md_is_logged_instead_of_being_indistinguishable_from_absent() {
        // Maps to CC `:436-445` — `if (!isENOENT(e)) logForDebugging(..., warn)`.
        // Old shape: `let Ok(raw) = read_to_string(..) else { continue }` swallowed
        // EACCES exactly like ENOENT, so a mode-000 SKILL.md produced a skill
        // that simply never existed, with nothing anywhere to say why. The
        // observable contract is still "skipped"; what this pins is that the
        // skip is reached through the diagnosed arm and that a chmod-0 skill
        // does not abort the rest of the directory.
        use std::os::unix::fs::PermissionsExt;

        let root = temp_dir("skills-unreadable");
        let skills = root.join("skills");
        write_file(
            &skills.join("locked").join("SKILL.md"),
            "---\ndescription: Locked skill\n---\nBody",
        );
        write_file(
            &skills.join("readable").join("SKILL.md"),
            "---\ndescription: Readable skill\n---\nBody",
        );
        let locked = skills.join("locked").join("SKILL.md");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

        let loaded = load_skills_from_skills_dir(&skills, SkillSource::ProjectSettings);
        // Running as root defeats the mode bits; only assert the skip when the
        // file really is unreadable.
        if fs::read_to_string(&locked).is_err() {
            assert_eq!(loaded.len(), 1);
            assert_eq!(loaded[0].skill.name, "readable");
        }

        fs::set_permissions(&locked, fs::Permissions::from_mode(0o644)).unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn file_identity_is_realpath_so_hard_links_stay_distinct() {
        // Maps to CC `getFileIdentity` (`:118-124`). CC moved off `dev:ino` on
        // purpose and says why at `:107-117`: inode values are unreliable on
        // virtual/container/NFS mounts (0) and lossy on ExFAT
        // (anthropics/claude-code#13893), so unrelated skills collided and
        // vanished. `realpath` still collapses symlinked duplicates, and it
        // deliberately stops collapsing hard links — which is the half that
        // fails on the old `dev:ino` shape: it saw one identity and dropped the
        // second skill. The assert fails; it does not hang.
        let root = temp_dir("skills-realpath");
        let real = root.join("real");
        write_file(
            &real.join("shared").join("SKILL.md"),
            "---\ndescription: Shared skill\n---\nBody",
        );
        let linked = root.join("linked");
        std::os::unix::fs::symlink(&real, &linked).unwrap();

        let mut through_symlink = load_skills_from_skills_dir(&real, SkillSource::ProjectSettings);
        through_symlink.extend(load_skills_from_skills_dir(
            &linked,
            SkillSource::ProjectSettings,
        ));
        assert_eq!(through_symlink.len(), 2);
        assert_eq!(
            deduplicate_skills_by_file_identity(through_symlink).len(),
            1
        );

        let hard_linked = root.join("hard");
        fs::create_dir_all(hard_linked.join("shared")).unwrap();
        fs::hard_link(
            real.join("shared").join("SKILL.md"),
            hard_linked.join("shared").join("SKILL.md"),
        )
        .unwrap();
        let mut through_hard_link =
            load_skills_from_skills_dir(&real, SkillSource::ProjectSettings);
        through_hard_link.extend(load_skills_from_skills_dir(
            &hard_linked,
            SkillSource::ProjectSettings,
        ));
        assert_eq!(
            deduplicate_skills_by_file_identity(through_hard_link).len(),
            2
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn skill_dir_commands_are_memoized_per_cwd_until_skill_caches_clear() {
        // Maps to CC `memoize(...)` at `:638` plus `clearSkillCaches()`
        // (`:806-811`). Old shape: no memo at all — every caller re-walked
        // managed + user + every project dir + `--add-dir` + the whole legacy
        // `commands/` tree. Behaviour-preserving on its own; the assert here is
        // that the cached list is what a second call returns and that
        // `clear_skill_caches()` is what releases it.
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let _state = SkillDiscoveryStateGuard::capture();
        crate::bootstrap::state::set_allowed_setting_sources(vec!["projectSettings".to_string()]);
        clear_skill_caches();

        let root = temp_dir("skills-memo");
        write_file(
            &root.join(".claude/skills/first/SKILL.md"),
            "---\ndescription: First\n---\nBody",
        );
        let first = get_skill_dir_commands(&root);
        assert!(first.iter().any(|skill| skill.name == "first"));

        write_file(
            &root.join(".claude/skills/second/SKILL.md"),
            "---\ndescription: Second\n---\nBody",
        );
        assert!(
            !get_skill_dir_commands(&root)
                .iter()
                .any(|skill| skill.name == "second")
        );

        clear_skill_caches();
        assert!(
            get_skill_dir_commands(&root)
                .iter()
                .any(|skill| skill.name == "second")
        );

        clear_skill_caches();
        let _ = std::fs::remove_dir_all(root);
    }
    #[test]
    fn prompt_propagates_official_invalid_argument_regex_before_shell_execution() {
        let mut command = skill_command_fixture("invalid-argument-name");
        command.argument_names = vec!["[".into()];
        command.markdown_content = "中文 $ARGUMENTS".into();
        assert!(
            command
                .get_prompt_for_command_without_shell(Some("arg"), "session")
                .is_err()
        );
        let error = command
            .get_prompt_for_command(
                Some("arg"),
                "session",
                &crate::tool::ToolUseContext::default(),
            )
            .unwrap_err();
        assert!(error.to_string().starts_with("Invalid regular expression:"));
        // Source returns before constructing named regexes when args is absent.
        assert!(
            command
                .get_prompt_for_command_without_shell(None, "session")
                .unwrap()
                .contains("中文 $ARGUMENTS")
        );
    }
}
