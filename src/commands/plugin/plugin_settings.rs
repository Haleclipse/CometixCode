//! Maps to: CC `commands/plugin/PluginSettings.tsx`.
//! Parent-owned navigation, completion and plugin-refresh signal.
use super::parse_args::{MarketplaceAction, ParsedCommand, parse_plugin_args};
use super::validate_plugin::ValidatePlugin;
use super::{
    add_marketplace::AddMarketplace, browse_marketplace::BrowseMarketplace,
    discover_plugins::DiscoverPlugins, manage_marketplaces::ManageMarketplaces,
    manage_plugins::ManagePlugins,
};
use crate::components::design_system::{
    pane::Pane,
    tabs::{Tab, Tabs},
};
use crate::utils::plugins::marketplace_manager::load_known_marketplaces_config;
use iocraft::prelude::*;

/// Native representation adapter for this plugin panel's source useState slots.
/// Worker writes modify the protected payload, never iocraft's render-borrowed
/// slot. The retained future only requests another frame; it does not reorder
/// installation effects or callbacks. Like React setters, writes after unmount
/// are ignored. Async code must capture values before awaiting, not read a
/// dropped owner's state or manufacture a default snapshot.
pub(crate) struct PluginUiState<T: Send + Sync + 'static> {
    value: State<std::sync::Mutex<T>>,
    notify: State<async_channel::Sender<()>>,
}
impl<T: Send + Sync + 'static> Copy for PluginUiState<T> {}
impl<T: Send + Sync + 'static> Clone for PluginUiState<T> {
    fn clone(&self) -> Self {
        *self
    }
}
pub(crate) struct PluginUiSnapshot<T>(T);
impl<T> std::ops::Deref for PluginUiSnapshot<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}
impl<T: Clone + Send + Sync + 'static> PluginUiState<T> {
    pub(crate) fn read(&self) -> PluginUiSnapshot<T> {
        PluginUiSnapshot(self.value.read().lock().unwrap().clone())
    }
    pub(crate) fn get(&self) -> T {
        self.read().0
    }
    pub(crate) fn set(&self, next: T) {
        self.update(|value| *value = next);
    }
    pub(crate) fn update(&self, update: impl FnOnce(&mut T)) {
        if let Some(value) = self.value.try_read() {
            update(&mut value.lock().unwrap());
            if let Some(notify) = self.notify.try_read() {
                let _ = notify.try_send(());
            }
        }
    }
}
pub(crate) fn use_plugin_ui_state<T: Clone + Send + Sync + Unpin + 'static>(
    hooks: &mut Hooks,
    initial: impl FnOnce() -> T,
) -> PluginUiState<T> {
    let value = hooks.use_state(|| std::sync::Mutex::new(initial()));
    let channel = hooks.use_const(|| std::sync::Arc::new(async_channel::unbounded::<()>()));
    let (sender, receiver) = channel.as_ref().clone();
    let notify = hooks.use_state(|| sender);
    let mut revision = hooks.use_state(|| 0usize);
    hooks.use_future(async move {
        while receiver.recv().await.is_ok() {
            revision.set(revision.get().wrapping_add(1));
        }
    });
    PluginUiState { value, notify }
}

#[derive(Default, Props)]
struct MarketplaceListProps {
    on_complete: Handler<Option<String>>,
}

/// Maps to: CC `PluginSettings.tsx:47-62#loadList`, nested in MarketplaceList.
async fn load_list(
    on_complete: Handler<Option<String>>,
    #[cfg(test)] fixture: Option<MarketplaceConfigFixture>,
) {
    #[cfg(test)]
    let result = match fixture {
        Some(fixture) => (fixture.0)().await,
        None => load_known_marketplaces_config().await,
    };
    #[cfg(not(test))]
    let result = load_known_marketplaces_config().await;
    match result {
        Ok(config) => {
            let names = crate::utils::process_env::ecmascript_object_entries(&config)
                .into_iter()
                .map(|(name, _)| name)
                .collect::<Vec<_>>();
            if names.is_empty() {
                on_complete(Some("No marketplaces configured".to_string()));
            } else {
                on_complete(Some(format!(
                    "Configured marketplaces:\n{}",
                    names
                        .iter()
                        .map(|name| format!("  • {name}"))
                        .collect::<Vec<_>>()
                        .join("\n")
                )));
            }
        }
        Err(error) => {
            // CC errors.ts:119-121 errorMessage(Error) -> error.message.
            // The canonical loader retains that message in its native Error.
            on_complete(Some(format!("Error loading marketplaces: {error}")));
        }
    }
}

/// Maps to: CC `PluginSettings.tsx:41-68#MarketplaceList`.
#[component]
fn MarketplaceList(
    props: &mut MarketplaceListProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let on_complete = props.on_complete.clone();
    let callback_identity = (&*on_complete as *const dyn Fn(Option<String>)) as *const () as usize;
    #[cfg(test)]
    let fixture = hooks
        .try_use_context::<MarketplaceConfigFixture>()
        .map(|value| value.clone());
    // Source effect has no cleanup: a late load still completes after unmount.
    // Reuse the process runtime and callback-reference effect carrier used by
    // the adjacent source ValidatePlugin owner.
    hooks.use_effect(
        move || {
            crate::utils::process_runtime::runtime_handle_for_detached_work()
                .expect("marketplace listing requires the initialized process runtime")
                .spawn(load_list(
                    on_complete,
                    #[cfg(test)]
                    fixture,
                ));
        },
        callback_identity,
    );
    element! { Text(content: "Loading marketplaces...") }
}

/// Test-only imported loader boundary; parent/child/effect/formatter remain real.
#[cfg(test)]
#[derive(Clone)]
pub(crate) struct MarketplaceConfigFixture(
    pub(crate)  std::sync::Arc<
        dyn Fn() -> futures::future::BoxFuture<
                'static,
                anyhow::Result<crate::utils::plugins::marketplace_manager::KnownMarketplacesConfig>,
            > + Send
            + Sync,
    >,
);

/// Source consumer-derived carrier: `types.ts` is a generated stub whose string
/// alias contradicts PluginSettings.tsx:644-715's actual tagged object values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewState {
    Menu,
    Help,
    Validate {
        path: Option<String>,
    },
    BrowseMarketplace {
        target_marketplace: Option<String>,
        target_plugin: Option<String>,
    },
    DiscoverPlugins {
        target_plugin: Option<String>,
    },
    ManagePlugins {
        target_plugin: Option<String>,
        target_marketplace: Option<String>,
        action: Option<String>,
    },
    MarketplaceList,
    AddMarketplace {
        initial_value: Option<String>,
    },
    ManageMarketplaces {
        target_marketplace: Option<String>,
        action: Option<String>,
    },
    MarketplaceMenu,
}

/// Maps to: CC `PluginSettings.tsx:644-715#getInitialViewState`.
pub fn get_initial_view_state(command: ParsedCommand) -> ViewState {
    match command {
        ParsedCommand::Help => ViewState::Help,
        ParsedCommand::Validate { path } => ViewState::Validate { path },
        ParsedCommand::Install {
            marketplace,
            plugin,
        } => {
            if let Some(marketplace) = marketplace.filter(|value| !value.is_empty()) {
                ViewState::BrowseMarketplace {
                    target_marketplace: Some(marketplace),
                    target_plugin: plugin,
                }
            } else {
                ViewState::DiscoverPlugins {
                    target_plugin: plugin.filter(|value| !value.is_empty()),
                }
            }
        }
        ParsedCommand::Manage => ViewState::ManagePlugins {
            target_marketplace: None,
            target_plugin: None,
            action: None,
        },
        ParsedCommand::Uninstall { plugin } => ViewState::ManagePlugins {
            target_marketplace: None,
            target_plugin: plugin,
            action: Some("uninstall".into()),
        },
        ParsedCommand::Enable { plugin } => ViewState::ManagePlugins {
            target_marketplace: None,
            target_plugin: plugin,
            action: Some("enable".into()),
        },
        ParsedCommand::Disable { plugin } => ViewState::ManagePlugins {
            target_marketplace: None,
            target_plugin: plugin,
            action: Some("disable".into()),
        },
        ParsedCommand::Marketplace { action, target } => match action {
            Some(MarketplaceAction::List) => ViewState::MarketplaceList,
            Some(MarketplaceAction::Add) => ViewState::AddMarketplace {
                initial_value: target,
            },
            Some(MarketplaceAction::Remove) => ViewState::ManageMarketplaces {
                target_marketplace: target,
                action: Some("remove".into()),
            },
            Some(MarketplaceAction::Update) => ViewState::ManageMarketplaces {
                target_marketplace: target,
                action: Some("update".into()),
            },
            None => ViewState::MarketplaceMenu,
        },
        ParsedCommand::Menu => ViewState::DiscoverPlugins {
            target_plugin: None,
        },
    }
}

