//! Maps to: CC `commands.ts` and `types/command.ts`.
//!
//! This module is the aggregate command owner. CC resolves `getCommands(cwd)`
//! after setup and passes that immutable array through `REPL Props.commands`.
//! Cometix mirrors that ownership: command consumers receive a launch snapshot;
//! they do not read a second UI-only builtin registry.
//!
//! Bundled/workflow command producers remain an explicit follow-up at
//! `load_all_commands`; the plugin command/skill readers are source-owned and
//! already participate in this aggregate. This module wires the complete
//! direct/provider command catalog, official runtime filters, aliases, and
//! safety predicates without pretending that unported `load()/call()`
//! implementations succeed.

pub mod add_dir;
pub mod advisor;
pub mod agents;
pub mod branch;
pub mod btw;
pub mod clear;
pub mod color;
pub mod compact;
pub mod context;
pub mod copy;
pub mod diff;
pub mod doctor;
pub mod effort;
pub mod export;
pub mod fast;
pub mod help;
pub mod hooks;
pub mod ide;
pub mod init;
pub mod keybindings;
pub mod login;
pub mod logout;
pub mod mcp;
pub mod memory;
pub mod model;
pub mod output_style;

pub mod permissions;
pub mod plan;
pub mod plugin;
pub mod provider;
pub mod reload_plugins;
pub mod rename;
pub mod resume;
pub mod review;
pub mod sandbox;
pub mod skills;
pub mod stats;
pub mod statusline;
pub mod tasks;
pub mod terminal_setup;
pub mod theme;
pub mod vim;

use crate::utils::auth::is_claude_ai_subscriber;
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandKind {
    Prompt,
    Local,
    /// Established L1 name: CC `local-jsx` ≙ Rust/iocraft `LocalUi`.
    LocalUi,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandSource {
    Builtin,
    PolicySettings,
    UserSettings,
    ProjectSettings,
    Plugin,
    Bundled,
    Mcp,
}

/// Maps to: CC `types/command.ts::CommandAvailability`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandAvailability {
    ClaudeAi,
    Console,
}

/// Metadata half of CC `types/command.ts::Command`.
///
/// Execution remains owned by each corresponding Rust command module and the
/// typed `processSlashCommand` adapter. `is_enabled` is reevaluated by
/// `get_commands`; `is_hidden` is the bool exposed by CC `CommandBase` and is
/// materialized into the immutable Rust launch snapshot.
pub type CommandCall = fn(
    &Command,
    &str,
    Option<String>,
    &crate::tool::ToolUseContext,
) -> crate::utils::process_user_input::ProcessUserInputBaseResult;

/// Maps to CC `PromptCommand.getPromptForCommand`: return content before the
/// processSlashCommand owner adds metadata, images and permission attachments.
pub type GetPromptForCommand = fn(
    &Command,
    &str,
    &crate::tool::ToolUseContext,
) -> anyhow::Result<Vec<crate::types::message::UserContent>>;

/// Maps to: CC `types/command.ts:33-36#PromptCommand.pluginInfo`.
#[derive(Debug, Clone, PartialEq)]
pub struct PluginInfo {
    pub plugin_manifest: Arc<crate::utils::plugins::schemas::PluginManifest>,
    pub repository: String,
}

#[derive(Debug, Clone)]
pub struct Command {
    pub name: Cow<'static, str>,
    pub aliases: Vec<Cow<'static, str>>,
    pub description: Cow<'static, str>,
    /// Maps to CC commands that declare `get description()` instead of a
    /// literal: the getter is re-read per access, so the resolver stays live
    /// past the memoized `load_all_commands` catalog.
    description_resolver: Option<fn() -> String>,
    pub has_user_specified_description: bool,
    pub argument_hint: Option<Cow<'static, str>>,
    /// Maps to CC commands that declare `get argumentHint()` instead of a
    /// literal. The resolver is evaluated when `get_commands` projects the
    /// cached catalog, so runtime model changes remain visible.
    argument_hint_resolver: Option<fn() -> String>,
    pub arg_names: Vec<Cow<'static, str>>,
    pub when_to_use: Option<Cow<'static, str>>,
    pub version: Option<Cow<'static, str>>,
    pub disable_model_invocation: bool,
    pub user_invocable: bool,
    pub loaded_from: Option<crate::skills::load_skills_dir::SkillLoadedFrom>,
    pub user_facing_name: Option<Cow<'static, str>>,
    pub kind: CommandKind,
    pub source: CommandSource,
    pub availability: Vec<CommandAvailability>,
    pub is_enabled: Option<fn() -> bool>,
    /// Maps to CC prompt-command `getPromptForCommand`.
    pub get_prompt_for_command: Option<GetPromptForCommand>,
    /// Maps to CC local/local-jsx module `call`.
    pub call: Option<CommandCall>,
    pub is_hidden: bool,
    hidden_resolver: Option<fn() -> bool>,
    pub immediate: bool,
    pub supports_non_interactive: bool,
    pub progress_message: Option<Cow<'static, str>>,
    pub content_length: Option<usize>,
    pub disable_non_interactive: bool,
    /// Maps to CC `types/command.ts#PromptCommand.allowedTools`; absent
    /// declarations use the same empty list as source consumers' `?? []`.
    pub allowed_tools: Vec<String>,
    /// Existing CC-shaped filesystem skill object captured by its prompt
    /// implementation. Builtins and local commands leave this unset.
    pub prompt_command: Option<Arc<crate::skills::load_skills_dir::SkillCommand>>,
    /// Maps to: CC `PromptCommand.pluginInfo`; shares the captured manifest.
    pub plugin_info: Option<PluginInfo>,
    /// L1 typed executable pointer: additional captures of the plugin owner's
    /// getPromptForCommand closure. Domain logic stays in loadPluginCommands.
    pub plugin_context:
        Option<Arc<crate::utils::plugins::load_plugin_commands::PluginCommandContext>>,
}

impl PartialEq for Command {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && self.aliases == other.aliases
            && self.description == other.description
            && self.description_resolver.is_some() == other.description_resolver.is_some()
            && self.has_user_specified_description == other.has_user_specified_description
            && self.argument_hint == other.argument_hint
            && self.argument_hint_resolver.is_some() == other.argument_hint_resolver.is_some()
            && self.arg_names == other.arg_names
            && self.when_to_use == other.when_to_use
            && self.version == other.version
            && self.disable_model_invocation == other.disable_model_invocation
            && self.user_invocable == other.user_invocable
            && self.loaded_from == other.loaded_from
            && self.user_facing_name == other.user_facing_name
            && self.kind == other.kind
            && self.source == other.source
            && self.availability == other.availability
            && self.is_enabled.is_some() == other.is_enabled.is_some()
            && self.get_prompt_for_command.is_some() == other.get_prompt_for_command.is_some()
            && self.call.is_some() == other.call.is_some()
            && self.is_hidden == other.is_hidden
            && self.hidden_resolver.is_some() == other.hidden_resolver.is_some()
            && self.immediate == other.immediate
            && self.supports_non_interactive == other.supports_non_interactive
            && self.progress_message == other.progress_message
            && self.content_length == other.content_length
            && self.disable_non_interactive == other.disable_non_interactive
            && self.allowed_tools == other.allowed_tools
            && self.prompt_command == other.prompt_command
            && self.plugin_info == other.plugin_info
            && self.plugin_context == other.plugin_context
    }
}

impl Eq for Command {}

impl Command {
    fn new(kind: CommandKind, name: &'static str, description: &'static str) -> Self {
        Self {
            name: Cow::Borrowed(name),
            aliases: Vec::new(),
            description: Cow::Borrowed(description),
            description_resolver: None,
            has_user_specified_description: false,
            argument_hint: None,
            argument_hint_resolver: None,
            arg_names: Vec::new(),
            when_to_use: None,
            version: None,
            disable_model_invocation: false,
            user_invocable: true,
            loaded_from: None,
            user_facing_name: None,
            kind,
            source: CommandSource::Builtin,
            availability: Vec::new(),
            is_enabled: None,
            get_prompt_for_command: None,
            call: None,
            is_hidden: false,
            hidden_resolver: None,
            immediate: false,
            supports_non_interactive: false,
            progress_message: None,
            content_length: None,
            disable_non_interactive: false,
            allowed_tools: Vec::new(),
            prompt_command: None,
            plugin_info: None,
            plugin_context: None,
        }
    }

    fn prompt(name: &'static str, description: &'static str) -> Self {
        Self::new(CommandKind::Prompt, name, description)
    }

    fn local(name: &'static str, description: &'static str) -> Self {
        Self::new(CommandKind::Local, name, description)
    }

    fn local_ui(name: &'static str, description: &'static str) -> Self {
        Self::new(CommandKind::LocalUi, name, description)
    }

