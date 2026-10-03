//! Maps to: CC `utils/settings/settings.ts`.
//!
//! 1. userSettings     — ~/.claude/settings.json
//! 2. projectSettings  — ${cwd}/.claude/settings.json
//! 3. localSettings    — ${cwd}/.claude/settings.local.json
//! 5. policySettings   — managed-settings.json + drop-in (Phase 2: remote, MDM, HKCU)
//! - [ ] Policy: HKCU (Windows user registry)
//! - [ ] Cowork mode: ~/.claude/cowork_settings.json

pub mod apply_settings_change;
pub mod change_detector;
pub mod constants;
mod file_io;
#[cfg(test)]
mod fs_io_tests;
pub(crate) mod internal_writes;
pub mod managed_path;
pub mod permission_validation;
pub mod plugin_only_policy;
pub mod schema_output;
pub mod settings_cache;
pub mod tool_validation_config;
pub mod types;
pub mod validate_edit_tool;
pub mod validation;
pub mod validation_tips;

pub use crate::bootstrap::state::{
    get_flag_settings_inline, get_flag_settings_path, set_flag_settings_inline,
    set_flag_settings_path,
};
pub use constants::{
    SettingSource, get_enabled_setting_sources, get_setting_source_name, is_setting_source_enabled,
    parse_setting_sources_flag,
};
pub use types::SettingsJson;
pub use validation::{
    SettingsWithErrors, ValidationError, filter_invalid_permission_rules, validate_settings,
    validate_settings_file_content,
};

use managed_path::{get_managed_file_path, get_managed_settings_drop_in_dir};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ManagedFileSettingsPresence {
    pub has_base: bool,
    pub has_drop_ins: bool,
}

// ════════════════════════════════════════════════════════════
// ════════════════════════════════════════════════════════════

/// User → ~/.claude/
/// Maps to: CC `utils/settings/settings.ts#getSettingsRootPathForSource`.
pub(crate) fn get_settings_root_path_for_source(source: SettingSource) -> PathBuf {
    match source {
        SettingSource::User => crate::utils::env_utils::get_claude_config_home_dir(),
        SettingSource::Project | SettingSource::Local | SettingSource::Policy => {
            crate::bootstrap::state::get_original_cwd()
        }
        SettingSource::Flag => get_flag_settings_path()
            .and_then(|path| path.parent().map(Path::to_path_buf))
            .unwrap_or_else(crate::bootstrap::state::get_original_cwd),
    }
}

/// Maps to: CC `utils/settings/settings.ts#getRelativeSettingsFilePathForSource`.
pub(crate) fn get_relative_settings_file_path_for_source(source: SettingSource) -> &'static str {
    match source {
        SettingSource::Project => ".claude/settings.json",
        SettingSource::Local => ".claude/settings.local.json",
        _ => "",
    }
}

///   User → configHome + getUserSettingsFilePath()
///   Project/Local → cwd + relative
///   Policy → getManagedSettingsFilePath()
///   Flag → getFlagSettingsPath()
/// Maps to: CC `utils/settings/settings.ts#getSettingsFilePathForSource`.
pub(crate) fn get_settings_file_path_for_source(source: SettingSource) -> Option<PathBuf> {
    match source {
        SettingSource::User => {
            let root = get_settings_root_path_for_source(source);
            // Phase 2: cowork mode → cowork_settings.json
            Some(root.join("settings.json"))
        }
        SettingSource::Project | SettingSource::Local => {
            let root = get_settings_root_path_for_source(source);
            let relative = get_relative_settings_file_path_for_source(source);
            Some(root.join(relative))
        }
        SettingSource::Policy => Some(get_managed_settings_file_path()),
        SettingSource::Flag => get_flag_settings_path(),
    }
}

/// Maps to: CC `utils/settings/settings.ts#updateSettingsForSource` for editable sources.
/// Existing unknown fields are preserved; supplied keys are deep-merged by
/// `apply_settings_update` exactly like the source's `mergeWith` customizer
/// (:473-495). Policy and flag sources are intentionally not writable.
pub fn update_settings_for_source(
    source: SettingSource,
    updates: &serde_json::Map<String, serde_json::Value>,
) -> anyhow::Result<()> {
    file_io::update_settings_for_source(source, updates)
}

