//! Maps to: CC `utils/getWorktreePathsPortable.ts`.
//!
//! Synchronous Rust projection of `getWorktreePathsPortable(cwd)`. Unlike
//! `get_worktree_paths` (CC `getWorktreePaths`), it runs a bare `git` that the
//! child resolves through the inherited `process.env.PATH` rather than
//! `gitExe()`, and keeps git's own order — no sorting, no current-first move.
//!
//! Deviation: CC's `execFile` options `timeout: 5000` and the default 1 MiB
//! `maxBuffer` are not enforced; this projection waits for git to exit.

use std::process::Command;

use unicode_normalization::UnicodeNormalization;

/// Maps to: CC `utils/getWorktreePathsPortable.ts:12-27` `getWorktreePathsPortable`.
pub fn get_worktree_paths_portable(cwd: &str) -> Vec<String> {
    let mut command = Command::new("git");
    // CC's execFile inherits process.env; the carrier is its counterpart.
    crate::utils::subprocess_env::apply_process_env_std(&mut command);
    // A spawn failure or non-zero exit rejects execFile, which the catch maps to [].
    let Ok(output) = command
        .args(["worktree", "list", "--porcelain"])
        .current_dir(cwd)
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    if stdout.is_empty() {
        return Vec::new();
    }
    stdout
        .split('\n')
        .filter_map(|line| line.strip_prefix("worktree "))
        .map(|path| path.nfc().collect())
        .collect()
}
