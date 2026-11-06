//! Maps to: CC `utils/settings/settingsCache.ts`.
//!
//! Three independent caches plus the plugin base layer, all invalidated by
//! the single [`reset_settings_cache`] entry point (CC :55-59). The reset is
//! driven by one producer — the settings change detector's fan-out — because
//! resetting per consumer caused N-way thrashing in CC (each listener cleared
//! the cache, re-read from disk, then the next cleared it again; see
//! `changeDetector.ts:420-436`).
//!
//! Process-global like the CC module scope. Tests that mutate settings on
//! disk must call [`reset_settings_cache`] (the `TEST_ENV_LOCK` guards
//! already do so through their teardown).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

use super::constants::SettingSource;
use super::types::SettingsJson;
use super::validation::{SettingsWithErrors, ValidationError};

/// Maps to: CC `parseSettingsFile`'s return shape cached at
/// `settingsCache.ts:41-44` `ParsedSettings`.
#[derive(Clone, Debug, Default)]
pub struct ParsedSettings {
    pub settings: Option<SettingsJson>,
    /// Complete Zod output, including unknown keys and absence of optional keys.
    pub(crate) value: Option<serde_json::Value>,
    pub errors: Vec<ValidationError>,
}

#[derive(Default)]
struct SettingsCaches {
    /// CC `sessionSettingsCache` (:5).
    session: Option<SettingsWithErrors>,
    /// CC `perSourceCache` (:20). `Some(None)` is a cached "no settings for
    /// this source"; an absent key is a miss — the same distinction CC draws
    /// between `null` and `undefined` at :26.
    per_source: HashMap<SettingSource, Option<SettingsJson>>,
    /// CC `parseFileCache` (:45), keyed by path.
    parsed_files: HashMap<PathBuf, ParsedSettings>,
    /// CC `pluginSettingsBase` (:66) — the lowest-priority cascade layer that
    /// the plugin loader writes and `loadSettingsFromDisk` reads.
    plugin_base: Option<SettingsJson>,
}

fn caches() -> MutexGuard<'static, SettingsCaches> {
    static CACHES: OnceLock<Mutex<SettingsCaches>> = OnceLock::new();
    CACHES
        .get_or_init(|| Mutex::new(SettingsCaches::default()))
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// Maps to: CC `getSessionSettingsCache` (:7-9).
pub fn get_session_settings_cache() -> Option<SettingsWithErrors> {
    caches().session.clone()
}

/// Maps to: CC `setSessionSettingsCache` (:11-13).
pub fn set_session_settings_cache(value: SettingsWithErrors) {
    caches().session = Some(value);
}

/// Test support: make `settings` the session's settings, as launch would.
#[cfg(test)]
pub(crate) fn install_test_settings(settings: SettingsJson) {
    set_session_settings_cache(SettingsWithErrors {
        settings,
        ..Default::default()
    });
}

/// Maps to: CC `getCachedSettingsForSource` (:22-27). The outer `Option` is
/// CC's miss (`undefined`); the inner one is the cached "no settings"
/// (`null`).
pub fn get_cached_settings_for_source(source: SettingSource) -> Option<Option<SettingsJson>> {
    caches().per_source.get(&source).cloned()
}

/// Maps to: CC `setCachedSettingsForSource` (:29-34).
pub fn set_cached_settings_for_source(source: SettingSource, value: Option<SettingsJson>) {
    caches().per_source.insert(source, value);
}

/// Maps to: CC `getCachedParsedFile` (:47-49).
pub fn get_cached_parsed_file(path: &Path) -> Option<ParsedSettings> {
    caches().parsed_files.get(path).cloned()
}

/// Maps to: CC `setCachedParsedFile` (:51-53).
pub fn set_cached_parsed_file(path: &Path, value: ParsedSettings) {
    caches().parsed_files.insert(path.to_path_buf(), value);
}

/// Maps to: CC `resetSettingsCache` (:55-59). Note the plugin base layer is
/// deliberately NOT cleared here — CC keeps it across resets and clears it
/// only through `clearPluginSettingsBase` (:78-80).
pub fn reset_settings_cache() {
    let mut caches = caches();
    caches.session = None;
    caches.per_source.clear();
    caches.parsed_files.clear();
}

/// Maps to: CC `getPluginSettingsBase` (:68-70).
pub fn get_plugin_settings_base() -> Option<SettingsJson> {
    caches().plugin_base.clone()
}

/// Maps to: CC `setPluginSettingsBase` (:72-76).
pub fn set_plugin_settings_base(settings: Option<SettingsJson>) {
    caches().plugin_base = settings;
}

/// Maps to: CC `clearPluginSettingsBase` (:78-80).
pub fn clear_plugin_settings_base() {
    caches().plugin_base = None;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Maps to: CC `settingsCache.ts:26` — an absent key is a miss
    /// (`undefined`), a stored `None` is a cached "no settings for this
    /// source" (`null`). Collapsing the two would re-read the disk forever
    /// for sources that legitimately have no file.
    #[test]
    fn per_source_cache_distinguishes_miss_from_cached_absence() {
        reset_settings_cache();
        assert!(get_cached_settings_for_source(SettingSource::Project).is_none());

        set_cached_settings_for_source(SettingSource::Project, None);
        assert_eq!(
            get_cached_settings_for_source(SettingSource::Project),
            Some(None),
            "cached absence must be distinguishable from a miss"
        );

        set_cached_settings_for_source(SettingSource::Project, Some(SettingsJson::default()));
        assert!(matches!(
            get_cached_settings_for_source(SettingSource::Project),
            Some(Some(_))
        ));
        reset_settings_cache();
    }

    /// Maps to: CC `resetSettingsCache` (:55-59) clearing the three caches
    /// while `pluginSettingsBase` survives (only :78-80 clears it).
    #[test]
    fn reset_clears_three_caches_but_keeps_plugin_base() {
        reset_settings_cache();
        clear_plugin_settings_base();

        set_session_settings_cache(SettingsWithErrors {
            settings: SettingsJson::default(),
            errors: Vec::new(),
            policy_settings: None,
        });
        set_cached_settings_for_source(SettingSource::User, Some(SettingsJson::default()));
        set_cached_parsed_file(Path::new("/tmp/settings.json"), ParsedSettings::default());
        set_plugin_settings_base(Some(SettingsJson::default()));

        reset_settings_cache();

        assert!(get_session_settings_cache().is_none());
        assert!(get_cached_settings_for_source(SettingSource::User).is_none());
        assert!(get_cached_parsed_file(Path::new("/tmp/settings.json")).is_none());
        assert!(
            get_plugin_settings_base().is_some(),
            "plugin base survives resetSettingsCache (CC :55-59 does not touch it)"
        );

        clear_plugin_settings_base();
        assert!(get_plugin_settings_base().is_none());
    }
}
