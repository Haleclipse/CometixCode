//! Maps to: Node's `os` built-in, for the two functions CC calls from it
//! directly (55 files), with no wrapper of its own: `homedir()` and
//! `tmpdir()`. Call sites call these in place, as CC calls `os`.
//!
//! Neither is its Rust standard library counterpart. Both read `process.env`,
//! which here is the carrier, while the standard library reads the OS
//! environment. `std::env::home_dir()` also skips an empty `HOME`, which Node
//! returns. `std::env::temp_dir()` ignores `TMP` and `TEMP`, returns an empty
//! `TMPDIR` as is, keeps a trailing slash, and on macOS falls back to the
//! per-user `/var/folders/…/T/` instead of `/tmp`.

use std::path::PathBuf;

use crate::utils::process_env::{self, EnvSnapshot, JsTruthy};

/// Maps to: Node `lib/os.js` `homedir()`, libuv `uv_os_homedir`: `HOME`
/// (Windows: `USERPROFILE`) whenever it is set, an empty string included;
/// otherwise the account's home directory. Reads the current environment, as
/// libuv's `getenv` sees `process.env` writes.
///
/// The account lookup is the standard library's, reached because a variable
/// missing from the carrier is missing from the OS environment too: the
/// carrier starts from it, and CC never removes `HOME` or `USERPROFILE`.
/// Residuals: the standard library looks up the real user (`getuid`), libuv
/// the effective one (`geteuid`). Node throws where the lookup fails, and on
/// Windows for a `USERPROFILE` shorter than three characters; this returns an
/// empty path and the variable as is.
pub fn homedir() -> PathBuf {
    homedir_in(&process_env::snapshot())
}

fn homedir_in(env: &EnvSnapshot) -> PathBuf {
    let key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    env.var_os(key)
        .map(PathBuf::from)
        .or_else(std::env::home_dir)
        .unwrap_or_default()
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
    let bytes = dir.as_encoded_bytes();
    if bytes.len() > 1 && bytes.ends_with(b"/") {
        // SAFETY: dropping a trailing ASCII byte keeps the encoding valid.
        return PathBuf::from(unsafe {
            std::ffi::OsStr::from_encoded_bytes_unchecked(&bytes[..bytes.len() - 1])
        });
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

    /// Node v24 on macOS: `HOME` whenever set, an empty one included, and
    /// never `USERPROFILE`.
    #[test]
    fn homedir_takes_home_even_when_empty() {
        let home =
            |pairs: &[(&str, &str)]| homedir_in(&EnvSnapshot::from_pairs(pairs.iter().copied()));
        assert_eq!(
            home(&[("HOME", "/home/someone"), ("USERPROFILE", "/elsewhere")]),
            PathBuf::from("/home/someone")
        );
        assert_eq!(home(&[("HOME", "")]), PathBuf::new());
        assert_ne!(
            home(&[("USERPROFILE", "/elsewhere")]),
            PathBuf::from("/elsewhere")
        );
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
