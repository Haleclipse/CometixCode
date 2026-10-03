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
    is_env_truthy(process_env::var("CLAUDE_CODE_SIMPLE").as_deref())
        || std::env::args_os().any(|argument| argument == std::ffi::OsStr::new("--bare"))
}

/// Maps to: CC `utils/envUtils.ts:96-98` `getAWSRegion`.
pub fn get_aws_region() -> String {
    let env = process_env::snapshot();
    env.var("AWS_REGION")
        .truthy()
        .or_else(|| env.var("AWS_DEFAULT_REGION").truthy())
        .unwrap_or("us-east-1")
        .to_string()
}

/// Maps to: CC `utils/envUtils.ts:103-105` `getDefaultVertexRegion`.
pub fn get_default_vertex_region() -> String {
    process_env::var("CLOUD_ML_REGION")
        .truthy()
        .unwrap_or_else(|| "us-east5".to_string())
}

/// Maps to: CC `utils/envUtils.ts:111-113` `shouldMaintainProjectWorkingDir`.
pub fn should_maintain_project_working_dir() -> bool {
    is_env_truthy(process_env::var("CLAUDE_BASH_MAINTAIN_PROJECT_WORKING_DIR").as_deref())
}

/// Maps to: CC `utils/envUtils.ts#getVertexRegionForModel` and its ordered
/// `VERTEX_REGION_OVERRIDES` table. More-specific prefixes must stay first.
pub fn get_vertex_region_for_model(model: Option<&str>) -> String {
    const OVERRIDES: &[(&str, &str)] = &[
        ("claude-haiku-4-5", "VERTEX_REGION_CLAUDE_HAIKU_4_5"),
        ("claude-3-5-haiku", "VERTEX_REGION_CLAUDE_3_5_HAIKU"),
        ("claude-3-5-sonnet", "VERTEX_REGION_CLAUDE_3_5_SONNET"),
        ("claude-3-7-sonnet", "VERTEX_REGION_CLAUDE_3_7_SONNET"),
        ("claude-opus-4-1", "VERTEX_REGION_CLAUDE_4_1_OPUS"),
        ("claude-opus-4", "VERTEX_REGION_CLAUDE_4_0_OPUS"),
        ("claude-sonnet-4-6", "VERTEX_REGION_CLAUDE_4_6_SONNET"),
        ("claude-sonnet-4-5", "VERTEX_REGION_CLAUDE_4_5_SONNET"),
        ("claude-sonnet-4", "VERTEX_REGION_CLAUDE_4_0_SONNET"),
    ];
    if let Some((_, variable)) = model.and_then(|model| {
        OVERRIDES
            .iter()
            .find(|(prefix, _)| model.starts_with(prefix))
    }) {
        if let Some(region) = process_env::var(variable).truthy() {
            return region;
        }
    }
    get_default_vertex_region()
}

/// Repository-wide mutation gate, independent of transcript persistence.
/// Production writes unless explicitly disabled; tests must explicitly opt in.
///
/// Rust-only, and so not part of CC's `envUtils.ts`; moving it out of this
/// file is a later cleanup (environment redesign §12).
pub fn is_cometix_write_enabled() -> bool {
    #[cfg(test)]
    {
        is_env_truthy(crate::utils::process_env::var("COMETIX_WRITE_ENABLED").as_deref())
    }
    #[cfg(not(test))]
    {
        !is_env_defined_falsy(crate::utils::process_env::var("COMETIX_WRITE_ENABLED").as_deref())
    }
}

