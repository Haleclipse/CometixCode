//! `CliConfig` + per-category implementation status.
//!
//! Progress table companion: `docs/ENTRY_OWNERSHIP_AUDIT.md` Batch 9.

use std::path::PathBuf;

/// Implementation status for a flag category or individual option.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImplStatus {
    /// Behavior is live on the interactive path.
    Live,
    /// Parsed and partially applied; remaining bits still short-circuit when required.
    Partial,
    /// Parsed into config but must not silently no-op — dispatch exits 1.
    Unimplemented,
}

/// Structured CLI config — Maps to CC commander options on `main.tsx`.
#[derive(Clone, Debug, Default)]
pub struct CliConfig {
    /// Raw argv including binary name (index 0).
    pub argv: Vec<String>,

    // --- Version / help ---
    pub show_version: bool,
    pub show_help: bool,

    // --- Debug / verbose ---
    pub debug: bool,
    pub debug_to_stderr: bool,
    pub debug_file: Option<PathBuf>,
    /// Optional category filter from `-d [filter]` / `--debug=filter`.
    pub debug_filter: Option<String>,
    pub verbose: bool,

    // --- Bare ---
    pub bare: bool,

    // --- Print / headless ---
    pub print: bool,
    pub output_format: Option<String>,
    pub input_format: Option<String>,
    pub json_schema: Option<String>,
    pub stream_json: bool,
    pub include_partial_messages: bool,
    pub replay_user_messages: bool,
    pub enable_auth_status: bool,

    // --- Prompt positional ---
    pub prompt: Option<String>,

    // --- Model / thinking / effort ---
    pub model: Option<String>,
    pub thinking: Option<String>,
    pub effort: Option<String>,
    pub max_thinking_tokens: Option<String>,
    pub max_turns: Option<String>,
    pub max_budget_usd: Option<String>,
    pub task_budget: Option<String>,
    pub fallback_model: Option<String>,
    pub workload: Option<String>,

    // --- Permissions ---
    pub permission_mode: Option<String>,
    pub dangerously_skip_permissions: bool,
    pub allowed_tools: Vec<String>,
    pub disallowed_tools: Vec<String>,
    /// Maps to CC `--tools`: explicit model-facing built-in tool subset.
    pub tools: Option<Vec<String>>,
    /// Maps to CC `--permission-prompt-tool`; `stdio` enables the SDK
    /// `can_use_tool` control request/response bridge.
    pub permission_prompt_tool: Option<String>,
    /// Maps to CC `--disable-slash-commands` / REPL `disableSlashCommands`.
    pub disable_slash_commands: bool,

    // --- Session ---
    pub resume: Option<String>,
    /// `Some("")` represents bare `--from-pr`; otherwise stores its value.
    pub from_pr: Option<String>,
    pub continue_session: bool,
    pub session_id: Option<String>,
    pub fork_session: bool,
    /// Commander `--no-session-persistence` projects to `sessionPersistence=false`.
    pub session_persistence: Option<bool>,

    // --- System prompt ---
    pub system_prompt: Option<String>,
    pub system_prompt_file: Option<PathBuf>,
    pub append_system_prompt: Option<String>,
    pub append_system_prompt_file: Option<PathBuf>,

    // --- MCP ---
    pub mcp_config: Vec<String>,
    pub mcp_debug: bool,
    pub strict_mcp_config: bool,

    // --- Settings / dirs / agents / plugins ---
    /// Comma-separated `user,project,local` source allowlist.
    pub setting_sources: Option<String>,
    pub settings: Option<PathBuf>,
    pub add_dirs: Vec<PathBuf>,
    /// Maps to CC `--agent <agent>` (main-thread agent type name).
    pub agent: Option<String>,
    pub agents_json: Option<String>,
    pub plugin_dirs: Vec<PathBuf>,
    pub cowork: bool,

    // --- Teammate / swarm ---
    pub agent_id: Option<String>,
    pub agent_name: Option<String>,
    pub team_name: Option<String>,
    pub agent_color: Option<String>,
    pub plan_mode_required: bool,
    pub parent_session_id: Option<String>,
    pub teammate_mode: Option<String>,
    pub agent_type: Option<String>,

    // --- Chrome ---
    pub chrome: Option<bool>,

    // --- Worktree / tmux / remote / bg / daemon / subcommands ---
    pub subcommand: Option<String>,
    pub worktree: bool,
    pub tmux: bool,
    pub remote: bool,
    pub background: bool,
    pub daemon: bool,

    // --- Init / maintenance ---
    pub init: bool,
    pub init_only: bool,
    pub maintenance: bool,

    /// Unknown flags (not in the wired table) — always Unimplemented.
    pub unknown_flags: Vec<String>,
}

