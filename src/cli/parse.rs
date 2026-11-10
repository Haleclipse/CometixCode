//! Argv → [`CliConfig`] representation adapter.
//!
//! Maps to CC `main.tsx` Commander option declarations. Rust has no Commander
//! runtime, so this module projects argv into typed startup intent without
//! owning settings or other option semantics.

use super::config::CliConfig;
use std::path::PathBuf;

const SUBCOMMANDS: &[&str] = &[
    "mcp",
    "auth",
    "plugin",
    "plugins",
    "doctor",
    "update",
    "upgrade",
    "agents",
    "auto-mode",
    "install",
    "setup-token",
    "migrate",
    "ps",
    "logs",
    "attach",
    "kill",
    "new",
    "list",
    "reply",
    "environment-runner",
    "self-hosted-runner",
    "daemon",
];

fn take_value(args: &[String], index: &mut usize) -> Option<String> {
    if *index + 1 < args.len() {
        *index += 1;
        Some(args[*index].clone())
    } else {
        None
    }
}

fn take_optional_value(args: &[String], index: &mut usize) -> Option<String> {
    let next = args.get(*index + 1)?;
    if next.starts_with('-') {
        return None;
    }
    *index += 1;
    Some(next.clone())
}

fn take_values_until_flag(args: &[String], index: &mut usize) -> Vec<String> {
    let mut values = Vec::new();
    while *index + 1 < args.len() {
        let next = &args[*index + 1];
        if next.starts_with('-') {
            break;
        }
        *index += 1;
        values.push(args[*index].clone());
    }
    values
}

fn strip_eq(flag: &str, arg: &str) -> Option<String> {
    let prefix = format!("{flag}=");
    arg.strip_prefix(&prefix).map(str::to_string)
}