/// Maps to: CC `utils/settings/settings.ts:473-495` — the inline `mergeWith`
/// customizer `updateSettingsForSource` hands to lodash. lodash re-invokes the
/// customizer for every key it walks, so all three rules apply at **any**
/// depth, not just at the top level:
///
/// 1. `srcValue === undefined` (:483-486) → `delete object[key]`. L1
///    representation: Rust `Value::Null` carries JavaScript's explicit
///    `undefined` patch value; settings fields are not nullable. (lodash then
///    re-assigns `undefined` to the deleted key via `assignMergeValue`, which
///    `jsonStringify` at :502 drops again — so removing the key is the same
///    on-disk result. `shift_remove` keeps the surviving keys in order like
///    JavaScript `delete`, which plain `remove` would not under
///    `preserve_order`.)
/// 2. `Array.isArray(srcValue)` (:489-491) → the patch array replaces the
///    existing one wholesale; the comment at :488 puts computing the final
///    state on the caller. Note this is the *opposite* of
///    `settings_merge_customizer` (:538-547), which concatenates when merging
///    settings *sources* together.
/// 3. anything else (:492-493) → lodash's default merge: plain object over
///    plain object recurses (`baseMergeDeep`), every other pair is overwritten
///    by assignment (`assignMergeValue`). This is the rule that keeps
///    `permissions.allow/deny/ask/additionalDirectories` alive when a caller
///    patches only `permissions.defaultMode` (CC `ConfigTool.ts:330-331`
///    writes `buildNestedObject(path, value)`, a partial nested object).
fn apply_settings_update(
    target: &mut serde_json::Map<String, serde_json::Value>,
    updates: &serde_json::Map<String, serde_json::Value>,
) {
    for (key, update) in updates {
        match update {
            serde_json::Value::Null => {
                target.shift_remove(key);
            }
            serde_json::Value::Object(update_object) => {
                // lodash `baseMergeDeep`: the merge base is the existing value
                // when it is an object, and a fresh `{}` otherwise — so an
                // object patch over a scalar (or over a missing key) replaces
                // it, while nested `undefined`s inside the patch still delete.
                let existing = target
                    .entry(key.clone())
                    .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
                if !existing.is_object() && !existing.is_array() {
                    *existing = serde_json::Value::Object(serde_json::Map::new());
                }
                match existing {
                    serde_json::Value::Object(existing_object) => {
                        apply_settings_update(existing_object, update_object);
                    }
                    // Object patch over an array: lodash keeps the array as the
                    // merge base and hangs the patch's string keys off it,
                    // which `jsonStringify` then drops. `ConfigTool` only
                    // accepts paths from its supported-settings table
                    // (`supportedSettings.ts#getPath`) and none of them nests
                    // under an array, so the array is left as-is.
                    _ => {}
                }
            }
            // Arrays (rule 2) and scalars (default assignment) both overwrite.
            _ => {
                target.insert(key.clone(), update.clone());
            }
        }
    }
}

fn parse_flag_settings_inline() -> Option<SettingsJson> {
    let inline = get_flag_settings_inline()?;
    let content = serde_json::to_string(&inline).ok()?;
    let (settings, errors) =
        validation::validate_settings(&content, Some("flagSettingsInline")).ok()?;
    if errors.is_empty() { settings } else { None }
}

// ════════════════════════════════════════════════════════════
// ════════════════════════════════════════════════════════════

/// Maps to: CC `utils/settings/settings.ts#getManagedSettingsFilePath`.
pub fn get_managed_settings_file_path() -> PathBuf {
    get_managed_file_path().join("managed-settings.json")
}

fn settings_value_has_fields(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => false,
        serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_) => true,
        serde_json::Value::Array(_) => true,
        serde_json::Value::Object(object) => object.values().any(settings_value_has_fields),
    }
}

fn settings_has_fields(settings: &SettingsJson) -> bool {
    serde_json::to_value(settings)
        .map(|value| settings_value_has_fields(&value))
        .unwrap_or(false)
}

fn settings_file_has_fields(path: &Path) -> bool {
    parse_settings_file(path)
        .0
        .as_ref()
        .is_some_and(settings_has_fields)
}

/// Maps to: CC `utils/settings/settings.ts#getManagedFileSettingsPresence`.
pub fn get_managed_file_settings_presence() -> ManagedFileSettingsPresence {
    let has_base = settings_file_has_fields(&get_managed_settings_file_path());
    let mut has_drop_ins = false;

    if let Ok(entries) = crate::utils::fs_operations::get_fs_implementation()
        .readdir_sync(&get_managed_settings_drop_in_dir())
    {
        has_drop_ins = entries.into_iter().any(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            entry
                .file_type()
                .is_ok_and(|kind| kind.is_file() || kind.is_symlink())
                && name.ends_with(".json")
                && !name.starts_with('.')
        });
    }

    ManagedFileSettingsPresence {
        has_base,
        has_drop_ins,
    }
}

// ════════════════════════════════════════════════════════════
// ════════════════════════════════════════════════════════════

/// Maps to: CC `utils/settings/settings.ts#loadSettingsFromDisk`.
pub fn load_settings_from_disk() -> SettingsWithErrors {
    let mut merged = SettingsJson::default();
    let mut all_errors: Vec<ValidationError> = Vec::new();
    let mut seen_errors: HashSet<String> = HashSet::new();
    let mut seen_files: HashSet<PathBuf> = HashSet::new();
    let mut policy_settings_snapshot: Option<SettingsJson> = None;

    for source in get_enabled_setting_sources() {
        if source == SettingSource::Policy {
            let (policy_settings, policy_errors) = load_managed_file_settings();
            collect_errors(&policy_errors, &mut all_errors, &mut seen_errors);
            if let Some(ps) = policy_settings {
                policy_settings_snapshot = Some(ps.clone());
                merge_settings_deep(&mut merged, &ps);
            }
            continue;
        }

        let Some(file_path) = get_settings_file_path_for_source(source) else {
            if source == SettingSource::Flag {
                if let Some(inline_settings) = parse_flag_settings_inline() {
                    merge_settings_deep(&mut merged, &inline_settings);
                }
            }
            continue;
        };

        let resolved = file_path
            .canonicalize()
            .unwrap_or_else(|_| file_path.clone());
        if seen_files.contains(&resolved) {
            continue;
        }
        seen_files.insert(resolved);

        let (settings, errors) = parse_settings_file(&file_path);
        collect_errors(&errors, &mut all_errors, &mut seen_errors);

        if let Some(s) = settings {
            merge_settings_deep(&mut merged, &s);
        }

        if source == SettingSource::Flag {
            if let Some(inline_settings) = parse_flag_settings_inline() {
                merge_settings_deep(&mut merged, &inline_settings);
            }
        }
    }

    SettingsWithErrors {
        settings: merged,
        errors: all_errors,
        policy_settings: policy_settings_snapshot,
    }
}

