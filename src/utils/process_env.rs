//! Canonical runtime `process.env` carrier.
//!
//! CC mutates Node's ordered `process.env` object after startup. Rust's
//! process environment is neither an ordered object nor a safe concurrent
//! mutation surface, so this boundary exposes immutable snapshots to migrated
//! readers and the migration-time subprocess adapter. It owns representation
//! and lifecycle only; business policy remains in source-shaped callers.
//!
//! Rust-only, policy-free representation/lifecycle adapter for Node
//! `process.env`; domain writes remain in their source-shaped CC owners.
//! See `PORTING.md` "Global process.env carrier" and `docs/MODULE_MAP.tsv`.

use std::cell::Cell;
use std::ffi::{OsStr, OsString};
use std::sync::{Arc, LazyLock, RwLock, RwLockWriteGuard};

#[derive(Clone, Debug)]
struct EnvEntry {
    key: OsString,
    value: OsString,
    insertion_ordinal: usize,
}

#[derive(Clone, Debug, Default)]
struct EnvTable {
    entries: Vec<EnvEntry>,
    next_insertion_ordinal: usize,
}

impl EnvTable {
    fn position(&self, key: &OsStr) -> Option<usize> {
        self.entries
            .iter()
            .position(|entry| keys_equal(&entry.key, key))
    }

    fn entry(&self, key: &OsStr) -> Option<&EnvEntry> {
        self.position(key).map(|index| &self.entries[index])
    }

    /// JS assignment replaces in place; only delete followed by re-add moves
    /// an ordinary property to the insertion tail.
    fn insert(&mut self, key: OsString, value: OsString) {
        if let Some(index) = self.position(&key) {
            let insertion_ordinal = self.entries[index].insertion_ordinal;
            self.entries[index] = EnvEntry {
                key,
                value,
                insertion_ordinal,
            };
        } else {
            let insertion_ordinal = self.next_insertion_ordinal;
            self.next_insertion_ordinal += 1;
            self.entries.push(EnvEntry {
                key,
                value,
                insertion_ordinal,
            });
        }
    }

    fn remove(&mut self, key: &OsStr) -> bool {
        if let Some(index) = self.position(key) {
            self.entries.remove(index);
            true
        } else {
            false
        }
    }

    fn projected(&self) -> Vec<&EnvEntry> {
        let mut integer = self
            .entries
            .iter()
            .filter_map(|entry| array_index(&entry.key).map(|index| (index, entry)))
            .collect::<Vec<_>>();
        integer.sort_by_key(|(index, _)| *index);
        integer
            .into_iter()
            .map(|(_, entry)| entry)
            .chain(
                self.entries
                    .iter()
                    .filter(|entry| array_index(&entry.key).is_none()),
            )
            .collect()
    }
}

/// One immutable version of the effective runtime environment.
#[derive(Clone, Debug)]
pub struct EnvSnapshot(Arc<EnvTable>);

/// Projects a JavaScript object carrier into ECMAScript own-key order:
/// integer-index names ascending, then ordinary names in source insertion order.
pub(crate) fn ecmascript_object_entries<'a, V: 'a>(
    entries: impl IntoIterator<Item = (&'a String, &'a V)>,
) -> Vec<(&'a str, &'a V)> {
    let mut integer = Vec::new();
    let mut ordinary = Vec::new();
    for (key, value) in entries {
        if let Some(index) = array_index(OsStr::new(key)) {
            integer.push((index, key.as_str(), value));
        } else {
            ordinary.push((key.as_str(), value));
        }
    }
    integer.sort_by_key(|(index, _, _)| *index);
    integer
        .into_iter()
        .map(|(_, key, value)| (key, value))
        .chain(ordinary)
        .collect()
}

/// Serde adapter for JSON objects whose observable order follows JavaScript.
pub(crate) fn serialize_ecmascript_object<'a, S, V: serde::Serialize + 'a>(
    entries: impl IntoIterator<Item = (&'a String, &'a V)>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.collect_map(ecmascript_object_entries(entries))
}

impl EnvSnapshot {
    pub fn var_os(&self, key: impl AsRef<OsStr>) -> Option<&OsStr> {
        let key = normalize_key(key.as_ref())?;
        self.0.entry(&key).map(|entry| entry.value.as_os_str())
    }

