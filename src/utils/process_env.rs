//! Canonical runtime `process.env` carrier.
//!
//! CC mutates Node's ordered `process.env` object after startup. Rust's
//! process environment is neither an ordered object nor a safe concurrent
//! mutation surface, so this carrier is the process's only mutable
//! environment and the real one stays the frozen startup capture. Readers,
//! children (`subprocess_env`) and HTTP clients (`utils::http`) all observe
//! committed immutable versions; a writer stages one turn and publishes it
//! with a single pointer swap. It owns representation and lifecycle only;
//! business policy remains in source-shaped callers.
//!
//! Rust-only, policy-free representation/lifecycle adapter for Node
//! `process.env`; domain writes remain in their source-shaped CC owners.
//! See `PORTING.md` "Global process.env carrier" and `docs/MODULE_MAP.tsv`.

use std::borrow::Cow;
use std::cell::Cell;
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, OnceLock, PoisonError, RwLock};

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
    /// Lookup index for this version, built on first read and dropped by every
    /// mutation, so a published version pays for it once.
    index: OnceLock<KeyIndex>,
}

/// Entry positions by [`key_identity`]; names without one stay pairwise.
#[derive(Clone, Debug, Default)]
struct KeyIndex {
    positions: HashMap<Box<[u8]>, usize>,
    pairwise: Vec<usize>,
}

impl KeyIndex {
    fn build(entries: &[EnvEntry]) -> Self {
        let mut index = Self::default();
        for (position, entry) in entries.iter().enumerate() {
            match key_identity(&entry.key) {
                Some(identity) => {
                    index
                        .positions
                        .insert(identity.into_owned().into_boxed_slice(), position);
                }
                None => index.pairwise.push(position),
            }
        }
        index
    }
}

impl EnvTable {
    fn position(&self, key: &OsStr) -> Option<usize> {
        self.entries
            .iter()
            .position(|entry| keys_equal(&entry.key, key))
    }

    fn entry(&self, key: &OsStr) -> Option<&EnvEntry> {
        let index = self.index.get_or_init(|| KeyIndex::build(&self.entries));
        let position = match key_identity(key) {
            Some(identity) => index.positions.get(identity.as_ref()).copied().or_else(|| {
                index
                    .pairwise
                    .iter()
                    .copied()
                    .find(|&position| keys_equal(&self.entries[position].key, key))
            }),
            None => self.position(key),
        }?;
        Some(&self.entries[position])
    }

    fn entries_mut(&mut self) -> &mut Vec<EnvEntry> {
        self.index = OnceLock::new();
        &mut self.entries
    }

    /// JS assignment replaces in place; only delete followed by re-add moves
    /// an ordinary property to the insertion tail.
    fn insert(&mut self, key: OsString, value: OsString) {
        if let Some(index) = self.position(&key) {
            let insertion_ordinal = self.entries[index].insertion_ordinal;
            self.entries_mut()[index] = EnvEntry {
                key,
                value,
                insertion_ordinal,
            };
        } else {
            let insertion_ordinal = self.next_insertion_ordinal;
            self.next_insertion_ordinal += 1;
            self.entries_mut().push(EnvEntry {
                key,
                value,
                insertion_ordinal,
            });
        }
    }

    fn remove(&mut self, key: &OsStr) -> bool {
        if let Some(index) = self.position(key) {
            self.entries_mut().remove(index);
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
        let key = lookup_key(key.as_ref())?;
        self.0.entry(&key).map(|entry| entry.value.as_os_str())
    }

    pub fn var(&self, key: impl AsRef<OsStr>) -> Option<&str> {
        let key = lookup_key(key.as_ref())?;
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
        let key = lookup_key(key.as_ref())?;
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
    /// The committed version. Its lock is held only to clone or replace the
    /// `Arc`, never across staging, so a reader never waits on a writer.
    current: RwLock<Arc<EnvTable>>,
    /// Serializes read-modify-write turns; staging runs under this lock only.
    writer: Mutex<()>,
}

impl ProcessEnv {
    #[allow(clippy::disallowed_methods)] // The one sanctioned read of the real environment.
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
            writer: Mutex::new(()),
            startup,
        }
    }