    fn aliases(mut self, aliases: &'static [&'static str]) -> Self {
        self.aliases = aliases.iter().copied().map(Cow::Borrowed).collect();
        self
    }

    fn description_when(mut self, resolver: fn() -> String) -> Self {
        self.description_resolver = Some(resolver);
        self
    }

    fn argument_hint(mut self, argument_hint: impl Into<Cow<'static, str>>) -> Self {
        self.argument_hint = Some(argument_hint.into());
        self
    }

    fn argument_hint_when(mut self, resolver: fn() -> String) -> Self {
        self.argument_hint_resolver = Some(resolver);
        self
    }

    fn availability(mut self, availability: &'static [CommandAvailability]) -> Self {
        self.availability = availability.to_vec();
        self
    }

    fn enabled_when(mut self, predicate: fn() -> bool) -> Self {
        self.is_enabled = Some(predicate);
        self
    }

    fn executable(mut self, call: CommandCall) -> Self {
        self.call = Some(call);
        self
    }

    fn prompt_executable(mut self, get_prompt_for_command: GetPromptForCommand) -> Self {
        self.get_prompt_for_command = Some(get_prompt_for_command);
        self
    }

    fn hidden_when(mut self, predicate: fn() -> bool) -> Self {
        self.hidden_resolver = Some(predicate);
        self
    }

    fn hidden(mut self) -> Self {
        self.is_hidden = true;
        self
    }

    fn immediate(mut self) -> Self {
        self.immediate = true;
        self
    }

    fn immediate_when(mut self, predicate: fn() -> bool) -> Self {
        self.immediate = predicate();
        self
    }

    fn supports_non_interactive(mut self) -> Self {
        self.supports_non_interactive = true;
        self
    }

    fn prompt_metadata(mut self, progress_message: &'static str, content_length: usize) -> Self {
        self.progress_message = Some(Cow::Borrowed(progress_message));
        self.content_length = Some(content_length);
        self
    }

    fn disable_non_interactive(mut self) -> Self {
        self.disable_non_interactive = true;
        self
    }

    pub(crate) fn from_mcp_prompt(
        prompt: crate::services::mcp::client::McpPromptCommandSnapshot,
    ) -> Self {
        let argument_hint = (!prompt.arg_names.is_empty())
            .then(|| format!("[{}]", prompt.arg_names.join(" ")))
            .map(Cow::Owned);
        Self {
            name: Cow::Owned(prompt.name),
            aliases: Vec::new(),
            description: Cow::Owned(prompt.description),
            description_resolver: None,
            has_user_specified_description: prompt.has_user_specified_description,
            argument_hint,
            argument_hint_resolver: None,
            arg_names: prompt.arg_names.into_iter().map(Cow::Owned).collect(),
            when_to_use: None,
            version: None,
            disable_model_invocation: false,
            user_invocable: true,
            loaded_from: Some(crate::skills::load_skills_dir::SkillLoadedFrom::Mcp),
            user_facing_name: Some(Cow::Owned(prompt.user_facing_name)),
            kind: CommandKind::Prompt,
            source: CommandSource::Mcp,
            availability: Vec::new(),
            is_enabled: None,
            // MCP prompt retrieval is async and remains owned by REPL's
            // `get_mcp_prompt_for_command` action before generic slash dispatch.
            get_prompt_for_command: None,
            call: None,
            is_hidden: false,
            hidden_resolver: None,
            immediate: false,
            supports_non_interactive: false,
            progress_message: Some(Cow::Borrowed("running")),
            content_length: Some(0),
            disable_non_interactive: false,
            allowed_tools: Vec::new(),
            prompt_command: None,
            plugin_info: None,
            plugin_context: None,
        }
    }

    /// Maps to: CC `createSkillCommand(skill)` (`loadSkillsDir.ts:300-400`) —
    /// the `Command` projection every filesystem/plugin skill enters the
    /// registry through. `pub(crate)` because callers outside this module need
    /// the same projection to build the `Command[]` slices CC hands around
    /// (e.g. `runAgent.ts:580`'s `allSkills`).
    pub(crate) fn from_skill(skill: crate::skills::load_skills_dir::SkillCommand) -> Self {
        use crate::skills::load_skills_dir::SkillSource;

        let source = match skill.source {
            SkillSource::PolicySettings => CommandSource::PolicySettings,
            SkillSource::UserSettings => CommandSource::UserSettings,
            SkillSource::ProjectSettings => CommandSource::ProjectSettings,
        };
        let skill = Arc::new(skill);
        Self {
            name: Cow::Owned(skill.name.clone()),
            aliases: Vec::new(),
            description: Cow::Owned(skill.description.clone()),
            description_resolver: None,
            has_user_specified_description: skill.has_user_specified_description,
            argument_hint: skill.argument_hint.clone().map(Cow::Owned),
            argument_hint_resolver: None,
            arg_names: skill
                .argument_names
                .iter()
                .cloned()
                .map(Cow::Owned)
                .collect(),
            when_to_use: skill.when_to_use.clone().map(Cow::Owned),
            version: skill.version.clone().map(Cow::Owned),
            disable_model_invocation: skill.disable_model_invocation,
            user_invocable: skill.user_invocable,
            loaded_from: Some(skill.loaded_from),
            user_facing_name: skill.display_name.clone().map(Cow::Owned),
            kind: CommandKind::Prompt,
            source,
            availability: Vec::new(),
            is_enabled: None,
            get_prompt_for_command: Some(
                crate::utils::process_user_input::process_slash_command::call_skill_prompt,
            ),
            call: None,
            is_hidden: !skill.user_invocable,
            hidden_resolver: None,
            immediate: false,
            supports_non_interactive: false,
            // Maps to: CC `createSkillCommand(...).progressMessage` — the
            // unconditional `'running'` at `loadSkillsDir.ts:336`. Only the
            // plugin projection below overrides it (`'loading'` for skills).
            progress_message: Some(Cow::Borrowed("running")),
            content_length: Some(skill.content_length),
            disable_non_interactive: false,
            allowed_tools: skill.allowed_tools.clone(),
            prompt_command: Some(skill),
            plugin_info: None,
            plugin_context: None,
        }
    }
}

fn is_non_interactive() -> bool {
    crate::bootstrap::state::get_is_non_interactive_session()
}

fn is_interactive() -> bool {
    !is_non_interactive()
}

fn is_internal_build() -> bool {
    crate::utils::build_profile::has_internal_capability(
        crate::utils::build_profile::InternalCapability::Commands,
    )
}

fn is_internal_non_demo() -> bool {
    is_internal_build()
        && !crate::utils::env_utils::is_env_truthy(std::env::var("IS_DEMO").ok().as_deref())
}

fn compact_enabled() -> bool {
    !crate::utils::env_utils::is_env_truthy(std::env::var("DISABLE_COMPACT").ok().as_deref())
}

fn doctor_enabled() -> bool {
    !crate::utils::env_utils::is_env_truthy(std::env::var("DISABLE_DOCTOR_COMMAND").ok().as_deref())
}

fn install_github_app_enabled() -> bool {
    !crate::utils::env_utils::is_env_truthy(
        std::env::var("DISABLE_INSTALL_GITHUB_APP_COMMAND")
            .ok()
            .as_deref(),
    )
}

fn login_enabled() -> bool {
    !crate::utils::env_utils::is_env_truthy(std::env::var("DISABLE_LOGIN_COMMAND").ok().as_deref())
}

fn logout_enabled() -> bool {
    !crate::utils::env_utils::is_env_truthy(std::env::var("DISABLE_LOGOUT_COMMAND").ok().as_deref())
}

fn is_using_3p_services() -> bool {
    [
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_VERTEX",
        "CLAUDE_CODE_USE_FOUNDRY",
    ]
    .into_iter()
    .any(|key| crate::utils::env_utils::is_env_truthy(std::env::var(key).ok().as_deref()))
}

fn is_console_user() -> bool {
    !is_claude_ai_subscriber()
        && !is_using_3p_services()
        && crate::utils::model::providers::is_first_party_anthropic_base_url()
}

fn chrome_enabled() -> bool {
    is_interactive()
}

fn desktop_supported() -> bool {
    cfg!(all(target_os = "macos", target_arch = "aarch64"))
        || cfg!(all(target_os = "windows", target_arch = "x86_64"))
}

fn fast_enabled() -> bool {
    crate::utils::fast_mode::is_fast_mode_enabled()
}

fn fast_hidden() -> bool {
    !fast_enabled()
}

fn cost_hidden() -> bool {
    !is_internal_build() && is_claude_ai_subscriber()
}

fn terminal_setup_hidden() -> bool {
    crate::commands::terminal_setup::terminal_setup_command_is_hidden_for(
        crate::utils::env::get().terminal.as_deref(),
    )
}