    pub fn var(&self, key: impl AsRef<OsStr>) -> Option<&str> {
        let key = normalize_key(key.as_ref())?;
        self.0.entry(&key)?.value.to_str()
    }

    pub fn contains(&self, key: impl AsRef<OsStr>) -> bool {
        self.var_os(key).is_some()
    }

    /// ECMAScript own-key order: array-index names ascending, then ordinary
    /// names in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = (&OsStr, &OsStr)> + '_ {
        self.0
            .projected()
            .into_iter()
            .map(|entry| (entry.key.as_os_str(), entry.value.as_os_str()))
    }

    pub fn keys(&self) -> impl Iterator<Item = &OsStr> + '_ {
        self.iter().map(|(key, _)| key)
    }

    #[cfg(all(test, windows))]
    pub(crate) fn entry(&self, key: impl AsRef<OsStr>) -> Option<(&OsStr, &OsStr)> {
        let key = normalize_key(key.as_ref())?;
        self.0
            .entry(&key)
            .map(|entry| (entry.key.as_os_str(), entry.value.as_os_str()))
    }

    #[cfg(test)]
    fn same_version(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// Test-only token for restoring one entry without replacing unrelated state.
#[cfg(test)]
pub(crate) struct EnvEntryRestore {
    key: OsString,
    previous: Option<EnvEntry>,
}

#[cfg(test)]
pub(crate) fn save_entry_for_restore(key: impl AsRef<OsStr>) -> EnvEntryRestore {
    let key = normalize_key(key.as_ref()).expect("test environment key must be valid");
    let previous = snapshot().0.entry(&key).cloned();
    EnvEntryRestore { key, previous }
}

struct ProcessEnv {
    startup: Arc<EnvTable>,
    current: RwLock<Arc<EnvTable>>,
}

impl ProcessEnv {
    fn capture() -> Self {
        let mut table = EnvTable::default();
        for (key, value) in std::env::vars_os() {
            if let Some((key, value)) = normalize_assignment(&key, &value) {
                table.insert(key, value);
            }
        }
        let startup = Arc::new(table);
        Self {
            current: RwLock::new(Arc::clone(&startup)),
            startup,
        }
    }
}

static PROCESS_ENV: LazyLock<ProcessEnv> = LazyLock::new(ProcessEnv::capture);

thread_local! {
    static UPDATE_OPEN: Cell<bool> = const { Cell::new(false) };
}

fn assert_global_access() {
    UPDATE_OPEN.with(|open| {
        assert!(
            !open.get(),
            "process_env global read during an open update; use EnvUpdate::snapshot()"
        );
    });
}

/// Forces the frozen startup capture. Repeated calls are idempotent.
pub fn capture_startup() {
    assert_global_access();
    LazyLock::force(&PROCESS_ENV);
}

/// Native-runtime input snapshot for CC's Bun.which import. Bun reads the
/// startup PATH even after process.env.PATH changes; it does not consult the
/// mutable JS environment object. Retain that existing capture separately
/// from current versions, without changing any ordinary process.env reader.
pub(crate) fn startup_snapshot() -> EnvSnapshot {
    assert_global_access();
    EnvSnapshot(Arc::clone(&PROCESS_ENV.startup))
}

/// Returns the currently committed immutable environment version.
pub fn snapshot() -> EnvSnapshot {
    assert_global_access();
    let current = PROCESS_ENV
        .current
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    EnvSnapshot(Arc::clone(&current))
}

pub fn var_os(key: impl AsRef<OsStr>) -> Option<OsString> {
    snapshot().var_os(key).map(OsStr::to_os_string)
}

pub fn var(key: impl AsRef<OsStr>) -> Option<String> {
    snapshot().var(key).map(str::to_owned)
}

/// Begins one source-synchronous environment staging turn. The lock covers
/// only staging/publication; callers must not perform I/O, callbacks, awaits,
/// or joins while it is held. A nested writer on the same thread panics before
/// attempting the non-reentrant lock.
pub(crate) fn begin_update() -> EnvUpdate<'static> {
    UPDATE_OPEN.with(|open| {
        assert!(
            !open.replace(true),
            "nested process_env update; pass the outer EnvUpdate or its staged snapshot"
        );
    });
    let guard = PROCESS_ENV
        .current
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let staged = Arc::clone(&guard);
    EnvUpdate {
        guard,
        staged,
        #[cfg(any(unix, windows))]
        os_operations: Vec::new(),
    }
}

