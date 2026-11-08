//! Permission update schema parsing.
//! Maps to: CC `utils/permissions/PermissionUpdateSchema.ts`.
//!
//! This file owns the external JSON shape (`type: addRules`, `destination:
//! session`, `toolName`, etc.). Applying already-parsed updates remains in
//! `permission_update.rs`, matching CC's split between schema and behavior.

use super::permission_mode::external_permission_mode_from_string;
use crate::types::permissions::{
    PermissionBehavior, PermissionRuleValue, PermissionUpdate, PermissionUpdateDestination,
};

/// Convert parsed permission updates back to the official external JSON shape.
/// Maps to: CC `utils/permissions/PermissionUpdateSchema.ts` serialized
/// `PermissionUpdate[]` values passed through swarm mailboxes.
pub fn permission_updates_to_official_json(updates: &[PermissionUpdate]) -> Vec<serde_json::Value> {
    updates
        .iter()
        .map(permission_update_to_official_json)
        .collect()
}

/// Convert one parsed permission update into official external JSON.
pub fn permission_update_to_official_json(update: &PermissionUpdate) -> serde_json::Value {
    match update {
        PermissionUpdate::AddRules {
            destination,
            behavior,
            rules,
        } => serde_json::json!({
            "type": "addRules",
            "destination": permission_update_destination_to_official_str(*destination),
            "behavior": permission_behavior_to_official_str(*behavior),
            "rules": permission_rules_to_official_json(rules),
        }),
        PermissionUpdate::ReplaceRules {
            destination,
            behavior,
            rules,
        } => serde_json::json!({
            "type": "replaceRules",
            "destination": permission_update_destination_to_official_str(*destination),
            "behavior": permission_behavior_to_official_str(*behavior),
            "rules": permission_rules_to_official_json(rules),
        }),
        PermissionUpdate::RemoveRules {
            destination,
            behavior,
            rules,
        } => serde_json::json!({
            "type": "removeRules",
            "destination": permission_update_destination_to_official_str(*destination),
            "behavior": permission_behavior_to_official_str(*behavior),
            "rules": permission_rules_to_official_json(rules),
        }),
        PermissionUpdate::SetMode { destination, mode } => serde_json::json!({
            "type": "setMode",
            "destination": permission_update_destination_to_official_str(*destination),
            "mode": super::permission_mode::to_external_permission_mode(*mode),
        }),
        PermissionUpdate::AddDirectories {
            destination,
            directories,
        } => serde_json::json!({
            "type": "addDirectories",
            "destination": permission_update_destination_to_official_str(*destination),
            "directories": directories,
        }),
        PermissionUpdate::RemoveDirectories {
            destination,
            directories,
        } => serde_json::json!({
            "type": "removeDirectories",
            "destination": permission_update_destination_to_official_str(*destination),
            "directories": directories,
        }),
    }
}

/// Parse official external `PermissionUpdate[]` JSON values.
/// Maps to: CC `permissionUpdateSchema()` array consumers.
pub fn permission_updates_from_official_json(value: &serde_json::Value) -> Vec<PermissionUpdate> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(permission_update_from_official_json)
                .collect()
        })
        .unwrap_or_default()
}

