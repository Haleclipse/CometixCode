//! Maps to: CC `commands/plugin/ManageMarketplaces.tsx`.
//! React/Ink → iocraft component-local state and callback carriers. A1/A6/A7:
//! source async callbacks and timers run on the process lifetime runtime.
use super::plugin_settings::ViewState;
use crate::components::{
    configurable_shortcut_hint::ConfigurableShortcutHint,
    design_system::{byline::Byline, keyboard_shortcut_hint::KeyboardShortcutHint},
};
use crate::hooks::use_exit::ExitKeyState;
use crate::keybindings::{
    types::ContextName,
    use_keybinding::{use_keybinding, use_keybindings},
};
use crate::types::plugin::LoadedPlugin;
use crate::utils::plugins::{
    marketplace_helpers::*, marketplace_manager::*, plugin_loader::load_all_plugins,
    schemas::is_marketplace_auto_update,
};
use crate::utils::settings::{SettingSource, get_settings_for_source, update_settings_for_source};
use crate::utils::string_utils::plural;
use iocraft::prelude::*;

/// Maps to: CC `commands/plugin/ManageMarketplaces.tsx:44-56#Props`.
#[derive(Default, Props)]
pub struct ManageMarketplacesProps {
    pub set_view_state: Handler<ViewState>,
    pub error: Option<String>,
    pub set_error: Handler<Option<String>>,
    pub set_result: Handler<Option<String>>,
    pub exit_state: Option<ExitKeyState>,
    pub on_manage_complete: Handler<()>,
    pub target_marketplace: Option<String>,
    pub action: Option<String>,
}
/// Maps to: CC `commands/plugin/ManageMarketplaces.tsx:58-67#MarketplaceState`.
#[derive(Clone, Debug, Default)]
struct MarketplaceState {
    name: String,
    source: String,
    last_updated: Option<String>,
    plugin_count: Option<usize>,
    installed_plugins: Vec<LoadedPlugin>,
    pending_update: bool,
    pending_remove: bool,
    auto_update: bool,
}
/// Maps to: CC `commands/plugin/ManageMarketplaces.tsx:69-69#InternalViewState`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum InternalViewState {
    #[default]
    List,
    Details,
    ConfirmRemove,
}

