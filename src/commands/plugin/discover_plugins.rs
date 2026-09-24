//! Maps to: CC commands/plugin/DiscoverPlugins.tsx.
use super::plugin_details_helpers::{
    InstallablePlugin, build_plugin_details_menu_options, extract_git_hub_repo,
};
use super::plugin_options_flow::{PluginOptionsFlow, find_plugin_options_target};
use super::plugin_settings::{ViewState as ParentViewState, use_plugin_ui_state};
use super::plugin_trust_warning::PluginTrustWarning;
use super::use_pagination::{UsePaginationOptions, use_pagination};
use crate::components::configurable_shortcut_hint::ConfigurableShortcutHint;
use crate::components::design_system::byline::Byline;
use crate::components::search_box::SearchBox;
use crate::keybindings::{
    types::ContextName,
    use_keybinding::{KeybindingHandlers, use_keybinding, use_keybindings},
};
use crate::types::plugin::LoadedPlugin;
use crate::utils::plugins::{
    cache_utils::clear_all_caches,
    install_counts::{InstallCountInput, InstallCounts, format_install_count, get_install_counts},
    installed_plugins_manager::is_plugin_globally_installed,
    marketplace_helpers::*,
    marketplace_manager::load_known_marketplaces_config,
    plugin_installation_helpers::{
        InstallPluginParams, InstallPluginResult, install_plugin_from_marketplace,
    },
    plugin_policy::is_plugin_blocked_by_policy,
    schemas::PluginScope,
};
use iocraft::prelude::*;
use serde_json::Value;
use std::collections::HashSet;

/// Maps to: CC DiscoverPlugins.tsx:50-59#Props.
#[derive(Default, Props)]
pub struct DiscoverPluginsProps {
    pub error: Option<String>,
    pub set_error: Handler<Option<String>>,
    pub result: Option<String>,
    pub set_result: Handler<Option<String>>,
    pub set_view_state: Handler<ParentViewState>,
    pub on_install_complete: Handler<()>,
    pub on_search_mode_change: Handler<bool>,
    pub target_plugin: Option<String>,
}
/// Maps to: CC DiscoverPlugins.tsx:61-64#ViewState.
#[derive(Clone)]
enum ViewState {
    PluginList,
    PluginDetails,
    PluginOptions {
        plugin: LoadedPlugin,
        plugin_id: String,
    },
}