/// One-key `process.env[key] = value` convenience for source-owned writers.
pub(crate) fn set(key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) {
    let mut update = begin_update();
    update.set(key, value);
    update.commit();
}

/// One-key `delete process.env[key]` convenience.
pub(crate) fn remove(key: impl AsRef<OsStr>) {
    let mut update = begin_update();
    update.remove(key);
    update.commit();
}

/// Restores one test entry at its stable carrier-owned insertion ordinal while
/// retaining every unrelated mutation made by a compound fixture. Distinct-key
/// aggregate guards may drop first-to-last; same-key nesting remains LIFO, and
/// manually dropping same-key guards out of ownership order is unsupported.
#[cfg(test)]
pub(crate) fn restore_entry(saved: EnvEntryRestore) {
    let mut update = begin_update();
    let current = update.staged.position(&saved.key);
    match saved.previous {
        Some(entry) => {
            let table = Arc::make_mut(&mut update.staged);
            if let Some(current) = current {
                table.entries.remove(current);
            }
            let position = table
                .entries
                .partition_point(|candidate| candidate.insertion_ordinal < entry.insertion_ordinal);
            table.entries.insert(position, entry.clone());
            #[cfg(any(unix, windows))]
            update
                .os_operations
                .push(OsOperation::Set(entry.key, entry.value));
        }
        None => {
            if let Some(current) = current {
                Arc::make_mut(&mut update.staged).entries.remove(current);
                #[cfg(any(unix, windows))]
                update.os_operations.push(OsOperation::Remove(saved.key));
            }
        }
    }
    update.commit();
}

#[cfg(any(unix, windows))]
enum OsOperation {
    Set(OsString, OsString),
    Remove(OsString),
}

/// A staged environment update. Only [`EnvUpdate::commit`] publishes it;
/// dropping or unwinding aborts the complete turn.
pub(crate) struct EnvUpdate<'a> {
    guard: RwLockWriteGuard<'a, Arc<EnvTable>>,
    staged: Arc<EnvTable>,
    #[cfg(any(unix, windows))]
    os_operations: Vec<OsOperation>,
}

impl EnvUpdate<'_> {
    /// Snapshot of this turn's staged state. It remains immutable if later
    /// operations mutate the turn.
    pub(crate) fn snapshot(&self) -> EnvSnapshot {
        EnvSnapshot(Arc::clone(&self.staged))
    }

    pub(crate) fn set(&mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) {
        let Some((key, value)) = normalize_assignment(key.as_ref(), value.as_ref()) else {
            return;
        };
        Arc::make_mut(&mut self.staged).insert(key.clone(), value.clone());
        #[cfg(any(unix, windows))]
        self.os_operations.push(OsOperation::Set(key, value));
    }

    pub(crate) fn remove(&mut self, key: impl AsRef<OsStr>) {
        let Some(key) = normalize_key(key.as_ref()) else {
            return;
        };
        if self.staged.entry(&key).is_none() {
            return;
        }
        Arc::make_mut(&mut self.staged).remove(&key);
        #[cfg(any(unix, windows))]
        self.os_operations.push(OsOperation::Remove(key));
    }

    pub(crate) fn apply<I, K, V>(&mut self, variables: I)
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        for (key, value) in variables {
            self.set(key, value);
        }
    }

    /// Publishes the complete staged version exactly once. Until production
    /// raw readers are migrated, every platform mirrors the already-normalized
    /// operations into the real environment at this boundary only. A build
    /// without this write-through strands every carrier-only write where the
    /// raw `std::env` readers cannot see it — settings.json `env` included,
    /// which is how `ANTHROPIC_AUTH_TOKEN` stopped reaching auth
    /// (`services/api/client.rs:84`).
    #[allow(clippy::disallowed_methods)] // Transitional carrier-owned real-env write-through.
    pub(crate) fn commit(mut self) -> EnvSnapshot {
        #[cfg(any(unix, windows))]
        for operation in &self.os_operations {
            // SAFETY: `normalize_assignment`/`normalize_key` strip the NUL and
            // `=` shapes that make std's API panic. Windows `set_var` is
            // documented as sound — the same premise `entrypoints/cli.rs:19-23`
            // already relies on. On Unix the write-through stays unsound with
            // concurrent raw OS readers and is not atomic with carrier
            // publication, so the integration child removes this bridge only
            // after the raw readers are migrated.
            unsafe {
                match operation {
                    OsOperation::Set(key, value) => std::env::set_var(key, value),
                    OsOperation::Remove(key) => std::env::remove_var(key),
                }
            }
        }
        *self.guard = Arc::clone(&self.staged);
        EnvSnapshot(Arc::clone(&self.staged))
    }
}

