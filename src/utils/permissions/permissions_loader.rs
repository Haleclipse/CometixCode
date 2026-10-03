//! Maps to: CC `utils/permissions/permissionsLoader.ts`.
//!
//! Owns source-aware rule loading and editing. The settings owner performs disk
//! writes and cache invalidation; this owner preserves rule normalization and
//! the managed-only AddRules gate.

use super::permission_rule_parser::{
    permission_rule_value_from_string, permission_rule_value_to_string,
};
use crate::types::permissions::{
    PermissionBehavior, PermissionRule, PermissionRuleSource, PermissionRuleValue,
};
use crate::utils::fs_operations::{get_fs_implementation, safe_resolve_path};
use crate::utils::settings::{SettingSource, SettingsJson, get_settings_for_source};

pub const SUPPORTED_RULE_BEHAVIORS: &[PermissionBehavior] = &[
    PermissionBehavior::Allow,
    PermissionBehavior::Deny,
    PermissionBehavior::Ask,
];

/// Maps to: CC `shouldAllowManagedPermissionRulesOnly()`.
pub fn should_allow_managed_permission_rules_only() -> bool {
    get_settings_for_source(SettingSource::Policy)
        .and_then(|settings| settings.allow_managed_permission_rules_only)
        == Some(true)
}

/// Maps to: CC `shouldShowAlwaysAllowOptions()`.
pub fn should_show_always_allow_options() -> bool {
    !should_allow_managed_permission_rules_only()
}

/// Maps to: CC `settingsJsonToRules(...)`.
pub fn settings_json_to_rules(
    data: Option<&SettingsJson>,
    source: PermissionRuleSource,
) -> Vec<PermissionRule> {
    let Some(permissions) = data.and_then(|settings| settings.permissions.as_ref()) else {
        return Vec::new();
    };
    let mut rules = Vec::new();
    for behavior in SUPPORTED_RULE_BEHAVIORS {
        let behavior_rules = match behavior {
            PermissionBehavior::Allow => permissions.allow.as_ref(),
            PermissionBehavior::Deny => permissions.deny.as_ref(),
            PermissionBehavior::Ask => permissions.ask.as_ref(),
        };
        if let Some(behavior_rules) = behavior_rules {
            rules.extend(behavior_rules.iter().map(|rule_string| PermissionRule {
                source,
                rule_behavior: *behavior,
                rule_value: permission_rule_value_from_string(rule_string),
            }));
        }
    }
    rules
}

/// Maps to: CC `utils/permissions/permissionsLoader.ts:140-145#getPermissionRulesForSource`.
pub fn get_permission_rules_for_source(source: SettingSource) -> Vec<PermissionRule> {
    let settings = get_settings_for_source(source);
    let rule_source = match source {
        SettingSource::User => PermissionRuleSource::UserSettings,
        SettingSource::Project => PermissionRuleSource::ProjectSettings,
        SettingSource::Local => PermissionRuleSource::LocalSettings,
        SettingSource::Flag => PermissionRuleSource::FlagSettings,
        SettingSource::Policy => PermissionRuleSource::PolicySettings,
    };
    settings_json_to_rules(settings.as_ref(), rule_source)
}

/// Maps to: CC `loadAllPermissionRulesFromDisk()`.
pub fn load_all_permission_rules_from_disk() -> Vec<PermissionRule> {
    if should_allow_managed_permission_rules_only() {
        return get_permission_rules_for_source(SettingSource::Policy);
    }
    crate::utils::settings::get_enabled_setting_sources()
        .into_iter()
        .flat_map(get_permission_rules_for_source)
        .collect()
}

/// Maps to: CC `utils/permissions/permissionsLoader.ts:147-149`.
/// Rust checks the editable-source refinement at the disk-editing entry.
pub type PermissionRuleFromEditableSettings = PermissionRule;

