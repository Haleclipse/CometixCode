//! Maps to: CC `entrypoints/cli.tsx`.
//!
//! Bootstrap entrypoint — truly zero-dep `--version` remains here for the
//! historical fast-path; remaining flags parse through [`crate::cli`]
//! `CliConfig` then dispatch (Unimplemented short-circuit or [`crate::main::run`]).

use crate::cli::{dispatch, parse_cli_config};
use crate::constants::product::VERSION;
use crate::utils::process_env::JsTruthy;

/// Maps to: CC `entrypoints/cli.tsx:1-16` top-level env side effects, and
/// `main.tsx:876-879` at the start of `main()`.
fn apply_cli_bootstrap_env() {
    // `cli.tsx:3-5`: corepack's auto-pinning adds yarnpkg to package.json
    // files; the commands Claude runs inherit this.
    crate::utils::process_env::set("COREPACK_ENABLE_AUTO_PIN", "0");
    // `cli.tsx:7-16`: a larger heap for child Node processes in CCR containers.
    if crate::utils::process_env::var("CLAUDE_CODE_REMOTE").as_deref() == Some("true") {
        // Node reads the value lossily decoded, as `process.env` holds it.
        let existing = crate::utils::process_env::var_os("NODE_OPTIONS")
            .map(|value| value.to_string_lossy().into_owned());
        let node_options = match existing.truthy() {
            Some(existing) => format!("{existing} --max-old-space-size=8192"),
            None => "--max-old-space-size=8192".to_owned(),
        };
        crate::utils::process_env::set("NODE_OPTIONS", node_options);
    }

    // CC sets `process.env.NoDefaultCurrentDirectoryInExePath = "1"` before
    // any command runs: it hardens Windows executable resolution
    // (CreateProcess stops searching the CWD) and is inherited by children on
    // every platform. The real-OS write is what the current Windows process's
    // own spawns honor; Rust documents `set_var` as always sound on Windows.
    // The carrier write covers child inheritance on all platforms.
    #[cfg(windows)]
    #[allow(clippy::disallowed_methods)] // The sanctioned Windows harden write.
    // SAFETY: `std::env::set_var` is documented as safe to call on Windows.
    unsafe {
        std::env::set_var("NoDefaultCurrentDirectoryInExePath", "1");
    }
    crate::utils::process_env::set("NoDefaultCurrentDirectoryInExePath", "1");
}

/// Maps to: CC's module evaluation when `cli.tsx` imports `main.js`. The
/// constants that modules in `main.js`'s static import graph compute at
/// import see the environment as it is after cli.tsx's own writes and before
/// `init()` applies settings env. Two ast-grep queries found them:
/// `process.env` reads outside any function, and module-level calls of a
/// function that reads it (`const X = f()`). Rust has no import time, so
/// each one is a `LazyLock` its module owns, forced here; afterwards none of
/// them reads the environment again.
///
/// `PowerShellTool.tsx` is not in that graph (`tools.ts:150-155` requires it
/// lazily), so its constant is forced after `init()` instead.
fn evaluate_import_time_constants() {
    // `env.ts:316-333`: `isCI`, `platform`, `terminal`.
    crate::utils::env::get();
    // `BashTool.tsx:332`, `AgentTool.tsx:145`.
    std::sync::LazyLock::force(&crate::tools::bash_tool::IS_BACKGROUND_TASKS_DISABLED);
    std::sync::LazyLock::force(&crate::tools::agent_tool::IS_BACKGROUND_TASKS_DISABLED);
    // `swarm/backends/detection.ts:10,19`.
    std::sync::LazyLock::force(&crate::utils::swarm::backends::detection::ORIGINAL_USER_TMUX);
    std::sync::LazyLock::force(&crate::utils::swarm::backends::detection::ORIGINAL_TMUX_PANE);
    // `Spinner.tsx:52`, `Spinner/SpinnerGlyph.tsx:11`: `getDefaultCharacters()`.
    std::sync::LazyLock::force(&crate::components::spinner::utils::DEFAULT_CHARACTERS);
    // `cachePaths.ts:6`: `envPaths('claude-cli')`.
    std::sync::LazyLock::force(&crate::utils::cache_paths::CACHE_ROOT);
    // npm `figures`: `shouldUseMain = isUnicodeSupported()`.
    std::sync::LazyLock::force(&crate::constants::figures::SHOULD_USE_MAIN);
    // chalk's import-time level detection and `ink/colorize.ts:61-62`'s
    // xterm.js boost and tmux clamp, which the chalk port applies on first
    // read.
    chalk::stdout_level();
    chalk::stderr_level();
}

/// Zero-dependency version fast-path (CC cli.tsx before importing main).
///
/// Returns `Some(0)` only for a lone `--version`/`-v`/`-V`. All other flags
/// go through [`parse_cli_config`] + [`dispatch`].
pub fn try_version_fast_path(args: &[String]) -> Option<i32> {
    if args.len() == 1 && matches!(args[0].as_str(), "--version" | "-v" | "-V") {
        println!("{VERSION} (Claude Code)");
        return Some(0);
    }
    None
}