/// Maps to: CC `utils/settings/settings.ts:812-815 getInitialSettings` —
/// `const { settings } = getSettingsWithErrors(); return settings || {}`.
///
/// Reads through the session cache exactly like the source. It previously
/// called `load_settings_from_disk()` directly, which bypassed the cache and
/// made every consumer (including the per-notification read inside
/// `use_settings_change`) pay a full multi-source disk read.
pub fn get_initial_settings() -> SettingsJson {
    get_settings_with_errors().settings
}

/// Maps to: CC `utils/settings/settings.ts#getSettingsWithErrors` — reads
/// through the session cache (`settingsCache.ts:5-13`), so one settings
/// change notification costs exactly one disk reload no matter how many
/// consumers read afterwards (`changeDetector.ts:420-436`).
pub fn get_settings_with_errors() -> SettingsWithErrors {
    if let Some(cached) = settings_cache::get_session_settings_cache() {
        return cached;
    }
    let loaded = load_settings_from_disk();
    // Warn on the miss only: this used to sit in `get_initial_settings`, where
    // it re-emitted the same validation errors on every read.
    for err in &loaded.errors {
        tracing::warn!("Settings validation: {}", err);
    }
    settings_cache::set_session_settings_cache(loaded.clone());
    loaded
}

/// Load settings from a single source without merging other sources.
/// Maps to: CC `utils/settings/settings.ts#getSettingsForSource`, reading
/// through the per-source cache (`settingsCache.ts:20-34`).
pub fn get_settings_for_source(source: SettingSource) -> Option<SettingsJson> {
    if let Some(cached) = settings_cache::get_cached_settings_for_source(source) {
        return cached;
    }
    let loaded = load_settings_for_source_uncached(source);
    settings_cache::set_cached_settings_for_source(source, loaded.clone());
    loaded
}

/// Maps to: CC settings.ts:936-980 `getAutoModeConfig`.
/// Only trusted source settings contribute; preserve source order and duplicates.
/// The source's local array schema is represented by the already validated
/// `AutoModeSettings` fields (each is an optional `Vec<String>`).
pub fn get_auto_mode_config() -> Option<types::AutoModeSettings> {
    if !crate::utils::feature_flags::feature_enabled(
        crate::utils::feature_flags::FeatureFlag::TranscriptClassifier,
    ) {
        return None;
    }
    let mut allow = Vec::new();
    let mut soft_deny = Vec::new();
    let mut environment = Vec::new();
    for source in [
        SettingSource::User,
        SettingSource::Local,
        SettingSource::Flag,
        SettingSource::Policy,
    ] {
        let Some(auto_mode) =
            get_settings_for_source(source).and_then(|settings| settings.auto_mode)
        else {
            continue;
        };
        allow.extend(auto_mode.allow.unwrap_or_default());
        soft_deny.extend(auto_mode.soft_deny.unwrap_or_default());
        if crate::utils::build_profile::build_audience().is_internal() {
            soft_deny.extend(auto_mode.deny.unwrap_or_default());
        }
        environment.extend(auto_mode.environment.unwrap_or_default());
    }
    if allow.is_empty() && soft_deny.is_empty() && environment.is_empty() {
        return None;
    }
    Some(types::AutoModeSettings {
        allow: (!allow.is_empty()).then_some(allow),
        soft_deny: (!soft_deny.is_empty()).then_some(soft_deny),
        deny: None,
        environment: (!environment.is_empty()).then_some(environment),
    })
}

/// Official GrowthBook `tengu_prompt_cache_1h_config.allowlist`.
///
/// @cometix offset: GrowthBook is not ported. Users set
/// `promptCache1h.allowlist` in settings.json (same `{ allowlist: string[] }`
/// shape, trailing `*` prefix match). Later enabled sources override earlier
/// ones so the merged value stays a single payload, not a concatenated union.
pub fn get_prompt_cache_1h_allowlist() -> Vec<String> {
    let mut allowlist = Vec::new();
    for source in get_enabled_setting_sources() {
        if let Some(list) = get_settings_for_source(source)
            .and_then(|settings| settings.prompt_cache_1h)
            .and_then(|config| config.allowlist)
        {
            allowlist = list;
        }
    }
    allowlist
}

fn load_settings_for_source_uncached(source: SettingSource) -> Option<SettingsJson> {
    match source {
        SettingSource::Policy => load_managed_file_settings().0,
        SettingSource::Flag => {
            let mut merged = SettingsJson::default();
            let mut found = false;
            if let Some(path) = get_settings_file_path_for_source(source) {
                if let Some(settings) = parse_settings_file(&path).0 {
                    merge_settings_deep(&mut merged, &settings);
                    found = true;
                }
            }
            if let Some(inline_settings) = parse_flag_settings_inline() {
                merge_settings_deep(&mut merged, &inline_settings);
                found = true;
            }
            found.then_some(merged)
        }
        _ => {
            get_settings_file_path_for_source(source).and_then(|path| parse_settings_file(&path).0)
        }
    }
}

/// Maps to: CC `utils/settings/settings.ts#hasSkipDangerousModePermissionPrompt`.
pub fn has_skip_dangerous_mode_permission_prompt() -> bool {
    [
        SettingSource::User,
        SettingSource::Local,
        SettingSource::Flag,
        SettingSource::Policy,
    ]
    .into_iter()
    .filter(|source| is_setting_source_enabled(*source))
    .any(|source| {
        get_settings_for_source(source)
            .and_then(|settings| settings.skip_dangerous_mode_permission_prompt)
            == Some(true)
    })
}

// ════════════════════════════════════════════════════════════
// ════════════════════════════════════════════════════════════

