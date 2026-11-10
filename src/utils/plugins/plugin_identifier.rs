//! Maps to: CC `utils/plugins/pluginIdentifier.ts`.

use crate::utils::plugins::schemas::PluginScope;
use crate::utils::settings::constants::{EditableSettingSource, SettingSource};

/// Maps to: CC `utils/plugins/pluginIdentifier.ts:14#ExtendedPluginScope`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtendedPluginScope {
    Persistable(PluginScope),
    Flag,
}

/// Maps to: CC `utils/plugins/pluginIdentifier.ts:20#PersistablePluginScope`.
pub type PersistablePluginScope = PluginScope;

/// Maps to: CC `utils/plugins/pluginIdentifier.ts:26-32#SETTING_SOURCE_TO_SCOPE`.
/// @nyan: no `policySettings → managed` row (no managed policy layer).
pub const SETTING_SOURCE_TO_SCOPE: [(SettingSource, ExtendedPluginScope); 4] = [
    (
        SettingSource::User,
        ExtendedPluginScope::Persistable(PluginScope::User),
    ),
    (
        SettingSource::Project,
        ExtendedPluginScope::Persistable(PluginScope::Project),
    ),
    (
        SettingSource::Local,
        ExtendedPluginScope::Persistable(PluginScope::Local),
    ),
    (SettingSource::Flag, ExtendedPluginScope::Flag),
];

/// Maps to: CC `ParsedPluginIdentifier`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedPluginIdentifier {
    pub name: String,
    pub marketplace: Option<String>,
}

/// Maps to: CC `utils/plugins/pluginIdentifier.ts#parsePluginIdentifier`.
pub fn parse_plugin_identifier(plugin: &str) -> ParsedPluginIdentifier {
    if plugin.contains('@') {
        let mut parts = plugin.split('@');
        return ParsedPluginIdentifier {
            name: parts.next().unwrap_or_default().to_string(),
            marketplace: parts.next().map(ToString::to_string),
        };
    }
    ParsedPluginIdentifier {
        name: plugin.to_string(),
        marketplace: None,
    }
}

/// Maps to: CC `utils/plugins/schemas.ts:1339-1347#PluginIdSchema` regex predicate.
/// Existing boolean adapter for cache-only consumers: exactly one `@`; each component starts with
/// an ASCII alphanumeric and then contains only ASCII alphanumerics, `-`,
/// `.`, or `_`.
pub fn is_valid_plugin_id(plugin_id: &str) -> bool {
    let mut parts = plugin_id.split('@');
    let Some(name) = parts.next() else {
        return false;
    };
    let Some(marketplace) = parts.next() else {
        return false;
    };
    if parts.next().is_some() {
        return false;
    }
    [name, marketplace].into_iter().all(|component| {
        let mut bytes = component.bytes();
        bytes
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
            && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_'))
    })
}

/// Maps to: CC `utils/plugins/pluginIdentifier.ts:65-67#buildPluginId`.
pub fn build_plugin_id(name: &str, marketplace: Option<&str>) -> String {
    marketplace
        .filter(|marketplace| !marketplace.is_empty())
        .map(|marketplace| format!("{name}@{marketplace}"))
        .unwrap_or_else(|| name.to_string())
}

/// Maps to: CC `utils/plugins/pluginIdentifier.ts:89-96#SCOPE_TO_EDITABLE_SOURCE`.
const SCOPE_TO_EDITABLE_SOURCE: [(PluginScope, EditableSettingSource); 3] = [
    (PluginScope::User, EditableSettingSource::User),
    (PluginScope::Project, EditableSettingSource::Project),
    (PluginScope::Local, EditableSettingSource::Local),
];

/// Maps to: CC `utils/plugins/pluginIdentifier.ts:104-111#scopeToSettingSource`.
pub fn scope_to_setting_source(scope: PluginScope) -> Result<EditableSettingSource, String> {
    if scope == PluginScope::Managed {
        return Err("Cannot install plugins to managed scope".to_string());
    }
    Ok(SCOPE_TO_EDITABLE_SOURCE
        .iter()
        .find_map(|(candidate, source)| (*candidate == scope).then_some(*source))
        .expect("all non-managed PluginScope variants have an editable source"))
}