/// Parse one official external `PermissionUpdate` JSON value.
/// Maps to: CC `utils/permissions/PermissionUpdateSchema.ts`
/// `permissionUpdateSchema()`.
pub fn permission_update_from_official_json(value: &serde_json::Value) -> Option<PermissionUpdate> {
    // CC validates each complete update before exposing typed rules to a
    // callback. In particular, null/non-string ruleContent must not become
    // an unrestricted rule, and one malformed array member rejects the entry.
    let data = crate::utils::zod::safe_parse(permission_update_zod_schema(), value).ok()?;
    let object = data.as_object()?;
    let destination = object
        .get("destination")
        .and_then(|value| value.as_str())
        .and_then(permission_update_destination_from_official_str)?;
    match object.get("type").and_then(|value| value.as_str())? {
        "addRules" => Some(PermissionUpdate::AddRules {
            destination,
            behavior: object
                .get("behavior")
                .and_then(|value| value.as_str())
                .and_then(permission_behavior_from_official_str)?,
            rules: permission_rules_from_official_json(object.get("rules")?)?,
        }),
        "replaceRules" => Some(PermissionUpdate::ReplaceRules {
            destination,
            behavior: object
                .get("behavior")
                .and_then(|value| value.as_str())
                .and_then(permission_behavior_from_official_str)?,
            rules: permission_rules_from_official_json(object.get("rules")?)?,
        }),
        "removeRules" => Some(PermissionUpdate::RemoveRules {
            destination,
            behavior: object
                .get("behavior")
                .and_then(|value| value.as_str())
                .and_then(permission_behavior_from_official_str)?,
            rules: permission_rules_from_official_json(object.get("rules")?)?,
        }),
        "setMode" => Some(PermissionUpdate::SetMode {
            destination,
            mode: object
                .get("mode")
                .and_then(|value| value.as_str())
                .and_then(external_permission_mode_from_string)?,
        }),
        "addDirectories" => Some(PermissionUpdate::AddDirectories {
            destination,
            directories: string_array_from_official_json(object.get("directories")?)?,
        }),
        "removeDirectories" => Some(PermissionUpdate::RemoveDirectories {
            destination,
            directories: string_array_from_official_json(object.get("directories")?)?,
        }),
        _ => None,
    }
}

fn string_array_from_official_json(value: &serde_json::Value) -> Option<Vec<String>> {
    value
        .as_array()?
        .iter()
        .map(|item| item.as_str().map(str::to_string))
        .collect()
}

fn permission_rules_to_official_json(rules: &[PermissionRuleValue]) -> Vec<serde_json::Value> {
    rules
        .iter()
        .map(|rule| {
            let mut object = serde_json::Map::new();
            object.insert(
                "toolName".to_string(),
                serde_json::Value::String(rule.tool_name.clone()),
            );
            if let Some(rule_content) = &rule.rule_content {
                object.insert(
                    "ruleContent".to_string(),
                    serde_json::Value::String(rule_content.clone()),
                );
            }
            serde_json::Value::Object(object)
        })
        .collect()
}

fn permission_rules_from_official_json(
    value: &serde_json::Value,
) -> Option<Vec<PermissionRuleValue>> {
    value
        .as_array()?
        .iter()
        .map(|rule| {
            let object = rule.as_object()?;
            let tool_name = object.get("toolName")?.as_str()?;
            let rule_content = match object.get("ruleContent") {
                Some(value) => Some(value.as_str()?.to_string()),
                None => None,
            };
            Some(PermissionRuleValue::new(tool_name, rule_content))
        })
        .collect()
}

fn permission_update_destination_to_official_str(
    destination: PermissionUpdateDestination,
) -> &'static str {
    match destination {
        PermissionUpdateDestination::UserSettings => "userSettings",
        PermissionUpdateDestination::ProjectSettings => "projectSettings",
        PermissionUpdateDestination::LocalSettings => "localSettings",
        PermissionUpdateDestination::Session => "session",
        PermissionUpdateDestination::CliArg => "cliArg",
    }
}

fn permission_update_destination_from_official_str(
    value: &str,
) -> Option<PermissionUpdateDestination> {
    match value {
        "userSettings" => Some(PermissionUpdateDestination::UserSettings),
        "projectSettings" => Some(PermissionUpdateDestination::ProjectSettings),
        "localSettings" => Some(PermissionUpdateDestination::LocalSettings),
        "session" => Some(PermissionUpdateDestination::Session),
        "cliArg" => Some(PermissionUpdateDestination::CliArg),
        _ => None,
    }
}

fn permission_behavior_to_official_str(behavior: PermissionBehavior) -> &'static str {
    match behavior {
        PermissionBehavior::Allow => "allow",
        PermissionBehavior::Deny => "deny",
        PermissionBehavior::Ask => "ask",
    }
}

fn permission_behavior_from_official_str(value: &str) -> Option<PermissionBehavior> {
    match value {
        "allow" => Some(PermissionBehavior::Allow),
        "deny" => Some(PermissionBehavior::Deny),
        "ask" => Some(PermissionBehavior::Ask),
        _ => None,
    }
}

