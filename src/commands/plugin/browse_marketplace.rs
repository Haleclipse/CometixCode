//! Maps to: CC commands/plugin/BrowseMarketplace.tsx.
use super::plugin_details_helpers::{
    InstallablePlugin, PluginSelectionKeyHint, build_plugin_details_menu_options,
    extract_git_hub_repo,
};
use super::plugin_options_flow::{PluginOptionsFlow, find_plugin_options_target};
use super::plugin_settings::{ViewState as ParentViewState, use_plugin_ui_state};
use super::plugin_trust_warning::PluginTrustWarning;
use super::use_pagination::{UsePaginationOptions, use_pagination};
use crate::components::configurable_shortcut_hint::ConfigurableShortcutHint;
use crate::components::design_system::byline::Byline;
use iocraft::prelude::*;
use serde_json::Value;
use std::collections::HashSet;

use crate::keybindings::{
    types::ContextName,
    use_keybinding::{KeybindingHandlers, use_keybinding, use_keybindings},
};
use crate::types::plugin::LoadedPlugin;
use crate::utils::plugins::{
    cache_utils::clear_all_caches,
    install_counts::{InstallCountInput, InstallCounts, format_install_count, get_install_counts},
    installed_plugins_manager::{is_plugin_globally_installed, is_plugin_installed},
    marketplace_helpers::*,
    marketplace_manager::{get_marketplace, load_known_marketplaces_config},
    plugin_installation_helpers::{
        InstallPluginParams, InstallPluginResult, install_plugin_from_marketplace,
    },
    plugin_policy::is_plugin_blocked_by_policy,
    schemas::PluginScope,
};