/// Maps to: CC `utils/envUtils.ts:7-14` `getClaudeConfigHomeDir`:
/// `(process.env.CLAUDE_CONFIG_DIR ?? join(homedir(), '.claude'))
/// .normalize('NFC')`, memoized keyed on `CLAUDE_CONFIG_DIR`. `??` keeps an
/// empty value. Each value of the variable is computed once, so with it unset
/// the first `homedir()` answer stays, as lodash `memoize` keeps it under the
/// `undefined` key.
pub fn get_claude_config_home_dir() -> PathBuf {
    static CACHE: std::sync::LazyLock<
        std::sync::Mutex<std::collections::HashMap<Option<std::ffi::OsString>, PathBuf>>,
    > = std::sync::LazyLock::new(Default::default);
    let key = process_env::var_os("CLAUDE_CONFIG_DIR");
    let mut cache = CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    cache
        .entry(key)
        .or_insert_with_key(|key| {
            let dir = match key {
                Some(dir) => PathBuf::from(dir),
                None => crate::utils::node_os::homedir().join(".claude"),
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
    get_claude_config_home_dir().join("teams")
}

/// Pure audience-injected form of CC `utils/envUtils.ts#isRunningOnHomespace`.
pub fn is_running_on_homespace_for_audience(
    get_env: &impl Fn(&str) -> Option<String>,
    audience: crate::utils::build_profile::BuildAudience,
) -> bool {
    crate::utils::build_profile::audience_has_internal_capability(
        audience,
        crate::utils::build_profile::InternalCapability::ManagedConfiguration,
    ) && is_env_truthy(get_env("COO_RUNNING_ON_HOMESPACE").as_deref())
}

/// Maps to: CC `utils/envUtils.ts:114-123` `isRunningOnHomespace`.
pub fn is_running_on_homespace() -> bool {
    is_running_on_homespace_for_audience(
        &|key| process_env::var(key),
        crate::utils::build_profile::build_audience(),
    )
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

    /// CC `envUtils.ts:7-14`: `??` keeps an empty `CLAUDE_CONFIG_DIR`, the
    /// result is NFC, and otherwise it is under `homedir()`. Memoized keyed
    /// on `CLAUDE_CONFIG_DIR`: with it unset, a later home directory is not
    /// seen.
    #[test]
    fn config_home_dir_matches_official_nullish_nfc_homedir_and_memo() {
        use std::path::PathBuf;
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _home = EnvVarGuard::set(HOME_VAR, "/home/first");
        {
            let _dir = EnvVarGuard::set("CLAUDE_CONFIG_DIR", "");
            assert_eq!(super::get_claude_config_home_dir(), PathBuf::from(""));
        }
        {
            let _dir = EnvVarGuard::set("CLAUDE_CONFIG_DIR", "/tmp/cafe\u{301}");
            assert_eq!(
                super::get_claude_config_home_dir(),
                PathBuf::from("/tmp/caf\u{e9}")
            );
        }
        let _dir = EnvVarGuard::unset("CLAUDE_CONFIG_DIR");
        let first = PathBuf::from("/home/first").join(".claude");
        assert_eq!(super::get_claude_config_home_dir(), first);
        let _later = EnvVarGuard::set(HOME_VAR, "/home/later");
        assert_eq!(super::get_claude_config_home_dir(), first);
    }

    #[test]
    fn homespace_detection_matches_official_internal_build_and_env_gate() {
        use crate::utils::build_profile::BuildAudience;

        assert!(!super::is_running_on_homespace_for_audience(
            &|_| None,
            BuildAudience::AnthropicInternal,
        ));
        assert!(!super::is_running_on_homespace_for_audience(
            &|key| (key == "COO_RUNNING_ON_HOMESPACE").then(|| "1".to_string()),
            BuildAudience::External,
        ));
        assert!(super::is_running_on_homespace_for_audience(
            &|key| (key == "COO_RUNNING_ON_HOMESPACE").then(|| "true".to_string()),
            BuildAudience::AnthropicInternal,
        ));
    }

    #[test]
    fn homespace_detection_matches_official_canonical_process_environment() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _homespace = EnvVarGuard::unset("COO_RUNNING_ON_HOMESPACE");

        assert!(!super::is_running_on_homespace());
        crate::utils::process_env::set("COO_RUNNING_ON_HOMESPACE", " true ");
        assert_eq!(
            super::is_running_on_homespace(),
            crate::utils::build_profile::build_audience().is_internal()
        );
        crate::utils::process_env::set("COO_RUNNING_ON_HOMESPACE", "off");
        assert!(!super::is_running_on_homespace());
    }
}