/// Maps to: CC `commands/plugin/ManageMarketplaces.tsx:71-935#ManageMarketplaces`.
#[component]
pub fn ManageMarketplaces(
    props: &mut ManageMarketplacesProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let mut marketplace_states = hooks.use_state(Vec::<MarketplaceState>::new);
    let mut loading = hooks.use_state(|| true);
    let mut selected_index = hooks.use_state(|| 0usize);
    let is_processing = hooks.use_state(|| false);
    let mut process_error = hooks.use_state(|| None::<String>);
    let success_message = hooks.use_state(|| None::<String>);
    let progress_message = hooks.use_state(|| None::<String>);
    let mut internal_view = hooks.use_state(InternalViewState::default);
    let mut selected_marketplace = hooks.use_state(|| None::<MarketplaceState>);
    let mut details_menu_index = hooks.use_state(|| 0usize);
    let mut has_attempted_auto_action = hooks.use_ref(|| false);
    let set_error = props.set_error.clone();
    let set_result = props.set_result.clone();
    let set_view_state = props.set_view_state.clone();
    let on_manage_complete = props.on_manage_complete.clone();
    let rendered_states = marketplace_states.read().clone();
    let rendered_selected = selected_marketplace.read().clone();
    let was_in_details_view = internal_view.get() == InternalViewState::Details;
    // Maps to: CC `commands/plugin/ManageMarketplaces.tsx:212-378#applyChanges`.
    // Keep render-closure snapshots: the auto-action timer retains the closure
    // which scheduled it, rather than observing a later navigation view.
    let apply_changes = Handler::from(move |states: Option<Vec<MarketplaceState>>| {
        let states_to_process = states.unwrap_or_else(|| rendered_states.clone());
        let selected = rendered_selected.clone();
        let set_error = set_error.clone();
        let set_result = set_result.clone();
        let set_view_state = set_view_state.clone();
        let on_manage_complete = on_manage_complete.clone();
        let mut is_processing = is_processing;
        let mut process_error = process_error;
        let mut success_message = success_message;
        let mut progress_message = progress_message;
        let mut marketplace_states = marketplace_states;
        let mut selected_marketplace = selected_marketplace;
        is_processing.set(true);
        process_error.set(None);
        success_message.set(None);
        progress_message.set(None);
        crate::utils::process_runtime::runtime_handle_for_detached_work().expect("marketplace runtime").spawn(async move {
            let result:anyhow::Result<()>=async {
                let settings=get_settings_for_source(SettingSource::User);
                let mut updated_count=0;let mut removed_count=0;
                let mut refreshed_marketplaces=std::collections::HashSet::new();
                for state in states_to_process {
                    if state.pending_remove {
                        if !state.installed_plugins.is_empty() {
                            let mut enabled=settings.as_ref().and_then(|s|s.enabled_plugins.as_ref()).and_then(|v|v.as_object()).cloned().unwrap_or_default();
                            for plugin in &state.installed_plugins {enabled.insert(create_plugin_id(&plugin.name,&state.name),serde_json::Value::Bool(false));}
                            let _=update_settings_for_source(SettingSource::User,&serde_json::Map::from_iter([("enabledPlugins".into(),serde_json::Value::Object(enabled))]));
                        }
                        remove_marketplace_source(&state.name).await?;removed_count+=1;
                        crate::services::analytics::log_event("tengu_marketplace_removed",serde_json::json!({"marketplace_name":state.name,"plugins_uninstalled":state.installed_plugins.len()}));
                        continue;
                    }
                    if state.pending_update {
                        let progress=move|message:&str|{let mut state=progress_message;state.set(Some(message.into()));Ok(())};
                        refresh_marketplace(&state.name,Some(&progress),false).await?;updated_count+=1;
                        refreshed_marketplaces.insert(state.name.to_lowercase());
                        crate::services::analytics::log_event("tengu_marketplace_updated",serde_json::json!({"marketplace_name":state.name}));
                    }
                }
                let updated_plugin_count=if refreshed_marketplaces.is_empty(){0}else{crate::utils::plugins::plugin_autoupdate::update_plugins_for_marketplaces(&refreshed_marketplaces).await.len()};
                crate::utils::plugins::cache_utils::clear_all_caches();on_manage_complete(());
                let config=load_known_marketplaces_config().await?;
                let loaded=load_all_plugins().await?;
                let all_plugins=loaded.enabled.into_iter().chain(loaded.disabled).collect::<Vec<_>>();
                let loaded=load_marketplaces_with_graceful_degradation(&config).await;
                let mut new_states=Vec::new();
                for market in loaded.marketplaces {
                    let installed_plugins=all_plugins.iter().filter(|p|p.source.ends_with(&format!("@{}",market.name))).cloned().collect();
                    new_states.push(MarketplaceState{name:market.name.clone(),source:get_marketplace_source_display(&market.config["source"]),last_updated:market.config["lastUpdated"].as_str().map(str::to_owned),plugin_count:market.data.as_ref().and_then(|d|d["plugins"].as_array()).map(Vec::len),installed_plugins,pending_update:false,pending_remove:false,auto_update:is_marketplace_auto_update(&market.name,&market.config)});
                }
                // CC `ManageMarketplaces.tsx:320-325` verbatim: first-match pinning is not antisymmetric, so the sort goes through the non-validating JS-sort primitive (utils/js_sort.rs; mirror PR #7).
                crate::utils::js_sort::sort_by(&mut new_states,|a,b|{if a.name=="claude-plugin-directory"{std::cmp::Ordering::Less}else if b.name=="claude-plugin-directory"{std::cmp::Ordering::Greater}else{crate::tools::grep_tool::javascript_locale_compare(&a.name,&b.name)}});
                marketplace_states.set(new_states.clone());
                if was_in_details_view {if let Some(selected)=selected {if let Some(updated)=new_states.iter().find(|s|s.name==selected.name){selected_marketplace.set(Some(updated.clone()));}}}
                let mut actions=Vec::new();
                if updated_count>0 {let plugin_part=if updated_plugin_count>0{format!(" ({updated_plugin_count} {} bumped)",plural(updated_plugin_count,"plugin",None))}else{String::new()};actions.push(format!("Updated {updated_count} {}{plugin_part}",plural(updated_count,"marketplace",None)));}
                if removed_count>0 {actions.push(format!("Removed {removed_count} {}",plural(removed_count,"marketplace",None)));}
                if !actions.is_empty(){let message=format!("✔ {}",actions.join(", "));if was_in_details_view{success_message.set(Some(message));}else{set_result(Some(message));crate::utils::process_runtime::runtime_handle_for_detached_work().expect("marketplace timer runtime").spawn(async move{tokio::time::sleep(std::time::Duration::from_millis(2000)).await;set_view_state(ViewState::Menu);});}}
                else if !was_in_details_view {set_view_state(ViewState::Menu);}
                Ok(())
            }.await;
            if let Err(error)=result {let message=error.to_string();process_error.set(Some(message.clone()));set_error(Some(message));}
            is_processing.set(false);progress_message.set(None);
        });
    });
    let auto_apply = apply_changes.clone();
    let set_error = props.set_error.clone();
    let target = props.target_marketplace.clone();
    let action = props.action.clone();
    let error = props.error.clone();
    let dependencies = (target.clone(), action.clone(), error.clone());
    #[cfg(test)]
    let load_test_imports = hooks
        .try_use_context::<super::plugin_details_helpers::PluginUiTestImports>()
        .map(|fixture| fixture.clone());
    // Maps to: CC `commands/plugin/ManageMarketplaces.tsx:98-191#loadMarketplaces`.
    hooks.use_effect(move||{
        crate::utils::process_runtime::runtime_handle_for_detached_work().expect("marketplace runtime").spawn(async move{
            let result:anyhow::Result<()>=async{
                #[cfg(not(test))] let config=load_known_marketplaces_config().await?;
                #[cfg(test)] let config=if let Some(fixture)=&load_test_imports{fixture.config()}else{load_known_marketplaces_config().await?};
                #[cfg(not(test))] let loaded=load_all_plugins().await?;
                #[cfg(test)] let loaded=if load_test_imports.is_some(){crate::types::plugin::PluginLoadResult::default()}else{load_all_plugins().await?};
                let all_plugins=loaded.enabled.into_iter().chain(loaded.disabled).collect::<Vec<_>>();
                #[cfg(not(test))] let loaded=load_marketplaces_with_graceful_degradation(&config).await;
                #[cfg(test)] let loaded=if let Some(fixture)=&load_test_imports{fixture.marketplaces()}else{load_marketplaces_with_graceful_degradation(&config).await};
                let success_count=loaded.marketplaces.iter().filter(|m|m.data.is_some()).count();
                let mut states=Vec::new();
                for market in loaded.marketplaces {
                    let installed_plugins=all_plugins.iter().filter(|p|p.source.ends_with(&format!("@{}",market.name))).cloned().collect();
                    states.push(MarketplaceState{name:market.name.clone(),source:get_marketplace_source_display(&market.config["source"]),last_updated:market.config["lastUpdated"].as_str().map(str::to_owned),plugin_count:market.data.as_ref().and_then(|d|d["plugins"].as_array()).map(Vec::len),installed_plugins,pending_update:false,pending_remove:false,auto_update:is_marketplace_auto_update(&market.name,&market.config)});
                }
                // CC `ManageMarketplaces.tsx:127-132` verbatim: same non-antisymmetric pinning as above, same JS-sort routing.
                crate::utils::js_sort::sort_by(&mut states,|a,b|{if a.name=="claude-plugin-directory"{std::cmp::Ordering::Less}else if b.name=="claude-plugin-directory"{std::cmp::Ordering::Greater}else{crate::tools::grep_tool::javascript_locale_compare(&a.name,&b.name)}});
                marketplace_states.set(states.clone());
                if let Some(error)=format_marketplace_loading_errors(&loaded.failures,success_count){if error.r#type==MarketplaceLoadingErrorType::Warning{process_error.set(Some(error.message));}else{anyhow::bail!(error.message);}}
                if let Some(target)=target.filter(|s|!s.is_empty()) {if !*has_attempted_auto_action.read() && error.as_deref().is_none_or(str::is_empty){
                    *has_attempted_auto_action.write()=true;
                    if let Some(index)=states.iter().position(|s|s.name==target){selected_index.set(index+1);if let Some(action)=action.filter(|s|!s.is_empty()){
                        if action=="update"{states[index].pending_update=true;}else if action=="remove"{states[index].pending_remove=true;}
                        marketplace_states.set(states.clone());
                        crate::utils::process_runtime::runtime_handle_for_detached_work().expect("marketplace timer runtime").spawn(async move{tokio::time::sleep(std::time::Duration::from_millis(100)).await;auto_apply(Some(states));});
                    }else{selected_marketplace.set(Some(states[index].clone()));internal_view.set(InternalViewState::Details);}}
                    else{set_error(Some(format!("Marketplace not found: {target}")));}
                }}
                Ok(())
            }.await;
            if let Err(error)=result{let message=error.to_string();set_error(Some(message.clone()));process_error.set(Some(message));}
            loading.set(false);
        });
    },dependencies);
    // Maps to: CC `commands/plugin/ManageMarketplaces.tsx:198-202#hasPendingChanges`.
    let has_pending_changes = || {
        marketplace_states
            .read()
            .iter()
            .any(|s| s.pending_update || s.pending_remove)
    };
    // Maps to: CC `commands/plugin/ManageMarketplaces.tsx:205-209#getPendingCounts`.
    let get_pending_counts = || {
        let states = marketplace_states.read();
        (
            states.iter().filter(|s| s.pending_update).count(),
            states.iter().filter(|s| s.pending_remove).count(),
        )
    };
    let apply = apply_changes.clone();
    // Maps to: CC `commands/plugin/ManageMarketplaces.tsx:381-392#confirmRemove`.
    let confirm_remove = Handler::from(move |()| {
        let selected = selected_marketplace.read().clone();
        let Some(selected) = selected else { return };
        let mut states = marketplace_states.read().clone();
        for state in &mut states {
            if state.name == selected.name {
                state.pending_remove = true;
            }
        }
        let mut marketplace_states = marketplace_states;
        marketplace_states.set(states.clone());
        apply(Some(states));
    });
    // Native date display boundary for source Date.toLocaleDateString: schema
    // timestamps are RFC3339; chrono converts to the host timezone. The default
    // en-US numeric date representation is retained here; arbitrary ICU locale
    // selection is not supplied by iocraft and remains an explicit native seam.
    let date_string = |value: &str| {
        chrono::DateTime::parse_from_rfc3339(value)
            .map(|date| {
                date.with_timezone(&chrono::Local)
                    .format("%-m/%-d/%Y")
                    .to_string()
            })
            .unwrap_or_else(|_| "Invalid Date".into())
    };
    // Maps to: CC `commands/plugin/ManageMarketplaces.tsx:395-431#buildDetailsMenuOptions`.
    let build_details_menu_options = |marketplace: Option<&MarketplaceState>| {
        let Some(marketplace) = marketplace else {
            return Vec::new();
        };
        let mut options = vec![
            (
                format!("Browse plugins ({})", marketplace.plugin_count.unwrap_or(0)),
                None,
                "browse",
            ),
            (
                "Update marketplace".into(),
                marketplace
                    .last_updated
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .map(|s| format!("(last updated {})", date_string(s))),
                "update",
            ),
        ];
        if !crate::utils::config::should_skip_plugin_autoupdate() {
            options.push((
                if marketplace.auto_update {
                    "Disable auto-update"
                } else {
                    "Enable auto-update"
                }
                .into(),
                None,
                "toggle-auto-update",
            ));
        }
        options.push(("Remove marketplace".into(), None, "remove"));
        options
    };
    // Maps to: CC `commands/plugin/ManageMarketplaces.tsx:434-457#handleToggleAutoUpdate`.
    let handle_toggle_auto_update = Handler::from(move |marketplace: MarketplaceState| {
        let mut marketplace_states = marketplace_states;
        let mut selected_marketplace = selected_marketplace;
        let mut process_error = process_error;
        crate::utils::process_runtime::runtime_handle_for_detached_work()
            .expect("marketplace runtime")
            .spawn(async move {
                let new_auto_update = !marketplace.auto_update;
                match set_marketplace_auto_update(&marketplace.name, new_auto_update).await {
                    Ok(()) => {
                        let mut states = marketplace_states.read().clone();
                        for state in &mut states {
                            if state.name == marketplace.name {
                                state.auto_update = new_auto_update;
                            }
                        }
                        marketplace_states.set(states);
                        let mut selected = selected_marketplace.read().clone();
                        if let Some(ref mut selected) = selected {
                            selected.auto_update = new_auto_update;
                        }
                        selected_marketplace.set(selected);
                    }
                    Err(error) => process_error.set(Some(error.to_string())),
                }
            });
    });
    let runtime = hooks
        .try_use_context::<crate::keybindings::keybinding_context::KeybindingRuntime>()
        .map(|r| r.clone());
    let processing = is_processing.get();
    let view = internal_view.get();
    let pending = has_pending_changes();
    use_keybinding(
        &mut hooks,
        runtime.clone(),
        "confirm:no",
        ContextName::Confirmation,
        move || {
            !processing
                && matches!(
                    view,
                    InternalViewState::Details | InternalViewState::ConfirmRemove
                )
        },
        move || {
            internal_view.set(InternalViewState::List);
            details_menu_index.set(0);
            true
        },
    );
    use_keybinding(
        &mut hooks,
        runtime.clone(),
        "confirm:no",
        ContextName::Confirmation,
        move || !processing && view == InternalViewState::List && pending,
        move || {
            let mut states = marketplace_states.read().clone();
            for state in &mut states {
                state.pending_update = false;
                state.pending_remove = false;
            }
            marketplace_states.set(states);
            selected_index.set(0);
            true
        },
    );
    let set_view = props.set_view_state.clone();
    use_keybinding(
        &mut hooks,
        runtime.clone(),
        "confirm:no",
        ContextName::Confirmation,
        move || !processing && view == InternalViewState::List && !pending,
        move || {
            set_view(ViewState::Menu);
            true
        },
    );
    let set_view = props.set_view_state.clone();
    let apply = apply_changes.clone();
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
                Box::new(move || {
                    selected_index
                        .set((selected_index.get() + 1).min(marketplace_states.read().len()));
                    true
                }),
            ),
            (
                "select:accept".into(),
                Box::new(move || {
                    if selected_index.get() == 0 {
                        set_view(ViewState::AddMarketplace {
                            initial_value: None,
                        });
                    } else if pending {
                        apply(None);
                    } else {
                        let marketplace = marketplace_states
                            .read()
                            .get(selected_index.get() - 1)
                            .cloned();
                        if let Some(marketplace) = marketplace {
                            selected_marketplace.set(Some(marketplace));
                            internal_view.set(InternalViewState::Details);
                            details_menu_index.set(0);
                        }
                    }
                    true
                }),
            ),
        ],
        ContextName::Select,
        move || !processing && view == InternalViewState::List,
    );
    let selected = selected_marketplace.read().clone();
    let menu_options = build_details_menu_options(selected.as_ref());
    let menu_length = menu_options.len();
    let selected_option = menu_options
        .get(details_menu_index.get())
        .map(|option| option.2);
    let set_view = props.set_view_state.clone();
    let apply = apply_changes.clone();
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
                    details_menu_index
                        .set((details_menu_index.get() + 1).min(menu_length.saturating_sub(1)));
                    true
                }),
            ),
            (
                "select:accept".into(),
                Box::new(move || {
                    let Some(selected) = selected.clone() else {
                        return true;
                    };
                    match selected_option {
                        Some("browse") => set_view(ViewState::BrowseMarketplace {
                            target_marketplace: Some(selected.name),
                            target_plugin: None,
                        }),
                        Some("update") => {
                            let mut states = marketplace_states.read().clone();
                            for state in &mut states {
                                if state.name == selected.name {
                                    state.pending_update = true;
                                }
                            }
                            marketplace_states.set(states.clone());
                            apply(Some(states));
                        }
                        Some("toggle-auto-update") => handle_toggle_auto_update(selected),
                        Some("remove") => internal_view.set(InternalViewState::ConfirmRemove),
                        _ => {}
                    }
                    true
                }),
            ),
        ],
        ContextName::Select,
        move || !processing && view == InternalViewState::Details,
    );
    hooks.use_terminal_events(move |event| {
        if processing {
            return;
        }
        let input = match event {
            TerminalEvent::Key(key) if key.kind != KeyEventKind::Release => {
                if let KeyCode::Char(c) = key.code {
                    c.to_string()
                } else {
                    return;
                }
            }
            TerminalEvent::Paste(text) => text,
            _ => return,
        };
        if view == InternalViewState::List {
            if matches!(input.as_str(), "u" | "U") && selected_index.get() > 0 {
                let mut states = marketplace_states.read().clone();
                if let Some(state) = states.get_mut(selected_index.get() - 1) {
                    if !state.pending_update {
                        state.pending_remove = false;
                    }
                    state.pending_update = !state.pending_update;
                }
                marketplace_states.set(states);
            } else if matches!(input.as_str(), "r" | "R") && selected_index.get() > 0 {
                let selected = marketplace_states
                    .read()
                    .get(selected_index.get() - 1)
                    .cloned();
                if let Some(selected) = selected {
                    selected_marketplace.set(Some(selected));
                    internal_view.set(InternalViewState::ConfirmRemove);
                }
            }
        } else if view == InternalViewState::ConfirmRemove {
            if matches!(input.as_str(), "y" | "Y") {
                confirm_remove(());
            } else if matches!(input.as_str(), "n" | "N") {
                internal_view.set(InternalViewState::List);
                selected_marketplace.set(None);
            }
        }
    });
    let theme = *hooks.use_context::<crate::utils::theme::Theme>();
    let figures = crate::constants::figures::figures();
    if loading.get() {
        return element! {Text(content:"Loading marketplaces…")}.into_any();
    }
    let states = marketplace_states.read().clone();
    let selected = selected_marketplace.read().clone();
    let exit_state = props.exit_state.unwrap_or_default();
    if states.is_empty() {
        return element!{View(flex_direction:FlexDirection::Column){
        View(margin_bottom:1u32){Text(content:"Manage marketplaces",weight:Weight::Bold)}
        View(gap:1u32){Text(content:format!("{} +",figures.pointer),color:theme.suggestion) Text(content:"Add Marketplace",weight:Weight::Bold,color:theme.suggestion)}
        View(margin_left:3u32){#(if exit_state.pending{element!{Text(content:format!("Press {} again to go back",exit_state.key_name.unwrap_or("")),dim:true,italic:true)}.into_any()}else{element!{ContextProvider(value:Context::owned(crate::components::design_system::keyboard_shortcut_hint::KeyboardShortcutHintStyleContext{dim:true,italic:true})){Byline{ConfigurableShortcutHint(action:"select:accept",context:"Select",fallback:"Enter",description:"select") ConfigurableShortcutHint(action:"confirm:no",context:"Confirmation",fallback:"Esc",description:"go back")}}}.into_any()})}
    }}.into_any();
    }
    if view == InternalViewState::ConfirmRemove {
        if let Some(selected) = selected.as_ref() {
            let count = selected.installed_plugins.len();
            return element!{View(flex_direction:FlexDirection::Column){
        View{Text(content:"Remove marketplace ",weight:Weight::Bold,color:theme.warning) Text(content:selected.name.clone(),weight:Weight::Bold,italic:true,color:theme.warning) Text(content:"?",weight:Weight::Bold,color:theme.warning)}
        View(flex_direction:FlexDirection::Column){
            #( (count>0).then(||element!{View(margin_top:1u32){Text(content:format!("This will also uninstall {count} {} from this marketplace:",plural(count,"plugin",None)),color:theme.warning)}}))
            #( (count>0).then(||element!{View(flex_direction:FlexDirection::Column,margin_top:1u32,margin_left:2u32){#(selected.installed_plugins.iter().map(|plugin|element!{Text(content:format!("• {}",plugin.name),dim:true)}))}}))
            View(margin_top:1u32){Text(content:"Press ") Text(content:"y",weight:Weight::Bold) Text(content:" to confirm or ") Text(content:"n",weight:Weight::Bold) Text(content:" to cancel")}
        }
    }}.into_any();
        }
    }
    if view == InternalViewState::Details {
        if let Some(selected) = selected.as_ref() {
            let updating = selected.pending_update || processing;
            let count = selected.plugin_count.unwrap_or(0);
            return element!{View(flex_direction:FlexDirection::Column){
            Text(content:selected.name.clone(),weight:Weight::Bold) Text(content:selected.source.clone(),dim:true)
            View(margin_top:1u32){Text(content:format!("{count} available {}",plural(count,"plugin",None)))}
            #((!selected.installed_plugins.is_empty()).then(||element!{View(flex_direction:FlexDirection::Column,margin_top:1u32){Text(content:format!("Installed plugins ({}):",selected.installed_plugins.len()),weight:Weight::Bold)
                View(flex_direction:FlexDirection::Column,margin_left:1u32){#(selected.installed_plugins.iter().map(|plugin|element!{View(gap:1u32){Text(content:figures.bullet) View(flex_direction:FlexDirection::Column){Text(content:plugin.name.clone()) Text(content:plugin.manifest.description.clone().unwrap_or_default(),dim:true)}}}))}
            }}))
            #(updating.then(||element!{View(margin_top:1u32,flex_direction:FlexDirection::Column){Text(content:"Updating marketplace…",color:theme.claude) #(progress_message.read().clone().filter(|s|!s.is_empty()).map(|message|element!{Text(content:message,dim:true)}))}}))
            #((!updating).then(||success_message.read().clone()).flatten().filter(|s|!s.is_empty()).map(|message|element!{View(margin_top:1u32){Text(content:message,color:theme.claude)}}))
            #((!updating).then(||process_error.read().clone()).flatten().filter(|s|!s.is_empty()).map(|message|element!{View(margin_top:1u32){Text(content:message,color:theme.error)}}))
            #((!updating).then(||element!{View(flex_direction:FlexDirection::Column,margin_top:1u32){#(menu_options.iter().enumerate().map(|(index,option)|element!{View{Text(content:format!("{} {}",if index==details_menu_index.get(){figures.pointer}else{" "},option.0),color:if index==details_menu_index.get(){theme.suggestion}else{theme.text}) #(option.1.as_ref().map(|label|element!{Text(content:format!(" {label}"),dim:true)}))}}))}}))
            #((!updating&&!crate::utils::config::should_skip_plugin_autoupdate()&&selected.auto_update).then(||element!{View(margin_top:1u32){Text(content:"Auto-update enabled. Claude Code will automatically update this marketplace and its installed plugins.",dim:true)}}))
            View(margin_left:3u32){#(if updating{element!{Text(content:"Please wait…",dim:true,italic:true)}.into_any()}else{element!{ContextProvider(value:Context::owned(crate::components::design_system::keyboard_shortcut_hint::KeyboardShortcutHintStyleContext{dim:true,italic:true})){Byline{ConfigurableShortcutHint(action:"select:accept",context:"Select",fallback:"Enter",description:"select") ConfigurableShortcutHint(action:"confirm:no",context:"Confirmation",fallback:"Esc",description:"go back")}}}.into_any()})}
        }}.into_any();
        }
    }
    let (update_count, remove_count) = get_pending_counts();
    element!{View(flex_direction:FlexDirection::Column){
        View(margin_bottom:1u32){Text(content:"Manage marketplaces",weight:Weight::Bold)}
        View(gap:1u32,margin_bottom:1u32){Text(content:format!("{} +",if selected_index.get()==0{figures.pointer}else{" "}),color:if selected_index.get()==0{theme.suggestion}else{theme.text}) Text(content:"Add Marketplace",weight:Weight::Bold,color:if selected_index.get()==0{theme.suggestion}else{theme.text})}
        View(flex_direction:FlexDirection::Column){#(states.iter().enumerate().map(|(index,state)|{
            let mut indicators=Vec::new();if state.pending_update{indicators.push("UPDATE");}if state.pending_remove{indicators.push("REMOVE");}
            let mut details=state.plugin_count.map(|count|format!("{count} available")).unwrap_or_default();if !state.installed_plugins.is_empty(){details.push_str(&format!(" • {} installed",state.installed_plugins.len()));}if let Some(date)=state.last_updated.as_deref().filter(|s|!s.is_empty()){details.push_str(&format!(" • Updated {}",date_string(date)));}
            element!{View(gap:1u32,margin_bottom:1u32){Text(content:format!("{} {}",if index+1==selected_index.get(){figures.pointer}else{" "},if state.pending_remove{figures.cross}else{figures.bullet}),color:if index+1==selected_index.get(){theme.suggestion}else{theme.text})
                View(flex_direction:FlexDirection::Column,flex_grow:1.0f32){
                    View(gap:1u32){View{
                        #((state.name=="claude-plugins-official").then(||element!{Text(content:"✻ ",color:theme.claude,weight:Weight::Bold,dim:state.pending_remove)}))
                        Text(content:state.name.clone(),weight:Weight::Bold,dim:state.pending_remove,strikethrough:state.pending_remove)
                        #((state.name=="claude-plugins-official").then(||element!{Text(content:" ✻",color:theme.claude,weight:Weight::Bold,dim:state.pending_remove)}))
                    } #((!indicators.is_empty()).then(||element!{Text(content:format!("[{}]",indicators.join(", ")),color:theme.warning)}))}
                    Text(content:state.source.clone(),dim:true) Text(content:details,dim:true)
                }
            }}
        }))}
        #(pending.then(||element!{View(margin_top:1u32,flex_direction:FlexDirection::Column){
            View{Text(content:"Pending changes:",weight:Weight::Bold) Text(content:" Enter to apply",dim:true)}
            #((update_count>0).then(||element!{Text(content:format!("• Update {update_count} {}",plural(update_count,"marketplace",None)))}))
            #((remove_count>0).then(||element!{Text(content:format!("• Remove {remove_count} {}",plural(remove_count,"marketplace",None)),color:theme.warning)}))
        }}))
        #(processing.then(||element!{View(margin_top:1u32){Text(content:"Processing changes…",color:theme.claude)}}))
        #(process_error.read().clone().filter(|s|!s.is_empty()).map(|message|element!{View(margin_top:1u32){Text(content:message,color:theme.error)}}))
        ManageMarketplacesKeyHints(exit_state:exit_state,has_pending_actions:pending)
    }}.into_any()
}

