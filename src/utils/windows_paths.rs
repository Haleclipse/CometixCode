//! Windows ↔ Git-Bash path conversion and shell discovery.
//!
//! Maps to: CC `utils/windowsPaths.ts:84-179` for the Bash execution path.

use crate::utils::process_env::JsTruthy;
use std::path::{Path, PathBuf};

/// Maps to CC `windowsPathToPosixPath`.
pub fn windows_path_to_posix_path(path: &str) -> String {
    if path.starts_with("\\\\") {
        return path.replace('\\', "/");
    }
    let bytes = path.as_bytes();
    if bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\')
    {
        let drive = (bytes[0] as char).to_ascii_lowercase();
        return format!("/{drive}{}", path[2..].replace('\\', "/"));
    }
    path.replace('\\', "/")
}

/// Maps to CC `posixPathToWindowsPath`.
pub fn posix_path_to_windows_path(path: &str) -> String {
    if path.starts_with("//") {
        return path.replace('/', "\\");
    }
    if let Some(rest) = path.strip_prefix("/cygdrive/") {
        let mut chars = rest.chars();
        if let Some(drive) = chars.next().filter(char::is_ascii_alphabetic) {
            let rest = chars.as_str();
            if rest.is_empty() || rest.starts_with('/') {
                let suffix = if rest.is_empty() { "\\" } else { rest };
                return format!(
                    "{}:{}",
                    drive.to_ascii_uppercase(),
                    suffix.replace('/', "\\")
                );
            }
        }
    }
    let bytes = path.as_bytes();
    if bytes.len() >= 2
        && bytes[0] == b'/'
        && bytes[1].is_ascii_alphabetic()
        && (bytes.len() == 2 || bytes[2] == b'/')
    {
        let drive = (bytes[1] as char).to_ascii_uppercase();
        let rest = &path[2..];
        let suffix = if rest.is_empty() { "\\" } else { rest };
        return format!("{drive}:{}", suffix.replace('/', "\\"));
    }
    path.replace('/', "\\")
}

fn executable_exists(path: &Path) -> bool {
    path.is_file()
}

/// Maps to CC `findGitBashPath()` without process exit: callers surface the
/// official installation message as an execution error.
pub fn find_git_bash_path() -> Result<PathBuf, String> {
    if let Some(path) = crate::utils::process_env::var_os("CLAUDE_CODE_GIT_BASH_PATH")
        .as_deref()
        .truthy()
    {
        let path = PathBuf::from(path);
        if executable_exists(&path) {
            return Ok(path);
        }
        return Err(format!(
            "Claude Code was unable to find CLAUDE_CODE_GIT_BASH_PATH path \"{}\"",
            path.display()
        ));
    }

    for git in [
        r"C:\Program Files\Git\cmd\git.exe",
        r"C:\Program Files (x86)\Git\cmd\git.exe",
    ] {
        let git = PathBuf::from(git);
        if executable_exists(&git) {
            if let Some(root) = git.parent().and_then(Path::parent) {
                let bash = root.join("bin").join("bash.exe");
                if executable_exists(&bash) {
                    return Ok(bash);
                }
            }
        }
    }

    let mut where_exe = std::process::Command::new("where.exe");
    // CC `execSync_DEPRECATED('where.exe ...')` inherits process.env; the carrier
    // is its counterpart (where.exe searches the carrier PATH).
    crate::utils::subprocess_env::apply_process_env_std(&mut where_exe);
    if let Ok(output) = where_exe.arg("git").output() {
        let cwd = std::env::current_dir().ok();
        for candidate in String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
        {
            let git = PathBuf::from(candidate);
            if cwd.as_ref().is_some_and(|cwd| {
                git.parent().is_some_and(|parent| parent == cwd) || git.starts_with(cwd)
            }) {
                continue;
            }
            if let Some(root) = git.parent().and_then(Path::parent) {
                let bash = root.join("bin").join("bash.exe");
                if executable_exists(&bash) {
                    return Ok(bash);
                }
            }
        }
    }

    Err("Claude Code on Windows requires git-bash (https://git-scm.com/downloads/win). If installed but not in PATH, set environment variable pointing to your bash.exe, similar to: CLAUDE_CODE_GIT_BASH_PATH=C:\\Program Files\\Git\\bin\\bash.exe".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_and_posix_path_conversions_match_official_examples() {
        assert_eq!(windows_path_to_posix_path(r"C:\Users\foo"), "/c/Users/foo");
        assert_eq!(
            windows_path_to_posix_path(r"\\server\share"),
            "//server/share"
        );
        assert_eq!(posix_path_to_windows_path("/c/Users/foo"), r"C:\Users\foo");
        assert_eq!(posix_path_to_windows_path("/cygdrive/d/work"), r"D:\work");
    }
}
