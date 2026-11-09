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

pub fn git_bash_discovery_source() -> &'static str {
    if crate::utils::process_env::var_os("NYAN_GIT_BASH_PATH")
        .as_deref()
        .truthy()
        .is_some()
    {
        "NYAN_GIT_BASH_PATH"
    } else {
        "automatic discovery"
    }
}

/// Normalize raw NYAN_GIT_BASH_PATH value, handling quotes, POSIX
/// paths (`/c/...`, `//server/...`), Cygwin paths (`/cygdrive/...`), and UNC paths.
pub fn normalize_git_bash_path_str(raw: &str) -> PathBuf {
    let trimmed = raw.trim().trim_matches(['"', '\'']);
    let converted = posix_path_to_windows_path(trimmed);
    PathBuf::from(converted)
}

/// Validate that a path exists, is an executable binary, and is bash.
pub fn validate_git_bash_executable(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Err(format!(
            "NYAN_GIT_BASH_PATH file does not exist: \"{}\"",
            path.display()
        ));
    }
    if path.is_dir() {
        return Err(format!(
            "NYAN_GIT_BASH_PATH is a directory, not a file: \"{}\"",
            path.display()
        ));
    }
    #[cfg(windows)]
    {
        let is_exe = path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"));
        if !is_exe {
            return Err(format!(
                "NYAN_GIT_BASH_PATH is not an executable file (.exe): \"{}\"",
                path.display()
            ));
        }
    }
    let file_stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("");
    if !file_stem.eq_ignore_ascii_case("bash") {
        return Err(format!(
            "NYAN_GIT_BASH_PATH does not point to bash (expected bash.exe, got \"{}\")",
            path.display()
        ));
    }

    #[cfg(windows)]
    {
        let mut command = std::process::Command::new(path);
        crate::utils::subprocess_env::apply_process_env_std(&mut command);
        command.arg("--version");
        command.stdin(std::process::Stdio::null());
        command.stdout(std::process::Stdio::piped());
        command.stderr(std::process::Stdio::piped());
        match command.output() {
            Ok(output) => {
                if !output.status.success() {
                    return Err(format!(
                        "NYAN_GIT_BASH_PATH at \"{}\" exited with failure code when run with --version",
                        path.display()
                    ));
                }
                let stdout = String::from_utf8_lossy(&output.stdout);
                if !stdout.to_ascii_lowercase().contains("bash") {
                    return Err(format!(
                        "NYAN_GIT_BASH_PATH at \"{}\" is not GNU bash",
                        path.display()
                    ));
                }
            }
            Err(err) => {
                return Err(format!(
                    "NYAN_GIT_BASH_PATH at \"{}\" failed to execute: {err}",
                    path.display()
                ));
            }
        }
    }

    Ok(())
}

