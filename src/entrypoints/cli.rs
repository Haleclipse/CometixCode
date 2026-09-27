//! Maps to: CC `entrypoints/cli.tsx`.
//!
//! Bootstrap entrypoint — truly zero-dep `--version` remains here for the
//! historical fast-path; remaining flags parse through [`crate::cli`]
//! `CliConfig` then dispatch (Unimplemented short-circuit or [`crate::main::run`]).

use crate::cli::{dispatch, parse_cli_config};
use crate::constants::product::VERSION;

/// Maps to: CC `entrypoints/cli.tsx` top-level env side effects.
#[allow(clippy::disallowed_methods)] // Windows harden below is a sanctioned real-env write.
fn apply_cli_bootstrap_env() {
    // CC sets `process.env.NoDefaultCurrentDirectoryInExePath = "1"` first
    // thing at every entrypoint: it hardens Windows executable resolution
    // (CreateProcess stops searching the CWD) and is inherited by children on
    // every platform. The real-OS write is what the current Windows process's
    // own spawns honor; Rust documents `set_var` as always sound on Windows.
    // The carrier write covers child inheritance on all platforms.
    #[cfg(windows)]
    // SAFETY: `std::env::set_var` is documented as safe to call on Windows.
    unsafe {
        std::env::set_var("NoDefaultCurrentDirectoryInExePath", "1");
    }
    crate::utils::process_env::set("NoDefaultCurrentDirectoryInExePath", "1");

    // COREPACK_ENABLE_AUTO_PIN — Node-only; no-op on Rust binary.
    if crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_REMOTE")
            .ok()
            .as_deref(),
    ) {
        let _ = crate::utils::process_env::var("NODE_OPTIONS");
    }
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