    fn committed(&self) -> Arc<EnvTable> {
        Arc::clone(&self.current.read().unwrap_or_else(PoisonError::into_inner))
    }
}

static PROCESS_ENV: LazyLock<ProcessEnv> = LazyLock::new(ProcessEnv::capture);

thread_local! {
    static UPDATE_OPEN: Cell<bool> = const { Cell::new(false) };
}

/// A global read inside this thread's open update observes the last committed
/// version, not the staged one. That is never a deadlock, only a likely stale
/// read, so it is diagnosed in debug builds rather than in production.
fn assert_global_access() {
    if cfg!(debug_assertions) {
        UPDATE_OPEN.with(|open| {
            assert!(
                !open.get(),
                "process_env global read during an open update; use EnvUpdate::snapshot()"
            );
        });
    }
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
    EnvSnapshot(PROCESS_ENV.committed())
}

/// `std::env::var_os` over the effective `process.env`.
pub fn var_os(key: impl AsRef<OsStr>) -> Option<OsString> {
    snapshot().var_os(key).map(OsStr::to_os_string)
}

/// `std::env::var` over the effective `process.env`, with the same signature
/// so a raw read migrates by path alone.
pub fn var(key: impl AsRef<OsStr>) -> Result<String, std::env::VarError> {
    var_os(key)
        .ok_or(std::env::VarError::NotPresent)?
        .into_string()
        .map_err(std::env::VarError::NotUnicode)
}

/// Begins one source-synchronous environment staging turn. Turns serialize on
/// the writer lock, which covers only staging and publication; callers must not
/// perform I/O, callbacks, awaits, or joins while it is held. Readers are never
/// blocked by it. A nested writer on the same thread panics before attempting
/// the non-reentrant lock.
pub(crate) fn begin_update() -> EnvUpdate<'static> {
    UPDATE_OPEN.with(|open| {
        assert!(
            !open.replace(true),
            "nested process_env update; pass the outer EnvUpdate or its staged snapshot"
        );
    });
    // The lock guards `()`: an unwound turn leaves nothing half-written, since
    // its staged table was never published.
    let writer = PROCESS_ENV
        .writer
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    EnvUpdate {
        _writer: writer,
        staged: PROCESS_ENV.committed(),
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
            let entries = Arc::make_mut(&mut update.staged).entries_mut();
            if let Some(current) = current {
                entries.remove(current);
            }
            let position = entries
                .partition_point(|candidate| candidate.insertion_ordinal < entry.insertion_ordinal);
            entries.insert(position, entry);
        }
        None => {
            if let Some(current) = current {
                Arc::make_mut(&mut update.staged)
                    .entries_mut()
                    .remove(current);
            }
        }
    }
    update.commit();
}

/// A staged environment update. Only [`EnvUpdate::commit`] publishes it;
/// dropping or unwinding aborts the complete turn.
pub(crate) struct EnvUpdate<'a> {
    _writer: MutexGuard<'a, ()>,
    staged: Arc<EnvTable>,
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
        Arc::make_mut(&mut self.staged).insert(key, value);
    }

    pub(crate) fn remove(&mut self, key: impl AsRef<OsStr>) {
        let Some(key) = normalize_key(key.as_ref()) else {
            return;
        };
        if self.staged.entry(&key).is_none() {
            return;
        }
        Arc::make_mut(&mut self.staged).remove(&key);
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

    /// Publishes the complete staged version exactly once, as one pointer swap:
    /// a concurrent reader observes either the previous version or this one.
    /// The real environment is never written; children receive this version
    /// through `subprocess_env`.
    pub(crate) fn commit(self) -> EnvSnapshot {
        *PROCESS_ENV
            .current
            .write()
            .unwrap_or_else(PoisonError::into_inner) = Arc::clone(&self.staged);
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

/// [`normalize_key`] for reads: a NUL-free name, the only kind real callers
/// pass, is checked in place instead of being re-encoded. NUL and `=` are
/// single bytes in every `OsStr` encoding, so the byte tests are exact.
fn lookup_key(key: &OsStr) -> Option<Cow<'_, OsStr>> {
    let bytes = key.as_encoded_bytes();
    if bytes.contains(&0) {
        return normalize_key(key).map(Cow::Owned);
    }
    (!bytes.is_empty() && !bytes.contains(&b'=')).then_some(Cow::Borrowed(key))
}

/// A key's hashable identity under [`keys_equal`], where one exists: the exact
/// bytes, or on Windows the uppercased bytes of an ASCII name, for which that
/// fold is exactly the ordinal ignore-case comparison. Other Windows names
/// have none and are compared pairwise.
#[cfg(not(windows))]
fn key_identity(key: &OsStr) -> Option<Cow<'_, [u8]>> {
    Some(Cow::Borrowed(key.as_encoded_bytes()))
}