/// Maps to: CC `PluginSettings.tsx:717-721#getInitialTab`.
pub fn get_initial_tab(view: &ViewState) -> &'static str {
    match view {
        ViewState::ManagePlugins { .. } => "installed",
        ViewState::ManageMarketplaces { .. } => "marketplaces",
        _ => "discover",
    }
}

#[derive(Default, Props)]
pub struct PluginSettingsProps {
    pub args: Option<String>,
    pub show_mcp_redirect_message: bool,
    pub on_complete: Handler<Option<String>>,
}

/// Maps to: CC `PluginSettings.tsx:723-1045#PluginSettings`.
#[component]
pub fn PluginSettings(
    props: &mut PluginSettingsProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let parsed_command = parse_plugin_args(props.args.as_deref());
    let cli_mode = matches!(
        &parsed_command,
        ParsedCommand::Marketplace {
            action: Some(MarketplaceAction::Add),
            target: Some(_)
        }
    );
    let initial_view_state = get_initial_view_state(parsed_command);
    let mut view_state = use_plugin_ui_state(&mut hooks, || initial_view_state.clone());
    let mut active_tab = use_plugin_ui_state(&mut hooks, || {
        get_initial_tab(&initial_view_state).to_string()
    });
    let mut input_value = use_plugin_ui_state(&mut hooks, || match &initial_view_state {
        ViewState::AddMarketplace { initial_value } => initial_value.clone().unwrap_or_default(),
        _ => String::new(),
    });
    let mut cursor_offset = use_plugin_ui_state(&mut hooks, || 0usize);
    let mut error = use_plugin_ui_state(&mut hooks, || None::<String>);
    let mut result = use_plugin_ui_state(&mut hooks, || None::<String>);
    let mut child_search_active = use_plugin_ui_state(&mut hooks, || false);
    let store = crate::state::app_state::use_set_app_state(&mut hooks);
    let plugin_error_count = crate::state::app_state::use_app_state(&mut hooks, |state| {
        state.plugins.errors.len()
            + state
                .plugins
                .installation_status
                .marketplaces
                .iter()
                .filter(|m| m.status == "failed")
                .count()
    });
    let errors_title = if plugin_error_count > 0 {
        format!("Errors ({plugin_error_count})")
    } else {
        "Errors".to_string()
    };
    let exit_state = crate::hooks::use_exit::use_exit_on_ctrl_cd_with_keybindings(&mut hooks, true);
    // Source setState/useCallback identities are stable across child effects.
    let set_view_state = hooks.use_memo(
        move || {
            Handler::from(move |next| {
                let mut state = view_state;
                state.set(next);
            })
        },
        (),
    );
    let set_active_tab = hooks.use_memo(
        move || {
            Handler::from(move |next| {
                let mut state = active_tab;
                state.set(next);
            })
        },
        (),
    );
    let set_error = hooks.use_memo(
        move || {
            Handler::from(move |next| {
                let mut state = error;
                state.set(next);
            })
        },
        (),
    );
    let set_result = hooks.use_memo(
        move || {
            Handler::from(move |next| {
                let mut state = result;
                state.set(next);
            })
        },
        (),
    );
    let set_input_value = hooks.use_memo(
        move || {
            Handler::from(move |next| {
                let mut state = input_value;
                state.set(next);
            })
        },
        (),
    );
    let set_cursor_offset = hooks.use_memo(
        move || {
            Handler::from(move |next| {
                let mut state = cursor_offset;
                state.set(next);
            })
        },
        (),
    );
    let set_child_search_active = hooks.use_memo(
        move || {
            Handler::from(move |next| {
                let mut state = child_search_active;
                state.set(next);
            })
        },
        (),
    );
    let mark_plugins_changed = hooks.use_memo(
        move || {
            Handler::from(move |()| {
                store.set_state(|previous| {
                    if previous.plugins.needs_refresh {
                        return crate::state::store::UpdateDecision::Same(());
                    }
                    let mut next = (**previous).clone();
                    std::sync::Arc::make_mut(&mut next.plugins).needs_refresh = true;
                    crate::state::store::UpdateDecision::Replace {
                        next: std::sync::Arc::new(next),
                        result: (),
                    }
                });
            })
        },
        (),
    );
    let handle_tab_change = hooks.use_memo(
        move || {
            Handler::from(move |tab: String| {
                let mut active_tab = active_tab;
                let mut error = error;
                let mut view_state = view_state;
                active_tab.set(tab.clone());
                error.set(None);
                match tab.as_str() {
                    "discover" => view_state.set(ViewState::DiscoverPlugins {
                        target_plugin: None,
                    }),
                    "installed" => view_state.set(ViewState::ManagePlugins {
                        target_plugin: None,
                        target_marketplace: None,
                        action: None,
                    }),
                    "marketplaces" => view_state.set(ViewState::ManageMarketplaces {
                        target_marketplace: None,
                        action: None,
                    }),
                    _ => {}
                }
            })
        },
        (),
    );
    let view = view_state.read().clone();
    let on_complete = props.on_complete.clone();
    let callback_identity = (&*on_complete as *const dyn Fn(Option<String>)) as *const () as usize;
    let is_menu = matches!(&view, ViewState::Menu);
    let is_help = matches!(&view, ViewState::Help);
    let completion_result = result.read().clone();
    hooks.use_effect(
        move || {
            if is_menu && completion_result.as_ref().is_none_or(|s| s.is_empty()) {
                on_complete(None);
            }
        },
        (is_menu, result.read().clone(), callback_identity),
    );
    let browse = matches!(&view, ViewState::BrowseMarketplace { .. });
    let tab = active_tab.read().clone();
    hooks.use_effect(
        move || {
            if browse && tab != "discover" {
                active_tab.set("discover".into());
            }
        },
        (browse, active_tab.read().clone()),
    );
    let runtime = hooks
        .try_use_context::<crate::keybindings::keybinding_context::KeybindingRuntime>()
        .map(|r| r.clone());
    let add_active = matches!(&view, ViewState::AddMarketplace { .. });
    crate::keybindings::use_keybinding::use_keybinding(
        &mut hooks,
        runtime,
        "confirm:no",
        crate::keybindings::types::ContextName::Settings,
        move || add_active,
        move || {
            active_tab.set("marketplaces".into());
            view_state.set(ViewState::ManageMarketplaces {
                target_marketplace: None,
                action: None,
            });
            input_value.set(String::new());
            error.set(None);
            true
        },
    );
    let on_complete = props.on_complete.clone();
    let completion_result = result.read().clone();
    hooks.use_effect(
        move || {
            if let Some(message) = completion_result.filter(|s| !s.is_empty()) {
                on_complete(Some(message));
            }
        },
        (result.read().clone(), callback_identity),
    );
    let on_complete = props.on_complete.clone();
    hooks.use_effect(
        move || {
            if is_help {
                on_complete(None);
            }
        },
        (is_help, callback_identity),
    );
    let tab = active_tab.read().clone();
    match view {
        // Source's five <Text> </Text> rows remain one-cell blank lines.
        // Existing Text NoWrap preserves this literal: iocraft Wrap trims its
        // trailing space during measurement, turning it into zero-height text.
        ViewState::Help => element! {
            View(flex_direction: FlexDirection::Column) {
                Text(content: "Plugin Command Usage:", weight: Weight::Bold)
                Text(content: " ", wrap: TextWrap::NoWrap)
                Text(content: "Installation:", dim: true)
                Text(content: " /plugin install - Browse and install plugins")
                Text(content: " /plugin install <marketplace> - Install from specific marketplace")
                Text(content: " /plugin install <plugin> - Install specific plugin")
                Text(content: " /plugin install <plugin>@<market> - Install plugin from marketplace")
                Text(content: " ", wrap: TextWrap::NoWrap)
                Text(content: "Management:", dim: true)
                Text(content: " /plugin manage - Manage installed plugins")
                Text(content: " /plugin enable <plugin> - Enable a plugin")
                Text(content: " /plugin disable <plugin> - Disable a plugin")
                Text(content: " /plugin uninstall <plugin> - Uninstall a plugin")
                Text(content: " ", wrap: TextWrap::NoWrap)
                Text(content: "Marketplaces:", dim: true)
                Text(content: " /plugin marketplace - Marketplace management menu")
                Text(content: " /plugin marketplace add - Add a marketplace")
                Text(content: " /plugin marketplace add <path/url> - Add marketplace directly")
                Text(content: " /plugin marketplace update - Update marketplaces")
                Text(content: " /plugin marketplace update <name> - Update specific marketplace")
                Text(content: " /plugin marketplace remove - Remove a marketplace")
                Text(content: " /plugin marketplace remove <name> - Remove specific marketplace")
                Text(content: " /plugin marketplace list - List all marketplaces")
                Text(content: " ", wrap: TextWrap::NoWrap)
                Text(content: "Validation:", dim: true)
                Text(content: " /plugin validate <path> - Validate a manifest file or directory")
                Text(content: " ", wrap: TextWrap::NoWrap)
                Text(content: "Other:", dim: true)
                Text(content: " /plugin - Main plugin menu")
                Text(content: " /plugin help - Show this help")
                Text(content: " /plugins - Alias for /plugin")
            }
        }.into_any(),
        ViewState::Validate { path } => element! {
            ValidatePlugin(path: path, on_complete: props.on_complete.clone())
        }.into_any(),
        ViewState::MarketplaceList => element! {
            MarketplaceList(on_complete: props.on_complete.clone())
        }.into_any(),
        ViewState::MarketplaceMenu=>{view_state.set(ViewState::Menu);element!{View}.into_any()},
        ViewState::AddMarketplace{..}=>element!{AddMarketplace(input_value:input_value.read().clone(),set_input_value:set_input_value,cursor_offset:cursor_offset.get(),set_cursor_offset:set_cursor_offset,error:error.read().clone(),set_error:set_error,result:result.read().clone(),set_result:set_result,set_view_state:set_view_state,on_add_complete:mark_plugins_changed,cli_mode:cli_mode)}.into_any(),
        _=>{
            let body=match tab.as_str(){
                "installed"=>{
                    let (target_plugin,target_marketplace,action)=match view{ViewState::ManagePlugins{target_plugin,target_marketplace,action}=>(target_plugin,target_marketplace,action),_=>(None,None,None)};
                    element!{ManagePlugins(set_view_state:set_view_state,set_result:set_result,on_manage_complete:mark_plugins_changed,on_search_mode_change:set_child_search_active,target_plugin:target_plugin,target_marketplace:target_marketplace,action:action)}.into_any()
                },
                "marketplaces"=>{
                    let (target_marketplace,action)=match view{ViewState::ManageMarketplaces{target_marketplace,action}=>(target_marketplace,action),_=>(None,None)};
                    element!{ManageMarketplaces(set_view_state:set_view_state,error:error.read().clone(),set_error:set_error,set_result:set_result,exit_state:Some(exit_state),on_manage_complete:mark_plugins_changed,target_marketplace:target_marketplace,action:action)}.into_any()
                },
                "errors"=>element!{ErrorsTabContent(set_view_state:set_view_state,set_active_tab:set_active_tab,mark_plugins_changed:mark_plugins_changed)}.into_any(),
                _=>match view{
                    ViewState::BrowseMarketplace{target_marketplace,target_plugin}=>element!{BrowseMarketplace(error:error.read().clone(),set_error:set_error,result:result.read().clone(),set_result:set_result,set_view_state:set_view_state,on_install_complete:mark_plugins_changed,target_marketplace:target_marketplace,target_plugin:target_plugin)}.into_any(),
                    _=>{let target_plugin=match view{ViewState::DiscoverPlugins{target_plugin}=>target_plugin,_=>None};element!{DiscoverPlugins(error:error.read().clone(),set_error:set_error,result:result.read().clone(),set_result:set_result,set_view_state:set_view_state,on_install_complete:mark_plugins_changed,on_search_mode_change:set_child_search_active,target_plugin:target_plugin)}.into_any()}
                }
            };
            let theme=*hooks.use_context::<crate::utils::theme::Theme>();
            let mut body=Some(body);
            let tabs=[("discover","Discover".to_string()),("installed","Installed".to_string()),("marketplaces","Marketplaces".to_string()),("errors",errors_title)].into_iter().map(|(id,title)|{
                element!{Tab(id:Some(id.into()),title:title){#(if tab==id{body.take()}else{None})}}
            }).collect::<Vec<_>>();
            element!{Pane(color:Some(theme.suggestion)){
                Tabs(title:Some("Plugins".into()),color:Some(theme.suggestion),selected_tab:Some(tab.clone()),on_tab_change:handle_tab_change,disable_navigation:child_search_active.get(),banner:if props.show_mcp_redirect_message&&tab=="installed"{vec![element!{McpRedirectBanner}.into_any()]}else{vec![]}){#(tabs)}
            }}.into_any()
        },
    }
}

/// Maps to: CC PluginSettings.tsx:70-101#McpRedirectBanner.
#[component]
fn McpRedirectBanner(hooks: Hooks) -> impl Into<AnyElement<'static>> {
    if !crate::utils::build_profile::has_internal_capability(
        crate::utils::build_profile::InternalCapability::Prompts,
    ) {
        return element! {View}.into_any();
    }
    let theme = *hooks.use_context::<crate::utils::theme::Theme>();
    element!{View(flex_direction:FlexDirection::Row,align_items:AlignItems::FLEX_START,padding_left:1u32,margin_top:1u32,border_style:BorderStyle::Single,border_left:true,border_right:false,border_top:false,border_bottom:false,border_color:theme.permission){
        View(flex_shrink:0.0f32){Text(content:"i ",weight:Weight::Bold,italic:true,color:theme.permission)}
        Text(content:"[ANT-ONLY] MCP servers are now managed in /plugins. Use /mcp no-redirect to test old UI")
    }}.into_any()
}

/// Maps to: CC PluginSettings.tsx#ErrorRowAction.
#[derive(Clone, Debug)]
enum ErrorRowAction {
    Navigate {
        tab: String,
        view_state: ViewState,
    },
    RemoveExtraMarketplace {
        name: String,
        sources: Vec<(crate::utils::settings::SettingSource, String)>,
    },
    RemoveInstalledMarketplace {
        name: String,
    },
    ManagedOnly {
        name: String,
    },
    None,
}
/// Maps to: CC PluginSettings.tsx#ErrorRow.
#[derive(Clone, Debug)]
struct ErrorRow {
    label: String,
    message: String,
    guidance: Option<String>,
    action: ErrorRowAction,
    scope: Option<String>,
}
/// Maps to: CC PluginSettings.tsx#getExtraMarketplaceSourceInfo.
fn get_extra_marketplace_source_info(
    name: &str,
) -> (Vec<(crate::utils::settings::SettingSource, String)>, bool) {
    use crate::utils::settings::{SettingSource, get_settings_for_source};
    let mut editable = Vec::new();
    for (source, scope) in [
        (SettingSource::User, "user"),
        (SettingSource::Project, "project"),
        (SettingSource::Local, "local"),
    ] {
        if get_settings_for_source(source)
            .and_then(|s| s.extra_known_marketplaces)
            .and_then(|m| m.get(name).cloned())
            .is_some_and(|v| !v.is_null() && v != false)
        {
            editable.push((source, scope.into()));
        }
    }
    let policy = get_settings_for_source(SettingSource::Policy)
        .and_then(|s| s.extra_known_marketplaces)
        .and_then(|m| m.get(name).cloned())
        .is_some_and(|v| !v.is_null() && v != false);
    (editable, policy)
}
/// Maps to: CC PluginSettings.tsx#buildMarketplaceAction.
fn build_marketplace_action(name: &str) -> ErrorRowAction {
    let (sources, policy) = get_extra_marketplace_source_info(name);
    if !sources.is_empty() {
        ErrorRowAction::RemoveExtraMarketplace {
            name: name.into(),
            sources,
        }
    } else if policy {
        ErrorRowAction::ManagedOnly { name: name.into() }
    } else {
        ErrorRowAction::Navigate {
            tab: "marketplaces".into(),
            view_state: ViewState::ManageMarketplaces {
                target_marketplace: Some(name.into()),
                action: Some("remove".into()),
            },
        }
    }
}
/// Maps to: CC PluginSettings.tsx#buildPluginAction.
fn build_plugin_action(name: &str) -> ErrorRowAction {
    ErrorRowAction::Navigate {
        tab: "installed".into(),
        view_state: ViewState::ManagePlugins {
            target_plugin: Some(name.into()),
            target_marketplace: None,
            action: Some("uninstall".into()),
        },
    }
}
/// Maps to: CC PluginSettings.tsx#isTransientError and TRANSIENT_ERROR_TYPES.
fn is_transient_error(error: &crate::types::plugin::PluginError) -> bool {
    matches!(
        error,
        crate::types::plugin::PluginError::GitAuthFailed { .. }
            | crate::types::plugin::PluginError::GitTimeout { .. }
            | crate::types::plugin::PluginError::NetworkError { .. }
    )
}
/// Maps to: CC PluginSettings.tsx#getPluginNameFromError.
fn get_plugin_name_from_error(error: &crate::types::plugin::PluginError) -> Option<String> {
    let value = serde_json::to_value(error).expect("PluginError serializable");
    value
        .get("pluginId")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            value
                .get("plugin")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
        })
        .map(str::to_owned)
        .or_else(|| {
            error
                .source()
                .contains('@')
                .then(|| error.source().split('@').next().unwrap_or("").into())
        })
}
/// Maps to: CC PluginSettings.tsx#buildErrorRows.
fn build_error_rows(
    failed: &[crate::state::app_state_store::MarketplaceInstallationStatus],
    extra: &[crate::types::plugin::PluginError],
    plugin: &[crate::types::plugin::PluginError],
    other: &[crate::types::plugin::PluginError],
    broken: &[crate::utils::plugins::marketplace_helpers::MarketplaceLoadingFailure],
    transient: &[crate::types::plugin::PluginError],
    scopes: &indexmap::IndexMap<String, String>,
) -> Vec<ErrorRow> {
    use super::plugin_errors::{format_error_message, get_error_guidance};
    let mut rows = Vec::new();
    for error in transient {
        let value = serde_json::to_value(error).unwrap();
        let name = value
            .get("pluginId")
            .or_else(|| value.get("plugin"))
            .and_then(|v| v.as_str())
            .unwrap_or(error.source());
        rows.push(ErrorRow {
            label: name.into(),
            message: format_error_message(error),
            guidance: Some("Restart to retry loading plugins".into()),
            action: ErrorRowAction::None,
            scope: None,
        });
    }
    let mut shown = std::collections::HashSet::new();
    for m in failed {
        shown.insert(m.name.clone());
        let action = build_marketplace_action(&m.name);
        let (editable, policy) = get_extra_marketplace_source_info(&m.name);
        rows.push(ErrorRow {
            label: m.name.clone(),
            message: m
                .error
                .clone()
                .unwrap_or_else(|| "Installation failed".into()),
            guidance: matches!(action, ErrorRowAction::ManagedOnly { .. })
                .then(|| "Managed by your organization — contact your admin".into()),
            action,
            scope: if policy {
                Some("managed".into())
            } else {
                editable.first().map(|(_, scope)| scope.clone())
            },
        });
    }
    for error in extra {
        let value = serde_json::to_value(error).unwrap();
        let name = value
            .get("marketplace")
            .and_then(|v| v.as_str())
            .unwrap_or(error.source());
        if !shown.insert(name.into()) {
            continue;
        }
        let action = build_marketplace_action(name);
        let (editable, policy) = get_extra_marketplace_source_info(name);
        rows.push(ErrorRow {
            label: name.into(),
            message: format_error_message(error),
            guidance: if matches!(action, ErrorRowAction::ManagedOnly { .. }) {
                Some("Managed by your organization — contact your admin".into())
            } else {
                get_error_guidance(error)
            },
            action,
            scope: if policy {
                Some("managed".into())
            } else {
                editable.first().map(|(_, scope)| scope.clone())
            },
        });
    }
    for m in broken {
        if shown.insert(m.name.clone()) {
            rows.push(ErrorRow {
                label: m.name.clone(),
                message: m.error.clone(),
                guidance: None,
                action: ErrorRowAction::RemoveInstalledMarketplace {
                    name: m.name.clone(),
                },
                scope: None,
            });
        }
    }
    let mut shown = std::collections::HashSet::new();
    for error in plugin {
        let name = get_plugin_name_from_error(error);
        if name
            .as_ref()
            .is_some_and(|s| !s.is_empty() && !shown.insert(s.clone()))
        {
            continue;
        }
        let value = serde_json::to_value(error).unwrap();
        let market = value
            .get("marketplace")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());
        let named = name.as_deref().filter(|s| !s.is_empty());
        rows.push(ErrorRow {
            label: named.map_or_else(
                || error.source().into(),
                |name| market.map_or_else(|| name.into(), |m| format!("{name} @ {m}")),
            ),
            message: format_error_message(error),
            guidance: get_error_guidance(error),
            action: named.map_or(ErrorRowAction::None, build_plugin_action),
            scope: named
                .and_then(|n| scopes.get(error.source()).or_else(|| scopes.get(n)))
                .cloned(),
        });
    }
    for error in other {
        rows.push(ErrorRow {
            label: error.source().into(),
            message: format_error_message(error),
            guidance: get_error_guidance(error),
            action: ErrorRowAction::None,
            scope: None,
        });
    }
    rows
}
/// Maps to: CC PluginSettings.tsx#removeExtraMarketplace.
fn remove_extra_marketplace(
    name: &str,
    sources: &[(crate::utils::settings::SettingSource, String)],
) {
    use crate::utils::settings::{get_settings_for_source, update_settings_for_source};
    use serde_json::{Value, json};
    for (source, _) in sources {
        let Some(settings) = get_settings_for_source(*source) else {
            continue;
        };
        let mut updates = serde_json::Map::new();
        if let Some(mut known) = settings
            .extra_known_marketplaces
            .filter(|m| m.get(name).is_some_and(|v| !v.is_null()))
        {
            known[name] = Value::Null;
            updates.insert("extraKnownMarketplaces".into(), known);
        }
        if let Some(mut enabled) = settings.enabled_plugins {
            if let Some(map) = enabled.as_object_mut() {
                let suffix = format!("@{name}");
                let mut removed = false;
                for (key, value) in map {
                    if key.ends_with(&suffix) {
                        *value = Value::Null;
                        removed = true;
                    }
                }
                if removed {
                    updates.insert("enabledPlugins".into(), enabled);
                }
            }
        }
        if !updates.is_empty() {
            let _ = update_settings_for_source(*source, &updates);
        }
    }
}
#[derive(Default, Props)]
struct ErrorsTabContentProps {
    set_view_state: Handler<ViewState>,
    set_active_tab: Handler<String>,
    mark_plugins_changed: Handler<()>,
}
/// Maps to: CC PluginSettings.tsx#ErrorsTabContent.
#[component]
fn ErrorsTabContent(
    props: &mut ErrorsTabContentProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    use crate::keybindings::{
        keybinding_context::KeybindingRuntime,
        types::ContextName,
        use_keybinding::{use_keybinding, use_keybindings},
    };
    use crate::types::plugin::PluginError;
    use crate::utils::plugins::marketplace_helpers::{
        MarketplaceLoadingFailure, load_marketplaces_with_graceful_degradation,
    };
    let plugins = crate::state::app_state::use_app_state(&mut hooks, |s| s.plugins.clone());
    let store = crate::state::app_state::use_set_app_state(&mut hooks);
    let mut selected_index = use_plugin_ui_state(&mut hooks, || 0usize);
    let mut action_message = use_plugin_ui_state(&mut hooks, || None::<String>);
    let mut failures = use_plugin_ui_state(&mut hooks, Vec::<MarketplaceLoadingFailure>::new);
    hooks.use_effect(
        move || {
            crate::utils::process_runtime::runtime_handle_for_detached_work()
                .expect("plugin runtime")
                .spawn(async move {
                    if let Ok(config) = load_known_marketplaces_config().await {
                        failures.set(
                            load_marketplaces_with_graceful_degradation(&config)
                                .await
                                .failures,
                        );
                    }
                });
        },
        (),
    );
    let failed: Vec<_> = plugins
        .installation_status
        .marketplaces
        .iter()
        .filter(|m| m.status == "failed")
        .cloned()
        .collect();
    let names: std::collections::HashSet<_> = failed.iter().map(|m| m.name.as_str()).collect();
    let mut transient = Vec::new();
    let mut extra = Vec::new();
    let mut loading = Vec::new();
    let mut other = Vec::new();
    for error in &plugins.errors {
        if is_transient_error(error) {
            transient.push(error.clone());
            continue;
        }
        if matches!(
            error,
            PluginError::MarketplaceNotFound { .. }
                | PluginError::MarketplaceLoadFailed { .. }
                | PluginError::MarketplaceBlockedByPolicy { .. }
        ) {
            let value = serde_json::to_value(error).unwrap();
            if !names.contains(value["marketplace"].as_str().unwrap_or("")) {
                extra.push(error.clone());
            }
            continue;
        }
        if get_plugin_name_from_error(error).is_some() {
            loading.push(error.clone());
        } else {
            other.push(error.clone());
        }
    }
    let scopes = crate::utils::plugins::plugin_startup_check::get_plugin_editable_scopes();
    let rows = build_error_rows(
        &failed,
        &extra,
        &loading,
        &other,
        &failures.read(),
        &transient,
        &scopes,
    );
    let runtime = hooks
        .try_use_context::<KeybindingRuntime>()
        .map(|value| value.clone());
    let back = props.set_view_state.clone();
    use_keybinding(
        &mut hooks,
        runtime.clone(),
        "confirm:no",
        ContextName::Confirmation,
        || true,
        move || {
            back(ViewState::Menu);
            true
        },
    );
    let count = rows.len();
    // Capture the source render's selected action. Key events execute it here,
    // preserving repeated Enter and preventing a later store refresh from
    // substituting another row. Synchronous settings IO stays in the callback.
    let selected_action = rows.get(selected_index.get()).map(|row| row.action.clone());
    let set_active_tab = props.set_active_tab.clone();
    let set_view_state = props.set_view_state.clone();
    let mark_plugins_changed = props.mark_plugins_changed.clone();
    use_keybindings(
        &mut hooks,
        runtime,
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
                    selected_index.set((selected_index.get() + 1).min(count.saturating_sub(1)));
                    true
                }),
            ),
            (
                "select:accept".into(),
                Box::new(move || {
                    if let Some(action) = selected_action.clone() {
                        match action {
                            ErrorRowAction::Navigate { tab, view_state } => {
                                (set_active_tab)(tab);
                                (set_view_state)(view_state);
                            }
                            ErrorRowAction::RemoveExtraMarketplace { name, sources } => {
                                remove_extra_marketplace(&name, &sources);
                                crate::utils::plugins::cache_utils::clear_all_caches();
                                store.replace_with(|state| {
                                    let plugins = std::sync::Arc::make_mut(&mut state.plugins);
                                    plugins.errors.retain(|e| {
                                        serde_json::to_value(e)
                                            .unwrap()
                                            .get("marketplace")
                                            .and_then(|v| v.as_str())
                                            != Some(name.as_str())
                                    });
                                    plugins
                                        .installation_status
                                        .marketplaces
                                        .retain(|m| m.name != name);
                                });
                                action_message.set(Some(format!(
                                    "✔ Removed \"{name}\" from {} settings",
                                    sources
                                        .iter()
                                        .map(|(_, s)| s.as_str())
                                        .collect::<Vec<_>>()
                                        .join(", ")
                                )));
                                (mark_plugins_changed)(());
                            }
                            ErrorRowAction::RemoveInstalledMarketplace { name } => {
                                let changed = mark_plugins_changed.clone();
                                crate::utils::process_runtime::runtime_handle_for_detached_work().expect("plugin runtime").spawn(async move{match crate::utils::plugins::marketplace_manager::remove_marketplace_source(&name).await{Ok(())=>{crate::utils::plugins::cache_utils::clear_all_caches();failures.update(|next|next.retain(|f|f.name!=name));action_message.set(Some(format!("✔ Removed marketplace \"{name}\"")));changed(());},Err(e)=>action_message.set(Some(format!("Failed to remove \"{name}\": {e}"))),}});
                            }
                            ErrorRowAction::ManagedOnly { .. } | ErrorRowAction::None => {}
                        }
                    }
                    true
                }),
            ),
        ],
        ContextName::Select,
        move || count > 0,
    );
    let clamped = selected_index.get().min(rows.len().saturating_sub(1));
    if clamped != selected_index.get() {
        selected_index.set(clamped);
    }
    let theme = *hooks.use_context::<crate::utils::theme::Theme>();
    let has_action = rows.get(clamped).is_some_and(|r| {
        !matches!(
            r.action,
            ErrorRowAction::None | ErrorRowAction::ManagedOnly { .. }
        )
    });
    if rows.is_empty() {
        return element!{View(flex_direction:FlexDirection::Column){View(margin_left:1u32){Text(content:"No plugin errors",dim:true)}View(margin_top:1u32){crate::components::configurable_shortcut_hint::ConfigurableShortcutHint(action:"confirm:no",context:"Confirmation",fallback:"Esc",description:"back",dim:true,italic:true)}}}.into_any();
    }
    element!{View(flex_direction:FlexDirection::Column){
        #(rows.into_iter().enumerate().map(|(idx,row)|element!{View(key:idx,margin_left:1u32,margin_bottom:1u32,flex_direction:FlexDirection::Column){
            View{Text(content:if idx==clamped{"❯ "}else{"✘ "},color:if idx==clamped{theme.suggestion}else{theme.error})Text(content:row.label,weight:if idx==clamped{Weight::Bold}else{Weight::Normal})#(row.scope.map(|scope|element!{Text(content:format!(" ({scope})"),dim:true)}))}
            View(margin_left:3u32){Text(content:row.message,color:theme.error)}#(row.guidance.filter(|s|!s.is_empty()).map(|guidance|element!{View(margin_left:3u32){Text(content:guidance,dim:true,italic:true)}}))
        }}))
        #(action_message.read().clone().filter(|s|!s.is_empty()).map(|message|element!{View(margin_top:1u32,margin_left:1u32){Text(content:message,color:theme.claude)}}))
        View(margin_top:1u32){crate::components::design_system::byline::Byline{
            crate::components::configurable_shortcut_hint::ConfigurableShortcutHint(action:"select:previous",context:"Select",fallback:"↑",description:"navigate",dim:true,italic:true)
            #(has_action.then(||element!{crate::components::configurable_shortcut_hint::ConfigurableShortcutHint(action:"select:accept",context:"Select",fallback:"Enter",description:"resolve",dim:true,italic:true)}))
            crate::components::configurable_shortcut_hint::ConfigurableShortcutHint(action:"confirm:no",context:"Confirmation",fallback:"Esc",description:"back",dim:true,italic:true)
        }}
    }}.into_any()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};
    #[derive(Default, Props)]
    struct WorkerStateProbeProps {
        handle: std::sync::Arc<std::sync::Mutex<Option<PluginUiState<usize>>>>,
    }
    #[component]
    fn WorkerStateProbe(
        props: &WorkerStateProbeProps,
        mut hooks: Hooks,
    ) -> impl Into<AnyElement<'static>> {
        let state = use_plugin_ui_state(&mut hooks, || 0usize);
        *props.handle.lock().unwrap() = Some(state);
        // Hold the same outer render borrow that makes iocraft State::set's
        // try_write fail. Native source payload mutation must still succeed.
        let borrowed_by_renderer = state.value.read();
        std::thread::spawn(move || state.set(42)).join().unwrap();
        assert_eq!(*borrowed_by_renderer.lock().unwrap(), 42);
        element! {Text(content: state.get().to_string())}
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn mounted_four_tabs_then_text_burst_matches_official_discover_lifecycle() {
        use super::super::{
            discover_plugins::tests::DiscoverKeybindingTestRoot,
            manage_plugins::ManagePluginsTestImports, plugin_details_helpers::PluginUiTestImports,
        };
        use crate::services::mcp::mcp_connection_manager::McpConnectionManager;
        use futures::StreamExt;
        use std::{
            sync::{Arc, Mutex},
            time::Duration,
        };
        crate::utils::process_runtime::initialize_test_process_runtime();
        let _env = TEST_ENV_LOCK.lock().unwrap();
        let home =
            std::env::temp_dir().join(format!("cometix-plugin-burst-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &home);
        for (paste, query) in [
            (false, "frontend"),
            (true, "frontend"),
            (true, "nf"),
            (true, "nothing"),
        ] {
            let fixture = PluginUiTestImports {
                plugins: vec![
                    serde_json::json!({"name":"Alpha","source":"./alpha","description":"first frontend nf nothing plugin"}),
                    serde_json::json!({"name":"Beta","source":"./beta","description":"second frontend nf nothing plugin"}),
                    serde_json::json!({"name":"Zulu","source":"./zulu","description":"last frontend nf nothing plugin"}),
                ],
                ..Default::default()
            };
            let installed = ManagePluginsTestImports {
                loaded: Default::default(),
            };
            let store = crate::state::store::AppStore::new(
                crate::state::app_state_store::AppState::default(),
                None,
            );
            let completed = Arc::new(Mutex::new(Vec::new()));
            let complete = completed.clone();
            let mut app = element! {ContextProvider(value:Context::owned(*crate::utils::theme::current())) {
                ContextProvider(value:Context::owned(fixture)) {ContextProvider(value:Context::owned(installed)) {
                    ContextProvider(value:Context::owned(store.clone())) {
                        DiscoverKeybindingTestRoot(bindings:crate::keybindings::default_bindings::default_bindings()) {
                            McpConnectionManager(app_store:Some(store)) {FocusScope(handle_keys:false) {
                                PluginSettings(on_complete:move |output| complete.lock().unwrap().push(output))
                            }}
                        }
                    }
                }}
            }};
            let (keys, input) = async_channel::unbounded();
            let mut frames = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(input).with_size(120, 45),
            ));
            let mut history = Vec::new();
            // Real parent, real four children, real focus/context registrations.
            // Backend imports are isolated; no worker is allowed to use user config.
            for expected in [
                "Discover plugins",
                "Manage plugins",
                "Manage marketplaces",
                "No plugin errors",
                "Discover plugins",
            ] {
                tokio::time::timeout(Duration::from_secs(3), async {
                    loop {
                        let frame = frames.next().await.unwrap().to_string();
                        let ready = frame.contains(expected) && !frame.contains("Loading");
                        history.push(frame);
                        if ready {
                            break;
                        }
                    }
                })
                .await
                .unwrap_or_else(|_| panic!("missing {expected}; frames={history:?}"));
                assert!(completed.lock().unwrap().is_empty(), "{history:?}");
                if history.last().unwrap().contains("Discover plugins")
                    && history.iter().any(|f| f.contains("No plugin errors"))
                {
                    break;
                }
                keys.send(TerminalEvent::Key(KeyEvent::new(
                    KeyEventKind::Press,
                    KeyCode::Tab,
                )))
                .await
                .unwrap();
            }
            // One backend burst, with no render between its character events.
            if paste {
                keys.send(TerminalEvent::Paste(query.into())).await.unwrap();
            } else {
                for c in query.chars() {
                    keys.send(TerminalEvent::Key(KeyEvent::new(
                        KeyEventKind::Press,
                        KeyCode::Char(c),
                    )))
                    .await
                    .unwrap();
                }
            }
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    let frame = frames.next().await.unwrap().to_string();
                    let ready = frame.contains(&format!("⌕ {query}"))
                        && frame.contains("Alpha")
                        && frame.contains("Beta")
                        && frame.contains("Zulu");
                    history.push(frame);
                    assert!(
                        completed.lock().unwrap().is_empty(),
                        "search cancelled: {history:?}"
                    );
                    if ready {
                        break;
                    }
                }
            })
            .await
            .unwrap_or_else(|_| panic!("text burst failed: {history:?}"));
            // Actual component oracle: Esc clears; Enter leaves search; a second
            // Enter enters details. This also catches a stale Errors cancel hook.
            for (key, expected) in [
                (KeyCode::Esc, "⌕ Search…"),
                (KeyCode::Enter, "❯ ◯ Alpha"),
                (KeyCode::Enter, "Plugin details"),
            ] {
                keys.send(TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, key)))
                    .await
                    .unwrap();
                tokio::time::timeout(Duration::from_secs(3), async {
                    loop {
                        let frame = frames.next().await.unwrap().to_string();
                        let ready = frame.contains(expected);
                        history.push(frame);
                        assert!(
                            completed.lock().unwrap().is_empty(),
                            "unexpected completion: {history:?}"
                        );
                        if ready {
                            break;
                        }
                    }
                })
                .await
                .unwrap_or_else(|_| panic!("missing {expected}: {history:?}"));
            }
        }
        std::fs::remove_dir_all(home).unwrap();
    }

    #[tokio::test]
    async fn errors_selection_matches_official_current_render_action_and_repeated_events() {
        use crate::keybindings::{
            keybinding_context::KeybindingRuntime,
            parser::{parse_chord, parse_keystroke},
            types::{ContextName, ParsedBinding},
        };
        use crate::types::plugin::PluginError;
        use futures::StreamExt;
        crate::utils::process_runtime::initialize_test_process_runtime();
        let error = |name: &str| PluginError::PluginNotFound {
            source: format!("{name}@market"),
            plugin_id: name.into(),
            marketplace: "market".into(),
        };
        let mut initial = crate::state::app_state_store::AppState::default();
        std::sync::Arc::make_mut(&mut initial.plugins).errors = vec![error("Alpha")];
        let store = crate::state::store::AppStore::new(initial, None);
        let runtime = KeybindingRuntime::new(vec![ParsedBinding {
            chord: parse_chord("ctrl+x ctrl+a"),
            action: Some("select:accept".into()),
            context: ContextName::Select,
        }]);
        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = calls.clone();
        let mut app = element! {ContextProvider(value:Context::owned(*crate::utils::theme::current())) {
            ContextProvider(value:Context::owned(store.clone())) {ContextProvider(value:Context::owned(runtime.clone())) {
                FocusScope(handle_keys:false) {ErrorsTabContent(set_view_state:move |view| {
                    if let ViewState::ManagePlugins {target_plugin,..} = view { sink.lock().unwrap().push(target_plugin); } else { panic!("unexpected view {view:?}"); }
                })}
            }}
        }};
        let mut frames = Box::pin(
            app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(futures::stream::pending::<TerminalEvent>())
                    .with_size(100, 30),
            ),
        );
        let first = tokio::time::timeout(std::time::Duration::from_secs(2), frames.next())
            .await
            .unwrap()
            .unwrap()
            .to_string();
        assert!(first.contains("Alpha"), "{first}");
        let accept = || {
            runtime.observe_keystroke(&parse_keystroke("ctrl+x"));
            runtime.observe_keystroke(&parse_keystroke("ctrl+a"));
        };
        // Invoke the registered production callback without polling another
        // frame: no boolean request may defer or coalesce these two actions.
        accept();
        store.replace_with(|state| {
            std::sync::Arc::make_mut(&mut state.plugins).errors = vec![error("Beta")]
        });
        accept();
        assert_eq!(
            *calls.lock().unwrap(),
            vec![Some("Alpha".into()), Some("Alpha".into())]
        );
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if frames.next().await.unwrap().to_string().contains("Beta") {
                    break;
                }
            }
        })
        .await
        .unwrap();
        accept();
        assert_eq!(
            *calls.lock().unwrap(),
            vec![
                Some("Alpha".into()),
                Some("Alpha".into()),
                Some("Beta".into())
            ]
        );
    }

    #[test]
    fn native_worker_state_matches_official_setter_and_unmount_semantics() {
        let handle = std::sync::Arc::new(std::sync::Mutex::new(None));
        {
            let mut app = element! {WorkerStateProbe(handle:handle.clone())};
            assert_eq!(app.render(Some(20)).to_string().trim(), "42");
        }
        let state = handle.lock().unwrap().unwrap();
        assert!(state.value.try_read().is_none());
        // The async operation continues its external effect and parent
        // callback even though the child's late setter is now a no-op.
        let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let worker_events = events.clone();
        std::thread::spawn(move || {
            state.set(99);
            state.update(|_| panic!("unmounted source setter must be ignored"));
            worker_events.lock().unwrap().push("side effect");
            let callback = || worker_events.lock().unwrap().push("parent callback");
            callback();
        })
        .join()
        .unwrap();
        assert_eq!(*events.lock().unwrap(), ["side effect", "parent callback"]);
    }

    #[test]
    fn error_rows_and_navigation_matches_official_bun() {
        use crate::types::plugin::PluginError;
        use serde_json::{Value, json};
        let oracle:Value=serde_json::from_str(include_str!("../../../tests/fixtures/oracles/plugin-ui-complete-0914/errors-oracle.json")).unwrap();
        let failed = oracle["failed"]
            .as_array()
            .unwrap()
            .iter()
            .map(
                |v| crate::state::app_state_store::MarketplaceInstallationStatus {
                    name: v["name"].as_str().unwrap().into(),
                    status: "failed".into(),
                    error: v["error"].as_str().map(str::to_string),
                },
            )
            .collect::<Vec<_>>();
        let parse =
            |name: &str| serde_json::from_value::<Vec<PluginError>>(oracle[name].clone()).unwrap();
        let (extra, plugins, other, transient) = (
            parse("extra"),
            parse("plugins"),
            parse("other"),
            parse("transient"),
        );
        let broken = oracle["broken"]
            .as_array()
            .unwrap()
            .iter()
            .map(
                |v| crate::utils::plugins::marketplace_helpers::MarketplaceLoadingFailure {
                    name: v["name"].as_str().unwrap().into(),
                    error: v["error"].as_str().unwrap().into(),
                },
            )
            .collect::<Vec<_>>();
        let scopes = oracle["scopes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| (v[0].as_str().unwrap().into(), v[1].as_str().unwrap().into()))
            .collect();
        let rows = build_error_rows(
            &failed, &extra, &plugins, &other, &broken, &transient, &scopes,
        );
        let actual=rows.into_iter().map(|row|{
            let action=match row.action {
                ErrorRowAction::Navigate{tab,view_state}=>{let view=match view_state{ViewState::ManagePlugins{target_plugin,action,..}=>json!({"type":"manage-plugins","targetPlugin":target_plugin,"action":action}),ViewState::ManageMarketplaces{target_marketplace,action}=>json!({"type":"manage-marketplaces","targetMarketplace":target_marketplace,"action":action}),other=>panic!("unexpected navigation {other:?}")};json!({"kind":"navigate","tab":tab,"viewState":view})},
                ErrorRowAction::RemoveInstalledMarketplace{name}=>json!({"kind":"remove-installed-marketplace","name":name}),
                ErrorRowAction::None=>json!({"kind":"none"}),other=>panic!("unexpected settings source {other:?}")
            };
            let mut value=json!({"label":row.label,"message":row.message,"action":action});
            if let Some(guidance)=row.guidance{value["guidance"]=json!(guidance);}if let Some(scope)=row.scope{value["scope"]=json!(scope);}value
        }).collect::<Vec<_>>();
        assert_eq!(json!(actual), oracle["rows"]);
        let errors = plugins
            .iter()
            .chain(other.iter())
            .chain(transient.iter())
            .collect::<Vec<_>>();
        assert_eq!(
            json!(
                errors
                    .iter()
                    .map(|error| get_plugin_name_from_error(error))
                    .collect::<Vec<_>>()
            ),
            oracle["names"]
        );
        assert_eq!(
            json!(
                errors
                    .iter()
                    .map(|error| is_transient_error(error))
                    .collect::<Vec<_>>()
            ),
            oracle["transientFlags"]
        );
    }

    #[test]
    fn initial_views_match_official_plugin_settings_routing() {
        for (input, tab) in [
            ("validate", "discover"),
            ("manage", "installed"),
            ("marketplace remove", "marketplaces"),
            ("", "discover"),
        ] {
            assert_eq!(
                get_initial_tab(&get_initial_view_state(parse_plugin_args(Some(input)))),
                tab
            );
        }
        assert_eq!(
            get_initial_view_state(parse_plugin_args(Some("validate a b"))),
            ViewState::Validate {
                path: Some("a b".into())
            }
        );
        assert_eq!(
            get_initial_view_state(parse_plugin_args(Some("marketplace add"))),
            ViewState::AddMarketplace {
                initial_value: Some(String::new())
            }
        );
        assert_eq!(
            get_initial_view_state(parse_plugin_args(Some("install p@"))),
            ViewState::DiscoverPlugins {
                target_plugin: Some("p".into())
            }
        );
    }

    #[tokio::test]
    async fn marketplace_list_outputs_match_mounted_official_bun() {
        use std::sync::Arc;
        // Captured from actual PluginSettings MarketplaceList under Bun:
        // research/proof/plugin-list-help-ui-0914/bun-oracle.json. The fixture
        // replaces only its imported loader, never its formatter or callback.
        let cases = [
            (Ok(serde_json::json!({})), "No marketplaces configured"),
            (
                Ok(serde_json::from_str(r#"{"z":{},"10":{},"2":{},"a":{}}"#).unwrap()),
                "Configured marketplaces:\n  • 2\n  • 10\n  • z\n  • a",
            ),
            (
                Err("fixture load error"),
                "Error loading marketplaces: fixture load error",
            ),
        ];
        for (answer, expected) in cases {
            let fixture = MarketplaceConfigFixture(Arc::new(move || {
                let answer = answer.clone();
                Box::pin(async move {
                    answer
                        .map(|v| v.as_object().unwrap().clone())
                        .map_err(anyhow::Error::msg)
                })
            }));
            let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
            let callback = Handler::from({
                let observed = observed.clone();
                move |output| observed.lock().unwrap().push(output)
            });
            load_list(callback, Some(fixture)).await;
            assert_eq!(*observed.lock().unwrap(), vec![Some(expected.to_string())]);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn mounted_help_completes_without_payload_and_preserves_original_text_styles() {
        use futures::StreamExt;
        use std::sync::{
            Arc,
            atomic::{AtomicI32, Ordering},
        };
        use std::time::Duration;
        crate::utils::process_runtime::initialize_test_process_runtime();
        for args in ["help", "-h", "--help"] {
            let code = Arc::new(AtomicI32::new(9));
            let (complete, completed) = async_channel::unbounded();
            let callback = Handler::from(move |result| {
                complete.try_send(result).unwrap();
            });
            let fixture = MarketplaceConfigFixture(Arc::new(|| {
                panic!("source help must not load marketplaces")
            }));
            let runtime =
                crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings();
            let store = crate::state::store::AppStore::new(
                crate::state::app_state_store::AppState::default(),
                None,
            );
            let mut app = element! {
                ContextProvider(value: Context::owned(code.clone())) {
                    ContextProvider(value: Context::owned(fixture)) {
                        ContextProvider(value: Context::owned(store)) {
                            ContextProvider(value: Context::owned(runtime)) {
                                FocusScope(handle_keys: false) {
                                    PluginSettings(args: Some(args.to_string()), on_complete: callback)
                                }
                            }
                        }
                    }
                }
            };
            let mut frames = Box::pin(
                app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(futures::stream::pending::<TerminalEvent>())
                        .with_size(100, 45),
                ),
            );
            let first = tokio::time::timeout(Duration::from_secs(2), frames.next())
                .await
                .unwrap()
                .unwrap();
            let lines = first
                .to_string()
                .lines()
                .map(str::trim_end)
                .collect::<Vec<_>>()
                .join("\n");
            // Literal text observed in original Bun's real Ink frame; direct
            // Text retains angle brackets, unlike markdown completion output.
            let expected = concat!(
                "Plugin Command Usage:\n\n",
                "Installation:\n",
                " /plugin install - Browse and install plugins\n",
                " /plugin install <marketplace> - Install from specific marketplace\n",
                " /plugin install <plugin> - Install specific plugin\n",
                " /plugin install <plugin>@<market> - Install plugin from marketplace\n\n",
                "Management:\n",
                " /plugin manage - Manage installed plugins\n",
                " /plugin enable <plugin> - Enable a plugin\n",
                " /plugin disable <plugin> - Disable a plugin\n",
                " /plugin uninstall <plugin> - Uninstall a plugin\n\n",
                "Marketplaces:\n",
                " /plugin marketplace - Marketplace management menu\n",
                " /plugin marketplace add - Add a marketplace\n",
                " /plugin marketplace add <path/url> - Add marketplace directly\n",
                " /plugin marketplace update - Update marketplaces\n",
                " /plugin marketplace update <name> - Update specific marketplace\n",
                " /plugin marketplace remove - Remove a marketplace\n",
                " /plugin marketplace remove <name> - Remove specific marketplace\n",
                " /plugin marketplace list - List all marketplaces\n\n",
                "Validation:\n",
                " /plugin validate <path> - Validate a manifest file or directory\n\n",
                "Other:\n",
                " /plugin - Main plugin menu\n",
                " /plugin help - Show this help\n",
                " /plugins - Alias for /plugin",
            );
            assert_eq!(lines.trim_end(), expected);
            assert_eq!(
                first.resolved_text_style(0, 0).unwrap().weight,
                Weight::Bold
            );
            for (y, line) in expected.lines().enumerate() {
                if [
                    "Installation:",
                    "Management:",
                    "Marketplaces:",
                    "Validation:",
                    "Other:",
                ]
                .contains(&line)
                {
                    assert!(first.resolved_text_style(0, y).unwrap().dim);
                }
            }
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(2), completed.recv())
                    .await
                    .unwrap()
                    .unwrap(),
                None
            );
            assert!(completed.try_recv().is_err());
            assert_eq!(code.load(Ordering::SeqCst), 9);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn mounted_marketplace_list_ignores_escape_and_completes_after_unmount() {
        use futures::StreamExt;
        use std::sync::{
            Arc,
            atomic::{AtomicI32, Ordering},
        };
        use std::time::Duration;
        crate::utils::process_runtime::initialize_test_process_runtime();
        let (release, pending) = tokio::sync::oneshot::channel();
        let pending = Arc::new(std::sync::Mutex::new(Some(pending)));
        let (started, starting) = async_channel::bounded(1);
        let fixture = MarketplaceConfigFixture(Arc::new(move || {
            let pending = pending
                .lock()
                .unwrap()
                .take()
                .expect("one effect per callback");
            let started = started.clone();
            Box::pin(async move {
                started.send(()).await.unwrap();
                pending.await.unwrap()
            })
        }));
        let code = Arc::new(AtomicI32::new(9));
        let (complete, completed) = async_channel::bounded(1);
        let callback = Handler::from(move |result| {
            complete.try_send(result).unwrap();
        });
        {
            let runtime =
                crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings();
            let store = crate::state::store::AppStore::new(
                crate::state::app_state_store::AppState::default(),
                None,
            );
            let mut app = element! {
                ContextProvider(value: Context::owned(code.clone())) {
                    ContextProvider(value: Context::owned(fixture)) {
                        ContextProvider(value: Context::owned(store)) {
                            ContextProvider(value: Context::owned(runtime)) {
                                FocusScope(handle_keys: false) {
                                    PluginSettings(args: Some("marketplace list".to_string()), on_complete: callback)
                                }
                            }
                        }
                    }
                }
            };
            let (keys, events) = async_channel::unbounded();
            let mut frames = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(80, 5),
            ));
            let first = tokio::time::timeout(Duration::from_secs(2), frames.next())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(first.to_string().trim(), "Loading marketplaces...");
            tokio::time::timeout(Duration::from_secs(2), starting.recv())
                .await
                .unwrap()
                .unwrap();
            keys.send(TerminalEvent::Key(KeyEvent::new(
                KeyEventKind::Press,
                KeyCode::Esc,
            )))
            .await
            .unwrap();
            keys.send(TerminalEvent::Resize(79, 5)).await.unwrap();
            let after = tokio::time::timeout(Duration::from_secs(2), frames.next())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(after.to_string().trim(), "Loading marketplaces...");
            assert!(completed.try_recv().is_err());
            assert_eq!(code.load(Ordering::SeqCst), 9);
            drop(frames);
            drop(app);
        }
        release
            .send(Ok(serde_json::json!({"late":{}})
                .as_object()
                .unwrap()
                .clone()))
            .unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), completed.recv())
                .await
                .unwrap()
                .unwrap(),
            Some("Configured marketplaces:\n  • late".to_string())
        );
        assert_eq!(code.load(Ordering::SeqCst), 9);
    }

    #[derive(Clone, Default)]
    struct CallbackSwitch(std::sync::Arc<std::sync::Mutex<Option<State<bool>>>>);

    #[derive(Default, Props)]
    struct CallbackSwitchHarnessProps {
        old: Handler<Option<String>>,
        new: Handler<Option<String>>,
    }

    #[component]
    fn CallbackSwitchHarness(
        props: &mut CallbackSwitchHarnessProps,
        mut hooks: Hooks,
    ) -> impl Into<AnyElement<'static>> {
        let use_new = hooks.use_state(|| false);
        *hooks.use_context::<CallbackSwitch>().0.lock().unwrap() = Some(use_new);
        element! { PluginSettings(args: Some("marketplace list".to_string()), on_complete: if *use_new.read() { props.new.clone() } else { props.old.clone() }) }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn marketplace_callback_identity_restarts_load_and_keeps_both_late_completions() {
        use futures::StreamExt;
        use std::sync::Arc;
        use std::time::Duration;
        crate::utils::process_runtime::initialize_test_process_runtime();
        let (start, started) = async_channel::unbounded();
        let fixture = MarketplaceConfigFixture(Arc::new(move || {
            let (release, pending) = tokio::sync::oneshot::channel();
            let start = start.clone();
            Box::pin(async move {
                start.send(release).await.unwrap();
                pending.await.unwrap()
            })
        }));
        let (complete, completed) = async_channel::unbounded();
        let callback = |name| {
            Handler::from({
                let complete = complete.clone();
                move |output| {
                    complete.try_send((name, output)).unwrap();
                }
            })
        };
        let control = CallbackSwitch::default();
        let runtime =
            crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings();
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        let (old, new);
        {
            let mut app = element! {
                ContextProvider(value: Context::owned(control.clone())) {
                    ContextProvider(value: Context::owned(fixture)) {
                        ContextProvider(value: Context::owned(store)) {
                            ContextProvider(value: Context::owned(runtime)) {
                                FocusScope(handle_keys: false) {
                                    CallbackSwitchHarness(old: callback("old"), new: callback("new"))
                                }
                            }
                        }
                    }
                }
            };
            let mut frames = Box::pin(
                app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(futures::stream::pending::<TerminalEvent>())
                        .with_size(80, 5),
                ),
            );
            tokio::time::timeout(Duration::from_secs(2), frames.next())
                .await
                .unwrap()
                .unwrap();
            old = tokio::time::timeout(Duration::from_secs(2), started.recv())
                .await
                .unwrap()
                .unwrap();
            control.0.lock().unwrap().as_mut().unwrap().set(true);
            // Commit the new callback through the live render loop, but use
            // the real loader start as the effect barrier. iocraft render.rs
            // skips terminal writes for an unchanged canvas; mock output only
            // yields written canvases, so this identical loading view need not
            // emit a second frame. Source MarketplaceList likewise requires
            // another load on callback change, not another visible paint.
            new = tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    tokio::select! {
                        frame = frames.next() => {
                            assert!(frame.is_some(), "live owner exited before second effect");
                        }
                        result = started.recv() => break result.unwrap(),
                    }
                }
            })
            .await
            .expect("new callback must start a second real marketplace load");
            assert!(completed.try_recv().is_err());
            drop(frames);
            drop(app);
        }
        // Exact source Bun callback-change-and-unmount order: neither effect
        // has cleanup or a latest-wins guard. No State is read after unmount.
        new.send(Ok(serde_json::json!({"new":{}})
            .as_object()
            .unwrap()
            .clone()))
            .unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), completed.recv())
                .await
                .unwrap()
                .unwrap(),
            ("new", Some("Configured marketplaces:\n  • new".into()))
        );
        old.send(Ok(serde_json::json!({"old":{}})
            .as_object()
            .unwrap()
            .clone()))
            .unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), completed.recv())
                .await
                .unwrap()
                .unwrap(),
            ("old", Some("Configured marketplaces:\n  • old".into()))
        );
    }
}
