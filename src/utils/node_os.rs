//! Maps to: Node's `os` built-in, for the two functions CC calls from it
//! directly (55 files), with no wrapper of its own: `homedir()` and
//! `tmpdir()`. Call sites call these in place, as CC calls `os`.
//!
//! Neither is its Rust standard library counterpart. Both read `process.env`,
//! which here is the carrier, while the standard library reads the OS
//! environment. `std::env::home_dir()` also gives `None` where Node throws,
//! and on Unix looks up the real user (`getuid`) with a single call.
//! `std::env::temp_dir()` ignores `TMP` and `TEMP`, returns an empty `TMPDIR`
//! as is, keeps a trailing slash, and on macOS falls back to the per-user
//! `/var/folders/…/T/` instead of `/tmp`.

use std::path::PathBuf;

use crate::utils::process_env::{self, EnvSnapshot, JsTruthy};

/// Maps to: `os.homedir()` as CC's native build runs it on Unix (Bun,
/// `src/runtime/node/node_os.rs`), and as Node and Bun both run it on Windows
/// (libuv `uv_os_homedir`): `HOME` (Windows: `USERPROFILE`) when set, else the
/// account's home directory. On Unix an empty `HOME` counts as unset, as in
/// Bun, where Node returns it, and a value of any length is returned, where
/// Node's buffer throws `ENOBUFS`. The environment is the current one, as
/// Node's `getenv` sees `process.env` writes; Bun reads `HOME` as the process
/// started.
///
/// # Panics
///
/// With Node's message, where Node and Bun throw `uv_os_homedir returned
/// ENOENT`: the variable is unset (Unix: or empty) and the account lookup
/// fails, or on Windows `USERPROFILE` is shorter than three bytes. Nothing
/// catches it on the path where CC first calls `os.homedir()` (env-paths, at
/// import), so CC fails at startup; the startup window forces
/// `cache_paths::CACHE_ROOT`, so for the startup environment this fails there
/// too, before any UI. Not ported: the errno both report for other lookup
/// errors, and Node's `ENOBUFS` on Windows.
pub fn homedir() -> PathBuf {
    homedir_in(&process_env::snapshot()).unwrap_or_else(|| {
        panic!("A system error occurred: uv_os_homedir returned ENOENT (no such file or directory)")
    })
}

fn homedir_in(env: &EnvSnapshot) -> Option<PathBuf> {
    if cfg!(windows) {
        // libuv's, which Bun calls on Windows too: a `USERPROFILE` shorter
        // than three bytes is an error, not a reason to fall back.
        return match env.var_os("USERPROFILE") {
            Some(dir) if dir.len() < 3 => None,
            Some(dir) => Some(PathBuf::from(dir)),
            None => account_home_dir(),
        };
    }
    env.var_os("HOME")
        .truthy()
        .map(PathBuf::from)
        .or_else(account_home_dir)
}

/// The effective user's home directory, as libuv's `uv__getpwuid_r` looks it
/// up: `getpwuid_r(geteuid())` from a 2000-byte buffer, doubled on `ERANGE`
/// and retried on `EINTR`. Bun does the same from 4096 bytes. A null
/// `pw_dir`, which Bun returns as an empty string, counts as a failed lookup
/// here, so it never becomes a relative home directory.
#[cfg(unix)]
fn account_home_dir() -> Option<PathBuf> {
    account_home_dir_from(2000)
}

/// [`account_home_dir`] from a given buffer size, which tests set small to
/// take the `ERANGE` path.
#[cfg(unix)]
fn account_home_dir_from(initial_size: usize) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt as _;
    let mut size = initial_size.max(1);
    loop {
        let mut passwd = std::mem::MaybeUninit::<libc::passwd>::uninit();
        let mut result = std::ptr::null_mut();
        let mut buffer = vec![0 as libc::c_char; size];
        let status = loop {
            // SAFETY: `passwd` and `buffer` remain alive for the call, and
            // `result` is read only after it returns.
            let status = unsafe {
                libc::getpwuid_r(
                    libc::geteuid(),
                    passwd.as_mut_ptr(),
                    buffer.as_mut_ptr(),
                    buffer.len(),
                    &mut result,
                )
            };
            if status != libc::EINTR {
                break status;
            }
        };
        if status == libc::ERANGE {
            size *= 2;
            continue;
        }
        if status != 0 || result.is_null() {
            return None;
        }
        // SAFETY: a successful lookup points `result` at `passwd`, whose
        // `pw_dir`, when not null, is a NUL-terminated string in `buffer`.
        let dir = unsafe { (*result).pw_dir };
        if dir.is_null() {
            return None;
        }
        // SAFETY: as above.
        let dir = unsafe { std::ffi::CStr::from_ptr(dir) };
        return Some(PathBuf::from(std::ffi::OsStr::from_bytes(dir.to_bytes())));
    }
}