/// Maps to: CC `utils/plugins/pluginIdentifier.ts:119-123#settingSourceToScope`.
pub fn setting_source_to_scope(source: EditableSettingSource) -> PersistablePluginScope {
    let scope = SETTING_SOURCE_TO_SCOPE
        .iter()
        .find_map(|(candidate, scope)| {
            (*candidate == SettingSource::from(source)).then_some(*scope)
        })
        .expect("all EditableSettingSource variants have a plugin scope");
    match scope {
        ExtendedPluginScope::Persistable(scope) => scope,
        ExtendedPluginScope::Flag => {
            unreachable!("editable setting sources exclude flag")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_scope_mappings_match_official_bun_oracle() {
        // CC pluginIdentifier.ts:26-32,104-123: forward table, managed throw,
        // and editable inverse. Actual Bun oracle: proof/plugin-services-0913.
        assert_eq!(
            SETTING_SOURCE_TO_SCOPE,
            [
                (
                    SettingSource::User,
                    ExtendedPluginScope::Persistable(PluginScope::User)
                ),
                (
                    SettingSource::Project,
                    ExtendedPluginScope::Persistable(PluginScope::Project)
                ),
                (
                    SettingSource::Local,
                    ExtendedPluginScope::Persistable(PluginScope::Local)
                ),
                (SettingSource::Flag, ExtendedPluginScope::Flag),
            ]
        );
        for (scope, editable, source) in [
            (
                PluginScope::User,
                EditableSettingSource::User,
                SettingSource::User,
            ),
            (
                PluginScope::Project,
                EditableSettingSource::Project,
                SettingSource::Project,
            ),
            (
                PluginScope::Local,
                EditableSettingSource::Local,
                SettingSource::Local,
            ),
        ] {
            assert_eq!(scope_to_setting_source(scope).unwrap(), editable);
            assert_eq!(setting_source_to_scope(editable), scope);
            assert_eq!(SettingSource::from(editable), source);
        }
        assert_eq!(
            scope_to_setting_source(PluginScope::Managed).unwrap_err(),
            "Cannot install plugins to managed scope"
        );
    }

    #[test]
    fn plugin_id_build_matches_official_bun_oracle() {
        // CC pluginIdentifier.ts:65-67: empty marketplace uses the bare name.
        for (marketplace, expected) in [
            (None, "p"),
            (Some(""), "p"),
            (Some("m"), "p@m"),
            (Some(" "), "p@ "),
        ] {
            assert_eq!(build_plugin_id("p", marketplace), expected);
        }
    }

    #[test]
    fn parse_plugin_identifier_uses_only_first_separator_like_official() {
        assert_eq!(
            parse_plugin_identifier("mailbox@anthropic"),
            ParsedPluginIdentifier {
                name: "mailbox".to_string(),
                marketplace: Some("anthropic".to_string()),
            }
        );
        assert_eq!(
            parse_plugin_identifier("plugin@market@ignored"),
            ParsedPluginIdentifier {
                name: "plugin".to_string(),
                marketplace: Some("market".to_string()),
            }
        );
        assert_eq!(
            parse_plugin_identifier("local"),
            ParsedPluginIdentifier {
                name: "local".to_string(),
                marketplace: None,
            }
        );
        assert_eq!(
            build_plugin_id("mailbox", Some("anthropic")),
            "mailbox@anthropic"
        );
        assert_eq!(build_plugin_id("mailbox", None), "mailbox");
        assert!(is_valid_plugin_id("mail-box_2@anthropic.plugins"));
        for invalid in [
            "mailbox",
            "@market",
            "plugin@",
            "plugin@market@extra",
            "../plugin@market",
            "plugin@../market",
            "plugin name@market",
        ] {
            assert!(!is_valid_plugin_id(invalid), "{invalid:?}");
        }
    }
}