/// Maps to: CC DiscoverPlugins.tsx:66-832#DiscoverPlugins.
#[component]
pub fn DiscoverPlugins(
    props: &mut DiscoverPluginsProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    #[cfg(test)]
    let test_imports = hooks
        .try_use_context::<super::plugin_details_helpers::PluginUiTestImports>()
        .map(|f| f.clone());
    let mut view_state = use_plugin_ui_state(&mut hooks, || ViewState::PluginList);
    let mut selected_plugin = use_plugin_ui_state(&mut hooks, || None::<InstallablePlugin>);
    let mut available_plugins = use_plugin_ui_state(&mut hooks, Vec::<InstallablePlugin>::new);
    let mut loading = use_plugin_ui_state(&mut hooks, || true);
    let mut install_counts = use_plugin_ui_state(&mut hooks, || None::<InstallCounts>);
    let mut is_search_mode = use_plugin_ui_state(&mut hooks, || false);
    let on_search_mode_change = props.on_search_mode_change.clone();
    let set_is_search_mode = Handler::from(move |active| {
        is_search_mode.set(active);
        on_search_mode_change(active);
    });
    let mut search = crate::hooks::use_search_input::use_search_input(&mut hooks, "");
    let mut selected_index = use_plugin_ui_state(&mut hooks, || 0usize);
    let mut selected_for_install = use_plugin_ui_state(&mut hooks, HashSet::<String>::new);
    let mut installing_plugins = use_plugin_ui_state(&mut hooks, HashSet::<String>::new);
    let mut details_menu_index = use_plugin_ui_state(&mut hooks, || 0usize);
    let mut is_installing = use_plugin_ui_state(&mut hooks, || false);
    let mut install_error = use_plugin_ui_state(&mut hooks, || None::<String>);
    let mut warning = use_plugin_ui_state(&mut hooks, || None::<String>);
    let mut empty_reason = use_plugin_ui_state(&mut hooks, || None::<EmptyMarketplaceReason>);
    let view = view_state.read().clone();
    let details_active = matches!(view, ViewState::PluginDetails);
    let is_terminal_focused = hooks.use_terminal_focus();
    let query = search.text();
    let lower_query = query.to_lowercase();
    let filtered_plugins = available_plugins
        .read()
        .iter()
        .filter(|p| {
            query.is_empty()
                || p.entry["name"]
                    .as_str()
                    .unwrap_or("")
                    .to_lowercase()
                    .contains(&lower_query)
                || p.entry["description"]
                    .as_str()
                    .unwrap_or("")
                    .to_lowercase()
                    .contains(&lower_query)
                || p.marketplace_name.to_lowercase().contains(&lower_query)
        })
        .cloned()
        .collect::<Vec<_>>();
    let pagination = use_pagination(
        &mut hooks,
        UsePaginationOptions {
            total_items: filtered_plugins.len(),
            selected_index: Some(selected_index.get()),
            ..Default::default()
        },
    );
    hooks.use_effect(move || selected_index.set(0), query.clone());
    let target_plugin = props.target_plugin.clone();
    let set_error = props.set_error.clone();
    let error_identity = (&*set_error as *const dyn Fn(Option<String>)) as *const () as usize;
    #[cfg(test)]
    let load_test_imports = test_imports.clone();
    hooks.use_effect(move ||{
        crate::utils::process_runtime::runtime_handle_for_detached_work().expect("plugin runtime").spawn(async move {
            // Maps to source's loadAllPlugins closure; no unmount cancellation.
            let answer: anyhow::Result<()>=async {
                #[cfg(test)] let config=if let Some(f)=&load_test_imports{f.config()}else{load_known_marketplaces_config().await?};
                #[cfg(not(test))] let config=load_known_marketplaces_config().await?;
                #[cfg(test)] let loaded=if let Some(f)=&load_test_imports{f.marketplaces()}else{load_marketplaces_with_graceful_degradation(&config).await};
                #[cfg(not(test))] let loaded=load_marketplaces_with_graceful_degradation(&config).await;
                let mut all_plugins=Vec::new();
                for marketplace in &loaded.marketplaces {
                    if let Some(data)=&marketplace.data {
                        for entry in data["plugins"].as_array().into_iter().flatten() {
                            let plugin_id=create_plugin_id(entry["name"].as_str().unwrap_or(""),&marketplace.name);
                            all_plugins.push(InstallablePlugin{entry:entry.clone(),marketplace_name:marketplace.name.clone(),is_installed:is_plugin_globally_installed(&plugin_id),plugin_id});
                        }
                    }
                }
                let mut uninstalled=all_plugins.iter().filter(|p|!p.is_installed&&!is_plugin_blocked_by_policy(&p.plugin_id)).cloned().collect::<Vec<_>>();
                #[cfg(test)] let counts=if load_test_imports.is_some(){None}else{get_install_counts().await};
                #[cfg(not(test))] let counts=get_install_counts().await;
                let sort_failed=std::cell::Cell::new(false);
                // NaN reaches `partial_cmp().unwrap_or(Equal)` when to_number
                // fails mid-sort (the Cell mimics the source's try/catch), so
                // the comparator is not a total order; sorted through the
                // non-validating JS-sort primitive (utils/js_sort.rs; PR #7).
                crate::utils::js_sort::sort_by(&mut uninstalled,|a,b| {
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
                    uninstalled.sort_by(|a,b|crate::tools::grep_tool::javascript_locale_compare(a.entry["name"].as_str().unwrap_or(""),b.entry["name"].as_str().unwrap_or("")));
                }
                install_counts.set(counts);
                let empty=uninstalled.is_empty();
                available_plugins.set(uninstalled);
                if empty {
                    #[cfg(test)] let reason=if load_test_imports.is_some(){EmptyMarketplaceReason::NoMarketplacesConfigured}else{detect_empty_marketplace_reason(config.len(),loaded.failures.len()).await};
                    #[cfg(not(test))] let reason=detect_empty_marketplace_reason(config.len(),loaded.failures.len()).await;
                    empty_reason.set(Some(reason));
                }
                if let Some(problem)=format_marketplace_loading_errors(&loaded.failures,loaded.marketplaces.iter().filter(|m|m.data.is_some()).count()) {
                    match problem.r#type { MarketplaceLoadingErrorType::Warning=>warning.set(Some(format!("{}. Showing available plugins.",problem.message))),MarketplaceLoadingErrorType::Error=>anyhow::bail!(problem.message) }
                }
                if let Some(target)=target_plugin.as_deref().filter(|s|!s.is_empty()) {
                    match all_plugins.into_iter().find(|p|p.entry["name"].as_str()==Some(target)) {
                        Some(plugin) if plugin.is_installed=>set_error(Some(format!("Plugin '{}' is already installed. Use '/plugin' to manage existing plugins.",plugin.plugin_id))),
                        Some(plugin)=>{selected_plugin.set(Some(plugin));view_state.set(ViewState::PluginDetails);}
                        None=>set_error(Some(format!("Plugin \"{target}\" not found in any marketplace"))),
                    }
                }
                Ok(())
            }.await;
            if let Err(error)=answer {set_error(Some(error.to_string()));}
            loading.set(false);
        });
    },(error_identity,props.target_plugin.clone()));
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
    let runtime = hooks
        .try_use_context::<crate::keybindings::keybinding_context::KeybindingRuntime>()
        .map(|r| r.clone());
    let search_toggle = set_is_search_mode.clone();
    // Maps to DiscoverPlugins.tsx useInput's single input-string branch.
    // Ink supplies pasted strings through the same callback as key text.
    let search_entry_toggle = search_toggle.clone();
    let mut enter_search = move |input: &str, modifiers: KeyModifiers| {
        static WHITESPACE: std::sync::LazyLock<regress::Regex> =
            std::sync::LazyLock::new(|| regress::Regex::new(r"^\s+$").expect("source regex"));
        if modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
            return;
        }
        if input == "/" {
            search_entry_toggle(true);
            search.clear();
        } else if !input.is_empty()
            && WHITESPACE.find(input).is_none()
            && !matches!(input, "j" | "k" | "i")
        {
            search_entry_toggle(true);
            search.set(input.to_owned());
        }
    };
    // The native backend splits a text burst into keys and drains each hook's
    // queue before the next hook. Handle that burst here before command hooks,
    // reading the retained source state between keys, so entering search takes
    // effect for the rest of the same burst. Ink receives the text as one input.
    // Known input-carrier limit: a burst starting with a cancel key ("nf")
    // cannot be distinguished from a separate "n" key after native tokenization.
    let input_runtime = runtime.clone();
    hooks.use_propagated_terminal_events(move |event| {
        if !matches!(view_state.get(), ViewState::PluginList)
            || loading.get()
            || event.is_propagation_stopped()
        {
            return;
        }
        match event.event() {
            TerminalEvent::Key(key) if key.kind != KeyEventKind::Release => {
                if is_search_mode.get() {
                    let mut on_exit = || search_toggle(false);
                    if search.handle_key_down(
                        &key.code,
                        &key.modifiers,
                        crate::hooks::use_search_input::SearchInputOptions {
                            is_active: true,
                            on_exit: &mut on_exit,
                            on_cancel: None,
                            on_exit_up: None,
                            passthrough_ctrl_keys: &[],
                            backspace_exits_on_empty: true,
                        },
                    ) {
                        event.stop_propagation();
                    }
                } else if let KeyCode::Char(c) = key.code {
                    // Preserve source's active Confirmation shortcut before its
                    // anonymous type-to-search callback. Resolve live user
                    // bindings rather than hard-coding the default n key.
                    let resolution = input_runtime.as_ref().and_then(|runtime| {
                        // Pending chords belong to the existing setup interceptor.
                        // Do not reinterpret a pending chord as search text.
                        if runtime.chord_pending() {
                            return None;
                        }
                        let keystroke = crate::keybindings::matcher::key_event_to_keystroke(key)?;
                        let mut contexts = runtime.active_contexts();
                        contexts.insert(ContextName::Confirmation);
                        contexts.insert(ContextName::Global);
                        Some(crate::keybindings::resolver::resolve_key_with_chord_state(
                            Some(&keystroke),
                            false,
                            &contexts,
                            runtime.bindings().as_slice(),
                            None,
                        ))
                    });
                    if input_runtime
                        .as_ref()
                        .is_some_and(|runtime| runtime.chord_pending())
                    {
                        return;
                    }
                    match resolution {
                        Some(crate::keybindings::types::ChordResolveResult::Unbound) => {
                            event.stop_propagation()
                        }
                        Some(crate::keybindings::types::ChordResolveResult::Match { action })
                            if action == "confirm:no" => {}
                        Some(crate::keybindings::types::ChordResolveResult::ChordStarted {
                            ..
                        }) => {}
                        _ => enter_search(&c.to_string(), key.modifiers),
                    }
                }
            }
            TerminalEvent::Paste(text) => {
                if is_search_mode.get() {
                    search.reset_key_state(&KeyCode::Char(' '), &KeyModifiers::empty());
                    search.insert_text(text);
                } else {
                    enter_search(text, KeyModifiers::empty());
                }
            }
            _ => {}
        }
    });
    use_keybinding(
        &mut hooks,
        runtime.clone(),
        "confirm:no",
        ContextName::Confirmation,
        move || details_active,
        move || {
            view_state.set(ViewState::PluginList);
            selected_plugin.set(None);
            true
        },
    );
    let set_parent = props.set_view_state.clone();
    let search_active = is_search_mode.get();
    use_keybinding(
        &mut hooks,
        runtime.clone(),
        "confirm:no",
        ContextName::Confirmation,
        move || matches!(view_state.get(), ViewState::PluginList) && !is_search_mode.get(),
        move || {
            set_parent(ParentViewState::Menu);
            true
        },
    );
    let filtered = filtered_plugins.clone();
    let toggle_search = set_is_search_mode.clone();
    let set_parent = props.set_view_state.clone();
    let install = install_selected_plugins.clone();
    let handlers: KeybindingHandlers = vec![
        (
            "select:previous".into(),
            Box::new(move || {
                if selected_index.get() == 0 {
                    toggle_search(true);
                } else {
                    selected_index.set(selected_index.get() - 1);
                }
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
        move || matches!(view_state.get(), ViewState::PluginList) && !is_search_mode.get(),
    );
    let filtered = filtered_plugins.clone();
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
        move || matches!(view_state.get(), ViewState::PluginList) && !is_search_mode.get(),
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
    let (terminal_width, _) = hooks.use_terminal_size();
    let theme = crate::utils::theme::current();
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
    if details_active {
        if let Some(plugin) = selected_plugin.read().clone() {
            let options = build_plugin_details_menu_options(
                plugin.entry["homepage"].as_str(),
                extract_git_hub_repo(&plugin),
            );
            return element!{View(flex_direction:FlexDirection::Column){
            View(margin_bottom:1u32){Text(content:"Plugin details",weight:Weight::Bold)}
            View(flex_direction:FlexDirection::Column,margin_bottom:1u32){
                Text(content:plugin.entry["name"].as_str().unwrap_or("").to_string(),weight:Weight::Bold)
                Text(content:format!("from {}",plugin.marketplace_name),dim:true)
                #(plugin.entry["version"].as_str().filter(|s|!s.is_empty()).map(|s|element!{Text(content:format!("Version: {s}"),dim:true)}))
                #(plugin.entry["description"].as_str().filter(|s|!s.is_empty()).map(|s|element!{View(margin_top:1u32){Text(content:s.to_string())}}))
                #(plugin.entry.get("author").filter(|v|!v.is_null()).map(|a|element!{View(margin_top:1u32){Text(content:format!("By: {}",a.as_str().or_else(||a["name"].as_str()).unwrap_or("")),dim:true)}}))
            }
            PluginTrustWarning
            #(install_error.read().as_ref().filter(|s|!s.is_empty()).map(|s|element!{View(margin_bottom:1u32){Text(content:format!("Error: {s}"),color:theme.error)}}))
            View(flex_direction:FlexDirection::Column){#(options.iter().enumerate().map(|(i,o)|element!{View{Text(content:if details_menu_index.get()==i{"> "}else{"  "}) Text(content:if is_installing.get()&&o.action.starts_with("install-"){ "Installing…" }else{o.label},weight:if details_menu_index.get()==i{Weight::Bold}else{Weight::Normal})}}))}
            View(margin_top:1u32){Byline{ConfigurableShortcutHint(action:"select:accept".to_string(),context:"Select".to_string(),fallback:"Enter".to_string(),description:"select".to_string(),dim:true) ConfigurableShortcutHint(action:"confirm:no".to_string(),context:"Confirmation".to_string(),fallback:"Esc".to_string(),description:"back".to_string(),dim:true)}}
        }}.into_any();
        }
    }
    if available_plugins.read().is_empty() {
        return element!{View(flex_direction:FlexDirection::Column){View(margin_bottom:1u32){Text(content:"Discover plugins",weight:Weight::Bold)} EmptyStateMessage(reason:empty_reason.get()) View(margin_top:1u32){Text(content:"Esc to go back",dim:true,italic:true)}}}.into_any();
    }
    let visible = pagination.get_visible_items(&filtered_plugins);
    element!{View(flex_direction:FlexDirection::Column){
        View{Text(content:"Discover plugins",weight:Weight::Bold) #(pagination.needs_pagination.then(||element!{Text(content:format!(" ({}/{})",pagination.scroll_position.current,pagination.scroll_position.total),dim:true)}))}
        View(margin_bottom:1u32,width:terminal_width.saturating_sub(4),flex_direction:FlexDirection::Column){SearchBox(query:query.clone(),is_focused:search_active,is_terminal_focused:is_terminal_focused,cursor_offset:Some(search.offset()))}
        #(warning.read().as_ref().map(|w|element!{View(margin_bottom:1u32){Text(content:format!("{} {w}",figures.warning),color:theme.warning)}}))
        #((filtered_plugins.is_empty()&&!query.is_empty()).then(||element!{View(margin_bottom:1u32){Text(content:format!("No plugins match \"{query}\""),dim:true)}}))
        #(pagination.scroll_position.can_scroll_up.then(||element!{Text(content:format!(" {} more above",figures.arrow_up),dim:true)}))
        #(visible.iter().enumerate().map(|(i,p)|{
            let selected=selected_index.get()==pagination.to_actual_index(i)&&!search_active;
            let marker=if installing_plugins.read().contains(&p.plugin_id){figures.ellipsis}else if selected_for_install.read().contains(&p.plugin_id){figures.radio_on}else{figures.radio_off};
            element!{View(key:format!("{}-{}",pagination.start_index,p.plugin_id),flex_direction:FlexDirection::Column,margin_bottom:if i+1==visible.len(){0u32}else{1u32}){
                View{Text(content:format!("{} ",if selected{figures.pointer}else{" "}),color:if selected{Some(theme.suggestion)}else{None}) Text(content:format!("{marker} {}",p.entry["name"].as_str().unwrap_or(""))) Text(content:format!(" · {}",p.marketplace_name),dim:true)
                    #(p.entry["tags"].as_array().is_some_and(|tags|tags.iter().any(|v|v=="community-managed")).then(||element!{Text(content:" [Community Managed]",dim:true)}))
                    #(install_counts.read().as_ref().filter(|_|p.marketplace_name==crate::utils::plugins::official_marketplace::OFFICIAL_MARKETPLACE_NAME).map(|counts|element!{Text(content:format!(" · {} installs",format_install_count(counts.get(&p.plugin_id).filter(|v|!v.is_null()).map_or(InstallCountInput::Number(0.0),InstallCountInput::Value))),dim:true)}))
                }
                #(p.entry["description"].as_str().filter(|s|!s.is_empty()).map(|description|element!{View(margin_left:4u32){Text(content:crate::utils::truncate::truncate_to_width(description,60),dim:true)}}))
            }}
        }))
        #(pagination.scroll_position.can_scroll_down.then(||element!{Text(content:format!(" {} more below",figures.arrow_down),dim:true)}))
        DiscoverPluginsKeyHint(has_selection:!selected_for_install.read().is_empty(),can_toggle:filtered_plugins.get(selected_index.get()).is_some_and(|p|!p.is_installed))
    }}.into_any()
}

#[derive(Default, Props)]
struct DiscoverPluginsKeyHintProps {
    has_selection: bool,
    can_toggle: bool,
}
/// Maps to: CC DiscoverPlugins.tsx:834-878#DiscoverPluginsKeyHint.
#[component]
fn DiscoverPluginsKeyHint(props: &DiscoverPluginsKeyHintProps) -> impl Into<AnyElement<'static>> {
    element! {View(margin_top:1u32){Byline{
        #(props.has_selection.then(||element!{ConfigurableShortcutHint(action:"plugin:install".to_string(),context:"Plugin".to_string(),fallback:"i".to_string(),description:"install".to_string(),bold:true,dim:true,italic:true)}))
        Text(content:"type to search",dim:true,italic:true)
        #(props.can_toggle.then(||element!{ConfigurableShortcutHint(action:"plugin:toggle".to_string(),context:"Plugin".to_string(),fallback:"Space".to_string(),description:"toggle".to_string(),dim:true,italic:true)}))
        ConfigurableShortcutHint(action:"select:accept".to_string(),context:"Select".to_string(),fallback:"Enter".to_string(),description:"details".to_string(),dim:true,italic:true)
        ConfigurableShortcutHint(action:"confirm:no".to_string(),context:"Confirmation".to_string(),fallback:"Esc".to_string(),description:"back".to_string(),dim:true,italic:true)
    }}}
}
#[derive(Default, Props)]
struct EmptyStateMessageProps {
    reason: Option<EmptyMarketplaceReason>,
}
/// Maps to: CC DiscoverPlugins.tsx:883-942#EmptyStateMessage.
#[component]
fn EmptyStateMessage(props: &EmptyStateMessageProps) -> impl Into<AnyElement<'static>> {
    let lines = match props.reason {
        Some(EmptyMarketplaceReason::GitNotInstalled) => [
            "Git is required to install marketplaces.",
            "Please install git and restart Claude Code.",
        ],
        Some(EmptyMarketplaceReason::AllBlockedByPolicy) => [
            "Your organization policy does not allow any external marketplaces.",
            "Contact your administrator.",
        ],
        Some(EmptyMarketplaceReason::PolicyRestrictsSources) => [
            "Your organization restricts which marketplaces can be added.",
            "Switch to the Marketplaces tab to view allowed sources.",
        ],
        Some(EmptyMarketplaceReason::AllMarketplacesFailed) => [
            "Failed to load marketplace data.",
            "Check your network connection.",
        ],
        Some(EmptyMarketplaceReason::AllPluginsInstalled) => [
            "All available plugins are already installed.",
            "Check for new plugins later or add more marketplaces.",
        ],
        _ => [
            "No plugins available.",
            "Add a marketplace first using the Marketplaces tab.",
        ],
    };
    element! {Fragment{#(lines.into_iter().map(|line|element!{Text(content:line,dim:true)}))}}
}

#[cfg(test)]
pub(super) mod tests {
    use super::super::plugin_details_helpers::PluginUiTestImports;
    use super::*;
    use futures::StreamExt;
    use serde_json::json;
    use std::time::Duration;

    #[derive(Default, Props)]
    pub(crate) struct DiscoverKeybindingTestRootProps {
        pub(crate) bindings: Vec<crate::keybindings::types::ParsedBinding>,
        pub(crate) children: Vec<AnyElement<'static>>,
    }
    #[component]
    pub(crate) fn DiscoverKeybindingTestRoot<'a>(
        props: &'a mut DiscoverKeybindingTestRootProps,
        mut hooks: Hooks,
    ) -> impl Into<AnyElement<'a>> {
        let runtime = crate::keybindings::keybinding_provider_setup::use_keybinding_setup(
            &mut hooks,
            crate::keybindings::keybinding_context::KeybindingRuntime::new(props.bindings.clone()),
        );
        let children = props.children.iter_mut().map(|child| -> AnyElement<'a> {
            let borrowed: AnyElement<'static> = AnyElement::from(child);
            borrowed
        });
        element! {ContextProvider(value:Context::owned(runtime)) {
        #(children)}}
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn discover_text_burst_cancel_and_paste_match_official_bun() {
        crate::utils::process_runtime::initialize_test_process_runtime();
        for (mode, input_text, cancelled, searched) in [
            ("default", "n", true, false),
            ("custom", "q", true, false),
            ("custom", "n", false, true),
            ("custom", "frontendq", false, true),
            ("unbound", "n", false, false),
            ("unbound", "x", false, false),
        ] {
            let fixture = PluginUiTestImports {
                plugins: vec![json!({"name":"Alpha","source":"./alpha"})],
                ..Default::default()
            };
            let calls = fixture.events.clone();
            let mut bindings = crate::keybindings::default_bindings::default_bindings();
            if mode == "custom" {
                bindings.retain(|binding| {
                    !(binding.context == ContextName::Confirmation
                        && binding.action.as_deref() == Some("confirm:no"))
                });
                bindings.push(crate::keybindings::types::ParsedBinding {
                    chord: crate::keybindings::parser::parse_chord("q"),
                    action: Some("confirm:no".into()),
                    context: ContextName::Confirmation,
                });
            }
            if mode == "unbound" {
                for key in ["n", "x"] {
                    bindings.push(crate::keybindings::types::ParsedBinding {
                        chord: crate::keybindings::parser::parse_chord(key),
                        action: None,
                        context: ContextName::Confirmation,
                    });
                }
            }
            let store = crate::state::store::AppStore::new(
                crate::state::app_state_store::AppState::default(),
                None,
            );
            let mut app = element! {ContextProvider(value:Context::owned(*crate::utils::theme::current())) {
                ContextProvider(value:Context::owned(fixture.clone())) {ContextProvider(value:Context::owned(store)) {
                    DiscoverKeybindingTestRoot(bindings:bindings) {FocusScope(handle_keys:false) {
                        DiscoverPlugins(set_view_state:move |view| {assert!(matches!(view, ParentViewState::Menu));calls.lock().unwrap().push(json!(["parent","menu"]));})
                    }}
                }}
            }};
            let (keys, input) = async_channel::unbounded();
            let mut frames = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(input).with_size(120, 30),
            ));
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    if frames.next().await.unwrap().to_string().contains("Alpha") {
                        break;
                    }
                }
            })
            .await
            .unwrap();
            for c in input_text.chars() {
                keys.send(TerminalEvent::Key(KeyEvent::new(
                    KeyEventKind::Press,
                    KeyCode::Char(c),
                )))
                .await
                .unwrap();
            }
            keys.send(TerminalEvent::Resize(119, 30)).await.unwrap();
            let mut history = Vec::new();
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    let frame = frames.next().await.unwrap();
                    let text = frame.to_string();
                    let ready = frame.width() == 119
                        && if cancelled {
                            !fixture.events.lock().unwrap().is_empty()
                        } else if searched {
                            text.contains(&format!("⌕ {input_text}"))
                        } else {
                            text.contains("⌕ Search…") && text.contains("Alpha")
                        };
                    history.push(text);
                    if ready {
                        break;
                    }
                }
            })
            .await
            .unwrap_or_else(|_| panic!("input={input_text},mode={mode},frames={history:?}"));
            assert_eq!(
                !fixture.events.lock().unwrap().is_empty(),
                cancelled,
                "{history:?}"
            );
            if mode == "custom" && input_text == "frontendq" {
                use crate::utils::cursor::kill_ring::{clear_kill_ring_for_tests, get_last_kill};
                clear_kill_ring_for_tests();
                let modified = |character, modifiers| {
                    let mut key = KeyEvent::new(KeyEventKind::Press, KeyCode::Char(character));
                    key.modifiers = modifiers;
                    TerminalEvent::Key(key)
                };
                // Source handleKeyDown resets kill accumulation before paste.
                // A second kill must replace, rather than append to, the first.
                for event in [
                    modified('a', KeyModifiers::CONTROL),
                    modified('k', KeyModifiers::CONTROL),
                    TerminalEvent::Paste("beta".into()),
                    modified('u', KeyModifiers::CONTROL),
                    TerminalEvent::Resize(118, 30),
                ] {
                    keys.send(event).await.unwrap();
                }
                tokio::time::timeout(Duration::from_secs(3), async {
                    loop {
                        let frame = frames.next().await.unwrap();
                        if frame.width() == 118 && frame.to_string().contains("⌕ Search…") {
                            break;
                        }
                    }
                })
                .await
                .unwrap();
                assert_eq!(get_last_kill(), "beta");
                // Paste after a yank ends yank-pop eligibility, too.
                for event in [
                    modified('y', KeyModifiers::CONTROL),
                    TerminalEvent::Paste("Z".into()),
                    modified('y', KeyModifiers::ALT),
                    TerminalEvent::Resize(117, 30),
                ] {
                    keys.send(event).await.unwrap();
                }
                tokio::time::timeout(Duration::from_secs(3), async {
                    loop {
                        let frame = frames.next().await.unwrap();
                        if frame.width() == 117 && frame.to_string().contains("⌕ betaZ") {
                            break;
                        }
                    }
                })
                .await
                .unwrap();
                assert!(fixture.events.lock().unwrap().is_empty());
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn mounted_discover_paste_entry_and_width_match_official_bun() {
        crate::utils::process_runtime::initialize_test_process_runtime();
        let oracle: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/oracles/plugin-ui-complete-0914/discover-entry-oracle.json"
        ))
        .unwrap();
        for case in oracle.as_array().unwrap() {
            for pasted in [false, true] {
                let input_text = case["input"].as_str().unwrap();
                if !pasted && input_text.chars().count() != 1 {
                    continue;
                }
                let fixture = PluginUiTestImports {
                    plugins: vec![json!({"name":"Alpha","source":"./alpha"})],
                    ..Default::default()
                };
                let changes = fixture.events.clone();
                let store = crate::state::store::AppStore::new(
                    crate::state::app_state_store::AppState::default(),
                    None,
                );
                let mut app = element! {ContextProvider(value:Context::owned(*crate::utils::theme::current())) {
                    ContextProvider(value:Context::owned(fixture.clone())) {ContextProvider(value:Context::owned(store)) {
                        ContextProvider(value:Context::owned(crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings())) {
                            FocusScope(handle_keys:false) {DiscoverPlugins(on_search_mode_change:move |active| changes.lock().unwrap().push(json!(["search",active])))}
                        }
                    }}
                }};
                let (keys, input) = async_channel::unbounded();
                let mut frames = Box::pin(app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(input).with_size(120, 30),
                ));
                let initial = tokio::time::timeout(Duration::from_secs(3), async {
                    loop {
                        let frame = frames.next().await.unwrap().to_string();
                        if frame.contains("Alpha") {
                            break frame;
                        }
                    }
                })
                .await
                .unwrap();
                let border_width = |frame: &str| {
                    frame
                        .lines()
                        .find(|line| line.contains('╭'))
                        .unwrap()
                        .trim()
                        .chars()
                        .count()
                };
                assert_eq!(border_width(&initial), 116, "{initial}");
                let event = if pasted {
                    TerminalEvent::Paste(input_text.to_owned())
                } else {
                    TerminalEvent::Key(KeyEvent::new(
                        KeyEventKind::Press,
                        KeyCode::Char(input_text.chars().next().unwrap()),
                    ))
                };
                keys.send(event).await.unwrap();
                keys.send(TerminalEvent::Resize(119, 30)).await.unwrap();
                let mut after = tokio::time::timeout(Duration::from_secs(3), async {
                    loop {
                        let frame = frames.next().await.unwrap().to_string();
                        if border_width(&frame) == 115 {
                            break frame;
                        }
                    }
                })
                .await
                .unwrap();
                // The oracle records the settled end state. The resize and the
                // key are independent changes that need not land in the same
                // frame (a same-frame settled resize can precede the key's
                // frame), so drain until the loop goes quiet before asserting.
                while let Ok(Some(frame)) =
                    tokio::time::timeout(Duration::from_millis(300), frames.next()).await
                {
                    after = frame.to_string();
                }
                let mut expected_events = case["events"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|event| event[0] == "search")
                    .cloned()
                    .collect::<Vec<_>>();
                // Source Select's k binding moves up from the first row into
                // search. A pasted k only reaches the raw input callback.
                if !pasted && input_text == "k" {
                    expected_events.push(json!(["search", true]));
                }
                assert_eq!(
                    *fixture.events.lock().unwrap(),
                    expected_events,
                    "input={input_text:?}, paste={pasted}: {after}"
                );
                if input_text == "/" {
                    assert!(
                        after.contains("Search…") && after.contains("Alpha"),
                        "{after}"
                    );
                }
                if input_text == "z" || input_text == "jk" {
                    assert!(
                        after.contains(&format!("No plugins match \"{input_text}\"")),
                        "{after}"
                    );
                }
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn mounted_discover_plugins_matches_official_bun_frames_and_install_callbacks() {
        crate::utils::process_runtime::initialize_test_process_runtime();
        let oracle: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/oracles/plugin-ui-complete-0914/panel-oracle.json"
        ))
        .unwrap();
        for name in [
            "discover-list",
            "discover-target",
            "discover-empty",
            "discover-search",
        ] {
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
            let callback_events = fixture.events.clone();
            let search_changed = Handler::from(move |active: bool| {
                callback_events
                    .lock()
                    .unwrap()
                    .push(json!(["search", active]))
            });
            let mut app = element! {
                ContextProvider(value:Context::owned(*crate::utils::theme::current())) {
                ContextProvider(value:Context::owned(fixture.clone())) {
                    ContextProvider(value:Context::owned(store)) {
                        ContextProvider(value:Context::owned(runtime)) {
                            FocusScope(handle_keys:false) {
                                DiscoverPlugins(target_plugin:target,set_result:result,on_install_complete:complete,set_view_state:parent,set_error:error,on_search_mode_change:search_changed)
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
                    if !frame.trim().is_empty() && !frame.contains("Loading…") {
                        break frame;
                    }
                }
            })
            .await
            .unwrap();
            let expected_initial = case["initial"].as_str().unwrap();
            let fragments: Vec<&str> = match name {
                "discover-list" | "discover-search" => vec![
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
            if name == "discover-search" {
                for (step, key) in [KeyCode::Char('z'), KeyCode::Esc, KeyCode::Esc]
                    .into_iter()
                    .enumerate()
                {
                    keys.send(TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, key)))
                        .await
                        .unwrap();
                    let frame = tokio::time::timeout(Duration::from_secs(4), async {
                        loop {
                            let frame = frames.next().await.unwrap().to_string();
                            let ready = match step {
                                0 => frame.contains("Zulu") && !frame.contains("Alpha"),
                                1 => {
                                    frame.contains("Alpha")
                                        && fixture.events.lock().unwrap().len() == 1
                                }
                                _ => fixture.events.lock().unwrap().len() == 2,
                            };
                            if ready {
                                break frame;
                            }
                        }
                    })
                    .await
                    .unwrap();
                    assert!(frame.contains("Zulu"));
                    if step > 0 {
                        assert!(frame.contains("Alpha"));
                    }
                }
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