/// libuv's Windows lookup is `GetUserProfileDirectoryW` for the process
/// token, which the standard library's fallback calls. It reads the OS
/// `USERPROFILE` first, which is absent here: the carrier starts from the OS
/// environment, lacks it, and CC never removes it, so the startup publication
/// leaves the OS without it too.
#[cfg(windows)]
#[allow(clippy::disallowed_methods)] // The account lookup behind libuv's fallback.
fn account_home_dir() -> Option<PathBuf> {
    std::env::home_dir()
}

/// Maps to: Node `lib/os.js` `tmpdir()`. Reads the current environment, as
/// Node's reads `process.env`.
pub fn tmpdir() -> PathBuf {
    tmpdir_in(&process_env::snapshot())
}

fn tmpdir_in(env: &EnvSnapshot) -> PathBuf {
    let var = |key: &str| env.var_os(key).truthy();
    if cfg!(windows) {
        // `process.env.TEMP || process.env.TMP ||
        //  (process.env.SystemRoot || process.env.windir) + '\\temp'`, without
        // a trailing backslash unless it is a drive root (`C:\`).
        let path = match var("TEMP").or_else(|| var("TMP")) {
            Some(path) => path.to_string_lossy().into_owned(),
            None => {
                let root = var("SystemRoot").or_else(|| var("windir"));
                // JS string concatenation renders a missing root as "undefined".
                let root = root.map_or_else(|| "undefined".into(), |root| root.to_string_lossy());
                format!("{root}\\temp")
            }
        };
        if path.len() > 1 && path.ends_with('\\') && !path.ends_with(":\\") {
            return PathBuf::from(&path[..path.len() - 1]);
        }
        return PathBuf::from(path);
    }
    // `getTempDir() || '/tmp'`: the first non-empty of TMPDIR, TMP and TEMP,
    // without one trailing slash.
    let Some(dir) = var("TMPDIR").or_else(|| var("TMP")).or_else(|| var("TEMP")) else {
        return PathBuf::from("/tmp");
    };
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        let bytes = dir.as_bytes();
        if bytes.len() > 1 && bytes.ends_with(b"/") {
            return PathBuf::from(std::ffi::OsStr::from_bytes(&bytes[..bytes.len() - 1]));
        }
    }
    PathBuf::from(dir)
}

// Unix only: the Windows branches are picked by `cfg!(windows)`, a compile-time
// constant, and no gate builds for Windows.
#[cfg(all(test, not(windows)))]
mod tests {
    use super::*;

    fn tmpdir_with(pairs: &[(&str, &str)]) -> PathBuf {
        tmpdir_in(&EnvSnapshot::from_pairs(pairs.iter().copied()))
    }

    /// Bun on macOS: `HOME` when set and non-empty, never `USERPROFILE`;
    /// otherwise the effective user's passwd entry.
    #[test]
    fn homedir_takes_a_non_empty_home_else_the_account() {
        let home =
            |pairs: &[(&str, &str)]| homedir_in(&EnvSnapshot::from_pairs(pairs.iter().copied()));
        assert_eq!(
            home(&[("HOME", "/home/someone"), ("USERPROFILE", "/elsewhere")]),
            Some(PathBuf::from("/home/someone"))
        );
        assert_eq!(home(&[("HOME", "")]), account_home_dir());
        assert_eq!(home(&[("USERPROFILE", "/elsewhere")]), account_home_dir());
    }

    /// libuv's `ERANGE` loop: a buffer too small for the entry doubles until
    /// it fits, and the answer is the one a large first buffer gets.
    #[test]
    fn account_lookup_grows_a_small_buffer() {
        assert_eq!(account_home_dir_from(1), account_home_dir_from(1 << 16));
    }

    /// Node v24 on macOS: TMPDIR, then TMP, then TEMP, each only when
    /// non-empty, else `/tmp`; one trailing slash dropped, `/` kept.
    #[test]
    fn tmpdir_matches_node_order_fallback_and_trailing_slash() {
        assert_eq!(tmpdir_with(&[]), PathBuf::from("/tmp"));
        assert_eq!(
            tmpdir_with(&[("TMPDIR", "/a"), ("TMP", "/b")]),
            PathBuf::from("/a")
        );
        assert_eq!(
            tmpdir_with(&[("TMPDIR", ""), ("TMP", "/b")]),
            PathBuf::from("/b")
        );
        assert_eq!(
            tmpdir_with(&[("TMP", "/b"), ("TEMP", "/c")]),
            PathBuf::from("/b")
        );
        assert_eq!(tmpdir_with(&[("TEMP", "/c")]), PathBuf::from("/c"));
        assert_eq!(tmpdir_with(&[("TMPDIR", "/a/")]), PathBuf::from("/a"));
        assert_eq!(tmpdir_with(&[("TMPDIR", "/")]), PathBuf::from("/"));
    }
}