/// Maps to: CC BrowseMarketplace.tsx:51-60#Props.
#[derive(Default, Props)]
pub struct BrowseMarketplaceProps {
    pub error: Option<String>,
    pub set_error: Handler<Option<String>>,
    pub result: Option<String>,
    pub set_result: Handler<Option<String>>,
    pub set_view_state: Handler<ParentViewState>,
    pub on_install_complete: Handler<()>,
    pub target_marketplace: Option<String>,
    pub target_plugin: Option<String>,
}
/// Maps to: CC BrowseMarketplace.tsx:62-66#ViewState.
#[derive(Clone)]
enum ViewState {
    MarketplaceList,
    PluginList,
    PluginDetails,
    PluginOptions {
        plugin: LoadedPlugin,
        plugin_id: String,
    },
}
/// Maps to: CC BrowseMarketplace.tsx:68-73#MarketplaceInfo.
#[derive(Clone)]
struct MarketplaceInfo {
    name: String,
    total_plugins: usize,
    installed_count: usize,
    source: String,
}
// Rust lifecycle carrier for the source loadPluginsForMarketplace effect's
// cancelled flag. Drop is the source effect cleanup on component unmount.
#[derive(Default)]
struct MarketplaceLoadCancellation(Option<std::sync::Arc<std::sync::atomic::AtomicBool>>);
impl Drop for MarketplaceLoadCancellation {
    fn drop(&mut self) {
        if let Some(flag) = &self.0 {
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }
}
/// Maps to: CC BrowseMarketplace.tsx:75-1031#BrowseMarketplace.
#[component]
pub fn BrowseMarketplace(
    props: &mut BrowseMarketplaceProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    #[cfg(test)]
    let test_imports = hooks
        .try_use_context::<super::plugin_details_helpers::PluginUiTestImports>()
        .map(|f| f.clone());
    let mut view_state = use_plugin_ui_state(&mut hooks, || ViewState::MarketplaceList);
    let mut selected_marketplace = use_plugin_ui_state(&mut hooks, || None::<String>);
    let mut selected_plugin = use_plugin_ui_state(&mut hooks, || None::<InstallablePlugin>);
    let mut marketplaces = use_plugin_ui_state(&mut hooks, Vec::<MarketplaceInfo>::new);
    let mut available_plugins = use_plugin_ui_state(&mut hooks, Vec::<InstallablePlugin>::new);
    let mut loading = use_plugin_ui_state(&mut hooks, || true);
    let mut install_counts = use_plugin_ui_state(&mut hooks, || None::<InstallCounts>);
    let mut selected_index = use_plugin_ui_state(&mut hooks, || 0usize);
    let mut selected_for_install = use_plugin_ui_state(&mut hooks, HashSet::<String>::new);
    let mut installing_plugins = use_plugin_ui_state(&mut hooks, HashSet::<String>::new);
    let mut details_menu_index = use_plugin_ui_state(&mut hooks, || 0usize);
    let mut is_installing = use_plugin_ui_state(&mut hooks, || false);
    let mut install_error = use_plugin_ui_state(&mut hooks, || None::<String>);
    let mut warning = use_plugin_ui_state(&mut hooks, || None::<String>);
    let view = view_state.read().clone();
    let list_active = matches!(view, ViewState::PluginList);
    let details_active = matches!(view, ViewState::PluginDetails);
    let marketplace_active = matches!(view, ViewState::MarketplaceList);
    let pagination = use_pagination(
        &mut hooks,
        UsePaginationOptions {
            total_items: available_plugins.read().len(),
            selected_index: Some(selected_index.get()),
            ..Default::default()
        },
    );
    let runtime = hooks
        .try_use_context::<crate::keybindings::keybinding_context::KeybindingRuntime>()
        .map(|r| r.clone());
    let target_marketplace = props.target_marketplace.clone();
    let set_parent = props.set_view_state.clone();
    use_keybinding(
        &mut hooks,
        runtime.clone(),
        "confirm:no",
        ContextName::Confirmation,
        || true,
        move || {
            if list_active {
                if let Some(target) = target_marketplace.clone().filter(|s| !s.is_empty()) {
                    set_parent(ParentViewState::ManageMarketplaces {
                        target_marketplace: Some(target),
                        action: None,
                    });
                } else if marketplaces.read().len() == 1 {
                    set_parent(ParentViewState::Menu);
                } else {
                    view_state.set(ViewState::MarketplaceList);
                    selected_marketplace.set(None);
                    selected_for_install.set(HashSet::new());
                }
            } else if details_active {
                view_state.set(ViewState::PluginList);
                selected_plugin.set(None);
            } else {
                set_parent(ParentViewState::Menu);
            }
            true
        },
    );
    let set_error = props.set_error.clone();
    let target_marketplace = props.target_marketplace.clone();
    let target_plugin = props.target_plugin.clone();
    let error_identity = (&*set_error as *const dyn Fn(Option<String>)) as *const () as usize;
    #[cfg(test)]
    let load_test_imports = test_imports.clone();
    hooks.use_effect(move ||{
        crate::utils::process_runtime::runtime_handle_for_detached_work().expect("plugin runtime").spawn(async move {
            let answer:anyhow::Result<()>=async {
                #[cfg(test)] let config=if let Some(f)=&load_test_imports{f.config()}else{load_known_marketplaces_config().await?};
                #[cfg(not(test))] let config=load_known_marketplaces_config().await?;
                #[cfg(test)] let loaded=if let Some(f)=&load_test_imports{f.marketplaces()}else{load_marketplaces_with_graceful_degradation(&config).await};
                #[cfg(not(test))] let loaded=load_marketplaces_with_graceful_degradation(&config).await;
                let mut infos=Vec::new();
                for marketplace in &loaded.marketplaces {if let Some(data)=&marketplace.data{
                    let entries=data["plugins"].as_array().cloned().unwrap_or_default();
                    let installed_count=entries.iter().filter(|entry|is_plugin_installed(&create_plugin_id(entry["name"].as_str().unwrap_or(""),&marketplace.name))).count();
                    infos.push(MarketplaceInfo{name:marketplace.name.clone(),total_plugins:entries.len(),installed_count,source:get_marketplace_source_display(&marketplace.config["source"])});
                }}
                // First-match pinning is not antisymmetric (not a total order); kept verbatim per source, sorted through the non-validating JS-sort primitive (utils/js_sort.rs; mirror PR #7).
                crate::utils::js_sort::sort_by(&mut infos,|a,b|{if a.name=="claude-plugin-directory"{std::cmp::Ordering::Less}else if b.name=="claude-plugin-directory"{std::cmp::Ordering::Greater}else{std::cmp::Ordering::Equal}});
                marketplaces.set(infos.clone());
                if let Some(problem)=format_marketplace_loading_errors(&loaded.failures,loaded.marketplaces.iter().filter(|m|m.data.is_some()).count()){
                    match problem.r#type{MarketplaceLoadingErrorType::Warning=>warning.set(Some(format!("{}. Showing available marketplaces.",problem.message))),MarketplaceLoadingErrorType::Error=>anyhow::bail!(problem.message)}
                }
                let target=target_plugin.as_deref().filter(|s|!s.is_empty());
                let market=target_marketplace.as_deref().filter(|s|!s.is_empty());
                if infos.len()==1&&target.is_none()&&market.is_none(){selected_marketplace.set(Some(infos[0].name.clone()));view_state.set(ViewState::PluginList);}
                if let Some(target)=target {
                    let mut found=None;
                    // Source intentionally searches every configured marketplace even with targetMarketplace.
                    for (name,_) in crate::utils::process_env::ecmascript_object_entries(&config) {
                        #[cfg(test)] let data=if let Some(f)=&load_test_imports{std::sync::Arc::new(serde_json::json!({"plugins":f.plugins}))}else{get_marketplace(name).await?};
                        #[cfg(not(test))] let data=get_marketplace(name).await?;
                        if let Some(entry)=data["plugins"].as_array().into_iter().flatten().find(|p|p["name"].as_str()==Some(target)) {
                            let plugin_id=create_plugin_id(target,name);
                            found=Some(InstallablePlugin{entry:entry.clone(),marketplace_name:name.to_string(),is_installed:is_plugin_globally_installed(&plugin_id),plugin_id});break;
                        }
                    }
                    if let Some(plugin)=found {
                        if is_plugin_globally_installed(&plugin.plugin_id){set_error(Some(format!("Plugin '{}' is already installed globally. Use '/plugin' to manage existing plugins.",plugin.plugin_id)));}
                        else{selected_marketplace.set(Some(plugin.marketplace_name.clone()));selected_plugin.set(Some(plugin));view_state.set(ViewState::PluginDetails);}
                    }else{set_error(Some(format!("Plugin \"{target}\" not found in any marketplace")));}
                }else if let Some(target)=market {
                    if infos.iter().any(|m|m.name==target){selected_marketplace.set(Some(target.to_string()));view_state.set(ViewState::PluginList);}
                    else{set_error(Some(format!("Marketplace \"{target}\" not found")));}
                }
                Ok(())
            }.await;
            if let Err(error)=answer{set_error(Some(error.to_string()));}
            loading.set(false);
        });
    },(error_identity,props.target_marketplace.clone(),props.target_plugin.clone()));
    let selected = selected_marketplace.read().clone();
    let set_error = props.set_error.clone();
    // L1 effect cleanup carrier: drop marks the source closure's cancelled flag;
    // the detached future still reaches its finally setLoading(false).
    let mut cancellation = hooks.use_ref(MarketplaceLoadCancellation::default);
    #[cfg(test)]
    let selected_test_imports = test_imports.clone();
    hooks.use_effect(
        move || {
            if let Some(old) = cancellation.write().0.take() {
                old.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            let Some(name) = selected.filter(|s| !s.is_empty()) else {
                return;
            };
            let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            cancellation.write().0 = Some(cancelled.clone());
            crate::utils::process_runtime::runtime_handle_for_detached_work()
                .expect("plugin runtime")
                .spawn(async move {
                    loading.set(true);
                    let answer: anyhow::Result<()> = async {
                        #[cfg(test)] let data=if let Some(f)=&selected_test_imports{std::sync::Arc::new(serde_json::json!({"plugins":f.plugins}))}else{get_marketplace(&name).await?};
                        #[cfg(not(test))] let data = get_marketplace(&name).await?;
                        if cancelled.load(std::sync::atomic::Ordering::SeqCst) {
                            return Ok(());
                        }
                        let mut plugins = Vec::new();
                        for entry in data["plugins"].as_array().into_iter().flatten() {
                            let plugin_id =
                                create_plugin_id(entry["name"].as_str().unwrap_or(""), &name);
                            if is_plugin_blocked_by_policy(&plugin_id) {
                                continue;
                            }
                            plugins.push(InstallablePlugin {
                                entry: entry.clone(),
                                marketplace_name: name.clone(),
                                is_installed: is_plugin_globally_installed(&plugin_id),
                                plugin_id,
                            });
                        }
                        #[cfg(test)] let counts=if selected_test_imports.is_some(){None}else{get_install_counts().await};
                        #[cfg(not(test))] let counts = get_install_counts().await;
                        if cancelled.load(std::sync::atomic::Ordering::SeqCst) {
                            return Ok(());
                        }
                        let sort_failed=std::cell::Cell::new(false);
                // NaN reaches `partial_cmp().unwrap_or(Equal)` when to_number
                // fails mid-sort (the Cell mimics the source's try/catch), so
                // the comparator is not a total order; sorted through the
                // non-validating JS-sort primitive (utils/js_sort.rs; PR #7).
                crate::utils::js_sort::sort_by(&mut plugins,|a,b| {
                    let a_raw=counts.as_ref().and_then(|c|c.get(&a.plugin_id)).filter(|v|!v.is_null());
                    let b_raw=counts.as_ref().and_then(|c|c.get(&b.plugin_id)).filter(|v|!v.is_null());
                    let equal=match (a_raw,b_raw){
                        (None,None)=>true,
                        (None,Some(v))|(Some(v),None)=>v.kind==11&&v.number==Some(0.0),
                        (Some(a),Some(b)) if a.kind==b.kind=>match a.kind{8|9=>true,10=>a.string_units==b.string_units,11=>a.number.zip(b.number).is_some_and(|(a,b)|a==b),_=>std::ptr::eq(a,b)},
                        _=>false,
                    };
                    if !equal {
                        let a_number=a_raw.map_or(0.0,|v|v.to_number().unwrap_or_else(|_|{sort_failed.set(true);f64::NAN}));
                        let b_number=b_raw.map_or(0.0,|v|v.to_number().unwrap_or_else(|_|{sort_failed.set(true);f64::NAN}));
                        (b_number-a_number).partial_cmp(&0.0).unwrap_or(std::cmp::Ordering::Equal)
                    } else {crate::tools::grep_tool::javascript_locale_compare(a.entry["name"].as_str().unwrap_or(""),b.entry["name"].as_str().unwrap_or(""))}
                });
                if sort_failed.get(){
                    crate::utils::debug::log_for_debugging("Failed to fetch install counts: Cannot convert object to primitive value");
                    plugins.sort_by(|a,b|crate::tools::grep_tool::javascript_locale_compare(a.entry["name"].as_str().unwrap_or(""),b.entry["name"].as_str().unwrap_or("")));
                }
                        install_counts.set(counts);
                        available_plugins.set(plugins);
                        selected_index.set(0);
                        selected_for_install.set(HashSet::new());
                        Ok(())
                    }
                    .await;
                    if let Err(error) = answer {
                        if !cancelled.load(std::sync::atomic::Ordering::SeqCst) {
                            set_error(Some(error.to_string()));
                        }
                    }
                    loading.set(false);
                });
        },
        (selected_marketplace.read().clone(), error_identity),
    );
    let set_result = props.set_result.clone();
    let error = props.error.clone();
    let result_identity = (&*set_result as *const dyn Fn(Option<String>)) as *const () as usize;
    hooks.use_effect(
        move || {
            if let Some(error) = error.filter(|s| !s.is_empty()) {
                set_result(Some(error));
            }
        },
        (props.error.clone(), result_identity),
    );

    let set_error = props.set_error.clone();
    let set_result = props.set_result.clone();
    let set_parent = props.set_view_state.clone();
    let complete = props.on_install_complete.clone();
    #[cfg(test)]
    let batch_test_imports = test_imports.clone();
    let install_selected_plugins = Handler::from(move |()| {
        if selected_for_install.read().is_empty() {
            return;
        }
        let plugins = available_plugins
            .read()
            .iter()
            .filter(|p| selected_for_install.read().contains(&p.plugin_id))
            .cloned()
            .collect::<Vec<_>>();
        installing_plugins.set(plugins.iter().map(|p| p.plugin_id.clone()).collect());
        let set_error = set_error.clone();
        let set_result = set_result.clone();
        let set_parent = set_parent.clone();
        let complete = complete.clone();
        #[cfg(test)]
        let batch_test_imports = batch_test_imports.clone();
        crate::utils::process_runtime::runtime_handle_for_detached_work().expect("plugin runtime").spawn(async move {
            let mut success_count=0usize;let mut failed=Vec::new();
            for plugin in plugins {
                let name=plugin.entry["name"].as_str().unwrap_or("").to_string();
                #[cfg(test)] let fixture_result=batch_test_imports.as_ref().map(|f|f.install(&plugin,PluginScope::User));
                #[cfg(test)] let outcome=if let Some(result)=fixture_result{result}else{install_plugin_from_marketplace(InstallPluginParams{plugin_id:plugin.plugin_id,entry:plugin.entry,marketplace_name:plugin.marketplace_name,scope:Some(PluginScope::User),trigger:None}).await};
                #[cfg(not(test))] let outcome=install_plugin_from_marketplace(InstallPluginParams{plugin_id:plugin.plugin_id,entry:plugin.entry,marketplace_name:plugin.marketplace_name,scope:Some(PluginScope::User),trigger:None}).await;
                match outcome {
                    InstallPluginResult::Success{..}=>success_count+=1,
                    InstallPluginResult::Failure{error}=>failed.push(PluginFailureDetail{name,reason:Some(error),error:None}),
                }
            }
            installing_plugins.set(HashSet::new());selected_for_install.set(HashSet::new());#[cfg(test)] if let Some(f)=&batch_test_imports{f.events.lock().unwrap().push(serde_json::json!(["clear"]));}else{clear_all_caches();}
            #[cfg(not(test))] clear_all_caches();
            if failed.is_empty(){set_result(Some(format!("✓ Installed {success_count} {}. Run /reload-plugins to activate.",if success_count==1{"plugin"}else{"plugins"})));}
            else if success_count==0{set_error(Some(format!("Failed to install: {}",format_failure_details(&failed,true))));}
            else{set_result(Some(format!("✓ Installed {success_count} of {} plugins. Failed: {}. Run /reload-plugins to activate successfully installed plugins.",success_count+failed.len(),format_failure_details(&failed,false))));}
            if success_count>0{complete(());}
            set_parent(ParentViewState::Menu);
        });
    });
    let set_result = props.set_result.clone();
    let set_parent = props.set_view_state.clone();
    let complete = props.on_install_complete.clone();
    #[cfg(test)]
    let single_test_imports = test_imports.clone();
    let handle_single_plugin_install =
        Handler::from(move |(plugin, scope): (InstallablePlugin, PluginScope)| {
            #[cfg(test)]
            let single_test_imports = single_test_imports.clone();
            is_installing.set(true);
            install_error.set(None);
            let set_result = set_result.clone();
            let set_parent = set_parent.clone();
            let complete = complete.clone();
            crate::utils::process_runtime::runtime_handle_for_detached_work()
                .expect("plugin runtime")
                .spawn(async move {
                    #[cfg(test)]
                    let fixture_result = single_test_imports
                        .as_ref()
                        .map(|f| f.install(&plugin, scope));
                    #[cfg(test)]
                    let outcome = if let Some(result) = fixture_result {
                        result
                    } else {
                        install_plugin_from_marketplace(InstallPluginParams {
                            plugin_id: plugin.plugin_id.clone(),
                            entry: plugin.entry,
                            marketplace_name: plugin.marketplace_name,
                            scope: Some(scope),
                            trigger: None,
                        })
                        .await
                    };
                    #[cfg(not(test))]
                    let outcome = install_plugin_from_marketplace(InstallPluginParams {
                        plugin_id: plugin.plugin_id.clone(),
                        entry: plugin.entry,
                        marketplace_name: plugin.marketplace_name,
                        scope: Some(scope),
                        trigger: None,
                    })
                    .await;
                    match outcome {
                        InstallPluginResult::Success { message } => {
                            // Source leaves a rejected options lookup uncaught; do not silently close.
                            #[cfg(test)]
                            let options = if single_test_imports.is_some() {
                                Ok(None)
                            } else {
                                find_plugin_options_target(&plugin.plugin_id).await
                            };
                            #[cfg(not(test))]
                            let options = find_plugin_options_target(&plugin.plugin_id).await;
                            match options {
                                Ok(Some(loaded)) => {
                                    is_installing.set(false);
                                    view_state.set(ViewState::PluginOptions {
                                        plugin: loaded,
                                        plugin_id: plugin.plugin_id,
                                    });
                                    return;
                                }
                                Ok(None) => {}
                                Err(error) => {
                                    crate::utils::debug::log_for_debugging(&format!("{error}"));
                                    return;
                                }
                            }
                            set_result(Some(message));
                            complete(());
                            set_parent(ParentViewState::Menu);
                        }
                        InstallPluginResult::Failure { error } => {
                            is_installing.set(false);
                            install_error.set(Some(error));
                        }
                    }
                });
        });
    let infos = marketplaces.read().clone();
    use_keybindings(
        &mut hooks,
        runtime.clone(),
        vec![
            (
                "select:previous".into(),
                Box::new(move || {
                    selected_index.set(selected_index.get().saturating_sub(1));
                    true
                }),
            ),
            (
                "select:next".into(),
                Box::new({
                    let len = infos.len();
                    move || {
                        if selected_index.get() + 1 < len {
                            selected_index.set(selected_index.get() + 1);
                        }
                        true
                    }
                }),
            ),
            (
                "select:accept".into(),
                Box::new(move || {
                    if let Some(info) = infos.get(selected_index.get()) {
                        selected_marketplace.set(Some(info.name.clone()));
                        view_state.set(ViewState::PluginList);
                    }
                    true
                }),
            ),
        ],
        ContextName::Select,
        move || marketplace_active,
    );
    let filtered = available_plugins.read().clone();
    let set_parent = props.set_view_state.clone();
    let install = install_selected_plugins.clone();
    let handlers: KeybindingHandlers = vec![
        (
            "select:previous".into(),
            Box::new(move || {
                selected_index.set(selected_index.get().saturating_sub(1));
                true
            }),
        ),
        (
            "select:next".into(),
            Box::new({
                let len = filtered.len();
                move || {
                    if selected_index.get() + 1 < len {
                        selected_index.set(selected_index.get() + 1);
                    }
                    true
                }
            }),
        ),
        (
            "select:accept".into(),
            Box::new(move || {
                if selected_index.get() == filtered.len() && !selected_for_install.read().is_empty()
                {
                    install(());
                } else if let Some(plugin) = filtered.get(selected_index.get()) {
                    if plugin.is_installed {
                        set_parent(ParentViewState::ManagePlugins {
                            target_plugin: plugin.entry["name"].as_str().map(str::to_string),
                            target_marketplace: Some(plugin.marketplace_name.clone()),
                            action: None,
                        });
                    } else {
                        selected_plugin.set(Some(plugin.clone()));
                        view_state.set(ViewState::PluginDetails);
                        details_menu_index.set(0);
                        install_error.set(None);
                    }
                }
                true
            }),
        ),
    ];
    use_keybindings(
        &mut hooks,
        runtime.clone(),
        handlers,
        ContextName::Select,
        move || list_active,
    );
    let filtered = available_plugins.read().clone();
    let install = install_selected_plugins.clone();
    use_keybindings(
        &mut hooks,
        runtime.clone(),
        vec![
            (
                "plugin:toggle".into(),
                Box::new(move || {
                    if let Some(plugin) = filtered
                        .get(selected_index.get())
                        .filter(|p| !p.is_installed)
                    {
                        let mut selection = selected_for_install.read().clone();
                        if !selection.remove(&plugin.plugin_id) {
                            selection.insert(plugin.plugin_id.clone());
                        }
                        selected_for_install.set(selection);
                    }
                    true
                }),
            ),
            (
                "plugin:install".into(),
                Box::new(move || {
                    if !selected_for_install.read().is_empty() {
                        install(());
                    }
                    true
                }),
            ),
        ],
        ContextName::Plugin,
        move || list_active,
    );
    let plugin = selected_plugin.read().clone();
    let options = plugin
        .as_ref()
        .map(|p| {
            build_plugin_details_menu_options(p.entry["homepage"].as_str(), extract_git_hub_repo(p))
        })
        .unwrap_or_default();
    let menu_len = options.len();
    let install = handle_single_plugin_install.clone();
    use_keybindings(
        &mut hooks,
        runtime,
        vec![
            (
                "select:previous".into(),
                Box::new(move || {
                    details_menu_index.set(details_menu_index.get().saturating_sub(1));
                    true
                }),
            ),
            (
                "select:next".into(),
                Box::new(move || {
                    if details_menu_index.get() + 1 < menu_len {
                        details_menu_index.set(details_menu_index.get() + 1);
                    }
                    true
                }),
            ),
            (
                "select:accept".into(),
                Box::new(move || {
                    if let Some(plugin) = plugin.clone() {
                        let action = options.get(details_menu_index.get()).map(|o| o.action);
                        match action {
                            Some("install-user") => install((plugin, PluginScope::User)),
                            Some("install-project") => install((plugin, PluginScope::Project)),
                            Some("install-local") => install((plugin, PluginScope::Local)),
                            Some("back") => {
                                view_state.set(ViewState::PluginList);
                                selected_plugin.set(None);
                            }
                            Some("homepage") | Some("github") => {
                                let url = if action == Some("homepage") {
                                    plugin.entry["homepage"].as_str().map(str::to_string)
                                } else {
                                    extract_git_hub_repo(&plugin)
                                        .map(|r| format!("https://github.com/{r}"))
                                };
                                if let Some(url) = url {
                                    crate::utils::process_runtime::runtime_handle_for_detached_work().expect("plugin runtime").spawn(async move{let _=crate::utils::browser::open_browser(&url).await;});
                                }
                            }
                            _ => {}
                        }
                    }
                    true
                }),
            ),
        ],
        ContextName::Select,
        move || details_active,
    );
    let theme = *hooks.use_context::<crate::utils::theme::Theme>();
    let figures = crate::constants::figures::figures();
    if let ViewState::PluginOptions { plugin, plugin_id } = view {
        let name = plugin.name.clone();
        let set_result = props.set_result.clone();
        let complete = props.on_install_complete.clone();
        let set_parent = props.set_view_state.clone();
        return element!{PluginOptionsFlow(plugin:plugin,plugin_id:plugin_id,on_done:Handler::from(move |(outcome,detail):(String,Option<String>)| {
            let message=match outcome.as_str(){"configured"=>format!("✓ Installed and configured {name}. Run /reload-plugins to apply."),"skipped"=>format!("✓ Installed {name}. Run /reload-plugins to apply."),_=>format!("Installed but failed to save config: {}",detail.unwrap_or_else(||"undefined".into()))};
            set_result(Some(message));complete(());set_parent(ParentViewState::Menu);
        }))}.into_any();
    }
    if loading.get() {
        return element! {Text(content:"Loading…")}.into_any();
    }
    if let Some(error) = props.error.as_ref().filter(|s| !s.is_empty()) {
        return element! {Text(content:error.clone(),color:theme.error)}.into_any();
    }
    if marketplace_active {
        let infos = marketplaces.read();
        return element!{View(flex_direction:FlexDirection::Column){
            View(margin_bottom:1u32){Text(content:"Select marketplace",weight:Weight::Bold)}
            #(infos.is_empty().then(||element!{Fragment{Text(content:"No marketplaces configured.") Text(content:"Add a marketplace first using 'Add marketplace'.",dim:true)}}))
            #(warning.read().as_ref().map(|w|element!{View(margin_bottom:1u32){Text(content:format!("{} {w}",figures.warning),color:theme.warning)}}))
            #(infos.iter().enumerate().map(|(i,m)|element!{View(key:m.name.clone(),flex_direction:FlexDirection::Column,margin_bottom:if i+1<infos.len(){1u32}else{0u32}){
                Text(content:format!("{} {}",if selected_index.get()==i{figures.pointer}else{" "},m.name),color:if selected_index.get()==i{Some(theme.suggestion)}else{None})
                View(margin_left:2u32){Text(content:format!("{} {} available{}{}",m.total_plugins,if m.total_plugins==1{"plugin"}else{"plugins"},if m.installed_count>0{format!(" · {} already installed",m.installed_count)}else{String::new()},if !m.source.is_empty(){format!(" · {}",m.source)}else{String::new()}),dim:true)}
            }}))
            View(margin_top:1u32){Byline{
                #((!infos.is_empty()).then(||element!{ConfigurableShortcutHint(action:"select:accept".to_string(),context:"Select".to_string(),fallback:"Enter".to_string(),description:"select".to_string(),dim:true,italic:true)}))
                ConfigurableShortcutHint(action:"confirm:no".to_string(),context:"Confirmation".to_string(),fallback:"Esc".to_string(),description:"go back".to_string(),dim:true,italic:true)
            }}
        }}.into_any();
    }
    if details_active {
        if let Some(plugin) = selected_plugin.read().clone() {
            let options = build_plugin_details_menu_options(
                plugin.entry["homepage"].as_str(),
                extract_git_hub_repo(&plugin),
            );
            let mut components = Vec::new();
            for (key, label) in [
                ("commands", "Commands"),
                ("agents", "Agents"),
                ("hooks", "Hooks"),
                ("mcpServers", "MCP Servers"),
            ] {
                if let Some(value) = plugin
                    .entry
                    .get(key)
                    .filter(|v| !v.is_null() && *v != &Value::Bool(false) && v.as_str() != Some(""))
                {
                    let detail = if key == "hooks" && value.is_array() {
                        (0..value.as_array().unwrap().len())
                            .map(|i| i.to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    } else if let Some(array) = value.as_array() {
                        array
                            .iter()
                            .map(|v| {
                                v.as_str()
                                    .map(str::to_string)
                                    .unwrap_or_else(|| v.to_string())
                            })
                            .collect::<Vec<_>>()
                            .join(", ")
                    } else if let Some(object) = value.as_object() {
                        crate::utils::process_env::ecmascript_object_entries(object)
                            .into_iter()
                            .map(|(name, _)| name)
                            .collect::<Vec<_>>()
                            .join(", ")
                    } else if key == "mcpServers" {
                        "configured".to_string()
                    } else {
                        value
                            .as_str()
                            .unwrap_or("")
                            .encode_utf16()
                            .enumerate()
                            .map(|(i, _)| i.to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    };
                    components.push(format!("· {label}: {detail}"));
                }
            }
            if components.is_empty() {
                let remote = plugin.entry["source"]["source"]
                    .as_str()
                    .is_some_and(|s| matches!(s, "github" | "url" | "npm" | "pip"));
                components.push(
                    if remote {
                        "· Component summary not available for remote plugin"
                    } else {
                        "· Components will be discovered at installation"
                    }
                    .to_string(),
                );
            }
            return element!{View(flex_direction:FlexDirection::Column){
            View(margin_bottom:1u32){Text(content:"Plugin Details",weight:Weight::Bold)}
            View(flex_direction:FlexDirection::Column,margin_bottom:1u32){
                Text(content:plugin.entry["name"].as_str().unwrap_or("").to_string(),weight:Weight::Bold)
                #(plugin.entry["version"].as_str().filter(|s|!s.is_empty()).map(|s|element!{Text(content:format!("Version: {s}"),dim:true)}))
                #(plugin.entry["description"].as_str().filter(|s|!s.is_empty()).map(|s|element!{View(margin_top:1u32){Text(content:s.to_string())}}))
                #(plugin.entry.get("author").filter(|v|!v.is_null()).map(|a|element!{View(margin_top:1u32){Text(content:format!("By: {}",a.as_str().or_else(||a["name"].as_str()).unwrap_or("")),dim:true)}}))
            }
            View(flex_direction:FlexDirection::Column,margin_bottom:1u32){Text(content:"Will install:",weight:Weight::Bold) #(components.into_iter().map(|line|element!{Text(content:line,dim:true)}))}
            PluginTrustWarning
            #(install_error.read().as_ref().filter(|s|!s.is_empty()).map(|s|element!{View(margin_bottom:1u32){Text(content:format!("Error: {s}"),color:theme.error)}}))
            View(flex_direction:FlexDirection::Column){#(options.iter().enumerate().map(|(i,o)|element!{View{Text(content:if details_menu_index.get()==i{"> "}else{"  "}) Text(content:if is_installing.get()&&o.action=="install"{ "Installing…" }else{o.label},weight:if details_menu_index.get()==i{Weight::Bold}else{Weight::Normal})}}))}
            View(margin_top:1u32,padding_left:1u32){Byline{ConfigurableShortcutHint(action:"select:accept".to_string(),context:"Select".to_string(),fallback:"Enter".to_string(),description:"select".to_string(),dim:true) ConfigurableShortcutHint(action:"confirm:no".to_string(),context:"Confirmation".to_string(),fallback:"Esc".to_string(),description:"back".to_string(),dim:true)}}
        }}.into_any();
        }
    }
    if available_plugins.read().is_empty() {
        return element!{View(flex_direction:FlexDirection::Column){
        View(margin_bottom:1u32){Text(content:"Install plugins",weight:Weight::Bold)}
        Text(content:"No new plugins available to install.",dim:true)
        Text(content:"All plugins from this marketplace are already installed.",dim:true)
        View(margin_left:3u32){ConfigurableShortcutHint(action:"confirm:no".to_string(),context:"Confirmation".to_string(),fallback:"Esc".to_string(),description:"go back".to_string(),dim:true,italic:true)}
    }}.into_any();
    }
    let plugins = available_plugins.read();
    let visible = pagination.get_visible_items(&plugins);
    element!{View(flex_direction:FlexDirection::Column){
        View(margin_bottom:1u32){Text(content:"Install Plugins",weight:Weight::Bold)}
        #(pagination.scroll_position.can_scroll_up.then(||element!{Text(content:format!(" {} more above",figures.arrow_up),dim:true)}))
        #(visible.iter().enumerate().map(|(i,p)|{
            let selected=selected_index.get()==pagination.to_actual_index(i);
            let marker=if p.is_installed{figures.tick}else if installing_plugins.read().contains(&p.plugin_id){figures.ellipsis}else if selected_for_install.read().contains(&p.plugin_id){figures.radio_on}else{figures.radio_off};
            element!{View(key:p.plugin_id.clone(),flex_direction:FlexDirection::Column,margin_bottom:if i+1==visible.len(){0u32}else{1u32}){
                View{Text(content:format!("{} ",if selected{figures.pointer}else{" "}),color:if selected{Some(theme.suggestion)}else{None}) Text(content:format!("{marker} {}",p.entry["name"].as_str().unwrap_or("")),color:if p.is_installed{Some(theme.success)}else{None})
                    #(p.entry["category"].as_str().filter(|s|!s.is_empty()).map(|c|element!{Text(content:format!(" [{c}]"),dim:true)}))
                    #(p.entry["tags"].as_array().is_some_and(|tags|tags.iter().any(|v|v=="community-managed")).then(||element!{Text(content:" [Community Managed]",dim:true)}))
                    #(p.is_installed.then(||element!{Text(content:" (installed)",dim:true)}))
                    #(install_counts.read().as_ref().filter(|_|selected_marketplace.read().as_deref()==Some(crate::utils::plugins::official_marketplace::OFFICIAL_MARKETPLACE_NAME)).map(|counts|element!{Text(content:format!(" · {} installs",format_install_count(counts.get(&p.plugin_id).filter(|v|!v.is_null()).map_or(InstallCountInput::Number(0.0),InstallCountInput::Value))),dim:true)}))
                }
                #(p.entry["description"].as_str().filter(|s|!s.is_empty()).map(|description|element!{View(margin_left:4u32){Text(content:crate::utils::truncate::truncate_to_width(description,60),dim:true) #(p.entry["version"].as_str().filter(|s|!s.is_empty()).map(|v|element!{Text(content:format!(" · v{v}"),dim:true)}))}}))
            }}
        }))
        #(pagination.scroll_position.can_scroll_down.then(||element!{Text(content:format!(" {} more below",figures.arrow_down),dim:true)}))
        PluginSelectionKeyHint(has_selection:!selected_for_install.read().is_empty())
    }}.into_any()
}