fn session_enabled() -> bool {
    crate::utils::env_utils::is_env_truthy(std::env::var("CLAUDE_CODE_REMOTE").ok().as_deref())
}

fn feedback_enabled() -> bool {
    !is_using_3p_services()
        && !crate::utils::env_utils::is_env_truthy(
            std::env::var("DISABLE_FEEDBACK_COMMAND").ok().as_deref(),
        )
        && !crate::utils::env_utils::is_env_truthy(
            std::env::var("DISABLE_BUG_COMMAND").ok().as_deref(),
        )
        && !is_internal_build()
}

fn remote_env_enabled() -> bool {
    is_claude_ai_subscriber()
}

fn rate_limit_options_enabled() -> bool {
    is_claude_ai_subscriber()
}

fn privacy_settings_enabled() -> bool {
    if !is_claude_ai_subscriber() {
        return false;
    }
    matches!(
        crate::utils::auth::get_subscription_type().as_deref(),
        Some("pro" | "max")
    )
}

fn upgrade_enabled() -> bool {
    if crate::utils::env_utils::is_env_truthy(
        std::env::var("DISABLE_UPGRADE_COMMAND").ok().as_deref(),
    ) {
        return false;
    }
    crate::utils::auth::get_subscription_type().as_deref() != Some("enterprise")
}

fn passes_hidden() -> bool {
    let config = crate::utils::config::load_global_config();
    let now_ms = chrono::Utc::now().timestamp_millis();
    let cached = crate::services::api::referral::check_cached_passes_eligibility(&config, now_ms);
    !cached.eligible || !cached.has_cache
}

fn ultrareview_enabled() -> bool {
    false
}

fn extra_usage_enabled() -> bool {
    false
}

fn thinkback_enabled() -> bool {
    false
}

/// Maps to: CC `commands.ts` `COMMANDS()` direct/provider catalog.
///
/// Order intentionally matches the source. Duplicate names (`context`,
/// `extra-usage`) are alternate interactive/non-interactive implementations;
/// `get_commands` filters the inactive one.
fn commands() -> Vec<Command> {
    use crate::utils::process_user_input::process_slash_command as execution;
    use CommandAvailability::{ClaudeAi, Console};

    let mut result = vec![
        Command::local_ui("add-dir", "Add a new working directory").argument_hint("<path>").executable(add_dir::dispatch),
        advisor::command(),
        Command::local_ui(agents::NAME, agents::DESCRIPTION)
            .executable(execution::call_agents),
        branch::command(),
        Command::local_ui(btw::NAME, btw::DESCRIPTION)
            .argument_hint("<question>")
            .immediate()
            .executable(btw::call),
        Command::local_ui("chrome", "Claude in Chrome (Beta) settings")
            .availability(&[ClaudeAi])
            .enabled_when(chrome_enabled),
        Command::local("clear", "Clear conversation history and free up context")
            .aliases(&["reset", "new"])
            .executable(execution::call_clear),
        color::command(),
        Command::local(compact::NAME, compact::DESCRIPTION)
        .argument_hint("<optional custom summarization instructions>")
        .enabled_when(compact_enabled)
        .supports_non_interactive()
        .executable(execution::call_compact),
        Command::local_ui("config", "Open config panel")
            .aliases(&["settings"])
            .executable(execution::call_config),
        Command::local_ui(
            "copy",
            "Copy Claude's last response to clipboard (or /copy N for the Nth-latest)",
        )
        .executable(execution::call_copy),
        Command::local_ui("desktop", "Continue the current session in Claude Desktop")
            .aliases(&["app"])
            .availability(&[ClaudeAi])
            .enabled_when(desktop_supported)
            .hidden_when(|| !desktop_supported()),
        Command::local_ui(context::NAME, context::INTERACTIVE_DESCRIPTION)
            .enabled_when(is_interactive)
            .executable(execution::call_context),
        Command::local(context::NAME, context::NON_INTERACTIVE_DESCRIPTION)
            .enabled_when(is_non_interactive)
            .hidden_when(is_interactive)
            .supports_non_interactive()
            .executable(execution::call_context_noninteractive),
        Command::local("cost", "Show the total cost and duration of the current session")
            .hidden_when(cost_hidden)
            .supports_non_interactive()
            .executable(execution::call_cost),
        Command::local_ui(diff::NAME, diff::DESCRIPTION).executable(execution::call_diff),
        Command::local_ui(
            "doctor",
            "Diagnose and verify your Claude Code installation and settings",
        )
        .enabled_when(doctor_enabled)
        .executable(execution::call_doctor),
        Command::local_ui(effort::NAME, effort::DESCRIPTION)
            .argument_hint_when(effort::current_effort_argument_hint)
            .immediate_when(
                crate::utils::immediate_command::should_inference_config_command_be_immediate,
            )
            .executable(execution::call_effort),
        Command::local_ui("exit", "Exit the REPL")
            .aliases(&["quit"])
            .immediate()
            .executable(execution::call_exit),
        Command::local_ui("fast", "Toggle fast mode (Opus 4.6 only)")
            .argument_hint("[on|off]")
            .availability(&[ClaudeAi, Console])
            .enabled_when(fast_enabled)
            .hidden_when(fast_hidden)
            .immediate_when(
                crate::utils::immediate_command::should_inference_config_command_be_immediate,
            )
            .executable(execution::call_fast),
        Command::local("files", "List all files currently in context")
            .enabled_when(is_internal_build)
            .supports_non_interactive(),
        Command::local("heapdump", "Dump the JS heap to ~/Desktop")
            .hidden()
            .supports_non_interactive(),
        help::command(),
        ide::command(),
        Command::prompt(init::NAME, init::description())
            .description_when(|| init::description().to_string())
            .prompt_metadata(init::PROGRESS_MESSAGE, init::CONTENT_LENGTH)
            .prompt_executable(execution::call_init),
        Command::local(keybindings::NAME, keybindings::DESCRIPTION)
            .enabled_when(
                crate::keybindings::load_user_bindings::is_keybinding_customization_enabled,
            )
            .executable(keybindings::call),
        Command::local_ui(
            "install-github-app",
            "Set up Claude GitHub Actions for a repository",
        )
        .availability(&[ClaudeAi, Console])
        .enabled_when(install_github_app_enabled),
        Command::local("install-slack-app", "Install the Claude Slack app")
            .availability(&[ClaudeAi]),
        Command::local_ui("mcp", "Manage MCP servers")
            .argument_hint("[enable|disable [server-name]]")
            .immediate()
            .executable(execution::call_mcp),
        Command::local_ui("memory", "Edit Claude memory files")
            .executable(execution::call_memory),
        Command::local_ui("mobile", "Show QR code to download the Claude mobile app")
            .aliases(&["ios", "android"]),
        Command::local_ui("model", "Set the AI model for Claude Code")
            .description_when(model::description)
            .argument_hint("[model]")
            .immediate_when(
                crate::utils::immediate_command::should_inference_config_command_be_immediate,
            )
            .executable(execution::call_model),
        output_style::command(),
        Command::local_ui(
            "remote-env",
            "Configure the default remote environment for teleport sessions",
        )
        .enabled_when(remote_env_enabled)
        .hidden_when(|| !remote_env_enabled()),
        plugin::command(),
        provider::command(),
        Command::prompt("pr-comments", "Get comments from a GitHub pull request")
            .prompt_metadata("fetching PR comments", 0),
        Command::local("release-notes", "View release notes").supports_non_interactive(),
        reload_plugins::command(),
        Command::local_ui("rename", "Rename the current conversation")
            .argument_hint("[name]")
            .immediate()
            .executable(execution::call_rename),
        Command::local_ui("resume", "Resume a previous conversation")
            .aliases(&["continue"])
            .argument_hint("[conversation id or search term]")
            .executable(execution::call_resume),
        Command::local_ui("session", "Show remote session URL and QR code")
            .aliases(&["remote"])
            .enabled_when(session_enabled)
            .hidden_when(|| !session_enabled()),
        Command::local_ui("skills", "List available skills")
            .executable(execution::call_skills),
        Command::local_ui(stats::NAME, stats::DESCRIPTION).executable(stats::call),
        Command::local_ui(
            "status",
            "Show Claude Code status including version, model, account, API connectivity, and tool statuses",
        )
        .immediate()
        .executable(execution::call_status),
        statusline::command(),
        Command::local("stickers", "Order Claude Code stickers"),
        Command::local_ui("tag", "Toggle a searchable tag on the current session")
            .argument_hint("<tag-name>")
            .enabled_when(is_internal_build),
        Command::local_ui("theme", "Change the theme")
            .executable(execution::call_theme),
        Command::local_ui("feedback", "Submit feedback about Claude Code")
            .aliases(&["bug"])
            .argument_hint("[report]")
            .enabled_when(feedback_enabled),
        review::command(),
        Command::local_ui(
            "ultrareview",
            "~10–20 min · Finds and verifies bugs in your branch. Runs in Claude Code on the web.",
        )
        .enabled_when(ultrareview_enabled),
        Command::local("rewind", "Restore the code and/or conversation to a previous point")
            .aliases(&["checkpoint"])
            .argument_hint("")
            .executable(execution::call_rewind),
        Command::prompt(
            "security-review",
            "Complete a security review of the pending changes on the current branch",
        )
        .prompt_metadata("analyzing code changes for security risks", 0),
        Command::local_ui(
            "terminal-setup",
            crate::commands::terminal_setup::terminal_setup_command_description_for(
                crate::utils::env::get().terminal.as_deref(),
            ),
        )
        .hidden_when(terminal_setup_hidden)
        .executable(execution::call_terminal_setup),
        Command::local_ui("upgrade", "Upgrade to Max for higher rate limits and more Opus")
            .availability(&[ClaudeAi])
            .enabled_when(upgrade_enabled),
        Command::local_ui(
            "extra-usage",
            "Configure extra usage to keep working when limits are hit",
        )
        .enabled_when(|| extra_usage_enabled() && is_interactive()),
        Command::local(
            "extra-usage",
            "Configure extra usage to keep working when limits are hit",
        )
        .enabled_when(|| extra_usage_enabled() && is_non_interactive())
        .hidden_when(is_interactive)
        .supports_non_interactive(),
        Command::local_ui("rate-limit-options", "Show options when rate limit is reached")
            .enabled_when(rate_limit_options_enabled)
            .hidden(),
        Command::local_ui("usage", "Show plan usage limits")
            .availability(&[ClaudeAi])
            .executable(execution::call_usage),
        Command::prompt(
            "insights",
            "Generate a report analyzing your Claude Code sessions",
        )
        .prompt_metadata("analyzing your sessions", 0),
        Command::local("vim", "Toggle between Vim and Normal editing modes")
            .executable(execution::call_vim),
        Command::local_ui("think-back", "Your 2025 Claude Code Year in Review")
            .enabled_when(thinkback_enabled),
        Command::local("thinkback-play", "Play the thinkback animation")
            .enabled_when(thinkback_enabled)
            .hidden(),
        Command::local_ui("permissions", "Manage allow & deny tool permission rules")
            .aliases(&["allowed-tools"])
            .executable(execution::call_permissions),
        Command::local_ui(plan::NAME, plan::DESCRIPTION)
            .argument_hint(plan::ARGUMENT_HINT)
            .executable(execution::call_plan),
        Command::local_ui("privacy-settings", "View and update your privacy settings")
            .enabled_when(privacy_settings_enabled),
        Command::local_ui(hooks::NAME, hooks::DESCRIPTION)
            .immediate()
            .executable(execution::call_hooks),
        Command::local_ui(export::NAME, export::DESCRIPTION)
            .argument_hint(export::ARGUMENT_HINT)
            .executable(execution::call_export),
        Command::local_ui("sandbox", "◯ sandbox disabled (⏎ to configure)")
            .description_when(sandbox::description)
            .argument_hint("exclude \"command pattern\"")
            .hidden_when(sandbox::is_hidden)
            .immediate()
            .executable(execution::call_sandbox),
    ];

    // Maps to CC `!isUsing3PServices() ? [logout, login()] : []`.
    if !is_using_3p_services() {
        result.push(logout::command().enabled_when(logout_enabled));
        result.push(login::command().enabled_when(login_enabled));
    }

    result.push(
        Command::local_ui("passes", "Share a free week of Claude Code with friends")
            .hidden_when(passes_hidden),
    );
    result.push(
        Command::local_ui(tasks::NAME, tasks::DESCRIPTION)
            .aliases(tasks::ALIASES)
            .executable(tasks::call),
    );

    // Maps to CC internal-distribution + `!IS_DEMO` command registry gate.
    // `/version` is the only internal command with a live Rust implementation;
    // no-source internal descriptors are not advertised as executable.
    if is_internal_non_demo() {
        result.push(
            Command::local(
                "version",
                "Print the version this session is running (not what autoupdate downloaded)",
            )
            .enabled_when(is_internal_build)
            .supports_non_interactive()
            .executable(execution::call_version),
        );
    }

    result
}

