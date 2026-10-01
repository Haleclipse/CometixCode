use std::path::PathBuf;

use crate::utils::process_env::{self, JsTruthy};

/// Serialises the ~200 test modules that mutate process-wide state.
///
/// `lock()` keeps the `LockResult` shape so both established call forms
/// (`.unwrap()` and `.unwrap_or_else(PoisonError::into_inner)`) compile, but it
/// never yields `Err`. The mutex guards `()`, not data: callers pair it with
/// RAII guards that restore what they touched as the stack unwinds, so a
/// panicking test leaves nothing half-written behind the lock. Propagating the
/// poison instead converted one real panic into a `PoisonError` for every later
/// caller in the same process, which buried the failure that mattered under
/// hundreds of derived ones.
#[cfg(test)]
pub struct TestEnvLock(std::sync::Mutex<()>);

#[cfg(test)]
impl TestEnvLock {
    pub fn lock(&self) -> std::sync::LockResult<TestEnvGuard<'_>> {
        let guard = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reset_settings_derived_caches();
        Ok(TestEnvGuard(guard))
    }
}

/// Makes `settings_cache.rs:10-12`'s stated contract real: the caches derived
/// from settings are cleared on both edges of the critical section.
///
/// Neither cache is keyed — the merged-settings caches and the hooks-config
/// snapshot memoise a read of `CLAUDE_CONFIG_DIR` plus `get_original_cwd()`,
/// the exact state this lock exists to let a test move, and both populate
/// lazily on first read. A test that moved either and restored it left the
/// snapshot taken from its scratch directory behind for everyone after it, and
/// a test that moved neither pinned THIS repository's hooks for the rest of the
/// run. Clearing can never produce a wrong answer, only drop a stale one: the
/// next read re-derives from disk and the environment as they are then.
#[cfg(test)]
pub struct TestEnvGuard<'a>(#[allow(dead_code)] std::sync::MutexGuard<'a, ()>);

#[cfg(test)]
fn reset_settings_derived_caches() {
    crate::utils::settings::settings_cache::reset_settings_cache();
    crate::utils::hooks::hooks_config_snapshot::reset_hooks_config_snapshot();
}

#[cfg(test)]
impl Drop for TestEnvGuard<'_> {
    fn drop(&mut self) {
        reset_settings_derived_caches();
    }
}

#[cfg(test)]
pub static TEST_ENV_LOCK: std::sync::LazyLock<TestEnvLock> =
    std::sync::LazyLock::new(|| TestEnvLock(std::sync::Mutex::new(())));

/// The other half of `TEST_ENV_LOCK`'s bargain: the lock serialises the
/// mutations, this undoes them.
///
/// Writes go through the `process_env` carrier. Its temporary Unix bridge lets
/// migrated tests continue to exercise nearby raw readers until their owner
/// tasks move them; it is not a production environment-policy guarantee. The
/// full previous entry (stable insertion ordinal + spelling + value) is captured
/// so restore reproduces the exact prior state — including a non-canonical
/// Windows spelling and first-to-last aggregate drops of distinct-key guards —
/// and `Drop` keeps a panic from leaking the mutation into later tests. Guards
/// nested on the same key must retain normal LIFO ownership.
///
/// The lock stays necessary: the carrier is process-global logical state, so
/// parallel tests mutating the same key still race without serialisation.
#[cfg(test)]
pub struct EnvVarGuard {
    previous: Option<crate::utils::process_env::EnvEntryRestore>,
}

#[cfg(test)]
impl EnvVarGuard {
    /// Captures a full entry without changing it. Compound fixtures use this
    /// when their existing setup performs several writes after construction.
    pub fn preserve(key: &'static str) -> Self {
        Self {
            previous: Some(crate::utils::process_env::save_entry_for_restore(key)),
        }
    }

    pub fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let guard = Self::preserve(key);
        crate::utils::process_env::set(key, value);
        guard
    }

    pub fn unset(key: &'static str) -> Self {
        let guard = Self::preserve(key);
        crate::utils::process_env::remove(key);
        guard
    }
}

#[cfg(test)]
impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            crate::utils::process_env::restore_entry(previous);
        }
    }
}

