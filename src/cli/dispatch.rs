//! Dispatch parsed [`CliConfig`] — Live continues; Unimplemented exits 1.

use super::config::CliConfig;
use crate::constants::product::{NAME, VERSION};

/// Apply Live side effects that must run before interactive launch (bare env).
pub fn apply_live_env_side_effects(config: &CliConfig) {
    if config.bare {
        crate::utils::process_env::set("NYAN_SIMPLE", "1");
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
        println!("{NAME} {VERSION}");
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

    if config.subcommand.as_deref() == Some("doctor") {
        return Some(run_doctor_cli());
    }

    if config.subcommand.as_deref() == Some("update") {
        return Some(run_update_cli());
    }

    // Maps to CC `cli/print.ts`: print/SDK mode owns the query lifetime and
    // structured stdout protocol instead of entering the retained TUI.
    if config.print {
        return Some(crate::cli::print::run(config));
    }

    // Interactive prompt-only without -p is Partial: allowed (typed later in REPL).
    None
}

/// `nyan update`: download and install the newest GitHub Release asset.
fn run_update_cli() -> i32 {
    // The trusted settings env carries the proxy and CA variables the
    // download must honour, applied as `main::run` does before any request.
    crate::utils::managed_env::apply_safe_config_environment_variables();
    crate::utils::ca_certs_config::apply_extra_ca_certs_from_config();
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("Update failed: {error}");
            return 1;
        }
    };
    match runtime.block_on(crate::utils::auto_updater::run_update()) {
        Ok(outcome) if outcome.is_success() => {
            println!("{}", outcome.message());
            0
        }
        Ok(outcome) => {
            eprintln!("{}", outcome.message());
            1
        }
        Err(error) => {
            eprintln!("Update failed: {error}");
            1
        }
    }
}

fn run_doctor_cli() -> i32 {
    let diagnostic = crate::utils::doctor_diagnostic::get_doctor_diagnostic();
    println!("Diagnostics");
    println!(
        "└ Currently running: {} ({})",
        diagnostic.installation_type.as_str(),
        diagnostic.version
    );
    println!("└ Path: {}", diagnostic.installation_path);
    println!("└ Invoked: {}", diagnostic.invoked_binary);

    println!("\nModel Profiles");
    let settings = crate::utils::settings::load_settings_from_disk();
    let profile_diagnoses = settings
        .settings
        .nyan
        .as_deref()
        .map(crate::utils::model::profile::diagnose_profiles)
        .unwrap_or_default();

    if profile_diagnoses.is_empty() {
        println!("└ No model profiles configured");
    } else {
        for diag in profile_diagnoses {
            println!(
                "└ Profile {:?} ({}/{}):",
                diag.profile_name, diag.provider, diag.model
            );
            for field in diag.fields {
                let source_label = if field.is_override {
                    "(profile override)"
                } else {
                    "(builtin snapshot)"
                };
                println!("  └ {}: {} {}", field.field_name, field.value, source_label);
            }
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::parse::parse_cli_config;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    fn argv(args: &[&str]) -> Vec<String> {
        std::iter::once("nyan".to_string())
            .chain(args.iter().map(|a| (*a).to_string()))
            .collect()
    }

    #[test]
    fn dispatch_version_exits_zero() {
        let c = parse_cli_config(&argv(&["-v"]));
        assert_eq!(dispatch(&c), Some(0));
    }

    #[test]
    fn dispatch_doctor_subcommand_exits_zero() {
        let c = parse_cli_config(&argv(&["doctor"]));
        assert_eq!(dispatch(&c), Some(0));
    }

    #[test]
    fn update_subcommand_is_live_not_unimplemented() {
        let c = parse_cli_config(&argv(&["update"]));
        assert!(c.unimplemented_reasons().is_empty());
        assert_eq!(c.subcommand.as_deref(), Some("update"));
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
        let _simple = EnvVarGuard::unset("NYAN_SIMPLE");
        let c = parse_cli_config(&argv(&["--bare"]));
        assert_eq!(dispatch(&c), None);
        assert!(crate::utils::env_utils::is_env_truthy(
            crate::utils::process_env::var("NYAN_SIMPLE").as_deref()
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
