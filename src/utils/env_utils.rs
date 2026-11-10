use std::path::PathBuf;

use crate::utils::process_env::{self, JsTruthy};

/// Maps to: CC `utils/envUtils.ts:24-30` `hasNodeOption`: `flag` is one of
/// the `NODE_OPTIONS` words split on `/\s+/` (JS whitespace).
pub fn has_node_option(flag: &str) -> bool {
    process_env::var("NODE_OPTIONS")
        .truthy()
        .is_some_and(|options| {
            options
                .split(|c: char| (c.is_whitespace() && c != '\u{85}') || c == '\u{feff}')
                .any(|word| word == flag)
        })
}

/// Maps to: CC `utils/envUtils.ts:32-37` `isEnvTruthy`.
/// Rust's environment carrier supplies the source string/undefined cases; no
/// current Rust caller requires the TypeScript-only boolean union branch.
pub fn is_env_truthy(env_var: Option<&str>) -> bool {
    env_var.is_some_and(|value| {
        matches!(
            value.to_ascii_lowercase().trim(),
            "1" | "true" | "yes" | "on"
        )
    })
}

/// Maps to: CC `utils/envUtils.ts:39-47` `isEnvDefinedFalsy`.
pub fn is_env_defined_falsy(env_var: Option<&str>) -> bool {
    env_var
        .filter(|value| !value.is_empty())
        .is_some_and(|value| {
            matches!(
                value.to_ascii_lowercase().trim(),
                "0" | "false" | "no" | "off"
            )
        })
}

/// Maps to: CC `utils/envUtils.ts:60-65` `isBareMode`.
pub fn is_bare_mode() -> bool {
    is_env_truthy(process_env::var("NYAN_SIMPLE").as_deref())
        || std::env::args_os().any(|argument| argument == std::ffi::OsStr::new("--bare"))
}

/// Maps to: CC `utils/envUtils.ts:111-113` `shouldMaintainProjectWorkingDir`.
pub fn should_maintain_project_working_dir() -> bool {
    is_env_truthy(process_env::var("NYAN_BASH_MAINTAIN_PROJECT_WORKING_DIR").as_deref())
}

/// Repository-wide mutation gate, independent of transcript persistence.
/// Production writes unless explicitly disabled; tests must explicitly opt in.
///
/// Rust-only, and so not part of CC's `envUtils.ts`; moving it out of this
/// file is a later cleanup (environment redesign §12).
pub fn is_nyan_write_enabled() -> bool {
    #[cfg(test)]
    {
        is_env_truthy(crate::utils::process_env::var("NYAN_WRITE_ENABLED").as_deref())
    }
    #[cfg(not(test))]
    {
        !is_env_defined_falsy(crate::utils::process_env::var("NYAN_WRITE_ENABLED").as_deref())
    }
}

/// Maps to: CC `utils/envUtils.ts:7-14` `getClaudeConfigHomeDir`:
/// `(process.env.NYAN_CONFIG_DIR ?? join(homedir(), '.nyan'))
/// .normalize('NFC')`, memoized keyed on `NYAN_CONFIG_DIR`. `??` keeps an
/// empty value. Each value of the variable is computed once, so with it unset
/// the first `homedir()` answer stays, as lodash `memoize` keeps it under the
/// `undefined` key.
///
/// @nyan: the variable is `NYAN_CONFIG_DIR` (CC: `CLAUDE_CONFIG_DIR`), so a
/// nyan started from a Claude Code session does not adopt its directory.
pub fn get_config_home_dir() -> PathBuf {
    static CACHE: std::sync::LazyLock<
        std::sync::Mutex<std::collections::HashMap<Option<std::ffi::OsString>, PathBuf>>,
    > = std::sync::LazyLock::new(Default::default);
    let key = process_env::var_os("NYAN_CONFIG_DIR");
    let mut cache = CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    cache
        .entry(key)
        .or_insert_with_key(|key| {
            let dir = match key {
                Some(dir) => PathBuf::from(dir),
                None => crate::utils::node_os::homedir().join(".nyan"),
            };
            match dir.to_str() {
                Some(dir) => {
                    use unicode_normalization::UnicodeNormalization as _;
                    PathBuf::from(dir.nfc().collect::<String>())
                }
                None => dir,
            }
        })
        .clone()
}

/// Maps to: CC `utils/envUtils.ts#getTeamsDir`.
pub fn get_teams_dir() -> PathBuf {
    get_config_home_dir().join("teams")
}

#[cfg(test)]
mod tests {
    use crate::utils::test_env::{EnvVarGuard, HOME_VAR, TEST_ENV_LOCK};

    #[test]
    fn env_value_predicates_match_official_whitespace_and_undefined_semantics() {
        assert!(super::is_env_truthy(Some(" TRUE ")));
        assert!(!super::is_env_truthy(Some("0")));
        assert!(!super::is_env_truthy(Some("")));
        assert!(!super::is_env_truthy(None));
        assert!(super::is_env_defined_falsy(Some(" off ")));
        assert!(!super::is_env_defined_falsy(Some("")));
        assert!(!super::is_env_defined_falsy(None));
    }

    /// CC `envUtils.ts:7-14`: `??` keeps an empty `NYAN_CONFIG_DIR`, the
    /// result is NFC, and otherwise it is under `homedir()`. Memoized keyed
    /// on `NYAN_CONFIG_DIR`: with it unset, a later home directory is not
    /// seen.
    #[test]
    fn config_home_dir_matches_official_nullish_nfc_homedir_and_memo() {
        use std::path::PathBuf;
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _home = EnvVarGuard::set(HOME_VAR, "/home/first");
        {
            let _dir = EnvVarGuard::set("NYAN_CONFIG_DIR", "");
            assert_eq!(super::get_config_home_dir(), PathBuf::from(""));
        }
        {
            let _dir = EnvVarGuard::set("NYAN_CONFIG_DIR", "/tmp/cafe\u{301}");
            assert_eq!(
                super::get_config_home_dir(),
                PathBuf::from("/tmp/caf\u{e9}")
            );
        }
        let _dir = EnvVarGuard::unset("NYAN_CONFIG_DIR");
        let first = PathBuf::from("/home/first").join(".nyan");
        assert_eq!(super::get_config_home_dir(), first);
        let _later = EnvVarGuard::set(HOME_VAR, "/home/later");
        assert_eq!(super::get_config_home_dir(), first);
    }
}