/// Maps to CC `findGitBashPath()` without process exit: callers surface the
/// official installation message as an execution error.
pub fn find_git_bash_path() -> Result<PathBuf, String> {
    if let Some(raw_os) = crate::utils::process_env::var_os("NYAN_GIT_BASH_PATH")
        .as_deref()
        .truthy()
    {
        let raw_str = raw_os.to_string_lossy();
        let mut path = normalize_git_bash_path_str(&raw_str);
        if !path.is_file() && path.extension().is_none() {
            let with_exe = path.with_extension("exe");
            if with_exe.is_file() {
                path = with_exe;
            }
        }
        validate_git_bash_executable(&path)?;
        return Ok(path);
    }

    for git in [
        r"C:\Program Files\Git\cmd\git.exe",
        r"C:\Program Files (x86)\Git\cmd\git.exe",
    ] {
        let git = PathBuf::from(git);
        if executable_exists(&git)
            && let Some(root) = git.parent().and_then(Path::parent)
        {
            let bash = root.join("bin").join("bash.exe");
            if executable_exists(&bash) {
                return Ok(bash);
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

    Err("nyan on Windows requires Git for Windows (Git Bash). Install it from https://git-scm.com/downloads/win, or set NYAN_GIT_BASH_PATH to your bash.exe path (e.g. C:\\Program Files\\Git\\bin\\bash.exe).".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[test]
    fn git_bash_discovery_source_identifies_override_or_automatic_lookup() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _override = EnvVarGuard::unset("NYAN_GIT_BASH_PATH");
        assert_eq!(git_bash_discovery_source(), "automatic discovery");
        let _override = EnvVarGuard::set("NYAN_GIT_BASH_PATH", r"C:\Git\bin\bash.exe");
        assert_eq!(git_bash_discovery_source(), "NYAN_GIT_BASH_PATH");
    }

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

    #[test]
    fn normalize_git_bash_path_handles_drives_unc_and_posix() {
        assert_eq!(
            normalize_git_bash_path_str(r"C:\Program Files\Git\bin\bash.exe"),
            PathBuf::from(r"C:\Program Files\Git\bin\bash.exe")
        );
        assert_eq!(
            normalize_git_bash_path_str("C:/Program Files/Git/bin/bash.exe"),
            PathBuf::from(r"C:\Program Files\Git\bin\bash.exe")
        );
        assert_eq!(
            normalize_git_bash_path_str("/c/Program Files/Git/bin/bash.exe"),
            PathBuf::from(r"C:\Program Files\Git\bin\bash.exe")
        );
        assert_eq!(
            normalize_git_bash_path_str("/cygdrive/d/Git/bin/bash.exe"),
            PathBuf::from(r"D:\Git\bin\bash.exe")
        );
        assert_eq!(
            normalize_git_bash_path_str(r"\\server\share\Git\bin\bash.exe"),
            PathBuf::from(r"\\server\share\Git\bin\bash.exe")
        );
        assert_eq!(
            normalize_git_bash_path_str("//server/share/Git/bin/bash.exe"),
            PathBuf::from(r"\\server\share\Git\bin\bash.exe")
        );
        assert_eq!(
            normalize_git_bash_path_str("\"C:\\Program Files\\Git\\bin\\bash.exe\""),
            PathBuf::from(r"C:\Program Files\Git\bin\bash.exe")
        );
    }

    #[test]
    fn validate_git_bash_executable_reports_clear_errors() {
        let nonexistent = Path::new("C:/nonexistent/path/bash.exe");
        let err = validate_git_bash_executable(nonexistent).unwrap_err();
        assert!(err.contains("does not exist"), "unexpected error: {err}");

        let temp_dir = crate::utils::node_os::tmpdir().join(format!(
            "nyan-bash-validate-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();
        let err = validate_git_bash_executable(&temp_dir).unwrap_err();
        assert!(err.contains("is a directory"), "unexpected error: {err}");

        let not_bash = temp_dir.join("sh.exe");
        std::fs::write(&not_bash, b"mock").unwrap();
        let err = validate_git_bash_executable(&not_bash).unwrap_err();
        assert!(
            err.contains("does not point to bash"),
            "unexpected error: {err}"
        );
        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[cfg(windows)]
    #[test]
    fn windows_smoke_path_conversion_covers_drive_and_unc() {
        assert_eq!(windows_path_to_posix_path(r"E:\work\repo"), "/e/work/repo");
        assert_eq!(
            windows_path_to_posix_path(r"\\server\share\repo"),
            "//server/share/repo"
        );
        assert_eq!(posix_path_to_windows_path("/e/work/repo"), r"E:\work\repo");
        assert_eq!(
            posix_path_to_windows_path("//server/share/repo"),
            r"\\server\share\repo"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_smoke_git_bash_executes_basic_command() {
        let bash = find_git_bash_path().unwrap_or_else(|error| {
            panic!("Git Bash is unavailable on the Windows runner: {error}")
        });
        let run = crate::utils::shell::run_command_streaming(
            &bash.display().to_string(),
            &["-lc", "printf 'nyan-windows-git-bash-smoke\\n'"],
            std::time::Duration::from_secs(5),
            None,
            &crate::tool::AbortController::default(),
            None,
        )
        .unwrap_or_else(|error| panic!("Git Bash smoke command failed to start: {error}"));
        assert!(!run.timed_out, "Git Bash smoke command timed out");
        assert!(
            run.output.status.success(),
            "Git Bash smoke command failed: {}",
            String::from_utf8_lossy(&run.output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&run.output.stdout).trim(),
            "nyan-windows-git-bash-smoke"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_smoke_nyan_git_bash_path_runtime_validation() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let discovered = find_git_bash_path().unwrap_or_else(|error| {
            panic!("Git Bash is unavailable on the Windows runner: {error}")
        });

        // 1. Exact path override
        {
            let _override =
                EnvVarGuard::set("NYAN_GIT_BASH_PATH", discovered.display().to_string());
            assert_eq!(git_bash_discovery_source(), "NYAN_GIT_BASH_PATH");
            let found = find_git_bash_path().expect("valid NYAN_GIT_BASH_PATH should resolve");
            assert_eq!(found, discovered);
        }

        // 2. POSIX path conversion override
        {
            let posix = windows_path_to_posix_path(&discovered.display().to_string());
            let _override = EnvVarGuard::set("NYAN_GIT_BASH_PATH", posix);
            let found = find_git_bash_path().expect("POSIX NYAN_GIT_BASH_PATH should resolve");
            assert_eq!(found, discovered);
        }

        // 3. Forward-slash path override
        {
            let forward = discovered.display().to_string().replace('\\', "/");
            let _override = EnvVarGuard::set("NYAN_GIT_BASH_PATH", forward);
            let found =
                find_git_bash_path().expect("Forward-slash NYAN_GIT_BASH_PATH should resolve");
            assert_eq!(found, discovered);
        }

        // 4. Invalid nonexistent path override
        {
            let _override =
                EnvVarGuard::set("NYAN_GIT_BASH_PATH", r"C:\nonexistent\tools\bash.exe");
            let err = find_git_bash_path().unwrap_err();
            assert!(err.contains("does not exist"), "unexpected error: {err}");
        }

        // 5. Invalid directory path override
        {
            let _override = EnvVarGuard::set(
                "NYAN_GIT_BASH_PATH",
                discovered.parent().unwrap().display().to_string(),
            );
            let err = find_git_bash_path().unwrap_err();
            assert!(err.contains("is a directory"), "unexpected error: {err}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_smoke_git_worktree_bash_and_path_conversion() {
        let bash = find_git_bash_path().unwrap_or_else(|error| {
            panic!("Git Bash is unavailable on the Windows runner: {error}")
        });
        let temp_dir = crate::utils::node_os::tmpdir().join(format!(
            "nyan-worktree-smoke-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let _ = std::fs::remove_dir_all(&temp_dir);
        let main_repo = temp_dir.join("main_repo");
        std::fs::create_dir_all(&main_repo).expect("create main repo directory");

        let run_cmd = |dir: &Path, program: &str, args: &[&str]| {
            let output = std::process::Command::new(program)
                .args(args)
                .current_dir(dir)
                .output()
                .unwrap_or_else(|e| panic!("Failed to run {program} {args:?}: {e}"));
            assert!(
                output.status.success(),
                "{program} {:?} failed: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        };

        run_cmd(&main_repo, "git", &["init"]);
        run_cmd(
            &main_repo,
            "git",
            &["config", "user.email", "smoke@example.com"],
        );
        run_cmd(&main_repo, "git", &["config", "user.name", "Smoke Test"]);
        let initial_file = main_repo.join("README.md");
        std::fs::write(&initial_file, "# Smoke Test").expect("write initial file");
        run_cmd(&main_repo, "git", &["add", "README.md"]);
        run_cmd(&main_repo, "git", &["commit", "-m", "initial commit"]);

        let worktree_dir = temp_dir.join("worktree_feature");
        run_cmd(
            &main_repo,
            "git",
            &[
                "worktree",
                "add",
                "-b",
                "feature-worktree",
                worktree_dir.to_str().unwrap(),
            ],
        );
        assert!(worktree_dir.is_dir(), "worktree directory was created");

        let worktree_str = worktree_dir.display().to_string();
        let posix_worktree = windows_path_to_posix_path(&worktree_str);
        assert!(
            posix_worktree.starts_with('/'),
            "POSIX worktree path must start with /: {posix_worktree}"
        );
        let roundtrip_windows = posix_path_to_windows_path(&posix_worktree);
        assert!(
            roundtrip_windows.eq_ignore_ascii_case(&worktree_str),
            "Round-trip path mismatch: expected {worktree_str}, got {roundtrip_windows}"
        );

        let script =
            "pwd -P && git rev-parse --show-toplevel && printf 'worktree-bash-smoke-ok\\n'";
        let run = crate::utils::shell::run_command_streaming(
            &bash.display().to_string(),
            &["-lc", script],
            std::time::Duration::from_secs(10),
            Some(&worktree_dir),
            &crate::tool::AbortController::default(),
            None,
        )
        .unwrap_or_else(|error| panic!("Git Bash worktree smoke failed to start: {error}"));
        assert!(!run.timed_out, "Git Bash worktree smoke timed out");
        assert!(
            run.output.status.success(),
            "Git Bash worktree smoke failed: {}",
            String::from_utf8_lossy(&run.output.stderr)
        );

        let stdout = String::from_utf8_lossy(&run.output.stdout);
        let lines: Vec<&str> = stdout
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        assert!(
            lines.iter().any(|line| *line == "worktree-bash-smoke-ok"),
            "Expected success marker in worktree output: {stdout}"
        );

        let bash_cwd_posix = lines[0];
        let bash_cwd_windows = posix_path_to_windows_path(bash_cwd_posix);
        assert!(
            Path::new(&bash_cwd_windows).exists(),
            "Converted Windows path from Git Bash pwd must exist: {bash_cwd_windows}"
        );

        let git_toplevel_posix = lines[1];
        let git_toplevel_windows = posix_path_to_windows_path(git_toplevel_posix);
        assert!(
            Path::new(&git_toplevel_windows).exists(),
            "Converted Windows path from git rev-parse must exist: {git_toplevel_windows}"
        );

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