/// Parse full argv (including binary name at index 0) into [`CliConfig`].
///
/// Maps to CC `main.tsx` Commander option declarations and parse result.
pub fn parse_cli_config(argv: &[String]) -> CliConfig {
    let mut config = CliConfig {
        argv: argv.to_vec(),
        ..CliConfig::default()
    };

    let args: Vec<String> = if argv.first().map(|a| !a.starts_with('-')).unwrap_or(false) {
        argv.iter().skip(1).cloned().collect()
    } else {
        argv.to_vec()
    };

    let mut index = 0;
    let mut positional: Vec<String> = Vec::new();

    while index < args.len() {
        let arg = args[index].clone();

        if let Some(sub) = SUBCOMMANDS.iter().find(|s| **s == arg) {
            config.subcommand = Some((*sub).to_string());
            if *sub == "daemon" {
                config.daemon = true;
            }
            if matches!(*sub, "ps" | "logs" | "attach" | "kill") {
                config.background = true;
            }
            if *sub == "update" {
                let mut options = true;
                for arg in &args[index + 1..] {
                    match arg.as_str() {
                        "--" if options => options = false,
                        "-h" | "--help" if options => config.show_help = true,
                        _ => config.unknown_flags.push(arg.clone()),
                    }
                }
            }
            // Other subcommands still consume remaining arguments as opaque.
            index = args.len();
            break;
        }

        match arg.as_str() {
            "-v" | "-V" | "--version" => config.show_version = true,
            "-h" | "--help" => config.show_help = true,
            "-d2e" | "--debug-to-stderr" => {
                config.debug = true;
                config.debug_to_stderr = true;
            }
            "-d" | "--debug" => {
                config.debug = true;
                // Commander `-d [filter]`: optional next non-flag token is filter.
                if index + 1 < args.len() && !args[index + 1].starts_with('-') {
                    index += 1;
                    config.debug_filter = Some(args[index].clone());
                }
            }
            "--verbose" => config.verbose = true,
            "--bare" => config.bare = true,
            "-p" | "--print" => config.print = true,
            "--stream-json" => {
                config.stream_json = true;
                config.print = true;
            }
            "--include-partial-messages" => config.include_partial_messages = true,
            "--replay-user-messages" => config.replay_user_messages = true,
            "--enable-auth-status" => config.enable_auth_status = true,
            "--continue" | "-c" => config.continue_session = true,
            "-r" => {
                // Maps to: CC `-r, --resume [value]` — optional value.
                config.resume =
                    take_optional_value(&args, &mut index).or_else(|| Some(String::new()));
            }
            other if other.starts_with("-r") && other.len() > 2 => {
                // Commander accepts an optional short-option value without a
                // separating space (`-rabc`, and `-r=abc` as `=abc`).
                config.resume = Some(other[2..].to_string());
            }
            "--fork-session" => config.fork_session = true,
            "--no-session-persistence" => config.session_persistence = Some(false),
            "--dangerously-skip-permissions" => config.dangerously_skip_permissions = true,
            "--disable-slash-commands" => config.disable_slash_commands = true,
            "--plan-mode-required" => config.plan_mode_required = true,
            "--mcp-debug" => config.mcp_debug = true,
            "--strict-mcp-config" => config.strict_mcp_config = true,
            "--cowork" | "--cowork=true" => config.cowork = true,
            "--worktree" => config.worktree = true,
            "--tmux" => config.tmux = true,
            "--bg" | "--background" => config.background = true,
            "--init" => config.init = true,
            "--init-only" => config.init_only = true,
            "--maintenance" => config.maintenance = true,
            "--daemon-worker" => config.daemon = true,

            other if other.starts_with("--debug-file") => {
                config.debug = true;
                if let Some(v) = strip_eq("--debug-file", other) {
                    config.debug_file = Some(PathBuf::from(v));
                } else if let Some(v) = take_value(&args, &mut index) {
                    config.debug_file = Some(PathBuf::from(v));
                }
            }
            other if other.starts_with("--debug=") => {
                config.debug = true;
                if let Some(v) = strip_eq("--debug", other)
                    && !v.is_empty()
                {
                    config.debug_filter = Some(v);
                }
            }
            other if other.starts_with("--output-format") => {
                config.print = true;
                config.output_format =
                    strip_eq("--output-format", other).or_else(|| take_value(&args, &mut index));
            }
            other if other.starts_with("--input-format") => {
                config.print = true;
                config.input_format =
                    strip_eq("--input-format", other).or_else(|| take_value(&args, &mut index));
            }
            other if other.starts_with("--json-schema") => {
                config.print = true;
                config.json_schema =
                    strip_eq("--json-schema", other).or_else(|| take_value(&args, &mut index));
            }
            other if other.starts_with("--model") => {
                config.model = strip_eq("--model", other).or_else(|| take_value(&args, &mut index));
            }
            other if other.starts_with("--thinking") => {
                config.thinking =
                    strip_eq("--thinking", other).or_else(|| take_value(&args, &mut index));
            }
            other if other.starts_with("--effort") => {
                config.effort =
                    strip_eq("--effort", other).or_else(|| take_value(&args, &mut index));
            }
            other if other.starts_with("--max-thinking-tokens") => {
                config.max_thinking_tokens = strip_eq("--max-thinking-tokens", other)
                    .or_else(|| take_value(&args, &mut index));
            }
            other if other.starts_with("--max-turns") => {
                config.max_turns =
                    strip_eq("--max-turns", other).or_else(|| take_value(&args, &mut index));
            }
            other if other.starts_with("--max-budget-usd") => {
                config.max_budget_usd =
                    strip_eq("--max-budget-usd", other).or_else(|| take_value(&args, &mut index));
            }
            other if other.starts_with("--task-budget") => {
                config.task_budget =
                    strip_eq("--task-budget", other).or_else(|| take_value(&args, &mut index));
            }
            other if other.starts_with("--fallback-model") => {
                config.fallback_model =
                    strip_eq("--fallback-model", other).or_else(|| take_value(&args, &mut index));
            }
            other if other.starts_with("--workload") => {
                config.workload =
                    strip_eq("--workload", other).or_else(|| take_value(&args, &mut index));
            }
            other if other.starts_with("--permission-mode") => {
                config.permission_mode =
                    strip_eq("--permission-mode", other).or_else(|| take_value(&args, &mut index));
            }
            other if other.starts_with("--permission-prompt-tool") => {
                config.permission_prompt_tool = strip_eq("--permission-prompt-tool", other)
                    .or_else(|| take_value(&args, &mut index));
            }
            other if other == "--tools" || other.starts_with("--tools=") => {
                let values = match strip_eq("--tools", other) {
                    Some(value) => vec![value],
                    None => take_values_until_flag(&args, &mut index),
                };
                config.tools = Some(values);
            }
            other
                if other.starts_with("--allowedTools") || other.starts_with("--allowed-tools") =>
            {
                let flag = if other.starts_with("--allowedTools") {
                    "--allowedTools"
                } else {
                    "--allowed-tools"
                };
                if let Some(v) = strip_eq(flag, other) {
                    config.allowed_tools.push(v);
                } else {
                    config
                        .allowed_tools
                        .extend(take_values_until_flag(&args, &mut index));
                }
            }
            other
                if other.starts_with("--disallowedTools")
                    || other.starts_with("--disallowed-tools") =>
            {
                let flag = if other.starts_with("--disallowedTools") {
                    "--disallowedTools"
                } else {
                    "--disallowed-tools"
                };
                if let Some(v) = strip_eq(flag, other) {
                    config.disallowed_tools.push(v);
                } else {
                    config
                        .disallowed_tools
                        .extend(take_values_until_flag(&args, &mut index));
                }
            }
            other if other == "--resume" || other.starts_with("--resume=") => {
                config.resume = match strip_eq("--resume", other) {
                    Some(value) => Some(value),
                    None => take_optional_value(&args, &mut index).or_else(|| Some(String::new())),
                };
            }
            other if other == "--from-pr" || other.starts_with("--from-pr=") => {
                // CC's arg parser maps a missing/empty optional value to `true`;
                // Rust represents that branch as an empty string.
                config.from_pr = match strip_eq("--from-pr", other) {
                    Some(value) => Some(value),
                    None => take_optional_value(&args, &mut index).or_else(|| Some(String::new())),
                };
            }
            other if other.starts_with("--session-id") => {
                config.session_id =
                    strip_eq("--session-id", other).or_else(|| take_value(&args, &mut index));
            }
            other if other.starts_with("--system-prompt-file") => {
                if let Some(v) = strip_eq("--system-prompt-file", other)
                    .or_else(|| take_value(&args, &mut index))
                {
                    config.system_prompt_file = Some(PathBuf::from(v));
                }
            }
            other if other.starts_with("--append-system-prompt-file") => {
                if let Some(v) = strip_eq("--append-system-prompt-file", other)
                    .or_else(|| take_value(&args, &mut index))
                {
                    config.append_system_prompt_file = Some(PathBuf::from(v));
                }
            }
            other if other.starts_with("--system-prompt") => {
                config.system_prompt =
                    strip_eq("--system-prompt", other).or_else(|| take_value(&args, &mut index));
            }
            other if other.starts_with("--append-system-prompt") => {
                config.append_system_prompt = strip_eq("--append-system-prompt", other)
                    .or_else(|| take_value(&args, &mut index));
            }
            other if other.starts_with("--mcp-config") => {
                if let Some(value) = strip_eq("--mcp-config", other) {
                    config.mcp_config.push(value);
                } else {
                    config
                        .mcp_config
                        .extend(take_values_until_flag(&args, &mut index));
                }
            }
            other if other.starts_with("--setting-sources") => {
                config.setting_sources =
                    strip_eq("--setting-sources", other).or_else(|| take_value(&args, &mut index));
            }
            other if other.starts_with("--settings") => {
                if let Some(v) =
                    strip_eq("--settings", other).or_else(|| take_value(&args, &mut index))
                {
                    config.settings = Some(PathBuf::from(v));
                }
            }
            other if other.starts_with("--add-dir") => {
                if let Some(v) = strip_eq("--add-dir", other) {
                    config.add_dirs.push(PathBuf::from(v));
                } else {
                    for v in take_values_until_flag(&args, &mut index) {
                        config.add_dirs.push(PathBuf::from(v));
                    }
                }
            }
            other if other.starts_with("--agents") => {
                config.agents_json =
                    strip_eq("--agents", other).or_else(|| take_value(&args, &mut index));
            }
            // Maps to the Commander declaration in `main.tsx:1689-1696`:
            // `--plugin-dir <path>` consumes exactly one path. Repeat the
            // option for multiple session-only plugins; do not swallow the
            // following positional prompt or subcommand as another path.
            "--plugin-dir" => {
                if let Some(v) = take_optional_value(&args, &mut index) {
                    config.plugin_dirs.push(PathBuf::from(v));
                }
            }
            other if other.starts_with("--plugin-dir=") => {
                if let Some(v) = strip_eq("--plugin-dir", other) {
                    config.plugin_dirs.push(PathBuf::from(v));
                }
            }
            other if other.starts_with("--agent-id") => {
                config.agent_id =
                    strip_eq("--agent-id", other).or_else(|| take_value(&args, &mut index));
            }
            other if other.starts_with("--agent-name") => {
                config.agent_name =
                    strip_eq("--agent-name", other).or_else(|| take_value(&args, &mut index));
            }
            other if other.starts_with("--team-name") => {
                config.team_name =
                    strip_eq("--team-name", other).or_else(|| take_value(&args, &mut index));
            }
            other if other.starts_with("--agent-color") => {
                config.agent_color =
                    strip_eq("--agent-color", other).or_else(|| take_value(&args, &mut index));
            }
            other if other.starts_with("--parent-session-id") => {
                config.parent_session_id = strip_eq("--parent-session-id", other)
                    .or_else(|| take_value(&args, &mut index));
            }
            other if other.starts_with("--teammate-mode") => {
                config.teammate_mode =
                    strip_eq("--teammate-mode", other).or_else(|| take_value(&args, &mut index));
            }
            other if other.starts_with("--agent-type") => {
                config.agent_type =
                    strip_eq("--agent-type", other).or_else(|| take_value(&args, &mut index));
            }
            // Must follow `--agent-*` / `--agents` arms so those are not swallowed.
            other if other == "--agent" || other.starts_with("--agent=") => {
                config.agent = strip_eq("--agent", other).or_else(|| take_value(&args, &mut index));
            }
            other
                if other.starts_with("--computer-use-mcp")
                    || other.starts_with("--dump-system-prompt")
                    || other == "--update"
                    || other == "--upgrade" =>
            {
                config.unknown_flags.push(other.to_string());
            }
            other if other.starts_with('-') => {
                config.unknown_flags.push(other.to_string());
            }
            _ => positional.push(arg),
        }

        index += 1;
    }

    if let Some(prompt) = positional.into_iter().next() {
        config.prompt = Some(prompt);
    }

    config
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(args: &[&str]) -> Vec<String> {
        std::iter::once("nyan".to_string())
            .chain(args.iter().map(|a| (*a).to_string()))
            .collect()
    }

    #[test]
    fn parse_update_help_before_and_after_subcommand() {
        for args in [
            vec!["update", "--help"],
            vec!["update", "-h"],
            vec!["--help", "update"],
            vec!["-h", "update"],
        ] {
            let config = parse_cli_config(&argv(&args));
            assert_eq!(config.subcommand.as_deref(), Some("update"), "{args:?}");
            assert!(config.show_help, "{args:?}");
            assert!(config.unimplemented_reasons().is_empty(), "{args:?}");
        }
    }

    #[test]
    fn parse_update_rejects_unsupported_arguments() {
        for arg in ["--check", "--unknown", "unexpected", "--help=bad"] {
            let config = parse_cli_config(&argv(&["update", arg]));
            assert!(!config.show_help);
            assert_eq!(config.unknown_flags, vec![arg]);
            assert_eq!(config.unimplemented_reasons(), vec![arg]);
        }
    }

    #[test]
    fn parse_update_respects_option_terminator() {
        let config = parse_cli_config(&argv(&["update", "--"]));
        assert!(!config.show_help);
        assert!(config.unimplemented_reasons().is_empty());

        for flag in ["--help", "-h"] {
            let config = parse_cli_config(&argv(&["update", "--", flag]));
            assert!(!config.show_help);
            assert_eq!(config.unimplemented_reasons(), vec![flag]);
        }
    }

    #[test]
    fn retired_remote_words_follow_normal_prompt_routing() {
        for word in ["remote-control", "rc", "remote", "sync", "bridge"] {
            let config = parse_cli_config(&argv(&[word, "hello"]));
            assert_eq!(config.prompt.as_deref(), Some(word));
            assert!(config.unknown_flags.is_empty());
            assert!(config.subcommand.is_none());
            assert!(config.unimplemented_reasons().is_empty());
        }
        let config = parse_cli_config(&argv(&["--remote", "hello"]));
        assert_eq!(config.unknown_flags, vec!["--remote"]);
        assert_eq!(config.prompt.as_deref(), Some("hello"));
        assert!(!config.unimplemented_reasons().is_empty());
        assert!(parse_cli_config(&argv(&["--bg"])).background);
        assert!(parse_cli_config(&argv(&["daemon"])).daemon);
    }

    #[test]
    fn parse_live_version_and_bare() {
        let c = parse_cli_config(&argv(&["--version"]));
        assert!(c.show_version);
        let c = parse_cli_config(&argv(&["--bare", "--add-dir", "/tmp/a", "/tmp/b"]));
        assert!(c.bare);
        assert_eq!(c.add_dirs.len(), 2);
    }

    #[test]
    fn parse_setting_sources_accepts_separate_and_equals_forms() {
        let separate = parse_cli_config(&argv(&["--setting-sources", "user,project"]));
        assert_eq!(separate.setting_sources.as_deref(), Some("user,project"));
        let equals = parse_cli_config(&argv(&["--setting-sources=local"]));
        assert_eq!(equals.setting_sources.as_deref(), Some("local"));
    }

    #[test]
    fn parse_plugin_dir_consumes_one_path_per_occurrence() {
        let repeated = parse_cli_config(&argv(&[
            "--plugin-dir",
            "/tmp/one",
            "--plugin-dir=/tmp/two",
            "prompt",
        ]));
        assert_eq!(
            repeated.plugin_dirs,
            vec![PathBuf::from("/tmp/one"), PathBuf::from("/tmp/two")]
        );
        assert_eq!(repeated.prompt.as_deref(), Some("prompt"));

        // Commander reports a missing option argument and leaves the next
        // flag available for its own parser; the projection keeps that flag
        // visible instead of consuming it as a directory.
        let missing = parse_cli_config(&argv(&["--plugin-dir", "--verbose"]));
        assert!(missing.plugin_dirs.is_empty());
        assert!(missing.verbose);
    }

    #[test]
    fn plugin_dir_prefixes_are_not_accepted_as_other_flags() {
        let config = parse_cli_config(&argv(&["--plugin-directory", "/tmp/one"]));
        assert_eq!(config.plugin_dirs, Vec::<PathBuf>::new());
        assert_eq!(config.unknown_flags, vec!["--plugin-directory"]);
        assert_eq!(config.prompt.as_deref(), Some("/tmp/one"));
    }

    #[test]
    fn parse_print_marks_headless() {
        let c = parse_cli_config(&argv(&["-p", "hi"]));
        assert!(c.print);
        assert_eq!(c.prompt.as_deref(), Some("hi"));
        assert!(c.unimplemented_reasons().is_empty());
    }

    #[test]
    fn parse_stdio_permission_prompt_tool_for_sdk_control_bridge() {
        let c = parse_cli_config(&argv(&[
            "-p",
            "--input-format=stream-json",
            "--output-format=stream-json",
            "--permission-prompt-tool=stdio",
            "--include-partial-messages",
            "--replay-user-messages",
            "--max-turns=4",
            "--max-budget-usd=1.5",
            "--task-budget=9000",
            "--fallback-model=haiku",
            "--workload=cron",
            "--tools=Read,Bash",
        ]));
        assert_eq!(c.permission_prompt_tool.as_deref(), Some("stdio"));
        assert!(c.include_partial_messages);
        assert!(c.replay_user_messages);
        assert_eq!(c.max_turns.as_deref(), Some("4"));
        assert_eq!(c.max_budget_usd.as_deref(), Some("1.5"));
        assert_eq!(c.task_budget.as_deref(), Some("9000"));
        assert_eq!(c.fallback_model.as_deref(), Some("haiku"));
        assert_eq!(c.workload.as_deref(), Some("cron"));
        assert_eq!(
            c.tools.as_deref(),
            Some(["Read,Bash".to_string()].as_slice())
        );
        assert!(c.unimplemented_reasons().is_empty());
    }

    #[test]
    fn parse_model_and_resume_are_live() {
        let c = parse_cli_config(&argv(&["--model", "opus", "--resume", "abc"]));
        assert_eq!(c.model.as_deref(), Some("opus"));
        assert_eq!(c.resume.as_deref(), Some("abc"));
        let reasons = c.unimplemented_reasons();
        assert!(!reasons.iter().any(|r| r.starts_with("--model")));
        assert!(!reasons.iter().any(|r| r.starts_with("--resume")));

        let fork = parse_cli_config(&argv(&[
            "-p",
            "--resume",
            "abc",
            "--fork-session",
            "--session-id",
            "123e4567-e89b-12d3-a456-426614174000",
        ]));
        assert!(fork.unimplemented_reasons().is_empty());
    }

    #[test]
    fn parse_agent_does_not_collide_with_teammate_flags() {
        let c = parse_cli_config(&argv(&[
            "--agent",
            "reviewer",
            "--agent-id",
            "r@team",
            "--agents",
            "{}",
        ]));
        assert_eq!(c.agent.as_deref(), Some("reviewer"));
        assert_eq!(c.agent_id.as_deref(), Some("r@team"));
        assert_eq!(c.agents_json.as_deref(), Some("{}"));
    }

    #[test]
    fn parse_system_prompt_is_live() {
        let c = parse_cli_config(&argv(&[
            "--system-prompt",
            "CUSTOM",
            "--append-system-prompt",
            "APPEND",
        ]));
        assert!(c.unimplemented_reasons().is_empty());
        assert_eq!(c.system_prompt.as_deref(), Some("CUSTOM"));
        assert_eq!(c.append_system_prompt.as_deref(), Some("APPEND"));
    }

    #[test]
    fn parse_from_pr_matches_commander_optional_value() {
        let bare = parse_cli_config(&argv(&["--from-pr", "--verbose"]));
        assert_eq!(bare.from_pr.as_deref(), Some(""));
        assert!(bare.verbose);

        let number = parse_cli_config(&argv(&["--from-pr", "123"]));
        assert_eq!(number.from_pr.as_deref(), Some("123"));
        assert!(number.unimplemented_reasons().is_empty());

        let url = parse_cli_config(&argv(&["--from-pr=https://github.com/acme/repo/pull/9"]));
        assert_eq!(
            url.from_pr.as_deref(),
            Some("https://github.com/acme/repo/pull/9")
        );
    }

    #[test]
    fn parse_short_resume_flag() {
        let c = parse_cli_config(&argv(&["-r"]));
        assert_eq!(c.resume.as_deref(), Some(""));
        assert!(c.unimplemented_reasons().is_empty());
    }

    #[test]
    fn parse_bare_resume_does_not_consume_the_next_option_like_commander() {
        let short = parse_cli_config(&argv(&["-r", "--verbose"]));
        assert_eq!(short.resume.as_deref(), Some(""));
        assert!(short.verbose);

        let long = parse_cli_config(&argv(&["--resume", "--verbose"]));
        assert_eq!(long.resume.as_deref(), Some(""));
        assert!(long.verbose);

        let attached = parse_cli_config(&argv(&["-rfind-me"]));
        assert_eq!(attached.resume.as_deref(), Some("find-me"));

        let equals_empty = parse_cli_config(&argv(&["--resume=", "prompt"]));
        assert_eq!(equals_empty.resume.as_deref(), Some(""));
        assert_eq!(equals_empty.prompt.as_deref(), Some("prompt"));

        let unknown = parse_cli_config(&argv(&["--resume-later"]));
        assert_eq!(unknown.resume, None);
        assert_eq!(unknown.unknown_flags, vec!["--resume-later"]);
    }

    #[test]
    fn parse_disable_slash_commands_is_live_launch_input() {
        let config = parse_cli_config(&argv(&["--disable-slash-commands"]));
        assert!(config.disable_slash_commands);
        assert!(config.unimplemented_reasons().is_empty());
    }

    #[test]
    fn parse_no_session_persistence_matches_commander_negated_option() {
        let config = parse_cli_config(&argv(&["--no-session-persistence"]));
        assert_eq!(config.session_persistence, Some(false));
    }

    #[test]
    fn parse_debug_filter_and_debug_file() {
        let c = parse_cli_config(&argv(&["--debug=api,hooks"]));
        assert!(c.debug);
        assert_eq!(c.debug_filter.as_deref(), Some("api,hooks"));
        assert!(!c.debug_to_stderr);

        let c = parse_cli_config(&argv(&["-d", "mcp", "--debug-file", "/tmp/x.log"]));
        assert!(c.debug);
        assert_eq!(c.debug_filter.as_deref(), Some("mcp"));
        assert_eq!(
            c.debug_file.as_deref(),
            Some(std::path::Path::new("/tmp/x.log"))
        );

        let c = parse_cli_config(&argv(&["-d2e"]));
        assert!(c.debug);
        assert!(c.debug_to_stderr);
    }

    #[test]
    fn parse_variadic_mcp_configs_and_strict_flag_like_commander() {
        let config = parse_cli_config(&argv(&[
            "--mcp-config",
            "one.json",
            "{\"mcpServers\":{}}",
            "--strict-mcp-config",
        ]));
        assert_eq!(config.mcp_config, vec!["one.json", "{\"mcpServers\":{}}"]);
        assert!(config.strict_mcp_config);
        assert!(config.unimplemented_reasons().is_empty());
    }

    #[test]
    fn parse_subcommand_mcp() {
        let c = parse_cli_config(&argv(&["mcp", "list"]));
        assert_eq!(c.subcommand.as_deref(), Some("mcp"));
        assert!(!c.unimplemented_reasons().is_empty());
    }

    #[test]
    fn parse_unknown_flag() {
        let c = parse_cli_config(&argv(&["--not-a-real-flag"]));
        assert_eq!(c.unknown_flags, vec!["--not-a-real-flag".to_string()]);
    }

    /// Claude in Chrome is not shipped, so its flags are not accepted.
    #[test]
    fn parse_rejects_claude_in_chrome_flags() {
        let c = parse_cli_config(&argv(&["--chrome", "--no-chrome", "--chrome-native-host"]));
        assert_eq!(
            c.unknown_flags,
            vec!["--chrome", "--no-chrome", "--chrome-native-host"]
        );
        assert!(!c.unimplemented_reasons().is_empty());
    }
}
