//! Test infrastructure for process-wide state: the environment lock, the
//! environment-variable guard, the pins for the project directory, and the
//! child-process re-run for code that reads the real OS environment.
//!
//! Rust-only, with no CC counterpart: this crate's unit tests share
//! process-global state (the `process_env` carrier, the settings caches, the
//! bootstrap state) and serialise on these locks. The module is compiled only
//! under `cfg(test)` (`utils/mod.rs`). It used to live at the top of
//! `env_utils.rs` (moved 2026-10-03), which maps to CC `utils/envUtils.ts`
//! and should hold only that file's functions.

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
pub struct TestEnvLock(std::sync::Mutex<()>);

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
pub struct TestEnvGuard<'a>(#[allow(dead_code)] std::sync::MutexGuard<'a, ()>);

fn reset_settings_derived_caches() {
    crate::utils::settings::settings_cache::reset_settings_cache();
    crate::utils::hooks::hooks_config_snapshot::reset_hooks_config_snapshot();
}

impl Drop for TestEnvGuard<'_> {
    fn drop(&mut self) {
        reset_settings_derived_caches();
    }
}

pub static TEST_ENV_LOCK: std::sync::LazyLock<TestEnvLock> =
    std::sync::LazyLock::new(|| TestEnvLock(std::sync::Mutex::new(())));

/// The variable `node_os::homedir()` reads, `USERPROFILE` on Windows and
/// `HOME` elsewhere: tests pin the home directory through it.
pub const HOME_VAR: &str = if cfg!(windows) { "USERPROFILE" } else { "HOME" };

/// The other half of `TEST_ENV_LOCK`'s bargain: the lock serialises the
/// mutations, this undoes them.
///
/// Writes go through the `process_env` carrier and never reach the real OS
/// environment; code that reads the OS environment itself (iocraft) needs
/// [`in_child_process`] instead. The
/// full previous entry (stable insertion ordinal + spelling + value) is captured
/// so restore reproduces the exact prior state — including a non-canonical
/// Windows spelling and first-to-last aggregate drops of distinct-key guards —
/// and `Drop` keeps a panic from leaking the mutation into later tests. Guards
/// nested on the same key must retain normal LIFO ownership.
///
/// The lock stays necessary: the carrier is process-global logical state, so
/// parallel tests mutating the same key still race without serialisation.
pub struct EnvVarGuard {
    previous: Option<crate::utils::process_env::EnvEntryRestore>,
}

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

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            crate::utils::process_env::restore_entry(previous);
        }
    }
}

/// For [`in_child_process`]: an SSH session outside tmux, where iocraft's
/// clipboard sends OSC 52 only and never runs the system clipboard tool.
pub const CLIPBOARD_OFF_SYSTEM: &[(&str, Option<&str>)] =
    &[("SSH_CONNECTION", Some("fixture")), ("TMUX", None)];

/// Runs the calling test again in a child process whose real environment has
/// `vars` set (`Some`) or removed (`None`), for code that reads the OS
/// environment rather than the carrier: iocraft's terminal and clipboard
/// detection reads it directly, and the carrier never writes it after startup.
/// Call it first, as
/// `if !in_child_process(module_path!(), "test_name", &[...]) { return; }`:
/// it returns `true` in the child, which then runs the body, and in the parent
/// asserts that the child ran exactly that test and passed.
pub fn in_child_process(module: &str, test: &str, vars: &[(&str, Option<&str>)]) -> bool {
    const CHILD: &str = "COMETIX_TEST_CHILD";
    // libtest names a test by its path without the crate.
    let module = module.split_once("::").map_or("", |(_, rest)| rest);
    let name = format!("{module}::{test}");
    if crate::utils::process_env::var(CHILD).as_deref() == Some(name.as_str()) {
        return true;
    }
    let mut child = std::process::Command::new(std::env::current_exe().unwrap());
    child
        .args(["--exact", &name, "--nocapture"])
        .env(CHILD, &name);
    for &(key, value) in vars {
        match value {
            Some(value) => child.env(key, value),
            None => child.env_remove(key),
        };
    }
    let output = child.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "{name}: {stdout} {}",
        String::from_utf8_lossy(&output.stderr)
    );
    false
}

/// The same poison-free contract for the other process-global test locks.
///
/// Each of those is an advisory `Mutex<()>` over module state that its tests
/// reset explicitly on entry, so a panic behind one leaves no half-written data
/// for `PoisonError` to protect. All it protects is the next test's ability to
/// report its own result: one timing-sensitive failure used to turn into a
/// `PoisonError` for every later test sharing the lock.
pub struct TestStateLock(std::sync::Mutex<()>);

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
pub struct IsolatedProjectSettings(std::path::PathBuf);

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

impl Drop for IsolatedProjectSettings {
    fn drop(&mut self) {
        crate::bootstrap::state::set_original_cwd(&self.0);
    }
}

/// The inverse pin: the harness defaults `ORIGINAL_CWD` to the isolated
/// workdir (justfile `COMETIX_TEST_PROJECT_DIR`), so a test whose fixtures
/// live under THIS repository must now say so explicitly instead of
/// inheriting the process cwd by accident.
pub struct PinnedProjectDir(std::path::PathBuf);

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

impl Drop for PinnedProjectDir {
    fn drop(&mut self) {
        crate::bootstrap::state::set_original_cwd(&self.0);
    }
}

mod tests {
    use super::*;

    /// CC tests isolate writes to the ordered environment object used by
    /// `cli/structuredIO.ts:348-360`; restoration must recover that before-state
    /// across nesting and unwinding.
    #[test]
    fn env_var_guard_restores_absence_nested_lifo_and_panic_cleanup() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
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
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
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
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
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
}
