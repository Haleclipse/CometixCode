//! Maps to: CC `utils/permissions/PermissionUpdate.ts`.
//! Owns in-memory projection and source-shaped persistence dispatch. The
//! canonical settings writer owns disk I/O and cache invalidation; the explicit
//! Cometix no-write seam remains at the persistence boundary.

use crate::tool::ToolPermissionContext;
use crate::types::permissions::{
    AdditionalWorkingDirectory, PermissionBehavior, PermissionRuleSource, PermissionRuleValue,
    PermissionUpdate, PermissionUpdateDestination,
};

/// Maps to: CC `utils/permissions/PermissionUpdate.ts:30-40#extractRules`.
pub fn extract_rules(updates: &[PermissionUpdate]) -> Vec<PermissionRuleValue> {
    updates
        .iter()
        .flat_map(|update| match update {
            PermissionUpdate::AddRules { rules, .. } => rules.clone(),
            PermissionUpdate::SetMode { .. }
            | PermissionUpdate::ReplaceRules { .. }
            | PermissionUpdate::RemoveRules { .. }
            | PermissionUpdate::AddDirectories { .. }
            | PermissionUpdate::RemoveDirectories { .. } => Vec::new(),
        })
        .collect()
}

/// Maps to: CC `utils/permissions/PermissionUpdate.ts:361-378#createReadRuleSuggestion`.
pub fn create_read_rule_suggestion(
    dir_path: &str,
    destination: PermissionUpdateDestination,
) -> Option<PermissionUpdate> {
    let path_for_pattern = if cfg!(windows) {
        crate::utils::windows_paths::windows_path_to_posix_path(dir_path)
    } else {
        dir_path.to_string()
    };
    if path_for_pattern == "/" {
        return None;
    }
    let rule_content = if std::path::Path::new(&path_for_pattern).is_absolute() {
        format!("/{path_for_pattern}/**")
    } else {
        format!("{path_for_pattern}/**")
    };
    Some(PermissionUpdate::AddRules {
        destination,
        behavior: PermissionBehavior::Allow,
        rules: vec![PermissionRuleValue::new("Read", Some(rule_content))],
    })
}

/// Maps to: CC `utils/permissions/PermissionUpdate.ts:42-172#applyPermissionUpdate`.
/// Rule additions append without in-memory deduplication; persistence keeps its
/// separate settings-file duplicate filtering.
pub fn apply_permission_update(
    context: &ToolPermissionContext,
    update: &PermissionUpdate,
) -> ToolPermissionContext {
    let mut next = context.clone();
    match update {
        PermissionUpdate::SetMode { mode, .. } => {
            next.mode = *mode;
        }
        PermissionUpdate::AddRules {
            destination,
            behavior,
            rules,
        } => {
            let source = permission_rule_source_for_destination(*destination);
            rules_for_behavior_mut(&mut next, *behavior)
                .entry(source)
                .or_default()
                .extend(rules.iter().cloned());
        }
        PermissionUpdate::ReplaceRules {
            destination,
            behavior,
            rules,
        } => {
            rules_for_behavior_mut(&mut next, *behavior).insert(
                permission_rule_source_for_destination(*destination),
                rules.clone(),
            );
        }
        PermissionUpdate::RemoveRules {
            destination,
            behavior,
            rules,
        } => {
            let source = permission_rule_source_for_destination(*destination);
            let existing = rules_for_behavior_mut(&mut next, *behavior)
                .entry(source)
                .or_default();
            existing.retain(|rule| !rules.contains(rule));
        }
        PermissionUpdate::AddDirectories {
            destination,
            directories,
        } => {
            for directory in directories {
                next.additional_working_directories.insert(
                    directory.clone(),
                    AdditionalWorkingDirectory {
                        path: directory.clone(),
                        source: permission_rule_source_for_destination(*destination),
                    },
                );
            }
        }
        PermissionUpdate::RemoveDirectories { directories, .. } => {
            for directory in directories {
                // CC PermissionUpdate.ts:177 Map.delete preserves the order
                // of surviving entries; IndexMap::remove would swap them.
                next.additional_working_directories.shift_remove(directory);
            }
        }
    }
    next
}

