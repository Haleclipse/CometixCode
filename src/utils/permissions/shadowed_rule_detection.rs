//! Maps to: CC `utils/permissions/shadowedRuleDetection.ts`.

use super::permissions::permission_rule_source_display_string;
use crate::tool::ToolPermissionContext;
use crate::types::permissions::{PermissionRule, PermissionRuleSource, PermissionRuleValue};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShadowType {
    Ask,
    Deny,
}

impl ShadowType {
    pub fn as_str(self) -> &'static str {
        match self {
            ShadowType::Ask => "ask",
            ShadowType::Deny => "deny",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnreachableRule {
    pub rule: PermissionRule,
    pub reason: String,
    pub shadowed_by: PermissionRule,
    pub shadow_type: ShadowType,
    pub fix: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DetectUnreachableRulesOptions {
    pub sandbox_auto_allow_enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShadowResult {
    NotShadowed,
    Shadowed {
        shadowed_by: PermissionRule,
        shadow_type: ShadowType,
    },
}

/// Maps to: CC `isSharedSettingSource(...)`.
pub fn is_shared_setting_source(source: PermissionRuleSource) -> bool {
    matches!(
        source,
        PermissionRuleSource::ProjectSettings
            | PermissionRuleSource::PolicySettings
            | PermissionRuleSource::Command
    )
}

/// Maps to: CC `generateFixSuggestion(...)`.
pub fn generate_fix_suggestion(
    shadow_type: ShadowType,
    shadowing_rule: &PermissionRule,
    shadowed_rule: &PermissionRule,
) -> String {
    let shadowing_source = permission_rule_source_display_string(shadowing_rule.source);
    let shadowed_source = permission_rule_source_display_string(shadowed_rule.source);
    let tool_name = &shadowing_rule.rule_value.tool_name;
    match shadow_type {
        ShadowType::Deny => format!(
            "Remove the \"{tool_name}\" deny rule from {shadowing_source}, or remove the specific allow rule from {shadowed_source}"
        ),
        ShadowType::Ask => format!(
            "Remove the \"{tool_name}\" ask rule from {shadowing_source}, or remove the specific allow rule from {shadowed_source}"
        ),
    }
}

/// Maps to: CC `isAllowRuleShadowedByAskRule(...)`.
pub fn is_allow_rule_shadowed_by_ask_rule(
    allow_rule: &PermissionRule,
    ask_rules: &[PermissionRule],
    options: DetectUnreachableRulesOptions,
) -> ShadowResult {
    let PermissionRuleValue {
        tool_name,
        rule_content,
    } = &allow_rule.rule_value;
    if rule_content.is_none() {
        return ShadowResult::NotShadowed;
    }
    let Some(shadowing_ask_rule) = ask_rules.iter().find(|ask_rule| {
        ask_rule.rule_value.tool_name == *tool_name && ask_rule.rule_value.rule_content.is_none()
    }) else {
        return ShadowResult::NotShadowed;
    };

    if tool_name == crate::tools::bash_tool::tool_name::BASH_TOOL_NAME
        && options.sandbox_auto_allow_enabled
        && !is_shared_setting_source(shadowing_ask_rule.source)
    {
        return ShadowResult::NotShadowed;
    }

    ShadowResult::Shadowed {
        shadowed_by: shadowing_ask_rule.clone(),
        shadow_type: ShadowType::Ask,
    }
}

/// Maps to: CC `isAllowRuleShadowedByDenyRule(...)`.
pub fn is_allow_rule_shadowed_by_deny_rule(
    allow_rule: &PermissionRule,
    deny_rules: &[PermissionRule],
) -> ShadowResult {
    let PermissionRuleValue {
        tool_name,
        rule_content,
    } = &allow_rule.rule_value;
    if rule_content.is_none() {
        return ShadowResult::NotShadowed;
    }
    let Some(shadowing_deny_rule) = deny_rules.iter().find(|deny_rule| {
        deny_rule.rule_value.tool_name == *tool_name && deny_rule.rule_value.rule_content.is_none()
    }) else {
        return ShadowResult::NotShadowed;
    };
    ShadowResult::Shadowed {
        shadowed_by: shadowing_deny_rule.clone(),
        shadow_type: ShadowType::Deny,
    }
}

/// Maps to: CC `detectUnreachableRules(...)` using explicit rule lists.
pub fn detect_unreachable_rules_from_rules(
    allow_rules: &[PermissionRule],
    ask_rules: &[PermissionRule],
    deny_rules: &[PermissionRule],
    options: DetectUnreachableRulesOptions,
) -> Vec<UnreachableRule> {
    let mut unreachable = Vec::new();
    for allow_rule in allow_rules {
        match is_allow_rule_shadowed_by_deny_rule(allow_rule, deny_rules) {
            ShadowResult::Shadowed {
                shadowed_by,
                shadow_type,
            } => {
                let shadow_source = permission_rule_source_display_string(shadowed_by.source);
                unreachable.push(UnreachableRule {
                    rule: allow_rule.clone(),
                    reason: format!(
                        "Blocked by \"{}\" deny rule (from {shadow_source})",
                        shadowed_by.rule_value.tool_name
                    ),
                    fix: generate_fix_suggestion(shadow_type, &shadowed_by, allow_rule),
                    shadowed_by,
                    shadow_type,
                });
                continue;
            }
            ShadowResult::NotShadowed => {}
        }

        if let ShadowResult::Shadowed {
            shadowed_by,
            shadow_type,
        } = is_allow_rule_shadowed_by_ask_rule(allow_rule, ask_rules, options)
        {
            let shadow_source = permission_rule_source_display_string(shadowed_by.source);
            unreachable.push(UnreachableRule {
                rule: allow_rule.clone(),
                reason: format!(
                    "Shadowed by \"{}\" ask rule (from {shadow_source})",
                    shadowed_by.rule_value.tool_name
                ),
                fix: generate_fix_suggestion(shadow_type, &shadowed_by, allow_rule),
                shadowed_by,
                shadow_type,
            });
        }
    }
    unreachable
}

/// Maps to: CC `detectUnreachableRules(context, options)`.
pub fn detect_unreachable_rules(
    context: &ToolPermissionContext,
    options: DetectUnreachableRulesOptions,
) -> Vec<UnreachableRule> {
    let allow_rules = super::permissions::get_allow_rules(context);
    let ask_rules = super::permissions::get_ask_rules(context);
    let deny_rules = super::permissions::get_deny_rules(context);
    detect_unreachable_rules_from_rules(&allow_rules, &ask_rules, &deny_rules, options)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::permissions::{PermissionBehavior, PermissionRuleValue};
    use std::collections::HashMap;

    fn rule(
        source: PermissionRuleSource,
        behavior: PermissionBehavior,
        tool: &str,
        content: Option<&str>,
    ) -> PermissionRule {
        PermissionRule {
            source,
            rule_behavior: behavior,
            rule_value: PermissionRuleValue::new(tool, content.map(str::to_string)),
        }
    }

    #[test]
    fn shared_setting_source_matches_official_definition() {
        assert!(is_shared_setting_source(
            PermissionRuleSource::ProjectSettings
        ));
        assert!(is_shared_setting_source(
            PermissionRuleSource::PolicySettings
        ));
        assert!(is_shared_setting_source(PermissionRuleSource::Command));
        assert!(!is_shared_setting_source(
            PermissionRuleSource::UserSettings
        ));
        assert!(!is_shared_setting_source(
            PermissionRuleSource::LocalSettings
        ));
    }

    #[test]
    fn deny_shadowing_takes_precedence_over_ask_shadowing() {
        let allow = rule(
            PermissionRuleSource::LocalSettings,
            PermissionBehavior::Allow,
            "Bash",
            Some("ls:*"),
        );
        let ask = rule(
            PermissionRuleSource::UserSettings,
            PermissionBehavior::Ask,
            "Bash",
            None,
        );
        let deny = rule(
            PermissionRuleSource::ProjectSettings,
            PermissionBehavior::Deny,
            "Bash",
            None,
        );
        let unreachable = detect_unreachable_rules_from_rules(
            &[allow],
            &[ask],
            &[deny],
            DetectUnreachableRulesOptions::default(),
        );
        assert_eq!(unreachable.len(), 1);
        assert_eq!(unreachable[0].shadow_type, ShadowType::Deny);
        assert_eq!(
            unreachable[0].reason,
            "Blocked by \"Bash\" deny rule (from shared project settings)"
        );
    }

    #[test]
    fn sandbox_auto_allow_exempts_personal_bash_ask_but_not_shared_ask() {
        let allow = rule(
            PermissionRuleSource::LocalSettings,
            PermissionBehavior::Allow,
            "Bash",
            Some("npm test:*"),
        );
        let personal_ask = rule(
            PermissionRuleSource::UserSettings,
            PermissionBehavior::Ask,
            "Bash",
            None,
        );
        let options = DetectUnreachableRulesOptions {
            sandbox_auto_allow_enabled: true,
        };
        assert!(
            detect_unreachable_rules_from_rules(
                std::slice::from_ref(&allow),
                &[personal_ask],
                &[],
                options,
            )
            .is_empty()
        );

        let shared_ask = rule(
            PermissionRuleSource::ProjectSettings,
            PermissionBehavior::Ask,
            "Bash",
            None,
        );
        let unreachable =
            detect_unreachable_rules_from_rules(&[allow], &[shared_ask], &[], options);
        assert_eq!(unreachable.len(), 1);
        assert_eq!(unreachable[0].shadow_type, ShadowType::Ask);
        assert!(unreachable[0].fix.contains("ask rule"));
    }

    #[test]
    fn detect_unreachable_rules_reads_current_context_maps() {
        let mut allow = HashMap::new();
        allow.insert(
            PermissionRuleSource::LocalSettings,
            vec![PermissionRuleValue::new(
                "Write",
                Some("src/main.rs".to_string()),
            )],
        );
        let mut deny = HashMap::new();
        deny.insert(
            PermissionRuleSource::UserSettings,
            vec![PermissionRuleValue::new("Write", None)],
        );
        let context = ToolPermissionContext {
            always_allow_rules: allow,
            always_deny_rules: deny,
            ..ToolPermissionContext::default()
        };
        let unreachable =
            detect_unreachable_rules(&context, DetectUnreachableRulesOptions::default());
        assert_eq!(unreachable.len(), 1);
        assert_eq!(
            unreachable[0].shadowed_by.source,
            PermissionRuleSource::UserSettings
        );
    }
}