/// Maps to: CC `commands.ts::builtInCommandNames`.
static BUILT_IN_COMMAND_NAMES: LazyLock<HashSet<String>> = LazyLock::new(|| {
    commands()
        .into_iter()
        .flat_map(|command| {
            std::iter::once(command.name.into_owned())
                .chain(command.aliases.into_iter().map(Cow::into_owned))
        })
        .collect()
});

pub fn built_in_command_names() -> &'static HashSet<String> {
    &BUILT_IN_COMMAND_NAMES
}

/// Maps to: CC `commands.ts:505` — the set `getCommands` builds locally to find
/// the dynamic-skill insertion point. Deliberately NOT the exported
/// `builtInCommandNames()` (`:348-351`): that one flat-maps aliases in, this one
/// is `COMMANDS().map(c => c.name)`.
static BUILT_IN_COMMAND_NAMES_WITHOUT_ALIASES: LazyLock<HashSet<String>> = LazyLock::new(|| {
    commands()
        .into_iter()
        .map(|command| command.name.into_owned())
        .collect()
});

pub fn get_command_name(command: &Command) -> &str {
    command
        .user_facing_name
        .as_deref()
        .unwrap_or(command.name.as_ref())
}

pub fn is_command_enabled(command: &Command) -> bool {
    command.is_enabled.is_none_or(|predicate| predicate())
}

pub fn is_command_hidden(command: &Command) -> bool {
    command.is_hidden || command.hidden_resolver.is_some_and(|predicate| predicate())
}

/// Reads CC's `description` property, evaluating the getter when the command
/// declares one.
pub fn command_description(command: &Command) -> Cow<'_, str> {
    match command.description_resolver {
        Some(resolver) => Cow::Owned(resolver()),
        None => Cow::Borrowed(command.description.as_ref()),
    }
}

/// Maps to: CC `commands.ts::meetsAvailabilityRequirement`.
pub fn meets_availability_requirement(command: &Command) -> bool {
    meets_availability_requirement_with(command, is_claude_ai_subscriber(), is_console_user())
}

fn meets_availability_requirement_with(command: &Command, claude_ai: bool, console: bool) -> bool {
    command.availability.is_empty()
        || command
            .availability
            .iter()
            .any(|requirement| match requirement {
                CommandAvailability::ClaudeAi => claude_ai,
                CommandAvailability::Console => console,
            })
}

/// Maps to CC's cwd-keyed memoization around `loadAllCommands(cwd)`.
static LOAD_ALL_COMMANDS_CACHE: LazyLock<Mutex<HashMap<(PathBuf, u64), Arc<Vec<Command>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Maps to: CC `commands.ts::loadAllCommands(cwd)`.
///
/// The live filesystem skill/legacy-command source is inserted before
/// builtins, matching the official strata. Plugin commands and skills use
/// their independent source-owned readers and memoization caches; manifest
/// `commands`/`skills` custom paths are included by those readers before this
/// aggregate is cached.
fn load_all_commands(cwd: &Path) -> Vec<Command> {
    // Rust adaptation of the `skillsLoaded` SUBSCRIPTION, not of the merge: CC
    // wires `onDynamicSkillsLoaded(() => clearCommandMemoizationCaches())` in
    // `utils/skills/skillChangeDetector.ts:92-100`, which is unported. Keying
    // the memo on the generation counter and discarding stale generations gives
    // the same invalidation without the watcher module.
    let generation = crate::skills::load_skills_dir::dynamic_skills_generation();
    let cache_key = (
        cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf()),
        generation,
    );
    {
        let mut cache = LOAD_ALL_COMMANDS_CACHE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.retain(|(_, cached_generation), _| *cached_generation == generation);
        if let Some(cached) = cache.get(&cache_key).cloned() {
            return cached.as_ref().clone();
        }
    }

    let mut loaded = crate::skills::load_skills_dir::get_skill_dir_commands(cwd)
        .into_iter()
        .map(Command::from_skill)
        .collect::<Vec<_>>();
    let plugin_results = crate::utils::process_runtime::block_on_from_sync(async {
        futures::join!(
            crate::utils::plugins::load_plugin_commands::get_plugin_commands(),
            crate::utils::plugins::load_plugin_commands::get_plugin_skills()
        )
    });
    // A4: the existing synchronous Vec registry cannot propagate the async
    // getCommands rejection. Preserve its explicit error/log seam; the canonical
    // plugin readers and refresh caller retain their fallible source results.
    if let Some((plugin_commands, plugin_skills)) = plugin_results {
        for result in [plugin_commands, plugin_skills] {
            match result {
                Ok(commands) => loaded.extend(commands.iter().cloned()),
                Err(error) => crate::utils::log::log_error(crate::utils::log::LogError::new(
                    error.to_string(),
                )),
            }
        }
    } else {
        crate::utils::log::log_error(crate::utils::log::LogError::new(
            "Unable to initialize plugin loading runtime",
        ));
    }
    loaded.extend(commands());

    LOAD_ALL_COMMANDS_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(cache_key, Arc::new(loaded.clone()));
    loaded
}