/// Maps to: CC `utils/permissions/PermissionUpdate.ts:196-203#applyPermissionUpdates`.
pub fn apply_permission_updates(
    context: &ToolPermissionContext,
    updates: &[PermissionUpdate],
) -> ToolPermissionContext {
    updates.iter().fold(context.clone(), |ctx, update| {
        apply_permission_update(&ctx, update)
    })
}

/// Maps to: CC `utils/permissions/PermissionUpdate.ts:208-216#supportsPersistence`.
pub fn supports_persistence(destination: PermissionUpdateDestination) -> bool {
    matches!(
        destination,
        PermissionUpdateDestination::LocalSettings
            | PermissionUpdateDestination::UserSettings
            | PermissionUpdateDestination::ProjectSettings
    )
}

pub(crate) fn setting_source_for_destination(
    destination: PermissionUpdateDestination,
) -> Option<crate::utils::settings::constants::SettingSource> {
    use crate::utils::settings::constants::SettingSource;
    match destination {
        PermissionUpdateDestination::LocalSettings => Some(SettingSource::Local),
        PermissionUpdateDestination::UserSettings => Some(SettingSource::User),
        PermissionUpdateDestination::ProjectSettings => Some(SettingSource::Project),
        _ => None,
    }
}

fn behavior_key(behavior: PermissionBehavior) -> &'static str {
    match behavior {
        PermissionBehavior::Allow => "allow",
        PermissionBehavior::Deny => "deny",
        PermissionBehavior::Ask => "ask",
    }
}

