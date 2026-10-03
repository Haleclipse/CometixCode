//! Maps to: CC `keybindings/loadUserBindings.ts`.
//!
//! The synchronous startup loader, merged default/user ordering, cached warning
//! snapshot, explicit reload path, and background stable-write watcher are wired.

use super::default_bindings::default_bindings;
use super::parser::parse_chord;
use super::types::{ContextName, ParsedBinding};
use super::validate::KeybindingWarning;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::{LazyLock, RwLock};
use std::time::{Duration, Instant, SystemTime};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct KeybindingsLoadResult {
    pub bindings: Vec<ParsedBinding>,
    pub warnings: Vec<KeybindingWarning>,
}

static CACHED_RESULT: LazyLock<RwLock<Option<KeybindingsLoadResult>>> =
    LazyLock::new(|| RwLock::new(None));

type ChangeSubscribers =
    Vec<(u64, futures::channel::mpsc::UnboundedSender<KeybindingsLoadResult>)>;

/// Maps to: CC `keybindingsChanged = createSignal<[result]>()`
/// (loadUserBindings.ts:71): every mounted KeybindingSetup hears each reload.
static CHANGE_SUBSCRIBERS: LazyLock<std::sync::Mutex<ChangeSubscribers>> =
    LazyLock::new(|| std::sync::Mutex::new(Vec::new()));
static NEXT_SUBSCRIBER_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A live `subscribeToKeybindingChanges` registration; dropping it is CC's
/// returned `unsubscribe()` (KeybindingProviderSetup.tsx:213-216).
pub struct KeybindingChangeSubscription {
    id: u64,
}

impl Drop for KeybindingChangeSubscription {
    fn drop(&mut self) {
        CHANGE_SUBSCRIBERS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .retain(|(id, _)| *id != self.id);
    }
}

/// Maps to: CC `subscribeToKeybindingChanges` (loadUserBindings.ts:422).
/// Each reload arrives on the returned receiver.
pub fn subscribe_to_keybinding_changes() -> (
    KeybindingChangeSubscription,
    futures::channel::mpsc::UnboundedReceiver<KeybindingsLoadResult>,
) {
    let id = NEXT_SUBSCRIBER_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let (sender, receiver) = futures::channel::mpsc::unbounded();
    CHANGE_SUBSCRIBERS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .push((id, sender));
    (KeybindingChangeSubscription { id }, receiver)
}

/// Maps to: CC `keybindingsChanged.emit(result)` (loadUserBindings.ts:436).
fn emit_keybinding_change(result: &KeybindingsLoadResult) {
    for (_, sender) in CHANGE_SUBSCRIBERS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .iter()
    {
        let _ = sender.unbounded_send(result.clone());
    }
}

/// Reloads from disk and tells every subscriber, the way a watcher-detected
/// change does. Used after `/keybindings` returns from the external editor.
pub fn reload_keybindings_and_notify() {
    emit_keybinding_change(&reload_keybindings_sync_with_warnings());
}

const FILE_STABILITY_THRESHOLD: Duration = Duration::from_millis(500);
const FILE_STABILITY_POLL_INTERVAL: Duration = Duration::from_millis(200);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileFingerprint {
    modified: Option<SystemTime>,
    len: u64,
}

fn file_fingerprint(path: &std::path::Path) -> Option<FileFingerprint> {
    let metadata = std::fs::metadata(path).ok()?;
    Some(FileFingerprint {
        modified: metadata.modified().ok(),
        len: metadata.len(),
    })
}