/// Maps to: CC `utils/permissions/permissionsLoader.ts:60-83`
/// `getSettingsForSourceLenient_FOR_EDITING_ONLY_NOT_FOR_READING`.
/// Raw JSON is necessary here: malformed unrelated hooks/settings must survive
/// editing. This value must never be used for execution settings.
fn get_settings_for_source_lenient_for_editing_only_not_for_reading(
    source: SettingSource,
) -> Option<serde_json::Value> {
    let path = crate::utils::settings::get_settings_file_path_for_source(source)?;
    let resolved = safe_resolve_path(get_fs_implementation().as_ref(), &path);
    let content = crate::utils::file_read::read_file_sync(&resolved.resolved_path).ok()?;
    if content.trim().is_empty() {
        return Some(serde_json::json!({}));
    }
    let value: serde_json::Value = serde_json::from_str(&content).ok()?;
    (value.is_object() || value.is_array()).then_some(value)
}

/// Maps to: CC `utils/permissions/permissionsLoader.ts:163-215`
/// `deletePermissionRuleFromSettings`.
/// Ordinary editing failures are logged and return false, as in the source.
pub fn delete_permission_rule_from_settings(rule: &PermissionRuleFromEditableSettings) -> bool {
    let source = match rule.source {
        PermissionRuleSource::UserSettings => SettingSource::User,
        PermissionRuleSource::ProjectSettings => SettingSource::Project,
        PermissionRuleSource::LocalSettings => SettingSource::Local,
        _ => return false,
    };
    let Some(settings) = get_settings_for_source(source) else {
        return false;
    };
    let Some(permissions) = settings.permissions.as_ref() else {
        return false;
    };
    let (key, existing) = match rule.rule_behavior {
        PermissionBehavior::Allow => ("allow", permissions.allow.as_ref()),
        PermissionBehavior::Deny => ("deny", permissions.deny.as_ref()),
        PermissionBehavior::Ask => ("ask", permissions.ask.as_ref()),
    };
    let Some(existing) = existing else {
        return false;
    };
    let target = permission_rule_value_to_string(&rule.rule_value);
    let normalize =
        |raw: &str| permission_rule_value_to_string(&permission_rule_value_from_string(raw));
    if !existing.iter().any(|raw| normalize(raw) == target) {
        return false;
    }
    let remaining = existing
        .iter()
        .filter(|raw| normalize(raw) != target)
        .cloned()
        .collect::<Vec<_>>();
    let result = (|| -> anyhow::Result<()> {
        // CC :193-205 spreads the FULL cached snapshot, not only this array.
        // A later disk rewrite must not replace the snapshot the loader chose.
        let mut patch = serde_json::to_value(&settings)?;
        crate::utils::settings::strip_absent_settings_fields(&mut patch);
        let permissions = patch
            .get_mut("permissions")
            .and_then(serde_json::Value::as_object_mut)
            .ok_or_else(|| anyhow::anyhow!("settings permissions must be an object"))?;
        permissions.insert(key.to_string(), serde_json::json!(remaining));
        let patch = patch
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("settings must be an object"))?;
        // Explicit Cometix no-write seam, not a CC permission policy.
        if !crate::utils::session_storage::is_session_write_enabled() {
            anyhow::bail!(crate::tools::shared::write_gate::PERMISSION_PERSISTENCE_DISABLED_ERROR);
        }
        crate::utils::settings::update_settings_for_source(source, patch)
    })();
    match result {
        Ok(()) => true,
        Err(error) => {
            // CC :211-214 logError(error), return false.
            crate::utils::debug::log_for_debugging(&error.to_string());
            false
        }
    }
}

/// Maps to: CC `utils/permissions/permissionsLoader.ts:217-221`.
fn get_empty_permission_settings_json() -> serde_json::Value {
    serde_json::json!({"permissions": {}})
}