/// Maps to: CC `utils/settings/settings.ts#loadManagedFileSettings`.
fn load_managed_file_settings() -> (Option<SettingsJson>, Vec<ValidationError>) {
    let mut errors: Vec<ValidationError> = Vec::new();
    let mut merged = SettingsJson::default();
    let mut found = false;

    // 1. Base: managed-settings.json
    let base_path = get_managed_settings_file_path();
    let (base_settings, base_errors) = parse_settings_file(&base_path);
    errors.extend(base_errors);
    if let Some(s) = base_settings.filter(settings_has_fields) {
        merge_settings_deep(&mut merged, &s);
        found = true;
    }

    let drop_in_dir = get_managed_settings_drop_in_dir();
    if let Ok(entries) =
        crate::utils::fs_operations::get_fs_implementation().readdir_sync(&drop_in_dir)
    {
        let mut json_files: Vec<PathBuf> = entries
            .into_iter()
            .filter(|e| {
                let name = e.file_name();
                let name_str = name.to_string_lossy();
                e.file_type()
                    .is_ok_and(|kind| kind.is_file() || kind.is_symlink())
                    && name_str.ends_with(".json")
                    && !name_str.starts_with('.')
            })
            .map(|e| e.path())
            .collect();

        json_files.sort();

        for file_path in json_files {
            let (settings, file_errors) = parse_settings_file(&file_path);
            errors.extend(file_errors);
            if let Some(s) = settings.filter(settings_has_fields) {
                merge_settings_deep(&mut merged, &s);
                found = true;
            }
        }
    }

    if found {
        (Some(merged), errors)
    } else {
        (None, errors)
    }
}

// ════════════════════════════════════════════════════════════
// ════════════════════════════════════════════════════════════

/// Maps to: CC `utils/settings/settings.ts#parseSettingsFile`, reading
/// through the path-keyed cache (`settingsCache.ts:37-53`) — both
/// `getSettingsForSource` and `loadSettingsFromDisk` parse the same paths
/// during startup, and this dedupes the read + validation.
pub(crate) fn parse_settings_file(path: &Path) -> (Option<SettingsJson>, Vec<ValidationError>) {
    if let Some(cached) = settings_cache::get_cached_parsed_file(path) {
        return (cached.settings, cached.errors);
    }
    let parsed = file_io::parse_settings_file_uncached(path);
    let result = (parsed.settings.clone(), parsed.errors.clone());
    settings_cache::set_cached_parsed_file(path, parsed);
    result
}

// ════════════════════════════════════════════════════════════
// ════════════════════════════════════════════════════════════

/// Maps to: CC `utils/settings/settings.ts:529-531` `mergeArrays`.
fn merge_arrays(
    target_array: &[serde_json::Value],
    source_array: &[serde_json::Value],
) -> Vec<serde_json::Value> {
    let mut merged = Vec::with_capacity(target_array.len() + source_array.len());
    for value in target_array.iter().chain(source_array) {
        // Settings are JSON-shaped, so object/array references cannot alias
        // across independently parsed sources. Lodash `uniq` therefore keeps
        // every compound value and deduplicates only JSON primitives here.
        let is_compound = value.is_array() || value.is_object();
        if is_compound || !merged.iter().any(|existing| existing == value) {
            merged.push(value.clone());
        }
    }
    merged
}

/// Maps to: CC `utils/settings/settings.ts:538-547` `settingsMergeCustomizer`.
pub(crate) fn settings_merge_customizer(
    obj_value: &serde_json::Value,
    src_value: &serde_json::Value,
) -> Option<serde_json::Value> {
    match (obj_value.as_array(), src_value.as_array()) {
        (Some(target), Some(source)) => {
            Some(serde_json::Value::Array(merge_arrays(target, source)))
        }
        _ => None,
    }
}

/// Narrow Rust implementation of the external `lodash-es/mergeWith.js`
/// dependency invoked by CC `utils/settings/settings.ts` with
/// `settingsMergeCustomizer`; it owns recursion only, not settings policy.
fn merge_with_settings_customizer(target: &mut serde_json::Value, source: &serde_json::Value) {
    if let Some(customized) = settings_merge_customizer(target, source) {
        *target = customized;
        return;
    }

    match (target, source) {
        (serde_json::Value::Object(target), serde_json::Value::Object(source)) => {
            for (key, source_value) in source {
                if let Some(target_value) = target.get_mut(key) {
                    merge_with_settings_customizer(target_value, source_value);
                } else {
                    target.insert(key.clone(), source_value.clone());
                }
            }
        }
        (target, source) => *target = source.clone(),
    }
}

pub(crate) fn strip_absent_settings_fields(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(object) => object.retain(|_, value| {
            if value.is_null() {
                return false;
            }
            strip_absent_settings_fields(value);
            true
        }),
        serde_json::Value::Array(array) => {
            for value in array {
                strip_absent_settings_fields(value);
            }
        }
        _ => {}
    }
}

fn merge_settings_deep(target: &mut SettingsJson, source: &SettingsJson) {
    let mut target_value =
        serde_json::to_value(&*target).expect("SettingsJson must serialize to JSON");
    let mut source_value =
        serde_json::to_value(source).expect("SettingsJson must serialize to JSON");
    // `SettingsJson` uses `Option` fields without per-field serde omission.
    // Remove their transport `null`s before applying lodash-style merge so a
    // missing CC property cannot overwrite a lower-priority source.
    strip_absent_settings_fields(&mut source_value);
    merge_with_settings_customizer(&mut target_value, &source_value);
    *target = serde_json::from_value(target_value).expect("merged SettingsJson must deserialize");
}

// ════════════════════════════════════════════════════════════
// ════════════════════════════════════════════════════════════

