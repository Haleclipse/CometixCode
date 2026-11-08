//! Maps to: CC `components/sandbox/SandboxConfigTab.tsx`.

use crate::utils::sandbox::sandbox_adapter::{
    SandboxDependencyCheck, convert_to_sandbox_runtime_config, get_excluded_commands,
};
use crate::utils::settings::types::SettingsJson;
use iocraft::prelude::*;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SandboxConfigViewData {
    pub is_enabled: bool,
    pub excluded_commands: Vec<String>,
    pub deny_read: Vec<String>,
    pub allow_read: Vec<String>,
    pub allow_write: Vec<String>,
    pub deny_write: Vec<String>,
    pub allowed_hosts: Vec<String>,
    pub denied_hosts: Vec<String>,
    pub allow_unix_sockets: Vec<String>,
    pub warnings: Vec<String>,
}

pub fn sandbox_config_view_data(
    settings: &SettingsJson,
    dep_check: &SandboxDependencyCheck,
) -> SandboxConfigViewData {
    let config = convert_to_sandbox_runtime_config(settings);
    SandboxConfigViewData {
        is_enabled: settings
            .sandbox
            .as_ref()
            .and_then(|sandbox| sandbox.pointer("/enabled"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        excluded_commands: get_excluded_commands(settings),
        deny_read: config.filesystem.deny_read,
        allow_read: config.filesystem.allow_read,
        allow_write: config.filesystem.allow_write,
        deny_write: config.filesystem.deny_write,
        allowed_hosts: config.network.allowed_domains,
        denied_hosts: config.network.denied_domains,
        allow_unix_sockets: config.network.allow_unix_sockets.unwrap_or_default(),
        warnings: dep_check.warnings.clone(),
    }
}

#[derive(Default, Props)]
pub struct SandboxConfigTabProps {
    pub settings: SettingsJson,
    pub dep_check: SandboxDependencyCheck,
}

#[component]
pub fn SandboxConfigTab(
    props: &SandboxConfigTabProps,
    hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<crate::utils::theme::Theme>();
    let data = sandbox_config_view_data(&props.settings, &props.dep_check);

    if !data.is_enabled {
        return element! {
            View(flex_direction: FlexDirection::Column, padding_top: 1u32, padding_bottom: 1u32) {
                Text(content: "Sandbox is not enabled".to_string(), color: theme.inactive, wrap: TextWrap::NoWrap)
                #(data.warnings.into_iter().map(|warning| element! {
                    Text(content: warning, color: theme.inactive, wrap: TextWrap::Wrap)
                }))
            }
        }
        .into_any();
    }

    element! {
        View(flex_direction: FlexDirection::Column, padding_top: 1u32, padding_bottom: 1u32) {
            Text(content: "Excluded Commands:".to_string(), color: theme.permission, weight: Weight::Bold, wrap: TextWrap::NoWrap)
            Text(content: if data.excluded_commands.is_empty() { "None".to_string() } else { data.excluded_commands.join(", ") }, color: theme.inactive, wrap: TextWrap::Wrap)

            #(if !data.deny_read.is_empty() {
                Some(element! {
                    View(flex_direction: FlexDirection::Column, margin_top: 1u32) {
                        Text(content: "Filesystem Read Restrictions:".to_string(), color: theme.permission, weight: Weight::Bold, wrap: TextWrap::NoWrap)
                        Text(content: format!("Denied: {}", data.deny_read.join(", ")), color: theme.inactive, wrap: TextWrap::Wrap)
                        #(if !data.allow_read.is_empty() { Some(element! { Text(content: format!("Allowed within denied: {}", data.allow_read.join(", ")), color: theme.inactive, wrap: TextWrap::Wrap) }) } else { None })
                    }
                })
            } else { None })

            #(if !data.allow_write.is_empty() {
                Some(element! {
                    View(flex_direction: FlexDirection::Column, margin_top: 1u32) {
                        Text(content: "Filesystem Write Restrictions:".to_string(), color: theme.permission, weight: Weight::Bold, wrap: TextWrap::NoWrap)
                        Text(content: format!("Allowed: {}", data.allow_write.join(", ")), color: theme.inactive, wrap: TextWrap::Wrap)
                        #(if !data.deny_write.is_empty() { Some(element! { Text(content: format!("Denied within allowed: {}", data.deny_write.join(", ")), color: theme.inactive, wrap: TextWrap::Wrap) }) } else { None })
                    }
                })
            } else { None })

            #(if !data.allowed_hosts.is_empty() || !data.denied_hosts.is_empty() {
                Some(element! {
                    View(flex_direction: FlexDirection::Column, margin_top: 1u32) {
                        Text(content: "Network Restrictions:".to_string(), color: theme.permission, weight: Weight::Bold, wrap: TextWrap::NoWrap)
                        #(if !data.allowed_hosts.is_empty() { Some(element! { Text(content: format!("Allowed: {}", data.allowed_hosts.join(", ")), color: theme.inactive, wrap: TextWrap::Wrap) }) } else { None })
                        #(if !data.denied_hosts.is_empty() { Some(element! { Text(content: format!("Denied: {}", data.denied_hosts.join(", ")), color: theme.inactive, wrap: TextWrap::Wrap) }) } else { None })
                    }
                })
            } else { None })

            #(if !data.allow_unix_sockets.is_empty() {
                Some(element! {
                    View(flex_direction: FlexDirection::Column, margin_top: 1u32) {
                        Text(content: "Allowed Unix Sockets:".to_string(), color: theme.permission, weight: Weight::Bold, wrap: TextWrap::NoWrap)
                        Text(content: data.allow_unix_sockets.join(", "), color: theme.inactive, wrap: TextWrap::Wrap)
                    }
                })
            } else { None })

            #(data.warnings.into_iter().map(|warning| element! {
                Text(content: warning, color: theme.inactive, wrap: TextWrap::Wrap)
            }))
        }
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;

    #[test]
    fn sandbox_config_tab_renders_disabled_branch_like_official() {
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                SandboxConfigTab(settings: SettingsJson::default(), dep_check: SandboxDependencyCheck::default())
            }
        }
        .render(Some(120))
        .to_string();
        assert!(text.contains("Sandbox is not enabled"), "canvas=\n{text}");
    }

    #[test]
    fn sandbox_config_view_data_projects_official_sections() {
        let settings: SettingsJson = serde_json::from_value(serde_json::json!({
            "sandbox": {
                "enabled": true,
                "network": {"allowedDomains": ["example.com"], "allowUnixSockets": ["/tmp/sock"]},
                "filesystem": {"allowWrite": ["src"], "denyRead": ["/secret"]},
                "excludedCommands": ["npm run test:*"]
            }
        }))
        .unwrap();
        let data = sandbox_config_view_data(&settings, &SandboxDependencyCheck::default());
        assert!(data.is_enabled);
        assert_eq!(data.excluded_commands, vec!["npm run test:*"]);
        assert!(data.allowed_hosts.contains(&"example.com".to_string()));
        assert!(data.allow_unix_sockets.contains(&"/tmp/sock".to_string()));
    }
}