/// Maps to: CC `utils/permissions/PermissionUpdate.ts:222-347#persistPermissionUpdate`.
/// `false` means a session/CLI destination. Editable destinations return true
/// even for source-defined no-ops (including the AddRules loader returning
/// false): CC ignores that loader's boolean and proceeds to the next update.
///
/// Explicit retained deviation: COMETIX_WRITE_ENABLED=0 returns an error at
/// this entry. Ordinary settings failures are caught/logged and do not become
/// exceptions: CC's writer returns `{ error }`, which this owner ignores for
/// all five non-AddRules branches. AddRules catches in the source-owned loader.
pub fn persist_permission_update(update: &PermissionUpdate) -> anyhow::Result<bool> {
    let destination = match update {
        PermissionUpdate::SetMode { destination, .. }
        | PermissionUpdate::AddRules { destination, .. }
        | PermissionUpdate::ReplaceRules { destination, .. }
        | PermissionUpdate::RemoveRules { destination, .. }
        | PermissionUpdate::AddDirectories { destination, .. }
        | PermissionUpdate::RemoveDirectories { destination, .. } => *destination,
    };
    let Some(source) = setting_source_for_destination(destination) else {
        return Ok(false);
    };
    if !crate::utils::session_storage::is_session_write_enabled() {
        anyhow::bail!(crate::tools::shared::write_gate::PERMISSION_PERSISTENCE_DISABLED_ERROR);
    }
    use super::permission_rule_parser::{
        permission_rule_value_from_string, permission_rule_value_to_string,
    };
    let permissions = match update {
        PermissionUpdate::AddRules {
            rules, behavior, ..
        } => {
            // CC :235-242: the bool result is deliberately not inspected.
            super::permissions_loader::add_permission_rules_to_settings(rules, *behavior, source);
            return Ok(true);
        }
        PermissionUpdate::AddDirectories { directories, .. } => {
            let existing = crate::utils::settings::get_settings_for_source(source)
                .and_then(|settings| settings.permissions)
                .and_then(|permissions| permissions.additional_directories)
                .unwrap_or_default();
            let dirs_to_add = directories
                .iter()
                .filter(|directory| !existing.contains(directory))
                .cloned()
                .collect::<Vec<_>>();
            if dirs_to_add.is_empty() {
                return Ok(true);
            }
            serde_json::json!({"additionalDirectories": existing.into_iter().chain(dirs_to_add).collect::<Vec<_>>()})
        }
        PermissionUpdate::RemoveRules {
            rules, behavior, ..
        } => {
            let settings = crate::utils::settings::get_settings_for_source(source);
            let existing = settings
                .and_then(|settings| settings.permissions)
                .and_then(|permissions| match behavior {
                    PermissionBehavior::Allow => permissions.allow,
                    PermissionBehavior::Deny => permissions.deny,
                    PermissionBehavior::Ask => permissions.ask,
                })
                .unwrap_or_default();
            let removals = rules
                .iter()
                .map(permission_rule_value_to_string)
                .collect::<std::collections::HashSet<_>>();
            let filtered = existing
                .into_iter()
                .filter(|raw| {
                    !removals.contains(&permission_rule_value_to_string(
                        &permission_rule_value_from_string(raw),
                    ))
                })
                .collect::<Vec<_>>();
            serde_json::json!({(behavior_key(*behavior)): filtered})
        }
        PermissionUpdate::RemoveDirectories { directories, .. } => {
            let existing = crate::utils::settings::get_settings_for_source(source)
                .and_then(|settings| settings.permissions)
                .and_then(|permissions| permissions.additional_directories)
                .unwrap_or_default();
            let filtered = existing
                .into_iter()
                .filter(|directory| !directories.contains(directory))
                .collect::<Vec<_>>();
            serde_json::json!({"additionalDirectories": filtered})
        }
        PermissionUpdate::SetMode { mode, .. } => {
            serde_json::json!({"defaultMode": mode})
        }
        PermissionUpdate::ReplaceRules {
            rules, behavior, ..
        } => {
            let rule_strings = rules
                .iter()
                .map(permission_rule_value_to_string)
                .collect::<Vec<_>>();
            serde_json::json!({(behavior_key(*behavior)): rule_strings})
        }
    };
    let patch = serde_json::Map::from_iter([("permissions".to_string(), permissions)]);
    // CC PermissionUpdate.ts:259-263,289-293,309-313,321-325,334-338 ignores
    // the `{ error }` return from settings.ts:517-525. Preserve continuation
    // across ordinary writer failures; the explicit no-write gate above is
    // the only error produced by this persistence entry.
    if let Err(error) = crate::utils::settings::update_settings_for_source(source, &patch) {
        crate::utils::debug::log_for_debugging(&error.to_string());
    }
    Ok(true)
}

/// Maps to: CC `utils/permissions/PermissionUpdate.ts:349-355#persistPermissionUpdates`.
/// Each update settles independently in input order. Ordinary writer failures
/// do not skip later updates; only the explicit Cometix no-write error does.
pub fn persist_permission_updates(updates: &[PermissionUpdate]) -> anyhow::Result<()> {
    for update in updates {
        persist_permission_update(update)?;
    }
    Ok(())
}

/// Narrow Rust representation adapter for CC
/// `utils/permissions/PermissionUpdate.ts:42-172#applyPermissionUpdate`, where
/// `update.destination` can directly index a source-keyed object. Rust keeps
/// update destinations and live rule sources as distinct enums, so this owner
/// performs the one mechanical conversion at the mutation/UI projection seam.
pub fn permission_rule_source_for_destination(
    destination: PermissionUpdateDestination,
) -> PermissionRuleSource {
    match destination {
        PermissionUpdateDestination::UserSettings => PermissionRuleSource::UserSettings,
        PermissionUpdateDestination::ProjectSettings => PermissionRuleSource::ProjectSettings,
        PermissionUpdateDestination::LocalSettings => PermissionRuleSource::LocalSettings,
        PermissionUpdateDestination::Session => PermissionRuleSource::Session,
        PermissionUpdateDestination::CliArg => PermissionRuleSource::CliArg,
    }
}

