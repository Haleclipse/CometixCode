//! Maps to: CC `utils/which.ts:58-82` (the Bun runtime branch).
//! Native filesystem calls carry Bun.which; they never execute the candidate.
//! Node's execa fallback is not selected in the user-specified Bun baseline.
//! Windows executable-extension lookup remains an explicit unported platform.

use std::path::{Path, PathBuf};

/// Maps to: CC `utils/which.ts:71-73#which`, Bun-selected async lambda.
/// Ready preserves Bun.which's synchronous lookup at invocation, before the
/// caller receives its Promise. No deferred lookup or worker is introduced.
pub fn which(command: &str) -> std::future::Ready<Option<PathBuf>> {
    std::future::ready(bun_which(command))
}

/// Maps to: CC `utils/which.ts:81-82#whichSync`, Bun-selected alias.
pub fn which_sync(command: &str) -> Option<PathBuf> {
    bun_which(command)
}

// Native Bun.which boundary selected by CC which.ts:58-61. This is a runtime
// adapter, not another CC-defined function. PATH is the native startup input;
// changing the JS process.env object does not change Bun 1.3.14's lookup PATH.
// Preserve PATH spelling, symlinks, relative entries and empty-entry omission.
#[cfg(unix)]
fn bun_which(command: &str) -> Option<PathBuf> {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    if command.is_empty() {
        return None;
    }
    let executable = |candidate: &Path| {
        // Bun's native syscall sees the NUL-terminated prefix, but its returned
        // JS string retains the full candidate. JSON/file consumers own any
        // later path validation; this lookup must not invent one.
        let raw = candidate.as_os_str().as_bytes();
        let raw = raw.split(|byte| *byte == 0).next().unwrap_or_default();
        let Ok(c_path) = std::ffi::CString::new(raw) else {
            return false;
        };
        // SAFETY: c_path is alive, NUL-terminated, and access does not retain it.
        let allowed = unsafe { libc::access(c_path.as_ptr(), libc::X_OK) } == 0;
        allowed
            && std::fs::metadata(Path::new(std::ffi::OsStr::from_bytes(raw)))
                .is_ok_and(|metadata| metadata.is_file())
    };
    let command_path = Path::new(command);
    if command_path.is_absolute() {
        return executable(command_path).then(|| command_path.to_owned());
    }
    if command.contains('/') {
        let cwd = std::env::current_dir().ok()?;
        let mut resolved = cwd.as_os_str().as_bytes().to_vec();
        resolved.push(b'/');
        resolved.extend_from_slice(command.strip_prefix("./").unwrap_or(command).as_bytes());
        let resolved = PathBuf::from(std::ffi::OsString::from_vec(resolved));
        return executable(&resolved).then_some(resolved);
    }
    let startup = crate::utils::process_env::startup_snapshot();
    let path = startup.var_os("PATH")?;
    for directory in path
        .as_bytes()
        .split(|byte| *byte == b':')
        .filter(|s| !s.is_empty())
    {
        let mut candidate = directory.to_vec();
        candidate.push(b'/');
        candidate.extend_from_slice(command.as_bytes());
        let candidate = PathBuf::from(std::ffi::OsString::from_vec(candidate));
        if executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

#[cfg(not(unix))]
fn bun_which(_command: &str) -> Option<PathBuf> {
    // CC delegates native Windows extension/search rules to Bun. This batch
    // implements the macOS/Linux branch only, without guessing PATHEXT policy.
    None
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::process::Command;

    fn native_lookup_fresh_process_probe() {
        let Some(command) = crate::utils::process_env::var("COMETIX_WHICH_PROBE_COMMAND") else {
            return;
        };
        crate::utils::process_env::capture_startup();
        let expected = crate::utils::process_env::var("COMETIX_WHICH_PROBE_RESULT");
        if let Some(replacement) = crate::utils::process_env::var("COMETIX_WHICH_PROBE_REPLACE_PATH") {
            crate::utils::process_env::set("PATH", replacement);
        }
        let result = which_sync(&command).map(|p| p.to_string_lossy().into_owned());
        assert_eq!(result, expected);
        assert_eq!(
            futures::executor::block_on(which(&command)).map(|p| p.to_string_lossy().into_owned()),
            expected
        );
    }

    #[test]
    fn native_lookup_matches_official_bun_path_order_spelling_and_startup_snapshot() {
        if crate::utils::process_env::var_os("COMETIX_WHICH_PROBE_COMMAND").is_some() {
            native_lookup_fresh_process_probe();
            return;
        }
        // CC which.ts:58-82; real Bun source fresh-process oracle (29 cases),
        // research/proof/plugin-discover-data-0914/which-fresh.{py,json}.
        let root = std::env::temp_dir().join(format!("cometix-which-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("one/directory")).unwrap();
        std::fs::create_dir_all(root.join("two")).unwrap();
        for path in ["one/probe", "two/probe", "one/not-executable", "probe"] {
            let file = root.join(path);
            std::fs::write(&file, "#!/bin/sh\ntouch executed\n").unwrap();
            std::fs::set_permissions(
                &file,
                std::fs::Permissions::from_mode(if path.ends_with("not-executable") {
                    0o644
                } else {
                    0o755
                }),
            )
            .unwrap();
        }
        symlink("probe", root.join("one/link-probe")).unwrap();
        symlink("missing", root.join("one/broken")).unwrap();
        let cases = [
            (Some("one:two"), "probe", Some("one/probe")),
            (Some("two:one"), "probe", Some("two/probe")),
            (Some(""), "probe", None),
            (Some(":one"), "probe", Some("one/probe")),
            (Some("one:"), "probe", Some("one/probe")),
            (Some("one/"), "probe", Some("one//probe")),
            (Some("one//"), "probe", Some("one///probe")),
            (Some(" one"), "probe", None),
            (None, "probe", None),
            (Some("one"), "directory", None),
            (Some("one"), "not-executable", None),
            (Some("one"), "link-probe", Some("one/link-probe")),
            (Some("one"), "broken", None),
            (Some("one/../two"), "probe", Some("one/../two/probe")),
            (Some("."), "probe", Some("./probe")),
            (Some("one"), "", None),
        ];
        let run = |path: Option<&str>,
                   command: &str,
                   expected: Option<&str>,
                   replacement: Option<&str>| {
            let mut child = Command::new(std::env::current_exe().unwrap());
            child.args(["--exact", "utils::which::tests::native_lookup_matches_official_bun_path_order_spelling_and_startup_snapshot", "--nocapture"])
                .current_dir(&root).env("COMETIX_WHICH_PROBE_COMMAND", command)
                .env_remove("COMETIX_WHICH_PROBE_RESULT").env_remove("COMETIX_WHICH_PROBE_REPLACE_PATH").env_remove("PATH");
            if let Some(path) = path {
                child.env("PATH", path);
            }
            if let Some(expected) = expected {
                child.env("COMETIX_WHICH_PROBE_RESULT", expected);
            }
            if let Some(replacement) = replacement {
                child.env("COMETIX_WHICH_PROBE_REPLACE_PATH", replacement);
            }
            child
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());
            let mut child = child.spawn().unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
            loop {
                if child.try_wait().unwrap().is_some() {
                    break;
                }
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("native which probe timed out: {path:?}/{command:?}");
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            let output = child.wait_with_output().unwrap();
            assert!(
                output.status.success(),
                "{path:?}/{command:?}: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        };
        for (path, command, expected) in cases {
            run(path, command, expected, None);
        }
        run(Some("one"), "probe", Some("one/probe"), Some("two"));
        let absolute = root.join("one/probe");
        run(
            Some("two"),
            absolute.to_str().unwrap(),
            absolute.to_str(),
            None,
        );
        let resolved = root.canonicalize().unwrap().join("one/probe");
        run(Some("two"), "./one/probe", resolved.to_str(), None);
        run(Some("two"), "one/probe", resolved.to_str(), None);
        run(root.join("one").to_str(), "probe", absolute.to_str(), None);
        for command in [
            "one//probe",
            "one/./probe",
            "././one/probe",
            "one/../two/probe",
        ] {
            let expected = format!(
                "{}/{}",
                root.canonicalize().unwrap().display(),
                command.strip_prefix("./").unwrap_or(command)
            );
            run(Some("two"), command, Some(&expected), None);
        }
        assert!(!root.join("executed").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn async_lookup_matches_official_bun_eager_call_before_await() {
        let file =
            std::env::temp_dir().join(format!("cometix-which-eager-{}", uuid::Uuid::new_v4()));
        std::fs::write(&file, "#!/bin/sh\nexit 77\n").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        let nul_command = format!("{}\0suffix", file.display());
        assert_eq!(
            which_sync(&nul_command).unwrap().as_os_str(),
            std::ffi::OsStr::new(&nul_command)
        );
        let pending = which(file.to_str().unwrap());
        std::fs::remove_file(&file).unwrap();
        assert_eq!(futures::executor::block_on(pending), Some(file.clone()));
        assert_eq!(which_sync(file.to_str().unwrap()), None);
    }
}