/// Maps to: CC `entrypoints/cli.tsx` `void main()`.
pub fn run() {
    apply_cli_bootstrap_env();

    let argv: Vec<String> = std::env::args().collect();
    let args: Vec<String> = argv.iter().skip(1).cloned().collect();

    if let Some(code) = try_version_fast_path(&args) {
        crate::utils::cleanup_registry::exit_process(code);
    }

    evaluate_import_time_constants();

    // Maps to: CC `main.tsx:1104-1120`, which sits in `main()` between the
    // `claude ssh` argv rewrite and Commander's `.parse()`. `args` is
    // `process.argv.slice(2)`: Rust's `args()` puts the binary at [0] where
    // Node has [node, script], so `skip(1)` is the same slice.
    //
    // Position is load-bearing. CC computes this before `init()` so telemetry's
    // auth calls see it; here `parse_cli_config` + `dispatch` come next, and
    // `dispatch` hands `--print` to `cli/print.rs` without ever entering
    // `main::run`. Computing it any later would leave every headless session on
    // the seed value.
    crate::main::initialize_is_interactive(&args);

    let config = parse_cli_config(&argv);
    if let Some(code) = dispatch(&config) {
        crate::utils::cleanup_registry::exit_process(code);
    }

    crate::main::run(config);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    /// The import-time constants keep the startup window's environment: a
    /// later write, as settings env makes, does not reach them.
    #[test]
    fn import_time_constants_ignore_later_environment_writes() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _startup = [
            EnvVarGuard::unset("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS"),
            EnvVarGuard::unset("TMUX"),
            EnvVarGuard::unset("TMUX_PANE"),
            EnvVarGuard::set("TERM", "xterm-256color"),
        ];
        evaluate_import_time_constants();
        let terminal = crate::utils::env::get().terminal.clone();
        let spinner = crate::components::spinner::utils::spinner_frame(5);

        let _later = [
            EnvVarGuard::set("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS", "1"),
            EnvVarGuard::set("TMUX", "/tmp/tmux-501/default,1,0"),
            EnvVarGuard::set("TMUX_PANE", "%3"),
            EnvVarGuard::set("TERM", "xterm-ghostty"),
            EnvVarGuard::set("TERM_PROGRAM", "WezTerm"),
        ];
        assert!(!*crate::tools::bash_tool::IS_BACKGROUND_TASKS_DISABLED);
        assert!(!*crate::tools::agent_tool::IS_BACKGROUND_TASKS_DISABLED);
        assert!(!crate::utils::swarm::backends::detection::is_inside_tmux_sync());
        assert_eq!(
            crate::utils::swarm::backends::detection::get_leader_pane_id(),
            None
        );
        assert_eq!(crate::utils::env::get().terminal, terminal);
        // Ghostty's set would put `*` here.
        assert_eq!(crate::components::spinner::utils::spinner_frame(5), spinner);
        assert_ne!(spinner, "*");
    }

    /// `cli.tsx:5,9-16`: the corepack pin is always off; only a literal
    /// `CLAUDE_CODE_REMOTE=true` grows the children's Node heap, appended to
    /// a non-empty `NODE_OPTIONS`.
    #[test]
    fn bootstrap_env_matches_official_corepack_and_remote_heap_writes() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _guards = [
            EnvVarGuard::unset("COREPACK_ENABLE_AUTO_PIN"),
            EnvVarGuard::preserve("NoDefaultCurrentDirectoryInExePath"),
            EnvVarGuard::set("CLAUDE_CODE_REMOTE", "1"),
            EnvVarGuard::set("NODE_OPTIONS", "--trace-warnings"),
        ];
        let var = crate::utils::process_env::var;
        apply_cli_bootstrap_env();
        assert_eq!(var("COREPACK_ENABLE_AUTO_PIN").as_deref(), Some("0"));
        assert_eq!(var("NoDefaultCurrentDirectoryInExePath").as_deref(), Some("1"));
        assert_eq!(var("NODE_OPTIONS").as_deref(), Some("--trace-warnings"));

        crate::utils::process_env::set("CLAUDE_CODE_REMOTE", "true");
        apply_cli_bootstrap_env();
        assert_eq!(
            var("NODE_OPTIONS").as_deref(),
            Some("--trace-warnings --max-old-space-size=8192")
        );
        crate::utils::process_env::set("NODE_OPTIONS", "");
        apply_cli_bootstrap_env();
        assert_eq!(
            var("NODE_OPTIONS").as_deref(),
            Some("--max-old-space-size=8192")
        );
    }

    #[test]
    fn version_fast_path_matches_official_flag_set() {
        assert_eq!(try_version_fast_path(&["--version".into()]), Some(0));
        assert_eq!(try_version_fast_path(&["-v".into()]), Some(0));
        assert_eq!(try_version_fast_path(&["-V".into()]), Some(0));
        assert_eq!(try_version_fast_path(&["--help".into()]), None);
        assert_eq!(
            try_version_fast_path(&["--version".into(), "extra".into()]),
            None
        );
    }
}
