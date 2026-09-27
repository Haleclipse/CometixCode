//! Maps to: CC `utils/getWorktreePaths.ts`.
//!
//! Read-only synchronous Rust projection of `getWorktreePaths(cwd)`. CC's
//! implementation is async because its git helpers are async; callers keep the
//! same ownership and ordering while this local process call returns directly.

/// Maps to: CC `utils/getWorktreePaths.ts#getWorktreePaths`.
pub fn get_worktree_paths(cwd: &str) -> Vec<String> {
    let Ok(output) = crate::utils::subprocess_env::command("git")
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
    let mut paths = stdout
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .map(str::to_string)
        .collect::<Vec<_>>();
    if paths.is_empty() {
        return paths;
    }

    let current = paths
        .iter()
        .find(|path| path_is_or_under(cwd, path))
        .cloned();
    paths.sort();
    if let Some(current) = current {
        paths.retain(|path| path != &current);
        paths.insert(0, current);
    }
    paths
}

fn path_is_or_under(path: &str, base: &str) -> bool {
    path == base || std::path::Path::new(path).starts_with(std::path::Path::new(base))
}