fn collect_errors(
    new_errors: &[ValidationError],
    all_errors: &mut Vec<ValidationError>,
    seen: &mut HashSet<String>,
) {
    for err in new_errors {
        let key = format!(
            "{}:{}:{}",
            err.file.as_deref().unwrap_or(""),
            err.path,
            err.message
        );
        if seen.insert(key) {
            all_errors.push(err.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::settings::constants::get_setting_source_display_name_capitalized;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK, TestEnvLock};
    use std::fs;

    #[test]
    fn auto_mode_config_matches_official_trusted_source_order_and_alias() {
        let _lock = env_lock().lock().unwrap();
        settings_cache::reset_settings_cache();
        struct ResetCache;
        impl Drop for ResetCache {
            fn drop(&mut self) {
                settings_cache::reset_settings_cache();
            }
        }
        let _reset = ResetCache;
        for (source, name) in [
            (SettingSource::User, "user"),
            (SettingSource::Local, "local"),
            (SettingSource::Flag, "flag"),
            (SettingSource::Policy, "policy"),
            (SettingSource::Project, "project"),
        ] {
            settings_cache::set_cached_settings_for_source(
                source,
                Some(SettingsJson {
                    auto_mode: Some(types::AutoModeSettings {
                        allow: Some(vec![name.into(), "duplicate".into()]),
                        soft_deny: Some(vec![format!("{name}-soft")]),
                        deny: Some(vec![format!("{name}-legacy")]),
                        environment: Some(vec![format!("{name}-environment")]),
                    }),
                    ..Default::default()
                }),
            );
        }
        let result = get_auto_mode_config();
        if !crate::utils::feature_flags::feature_enabled(
            crate::utils::feature_flags::FeatureFlag::TranscriptClassifier,
        ) {
            assert!(result.is_none());
            return;
        }
        let result = result.unwrap();
        assert_eq!(
            result.allow,
            Some(vec![
                "user".into(),
                "duplicate".into(),
                "local".into(),
                "duplicate".into(),
                "flag".into(),
                "duplicate".into(),
                "policy".into(),
                "duplicate".into()
            ])
        );
        let mut expected = Vec::new();
        for name in ["user", "local", "flag", "policy"] {
            expected.push(format!("{name}-soft"));
            if crate::utils::build_profile::build_audience().is_internal() {
                expected.push(format!("{name}-legacy"));
            }
        }
        assert_eq!(result.soft_deny, Some(expected));
        assert!(result.deny.is_none());
        assert_eq!(
            result.environment,
            Some(vec![
                "user-environment".into(),
                "local-environment".into(),
                "flag-environment".into(),
                "policy-environment".into()
            ])
        );
        for source in [
            SettingSource::User,
            SettingSource::Local,
            SettingSource::Flag,
            SettingSource::Policy,
        ] {
            settings_cache::set_cached_settings_for_source(source, None);
        }
        assert!(
            get_auto_mode_config().is_none(),
            "project-only config must not contribute"
        );
    }

    #[test]
    fn prompt_cache_1h_allowlist_later_source_overrides() {
        let _lock = env_lock().lock().unwrap();
        settings_cache::reset_settings_cache();
        struct ResetCache;
        impl Drop for ResetCache {
            fn drop(&mut self) {
                settings_cache::reset_settings_cache();
            }
        }
        let _reset = ResetCache;
        for source in [
            SettingSource::User,
            SettingSource::Project,
            SettingSource::Local,
            SettingSource::Flag,
            SettingSource::Policy,
        ] {
            settings_cache::set_cached_settings_for_source(source, None);
        }
        settings_cache::set_cached_settings_for_source(
            SettingSource::User,
            Some(SettingsJson {
                prompt_cache_1h: Some(types::PromptCache1hSettings {
                    allowlist: Some(vec!["sdk".into()]),
                }),
                ..Default::default()
            }),
        );
        settings_cache::set_cached_settings_for_source(
            SettingSource::Policy,
            Some(SettingsJson {
                prompt_cache_1h: Some(types::PromptCache1hSettings {
                    allowlist: Some(vec!["repl_main_thread*".into()]),
                }),
                ..Default::default()
            }),
        );
        assert_eq!(
            get_prompt_cache_1h_allowlist(),
            vec!["repl_main_thread*".to_string()]
        );
        settings_cache::set_cached_settings_for_source(SettingSource::Policy, None);
        assert_eq!(get_prompt_cache_1h_allowlist(), vec!["sdk".to_string()]);
        settings_cache::set_cached_settings_for_source(SettingSource::User, None);
        assert!(get_prompt_cache_1h_allowlist().is_empty());
    }

    #[test]
    fn settings_source_matches_official_schema_failure_and_auto_mode_field_spelling() {
        let _lock = env_lock().lock().unwrap();
        let root =
            std::env::temp_dir().join(format!("cc-settings-source-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("settings.json");
        fs::write(
            &path,
            r#"{"model":42,"permissions":{"allow":["Read","Bash()"]}}"#,
        )
        .unwrap();
        settings_cache::reset_settings_cache();
        let (settings, errors) = parse_settings_file(&path);
        assert!(settings.is_none());
        assert_eq!(
            errors
                .iter()
                .map(|error| error.path.as_str())
                .collect::<Vec<_>>(),
            vec!["permissions.allow", "model"]
        );
        let (cached, errors) = parse_settings_file(&path);
        assert!(cached.is_none());
        assert_eq!(errors.len(), 2);
        fs::write(
            &path,
            r#"{"autoMode":{"soft_deny":["ask before deleting"]}}"#,
        )
        .unwrap();
        settings_cache::reset_settings_cache();
        let (settings, errors) = parse_settings_file(&path);
        assert!(errors.is_empty());
        if crate::utils::feature_flags::feature_enabled(
            crate::utils::feature_flags::FeatureFlag::TranscriptClassifier,
        ) {
            let auto_mode = settings.unwrap().auto_mode.unwrap();
            assert_eq!(
                auto_mode.soft_deny,
                Some(vec!["ask before deleting".into()])
            );
            let encoded = serde_json::to_value(auto_mode).unwrap();
            assert_eq!(
                encoded["soft_deny"],
                serde_json::json!(["ask before deleting"])
            );
            assert!(encoded.get("softDeny").is_none());
        }
        settings_cache::reset_settings_cache();
        let _ = fs::remove_dir_all(root);
    }

    fn env_lock() -> &'static TestEnvLock {
        &TEST_ENV_LOCK
    }

    struct AllowedSourcesRestore(Vec<String>);

    impl AllowedSourcesRestore {
        fn capture() -> Self {
            Self(crate::bootstrap::state::get_allowed_setting_sources())
        }
    }

    impl Drop for AllowedSourcesRestore {
        fn drop(&mut self) {
            crate::bootstrap::state::set_allowed_setting_sources(self.0.clone());
        }
    }

    fn unique_temp_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("cometix-settings-{name}-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn managed_file_settings_presence_matches_official_file_and_drop_in_labels() {
        let _lock = env_lock().lock().expect("env lock");
        let root = unique_temp_dir("managed-presence");
        let drop_ins = root.join("managed-settings.d");
        fs::create_dir_all(&drop_ins).expect("create drop-ins");
        let _managed_guard = EnvVarGuard::set("CLAUDE_CODE_MANAGED_SETTINGS_PATH", &root);
        // Relocating a settings root invalidates every cache keyed by the old
        // source→path mapping. Production never moves these mid-session (the
        // detector's fan-out owns invalidation there), so the test states the
        // invariant itself.
        settings_cache::reset_settings_cache();

        fs::write(root.join("managed-settings.json"), "{}").expect("write empty base");
        fs::write(drop_ins.join("10-empty.json"), "{}").expect("write empty drop-in");
        assert_eq!(
            get_managed_file_settings_presence(),
            ManagedFileSettingsPresence {
                has_base: false,
                has_drop_ins: true
            }
        );
        assert!(load_managed_file_settings().0.is_none());

        fs::write(root.join("managed-settings.json"), r#"{"model":"opus"}"#).expect("write base");
        // Direct disk writes bypass both production invalidation paths
        // (`update_settings_for_source` and the change detector's fan-out),
        // so the test states the invariant itself.
        settings_cache::reset_settings_cache();
        assert_eq!(
            get_managed_file_settings_presence(),
            ManagedFileSettingsPresence {
                has_base: true,
                has_drop_ins: true,
            }
        );

        fs::write(drop_ins.join("20-language.json"), r#"{"language":"fr"}"#)
            .expect("write drop-in");
        settings_cache::reset_settings_cache();
        assert_eq!(
            get_managed_file_settings_presence(),
            ManagedFileSettingsPresence {
                has_base: true,
                has_drop_ins: true,
            }
        );
        let policy = load_managed_file_settings().0.expect("policy settings");
        assert_eq!(policy.model.as_deref(), Some("opus"));
        assert_eq!(policy.language.as_deref(), Some("fr"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn settings_merge_customizer_matches_official_deep_object_and_array_semantics() {
        let mut target: SettingsJson = serde_json::from_value(serde_json::json!({
            "sandbox": {
                "enabled": true,
                "network": {"allowedDomains": ["user.example"]}
            },
            "enabledMcpjsonServers": ["shared", "user-only"],
            "allowedMcpServers": [{"serverName": "shared"}]
        }))
        .expect("target settings");
        let source: SettingsJson = serde_json::from_value(serde_json::json!({
            "sandbox": {
                "excludedCommands": ["npm run test:*"],
                "network": {"allowedDomains": ["user.example", "local.example"]}
            },
            "enabledMcpjsonServers": ["shared", "local-only"],
            "allowedMcpServers": [{"serverName": "shared"}]
        }))
        .expect("source settings");

        merge_settings_deep(&mut target, &source);
        let merged = serde_json::to_value(target).expect("merged settings");

        assert_eq!(merged["sandbox"]["enabled"], true);
        assert_eq!(
            merged["sandbox"]["excludedCommands"],
            serde_json::json!(["npm run test:*"])
        );
        assert_eq!(
            merged["sandbox"]["network"]["allowedDomains"],
            serde_json::json!(["user.example", "local.example"])
        );
        assert_eq!(
            merged["enabledMcpjsonServers"],
            serde_json::json!(["shared", "user-only", "local-only"])
        );
        let allowed_servers = merged["allowedMcpServers"]
            .as_array()
            .expect("allowed MCP server array");
        assert_eq!(allowed_servers.len(), 2);
        assert!(
            allowed_servers
                .iter()
                .all(|server| server["serverName"] == "shared")
        );
    }

    #[test]
    fn setting_sources_display_names_match_official_status_source_labels() {
        let _lock = env_lock().lock().expect("env lock");
        let _sources = AllowedSourcesRestore::capture();
        crate::bootstrap::state::set_allowed_setting_sources(vec![
            "userSettings".to_string(),
            "projectSettings".to_string(),
            "localSettings".to_string(),
        ]);
        let root = unique_temp_dir("source-labels");
        let config_home = root.join("config");
        let managed = root.join("managed");
        let drop_ins = managed.join("managed-settings.d");
        fs::create_dir_all(config_home.as_path()).expect("create config home");
        fs::create_dir_all(drop_ins.as_path()).expect("create drop-ins");
        let _config_guard = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config_home);
        let _managed_guard = EnvVarGuard::set("CLAUDE_CODE_MANAGED_SETTINGS_PATH", &managed);
        settings_cache::reset_settings_cache();

        fs::write(config_home.join("settings.json"), r#"{"model":"sonnet"}"#)
            .expect("write user settings");
        fs::write(
            managed.join("managed-settings.json"),
            r#"{"fastMode":false}"#,
        )
        .expect("write managed settings");
        fs::write(
            drop_ins.join("10-policy.json"),
            r#"{"spinnerTipsEnabled":false}"#,
        )
        .expect("write managed drop-in");

        assert_eq!(
            get_setting_source_display_name_capitalized(SettingSource::User),
            "User settings"
        );
        assert_eq!(
            get_setting_source_display_name_capitalized(SettingSource::Project),
            "Shared project settings"
        );
        assert_eq!(
            get_setting_source_display_name_capitalized(SettingSource::Local),
            "Project local settings"
        );
        assert_eq!(
            get_setting_source_display_name_capitalized(SettingSource::Policy),
            "Enterprise managed settings"
        );

        let names = crate::utils::status::build_setting_sources_properties();
        assert!(names.contains(&"User settings".to_string()));
        assert!(names.contains(&"Enterprise managed settings (file + drop-ins)".to_string()));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn update_settings_for_source_matches_official_preserving_patch() {
        let _lock = env_lock().lock().expect("env lock");
        let root = unique_temp_dir("settings-patch");
        fs::create_dir_all(&root).expect("create config home");
        let _config_guard = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root);
        settings_cache::reset_settings_cache();
        fs::write(
            root.join("settings.json"),
            r#"{"language":"en","autoMemoryEnabled":false}"#,
        )
        .expect("write user settings");

        update_settings_for_source(
            SettingSource::User,
            &serde_json::Map::from_iter([
                ("autoMemoryEnabled".to_string(), serde_json::json!(true)),
                ("autoDreamEnabled".to_string(), serde_json::json!(true)),
            ]),
        )
        .expect("patch settings");

        let written: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(root.join("settings.json")).expect("read settings"),
        )
        .expect("parse settings");
        assert_eq!(written["language"], "en");
        assert_eq!(written["autoMemoryEnabled"], true);
        assert_eq!(written["autoDreamEnabled"], true);

        update_settings_for_source(
            SettingSource::User,
            &serde_json::Map::from_iter([(
                "autoDreamEnabled".to_string(),
                serde_json::Value::Null,
            )]),
        )
        .expect("delete setting with undefined carrier");
        let written: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(root.join("settings.json")).expect("read settings after delete"),
        )
        .expect("parse settings after delete");
        assert!(
            !written
                .as_object()
                .unwrap()
                .contains_key("autoDreamEnabled")
        );
        assert_eq!(written["language"], "en");
        let _ = fs::remove_dir_all(root);
    }

    /// Regression: CC `ConfigTool.ts:330-331` writes
    /// `buildNestedObject(['permissions','defaultMode'], value)`, i.e. a
    /// *partial* `permissions` object, and relies on `mergeWith`'s default deep
    /// merge (settings.ts:492-493) to leave the rest of the block alone. A
    /// shallow top-level insert deletes the user's permission rules instead.
    #[test]
    fn update_settings_for_source_matches_official_deep_merge_preserving_permission_rules() {
        let _lock = env_lock().lock().expect("env lock");
        let root = unique_temp_dir("settings-deep-merge");
        fs::create_dir_all(&root).expect("create config home");
        let _config_guard = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root);
        settings_cache::reset_settings_cache();
        fs::write(
            root.join("settings.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "model": "sonnet",
                "permissions": {
                    "allow": ["Bash(git status:*)", "Read(//tmp/**)"],
                    "deny": ["Bash(rm:*)"],
                    "ask": ["WebFetch"],
                    "additionalDirectories": ["/tmp/extra"],
                    "defaultMode": "default"
                }
            }))
            .expect("serialize seed settings"),
        )
        .expect("write user settings");

        // Exactly the patch ConfigTool builds for `permissions.defaultMode`.
        update_settings_for_source(
            SettingSource::User,
            &serde_json::Map::from_iter([(
                "permissions".to_string(),
                serde_json::json!({"defaultMode": "plan"}),
            )]),
        )
        .expect("patch nested setting");

        let written: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(root.join("settings.json")).expect("read settings"),
        )
        .expect("parse settings");
        assert_eq!(written["permissions"]["defaultMode"], "plan");
        assert_eq!(
            written["permissions"]["allow"],
            serde_json::json!(["Bash(git status:*)", "Read(//tmp/**)"])
        );
        assert_eq!(
            written["permissions"]["deny"],
            serde_json::json!(["Bash(rm:*)"])
        );
        assert_eq!(
            written["permissions"]["ask"],
            serde_json::json!(["WebFetch"])
        );
        assert_eq!(
            written["permissions"]["additionalDirectories"],
            serde_json::json!(["/tmp/extra"])
        );
        assert_eq!(written["model"], "sonnet");
        let _ = fs::remove_dir_all(root);
    }

    /// CC settings.ts:487-491 — the write-side customizer returns `srcValue`
    /// for arrays, so the caller owns the final array. That is the opposite of
    /// `settings_merge_customizer` (:538-547), which concatenates when merging
    /// *sources*; a deep merge must not start concatenating here.
    #[test]
    fn update_settings_for_source_matches_official_array_replacement() {
        let _lock = env_lock().lock().expect("env lock");
        let root = unique_temp_dir("settings-array-replace");
        fs::create_dir_all(&root).expect("create config home");
        let _config_guard = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root);
        settings_cache::reset_settings_cache();
        fs::write(
            root.join("settings.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "enabledMcpjsonServers": ["kept", "dropped"],
                "permissions": {
                    "allow": ["Bash(git status:*)"],
                    "deny": ["Bash(rm:*)"]
                }
            }))
            .expect("serialize seed settings"),
        )
        .expect("write user settings");

        update_settings_for_source(
            SettingSource::User,
            &serde_json::Map::from_iter([
                (
                    "enabledMcpjsonServers".to_string(),
                    serde_json::json!(["replacement"]),
                ),
                (
                    "permissions".to_string(),
                    serde_json::json!({"allow": ["Bash(ls:*)"]}),
                ),
            ]),
        )
        .expect("patch arrays");

        let written: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(root.join("settings.json")).expect("read settings"),
        )
        .expect("parse settings");
        assert_eq!(
            written["enabledMcpjsonServers"],
            serde_json::json!(["replacement"])
        );
        assert_eq!(
            written["permissions"]["allow"],
            serde_json::json!(["Bash(ls:*)"])
        );
        // The sibling the patch never mentioned still survives the array swap.
        assert_eq!(
            written["permissions"]["deny"],
            serde_json::json!(["Bash(rm:*)"])
        );
        let _ = fs::remove_dir_all(root);
    }

    /// CC settings.ts:483-486 — the customizer deletes on `undefined` for every
    /// key lodash walks, so the deletion reaches nested records too (the
    /// `enabledPlugins`/`extraKnownMarketplaces` case called out in the
    /// `updateSettingsForSource` doc comment at :410-414).
    #[test]
    fn update_settings_for_source_matches_official_nested_undefined_deletion() {
        let _lock = env_lock().lock().expect("env lock");
        let root = unique_temp_dir("settings-nested-delete");
        fs::create_dir_all(&root).expect("create config home");
        let _config_guard = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root);
        settings_cache::reset_settings_cache();
        fs::write(
            root.join("settings.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "enabledPlugins": {"kept@market": true, "removed@market": true},
                "permissions": {"defaultMode": "plan", "allow": ["Bash(ls:*)"]}
            }))
            .expect("serialize seed settings"),
        )
        .expect("write user settings");

        update_settings_for_source(
            SettingSource::User,
            &serde_json::Map::from_iter([
                (
                    "enabledPlugins".to_string(),
                    serde_json::json!({"removed@market": null}),
                ),
                (
                    "permissions".to_string(),
                    serde_json::json!({"defaultMode": null}),
                ),
            ]),
        )
        .expect("delete nested keys with undefined carrier");

        let written: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(root.join("settings.json")).expect("read settings"),
        )
        .expect("parse settings");
        let plugins = written["enabledPlugins"]
            .as_object()
            .expect("enabledPlugins object");
        assert!(!plugins.contains_key("removed@market"));
        assert_eq!(plugins["kept@market"], true);
        let permissions = written["permissions"]
            .as_object()
            .expect("permissions object");
        assert!(!permissions.contains_key("defaultMode"));
        assert_eq!(permissions["allow"], serde_json::json!(["Bash(ls:*)"]));
        let _ = fs::remove_dir_all(root);
    }

    /// The collision matrix lodash `mergeWith` produces once CC's customizer
    /// returns `undefined` (settings.ts:492-493): `baseMergeDeep` recurses only
    /// when the patch value is a plain object, and `assignMergeValue`
    /// overwrites in every other pairing.
    #[test]
    fn apply_settings_update_matches_lodash_default_merge_collisions() {
        // Scalar patch over an existing object → plain assignment.
        let mut target = serde_json::json!({"permissions": {"defaultMode": "plan"}});
        apply_settings_update(
            target.as_object_mut().expect("object"),
            serde_json::json!({"permissions": "plan"})
                .as_object()
                .expect("patch object"),
        );
        assert_eq!(target, serde_json::json!({"permissions": "plan"}));

        // Object patch over an existing scalar → lodash rebases on a fresh
        // `{}` (`!isObject(objValue)`), so the scalar is replaced outright.
        let mut target = serde_json::json!({"permissions": "plan"});
        apply_settings_update(
            target.as_object_mut().expect("object"),
            serde_json::json!({"permissions": {"defaultMode": "plan"}})
                .as_object()
                .expect("patch object"),
        );
        assert_eq!(
            target,
            serde_json::json!({"permissions": {"defaultMode": "plan"}})
        );

        // Object patch over a missing key → same fresh `{}` base, and a nested
        // `undefined` inside it still lands as "absent".
        let mut target = serde_json::json!({});
        apply_settings_update(
            target.as_object_mut().expect("object"),
            serde_json::json!({"permissions": {"defaultMode": "plan", "allow": null}})
                .as_object()
                .expect("patch object"),
        );
        assert_eq!(
            target,
            serde_json::json!({"permissions": {"defaultMode": "plan"}})
        );

        // Three levels down, siblings at every level survive.
        let mut target = serde_json::json!({
            "sandbox": {
                "enabled": true,
                "network": {"allowUnixSockets": ["/tmp/sock"], "allowLocalBinding": true}
            }
        });
        apply_settings_update(
            target.as_object_mut().expect("object"),
            serde_json::json!({"sandbox": {"network": {"allowLocalBinding": false}}})
                .as_object()
                .expect("patch object"),
        );
        assert_eq!(
            target,
            serde_json::json!({
                "sandbox": {
                    "enabled": true,
                    "network": {"allowUnixSockets": ["/tmp/sock"], "allowLocalBinding": false}
                }
            })
        );

        // JavaScript `delete` keeps the surviving keys in their original order.
        let mut target = serde_json::json!({"a": 1, "b": 2, "c": 3});
        apply_settings_update(
            target.as_object_mut().expect("object"),
            serde_json::json!({"b": null})
                .as_object()
                .expect("patch object"),
        );
        let keys: Vec<&str> = target
            .as_object()
            .expect("object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, vec!["a", "c"]);
    }
}