#[cfg(test)]
mod tests {
    use super::super::plugin_details_helpers::PluginUiTestImports;
    use super::*;
    use futures::StreamExt;
    use serde_json::json;
    use std::time::Duration;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn mounted_browse_marketplace_matches_official_bun_frames_and_install_callbacks() {
        crate::utils::process_runtime::initialize_test_process_runtime();
        let oracle: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/oracles/plugin-ui-complete-0914/panel-oracle.json"
        ))
        .unwrap();
        for name in ["browse-details"] {
            let case = oracle
                .as_array()
                .unwrap()
                .iter()
                .find(|case| case["name"] == name)
                .unwrap();
            let fixture = PluginUiTestImports {
                plugins: if name == "discover-empty" {
                    vec![]
                } else {
                    vec![
                        json!({"name":"Zulu","source":"./z","description":"last plugin"}),
                        json!({"name":"Alpha","source":{"source":"github","repo":"example/alpha"},"description":"first plugin","homepage":"https://example.test","version":"1.2.3","author":{"name":"Example"}}),
                    ]
                },
                ..Default::default()
            };
            let callback_events = fixture.events.clone();
            let result = Handler::from(move |value: Option<String>| {
                callback_events
                    .lock()
                    .unwrap()
                    .push(json!(["result", value]))
            });
            let callback_events = fixture.events.clone();
            let complete =
                Handler::from(move |()| callback_events.lock().unwrap().push(json!(["complete"])));
            let callback_events = fixture.events.clone();
            let parent = Handler::from(move |value: ParentViewState| {
                assert!(matches!(value, ParentViewState::Menu));
                callback_events
                    .lock()
                    .unwrap()
                    .push(json!(["parent",{"type":"menu"}]));
            });
            let callback_events = fixture.events.clone();
            let error = Handler::from(move |value: Option<String>| {
                callback_events
                    .lock()
                    .unwrap()
                    .push(json!(["error", value]))
            });
            let runtime =
                crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings();
            let store = crate::state::store::AppStore::new(
                crate::state::app_state_store::AppState::default(),
                None,
            );
            let target =
                matches!(name, "discover-target" | "browse-details").then(|| "Alpha".to_string());
            let mut app = element! {
                ContextProvider(value:Context::owned(*crate::utils::theme::current())) {
                ContextProvider(value:Context::owned(fixture.clone())) {
                    ContextProvider(value:Context::owned(store)) {
                        ContextProvider(value:Context::owned(runtime)) {
                            FocusScope(handle_keys:false) {
                                BrowseMarketplace(target_plugin:target,set_result:result,on_install_complete:complete,set_view_state:parent,set_error:error)
                            }
                        }
                    }
                }
                }
            };
            let (keys, events) = async_channel::unbounded();
            let mut frames = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(96, 60),
            ));
            let initial = tokio::time::timeout(Duration::from_secs(4), async {
                loop {
                    let frame = frames.next().await.expect("mounted frame").to_string();
                    // The Bun oracle records output after its 160 ms effect
                    // observation window, not the first non-loading frame.
                    // Wait for this case's target view; keep every oracle
                    // content assertion below, as well as keys and callbacks.
                    let target_ready = match name {
                        "browse-details" => {
                            frame.contains("Plugin Details") && frame.contains("Alpha")
                        }
                        "discover-target" => {
                            frame.contains("Plugin details") && frame.contains("Alpha")
                        }
                        "discover-empty" => frame.contains("No plugins available."),
                        _ => frame.contains("Discover plugins") && frame.contains("Alpha"),
                    };
                    if target_ready && !frame.contains("Loading…") {
                        break frame;
                    }
                }
            })
            .await
            .unwrap();
            let expected_initial = case["initial"].as_str().unwrap();
            let fragments: Vec<&str> = match name {
                "discover-list" => vec![
                    "Discover plugins",
                    "Alpha · market",
                    "first plugin",
                    "Zulu · market",
                    "last plugin",
                    "type to search",
                    "Space to toggle",
                    "Enter to details",
                ],
                "discover-empty" => vec![
                    "Discover plugins",
                    "No plugins available.",
                    "Add a marketplace first using the Marketplaces tab.",
                    "Esc to go back",
                ],
                "browse-details" => vec![
                    "Plugin Details",
                    "Alpha",
                    "Version: 1.2.3",
                    "By: Example",
                    "Will install:",
                    "Component summary not available for remote plugin",
                    "Install for you (user scope)",
                    "Install for all collaborators on this repository (project scope)",
                    "Install for you, in this repo only (local scope)",
                    "Open homepage",
                    "View on GitHub",
                    "Back to plugin list",
                ],
                _ => vec![
                    "Plugin details",
                    "Alpha",
                    "from market",
                    "Version: 1.2.3",
                    "By: Example",
                    "Install for you (user scope)",
                    "Install for all collaborators on this repository (project scope)",
                    "Install for you, in this repo only (local scope)",
                    "Open homepage",
                    "View on GitHub",
                    "Back to plugin list",
                ],
            };
            for fragment in fragments {
                assert!(
                    expected_initial.contains(fragment),
                    "official oracle missing {fragment}"
                );
                assert!(
                    initial.contains(fragment),
                    "{name}: missing {fragment}:\n{initial}"
                );
            }
            if name == "discover-list" {
                assert!(initial.find("Alpha").unwrap() < initial.find("Zulu").unwrap());
                keys.send(TerminalEvent::Key(KeyEvent::new(
                    KeyEventKind::Press,
                    KeyCode::Char(' '),
                )))
                .await
                .unwrap();
                tokio::time::timeout(Duration::from_secs(4), async {
                    loop {
                        if frames
                            .next()
                            .await
                            .unwrap()
                            .to_string()
                            .contains("i to install")
                        {
                            break;
                        }
                    }
                })
                .await
                .unwrap();
                keys.send(TerminalEvent::Key(KeyEvent::new(
                    KeyEventKind::Press,
                    KeyCode::Char('i'),
                )))
                .await
                .unwrap();
            } else if name == "discover-target" {
                keys.send(TerminalEvent::Key(KeyEvent::new(
                    KeyEventKind::Press,
                    KeyCode::Enter,
                )))
                .await
                .unwrap();
            }
            if name == "discover-list" || name == "discover-target" {
                tokio::time::timeout(Duration::from_secs(4),async {
                    loop {
                        if fixture.events.lock().unwrap().len()>=case["events"].as_array().unwrap().len(){break;}
                        tokio::select!{_ = frames.next()=>{},_ = tokio::time::sleep(Duration::from_millis(10))=>{}}
                    }
                }).await.unwrap();
            }
            assert_eq!(
                json!(*fixture.events.lock().unwrap()),
                case["events"],
                "{name}"
            );
        }
    }
}