impl Drop for EnvUpdate<'_> {
    fn drop(&mut self) {
        UPDATE_OPEN.with(|open| open.set(false));
    }
}

fn normalize_assignment(key: &OsStr, value: &OsStr) -> Option<(OsString, OsString)> {
    Some((normalize_key(key)?, truncate_at_nul(value)))
}

fn normalize_key(key: &OsStr) -> Option<OsString> {
    let key = truncate_at_nul(key);
    if key.is_empty() || contains_equals(&key) {
        None
    } else {
        Some(key)
    }
}

#[cfg(unix)]
fn truncate_at_nul(value: &OsStr) -> OsString {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let bytes = value.as_bytes();
    OsString::from_vec(
        bytes[..bytes
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(bytes.len())]
            .to_vec(),
    )
}

#[cfg(windows)]
fn truncate_at_nul(value: &OsStr) -> OsString {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    let units = value.encode_wide().collect::<Vec<_>>();
    OsString::from_wide(
        &units[..units
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(units.len())],
    )
}

#[cfg(not(any(unix, windows)))]
fn truncate_at_nul(value: &OsStr) -> OsString {
    value.to_os_string()
}

#[cfg(unix)]
fn contains_equals(value: &OsStr) -> bool {
    use std::os::unix::ffi::OsStrExt;
    value.as_bytes().contains(&b'=')
}

#[cfg(windows)]
fn contains_equals(value: &OsStr) -> bool {
    use std::os::windows::ffi::OsStrExt;
    value.encode_wide().any(|unit| unit == b'=' as u16)
}

#[cfg(not(any(unix, windows)))]
fn contains_equals(value: &OsStr) -> bool {
    value.as_encoded_bytes().contains(&b'=')
}

#[cfg(unix)]
fn keys_equal(left: &OsStr, right: &OsStr) -> bool {
    use std::os::unix::ffi::OsStrExt;
    left.as_bytes() == right.as_bytes()
}

#[cfg(windows)]
fn keys_equal(left: &OsStr, right: &OsStr) -> bool {
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CompareStringOrdinal(
            string1: *const u16,
            count1: i32,
            string2: *const u16,
            count2: i32,
            ignore_case: i32,
        ) -> i32;
    }

    let left = left.encode_wide().collect::<Vec<_>>();
    let right = right.encode_wide().collect::<Vec<_>>();
    let (Ok(left_len), Ok(right_len)) = (i32::try_from(left.len()), i32::try_from(right.len()))
    else {
        return false;
    };
    // CSTR_EQUAL = 2. CompareStringOrdinal is the platform's ordinal,
    // locale-independent environment-name identity primitive.
    unsafe { CompareStringOrdinal(left.as_ptr(), left_len, right.as_ptr(), right_len, 1) == 2 }
}

#[cfg(not(any(unix, windows)))]
fn keys_equal(left: &OsStr, right: &OsStr) -> bool {
    left == right
}

fn parse_array_index(units: impl IntoIterator<Item = u32>) -> Option<u32> {
    let mut value = 0u32;
    let mut count = 0usize;
    for unit in units {
        if !(u32::from(b'0')..=u32::from(b'9')).contains(&unit) || (count == 1 && value == 0) {
            return None;
        }
        value = value.checked_mul(10)?.checked_add(unit - u32::from(b'0'))?;
        count += 1;
    }
    (count > 0 && value != u32::MAX).then_some(value)
}

#[cfg(unix)]
fn array_index(key: &OsStr) -> Option<u32> {
    use std::os::unix::ffi::OsStrExt;
    parse_array_index(key.as_bytes().iter().copied().map(u32::from))
}