/// The same poison-free contract for the other process-global test locks.
///
/// Each of those is an advisory `Mutex<()>` over module state that its tests
/// reset explicitly on entry, so a panic behind one leaves no half-written data
/// for `PoisonError` to protect. All it protects is the next test's ability to
/// report its own result: one timing-sensitive failure used to turn into a
/// `PoisonError` for every later test sharing the lock.
#[cfg(test)]
pub struct TestStateLock(std::sync::Mutex<()>);

#[cfg(test)]
impl TestStateLock {
    pub const fn new() -> Self {
        Self(std::sync::Mutex::new(()))
    }

    pub fn lock(&self) -> std::sync::LockResult<std::sync::MutexGuard<'_, ()>> {
        Ok(self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner))
    }
}

/// Pins `projectSettings` to the isolated test workdir for the duration of a test.
///
/// `just test` pins `CLAUDE_CONFIG_DIR`, which covers `userSettings` and
/// `~/.claude.json`. But settings merge from five sources and `projectSettings`
/// is `${cwd}/.claude/settings.json` (`utils/settings/mod.rs:1-8`), rooted at
/// `bootstrap::state::get_original_cwd()` — process state, not an env var, so no
/// recipe can pin it. Without this, a test that reads settings reads THIS
/// repository's `.claude/settings.json`, which configures hooks; anything that
/// then runs those hooks tries to spawn a process.
///
/// `tests/fixtures/isolated-project` is the tracked hook-free workdir used by
/// the clean-checkout test harness.
#[cfg(test)]
pub struct IsolatedProjectSettings(std::path::PathBuf);

#[cfg(test)]
impl IsolatedProjectSettings {
    pub fn pin() -> Self {
        let previous = crate::bootstrap::state::get_original_cwd();
        crate::bootstrap::state::set_original_cwd(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/isolated-project"),
        );
        Self(previous)
    }
}

#[cfg(test)]
impl Drop for IsolatedProjectSettings {
    fn drop(&mut self) {
        crate::bootstrap::state::set_original_cwd(&self.0);
    }
}

/// The inverse pin: the harness defaults `ORIGINAL_CWD` to the isolated
/// workdir (justfile `COMETIX_TEST_PROJECT_DIR`), so a test whose fixtures
/// live under THIS repository must now say so explicitly instead of
/// inheriting the process cwd by accident.
#[cfg(test)]
pub struct PinnedProjectDir(std::path::PathBuf);

#[cfg(test)]
impl PinnedProjectDir {
    pub fn at(dir: impl AsRef<std::path::Path>) -> Self {
        let previous = crate::bootstrap::state::get_original_cwd();
        crate::bootstrap::state::set_original_cwd(dir.as_ref());
        Self(previous)
    }

    /// Pin to the repository root — for tests that build fixtures from
    /// real in-repo paths.
    pub fn at_manifest_root() -> Self {
        Self::at(std::path::Path::new(env!("CARGO_MANIFEST_DIR")))
    }
}

#[cfg(test)]
impl Drop for PinnedProjectDir {
    fn drop(&mut self) {
        crate::bootstrap::state::set_original_cwd(&self.0);
    }
}

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
/// Rust-only; it moves to its callers with the other `COMETIX_*` reads in C5.
/// Until then it reads the OS environment, which `use_log_messages.rs`'s
/// tests write directly (environment redesign §11).
pub fn is_cometix_write_enabled() -> bool {
    #[cfg(test)]
    {
        is_env_truthy(std::env::var("COMETIX_WRITE_ENABLED").ok().as_deref())
    }
    #[cfg(not(test))]
    {
        !is_env_defined_falsy(std::env::var("COMETIX_WRITE_ENABLED").ok().as_deref())
    }
}

/// Maps to: CC `utils/envUtils.ts:7-14` `getClaudeConfigHomeDir`:
/// `(process.env.CLAUDE_CONFIG_DIR ?? join(homedir(), '.claude'))
/// .normalize('NFC')`. `??` keeps an empty `CLAUDE_CONFIG_DIR`.
///
/// Residual: CC memoizes keyed on `CLAUDE_CONFIG_DIR` alone, so with it unset
/// the first `homedir()` answer is kept even if `HOME` changes later. This
/// recomputes, following a later `HOME` (environment redesign §12, C5).
pub fn get_claude_config_home_dir() -> PathBuf {
    get_claude_config_home_dir_from_snapshot(&process_env::snapshot())
}