/// L1 adaptation of CC's chokidar watcher: polling runs on a background thread,
/// never in the retained frame, and waits for the same 500 ms write stability.
pub struct KeybindingWatcher {
    stop_tx: Option<std::sync::mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for KeybindingWatcher {
    fn drop(&mut self) {
        if let Some(stop_tx) = self.stop_tx.take() {
            let _ = stop_tx.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Maps to: CC `initializeKeybindingWatcher()` (loadUserBindings.ts:353-404):
/// a stable write reloads the file and emits the result to every
/// `subscribe_to_keybinding_changes` subscriber (:424-437).
pub fn initialize_keybinding_watcher() -> Option<KeybindingWatcher> {
    if !is_keybinding_customization_enabled() {
        return None;
    }
    let path = get_keybindings_path();
    if !path.parent().is_some_and(std::path::Path::is_dir) {
        return None;
    }

    let initial_fingerprint = file_fingerprint(&path);
    let (stop_tx, stop_rx) = std::sync::mpsc::channel();
    let thread = std::thread::spawn(move || {
        let mut observed = initial_fingerprint;
        let mut pending: Option<(Option<FileFingerprint>, Instant)> = None;
        loop {
            match stop_rx.recv_timeout(FILE_STABILITY_POLL_INTERVAL) {
                Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            }
            let current = file_fingerprint(&path);
            if current != observed {
                observed = current;
                pending = Some((current, Instant::now()));
                continue;
            }
            if pending.as_ref().is_some_and(|(fingerprint, since)| {
                *fingerprint == current && since.elapsed() >= FILE_STABILITY_THRESHOLD
            }) {
                pending = None;
                emit_keybinding_change(&reload_keybindings_sync_with_warnings());
            }
        }
    });
    Some(KeybindingWatcher {
        stop_tx: Some(stop_tx),
        thread: Some(thread),
    })
}

/// Maps to: CC `isKeybindingCustomizationEnabled()`.
pub fn is_keybinding_customization_enabled() -> bool {
    crate::utils::feature_flags::feature_enabled(
        crate::utils::feature_flags::FeatureFlag::KeybindingCustomization,
    )
}

/// Maps to: CC `getKeybindingsPath()`.
pub fn get_keybindings_path() -> PathBuf {
    crate::utils::env_utils::get_claude_config_home_dir().join("keybindings.json")
}

fn parse_error(
    message: impl Into<String>,
    suggestion: Option<impl Into<String>>,
) -> Vec<KeybindingWarning> {
    vec![KeybindingWarning::error(message, suggestion)]
}

fn parse_user_bindings(
    content: &str,
) -> Result<(Vec<ParsedBinding>, Vec<KeybindingWarning>), Vec<KeybindingWarning>> {
    let parsed = serde_json::from_str::<Value>(content).map_err(|error| {
        parse_error(
            format!("Failed to parse keybindings.json: {error}"),
            None::<String>,
        )
    })?;
    let Some(blocks) = parsed.get("bindings") else {
        return Err(parse_error(
            "keybindings.json must have a \"bindings\" array",
            Some("Use format: { \"bindings\": [ ... ] }"),
        ));
    };
    let Some(blocks) = blocks.as_array() else {
        return Err(parse_error(
            "\"bindings\" must be an array",
            Some("Set \"bindings\" to an array of keybinding blocks"),
        ));
    };

    let validation_warnings = super::validate::validate_user_bindings(content, blocks);
    let mut parsed_bindings = Vec::new();
    for block in blocks {
        let Some(context) = block.get("context").and_then(Value::as_str) else {
            return Err(parse_error(
                "keybindings.json contains invalid block structure",
                Some("Each block must have \"context\" (string) and \"bindings\" (object)"),
            ));
        };
        let Some(bindings) = block.get("bindings").and_then(Value::as_object) else {
            return Err(parse_error(
                "keybindings.json contains invalid block structure",
                Some("Each block must have \"context\" (string) and \"bindings\" (object)"),
            ));
        };
        for (keys, action) in bindings {
            let action = match action {
                Value::String(action) => Some(action.clone()),
                Value::Null => None,
                // CC validation reports this binding and preserves the other
                // structurally valid entries in the same file.
                _ => continue,
            };
            parsed_bindings.push(ParsedBinding {
                chord: parse_chord(keys),
                action,
                context: ContextName::from_official_str(context),
            });
        }
    }
    Ok((parsed_bindings, validation_warnings))
}

fn load_uncached(report_io_or_json_errors: bool) -> KeybindingsLoadResult {
    let defaults = default_bindings();
    if !is_keybinding_customization_enabled() {
        return KeybindingsLoadResult {
            bindings: defaults,
            warnings: Vec::new(),
        };
    }
    let path = get_keybindings_path();
    let content = match std::fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return KeybindingsLoadResult {
                bindings: defaults,
                warnings: Vec::new(),
            };
        }
        Err(error) => {
            return KeybindingsLoadResult {
                bindings: defaults,
                warnings: if report_io_or_json_errors {
                    parse_error(
                        format!("Failed to parse keybindings.json: {error}"),
                        None::<String>,
                    )
                } else {
                    Vec::new()
                },
            };
        }
    };
    match parse_user_bindings(&content) {
        Ok((user, warnings)) => {
            let mut bindings = defaults;
            bindings.extend(user);
            KeybindingsLoadResult { bindings, warnings }
        }
        Err(warnings) => KeybindingsLoadResult {
            bindings: defaults,
            warnings: if !report_io_or_json_errors
                && warnings.iter().any(|warning| {
                    warning
                        .message
                        .starts_with("Failed to parse keybindings.json:")
                }) {
                Vec::new()
            } else {
                warnings
            },
        },
    }
}

/// Maps to: CC `loadKeybindingsSyncWithWarnings()`.
pub fn load_keybindings_sync_with_warnings() -> KeybindingsLoadResult {
    if let Some(cached) = CACHED_RESULT
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
    {
        return cached;
    }
    let result = load_uncached(false);
    *CACHED_RESULT
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(result.clone());
    result
}

pub fn load_keybindings_sync() -> Vec<ParsedBinding> {
    load_keybindings_sync_with_warnings().bindings
}

/// Explicit reload used after `/keybindings` returns from the external editor.
pub fn reload_keybindings_sync_with_warnings() -> KeybindingsLoadResult {
    let result = load_uncached(true);
    *CACHED_RESULT
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(result.clone());
    result
}

pub fn get_cached_keybinding_warnings() -> Vec<KeybindingWarning> {
    CACHED_RESULT
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_ref()
        .map(|result| result.warnings.clone())
        .unwrap_or_default()
}

/// Emits a reload as the watcher would, without touching the disk.
#[cfg(test)]
pub fn emit_keybinding_change_for_testing(result: KeybindingsLoadResult) {
    emit_keybinding_change(&result);
}

/// How many `subscribe_to_keybinding_changes` registrations are live.
#[cfg(test)]
pub fn keybinding_change_subscriber_count_for_testing() -> usize {
    CHANGE_SUBSCRIBERS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .len()
}

/// Seeds the loader cache the way a `keybindings.json` would, for tests that
/// mount a `KeybindingSetup` (which reads this cache) with custom bindings.
#[cfg(test)]
pub fn set_cached_keybindings_for_testing(bindings: Vec<ParsedBinding>) {
    *CACHED_RESULT
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(KeybindingsLoadResult {
        bindings,
        warnings: Vec::new(),
    });
}

#[cfg(test)]
pub fn reset_keybinding_loader_for_testing() {
    *CACHED_RESULT
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keybindings::parser::parse_keystroke;
    use crate::keybindings::resolver::resolve_key_with_chord_state;
    use crate::keybindings::types::ChordResolveResult;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};
    use std::collections::HashSet;

    struct RestoreConfigDir {
        value: Option<EnvVarGuard>,
        root: PathBuf,
    }

    impl Drop for RestoreConfigDir {
        fn drop(&mut self) {
            drop(self.value.take());
            reset_keybinding_loader_for_testing();
            crate::utils::config::clear_global_config_cache_for_testing();
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn user_bindings_append_after_defaults_and_override_last_wins() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-keybindings-loader-{}",
            uuid::Uuid::new_v4()
        ));
        let _restore = RestoreConfigDir {
            value: Some(EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root)),
            root: root.clone(),
        };
        crate::utils::config::clear_global_config_cache_for_testing();
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("keybindings.json"),
            r#"{"bindings":[{"context":"Chat","bindings":{"enter":"chat:cancel"}}]}"#,
        )
        .unwrap();
        reset_keybinding_loader_for_testing();

        let result = load_keybindings_sync_with_warnings();
        assert!(result.warnings.is_empty());
        let contexts = HashSet::from([ContextName::Global, ContextName::Chat]);
        assert_eq!(
            resolve_key_with_chord_state(
                Some(&parse_keystroke("enter")),
                false,
                &contexts,
                &result.bindings,
                None,
            ),
            ChordResolveResult::Match {
                action: "chat:cancel".to_string()
            }
        );
    }

    #[test]
    fn initial_sync_parse_failure_is_silent_but_watcher_reload_reports_it() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-keybindings-parse-warning-{}",
            uuid::Uuid::new_v4()
        ));
        let _restore = RestoreConfigDir {
            value: Some(EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root)),
            root: root.clone(),
        };
        crate::utils::config::clear_global_config_cache_for_testing();
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("keybindings.json"), "{not json").unwrap();
        reset_keybinding_loader_for_testing();

        assert!(load_keybindings_sync_with_warnings().warnings.is_empty());
        let reloaded = reload_keybindings_sync_with_warnings();
        assert_eq!(reloaded.warnings.len(), 1);
        assert!(
            reloaded.warnings[0]
                .message
                .starts_with("Failed to parse keybindings.json:")
        );
    }

    #[test]
    fn loader_caches_official_validation_warnings_for_doctor() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-keybindings-warnings-{}",
            uuid::Uuid::new_v4()
        ));
        let _restore = RestoreConfigDir {
            value: Some(EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root)),
            root: root.clone(),
        };
        crate::utils::config::clear_global_config_cache_for_testing();
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("keybindings.json"),
            r#"{"bindings":[{"context":"Chat","bindings":{"ctrl+c":"chat:cancel"}}]}"#,
        )
        .unwrap();
        reset_keybinding_loader_for_testing();

        let result = load_keybindings_sync_with_warnings();
        assert!(
            result
                .warnings
                .iter()
                .any(|warning| warning.message.contains("Cannot be rebound"))
        );
        assert_eq!(get_cached_keybinding_warnings(), result.warnings);
    }

    #[test]
    fn watcher_reloads_after_stable_write_without_retained_frame_io() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-keybindings-watcher-{}",
            uuid::Uuid::new_v4()
        ));
        let _restore = RestoreConfigDir {
            value: Some(EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root)),
            root: root.clone(),
        };
        crate::utils::config::clear_global_config_cache_for_testing();
        std::fs::create_dir_all(&root).unwrap();
        reset_keybinding_loader_for_testing();

        // CC: the watcher emits to every `subscribeToKeybindingChanges`
        // subscriber (loadUserBindings.ts:424-437).
        let (_subscription, mut changes) = subscribe_to_keybinding_changes();
        let mut next_change = |what: &str| -> KeybindingsLoadResult {
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                if let Ok(result) = changes.try_recv() {
                    return result;
                }
                assert!(Instant::now() < deadline, "{what}");
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        let watcher =
            initialize_keybinding_watcher().expect("existing config directory should be watched");
        std::fs::write(
            root.join("keybindings.json"),
            r#"{"bindings":[{"context":"Chat","bindings":{"enter":"chat:cancel"}}]}"#,
        )
        .unwrap();

        let result = next_change("stable write should trigger reload");
        assert!(result.bindings.iter().any(|binding| {
            binding.context == ContextName::Chat
                && binding.action.as_deref() == Some("chat:cancel")
                && binding.chord == parse_chord("enter")
        }));

        std::fs::remove_file(root.join("keybindings.json")).unwrap();
        let deleted = next_change("stable deletion should restore defaults");
        assert!(!deleted.bindings.iter().any(|binding| {
            binding.context == ContextName::Chat
                && binding.action.as_deref() == Some("chat:cancel")
                && binding.chord == parse_chord("enter")
        }));
        assert!(deleted.warnings.is_empty());
        drop(watcher);
    }

    #[test]
    fn malformed_wrapper_uses_official_warning_copy() {
        let warnings = parse_user_bindings("{}").unwrap_err();
        assert_eq!(
            warnings[0].message,
            "keybindings.json must have a \"bindings\" array"
        );
        assert_eq!(
            warnings[0].suggestion.as_deref(),
            Some("Use format: { \"bindings\": [ ... ] }")
        );
    }
}
