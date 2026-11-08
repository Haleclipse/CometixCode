//! Maps to: CC `utils/permissions/PermissionRule.ts`.
//!
//! Runtime schema helpers for permission rule values. Core type definitions live
//! in `types/permissions.rs`, matching CC's extraction to
//! `types/permissions.ts`.

pub use crate::types::permissions::{
    PermissionBehavior, PermissionRule, PermissionRuleSource, PermissionRuleValue,
};

/// Maps to: CC `permissionBehaviorSchema`.
pub fn permission_behavior_from_official_str(value: &str) -> Option<PermissionBehavior> {
    match value {
        "allow" => Some(PermissionBehavior::Allow),
        "deny" => Some(PermissionBehavior::Deny),
        "ask" => Some(PermissionBehavior::Ask),
        _ => None,
    }
}

pub fn permission_behavior_to_official_str(value: PermissionBehavior) -> &'static str {
    match value {
        PermissionBehavior::Allow => "allow",
        PermissionBehavior::Deny => "deny",
        PermissionBehavior::Ask => "ask",
    }
}

pub fn permission_rule_source_from_official_str(value: &str) -> Option<PermissionRuleSource> {
    match value {
        "userSettings" => Some(PermissionRuleSource::UserSettings),
        "projectSettings" => Some(PermissionRuleSource::ProjectSettings),
        "localSettings" => Some(PermissionRuleSource::LocalSettings),
        "flagSettings" => Some(PermissionRuleSource::FlagSettings),
        "cliArg" => Some(PermissionRuleSource::CliArg),
        "command" => Some(PermissionRuleSource::Command),
        "session" => Some(PermissionRuleSource::Session),
        _ => None,
    }
}

pub fn permission_rule_source_to_official_str(value: PermissionRuleSource) -> &'static str {
    match value {
        PermissionRuleSource::UserSettings => "userSettings",
        PermissionRuleSource::ProjectSettings => "projectSettings",
        PermissionRuleSource::LocalSettings => "localSettings",
        PermissionRuleSource::FlagSettings => "flagSettings",
        PermissionRuleSource::CliArg => "cliArg",
        PermissionRuleSource::Command => "command",
        PermissionRuleSource::Session => "session",
    }
}

/// Maps to: CC `permissionRuleValueSchema`.
pub fn permission_rule_value_from_official_json(
    value: &serde_json::Value,
) -> Option<PermissionRuleValue> {
    let object = value.as_object()?;
    Some(PermissionRuleValue::new(
        object.get("toolName")?.as_str()?,
        object
            .get("ruleContent")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
    ))
}

pub fn permission_rule_value_to_official_json(value: &PermissionRuleValue) -> serde_json::Value {
    let mut object = serde_json::Map::new();
    object.insert(
        "toolName".to_string(),
        serde_json::Value::String(value.tool_name.clone()),
    );
    if let Some(rule_content) = value.rule_content.as_ref() {
        object.insert(
            "ruleContent".to_string(),
            serde_json::Value::String(rule_content.clone()),
        );
    }
    serde_json::Value::Object(object)
}

/// Maps to: CC `PermissionRule.ts:25-27` `permissionBehaviorSchema` —
/// `z.enum(['allow', 'deny', 'ask'])`.
pub fn permission_behavior_schema() -> &'static crate::utils::zod::Schema {
    static SCHEMA: std::sync::OnceLock<crate::utils::zod::Schema> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| crate::utils::zod::enumeration(vec!["allow", "deny", "ask"]))
}

/// Maps to: CC `PermissionRule.ts:35-40` `permissionRuleValueSchema`.
pub fn permission_rule_value_schema() -> &'static crate::utils::zod::Schema {
    static SCHEMA: std::sync::OnceLock<crate::utils::zod::Schema> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| {
        use crate::utils::zod;
        zod::object(vec![
            ("toolName", zod::string()),
            ("ruleContent", zod::string().optional()),
        ])
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_rule_schema_helpers_match_official_shapes() {
        assert_eq!(
            permission_behavior_from_official_str("allow"),
            Some(PermissionBehavior::Allow)
        );
        assert_eq!(
            permission_rule_source_from_official_str("flagSettings"),
            Some(PermissionRuleSource::FlagSettings)
        );
        assert_eq!(
            permission_rule_source_from_official_str("policySettings"),
            None
        );
        let value = PermissionRuleValue::new("Bash", Some("cargo test:*".to_string()));
        let json = permission_rule_value_to_official_json(&value);
        assert_eq!(json["toolName"], "Bash");
        assert_eq!(json["ruleContent"], "cargo test:*");
        assert_eq!(permission_rule_value_from_official_json(&json), Some(value));
    }
}