/// Maps to CC `clearCommandMemoizationCaches()`.
pub fn clear_command_memoization_caches() {
    LOAD_ALL_COMMANDS_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
}

/// Maps to CC `clearCommandsCache()` (`commands.ts:534-539`). Plugin-specific
/// command and skill caches are independently cleared at the source boundary.
///
/// The `clear_skill_caches()` call is the reason the two entry points are
/// distinct: `clear_command_memoization_caches()` alone must NOT rediscover
/// skills from disk (CC says so at `:519-522`), so a caller that wants a fresh
/// disk walk has to come through here.
pub fn clear_commands_cache() {
    clear_command_memoization_caches();
    crate::utils::plugins::load_plugin_commands::clear_plugin_command_cache();
    crate::utils::plugins::load_plugin_commands::clear_plugin_skills_cache();
    crate::skills::load_skills_dir::clear_skill_caches();
}

/// Maps to: CC `commands.ts::getCommands(cwd)`.
///
/// Availability and enablement are reevaluated for each launch snapshot, just
/// as CC does after the memoized expensive source load.
pub fn get_commands(cwd: &Path) -> Vec<Command> {
    let claude_ai = is_claude_ai_subscriber();
    let console = !claude_ai
        && !is_using_3p_services()
        && crate::utils::model::providers::is_first_party_anthropic_base_url();
    let resolve = |mut command: Command| {
        if let Some(resolver) = command.description_resolver.take() {
            command.description = Cow::Owned(resolver());
        }
        if let Some(resolver) = command.argument_hint_resolver.take() {
            command.argument_hint = Some(Cow::Owned(resolver()));
        }
        command.is_hidden =
            command.is_hidden || command.hidden_resolver.is_some_and(|predicate| predicate());
        command.hidden_resolver = None;
        command
    };
    let base_commands = load_all_commands(cwd)
        .into_iter()
        .filter(|command| meets_availability_requirement_with(command, claude_ai, console))
        .filter(is_command_enabled)
        .map(resolve)
        .collect::<Vec<_>>();

    // Maps to: CC `commands.ts:479-516`. Dynamic skills are merged HERE, not
    // inside `getSkillDirCommands`: the dedup set is every base command name
    // (builtins and plugin commands included), and the survivors land
    // immediately before the first builtin — i.e. after plugin skills, not
    // before them.
    let dynamic_skills = crate::skills::load_skills_dir::get_dynamic_skills();
    if dynamic_skills.is_empty() {
        return base_commands;
    }
    let base_command_names = base_commands
        .iter()
        .map(|command| command.name.to_string())
        .collect::<HashSet<_>>();
    let unique_dynamic_skills = dynamic_skills
        .into_iter()
        .map(Command::from_skill)
        .filter(|command| !base_command_names.contains(command.name.as_ref()))
        .filter(|command| meets_availability_requirement_with(command, claude_ai, console))
        .filter(is_command_enabled)
        .map(resolve)
        .collect::<Vec<_>>();
    if unique_dynamic_skills.is_empty() {
        return base_commands;
    }

    let mut base_commands = base_commands;
    let Some(insert_index) = base_commands
        .iter()
        .position(|command| BUILT_IN_COMMAND_NAMES_WITHOUT_ALIASES.contains(command.name.as_ref()))
    else {
        base_commands.extend(unique_dynamic_skills);
        return base_commands;
    };
    let tail = base_commands.split_off(insert_index);
    base_commands.extend(unique_dynamic_skills);
    base_commands.extend(tail);
    base_commands
}

/// Maps to: CC `commands.ts:563-583` `getSkillToolCommands(cwd)`.
pub fn get_skill_tool_commands(cwd: &Path) -> Vec<Command> {
    use crate::skills::load_skills_dir::SkillLoadedFrom;

    get_commands(cwd)
        .into_iter()
        .filter(|command| command.kind == CommandKind::Prompt)
        .filter(|command| !command.disable_model_invocation)
        .filter(|command| command.source != CommandSource::Builtin)
        .filter(|command| {
            matches!(
                command.loaded_from,
                Some(
                    SkillLoadedFrom::Bundled
                        | SkillLoadedFrom::Skills
                        | SkillLoadedFrom::CommandsDeprecated
                )
            ) || command.has_user_specified_description
                || command
                    .when_to_use
                    .as_ref()
                    .is_some_and(|value| !value.is_empty())
        })
        .collect()
}

/// Maps to CC `commands.ts:getMcpSkillCommands`.
///
/// MCP prompt commands live in `AppState.mcp.commands`, outside the local
/// command catalog. Keep this source-level predicate here so attachment and
/// search consumers do not each grow a subtly different MCP-skill filter.
pub fn get_mcp_skill_commands(commands: &[Command]) -> Vec<Command> {
    use crate::skills::load_skills_dir::SkillLoadedFrom;

    if !crate::utils::feature_flags::feature_enabled(
        crate::utils::feature_flags::FeatureFlag::McpSkills,
    ) {
        return Vec::new();
    }

    commands
        .iter()
        .filter(|command| {
            command.kind == CommandKind::Prompt
                && command.loaded_from == Some(SkillLoadedFrom::Mcp)
                && !command.disable_model_invocation
        })
        .cloned()
        .collect()
}

/// Maps to CC `getSlashCommandToolSkills(cwd)`.
pub fn get_slash_command_tool_skills(cwd: &Path) -> Vec<Command> {
    use crate::skills::load_skills_dir::SkillLoadedFrom;

    get_commands(cwd)
        .into_iter()
        .filter(|command| command.kind == CommandKind::Prompt)
        .filter(|command| command.source != CommandSource::Builtin)
        .filter(|command| {
            command.has_user_specified_description
                || command
                    .when_to_use
                    .as_ref()
                    .is_some_and(|value| !value.is_empty())
        })
        .filter(|command| {
            matches!(
                command.loaded_from,
                Some(SkillLoadedFrom::Skills | SkillLoadedFrom::Plugin | SkillLoadedFrom::Bundled)
            ) || command.disable_model_invocation
        })
        .collect()
}

pub fn find_command<'a>(name: &str, commands: &'a [Command]) -> Option<&'a Command> {
    commands.iter().find(|command| {
        command.name == name
            || get_command_name(command) == name
            || command.aliases.iter().any(|alias| alias == name)
    })
}

pub fn has_command(name: &str, commands: &[Command]) -> bool {
    find_command(name, commands).is_some()
}

pub fn get_command<'a>(name: &str, commands: &'a [Command]) -> &'a Command {
    find_command(name, commands).unwrap_or_else(|| {
        let mut available = commands
            .iter()
            .map(|command| {
                if command.aliases.is_empty() {
                    command.name.to_string()
                } else {
                    format!(
                        "{} (aliases: {})",
                        command.name,
                        command
                            .aliases
                            .iter()
                            .map(Cow::as_ref)
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                }
            })
            .collect::<Vec<_>>();
        available.sort();
        panic!(
            "Command {name} not found. Available commands: {}",
            available.join(", ")
        )
    })
}

pub fn filter_commands<'a>(commands: &'a [Command], query: &str) -> Vec<&'a Command> {
    let query = query.to_lowercase();
    commands
        .iter()
        .filter(|command| !is_command_hidden(command))
        .filter(|command| {
            command.name.starts_with(&query)
                || command
                    .aliases
                    .iter()
                    .any(|alias| alias.starts_with(&query))
                || command.description.to_lowercase().contains(&query)
        })
        .collect()
}

/// Maps to: CC `commands.ts::REMOTE_SAFE_COMMANDS`.
pub fn filter_commands_for_remote_mode(commands: Vec<Command>) -> Vec<Command> {
    const REMOTE_SAFE_NAMES: &[&str] = &[
        "session",
        "exit",
        "clear",
        "help",
        "theme",
        "color",
        "vim",
        "cost",
        "usage",
        "copy",
        "btw",
        "feedback",
        "plan",
        "keybindings",
        "statusline",
        "stickers",
        "mobile",
    ];
    commands
        .into_iter()
        .filter(|command| REMOTE_SAFE_NAMES.contains(&command.name.as_ref()))
        .collect()
}

/// Maps to: CC `commands.ts::isBridgeSafeCommand`.
pub fn is_bridge_safe_command(command: &Command) -> bool {
    const BRIDGE_SAFE_LOCAL_NAMES: &[&str] = &[
        "compact",
        "clear",
        "cost",
        "summary",
        "release-notes",
        "files",
    ];
    match command.kind {
        CommandKind::LocalUi => false,
        CommandKind::Prompt => true,
        CommandKind::Local => BRIDGE_SAFE_LOCAL_NAMES.contains(&command.name.as_ref()),
    }
}