impl CliConfig {
    /// Category-level status table (Batch 9 progress).
    pub fn category_status(&self) -> Vec<(&'static str, ImplStatus)> {
        vec![
            ("version/help", ImplStatus::Live),
            (
                "debug/verbose",
                if self.debug || self.debug_file.is_some() || self.verbose {
                    ImplStatus::Live
                } else {
                    ImplStatus::Live
                },
            ),
            ("bare", ImplStatus::Live),
            ("print/headless", ImplStatus::Live),
            (
                "prompt",
                if self.print {
                    ImplStatus::Live
                } else {
                    ImplStatus::Partial
                },
            ),
            ("model/thinking/effort", ImplStatus::Live),
            ("permissions", ImplStatus::Live),
            (
                "session",
                if (self.session_id.is_some() || self.fork_session) && !self.print {
                    ImplStatus::Unimplemented
                } else if self.from_pr.is_some() {
                    ImplStatus::Partial
                } else if self.resume.is_some() || self.continue_session {
                    ImplStatus::Partial
                } else {
                    ImplStatus::Live
                },
            ),
            ("system-prompt", ImplStatus::Live),
            ("mcp", ImplStatus::Live),
            ("settings/dirs/agents/plugins", ImplStatus::Live),
            ("teammate/swarm", ImplStatus::Live),
            ("chrome", ImplStatus::Live),
            (
                "worktree/tmux/remote/bg/daemon/subcommands",
                if self.subcommand.is_some()
                    || self.worktree
                    || self.tmux
                    || self.remote
                    || self.background
                    || self.daemon
                {
                    ImplStatus::Unimplemented
                } else {
                    ImplStatus::Live
                },
            ),
            (
                "init/maintenance",
                if self.init || self.init_only || self.maintenance {
                    ImplStatus::Unimplemented
                } else {
                    ImplStatus::Live
                },
            ),
        ]
    }

    /// Reasons that must short-circuit before interactive launch.
    pub fn unimplemented_reasons(&self) -> Vec<String> {
        let mut reasons = Vec::new();

        // Print, text/json/stream-json framing, and StructuredOutput schemas
        // are owned by `cli::print` and use the canonical query actor.

        // --model / --effort / --thinking / --max-thinking-tokens are Live
        // (applied by main's initialState + typed ReplProps launch assembly).
        // --permission-mode / --dangerously-skip-permissions / allow|deny tools
        // are Live (applied in build_initial_app_state).
        // --resume / --continue / --from-pr picker filtering are Live for
        // read-only initial restoration;
        // session-file adoption/worktree/fork side effects remain later seams.
        // --system-prompt* are Live (resolved by main into ReplProps).

        if !self.print {
            if let Some(id) = &self.session_id {
                reasons.push(format!("--session-id={id}"));
            }
            if self.fork_session {
                reasons.push("--fork-session".to_string());
            }
        }

        if let Some(sub) = &self.subcommand {
            if sub != "doctor" {
                reasons.push(sub.clone());
            }
        }
        if self.worktree {
            reasons.push("--worktree".to_string());
        }
        if self.tmux {
            reasons.push("--tmux".to_string());
        }
        if self.remote {
            reasons.push("--remote".to_string());
        }
        if self.background {
            reasons.push("--background".to_string());
        }
        if self.daemon {
            reasons.push("daemon".to_string());
        }

        if self.init {
            reasons.push("--init".to_string());
        }
        if self.init_only {
            reasons.push("--init-only".to_string());
        }
        if self.maintenance {
            reasons.push("--maintenance".to_string());
        }

        for flag in &self.unknown_flags {
            reasons.push(flag.clone());
        }

        reasons
    }

    pub fn print_help() {
        println!(
            "\
Usage: nyan [options] [prompt]

Live:
  -v, --version              Print version
  -h, --help                 Show this help
  -d, --debug [filter]       Enable debug log file (optional category filter)
      --debug-to-stderr      Debug to stderr instead of file (-d2e)
      --debug-file <path>    Debug log file (implies debug)
      --verbose              Verbose mode
      --bare                 Minimal/simple mode (CLAUDE_CODE_SIMPLE)
      --model <model>        Session model (or `default`)
      --effort <level>       Effort: low|medium|high|xhigh|max
      --thinking <mode>      adaptive|enabled|disabled
      --mcp-debug            Deprecated alias for debug logging
      --mcp-config <configs...>  Load MCP JSON files or inline JSON
      --strict-mcp-config    Ignore non-enterprise auto-discovered MCP configs
      --max-thinking-tokens <n>  Thinking budget (deprecated; prefer --thinking)
      --max-turns / --max-budget-usd / --task-budget  Headless limits
      --fallback-model <model>  API fallback model for overloaded requests
      --permission-mode <m>  default|acceptEdits|plan|bypassPermissions|…
      --permission-prompt-tool=stdio  SDK can_use_tool control bridge
      --dangerously-skip-permissions
      --allowedTools / --disallowedTools / --tools
      --disable-slash-commands  Disable all slash commands
      --system-prompt[-file] / --append-system-prompt[-file]
  -c, --continue             Resume latest conversation in this directory
  -r, --resume [session]     Resume session (omit value to open picker)
      --from-pr [value]      Resume a PR-linked session by number/URL or picker
      --settings <path>      Settings file override
      --add-dir <dirs...>    Extra AGENTS.md directories
      --agents <json>        Inline agents JSON
      --agent <name>         Main-thread agent type (--agent / settings.agent)
      --plugin-dir <path>    Inline plugin directory (repeatable)
      --chrome / --no-chrome Claude in Chrome onboarding
  -p, --print                Headless/print path
      --output-format / --input-format / --json-schema / --stream-json
      --include-partial-messages / --replay-user-messages
      --no-session-persistence  Disable transcript persistence in print mode
      Teammate flags         --agent-id/--agent-name/--team-name/...

Unimplemented (parsed, then exit 1):
      --session-id / --fork-session
      Subcommands: mcp, auth, plugin, doctor, update, …
      --init / --init-only / --maintenance
      --worktree / --tmux / --remote / --bg / daemon
"
        );
    }
}