/// Maps to: CC `PermissionUpdateSchema.ts:27-40`
/// `permissionUpdateDestinationSchema`.
pub fn permission_update_destination_schema() -> &'static crate::utils::zod::Schema {
    static SCHEMA: std::sync::OnceLock<crate::utils::zod::Schema> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| {
        crate::utils::zod::enumeration(vec![
            "userSettings",
            "projectSettings",
            "localSettings",
            "session",
            "cliArg",
        ])
    })
}

/// Maps to: CC `PermissionUpdateSchema.ts:42-78` `permissionUpdateSchema` —
/// the six-arm discriminated union on `type`. The serde converters above stay
/// as the post-parse projection.
pub fn permission_update_zod_schema() -> &'static crate::utils::zod::Schema {
    static SCHEMA: std::sync::OnceLock<crate::utils::zod::Schema> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| {
        use crate::utils::permissions::permission_mode::external_permission_mode_schema;
        use crate::utils::permissions::permission_rule::{
            permission_behavior_schema, permission_rule_value_schema,
        };
        use crate::utils::zod;
        use serde_json::json;
        let rules_arm = |tag: &'static str| {
            zod::object(vec![
                ("type", zod::literal(json!(tag))),
                ("rules", zod::array(permission_rule_value_schema().clone())),
                ("behavior", permission_behavior_schema().clone()),
                (
                    "destination",
                    permission_update_destination_schema().clone(),
                ),
            ])
        };
        let directories_arm = |tag: &'static str| {
            zod::object(vec![
                ("type", zod::literal(json!(tag))),
                ("directories", zod::array(zod::string())),
                (
                    "destination",
                    permission_update_destination_schema().clone(),
                ),
            ])
        };
        zod::discriminated_union(
            "type",
            vec![
                rules_arm("addRules"),
                rules_arm("replaceRules"),
                rules_arm("removeRules"),
                zod::object(vec![
                    ("type", zod::literal(json!("setMode"))),
                    ("mode", external_permission_mode_schema().clone()),
                    (
                        "destination",
                        permission_update_destination_schema().clone(),
                    ),
                ]),
                directories_arm("addDirectories"),
                directories_arm("removeDirectories"),
            ],
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::ToolPermissionContext;
    use crate::types::permissions::PermissionMode;
    use crate::utils::permissions::permission_update::apply_permission_updates;

    #[test]
    fn permission_updates_match_official_complete_entry_validation() {
        use serde_json::json;
        for kind in ["addRules", "replaceRules", "removeRules"] {
            for bad_rule in [
                json!({"toolName":"Bash", "ruleContent":42}),
                json!({"toolName":"Bash", "ruleContent":null}),
                json!({"toolName":42}),
                json!({"ruleContent":"ls"}),
                json!(null),
            ] {
                let update = json!({"type":kind,"destination":"session","behavior":"allow",
                    "rules":[{"toolName":"Read","ruleContent":"src/**"},bad_rule]});
                assert!(
                    permission_update_from_official_json(&update).is_none(),
                    "{update}"
                );
            }
            let update = json!({"type":kind,"destination":"session","behavior":"allow",
                "rules":[{"toolName":"Bash"},{"toolName":"Read","ruleContent":""}],"unknown":true});
            let parsed = permission_update_from_official_json(&update)
                .expect("optional string and unknown keys");
            let encoded = permission_update_to_official_json(&parsed);
            assert_eq!(encoded["rules"], update["rules"]);
            assert!(encoded.get("unknown").is_none());
        }
        for kind in ["addDirectories", "removeDirectories"] {
            for bad in [json!(42), json!(null), json!({})] {
                assert!(
                    permission_update_from_official_json(&json!({"type":kind,
                    "destination":"session","directories":["/repo",bad]}))
                    .is_none()
                );
            }
        }
        for mode in [json!("auto"), json!(null), json!(42)] {
            assert!(
                permission_update_from_official_json(&json!({"type":"setMode",
                "destination":"session","mode":mode}))
                .is_none()
            );
        }
        let updates = permission_updates_from_official_json(&json!([
            {"type":"addRules","destination":"session","behavior":"allow",
                "rules":[{"toolName":"Bash","ruleContent":null}]},
            {"type":"addRules","destination":"session","behavior":"allow",
                "rules":[{"toolName":"Read","ruleContent":"src/**"}]}
        ]));
        assert_eq!(updates.len(), 1);
        let next = apply_permission_updates(&ToolPermissionContext::default(), &updates);
        assert_eq!(
            next.always_allow_rules[&crate::types::permissions::PermissionRuleSource::Session],
            vec![PermissionRuleValue::new("Read", Some("src/**".to_string()))]
        );
    }

    #[test]
    fn parses_official_permission_updates_json() {
        let updates = permission_updates_from_official_json(&serde_json::json!([
            {
                "type": "addRules",
                "destination": "session",
                "behavior": "allow",
                "rules": [{"toolName": "Bash", "ruleContent": "cargo test"}]
            },
            {
                "type": "setMode",
                "destination": "session",
                "mode": "acceptEdits"
            }
        ]));

        assert_eq!(updates.len(), 2);
        let next = apply_permission_updates(&ToolPermissionContext::default(), &updates);
        assert_eq!(next.mode, PermissionMode::AcceptEdits);
        assert!(
            next.always_allow_rules
                .get(&crate::types::permissions::PermissionRuleSource::Session)
                .is_some_and(|rules| rules.contains(&PermissionRuleValue::new(
                    "Bash",
                    Some("cargo test".to_string())
                )))
        );
    }

    #[test]
    fn rejects_non_destination_rule_sources_at_manual_schema_boundary() {
        for destination in ["command", "flagSettings"] {
            let updates = permission_updates_from_official_json(&serde_json::json!([{
                "type": "addRules",
                "destination": destination,
                "behavior": "allow",
                "rules": [{"toolName": "Read"}]
            }]));
            assert!(updates.is_empty(), "{destination} must not be routable");
        }
    }

    #[test]
    fn serializes_permission_updates_to_official_mailbox_json() {
        let updates = vec![
            PermissionUpdate::AddRules {
                destination: PermissionUpdateDestination::Session,
                behavior: PermissionBehavior::Allow,
                rules: vec![PermissionRuleValue::new(
                    "Bash",
                    Some("cargo test".to_string()),
                )],
            },
            PermissionUpdate::SetMode {
                destination: PermissionUpdateDestination::Session,
                mode: PermissionMode::AcceptEdits,
            },
        ];

        let json = permission_updates_to_official_json(&updates);
        assert_eq!(
            json,
            vec![
                serde_json::json!({
                    "type": "addRules",
                    "destination": "session",
                    "behavior": "allow",
                    "rules": [{"toolName": "Bash", "ruleContent": "cargo test"}],
                }),
                serde_json::json!({
                    "type": "setMode",
                    "destination": "session",
                    "mode": "acceptEdits",
                }),
            ]
        );
        assert_eq!(
            permission_updates_from_official_json(&serde_json::Value::Array(json)),
            updates
        );
    }

    #[test]
    fn parses_official_directory_permission_updates_for_ui_suggestions() {
        let updates = permission_updates_from_official_json(&serde_json::json!([
            {"type": "addDirectories", "destination": "session", "directories": ["/tmp"]},
            {"type": "removeDirectories", "destination": "localSettings", "directories": ["/repo"]}
        ]));

        assert_eq!(
            updates,
            vec![
                PermissionUpdate::AddDirectories {
                    destination: PermissionUpdateDestination::Session,
                    directories: vec!["/tmp".to_string()],
                },
                PermissionUpdate::RemoveDirectories {
                    destination: PermissionUpdateDestination::LocalSettings,
                    directories: vec!["/repo".to_string()],
                },
            ]
        );

        let next = apply_permission_updates(&ToolPermissionContext::default(), &updates);
        assert!(
            next.additional_working_directories
                .get("/tmp")
                .is_some_and(|directory| directory.path == "/tmp")
        );
        assert!(!next.additional_working_directories.contains_key("/repo"));
    }
}