/// Maps to: CC `commands.ts::formatDescriptionWithSource`.
/// Workflow metadata remains unrepresented; plugin provenance uses pluginInfo.
pub fn format_description_with_source(command: &Command) -> String {
    let description = command_description(command);
    if command.kind != CommandKind::Prompt {
        return description.into_owned();
    }
    match command.source {
        CommandSource::Builtin | CommandSource::Mcp => description.into_owned(),
        CommandSource::Bundled => format!("{description} (bundled)"),
        CommandSource::Plugin => match command.plugin_info.as_ref() {
            Some(info) if !info.plugin_manifest.name.is_empty() => {
                format!("({}) {description}", info.plugin_manifest.name)
            }
            _ => format!("{description} (plugin)"),
        },
        CommandSource::PolicySettings => format!("{description} (managed)"),
        CommandSource::UserSettings => format!("{description} (user)"),
        CommandSource::ProjectSettings => format!("{description} (project)"),
    }
}

#[cfg(test)]
pub fn declared_commands_for_tests() -> Vec<Command> {
    commands()
        .into_iter()
        .map(|mut command| {
            // Component tests need the complete declaration catalog without
            // invoking auth/keychain/policy predicates. Preserve static hidden
            // metadata, but treat dynamic gates as enabled/visible.
            command.is_enabled = None;
            command.hidden_resolver = None;
            command
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn description_source_annotations_match_bun_prompt_and_plugin_gates() {
        // Oracle: research/proof/plugin-commands-0916/description-oracle.json,
        // executing the actual commands.ts function under Bun.
        for (kind, source, plugin_name, expected) in [
            (
                CommandKind::Prompt,
                CommandSource::Plugin,
                Some("command-acceptance-0916"),
                "(command-acceptance-0916) CUSTOM_METADATA_0916",
            ),
            (
                CommandKind::Prompt,
                CommandSource::Plugin,
                Some(""),
                "CUSTOM_METADATA_0916 (plugin)",
            ),
            (
                CommandKind::Prompt,
                CommandSource::Plugin,
                None,
                "CUSTOM_METADATA_0916 (plugin)",
            ),
            (
                CommandKind::Local,
                CommandSource::Plugin,
                Some("plugin-name"),
                "CUSTOM_METADATA_0916",
            ),
            (
                CommandKind::LocalUi,
                CommandSource::ProjectSettings,
                None,
                "CUSTOM_METADATA_0916",
            ),
            (
                CommandKind::Prompt,
                CommandSource::Builtin,
                Some("ignored"),
                "CUSTOM_METADATA_0916",
            ),
            (
                CommandKind::Prompt,
                CommandSource::Mcp,
                None,
                "CUSTOM_METADATA_0916",
            ),
            (
                CommandKind::Prompt,
                CommandSource::Bundled,
                None,
                "CUSTOM_METADATA_0916 (bundled)",
            ),
            (
                CommandKind::Prompt,
                CommandSource::PolicySettings,
                None,
                "CUSTOM_METADATA_0916 (managed)",
            ),
            (
                CommandKind::Prompt,
                CommandSource::UserSettings,
                None,
                "CUSTOM_METADATA_0916 (user)",
            ),
            (
                CommandKind::Prompt,
                CommandSource::ProjectSettings,
                None,
                "CUSTOM_METADATA_0916 (project)",
            ),
        ] {
            let mut command = Command::new(kind, "fixture", "CUSTOM_METADATA_0916");
            command.source = source;
            command.plugin_info = plugin_name.map(|name| PluginInfo {
                plugin_manifest: Arc::new(crate::utils::plugins::schemas::PluginManifest {
                    name: name.into(),
                    ..Default::default()
                }),
                repository: "fixture".into(),
            });
            assert_eq!(
                format_description_with_source(&command),
                expected,
                "{kind:?}/{source:?}/{plugin_name:?}"
            );
        }
    }

    #[test]
    fn model_and_sandbox_descriptions_resolve_their_official_getters() {
        let catalog = commands();
        let find = |name: &str| {
            catalog
                .iter()
                .find(|command| command.name == name)
                .unwrap_or_else(|| panic!("/{name} is declared"))
        };

        let model = command_description(find("model"));
        assert!(
            model.starts_with("Set the AI model for Claude Code (currently ")
                && model.ends_with(')'),
            "model={model}"
        );

        let sandbox = command_description(find("sandbox"));
        assert!(
            sandbox.contains("sandbox ") && sandbox.ends_with(" (⏎ to configure)"),
            "sandbox={sandbox}"
        );
    }

    #[test]
    fn inference_config_commands_share_one_immediate_gate() {
        let expected =
            crate::utils::immediate_command::should_inference_config_command_be_immediate();
        let catalog = commands();
        for name in ["model", "fast", effort::NAME] {
            let command = catalog
                .iter()
                .find(|command| command.name == name)
                .unwrap_or_else(|| panic!("/{name} is declared"));
            assert_eq!(command.immediate, expected, "/{name}");
        }
    }

    #[test]
    fn commands_catalog_matches_official_direct_and_provider_shape() {
        let _guard = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let _demo = crate::utils::env_utils::EnvVarGuard::unset("IS_DEMO");

        let catalog = commands();
        // 71 direct descriptors + provider-conditioned logout/login + internal /version.
        let base_len = if crate::utils::build_profile::build_audience().is_internal() {
            72
        } else {
            71
        };
        assert!(catalog.len() == base_len || catalog.len() == base_len + 2);
        assert_eq!(
            catalog.iter().any(|command| command.name == "login"),
            !is_using_3p_services()
        );
        assert_eq!(catalog[0].name, "add-dir");
        let declared_names = catalog
            .iter()
            .map(|command| command.name.as_ref())
            .collect::<HashSet<_>>();
        for expected in [
            "add-dir",
            "advisor",
            "agents",
            "branch",
            "btw",
            "chrome",
            "clear",
            "color",
            "compact",
            "config",
            "copy",
            "desktop",
            "context",
            "cost",
            "diff",
            "doctor",
            "effort",
            "exit",
            "fast",
            "files",
            "heapdump",
            "help",
            "ide",
            "init",
            "keybindings",
            "install-github-app",
            "install-slack-app",
            "mcp",
            "memory",
            "mobile",
            "model",
            "output-style",
            "remote-env",
            "plugin",
            "pr-comments",
            "release-notes",
            "reload-plugins",
            "rename",
            "resume",
            "session",
            "skills",
            "stats",
            "status",
            "statusline",
            "stickers",
            "tag",
            "theme",
            "feedback",
            "review",
            "ultrareview",
            "rewind",
            "security-review",
            "terminal-setup",
            "upgrade",
            "extra-usage",
            "rate-limit-options",
            "usage",
            "insights",
            "vim",
            "think-back",
            "thinkback-play",
            "permissions",
            "plan",
            "privacy-settings",
            "hooks",
            "export",
            "sandbox",
            "passes",
            "tasks",
        ] {
            assert!(declared_names.contains(expected), "missing /{expected}");
        }
        assert_eq!(
            catalog.iter().any(|command| command.name == "version"),
            crate::utils::build_profile::build_audience().is_internal()
        );
    }

    #[test]
    fn get_commands_loads_and_executes_filesystem_skill_before_builtins() {
        let root = std::env::temp_dir().join(format!(
            "cometix-command-registry-skill-{}",
            uuid::Uuid::new_v4()
        ));
        let skill_file = root
            .join(".claude")
            .join("skills")
            .join("registry-review")
            .join("SKILL.md");
        std::fs::create_dir_all(skill_file.parent().unwrap()).unwrap();
        std::fs::write(
            &skill_file,
            "---\ndescription: Review the requested scope\nargument-hint: [scope]\n---\nReview $ARGUMENTS for session ${CLAUDE_SESSION_ID}",
        )
        .unwrap();

        clear_commands_cache();
        let catalog = get_commands(&root);
        let skill_index = catalog
            .iter()
            .position(|command| command.name == "registry-review")
            .expect("filesystem skill should join getCommands");
        let builtin_index = catalog
            .iter()
            .position(|command| command.name == "add-dir")
            .expect("builtin catalog");
        assert!(skill_index < builtin_index);
        let skill = &catalog[skill_index];
        assert_eq!(skill.source, CommandSource::ProjectSettings);
        assert!(skill.get_prompt_for_command.is_some());

        let result = crate::utils::process_user_input::process_slash_command::process_slash_command(
            "/registry-review src",
            Some("skill-command".to_string()),
            &catalog,
        );
        assert!(result.should_query);
        assert!(result.messages.iter().any(|message| matches!(
            &message.kind,
            crate::types::message::RenderableMessageKind::User { message } if matches!(
                message.first_content_block(),
                Some(crate::types::message::UserContent::MetaText(text))
                    if text.contains("Review src for session")
            )
        )));
        assert_eq!(
            format_description_with_source(skill),
            "Review the requested scope (project)"
        );

        let second_skill = root
            .join(".claude")
            .join("skills")
            .join("registry-second")
            .join("SKILL.md");
        std::fs::create_dir_all(second_skill.parent().unwrap()).unwrap();
        std::fs::write(&second_skill, "---\ndescription: Second skill\n---\nSecond").unwrap();
        assert!(find_command("registry-second", &get_commands(&root)).is_none());
        // `clear_command_memoization_caches()` deliberately does NOT rediscover
        // skills — CC states it at `commands.ts:519-522` ("WITHOUT clearing
        // skill caches"), because the dynamic-skill path clears the command
        // memos while keeping the skills it just loaded. Only the full
        // `clear_commands_cache()` reaches `clearSkillCaches()`.
        clear_command_memoization_caches();
        assert!(find_command("registry-second", &get_commands(&root)).is_none());
        clear_commands_cache();
        assert!(find_command("registry-second", &get_commands(&root)).is_some());

        clear_commands_cache();
        let _ = std::fs::remove_dir_all(root);
    }

    /// Maps to: CC `getCommands` (`commands.ts:479-516`).
    ///
    /// The dedup set is `baseCommands.map(c => c.name)` — every enabled command,
    /// builtins included — and the insert point is the first builtin. While the
    /// merge lived in `get_skill_dir_commands` the dedup could only see
    /// skill-dir commands, so a dynamically discovered skill named after a
    /// builtin survived as a SECOND `/clear` and, sitting ahead of the builtins,
    /// won `find_command`. Old shape: both asserts below fail (2 matches, and
    /// the survivor is the skill) — a wrong-command execution, not a hang.
    #[test]
    fn dynamic_skill_named_after_a_builtin_is_dropped_instead_of_shadowing_it() {
        struct AllowedSourcesRestore(Vec<String>);
        impl Drop for AllowedSourcesRestore {
            fn drop(&mut self) {
                crate::bootstrap::state::set_allowed_setting_sources(self.0.clone());
            }
        }

        let _guard = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let _dynamic = crate::skills::load_skills_dir::DynamicSkillsTestSnapshot::capture();
        let _sources =
            AllowedSourcesRestore(crate::bootstrap::state::get_allowed_setting_sources());
        crate::bootstrap::state::set_allowed_setting_sources(vec!["projectSettings".to_string()]);
        clear_commands_cache();
        crate::skills::load_skills_dir::clear_dynamic_skills();

        let root = std::env::temp_dir().join(format!(
            "cometix-dynamic-builtin-collision-{}",
            uuid::Uuid::new_v4()
        ));
        let discovered = root.join("packages/pkg/.claude/skills");
        std::fs::create_dir_all(discovered.join("clear")).unwrap();
        std::fs::write(
            discovered.join("clear").join("SKILL.md"),
            "---\ndescription: Shadowing skill\n---\nShadow",
        )
        .unwrap();
        std::fs::create_dir_all(discovered.join("registry-nested")).unwrap();
        std::fs::write(
            discovered.join("registry-nested").join("SKILL.md"),
            "---\ndescription: Nested skill\n---\nNested",
        )
        .unwrap();
        crate::skills::load_skills_dir::add_skill_directories(std::slice::from_ref(&discovered));

        let catalog = get_commands(&root);
        assert_eq!(
            catalog
                .iter()
                .filter(|command| command.name == "clear")
                .count(),
            1
        );
        assert_eq!(
            find_command("clear", &catalog).map(|command| command.source),
            Some(CommandSource::Builtin)
        );

        // The non-colliding sibling still joins, immediately before the first
        // builtin (`commands.ts:504-516`).
        let nested_index = catalog
            .iter()
            .position(|command| command.name == "registry-nested")
            .expect("dynamic skill joins getCommands");
        let first_builtin_index = catalog
            .iter()
            .position(|command| {
                BUILT_IN_COMMAND_NAMES_WITHOUT_ALIASES.contains(command.name.as_ref())
            })
            .expect("builtin catalog");
        assert_eq!(nested_index, first_builtin_index - 1);

        crate::skills::load_skills_dir::clear_dynamic_skills();
        clear_commands_cache();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn load_all_commands_projects_inline_plugin_commands_and_skills_in_source_order() {
        let _guard = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let previous_inline = crate::bootstrap::state::get_inline_plugins();
        let root = std::env::temp_dir().join(format!(
            "cometix-command-registry-plugin-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(root.join(".claude-plugin")).unwrap();
        std::fs::write(
            root.join(".claude-plugin").join("plugin.json"),
            r#"{"name":"registry-plugin","description":"registry test"}"#,
        )
        .unwrap();
        std::fs::create_dir_all(root.join("commands")).unwrap();
        std::fs::write(
            root.join("commands").join("inspect.md"),
            "---\ndescription: Inspect through plugin\n---\nRoot=${CLAUDE_PLUGIN_ROOT} Args=$ARGUMENTS",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("skills").join("verify")).unwrap();
        std::fs::write(
            root.join("skills").join("verify").join("SKILL.md"),
            "---\ndescription: Verify through plugin\n---\nVerify $ARGUMENTS",
        )
        .unwrap();

        crate::bootstrap::state::set_inline_plugins(vec![root.clone()]);
        clear_commands_cache();
        let catalog = get_commands(&root);
        let plugin_command = find_command("registry-plugin:inspect", &catalog)
            .expect("default plugin commands directory");
        let plugin_skill = find_command("registry-plugin:verify", &catalog)
            .expect("default plugin skills directory");
        assert_eq!(plugin_command.source, CommandSource::Plugin);
        assert_eq!(plugin_command.loaded_from, None);
        assert_eq!(
            plugin_skill.loaded_from,
            Some(crate::skills::load_skills_dir::SkillLoadedFrom::Plugin)
        );
        let command_index = catalog
            .iter()
            .position(|command| command.name == "registry-plugin:inspect")
            .unwrap();
        let skill_index = catalog
            .iter()
            .position(|command| command.name == "registry-plugin:verify")
            .unwrap();
        let builtin_index = catalog
            .iter()
            .position(|command| command.name == "add-dir")
            .unwrap();
        assert!(command_index < skill_index && skill_index < builtin_index);

        let result = crate::utils::process_user_input::process_slash_command::process_slash_command(
            "/registry-plugin:inspect src",
            None,
            &catalog,
        );
        assert!(result.should_query);
        assert!(result.messages.iter().any(|message| matches!(
            &message.kind,
            crate::types::message::RenderableMessageKind::User { message } if matches!(
                message.first_content_block(),
                Some(crate::types::message::UserContent::MetaText(text))
                    if text.contains(&format!("Root={}", root.display())) && text.contains("Args=src")
            )
        )));

        clear_commands_cache();
        crate::bootstrap::state::set_inline_plugins(previous_inline);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn completed_command_descriptors_expose_official_callbacks_and_metadata() {
        let catalog = commands();
        let copy = find_command("copy", &catalog).unwrap();
        assert_eq!(copy.kind, CommandKind::LocalUi);
        assert!(copy.call.is_some());
        assert!(!copy.immediate);

        let rename = find_command("rename", &catalog).unwrap();
        assert_eq!(rename.argument_hint.as_deref(), Some("[name]"));
        assert!(rename.call.is_some());
        assert!(rename.immediate);

        let vim = find_command("vim", &catalog).unwrap();
        assert_eq!(vim.kind, CommandKind::Local);
        assert!(vim.call.is_some());
        assert!(!vim.supports_non_interactive);

        let effort = find_command("effort", &catalog).unwrap();
        assert_eq!(effort.kind, CommandKind::LocalUi);
        assert!(effort.argument_hint.is_none());
        assert!(effort.argument_hint_resolver.is_some());
        assert!(effort.call.is_some());

        let stats = find_command("stats", &catalog).unwrap();
        assert_eq!(stats.kind, CommandKind::LocalUi);
        assert!(stats.call.is_some());
        assert!(!stats.immediate);

        let init = find_command("init", &catalog).unwrap();
        assert_eq!(init.kind, CommandKind::Prompt);
        assert_eq!(
            init.progress_message.as_deref(),
            Some("analyzing your codebase")
        );
        assert_eq!(init.content_length, Some(0));
        assert!(init.get_prompt_for_command.is_some());
    }

    #[test]
    fn command_metadata_uses_current_official_aliases() {
        let catalog = commands();
        assert_eq!(find_command("checkpoint", &catalog).unwrap().name, "rewind");
        assert_eq!(find_command("quit", &catalog).unwrap().name, "exit");
        assert!(find_command("q", &catalog).is_none());
        assert!(find_command("?", &catalog).is_none());
    }

    /// CC `commands/branch/index.ts:8`
    /// `aliases: feature('FORK_SUBAGENT') ? [] : ['fork']`, with
    /// `scripts/build.ts:45` shipping `FORK_SUBAGENT: true` in production. The
    /// first assertion is the wiring itself — alias presence must track the
    /// build feature, so flipping the switch table flips the alias — and the
    /// rest pins the branch that table currently selects.
    #[test]
    fn branch_alias_tracks_the_fork_subagent_build_feature() {
        use crate::utils::feature_flags::{FeatureFlag, feature_enabled};

        let catalog = commands();
        let branch = find_command("branch", &catalog).expect("/branch is declared");
        assert_eq!(
            branch.aliases.is_empty(),
            feature_enabled(FeatureFlag::ForkSubagent),
            "aliases must be [] exactly when feature('FORK_SUBAGENT') is on"
        );

        assert!(feature_enabled(FeatureFlag::ForkSubagent));
        assert!(branch.aliases.is_empty());
        assert!(find_command("fork", &catalog).is_none());
    }

    /// CC 2.1.88's `commands/fork/index.ts` is a `@generated-stub` (`export {}`
    /// only), so `commands.ts:113-117`'s `forkCmd` has no portable body and
    /// Cometix registers no `/fork` — the same treatment every other
    /// stub-backed CC command gets here. The user-visible consequence is the
    /// unknown-command warning from
    /// `process_slash_command.rs:983-985`, reached before any command is
    /// enabled-checked or executed.
    #[test]
    fn fork_resolves_to_the_unknown_command_warning_like_other_stub_commands() {
        let catalog = commands();
        for stub_backed in ["fork", "buddy", "peers", "share", "teleport", "workflows"] {
            assert!(
                find_command(stub_backed, &catalog).is_none(),
                "/{stub_backed} has no CC source to port"
            );
        }

        let result = crate::utils::process_user_input::process_slash_command::process_slash_command(
            "/fork", None, &catalog,
        );
        assert!(!result.should_query);
        assert!(result.messages.iter().any(|message| matches!(
            &message.kind,
            crate::types::message::RenderableMessageKind::System(
                crate::types::message::SystemMessage::Informational { content, .. },
            ) if content == "Unknown command: /fork"
        )));
    }

    /// The predicate at `commands/branch/index.ts:8` is the RAW build feature,
    /// NOT `isForkSubagentEnabled()` (`forkSubagent.ts:33-38`). That gate adds
    /// coordinator-mode and non-interactive vetoes; wiring it here would
    /// resurrect the `fork` alias in every headless run, where CC keeps it
    /// dropped because the catalog is a module-scope constant that never sees
    /// a session. Holding the non-interactive veto proves the alias does not
    /// move with it.
    #[test]
    fn branch_alias_ignores_the_runtime_vetoes_that_gate_the_fork_route() {
        let _env_guard = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let _headless = crate::tools::agent_tool::fork_subagent::fork_veto_environment();
        assert!(
            !crate::tools::agent_tool::fork_subagent::is_fork_subagent_enabled(),
            "the runtime gate must be OFF for this test to say anything"
        );

        let catalog = commands();
        let branch = find_command("branch", &catalog).expect("/branch is declared");
        assert!(branch.aliases.is_empty());
        assert!(find_command("fork", &catalog).is_none());
    }

    #[test]
    fn remote_and_bridge_safety_sets_match_official_membership() {
        let catalog = commands();
        let remote = filter_commands_for_remote_mode(catalog.clone());
        assert!(remote.iter().any(|command| command.name == "copy"));
        assert!(!remote.iter().any(|command| command.name == "model"));
        assert!(is_bridge_safe_command(
            find_command("compact", &catalog).unwrap()
        ));
        assert!(!is_bridge_safe_command(
            find_command("config", &catalog).unwrap()
        ));
        assert!(is_bridge_safe_command(
            find_command("review", &catalog).unwrap()
        ));
    }

    /// CC commands.ts:563-583: source qualifications are independent of the
    /// outer prompt/disableModelInvocation/builtin exclusions. Seed only the
    /// existing loader cache so assertions exercise the real public consumer.
    #[test]
    fn skill_tool_commands_matches_official_bundled_and_listing_filters() {
        use crate::skills::load_skills_dir::SkillLoadedFrom;
        let _lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let _dynamic = crate::skills::load_skills_dir::DynamicSkillsTestSnapshot::capture();
        crate::skills::load_skills_dir::clear_dynamic_skills();
        let root =
            std::env::temp_dir().join(format!("cometix-skill-listing-{}", uuid::Uuid::new_v4()));
        let mut bundled = Command::prompt("bundled-no-description", "Derived description");
        bundled.source = CommandSource::Bundled;
        bundled.loaded_from = Some(SkillLoadedFrom::Bundled);
        let mut bundled_described = bundled.clone();
        bundled_described.name = "bundled-described".into();
        bundled_described.has_user_specified_description = true;
        let mut skills = bundled.clone();
        skills.name = "skills-no-description".into();
        skills.loaded_from = Some(SkillLoadedFrom::Skills);
        let mut legacy = bundled.clone();
        legacy.name = "legacy-no-description".into();
        legacy.loaded_from = Some(SkillLoadedFrom::CommandsDeprecated);
        let mut plugin = bundled.clone();
        plugin.name = "plugin-no-description".into();
        plugin.source = CommandSource::Plugin;
        plugin.loaded_from = Some(SkillLoadedFrom::Plugin);
        let mut described = plugin.clone();
        described.name = "plugin-described".into();
        described.has_user_specified_description = true;
        let mut when_to_use = plugin.clone();
        when_to_use.name = "plugin-when-to-use".into();
        when_to_use.when_to_use = Some("Use for review".into());
        let mut empty_when_to_use = plugin.clone();
        empty_when_to_use.name = "plugin-empty-when-to-use".into();
        empty_when_to_use.when_to_use = Some("".into());
        let mut disabled = bundled.clone();
        disabled.name = "bundled-disabled".into();
        disabled.disable_model_invocation = true;
        let mut local = bundled.clone();
        local.name = "bundled-local".into();
        local.kind = CommandKind::Local;
        let mut builtin = bundled.clone();
        builtin.name = "builtin".into();
        builtin.source = CommandSource::Builtin;
        let generation = crate::skills::load_skills_dir::dynamic_skills_generation();
        let key = (root.clone(), generation);
        LOAD_ALL_COMMANDS_CACHE.lock().unwrap().insert(
            key.clone(),
            Arc::new(vec![
                bundled,
                bundled_described,
                skills,
                legacy,
                plugin,
                described,
                when_to_use,
                empty_when_to_use,
                disabled,
                local,
                builtin,
            ]),
        );
        let actual = get_skill_tool_commands(&root);
        let slash_skills = get_slash_command_tool_skills(&root);
        LOAD_ALL_COMMANDS_CACHE.lock().unwrap().remove(&key);
        assert_eq!(
            actual
                .iter()
                .map(|command| command.name.as_ref())
                .collect::<Vec<_>>(),
            vec![
                "bundled-no-description",
                "bundled-described",
                "skills-no-description",
                "legacy-no-description",
                "plugin-described",
                "plugin-when-to-use",
            ]
        );
        assert_eq!(
            slash_skills
                .iter()
                .map(|command| command.name.as_ref())
                .collect::<Vec<_>>(),
            vec![
                "bundled-described",
                "plugin-described",
                "plugin-when-to-use",
            ]
        );
    }

    #[test]
    fn mcp_skill_commands_match_feature_gate_and_source_predicate() {
        use crate::utils::feature_flags::{FeatureFlag, feature_enabled};

        let enabled =
            Command::from_mcp_prompt(crate::services::mcp::client::McpPromptCommandSnapshot {
                name: "mcp:enabled".to_string(),
                description: "Enabled MCP skill".to_string(),
                has_user_specified_description: true,
                user_facing_name: "mcp:enabled".to_string(),
                arg_names: Vec::new(),
                source: "mcp",
            });
        let mut disabled = enabled.clone();
        disabled.name = "mcp:disabled".into();
        disabled.disable_model_invocation = true;
        let mut local = enabled.clone();
        local.name = "local:prompt".into();
        local.loaded_from = None;
        local.source = CommandSource::ProjectSettings;

        let actual = get_mcp_skill_commands(&[enabled, disabled, local]);
        if feature_enabled(FeatureFlag::McpSkills) {
            assert_eq!(
                actual
                    .iter()
                    .map(|command| command.name.as_ref())
                    .collect::<Vec<_>>(),
                vec!["mcp:enabled"]
            );
        } else {
            assert!(actual.is_empty());
        }
    }
}
