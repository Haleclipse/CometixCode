//! Maps to: CC commands/plugin/pluginDetailsHelpers.tsx.
use crate::components::configurable_shortcut_hint::ConfigurableShortcutHint;
use crate::components::design_system::byline::Byline;
use iocraft::prelude::*;
use serde_json::Value;

/// Maps to: CC pluginDetailsHelpers.tsx:17-22#InstallablePlugin.
#[derive(Clone, Debug, PartialEq)]
pub struct InstallablePlugin {
    pub entry: Value,
    pub marketplace_name: String,
    pub plugin_id: String,
    pub is_installed: bool,
}
/// Maps to: CC pluginDetailsHelpers.tsx:27-30#PluginDetailsMenuOption.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginDetailsMenuOption {
    pub label: &'static str,
    pub action: &'static str,
}
/// Maps to: CC pluginDetailsHelpers.tsx:35-52#extractGitHubRepo.
pub fn extract_git_hub_repo(plugin: &InstallablePlugin) -> Option<&str> {
    let source = plugin.entry.get("source")?.as_object()?;
    if source.get("source").and_then(Value::as_str) == Some("github") {
        source.get("repo").and_then(Value::as_str)
    } else {
        None
    }
}
/// Maps to: CC pluginDetailsHelpers.tsx:57-80#buildPluginDetailsMenuOptions.
pub fn build_plugin_details_menu_options(
    has_homepage: Option<&str>,
    github_repo: Option<&str>,
) -> Vec<PluginDetailsMenuOption> {
    let mut options = vec![
        PluginDetailsMenuOption {
            label: "Install for you (user scope)",
            action: "install-user",
        },
        PluginDetailsMenuOption {
            label: "Install for all collaborators on this repository (project scope)",
            action: "install-project",
        },
        PluginDetailsMenuOption {
            label: "Install for you, in this repo only (local scope)",
            action: "install-local",
        },
    ];
    if has_homepage.is_some_and(|s| !s.is_empty()) {
        options.push(PluginDetailsMenuOption {
            label: "Open homepage",
            action: "homepage",
        });
    }
    if github_repo.is_some_and(|s| !s.is_empty()) {
        options.push(PluginDetailsMenuOption {
            label: "View on GitHub",
            action: "github",
        });
    }
    options.push(PluginDetailsMenuOption {
        label: "Back to plugin list",
        action: "back",
    });
    options
}
#[derive(Default, Props)]
pub struct PluginSelectionKeyHintProps {
    pub has_selection: bool,
}
/// Maps to: CC pluginDetailsHelpers.tsx:85-123#PluginSelectionKeyHint.
#[component]
pub fn PluginSelectionKeyHint(
    props: &PluginSelectionKeyHintProps,
) -> impl Into<AnyElement<'static>> {
    element! { View(margin_top:1u32) { Byline {
        #(props.has_selection.then(|| element! { ConfigurableShortcutHint(action:"plugin:install".to_string(),context:"Plugin".to_string(),fallback:"i".to_string(),description:"install".to_string(),bold:true,dim:true,italic:true) }))
        ConfigurableShortcutHint(action:"plugin:toggle".to_string(),context:"Plugin".to_string(),fallback:"Space".to_string(),description:"toggle".to_string(),dim:true,italic:true)
        ConfigurableShortcutHint(action:"select:accept".to_string(),context:"Select".to_string(),fallback:"Enter".to_string(),description:"details".to_string(),dim:true,italic:true)
        ConfigurableShortcutHint(action:"confirm:no".to_string(),context:"Confirmation".to_string(),fallback:"Esc".to_string(),description:"back".to_string(),dim:true,italic:true)
    } } }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn details_menus_and_repo_matches_official_bun() {
        // Actual source oracle: research/proof/plugin-ui-complete-0914/helpers-oracle.json.
        let oracle: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/oracles/plugin-ui-complete-0914/helpers-oracle.json"
        ))
        .unwrap();
        for (index, (homepage, repo)) in [
            (None, None),
            (Some(""), None),
            (Some("https://example.test"), Some("org/repo")),
        ]
        .into_iter()
        .enumerate()
        {
            let actual = build_plugin_details_menu_options(homepage, repo)
                .into_iter()
                .map(|o| serde_json::json!({"label":o.label,"action":o.action}))
                .collect::<Vec<_>>();
            assert_eq!(serde_json::json!(actual), oracle["menus"][index]);
        }
        for (index, source) in [
            serde_json::json!("./local"),
            serde_json::json!({"source":"github","repo":"org/repo"}),
            serde_json::json!({"source":"url","url":"https://github.com/org/repo"}),
        ]
        .into_iter()
        .enumerate()
        {
            let plugin = InstallablePlugin {
                entry: serde_json::json!({"source":source}),
                marketplace_name: String::new(),
                plugin_id: String::new(),
                is_installed: false,
            };
            assert_eq!(
                serde_json::json!(extract_git_hub_repo(&plugin)),
                oracle["repos"][index]
            );
        }
    }
}

/// Test-only imported service boundary, matching panel-oracle.ts module mocks.
/// The actual components, states, event handlers and rendering remain mounted.
#[cfg(test)]
#[derive(Clone, Default)]
pub(crate) struct PluginUiTestImports {
    pub plugins: Vec<Value>,
    pub events: std::sync::Arc<std::sync::Mutex<Vec<Value>>>,
}
#[cfg(test)]
impl PluginUiTestImports {
    pub fn config(&self) -> crate::utils::plugins::marketplace_manager::KnownMarketplacesConfig {
        if self.plugins.is_empty() {
            Default::default()
        } else {
            serde_json::json!({"market":{"source":{"source":"github","repo":"example/market"}}})
                .as_object()
                .unwrap()
                .clone()
        }
    }
    pub fn marketplaces(
        &self,
    ) -> crate::utils::plugins::marketplace_helpers::LoadedMarketplacesWithGracefulDegradation {
        use crate::utils::plugins::marketplace_helpers::*;
        LoadedMarketplacesWithGracefulDegradation {
            marketplaces: self
                .config()
                .into_iter()
                .map(|(name, config)| LoadedMarketplace {
                    name,
                    config,
                    data: Some(std::sync::Arc::new(
                        serde_json::json!({"plugins":self.plugins}),
                    )),
                })
                .collect(),
            failures: vec![],
        }
    }
    pub fn install(
        &self,
        plugin: &InstallablePlugin,
        scope: crate::utils::plugins::schemas::PluginScope,
    ) -> crate::utils::plugins::plugin_installation_helpers::InstallPluginResult {
        self.events.lock().unwrap().push(serde_json::json!(["install",{"pluginId":plugin.plugin_id,"entry":plugin.entry,"marketplaceName":plugin.marketplace_name,"scope":scope}]));
        crate::utils::plugins::plugin_installation_helpers::InstallPluginResult::Success {
            message: "Installed fixture".into(),
        }
    }
}
