//! Dispatch parsed [`CliConfig`] — Live continues; Unimplemented exits 1.

use super::config::CliConfig;
use crate::constants::product::VERSION;

/// Apply Live side effects that must run before interactive launch (bare env).
pub fn apply_live_env_side_effects(config: &CliConfig) {
    if config.bare {
        crate::utils::process_env::set("CLAUDE_CODE_SIMPLE", "1");
    }
}

/// Short-circuit Unimplemented reasons to stderr; returns exit code `1`.
pub fn exit_unimplemented(reasons: &[String]) -> i32 {
    if reasons.is_empty() {
        eprintln!("Unimplemented: unknown");
    } else if reasons.len() == 1 {
        eprintln!("Unimplemented: {}", reasons[0]);
    } else {
        eprintln!("Unimplemented: {}", reasons.join(", "));
    }
    1
}

/// Maps to: CC `entrypoints/cli.tsx` + `main.tsx` early dispatch after parse.
///
/// Returns `Some(exit_code)` when the process should exit without interactive
/// [`crate::main::run`]. Returns `None` to continue into interactive launch.
pub fn dispatch(config: &CliConfig) -> Option<i32> {
    apply_live_env_side_effects(config);

    if config.show_version {
        println!("{VERSION} (Claude Code)");
        return Some(0);
    }

    if config.show_help {
        CliConfig::print_help();
        return Some(0);
    }

    // Maps to CC `main.tsx` validation: this negated option is print-only.
    if config.session_persistence == Some(false) && !config.print {
        eprintln!("Error: --no-session-persistence can only be used with --print mode.");
        return Some(1);
    }

    // Preserve historical ant-only / feature-gated messaging where useful,
    // but unify on `Unimplemented:` for incomplete commander surface.
    let reasons = config.unimplemented_reasons();
    if !reasons.is_empty() {
        return Some(exit_unimplemented(&reasons));
    }

    // Maps to CC `cli/print.ts`: print/SDK mode owns the query lifetime and
    // structured stdout protocol instead of entering the retained TUI.
    if config.print {
        return Some(crate::cli::print::run(config));
    }

    // Interactive prompt-only without -p is Partial: allowed (typed later in REPL).
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::parse::parse_cli_config;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    fn argv(args: &[&str]) -> Vec<String> {
        std::iter::once("cometix".to_string())
            .chain(args.iter().map(|a| (*a).to_string()))
            .collect()
    }

    #[test]
    fn dispatch_version_exits_zero() {
        let c = parse_cli_config(&argv(&["-v"]));
        assert_eq!(dispatch(&c), Some(0));
    }

    #[test]
    fn dispatch_rejects_no_session_persistence_outside_print_mode() {
        let config = parse_cli_config(&argv(&["--no-session-persistence"]));
        assert_eq!(dispatch(&config), Some(1));
    }

    #[test]
    fn dispatch_interactive_default_continues() {
        let c = parse_cli_config(&argv(&[]));
        assert_eq!(dispatch(&c), None);
    }

    #[test]
    fn dispatch_bare_sets_env_and_continues() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _simple = EnvVarGuard::unset("CLAUDE_CODE_SIMPLE");
        let c = parse_cli_config(&argv(&["--bare"]));
        assert_eq!(dispatch(&c), None);
        assert!(crate::utils::env_utils::is_env_truthy(
            crate::utils::process_env::var("CLAUDE_CODE_SIMPLE").as_deref()
        ));
    }

    #[test]
    fn dispatch_model_and_permission_continue_interactive() {
        let c = parse_cli_config(&argv(&[
            "--model",
            "sonnet",
            "--effort",
            "high",
            "--verbose",
            "--permission-mode",
            "acceptEdits",
            "--dangerously-skip-permissions",
            "--allowedTools",
            "Read",
        ]));
        assert!(c.unimplemented_reasons().is_empty());
        assert_eq!(dispatch(&c), None);
        assert_eq!(c.model.as_deref(), Some("sonnet"));
        assert_eq!(c.effort.as_deref(), Some("high"));
        assert!(c.verbose);
        assert_eq!(c.permission_mode.as_deref(), Some("acceptEdits"));
        assert!(c.dangerously_skip_permissions);
        assert_eq!(c.allowed_tools, vec!["Read".to_string()]);
    }

    #[test]
    fn dispatch_thinking_is_live() {
        let c = parse_cli_config(&argv(&["--thinking", "enabled"]));
        assert!(c.unimplemented_reasons().is_empty());
        assert_eq!(dispatch(&c), None);
        assert_eq!(c.thinking.as_deref(), Some("enabled"));
    }

    #[test]
    fn dispatch_max_thinking_tokens_is_live() {
        let c = parse_cli_config(&argv(&["--max-thinking-tokens", "8000"]));
        assert!(c.unimplemented_reasons().is_empty());
        assert_eq!(c.max_thinking_tokens.as_deref(), Some("8000"));
        assert_eq!(dispatch(&c), None);
    }
}