/// Maps to: CC `utils/permissions/permissionsLoader.ts:229-296`
/// `addPermissionRulesToSettings`.
/// `true` includes an empty batch or an already-present batch. Managed-only
/// and ordinary read/write failures return false; PermissionUpdate ignores
/// that boolean in CC, so neither becomes a new permission-denial policy.
pub fn add_permission_rules_to_settings(
    rule_values: &[PermissionRuleValue],
    rule_behavior: PermissionBehavior,
    source: SettingSource,
) -> bool {
    if should_allow_managed_permission_rules_only() {
        return false;
    }
    if rule_values.is_empty() {
        return true;
    }
    let rule_strings = rule_values
        .iter()
        .map(permission_rule_value_to_string)
        .collect::<Vec<_>>();
    // CC :250-253 selects a FULL validated snapshot or lenient raw object.
    // Strip only typed Option transport nulls; never sanitize the raw fallback.
    // SettingsJson's loss of unknown fields remains a parent-loader/type seam.
    let settings = get_settings_for_source(source)
        .map(|settings| {
            let mut snapshot = serde_json::json!(settings);
            crate::utils::settings::strip_absent_settings_fields(&mut snapshot);
            snapshot
        })
        .or_else(|| get_settings_for_source_lenient_for_editing_only_not_for_reading(source))
        .unwrap_or_else(get_empty_permission_settings_json);
    let result = (|| -> anyhow::Result<()> {
        let key = match rule_behavior {
            PermissionBehavior::Allow => "allow",
            PermissionBehavior::Deny => "deny",
            PermissionBehavior::Ask => "ask",
        };
        let existing = match settings.get("permissions").and_then(|value| value.get(key)) {
            // CC :260-261: `existingPermissions[ruleBehavior] || []` admits
            // every JSON falsy value, including malformed false/0/"" from the
            // lenient branch. Truthy non-arrays still enter the caught error.
            None | Some(serde_json::Value::Null | serde_json::Value::Bool(false)) => Vec::new(),
            Some(serde_json::Value::Number(number)) if number.as_f64() == Some(0.0) => Vec::new(),
            Some(serde_json::Value::String(value)) if value.is_empty() => Vec::new(),
            Some(value) => value
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("permissions.{key} must be an array"))?
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .map(str::to_string)
                        .ok_or_else(|| anyhow::anyhow!("permissions.{key} entries must be strings"))
                })
                .collect::<anyhow::Result<Vec<_>>>()?,
        };
        let existing_set = existing
            .iter()
            .map(|raw| permission_rule_value_to_string(&permission_rule_value_from_string(raw)))
            .collect::<std::collections::HashSet<_>>();
        // CC :270 filters against the ORIGINAL set. Duplicates within the new
        // batch intentionally survive; do not mutate existing_set in this loop.
        let new_rules = rule_strings
            .into_iter()
            .filter(|rule| !existing_set.contains(rule))
            .collect::<Vec<_>>();
        if new_rules.is_empty() {
            return Ok(());
        }
        let updated = existing.into_iter().chain(new_rules).collect::<Vec<_>>();
        // CC :278-285 preserves both object spreads. The writer's fresh read
        // does not substitute for this cached (or lenient) source snapshot.
        let mut patch = match settings.clone() {
            serde_json::Value::Object(object) => object,
            serde_json::Value::Array(array) => array
                .into_iter()
                .enumerate()
                .map(|(index, value)| (index.to_string(), value))
                .collect(),
            _ => serde_json::Map::new(),
        };
        let mut permissions = match patch.get("permissions") {
            Some(serde_json::Value::Object(object)) => object.clone(),
            Some(serde_json::Value::Array(array)) => array
                .iter()
                .cloned()
                .enumerate()
                .map(|(index, value)| (index.to_string(), value))
                .collect(),
            // CC :281 spreads string own-index properties. ASCII bytes are
            // exactly UTF-16 units here; do not approximate astral strings
            // with Rust scalar indexing. Non-ASCII malformed permissions
            // remain an explicit JSON/UTF-16 representation gap in this slice.
            Some(serde_json::Value::String(value)) if value.is_ascii() => value
                .bytes()
                .enumerate()
                .map(|(index, byte)| {
                    (
                        index.to_string(),
                        serde_json::Value::String(char::from(byte).to_string()),
                    )
                })
                .collect(),
            _ => serde_json::Map::new(),
        };
        permissions.insert(key.to_string(), serde_json::json!(updated));
        patch.insert(
            "permissions".to_string(),
            serde_json::Value::Object(permissions),
        );
        // Explicit Cometix no-write seam. It applies at the actual write, not
        // to managed-only / duplicate no-ops above.
        if !crate::utils::session_storage::is_session_write_enabled() {
            anyhow::bail!(crate::tools::shared::write_gate::PERMISSION_PERSISTENCE_DISABLED_ERROR);
        }
        crate::utils::settings::update_settings_for_source(source, &patch)
    })();
    match result {
        Ok(()) => true,
        Err(error) => {
            // CC :292-295 logError(error), return false.
            crate::utils::debug::log_for_debugging(&error.to_string());
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::settings::types::PermissionsSettings;

    fn settings(allow: &[&str], deny: &[&str], ask: &[&str]) -> SettingsJson {
        SettingsJson {
            permissions: Some(PermissionsSettings {
                allow: (!allow.is_empty()).then(|| allow.iter().map(|s| s.to_string()).collect()),
                deny: (!deny.is_empty()).then(|| deny.iter().map(|s| s.to_string()).collect()),
                ask: (!ask.is_empty()).then(|| ask.iter().map(|s| s.to_string()).collect()),
                ..PermissionsSettings::default()
            }),
            ..SettingsJson::default()
        }
    }

    #[test]
    fn settings_json_to_rules_preserves_official_behavior_order_and_source() {
        let rules = settings_json_to_rules(
            Some(&settings(&["Bash(ls:*)"], &["Write"], &["Read"])),
            PermissionRuleSource::ProjectSettings,
        );
        assert_eq!(
            rules
                .iter()
                .map(|rule| {
                    format!(
                        "{}:{}:{}",
                        super::super::permission_rule::permission_rule_source_to_official_str(
                            rule.source
                        ),
                        super::super::permission_rule::permission_behavior_to_official_str(
                            rule.rule_behavior
                        ),
                        permission_rule_value_to_string(&rule.rule_value)
                    )
                })
                .collect::<Vec<_>>(),
            vec![
                "projectSettings:allow:Bash(ls:*)",
                "projectSettings:deny:Write",
                "projectSettings:ask:Read",
            ]
        );
    }
}

#[cfg(test)]
mod persistence_tests {
    use super::*;
    use crate::utils::settings::settings_cache::reset_settings_cache;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};
    use serde_json::json;
    use std::path::PathBuf;

    struct Fixture {
        root: PathBuf,
        _config: EnvVarGuard,
        _managed: EnvVarGuard,
        _writes: EnvVarGuard,
    }

    impl Fixture {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("cc-permission-loader-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&root).unwrap();
            let root = root.canonicalize().unwrap();
            let result = Self {
                _config: EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root),
                _managed: EnvVarGuard::set("CLAUDE_CODE_MANAGED_SETTINGS_PATH", &root),
                _writes: EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1"),
                root,
            };
            reset_settings_cache();
            result
        }

        fn write(&self, value: serde_json::Value) {
            std::fs::write(
                self.root.join("settings.json"),
                serde_json::to_vec(&value).unwrap(),
            )
            .unwrap();
            reset_settings_cache();
        }

        fn read(&self) -> serde_json::Value {
            serde_json::from_str(&std::fs::read_to_string(self.root.join("settings.json")).unwrap())
                .unwrap()
        }

        /// CC settings.ts:219-225: exercise the real nullable producer and
        /// its cache, without installing a fabricated cached failure.
        fn assert_official_validation_failure_boundary(&self) {
            assert!(
                crate::utils::zod::safe_parse(
                    crate::utils::settings::types::settings_schema(),
                    &self.read(),
                )
                .is_err(),
                "the fixture must really fail the source-shaped schema"
            );
            assert!(get_settings_for_source(SettingSource::User).is_none());
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            reset_settings_cache();
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// CC permissionsLoader.ts:263-281 compares against normalized EXISTING rules,
    /// keeps duplicate NEW rules, and returns true without writing for no-ops.
    #[test]
    fn add_rules_matches_official_existing_legacy_normalization_and_batch_duplicates() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let fixture = Fixture::new();
        fixture.write(json!({"permissions":{"allow":["KillShell","Task","Bash(*)"]}}));
        let before = std::fs::read(fixture.root.join("settings.json")).unwrap();
        assert!(add_permission_rules_to_settings(
            &[
                PermissionRuleValue::new("TaskStop", None),
                PermissionRuleValue::new("Agent", None),
                PermissionRuleValue::new("Bash", None)
            ],
            PermissionBehavior::Allow,
            SettingSource::User,
        ));
        assert_eq!(
            std::fs::read(fixture.root.join("settings.json")).unwrap(),
            before
        );
        assert!(add_permission_rules_to_settings(
            &[
                PermissionRuleValue::new("Read", None),
                PermissionRuleValue::new("Read", None)
            ],
            PermissionBehavior::Allow,
            SettingSource::User,
        ));
        assert_eq!(
            fixture.read()["permissions"]["allow"],
            json!(["KillShell", "Task", "Bash(*)", "Read", "Read"])
        );
        // settings.ts:505-506 invalidates a cache already warmed by the first add.
        assert_eq!(
            get_settings_for_source(SettingSource::User)
                .unwrap()
                .permissions
                .unwrap()
                .allow
                .unwrap()
                .len(),
            5
        );
    }

    /// CC permissionsLoader.ts:239-246: managed gate precedes empty batch success;
    /// neither skipped branch creates or modifies the target file.
    #[test]
    fn add_rules_matches_official_managed_only_and_empty_no_write() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let fixture = Fixture::new();
        assert!(add_permission_rules_to_settings(
            &[],
            PermissionBehavior::Allow,
            SettingSource::User
        ));
        assert!(!fixture.root.join("settings.json").exists());
        std::fs::write(
            fixture.root.join("managed-settings.json"),
            r#"{"allowManagedPermissionRulesOnly":true}"#,
        )
        .unwrap();
        reset_settings_cache();
        assert!(!add_permission_rules_to_settings(
            &[],
            PermissionBehavior::Allow,
            SettingSource::User
        ));
        for source in [
            SettingSource::User,
            SettingSource::Project,
            SettingSource::Local,
        ] {
            assert!(!add_permission_rules_to_settings(
                &[PermissionRuleValue::new("Read", None)],
                PermissionBehavior::Allow,
                source
            ));
        }
        assert!(!fixture.root.join("settings.json").exists());
    }

    /// CC permissionsLoader.ts:250-253 uses the lenient file when unrelated hooks
    /// fail validation, while the settings writer preserves unrecognized keys.
    #[test]
    fn add_rules_matches_official_lenient_boundary_edit_and_ordinary_failure_boolean() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let fixture = Fixture::new();
        fixture.write(json!({"hooks":"invalid","custom":{"keep":7},"permissions":{"allow":["Read"],"custom":"keep"}}));
        fixture.assert_official_validation_failure_boundary();
        assert!(get_settings_for_source(SettingSource::User).is_none());
        assert!(add_permission_rules_to_settings(
            &[PermissionRuleValue::new("Grep", None)],
            PermissionBehavior::Allow,
            SettingSource::User
        ));
        assert_eq!(
            fixture.read(),
            json!({"hooks":"invalid","custom":{"keep":7},"permissions":{"allow":["Read","Grep"],"custom":"keep"}})
        );
        fixture.write(json!({"permissions":{"deny":"wrong-type"}}));
        fixture.assert_official_validation_failure_boundary();
        assert!(!add_permission_rules_to_settings(
            &[PermissionRuleValue::new("Bash", None)],
            PermissionBehavior::Deny,
            SettingSource::User
        ));
        assert_eq!(fixture.read(), json!({"permissions":{"deny":"wrong-type"}}));
    }

    /// CC permissionsLoader.ts:163-215 deletes canonicalized existing entries,
    /// refuses immutable sources, and returns false when nothing matches.
    #[test]
    fn delete_rule_matches_official_disk_edit_and_cache_invalidation() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let fixture = Fixture::new();
        fixture.write(json!({"permissions":{"allow":["KillShell","TaskStop","Read"]},"custom":42}));
        let mut rule = PermissionRule {
            source: PermissionRuleSource::UserSettings,
            rule_behavior: PermissionBehavior::Allow,
            rule_value: PermissionRuleValue::new("TaskStop", None),
        };
        assert!(delete_permission_rule_from_settings(&rule));
        assert_eq!(
            fixture.read(),
            json!({"permissions":{"allow":["Read"]},"custom":42})
        );
        assert!(!delete_permission_rule_from_settings(&rule));
        assert_eq!(
            get_settings_for_source(SettingSource::User)
                .unwrap()
                .permissions
                .unwrap()
                .allow
                .unwrap(),
            vec!["Read"]
        );
        rule.source = PermissionRuleSource::PolicySettings;
        assert!(!delete_permission_rule_from_settings(&rule));
    }

    /// CC safeResolvePath/readFileSync at permissionsLoader.ts:68-70 follows the
    /// configured settings symlink; no PermissionUpdate-specific rejection exists.
    #[cfg(unix)]
    #[test]
    fn add_rules_matches_official_settings_symlink_and_parent() {
        use std::os::unix::fs::symlink;
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let fixture = Fixture::new();
        let actual = fixture.root.join("actual.json");
        std::fs::write(&actual, r#"{"permissions":{"allow":["Read"]}}"#).unwrap();
        symlink(&actual, fixture.root.join("settings.json")).unwrap();
        assert!(add_permission_rules_to_settings(
            &[PermissionRuleValue::new("Grep", None)],
            PermissionBehavior::Allow,
            SettingSource::User
        ));
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&actual).unwrap()).unwrap();
        assert_eq!(value["permissions"]["allow"], json!(["Read", "Grep"]));
        let linked = fixture.root.join("linked-config");
        symlink(&fixture.root, &linked).unwrap();
        let _linked = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &linked);
        reset_settings_cache();
        assert!(add_permission_rules_to_settings(
            &[PermissionRuleValue::new("Glob", None)],
            PermissionBehavior::Allow,
            SettingSource::User
        ));
        assert_eq!(
            fixture.read()["permissions"]["allow"],
            json!(["Read", "Grep", "Glob"])
        );
    }

    /// CC permissionsLoader.ts:193-205 / :278-285 passes the cached snapshot
    /// to updateSettingsForSource. Its settings.ts:440 call bypasses only the
    /// per-source cache: getSettingsForSourceUncached (:349-352) still invokes
    /// parseSettingsFile, whose file-cache hit (:182-190) returns the old JSON.
    /// A raw external edit without resetSettingsCache therefore gets overwritten,
    /// including keys absent from both cached snapshots; there is no newer read.
    #[test]
    fn add_and_delete_rules_match_official_full_cached_snapshot_overlay() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let fixture = Fixture::new();
        for deleting in [false, true] {
            fixture.write(json!({"model":"old","permissions":{"allow":["Read"],"deny":["Bash"]}}));
            assert_eq!(
                get_settings_for_source(SettingSource::User)
                    .unwrap()
                    .model
                    .as_deref(),
                Some("old")
            );
            // Deliberately bypass cache invalidation to model an external edit
            // before the existing cached snapshot's consumer runs.
            std::fs::write(fixture.root.join("settings.json"), serde_json::to_vec(&json!({"model":"new","language":"zh","permissions":{"allow":["Read"],"deny":["Grep"]}})).unwrap()).unwrap();
            if deleting {
                assert!(delete_permission_rule_from_settings(&PermissionRule {
                    source: PermissionRuleSource::UserSettings,
                    rule_behavior: PermissionBehavior::Allow,
                    rule_value: PermissionRuleValue::new("Read", None),
                }));
            } else {
                assert!(add_permission_rules_to_settings(
                    &[PermissionRuleValue::new("Glob", None)],
                    PermissionBehavior::Allow,
                    SettingSource::User
                ));
            }
            let value = fixture.read();
            assert_eq!(value["model"], "old");
            assert!(
                !value.as_object().unwrap().contains_key("language"),
                "source bypasses per-source cache but retains the earlier parsed-file snapshot"
            );
            assert_eq!(value["permissions"]["deny"], json!(["Bash"]));
            assert_eq!(
                value["permissions"]["allow"],
                if deleting {
                    json!([])
                } else {
                    json!(["Read", "Glob"])
                }
            );
        }
    }

    /// CC permissionsLoader.ts:260-261 uses JS `|| []` after lenient loading.
    /// False/zero/empty-string/null become empty; truthy malformed arrays fail.
    #[test]
    fn add_rules_matches_official_lenient_boundary_falsy_and_truthy_array_values() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let fixture = Fixture::new();
        let rules = [PermissionRuleValue::new("Read", None)];
        for value in [json!(false), json!(0), json!(-0.0), json!(""), json!(null)] {
            fixture.write(json!({"hooks":"invalid","permissions":{"allow":value}}));
            fixture.assert_official_validation_failure_boundary();
            assert!(
                add_permission_rules_to_settings(
                    &rules,
                    PermissionBehavior::Allow,
                    SettingSource::User
                ),
                "falsy source value {value}"
            );
            assert_eq!(fixture.read()["permissions"]["allow"], json!(["Read"]));
        }
        for value in [
            json!(true),
            json!(1),
            json!("wrong-type"),
            json!({}),
            json!(["Read", 5]),
        ] {
            let original = json!({"hooks":"invalid","permissions":{"allow":value}});
            fixture.write(original.clone());
            fixture.assert_official_validation_failure_boundary();
            assert!(
                !add_permission_rules_to_settings(
                    &rules,
                    PermissionBehavior::Allow,
                    SettingSource::User
                ),
                "truthy malformed source value {value}"
            );
            assert_eq!(fixture.read(), original);
        }
    }

    /// CC permissionsLoader.ts:278-285 spreads a malformed ASCII permissions
    /// string's own-index properties after the source validation-failure boundary.
    /// This does not cover non-ASCII UTF-16 string-property representation.
    #[test]
    fn add_rules_matches_official_lenient_boundary_ascii_string_spread() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let fixture = Fixture::new();
        fixture.write(json!({"permissions":"abc"}));
        fixture.assert_official_validation_failure_boundary();
        assert!(add_permission_rules_to_settings(
            &[PermissionRuleValue::new("Read", None)],
            PermissionBehavior::Allow,
            SettingSource::User
        ));
        assert_eq!(
            fixture.read(),
            json!({"permissions":{"0":"a","1":"b","2":"c","allow":["Read"]}})
        );
    }

    /// CC settings.ts:219-224 -> permissionsLoader.ts:250-285: a schema
    /// failure in an unrelated field must preserve existing raw rules on edit.
    #[test]
    fn add_rules_matches_official_real_validation_failure_preserves_existing_rules() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let fixture = Fixture::new();
        fixture.write(json!({"model":42,"permissions":{"allow":["Read(src/**)"]},"custom":"keep"}));
        fixture.assert_official_validation_failure_boundary();
        assert!(add_permission_rules_to_settings(
            &[PermissionRuleValue::new("Bash", Some("ls".into()))],
            PermissionBehavior::Allow,
            SettingSource::User,
        ));
        assert_eq!(
            fixture.read(),
            json!({"model":42,
            "permissions":{"allow":["Read(src/**)","Bash(ls)"]},"custom":"keep"})
        );
        assert!(get_settings_for_source(SettingSource::User).is_none());
        assert!(add_permission_rules_to_settings(
            &[PermissionRuleValue::new("Grep", None)],
            PermissionBehavior::Allow,
            SettingSource::User,
        ));
        assert_eq!(
            fixture.read()["permissions"]["allow"],
            json!(["Read(src/**)", "Bash(ls)", "Grep"])
        );
    }
}