fn rules_for_behavior_mut(
    context: &mut ToolPermissionContext,
    behavior: PermissionBehavior,
) -> &mut crate::types::permissions::ToolPermissionRulesBySource {
    match behavior {
        PermissionBehavior::Allow => &mut context.always_allow_rules,
        PermissionBehavior::Deny => &mut context.always_deny_rules,
        PermissionBehavior::Ask => &mut context.always_ask_rules,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::permissions::{PermissionMode, PermissionUpdateDestination};

    #[test]
    fn extract_rules_matches_official_add_rules_only_semantics() {
        let rule = PermissionRuleValue::new("Read", Some("src/**".to_string()));
        let updates = vec![
            PermissionUpdate::AddRules {
                destination: PermissionUpdateDestination::Session,
                behavior: PermissionBehavior::Allow,
                rules: vec![rule.clone()],
            },
            PermissionUpdate::ReplaceRules {
                destination: PermissionUpdateDestination::Session,
                behavior: PermissionBehavior::Allow,
                rules: vec![PermissionRuleValue::new("Grep", None)],
            },
            PermissionUpdate::RemoveRules {
                destination: PermissionUpdateDestination::Session,
                behavior: PermissionBehavior::Allow,
                rules: vec![PermissionRuleValue::new("Glob", None)],
            },
        ];

        assert_eq!(extract_rules(&updates), vec![rule]);
    }

    #[test]
    fn read_rule_suggestion_matches_absolute_relative_and_root_shapes() {
        assert_eq!(
            create_read_rule_suggestion("/repo/src", PermissionUpdateDestination::Session),
            Some(PermissionUpdate::AddRules {
                destination: PermissionUpdateDestination::Session,
                behavior: PermissionBehavior::Allow,
                rules: vec![PermissionRuleValue::new(
                    "Read",
                    Some("//repo/src/**".to_string())
                )],
            })
        );
        assert!(create_read_rule_suggestion("/", PermissionUpdateDestination::Session).is_none());
        assert!(create_read_rule_suggestion("src", PermissionUpdateDestination::Session)
            .is_some_and(|update| matches!(update, PermissionUpdate::AddRules { rules, .. } if rules[0].rule_content.as_deref() == Some("src/**"))));
    }

    #[test]
    fn permission_update_adds_in_memory_allow_rule_without_persistence() {
        let ctx = ToolPermissionContext::default();
        let rule = PermissionRuleValue::new("Bash", Some("cargo test".to_string()));
        let next = apply_permission_update(
            &ctx,
            &PermissionUpdate::AddRules {
                destination: PermissionUpdateDestination::Session,
                behavior: PermissionBehavior::Allow,
                rules: vec![rule.clone()],
            },
        );

        assert!(
            next.always_allow_rules
                .get(&PermissionRuleSource::Session)
                .is_some_and(|rules| rules.contains(&rule))
        );
        assert!(ctx.always_allow_rules.is_empty());
    }

    #[test]
    fn add_rules_appends_duplicates_and_all_destinations_route_to_exact_sources() {
        let destinations = [
            (
                PermissionUpdateDestination::UserSettings,
                PermissionRuleSource::UserSettings,
            ),
            (
                PermissionUpdateDestination::ProjectSettings,
                PermissionRuleSource::ProjectSettings,
            ),
            (
                PermissionUpdateDestination::LocalSettings,
                PermissionRuleSource::LocalSettings,
            ),
            (
                PermissionUpdateDestination::Session,
                PermissionRuleSource::Session,
            ),
            (
                PermissionUpdateDestination::CliArg,
                PermissionRuleSource::CliArg,
            ),
        ];
        let duplicate = PermissionRuleValue::new("Read", None);
        for (destination, source) in destinations {
            let next = apply_permission_update(
                &ToolPermissionContext::default(),
                &PermissionUpdate::AddRules {
                    destination,
                    behavior: PermissionBehavior::Allow,
                    rules: vec![duplicate.clone(), duplicate.clone()],
                },
            );
            assert_eq!(next.always_allow_rules[&source].len(), 2);
            assert_eq!(next.always_allow_rules.len(), 1);
        }
    }

    #[test]
    fn permission_update_set_mode_is_in_memory() {
        let next = apply_permission_update(
            &ToolPermissionContext::default(),
            &PermissionUpdate::SetMode {
                destination: PermissionUpdateDestination::Session,
                mode: PermissionMode::AcceptEdits,
            },
        );

        assert_eq!(next.mode, PermissionMode::AcceptEdits);
    }

    #[test]
    fn directory_update_order_matches_official_map_set_delete_reinsert() {
        let next = apply_permission_update(
            &ToolPermissionContext::default(),
            &PermissionUpdate::AddDirectories {
                destination: PermissionUpdateDestination::Session,
                directories: vec![
                    "/z".into(),
                    "/middle".into(),
                    "/a".into(),
                    "/b".into(),
                    "/z".into(),
                ],
            },
        );
        // CC Tool.ts:125 + PermissionUpdate.ts:122-135,171-182: Map.set(existing)
        // retains its position; Map.delete preserves surviving insertion order.
        assert_eq!(
            next.additional_working_directories
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["/z", "/middle", "/a", "/b"]
        );
        let removed = apply_permission_update(
            &next,
            &PermissionUpdate::RemoveDirectories {
                destination: PermissionUpdateDestination::Session,
                directories: vec!["/middle".into()],
            },
        );
        assert_eq!(
            removed
                .additional_working_directories
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["/z", "/a", "/b"]
        );
        let reinserted = apply_permission_update(
            &removed,
            &PermissionUpdate::AddDirectories {
                destination: PermissionUpdateDestination::Session,
                directories: vec!["/middle".into()],
            },
        );
        assert_eq!(
            reinserted
                .additional_working_directories
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["/z", "/a", "/b", "/middle"]
        );
    }

    #[test]
    fn directory_updates_mutate_official_additional_working_directories_map() {
        let next = apply_permission_update(
            &ToolPermissionContext::default(),
            &PermissionUpdate::AddDirectories {
                destination: PermissionUpdateDestination::Session,
                directories: vec!["/tmp/project".to_string()],
            },
        );
        let directory = next
            .additional_working_directories
            .get("/tmp/project")
            .expect("directory should be added");
        assert_eq!(directory.path, "/tmp/project");
        assert_eq!(directory.source, PermissionRuleSource::Session);

        let removed = apply_permission_update(
            &next,
            &PermissionUpdate::RemoveDirectories {
                destination: PermissionUpdateDestination::Session,
                directories: vec!["/tmp/project".to_string()],
            },
        );
        assert!(removed.additional_working_directories.is_empty());
    }
}

#[cfg(test)]
mod persistence_tests {
    use super::*;
    use crate::utils::settings::{
        SettingSource, get_settings_for_source, settings_cache::reset_settings_cache,
    };
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};
    use serde_json::json;
    use std::path::PathBuf;

    struct Fixture {
        root: PathBuf,
        original_cwd: PathBuf,
        _config: EnvVarGuard,
        _managed: EnvVarGuard,
        _writes: EnvVarGuard,
    }

    impl Fixture {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("cc-permission-update-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&root).unwrap();
            let root = root.canonicalize().unwrap();
            let original_cwd = crate::bootstrap::state::get_original_cwd();
            std::fs::create_dir_all(root.join("workspace")).unwrap();
            crate::bootstrap::state::set_original_cwd(root.join("workspace"));
            let fixture = Self {
                original_cwd,
                _config: EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root),
                _managed: EnvVarGuard::set("CLAUDE_CODE_MANAGED_SETTINGS_PATH", &root),
                _writes: EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1"),
                root,
            };
            reset_settings_cache();
            fixture
        }

        fn read(&self) -> serde_json::Value {
            serde_json::from_str(&std::fs::read_to_string(self.root.join("settings.json")).unwrap())
                .unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            crate::bootstrap::state::set_original_cwd(&self.original_cwd);
            reset_settings_cache();
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// CC PermissionUpdate.ts:235-242 ignores loader false; PermissionContext.ts:
    /// 141-145 then applies the SAME updates to live state. Managed-only gates disk
    /// addition rather than adding a new in-memory denial at this layer.
    #[test]
    fn persist_add_rules_matches_official_managed_only_disk_skip_and_live_projection() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let fixture = Fixture::new();
        std::fs::write(
            fixture.root.join("managed-settings.json"),
            r#"{"allowManagedPermissionRulesOnly":true}"#,
        )
        .unwrap();
        reset_settings_cache();
        let update = PermissionUpdate::AddRules {
            destination: PermissionUpdateDestination::UserSettings,
            behavior: PermissionBehavior::Allow,
            rules: vec![PermissionRuleValue::new("Read", None)],
        };
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        assert!(
            crate::hooks::tool_permission::permission_context::persist_permissions(
                &store,
                &[update]
            )
            .unwrap()
        );
        assert!(!fixture.root.join("settings.json").exists());
        assert_eq!(
            store.tool_permission_context().always_allow_rules[&PermissionRuleSource::UserSettings],
            vec![PermissionRuleValue::new("Read", None)]
        );
    }

    /// CC permissionsLoader.ts:292-295 catches an ordinary AddRules failure and
    /// PermissionUpdate.ts:349-355 continues subsequent updates in the batch.
    #[test]
    fn persist_updates_matches_official_lenient_boundary_caught_failure_continues_batch() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let fixture = Fixture::new();
        std::fs::write(
            fixture.root.join("settings.json"),
            r#"{"permissions":{"deny":"wrong-type"}}"#,
        )
        .unwrap();
        reset_settings_cache();
        // CC settings.ts:219-225 supplies null after failed schema validation.
        // Isolate this source producer boundary from Rust validation.rs:362-377's
        // still-open tolerant-load policy; this test does not claim live fallback.
        assert!(
            crate::utils::zod::safe_parse(
                crate::utils::settings::types::settings_schema(),
                &fixture.read(),
            )
            .is_err()
        );
        crate::utils::settings::settings_cache::set_cached_settings_for_source(
            SettingSource::User,
            None,
        );
        persist_permission_updates(&[
            PermissionUpdate::AddRules {
                destination: PermissionUpdateDestination::UserSettings,
                behavior: PermissionBehavior::Deny,
                rules: vec![PermissionRuleValue::new("Bash", None)],
            },
            PermissionUpdate::AddRules {
                destination: PermissionUpdateDestination::UserSettings,
                behavior: PermissionBehavior::Allow,
                rules: vec![PermissionRuleValue::new("Read", None)],
            },
        ])
        .unwrap();
        assert_eq!(
            fixture.read(),
            json!({"permissions":{"deny":"wrong-type","allow":["Read"]}})
        );
    }

    /// CC PermissionUpdate.ts:245-346 delegates to settings and compares additions
    /// against the original directory array; settings.ts:505-506 clears read caches.
    #[test]
    fn persist_variants_match_official_arrays_normalization_and_cache() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let fixture = Fixture::new();
        assert!(get_settings_for_source(SettingSource::User).is_none());
        persist_permission_updates(&[
            PermissionUpdate::AddDirectories {
                destination: PermissionUpdateDestination::UserSettings,
                directories: vec!["/z".into(), "/z".into(), "/a".into()],
            },
            PermissionUpdate::RemoveDirectories {
                destination: PermissionUpdateDestination::UserSettings,
                directories: vec!["/a".into()],
            },
            PermissionUpdate::ReplaceRules {
                destination: PermissionUpdateDestination::UserSettings,
                behavior: PermissionBehavior::Allow,
                rules: vec![
                    PermissionRuleValue::new("TaskStop", None),
                    PermissionRuleValue::new("Read", None),
                ],
            },
            PermissionUpdate::RemoveRules {
                destination: PermissionUpdateDestination::UserSettings,
                behavior: PermissionBehavior::Allow,
                rules: vec![PermissionRuleValue::new("TaskStop", None)],
            },
            PermissionUpdate::SetMode {
                destination: PermissionUpdateDestination::UserSettings,
                mode: crate::types::permissions::PermissionMode::AcceptEdits,
            },
        ])
        .unwrap();
        assert_eq!(
            fixture.read()["permissions"],
            json!({"additionalDirectories":["/z","/z"],"allow":["Read"],"defaultMode":"acceptEdits"})
        );
        assert_eq!(
            get_settings_for_source(SettingSource::User)
                .unwrap()
                .permissions
                .unwrap()
                .allow
                .unwrap(),
            vec!["Read"]
        );
    }

    /// Explicit retained Cometix seam: disabling writes errors at the persistence
    /// entry before any settings/state publication, even though CC has no gate.
    #[test]
    fn persist_updates_retains_explicit_no_write_error_before_live_projection() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let fixture = Fixture::new();
        let _disabled = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "0");
        let update = PermissionUpdate::AddRules {
            destination: PermissionUpdateDestination::UserSettings,
            behavior: PermissionBehavior::Allow,
            rules: vec![PermissionRuleValue::new("Read", None)],
        };
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        assert_eq!(
            crate::hooks::tool_permission::permission_context::persist_permissions(
                &store,
                &[update]
            )
            .unwrap_err()
            .to_string(),
            crate::tools::shared::write_gate::PERMISSION_PERSISTENCE_DISABLED_ERROR
        );
        assert!(!fixture.root.join("settings.json").exists());
        assert!(
            store
                .tool_permission_context()
                .always_allow_rules
                .is_empty()
        );
    }

    /// CC PermissionUpdate.ts:259-338 ignores the settings writer's returned
    /// error for every non-AddRules branch; :349-355 keeps processing and
    /// PermissionContext.ts:141-145 publishes all live updates afterwards.
    #[test]
    fn persist_non_add_failures_match_official_batch_continuation_and_live_publication() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let fixture = Fixture::new();
        std::fs::write(fixture.root.join("settings.json"), "invalid json").unwrap();
        reset_settings_cache();
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        let updates = [
            PermissionUpdate::SetMode {
                destination: PermissionUpdateDestination::UserSettings,
                mode: crate::types::permissions::PermissionMode::AcceptEdits,
            },
            PermissionUpdate::ReplaceRules {
                destination: PermissionUpdateDestination::UserSettings,
                behavior: PermissionBehavior::Allow,
                rules: vec![
                    PermissionRuleValue::new("Glob", None),
                    PermissionRuleValue::new("Grep", None),
                ],
            },
            PermissionUpdate::RemoveRules {
                destination: PermissionUpdateDestination::UserSettings,
                behavior: PermissionBehavior::Allow,
                rules: vec![PermissionRuleValue::new("Glob", None)],
            },
            PermissionUpdate::AddDirectories {
                destination: PermissionUpdateDestination::UserSettings,
                directories: vec!["/first".into(), "/second".into()],
            },
            PermissionUpdate::RemoveDirectories {
                destination: PermissionUpdateDestination::UserSettings,
                directories: vec!["/first".into()],
            },
            PermissionUpdate::AddRules {
                destination: PermissionUpdateDestination::ProjectSettings,
                behavior: PermissionBehavior::Allow,
                rules: vec![PermissionRuleValue::new("Read", None)],
            },
        ];
        assert!(
            crate::hooks::tool_permission::permission_context::persist_permissions(
                &store, &updates
            )
            .unwrap()
        );
        assert_eq!(
            std::fs::read_to_string(fixture.root.join("settings.json")).unwrap(),
            "invalid json"
        );
        let project: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(fixture.root.join("workspace/.claude/settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(project["permissions"]["allow"], json!(["Read"]));
        let live = store.tool_permission_context();
        assert_eq!(
            live.mode,
            crate::types::permissions::PermissionMode::AcceptEdits
        );
        assert_eq!(
            live.always_allow_rules[&PermissionRuleSource::UserSettings],
            vec![PermissionRuleValue::new("Grep", None)]
        );
        assert_eq!(
            live.always_allow_rules[&PermissionRuleSource::ProjectSettings],
            vec![PermissionRuleValue::new("Read", None)]
        );
        assert_eq!(
            live.additional_working_directories
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec!["/second"]
        );
    }
}