/// [`get_claude_config_home_dir`] against a given snapshot, for multi-key
/// operations that must not mix environment versions.
pub fn get_claude_config_home_dir_from_snapshot(env: &process_env::EnvSnapshot) -> PathBuf {
    claude_config_home_dir_with(|key| env.var_os(key).map(std::ffi::OsStr::to_os_string))
}

/// The one implementation of [`get_claude_config_home_dir`], over any
/// environment source: the carrier, a snapshot, or a loader's injected
/// `get_env`.
pub(crate) fn claude_config_home_dir_with(
    var_os: impl Fn(&str) -> Option<std::ffi::OsString>,
) -> PathBuf {
    let dir = match var_os("CLAUDE_CONFIG_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => homedir_with(&var_os).join(".claude"),
    };
    match dir.to_str() {
        Some(dir) => {
            use unicode_normalization::UnicodeNormalization as _;
            PathBuf::from(dir.nfc().collect::<String>())
        }
        None => dir,
    }
}

/// Node `os.homedir()` (libuv `uv_os_homedir`): `HOME` (Windows:
/// `USERPROFILE`) when set, even to an empty string; otherwise the account's
/// home directory, which `std::env::home_dir` returns once that variable is
/// unset. That fallback reads the OS environment: after C8 removes the Unix
/// write-through, a variable deleted only from the carrier would still be
/// seen there.
pub(crate) fn homedir_with(var_os: impl Fn(&str) -> Option<std::ffi::OsString>) -> PathBuf {
    let key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    var_os(key)
        .map(PathBuf::from)
        .or_else(std::env::home_dir)
        .unwrap_or_default()
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
    use super::EnvVarGuard;

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

    /// CC tests isolate writes to the ordered environment object used by
    /// `cli/structuredIO.ts:348-360`; restoration must recover that before-state
    /// across nesting and unwinding.
    #[test]
    fn env_var_guard_restores_absence_nested_lifo_and_panic_cleanup() {
        let _env_lock = super::TEST_ENV_LOCK.lock().unwrap();
        let key = "COMETIX_ENV_VAR_GUARD_LIFECYCLE";
        let _restore = EnvVarGuard::preserve(key);
        crate::utils::process_env::remove(key);

        {
            let _first = EnvVarGuard::set(key, "first");
            let panic = std::panic::catch_unwind(|| {
                let _panic = EnvVarGuard::set(key, "panic");
                panic!("guard cleanup");
            });
            assert!(panic.is_err());
            assert_eq!(
                crate::utils::process_env::snapshot().var(key),
                Some("first")
            );

            {
                let _second = EnvVarGuard::set(key, "second");
                assert_eq!(
                    crate::utils::process_env::snapshot().var(key),
                    Some("second")
                );
            }
            assert_eq!(
                crate::utils::process_env::snapshot().var(key),
                Some("first")
            );
        }
        assert_eq!(crate::utils::process_env::snapshot().var_os(key), None);
    }

    /// CC `cli/structuredIO.ts:348-360` mutates one ordered `process.env`;
    /// delete/re-add cleanup must restore its pre-test own-key order.
    #[test]
    fn env_var_guard_order_and_nested_unwind_match_official_process_env() {
        let _env_lock = super::TEST_ENV_LOCK.lock().unwrap();
        let keys = [
            "COMETIX_ENV_GUARD_ORDER_A",
            "COMETIX_ENV_GUARD_ORDER_B",
            "COMETIX_ENV_GUARD_ORDER_C",
        ];
        let _cleanup_a = EnvVarGuard::unset(keys[0]);
        let _cleanup_b = EnvVarGuard::unset(keys[1]);
        let _cleanup_c = EnvVarGuard::unset(keys[2]);
        for (key, value) in keys.iter().zip(["a", "before", "c"]) {
            crate::utils::process_env::set(key, value);
        }
        let selected_keys = || {
            crate::utils::process_env::snapshot()
                .iter()
                .map(|(key, _)| key.to_string_lossy().into_owned())
                .filter(|key| keys.contains(&key.as_str()))
                .collect::<Vec<_>>()
        };
        assert_eq!(selected_keys(), keys);

        {
            let _outer = EnvVarGuard::set(keys[1], "outer");
            let panic = std::panic::catch_unwind(|| {
                let _inner = EnvVarGuard::unset(keys[1]);
                crate::utils::process_env::set(keys[1], "tail");
                assert_eq!(selected_keys(), [keys[0], keys[2], keys[1]]);
                panic!("exercise ordered unwind cleanup");
            });
            assert!(panic.is_err());
            assert_eq!(selected_keys(), keys);
            assert_eq!(
                crate::utils::process_env::var(keys[1]).as_deref(),
                Some("outer")
            );
        }

        assert_eq!(selected_keys(), keys);
        assert_eq!(
            crate::utils::process_env::var(keys[1]).as_deref(),
            Some("before")
        );

        // Rust arrays drop elements first-to-last. The second guard therefore
        // cannot restore from an absolute index captured after A was removed.
        let aggregate = [EnvVarGuard::unset(keys[0]), EnvVarGuard::unset(keys[2])];
        assert_eq!(selected_keys(), [keys[1]]);
        drop(aggregate);
        assert_eq!(selected_keys(), keys);
    }

    /// Node v24 on Windows uses case-insensitive key identity while preserving
    /// active spelling and ordinary-key position; see process-env-carrier.md §1.2.
    #[cfg(windows)]
    #[test]
    fn env_var_guard_restore_matches_official_windows_process_env() {
        let _env_lock = super::TEST_ENV_LOCK.lock().unwrap();
        let key = "COMETIX_ENV_VAR_GUARD_WINDOWS";
        let neighbor = "COMETIX_ENV_VAR_GUARD_WINDOWS_NEIGHBOR";
        let _restore_key = EnvVarGuard::preserve(key);
        crate::utils::process_env::remove(key);
        let _restore_neighbor = EnvVarGuard::preserve(neighbor);
        crate::utils::process_env::remove(neighbor);
        crate::utils::process_env::set("Cometix_Env_Var_Guard_Windows", "before");
        crate::utils::process_env::set(neighbor, "neighbor");

        {
            let _changed = EnvVarGuard::unset(key);
            crate::utils::process_env::set(key, "during");
            let keys = crate::utils::process_env::snapshot()
                .iter()
                .map(|(key, _)| key.to_string_lossy().into_owned())
                .filter(|candidate| candidate.eq_ignore_ascii_case(key) || candidate == neighbor)
                .collect::<Vec<_>>();
            assert_eq!(keys, [neighbor, key]);
        }

        let snapshot = crate::utils::process_env::snapshot();
        let (spelling, value) = snapshot.entry(key).unwrap();
        assert_eq!(
            spelling,
            std::ffi::OsStr::new("Cometix_Env_Var_Guard_Windows")
        );
        assert_eq!(value, std::ffi::OsStr::new("before"));
        let keys = snapshot
            .iter()
            .map(|(key, _)| key.to_string_lossy().into_owned())
            .filter(|candidate| candidate.eq_ignore_ascii_case(key) || candidate == neighbor)
            .collect::<Vec<_>>();
        assert_eq!(keys, ["Cometix_Env_Var_Guard_Windows", neighbor]);
    }

    /// CC `envUtils.ts:7-14`: `??` keeps an empty `CLAUDE_CONFIG_DIR`, the
    /// result is NFC, and `homedir()` takes `HOME` (Windows: `USERPROFILE`).
    #[test]
    fn config_home_dir_matches_official_nullish_nfc_and_homedir() {
        use crate::utils::process_env::EnvSnapshot;
        use std::path::PathBuf;
        let dir = |pairs: &[(&str, &str)]| {
            super::get_claude_config_home_dir_from_snapshot(&EnvSnapshot::from_pairs(
                pairs.iter().copied(),
            ))
        };
        assert_eq!(dir(&[("CLAUDE_CONFIG_DIR", "")]), PathBuf::from(""));
        assert_eq!(
            dir(&[("CLAUDE_CONFIG_DIR", "/tmp/cafe\u{301}")]),
            PathBuf::from("/tmp/caf\u{e9}")
        );
        let home_key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        assert_eq!(
            dir(&[(home_key, "/home/someone")]),
            PathBuf::from("/home/someone").join(".claude")
        );
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
        let _env_lock = super::TEST_ENV_LOCK.lock().unwrap();
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