/// Maps to: CC `commands/plugin/ManageMarketplaces.tsx:937-940#ManageMarketplacesKeyHintsProps`.
#[derive(Default, Props)]
struct ManageMarketplacesKeyHintsProps {
    exit_state: ExitKeyState,
    has_pending_actions: bool,
}
/// Maps to: CC `commands/plugin/ManageMarketplaces.tsx:942-992#ManageMarketplacesKeyHints`.
#[component]
fn ManageMarketplacesKeyHints(
    props: &mut ManageMarketplacesKeyHintsProps,
) -> impl Into<AnyElement<'static>> {
    if props.exit_state.pending {
        return element!{View(margin_top:1u32){Text(content:format!("Press {} again to go back",props.exit_state.key_name.unwrap_or("")),dim:true,italic:true)}}.into_any();
    }
    element!{View(margin_top:1u32){ContextProvider(value:Context::owned(crate::components::design_system::keyboard_shortcut_hint::KeyboardShortcutHintStyleContext{dim:true,italic:true})){Byline{
        ConfigurableShortcutHint(action:"select:accept",context:"Select",fallback:"Enter",description:if props.has_pending_actions{"apply changes"}else{"select"})
        #((!props.has_pending_actions).then(||element!{KeyboardShortcutHint(shortcut:"u",action:"update")}))
        #((!props.has_pending_actions).then(||element!{KeyboardShortcutHint(shortcut:"r",action:"remove")}))
        ConfigurableShortcutHint(action:"confirm:no",context:"Confirmation",fallback:"Esc",description:if props.has_pending_actions{"cancel"}else{"go back"})
    }}}}.into_any()
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use std::time::Duration;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn mounted_marketplace_pending_cancel_remove_cancel_and_details() {
        crate::utils::process_runtime::initialize_test_process_runtime();
        let fixture = super::super::plugin_details_helpers::PluginUiTestImports {
            plugins: vec![serde_json::json!({"name":"sample","source":"./sample"})],
            ..Default::default()
        };
        let mut app = element! {
            ContextProvider(value:Context::owned(*crate::utils::theme::current())) {
                ContextProvider(value:Context::owned(fixture)) {
                    ContextProvider(value:Context::owned(crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings())) {
                        FocusScope(handle_keys:false) {ManageMarketplaces}
                    }
                }
            }
        };
        let (keys, events) = async_channel::unbounded();
        let mut frames =
            Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(100, 40),
            ));
        let initial = tokio::time::timeout(Duration::from_secs(4), async {
            loop {
                let frame = frames.next().await.unwrap().to_string();
                if frame.contains("market") && !frame.contains("Loading marketplaces") {
                    break frame;
                }
            }
        })
        .await
        .unwrap();
        assert!(initial.contains("Manage marketplaces"));
        assert!(initial.contains("1 available"));
        // Same source handlers exercised by manage-oracle.ts; no imported
        // mutation is invoked, so this mounted test cannot touch user settings.
        for (key, expected, absent) in [
            (KeyCode::Down, "market", "Pending changes:"),
            (KeyCode::Char('u'), "Pending changes:", "[REMOVE]"),
            (KeyCode::Esc, "Add Marketplace", "Pending changes:"),
            (KeyCode::Down, "market", "Pending changes:"),
            (
                KeyCode::Char('r'),
                "Remove marketplace market?",
                "Manage marketplaces",
            ),
            (
                KeyCode::Char('n'),
                "Manage marketplaces",
                "Remove marketplace market?",
            ),
            (KeyCode::Enter, "Browse plugins (1)", "Manage marketplaces"),
            (KeyCode::Esc, "Manage marketplaces", "Browse plugins (1)"),
        ] {
            keys.send(TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, key)))
                .await
                .unwrap();
            let frame = tokio::time::timeout(Duration::from_secs(4), async {
                loop {
                    let frame = frames.next().await.unwrap().to_string();
                    if frame.contains(expected) && !frame.contains(absent) {
                        break frame;
                    }
                }
            })
            .await
            .unwrap();
            assert!(frame.contains(expected));
        }
    }

    #[test]
    fn official_auto_update_defaults_and_explicit_override_match_source() {
        for (name, entry, expected) in [
            ("CLAUDE-PLUGINS-OFFICIAL", serde_json::json!({}), true),
            ("knowledge-work-plugins", serde_json::json!({}), false),
            ("third-party", serde_json::json!({}), false),
            (
                "claude-plugins-official",
                serde_json::json!({"autoUpdate":false}),
                false,
            ),
            (
                "knowledge-work-plugins",
                serde_json::json!({"autoUpdate":true}),
                true,
            ),
        ] {
            assert_eq!(is_marketplace_auto_update(name, &entry), expected);
        }
    }
}