#[cfg(windows)]
fn array_index(key: &OsStr) -> Option<u32> {
    use std::os::windows::ffi::OsStrExt;
    parse_array_index(key.encode_wide().map(u32::from))
}

#[cfg(not(any(unix, windows)))]
fn array_index(key: &OsStr) -> Option<u32> {
    parse_array_index(key.as_encoded_bytes().iter().copied().map(u32::from))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::env_utils::EnvVarGuard;
    use std::time::Duration;

    fn strings(snapshot: &EnvSnapshot, selected: &[&str]) -> Vec<(String, String)> {
        snapshot
            .iter()
            .filter_map(|(key, value)| {
                let key = key.to_str()?;
                selected
                    .contains(&key)
                    .then(|| (key.to_string(), value.to_string_lossy().into_owned()))
            })
            .collect()
    }

    /// Rust process startup owns one frozen view, matching Node's one
    /// process-lifetime `process.env` object used by CC.
    #[test]
    fn startup_capture_is_idempotent_and_frozen_matches_official_process_lifetime() {
        let _lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        capture_startup();
        let key = "COMETIX_PROCESS_ENV_STARTUP_FREEZE";
        let _guard = EnvVarGuard::preserve(key);
        let initial = startup_snapshot();
        let initial_value = initial.var(key).map(str::to_owned);
        set(key, "runtime-value");
        let assigned = snapshot();

        capture_startup();
        let recaptured = snapshot();
        // On Unix the transitional write-through makes raw values unsuitable
        // as a recapture oracle. Pointer identity proves capture_startup neither
        // rebuilt nor republished the carrier.
        assert!(assigned.same_version(&recaptured));
        assert_eq!(recaptured.var(key), Some("runtime-value"));
        assert!(initial.same_version(&startup_snapshot()));
        assert_eq!(startup_snapshot().var(key), initial_value.as_deref());
    }

    /// CC `cli/structuredIO.ts:348-360` assigns `Object.entries` into the one
    /// ordered `process.env`; Node v24 oracle details are in process-env-carrier.md §1.2.
    #[test]
    fn own_key_order_replacement_delete_readd_and_empty_values_match_official_node() {
        let _lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let keys = [
            "4294967294",
            "2",
            "01",
            "0",
            "4294967295",
            "COMETIX_ORDER_A",
            "COMETIX_ORDER_B",
        ];
        let _guards = keys.map(EnvVarGuard::unset);

        let mut update = begin_update();
        update.apply([
            ("COMETIX_ORDER_A", "first"),
            ("2", "two"),
            ("01", "ordinary-leading-zero"),
            ("4294967294", "max-index"),
            ("0", "zero"),
            ("4294967295", "not-an-index"),
            ("COMETIX_ORDER_B", ""),
            ("COMETIX_ORDER_A", "replacement"),
        ]);
        let committed = update.commit();
        assert_eq!(
            strings(&committed, &keys),
            [
                ("0".into(), "zero".into()),
                ("2".into(), "two".into()),
                ("4294967294".into(), "max-index".into()),
                ("COMETIX_ORDER_A".into(), "replacement".into()),
                ("01".into(), "ordinary-leading-zero".into()),
                ("4294967295".into(), "not-an-index".into()),
                ("COMETIX_ORDER_B".into(), "".into()),
            ]
        );

        let mut update = begin_update();
        update.remove("COMETIX_ORDER_A");
        update.set("COMETIX_ORDER_A", "re-added");
        let committed = update.commit();
        let ordinary = strings(&committed, &["COMETIX_ORDER_A", "COMETIX_ORDER_B"]);
        assert_eq!(
            ordinary,
            [
                ("COMETIX_ORDER_B".into(), "".into()),
                ("COMETIX_ORDER_A".into(), "re-added".into()),
            ]
        );
    }

    /// CC `cli/structuredIO.ts:352-355` uses `Object.entries`, whose own-key
    /// projection puts integer indices before ordinary insertion order.
    #[test]
    fn object_entries_match_official_ecmascript_own_key_order() {
        let entries = indexmap::IndexMap::from([
            ("10".to_string(), "ten"),
            ("ordinary-a".to_string(), "a"),
            ("4294967295".to_string(), "not-an-index"),
            ("2".to_string(), "two"),
            ("0".to_string(), "zero"),
            ("4294967294".to_string(), "largest-index"),
            ("01".to_string(), "leading-zero"),
            ("ordinary-b".to_string(), "b"),
        ]);
        assert_eq!(
            ecmascript_object_entries(entries.iter())
                .into_iter()
                .map(|(key, _)| key)
                .collect::<Vec<_>>(),
            [
                "0",
                "2",
                "10",
                "4294967294",
                "ordinary-a",
                "4294967295",
                "01",
                "ordinary-b",
            ]
        );
    }

    /// CC `cli/structuredIO.ts:352-355` completes its synchronous assignment
    /// loop before another event-loop turn can observe the resulting object.
    #[test]
    fn aborted_and_unwound_updates_match_official_synchronous_visibility() {
        let _lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let key = "COMETIX_PROCESS_ENV_ABORT";
        let _guard = EnvVarGuard::unset(key);
        let before = snapshot();
        {
            let mut update = begin_update();
            update.set(key, "dropped");
        }
        let after_drop = snapshot();
        assert!(before.same_version(&after_drop));
        assert_eq!(after_drop.var_os(key), None);

        let result = std::panic::catch_unwind(|| {
            let mut update = begin_update();
            update.set(key, "unwound");
            panic!("abort turn");
        });
        assert!(result.is_err());
        let after_unwind = snapshot();
        assert!(before.same_version(&after_unwind));
        assert_eq!(after_unwind.var_os(key), None);
    }

    /// CC `cli/structuredIO.ts:355` delegates each arbitrary string assignment
    /// to Node's `process.env`; the Node v24 normalization oracle is recorded in
    /// process-env-carrier.md §1.2.
    #[test]
    fn normalization_matches_official_node_process_env_assignment() {
        let _lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let key = "COMETIX_PROCESS_ENV_NORMALIZE";
        let _guard = EnvVarGuard::unset(key);
        let before = snapshot();

        let mut ignored = begin_update();
        ignored.apply([("", "x"), ("=bad", "x"), ("\0emptied", "x")]);
        ignored.set("BAD=KEY", "x");
        let ignored = ignored.commit();
        assert!(before.same_version(&ignored));

        let mut update = begin_update();
        update.set(format!("{key}\0ignored"), "kept\0ignored");
        update.set("ALSO=IGNORED", "x");
        let committed = update.commit();
        assert_eq!(committed.var(key).as_deref(), Some("kept"));
        assert_eq!(
            committed.var(format!("{key}\0lookup-suffix")).as_deref(),
            Some("kept")
        );
        assert_eq!(committed.var_os("ALSO=IGNORED"), None);
        #[cfg(unix)]
        assert_eq!(std::env::var(key).as_deref(), Ok("kept"));
    }

    /// CC `cli/structuredIO.ts:352-360` applies and logs one synchronous update
    /// before the event loop can process another observer.
    #[test]
    fn staged_snapshots_match_official_complete_turn_visibility() {
        let _lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let first_key = "COMETIX_PROCESS_ENV_ATOMIC_A";
        let second_key = "COMETIX_PROCESS_ENV_ATOMIC_B";
        let _first = EnvVarGuard::unset(first_key);
        let _second = EnvVarGuard::unset(second_key);

        let mut update = begin_update();
        update.set(first_key, "new-a");
        let staged_before_second = update.snapshot();
        let (ready_send, ready_receive) = std::sync::mpsc::channel();
        let (send, receive) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            ready_send.send(()).unwrap();
            let current = snapshot();
            send.send((
                current.var(first_key).map(str::to_owned),
                current.var(second_key).map(str::to_owned),
            ))
            .unwrap();
        });
        ready_receive
            .recv_timeout(Duration::from_secs(2))
            .expect("reader reached the snapshot boundary");
        assert!(receive.recv_timeout(Duration::from_millis(50)).is_err());
        update.set(second_key, "new-b");
        assert_eq!(staged_before_second.var_os(second_key), None);
        update.commit();
        assert_eq!(
            receive.recv_timeout(Duration::from_secs(2)).unwrap(),
            (Some("new-a".into()), Some("new-b".into()))
        );
        reader.join().unwrap();
    }

    #[test]
    fn nested_writer_fails_before_lock_instead_of_deadlocking() {
        let (send, receive) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let update = begin_update();
            let nested = std::panic::catch_unwind(begin_update);
            drop(update);
            send.send(nested.is_err()).unwrap();
        });
        assert_eq!(receive.recv_timeout(Duration::from_secs(2)), Ok(true));
        worker.join().unwrap();
        begin_update().commit();
    }

    #[test]
    #[should_panic(expected = "use EnvUpdate::snapshot()")]
    fn global_read_inside_update_trips_the_fuse() {
        let _update = begin_update();
        let _ = snapshot();
    }

    /// CC uses Node's platform `process.env` key identity; on Unix names are
    /// byte-exact. Write-through remains only for this migration phase.
    #[cfg(unix)]
    #[test]
    fn unix_identity_and_commit_match_official_process_env_behavior() {
        let _lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let lower = "cometix_process_env_case";
        let upper = "COMETIX_PROCESS_ENV_CASE";
        let _lower = EnvVarGuard::unset(lower);
        let _upper = EnvVarGuard::unset(upper);
        let mut update = begin_update();
        update.set(lower, "lower");
        update.set(upper, "upper");
        update.commit();
        assert_eq!(var(lower).as_deref(), Some("lower"));
        assert_eq!(var(upper).as_deref(), Some("upper"));
        assert_eq!(std::env::var(lower).as_deref(), Ok("lower"));
        assert_eq!(std::env::var(upper).as_deref(), Ok("upper"));
    }

    /// CC uses Node's Windows `process.env` identity: ordinal ignore-case with
    /// the latest assignment's active spelling and value.
    #[cfg(windows)]
    #[test]
    fn windows_identity_matches_official_process_env_behavior() {
        use std::os::windows::ffi::OsStringExt;

        let mut table = EnvTable::default();
        table.insert("Path".into(), "one".into());
        table.insert("PATH".into(), "two".into());
        assert_eq!(table.entries.len(), 1);
        assert_eq!(table.entries[0].key, OsString::from("PATH"));
        assert_eq!(table.entries[0].value, OsString::from("two"));
        table.insert("OTHER".into(), "middle".into());
        assert!(table.remove(OsStr::new("path")));
        table.insert("pAtH".into(), "three".into());
        assert_eq!(
            table
                .entries
                .iter()
                .map(|entry| entry.key.clone())
                .collect::<Vec<_>>(),
            vec![OsString::from("OTHER"), OsString::from("pAtH")]
        );

        // Lossy UTF-8 folding would collapse both unpaired surrogates to U+FFFD;
        // the platform ordinal comparison correctly keeps them distinct.
        let first = OsString::from_wide(&[0xD800]);
        let second = OsString::from_wide(&[0xD801]);
        assert_eq!(first.to_string_lossy(), second.to_string_lossy());
        assert!(!keys_equal(&first, &second));
    }

    /// The carrier mirrors ordinary CC `process.env` assignments into the real
    /// environment (`cli/structuredIO.ts:348-360`). Node has ONE `process.env`
    /// object, so a carrier-only write strands settings `env` where the raw
    /// `std::env` readers cannot see it — which is exactly how
    /// `ANTHROPIC_AUTH_TOKEN` from settings.json stopped reaching auth
    /// (`services/api/client.rs:84`) before this write-through covered Windows.
    #[cfg(windows)]
    #[test]
    fn windows_carrier_operations_mirror_into_real_environment() {
        let _lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let key = "COMETIX_PROCESS_ENV_WINDOWS_OS_MIRRORED";
        let _carrier = EnvVarGuard::unset(key);

        // Names normalization rejects reach neither the carrier nor the OS.
        let before_invalid = snapshot();
        set("", "ignored");
        set("BAD=KEY", "ignored");
        assert!(before_invalid.same_version(&snapshot()));

        set(key, "valid\0truncated");
        assert_eq!(var(key).as_deref(), Some("valid"));
        assert_eq!(
            std::env::var(key).as_deref(),
            Ok("valid"),
            "a carrier write must reach the real Windows environment"
        );

        remove(key);
        assert_eq!(var(key), None);
        assert_eq!(std::env::var_os(key), None);
    }
}