#[cfg(windows)]
fn key_identity(key: &OsStr) -> Option<Cow<'_, [u8]>> {
    let bytes = key.as_encoded_bytes();
    if !bytes.is_ascii() {
        return None;
    }
    Some(if bytes.iter().any(u8::is_ascii_lowercase) {
        Cow::Owned(bytes.to_ascii_uppercase())
    } else {
        Cow::Borrowed(bytes)
    })
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

    // Nearly every variable name is ASCII, where the ordinal ignore-case table
    // folds exactly `a-z`; compare those without the UTF-16 copies below.
    let (left_bytes, right_bytes) = (left.as_encoded_bytes(), right.as_encoded_bytes());
    if left_bytes.is_ascii() && right_bytes.is_ascii() {
        return left_bytes.eq_ignore_ascii_case(right_bytes);
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
        // Pointer identity proves capture_startup neither rebuilt nor
        // republished the carrier.
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
    }

    /// CC `cli/structuredIO.ts:352-360` applies and logs one synchronous update
    /// before the event loop can process another observer: a concurrent reader
    /// sees the complete previous version while the turn stages, never waits on
    /// it, and sees the complete turn once it commits.
    #[test]
    fn staged_snapshots_match_official_complete_turn_visibility() {
        let _lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let first_key = "COMETIX_PROCESS_ENV_ATOMIC_A";
        let second_key = "COMETIX_PROCESS_ENV_ATOMIC_B";
        let _first = EnvVarGuard::unset(first_key);
        let _second = EnvVarGuard::unset(second_key);
        let read_elsewhere = || {
            let (send, receive) = std::sync::mpsc::channel();
            let reader = std::thread::spawn(move || {
                let current = snapshot();
                send.send((
                    current.var(first_key).map(str::to_owned),
                    current.var(second_key).map(str::to_owned),
                ))
                .unwrap();
            });
            let observed = receive
                .recv_timeout(Duration::from_secs(2))
                .expect("a reader never waits on a staging writer");
            reader.join().unwrap();
            observed
        };

        let mut update = begin_update();
        update.set(first_key, "new-a");
        let staged_before_second = update.snapshot();
        assert_eq!(read_elsewhere(), (None, None));
        update.set(second_key, "new-b");
        assert_eq!(staged_before_second.var_os(second_key), None);
        update.commit();
        assert_eq!(
            read_elsewhere(),
            (Some("new-a".into()), Some("new-b".into()))
        );
    }

    /// The same complete-turn visibility under contention: readers racing a
    /// stream of two-key turns never observe one key from each version.
    #[test]
    fn concurrent_readers_never_observe_a_partial_turn() {
        let _lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let keys = ["COMETIX_PROCESS_ENV_TORN_A", "COMETIX_PROCESS_ENV_TORN_B"];
        let _guards = keys.map(EnvVarGuard::unset);
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let start = Arc::new(std::sync::Barrier::new(5));
        let readers = (0..4)
            .map(|_| {
                let (done, start) = (Arc::clone(&done), Arc::clone(&start));
                std::thread::spawn(move || {
                    start.wait();
                    loop {
                        let current = snapshot();
                        assert_eq!(current.var(keys[0]), current.var(keys[1]));
                        if done.load(std::sync::atomic::Ordering::Acquire) {
                            break;
                        }
                    }
                })
            })
            .collect::<Vec<_>>();
        start.wait();
        for round in 0..200 {
            let value = round.to_string();
            let mut update = begin_update();
            update.set(keys[0], &value);
            update.set(keys[1], &value);
            update.commit();
        }
        done.store(true, std::sync::atomic::Ordering::Release);
        for reader in readers {
            reader.join().expect("a reader observed a partial turn");
        }
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

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "use EnvUpdate::snapshot()")]
    fn global_read_inside_update_trips_the_fuse() {
        let _update = begin_update();
        let _ = snapshot();
    }

    /// CC uses Node's platform `process.env` key identity; on Unix names are
    /// byte-exact.
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
        assert_eq!(var(lower).as_deref(), Ok("lower"));
        assert_eq!(var(upper).as_deref(), Ok("upper"));
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

        // Reads go through the version's index and keep the same identity:
        // ASCII names fold there, other names stay pairwise.
        let value = |table: &EnvTable, key: &str| {
            table
                .entry(OsStr::new(key))
                .map(|entry| entry.value.clone())
        };
        assert_eq!(value(&table, "PATH"), Some("three".into()));
        assert_eq!(value(&table, "other"), Some("middle".into()));
        table.insert("ÄRGER".into(), "umlaut".into());
        table.insert(first.clone(), "surrogate".into());
        assert_eq!(value(&table, "ärger"), Some("umlaut".into()));
        assert_eq!(
            table.entry(&first).map(|entry| entry.value.clone()),
            Some("surrogate".into())
        );
        assert!(table.entry(&second).is_none());
    }

    /// The read index is per version: every mutation drops it, including the
    /// removals that shift later positions.
    #[test]
    fn read_index_tracks_every_mutation() {
        let mut table = EnvTable::default();
        let value = |table: &EnvTable, key: &str| {
            table
                .entry(OsStr::new(key))
                .map(|entry| entry.value.clone())
        };
        table.insert("COMETIX_INDEX_A".into(), "one".into());
        assert_eq!(value(&table, "COMETIX_INDEX_A"), Some("one".into()));
        table.insert("COMETIX_INDEX_A".into(), "two".into());
        table.insert("COMETIX_INDEX_B".into(), "b".into());
        assert_eq!(value(&table, "COMETIX_INDEX_A"), Some("two".into()));
        assert_eq!(value(&table, "COMETIX_INDEX_B"), Some("b".into()));
        assert!(table.remove(OsStr::new("COMETIX_INDEX_A")));
        assert_eq!(value(&table, "COMETIX_INDEX_A"), None);
        assert_eq!(value(&table, "COMETIX_INDEX_B"), Some("b".into()));
    }

    /// The carrier is the L1 owner for ordinary CC `process.env` assignments
    /// (`cli/structuredIO.ts:348-360`) on every platform; only the Windows
    /// bootstrap hardening owner may mutate the real environment.
    #[test]
    #[allow(clippy::disallowed_methods)] // Inspects the real environment.
    fn carrier_operations_leave_real_environment_unchanged() {
        let _lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let key = "COMETIX_PROCESS_ENV_OS_UNCHANGED";
        let real_before = std::env::vars_os().collect::<Vec<_>>();
        let raw_value_before = std::env::var_os(key);
        let _carrier = EnvVarGuard::unset(key);
        assert_eq!(std::env::vars_os().collect::<Vec<_>>(), real_before);
        assert_eq!(std::env::var_os(key), raw_value_before);

        let before_invalid = snapshot();
        set("", "ignored");
        set("BAD=KEY", "ignored");
        assert!(before_invalid.same_version(&snapshot()));
        assert_eq!(std::env::vars_os().collect::<Vec<_>>(), real_before);

        set(key, "valid\0truncated");
        assert_eq!(var(key).as_deref(), Ok("valid"));
        assert_eq!(std::env::vars_os().collect::<Vec<_>>(), real_before);
        remove(key);
        assert_eq!(std::env::vars_os().collect::<Vec<_>>(), real_before);
    }
}
