//! Maps to: CC commands/plugin/ManagePlugins.tsx.
use super::{
    plugin_errors::*,
    plugin_options_dialog::PluginOptionsDialog,
    plugin_options_flow::PluginOptionsFlow,
    plugin_settings::ViewState as ParentViewState,
    unified_installed_cell::UnifiedInstalledCell,
    unified_types::{UnifiedInstalledItem, UnifiedInstalledKind},
    use_pagination::{UsePaginationOptions, use_pagination},
};
use crate::components::mcp::types::ServerInfo;
use crate::keybindings::{
    types::ContextName,
    use_keybinding::{KeybindingHandlers, use_keybinding, use_keybindings},
};
use crate::services::plugins::plugin_operations::*;
use crate::types::plugin::{LoadedPlugin, PluginError};
use crate::utils::plugins::{
    installed_plugins_manager::*, marketplace_manager::*, plugin_flagging::*, plugin_loader::*,
    plugin_options_storage as options, plugin_policy::*, schemas::PluginScope,
};
use iocraft::prelude::*;
use serde_json::{Map, Value};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
};
/// Maps to: CC ManagePlugins.tsx:87-96#Props.
#[derive(Default, Props)]
pub struct ManagePluginsProps {
    pub set_view_state: Handler<ParentViewState>,
    pub set_result: Handler<Option<String>>,
    pub on_manage_complete: Handler<()>,
    pub on_search_mode_change: Handler<bool>,
    pub target_plugin: Option<String>,
    pub target_marketplace: Option<String>,
    pub action: Option<String>,
}
#[cfg(test)]
#[derive(Clone)]
pub(super) struct ManagePluginsTestImports {
    pub(super) loaded: crate::types::plugin::PluginLoadResult,
}
/// Maps to: CC ManagePlugins.tsx#PluginState.
#[derive(Clone, Debug)]
pub struct PluginState {
    pub plugin: LoadedPlugin,
    pub marketplace: String,
    pub scope: String,
    pub pending_enable: Option<bool>,
    pub pending_update: bool,
}
/// Maps to: CC ManagePlugins.tsx#ViewState. ServerInfo retains the existing MCP
/// child component's native projection of the captured source client and tools.
#[derive(Clone)]
enum ViewState {
    PluginList,
    PluginDetails,
    PluginOptions,
    ConfiguringOptions(Value),
    Configuring(crate::utils::plugins::mcpb_handler::McpbNeedsConfigResult),
    ConfirmProjectUninstall,
    ConfirmDataCleanup(String),
    FlaggedDetail(UnifiedInstalledItem),
    FailedPluginDetails(UnifiedInstalledItem),
    McpDetail(ServerInfo),
    McpTools(ServerInfo),
    McpToolDetail(ServerInfo, usize),
}
/// Native aggregate of the component's source useState slots, not a new policy owner.
#[derive(Clone)]
struct ManageState {
    view: ViewState,
    plugins: Vec<PluginState>,
    marketplaces: Vec<String>,
    loading: bool,
    selected: Option<PluginState>,
    selected_index: usize,
    details_index: usize,
    pending: HashMap<String, String>,
    processing: bool,
    error: Option<String>,
    selected_has_mcpb: bool,
    auto_navigated: bool,
}
impl Default for ManageState {
    fn default() -> Self {
        Self {
            view: ViewState::PluginList,
            plugins: vec![],
            marketplaces: vec![],
            loading: true,
            selected: None,
            selected_index: 0,
            details_index: 0,
            pending: HashMap::new(),
            processing: false,
            error: None,
            selected_has_mcpb: false,
            auto_navigated: false,
        }
    }
}
// Native mailbox for source useState setters. Async continuations retain the
// state value and notify the retained UI future; only that future writes an
// iocraft State. A dropped view does not cancel an already started operation.
#[derive(Clone)]
struct ManageStateHandle {
    value: std::sync::Arc<std::sync::Mutex<ManageState>>,
    notify: tokio::sync::mpsc::UnboundedSender<()>,
}
impl ManageStateHandle {
    fn read(&self) -> std::sync::MutexGuard<'_, ManageState> {
        self.value.lock().unwrap()
    }
}
fn change(state: &ManageStateHandle, f: impl FnOnce(&mut ManageState)) {
    {
        let mut value = state.value.lock().unwrap();
        f(&mut value);
    }
    let _ = state.notify.send(());
}
fn launch(future: impl std::future::Future<Output = ()> + Send + 'static) {
    crate::utils::process_runtime::runtime_handle_for_detached_work()
        .expect("plugin runtime")
        .spawn(future);
}
/// Maps to: CC ManagePlugins.tsx:166-192#getBaseFileNames.
async fn get_base_file_names(path: &Path) -> Vec<String> {
    let result = async {
        let mut entries = tokio::fs::read_dir(path).await?;
        let mut names = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.file_type().await?.is_file() && name.ends_with(".md") {
                names.push(name[..name.len() - 3].into());
            }
        }
        names.sort_by(|a: &String, b| a.encode_utf16().cmp(b.encode_utf16()));
        Ok::<_, std::io::Error>(names)
    }
    .await;
    result.unwrap_or_else(|error| {
        crate::utils::log::log_error(crate::utils::log::LogError::new(error.to_string()));
        vec![]
    })
}
/// Maps to: CC ManagePlugins.tsx:195-230#getSkillDirNames.
async fn get_skill_dir_names(path: &Path) -> Vec<String> {
    let result = async {
        let mut entries = tokio::fs::read_dir(path).await?;
        let mut names = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            let kind = entry.file_type().await?;
            if (kind.is_dir() || kind.is_symlink())
                && tokio::fs::metadata(entry.path().join("SKILL.md"))
                    .await
                    .is_ok_and(|m| m.is_file())
            {
                names.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
        names.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
        Ok::<_, std::io::Error>(names)
    }
    .await;
    result.unwrap_or_else(|error| {
        crate::utils::log::log_error(crate::utils::log::LogError::new(error.to_string()));
        vec![]
    })
}
/// Maps to: CC ManagePlugins.tsx:481-494#checkIfLocalPlugin.
async fn check_if_local_plugin(name: &str, marketplace: &str) -> anyhow::Result<Option<String>> {
    let catalog = get_marketplace(marketplace).await?;
    Ok(catalog["plugins"].as_array().and_then(|entries|entries.iter().find(|e|e["name"].as_str()==Some(name))).and_then(|entry|entry["source"].as_str()).map(|source|format!("Local plugins cannot be updated remotely. To update, modify the source at: {source}")))
}
/// Maps to: CC ManagePlugins.tsx:503-511#filterManagedDisabledPlugins.
pub fn filter_managed_disabled_plugins(plugins: Vec<LoadedPlugin>) -> Vec<LoadedPlugin> {
    plugins
        .into_iter()
        .filter(|plugin| {
            let marketplace = plugin
                .source
                .split('@')
                .nth(1)
                .filter(|s| !s.is_empty())
                .unwrap_or("local");
            !is_plugin_blocked_by_policy(&format!("{}@{marketplace}", plugin.name))
        })
        .collect()
}
// Native projection of the source scope union. Non-installation scope labels
// must not silently become an editable user-scope installation.
fn scope_from_text(scope: &str) -> Option<PluginScope> {
    serde_json::from_value(Value::String(scope.into())).ok()
}
// Native input-payload projection of CC ManagePlugins.tsx:1916-1940's
// anonymous useInput callback. Keys and pasted strings use the same predicate.
fn search_entry_query(input: &str) -> Option<String> {
    static WHITESPACE: std::sync::LazyLock<regress::Regex> =
        std::sync::LazyLock::new(|| regress::Regex::new(r"^\s+$").expect("source regex"));
    if input == "/" {
        Some(String::new())
    } else if !input.is_empty()
        && WHITESPACE.find(input).is_none()
        && !matches!(input, "j" | "k" | " ")
    {
        Some(input.to_owned())
    } else {
        None
    }
}
fn is_enabled(id: &str) -> bool {
    crate::utils::settings::get_initial_settings()
        .enabled_plugins
        .is_none_or(|v| v[id] != Value::Bool(false))
}
fn plugin_id(state: &PluginState) -> String {
    format!("{}@{}", state.plugin.name, state.marketplace)
}
fn error_plugin(error: &PluginError) -> Option<String> {
    serde_json::to_value(error)
        .ok()
        .and_then(|v| v["plugin"].as_str().map(str::to_owned))
}
/// Maps to: CC ManagePlugins.tsx:661-965#unifiedItems useMemo closure.
fn unified_items(
    states: &[PluginState],
    clients: &[ServerInfo],
    errors: &[PluginError],
    pending: &HashMap<String, String>,
    flagged: &FlaggedPlugins,
) -> Vec<UnifiedInstalledItem> {
    let mut by_scope = indexmap::IndexMap::<String, Vec<UnifiedInstalledItem>>::new();
    let mut ids = HashSet::new();
    let mut names = HashSet::new();
    for state in states {
        let id = plugin_id(state);
        ids.insert(id.clone());
        names.insert(state.plugin.name.clone());
        let scope = if state.plugin.is_builtin {
            "builtin"
        } else {
            &state.scope
        };
        let matching = errors
            .iter()
            .filter(|e| {
                error_plugin(e).as_deref() == Some(&state.plugin.name)
                    || e.source() == id
                    || e.source().starts_with(&format!("{}@", state.plugin.name))
            })
            .cloned()
            .collect();
        by_scope
            .entry(scope.into())
            .or_default()
            .push(UnifiedInstalledItem {
                id: id.clone(),
                name: state.plugin.name.clone(),
                description: state.plugin.manifest.description.clone(),
                marketplace: Some(state.marketplace.clone()),
                scope: scope.into(),
                kind: UnifiedInstalledKind::Plugin {
                    plugin: state.plugin.clone(),
                    is_enabled: is_enabled(&id),
                    errors: matching,
                    pending_enable: state.pending_enable,
                    pending_update: state.pending_update,
                    pending_toggle: pending.get(&id).cloned(),
                },
            });
        for client in clients {
            let parts = client.name.split(':').collect::<Vec<_>>();
            if parts.len() >= 3 && parts[0] == "plugin" && parts[1] == state.plugin.name {
                let scope = if scope == "builtin" { "user" } else { scope };
                by_scope
                    .entry(scope.into())
                    .or_default()
                    .push(UnifiedInstalledItem {
                        id: format!("mcp:{}", client.name),
                        name: parts[2..].join(":"),
                        description: None,
                        marketplace: None,
                        scope: scope.into(),
                        kind: UnifiedInstalledKind::Mcp {
                            client: client.clone(),
                            indented: true,
                        },
                    });
            }
        }
    }
    for client in clients {
        if client.name == "ide" || client.name.starts_with("plugin:") {
            continue;
        }
        let scope = client.scope.as_str().to_owned();
        by_scope
            .entry(scope.clone())
            .or_default()
            .push(UnifiedInstalledItem {
                id: format!("mcp:{}", client.name),
                name: client.name.clone(),
                description: None,
                marketplace: None,
                scope,
                kind: UnifiedInstalledKind::Mcp {
                    client: client.clone(),
                    indented: false,
                },
            });
    }
    let mut orphan = indexmap::IndexMap::<String, Vec<PluginError>>::new();
    for error in errors {
        if ids.contains(error.source()) || error_plugin(error).is_some_and(|n| names.contains(&n)) {
            continue;
        }
        orphan
            .entry(error.source().into())
            .or_default()
            .push(error.clone());
    }
    let scopes = crate::utils::plugins::plugin_startup_check::get_plugin_editable_scopes();
    for (id, errors) in orphan {
        if flagged.contains_key(&id) {
            continue;
        }
        let parsed = crate::utils::plugins::plugin_identifier::parse_plugin_identifier(&id);
        let scope = scopes
            .get(&id)
            .filter(|s| s.as_str() != "flag")
            .cloned()
            .unwrap_or_else(|| "user".into());
        by_scope
            .entry(scope.clone())
            .or_default()
            .push(UnifiedInstalledItem {
                id: id.clone(),
                name: if parsed.name.is_empty() {
                    id
                } else {
                    parsed.name
                },
                description: None,
                marketplace: Some(
                    parsed
                        .marketplace
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| "unknown".into()),
                ),
                scope,
                kind: UnifiedInstalledKind::FailedPlugin { errors },
            });
    }
    for (id, flag) in flagged {
        let parsed = crate::utils::plugins::plugin_identifier::parse_plugin_identifier(id);
        by_scope
            .entry("flagged".into())
            .or_default()
            .push(UnifiedInstalledItem {
                id: id.clone(),
                name: if parsed.name.is_empty() {
                    id.clone()
                } else {
                    parsed.name
                },
                description: None,
                marketplace: Some(
                    parsed
                        .marketplace
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| "unknown".into()),
                ),
                scope: "flagged".into(),
                kind: UnifiedInstalledKind::FlaggedPlugin {
                    reason: "delisted".into(),
                    text: "Removed from marketplace".into(),
                    flagged_at: flag.flagged_at.clone(),
                },
            });
    }
    let rank = |s: &str| match s {
        "flagged" => -1,
        "project" => 0,
        "local" => 1,
        "user" => 2,
        "enterprise" => 3,
        "managed" => 4,
        "dynamic" => 5,
        "builtin" => 6,
        _ => 99,
    };
    let mut scopes = by_scope.into_iter().collect::<Vec<_>>();
    scopes.sort_by_key(|(scope, _)| rank(scope));
    let mut result = Vec::new();
    for (_, items) in scopes {
        let mut groups = Vec::<Vec<UnifiedInstalledItem>>::new();
        let mut standalone = Vec::new();
        let mut iter = items.into_iter().peekable();
        while let Some(item) = iter.next() {
            match item.kind {
                UnifiedInstalledKind::Mcp {
                    indented: false, ..
                } => standalone.push(item),
                UnifiedInstalledKind::Mcp { indented: true, .. } => {}
                _ => {
                    let mut group = vec![item];
                    while iter.peek().is_some_and(|n| {
                        matches!(n.kind, UnifiedInstalledKind::Mcp { indented: true, .. })
                    }) {
                        group.push(iter.next().unwrap());
                    }
                    groups.push(group);
                }
            }
        }
        groups.sort_by(|a, b| {
            crate::tools::grep_tool::javascript_locale_compare(&a[0].name, &b[0].name)
        });
        standalone
            .sort_by(|a, b| crate::tools::grep_tool::javascript_locale_compare(&a.name, &b.name));
        result.extend(groups.into_iter().flatten());
        result.extend(standalone);
    }
    result
}
/// Maps to: CC ManagePlugins.tsx:1190-1380#handleSingleOperation.
async fn handle_single_operation(
    operation: &str,
    state: ManageStateHandle,
    on_result: Handler<Option<String>>,
    on_complete: Handler<()>,
    set_parent: Handler<ParentViewState>,
) {
    let Some(selected) = state.read().selected.clone() else {
        return;
    };
    let builtin = selected.scope == "builtin";
    let scope = scope_from_text(&selected.scope);
    if builtin && matches!(operation, "update" | "uninstall") {
        change(&state, |s| {
            s.error = Some("Built-in plugins cannot be updated or uninstalled.".into())
        });
        return;
    }
    if !builtin && !scope.is_some_and(is_installable_scope) && operation != "update" {
        change(&state, |s| {
            s.error = Some(
                "This plugin is managed by your organization. Contact your admin to disable it."
                    .into(),
            )
        });
        return;
    }
    change(&state, |s| {
        s.processing = true;
        s.error = None;
    });
    let id = plugin_id(&selected);
    let result = async {
        let mut deps = None;
        match operation {
            "enable" => {
                let result = enable_plugin_op(&id, None).await?;
                if !result.success {
                    anyhow::bail!("{}", result.message);
                }
            }
            "disable" => {
                let result = disable_plugin_op(&id, None).await?;
                if !result.success {
                    anyhow::bail!("{}", result.message);
                }
                deps = result.reverse_dependents;
            }
            "uninstall" => {
                if is_plugin_enabled_at_project_scope(&id) {
                    change(&state, |s| {
                        s.processing = false;
                        s.view = ViewState::ConfirmProjectUninstall;
                    });
                    return Ok(());
                }
                let is_last = load_installed_plugins_v2().lock().unwrap()["plugins"][&id]
                    .as_array()
                    .is_none_or(|i| i.len() <= 1);
                if is_last {
                    if let Some(size) =
                        crate::utils::plugins::plugin_directories::get_plugin_data_dir_size(&id)
                            .await?
                    {
                        change(&state, |s| {
                            s.processing = false;
                            s.view = ViewState::ConfirmDataCleanup(size.human);
                        });
                        return Ok(());
                    }
                }
                let result = uninstall_plugin_op(
                    &id,
                    scope.expect("uninstall scope was checked above"),
                    true,
                )
                .await?;
                if !result.success {
                    anyhow::bail!("{}", result.message);
                }
                deps = result.reverse_dependents;
            }
            "update" => {
                let result = update_plugin_op(
                    &id,
                    scope.ok_or_else(|| {
                        anyhow::anyhow!("Invalid plugin installation scope: {}", selected.scope)
                    })?,
                )
                .await?;
                if !result.success {
                    anyhow::bail!("{}", result.message);
                }
                if result.already_up_to_date == Some(true) {
                    on_result(Some(format!(
                        "{} is already at the latest version ({}).",
                        selected.plugin.name,
                        result.new_version.as_deref().unwrap_or("undefined")
                    )));
                    on_complete(());
                    set_parent(ParentViewState::Menu);
                    return Ok(());
                }
            }
            _ => unreachable!(),
        };
        crate::utils::plugins::cache_utils::clear_all_caches();
        if is_enabled(&id) {
            change(&state, |s| {
                s.processing = false;
                s.view = ViewState::PluginOptions;
            });
            return Ok(());
        }
        let op = match operation {
            "enable" => "Enabled",
            "disable" => "Disabled",
            "update" => "Updated",
            _ => "Uninstalled",
        };
        let suffix = deps
            .filter(|d| !d.is_empty())
            .map(|d| format!(" · required by {}", d.join(", ")))
            .unwrap_or_default();
        on_result(Some(format!(
            "✓ {op} {}{suffix}. Run /reload-plugins to apply.",
            selected.plugin.name
        )));
        on_complete(());
        set_parent(ParentViewState::Menu);
        Ok::<_, anyhow::Error>(())
    }
    .await;
    if let Err(error) = result {
        change(&state, |s| {
            s.processing = false;
            s.error = Some(format!("Failed to {operation}: {error}"));
        });
        crate::utils::log::log_error(crate::utils::log::LogError::new(error.to_string()));
    }
}
#[derive(Clone)]
enum DetailAction {
    Operation(&'static str),
    MarkUpdate,
    ConfigureMcpb,
    ConfigureOptions(Value),
    Open(String),
    Back,
}
/// Maps to: CC ManagePlugins.tsx:1512-1700#detailsMenuItems useMemo.
fn details_menu(selected: &PluginState, has_mcpb: bool) -> Vec<(String, DetailAction)> {
    let mut menu = vec![(
        if is_enabled(&plugin_id(selected)) {
            "Disable plugin".into()
        } else {
            "Enable plugin".into()
        },
        DetailAction::Operation(if is_enabled(&plugin_id(selected)) {
            "disable"
        } else {
            "enable"
        }),
    )];
    if selected.marketplace != "builtin" {
        menu.push((
            if selected.pending_update {
                "Unmark for update".into()
            } else {
                "Mark for update".into()
            },
            DetailAction::MarkUpdate,
        ));
        if has_mcpb {
            menu.push(("Configure".into(), DetailAction::ConfigureMcpb));
        }
        if let Some(schema) = selected
            .plugin
            .manifest
            .user_config
            .as_ref()
            .filter(|v| v.as_object().is_some_and(|m| !m.is_empty()))
        {
            menu.push((
                "Configure options".into(),
                DetailAction::ConfigureOptions(schema.clone()),
            ));
        }
        menu.push(("Update now".into(), DetailAction::Operation("update")));
        menu.push(("Uninstall".into(), DetailAction::Operation("uninstall")));
    }
    if let Some(url) = selected
        .plugin
        .manifest
        .homepage
        .as_ref()
        .filter(|s| !s.is_empty())
    {
        menu.push(("Open homepage".into(), DetailAction::Open(url.clone())));
    }
    if let Some(url) = selected
        .plugin
        .manifest
        .repository
        .as_ref()
        .filter(|s| !s.is_empty())
    {
        menu.push(("View repository".into(), DetailAction::Open(url.clone())));
    }
    menu.push(("Back to plugin list".into(), DetailAction::Back));
    menu
}
fn mcpb_path(plugin: &LoadedPlugin) -> Option<String> {
    plugin.manifest.mcp_servers.as_ref().and_then(|v| {
        if let Some(s) = v.as_str() {
            crate::utils::plugins::mcpb_handler::is_mcpb_source(s).then(|| s.into())
        } else {
            v.as_array().and_then(|v| {
                v.iter()
                    .filter_map(Value::as_str)
                    .find(|s| crate::utils::plugins::mcpb_handler::is_mcpb_source(s))
                    .map(str::to_owned)
            })
        }
    })
}
/// Maps to: CC ManagePlugins.tsx:514-2815#ManagePlugins.
#[component]
pub fn ManagePlugins(
    props: &mut ManagePluginsProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    #[cfg(test)]
    let test_imports = hooks
        .try_use_context::<ManagePluginsTestImports>()
        .map(|fixture| fixture.clone());
    let native = hooks.use_state(|| {
        let (notify, receiver) = tokio::sync::mpsc::unbounded_channel();
        (
            ManageStateHandle {
                value: std::sync::Arc::new(std::sync::Mutex::new(ManageState::default())),
                notify,
            },
            std::sync::Arc::new(tokio::sync::Mutex::new(receiver)),
        )
    });
    let (state, receiver) = native.read().clone();
    let mut revision = hooks.use_state(|| 0usize);
    hooks.use_future(async move {
        let mut receiver = receiver.lock().await;
        while receiver.recv().await.is_some() {
            revision.set(revision.get().wrapping_add(1));
        }
    });
    let snapshot = state.read().clone();
    let mcp = crate::state::app_state::use_app_state(&mut hooks, |s| s.mcp.clone());
    let errors = crate::state::app_state::use_app_state(&mut hooks, |s| s.plugins.errors.clone());
    let clients = crate::components::mcp::mcp_settings::server_infos_from_mcp_state(
        &crate::utils::config::GlobalConfig::default(),
        &crate::utils::config::ProjectConfig::default(),
        &mcp,
        &[],
    );
    #[cfg(not(test))]
    let flagged = get_flagged_plugins();
    #[cfg(test)]
    let flagged = if test_imports.is_some() {
        FlaggedPlugins::new()
    } else {
        get_flagged_plugins()
    };
    let items = unified_items(
        &snapshot.plugins,
        &clients,
        &errors,
        &snapshot.pending,
        &flagged,
    );
    let mut searching = hooks.use_state(|| false);
    let mut search = crate::hooks::use_search_input::use_search_input(&mut hooks, "");
    let query = search.text();
    let lower = query.to_lowercase();
    let filtered = items
        .iter()
        .filter(|item| {
            query.is_empty()
                || item.name.to_lowercase().contains(&lower)
                || item
                    .description
                    .as_ref()
                    .is_some_and(|s| s.to_lowercase().contains(&lower))
        })
        .cloned()
        .collect::<Vec<_>>();
    let pagination = use_pagination(
        &mut hooks,
        UsePaginationOptions {
            total_items: filtered.len(),
            selected_index: Some(snapshot.selected_index),
            max_visible: std::num::NonZeroUsize::new(8),
        },
    );
    let focused = hooks.use_terminal_focus();
    let (width, _) = hooks.use_terminal_size();
    let on_search = props.on_search_mode_change.clone();
    let set_searching = Handler::from(move |active| {
        let mut searching = searching;
        searching.set(active);
        on_search(active);
    });
    hooks.use_effect(
        {
            let state = state.clone();
            move || change(&state, |s| s.selected_index = 0)
        },
        query.clone(),
    );
    hooks.use_effect(
        {
            let state = state.clone();
            move || {
                launch({
                    let state = state.clone();
                    async move {
                        #[cfg(not(test))]
                        let result = load_all_plugins().await;
                        #[cfg(test)]
                        let result = if let Some(fixture) = test_imports {
                            Ok(fixture.loaded)
                        } else {
                            load_all_plugins().await
                        };
                        match result {
                            Ok(loaded) => {
                                let mut groups =
                                    indexmap::IndexMap::<String, Vec<LoadedPlugin>>::new();
                                for plugin in filter_managed_disabled_plugins(
                                    loaded.enabled.into_iter().chain(loaded.disabled).collect(),
                                ) {
                                    let marketplace = plugin
                                        .source
                                        .split('@')
                                        .nth(1)
                                        .filter(|s| !s.is_empty())
                                        .unwrap_or("local")
                                        .to_owned();
                                    groups.entry(marketplace).or_default().push(plugin);
                                }
                                let mut groups = groups.into_iter().collect::<Vec<_>>();
                                // First-match pinning verbatim per source; not
                                // antisymmetric, so it sorts through the
                                // non-validating JS-sort primitive
                                // (utils/js_sort.rs; mirror PR #7).
                                crate::utils::js_sort::sort_by(&mut groups, |(a, _), (b, _)| {
                                    if a == "claude-plugin-directory" {
                                        std::cmp::Ordering::Less
                                    } else if b == "claude-plugin-directory" {
                                        std::cmp::Ordering::Greater
                                    } else {
                                        crate::tools::grep_tool::javascript_locale_compare(a, b)
                                    }
                                });
                                let marketplaces = groups.iter().map(|(m, _)| m.clone()).collect();
                                let mut plugins = Vec::new();
                                for (marketplace, entries) in groups {
                                    for plugin in entries {
                                        let scope = if plugin.is_builtin {
                                            "builtin".into()
                                        } else {
                                            serde_json::to_value(
                                                get_plugin_installation_from_v2(&format!(
                                                    "{}@{marketplace}",
                                                    plugin.name
                                                ))
                                                .scope,
                                            )
                                            .unwrap()
                                            .as_str()
                                            .unwrap()
                                            .into()
                                        };
                                        plugins.push(PluginState {
                                            plugin,
                                            marketplace: marketplace.clone(),
                                            scope,
                                            pending_enable: None,
                                            pending_update: false,
                                        });
                                    }
                                }
                                change(&state, |s| {
                                    s.marketplaces = marketplaces;
                                    s.plugins = plugins;
                                    s.selected_index = 0;
                                });
                            }
                            Err(error) => crate::utils::log::log_error(
                                crate::utils::log::LogError::new(error.to_string()),
                            ),
                        }
                        change(&state, |s| s.loading = false);
                    }
                });
            }
        },
        (),
    );
    let flagged_ids = items
        .iter()
        .filter(|i| matches!(i.kind, UnifiedInstalledKind::FlaggedPlugin { .. }))
        .map(|i| i.id.clone())
        .collect::<Vec<_>>();
    hooks.use_effect(
        move || {
            if !flagged_ids.is_empty() {
                launch(async move {
                    mark_flagged_plugins_seen(&flagged_ids).await;
                });
            }
        },
        items
            .iter()
            .filter(|i| matches!(i.kind, UnifiedInstalledKind::FlaggedPlugin { .. }))
            .map(|i| i.id.clone())
            .collect::<Vec<_>>(),
    );
    let selected_key = snapshot.selected.as_ref().map(|p| {
        (
            p.plugin.name.clone(),
            p.plugin.path.clone(),
            p.plugin.manifest.mcp_servers.clone(),
            p.marketplace.clone(),
        )
    });
    hooks.use_effect(
        {
            let state = state.clone();
            move || {
                let selected = state.read().selected.clone();
                if let Some(selected) = selected {
                    launch({
                        let state = state.clone();
                        async move {
                            let mut has = mcpb_path(&selected.plugin).is_some();
                            if !has {
                                let path = selected
                                    .plugin
                                    .path
                                    .join("../.claude-plugin/marketplace.json");
                                if let Ok(text) = tokio::fs::read_to_string(path).await {
                                    if let Ok(raw) = serde_json::from_str::<Value>(&text) {
                                        if let Some(spec) = raw["plugins"]
                                            .as_array()
                                            .and_then(|v| {
                                                v.iter().find(|e| {
                                                    e["name"].as_str()
                                                        == Some(&selected.plugin.name)
                                                })
                                            })
                                            .and_then(|e| e.get("mcpServers"))
                                        {
                                            has = if let Some(s) = spec.as_str() {
                                                crate::utils::plugins::mcpb_handler::is_mcpb_source(
                                                    s,
                                                )
                                            } else {
                                                spec.as_array().is_some_and(|v| {
                                                    v.iter().filter_map(Value::as_str).any(
                                                crate::utils::plugins::mcpb_handler::is_mcpb_source,
                                            )
                                                })
                                            };
                                        }
                                    }
                                }
                            }
                            change(&state, |s| s.selected_has_mcpb = has);
                        }
                    });
                } else {
                    change(&state, |s| s.selected_has_mcpb = false);
                }
            }
        },
        selected_key,
    );
    let operation = Handler::from({
        let result = props.set_result.clone();
        let complete = props.on_manage_complete.clone();
        let parent = props.set_view_state.clone();
        {
            let state = state.clone();
            move |op: &'static str| {
                let (result, complete, parent) = (result.clone(), complete.clone(), parent.clone());
                launch({
                    let state = state.clone();
                    async move {
                        handle_single_operation(op, state, result, complete, parent).await;
                    }
                });
            }
        }
    });
    let auto_target = props.target_plugin.clone();
    let target_marketplace = props.target_marketplace.clone();
    let action = props.action.clone();
    let result = props.set_result.clone();
    let operation_auto = operation.clone();
    let failed = items.clone();
    hooks.use_effect(
        {
            let state = state.clone();
            move || {
                let current = state.read().clone();
                if current.auto_navigated || current.loading || current.marketplaces.is_empty() {
                    return;
                }
                let Some(target) = auto_target.filter(|s| !s.is_empty()) else {
                    return;
                };
                let parsed =
                    crate::utils::plugins::plugin_identifier::parse_plugin_identifier(&target);
                let market = target_marketplace.or(parsed.marketplace);
                let found = current
                    .plugins
                    .iter()
                    .find(|p| {
                        p.plugin.name == parsed.name
                            && market
                                .as_ref()
                                .filter(|m| !m.is_empty())
                                .is_none_or(|m| m == &p.marketplace)
                    })
                    .cloned();
                if let Some(mut found) = found {
                    found.scope = serde_json::to_value(
                        get_plugin_installation_from_v2(&plugin_id(&found)).scope,
                    )
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .into();
                    change(&state, |s| {
                        s.selected = Some(found);
                        s.view = ViewState::PluginDetails;
                        s.auto_navigated = true;
                    });
                    if let Some(action) = action {
                        match action.as_str() {
                            "enable" => operation_auto("enable"),
                            "disable" => operation_auto("disable"),
                            "uninstall" => operation_auto("uninstall"),
                            _ => {}
                        }
                    }
                } else if let Some(failed) = failed.into_iter().find(|i| {
                    matches!(i.kind, UnifiedInstalledKind::FailedPlugin { .. })
                        && i.name == parsed.name
                }) {
                    change(&state, |s| {
                        s.view = ViewState::FailedPluginDetails(failed);
                        s.auto_navigated = true;
                    });
                } else if action.is_some() {
                    change(&state, |s| s.auto_navigated = true);
                    result(Some(format!(
                        "Plugin \"{target}\" is not installed in this project"
                    )));
                }
            }
        },
        (
            snapshot.loading,
            snapshot.marketplaces.clone(),
            props.target_plugin.clone(),
            props.target_marketplace.clone(),
            props.action.clone(),
        ),
    );
    let back = Handler::from({
        let result = props.set_result.clone();
        let complete = props.on_manage_complete.clone();
        let parent = props.set_view_state.clone();
        {
            let state = state.clone();
            move |_| {
                let current = state.read().clone();
                match current.view {
                    ViewState::PluginDetails => change(&state, |s| {
                        s.view = ViewState::PluginList;
                        s.selected = None;
                        s.error = None;
                    }),
                    ViewState::FailedPluginDetails(_)
                    | ViewState::FlaggedDetail(_)
                    | ViewState::McpDetail(_) => change(&state, |s| {
                        s.view = ViewState::PluginList;
                        s.error = None;
                    }),
                    ViewState::Configuring(_) => {
                        change(&state, |s| s.view = ViewState::PluginDetails)
                    }
                    ViewState::PluginOptions | ViewState::ConfiguringOptions(_) => {
                        change(&state, |s| {
                            s.view = ViewState::PluginList;
                            s.selected = None;
                        });
                        result(Some(
                            "Plugin enabled. Configuration skipped — run /reload-plugins to apply."
                                .into(),
                        ));
                        complete(());
                    }
                    ViewState::McpTools(client) => {
                        change(&state, |s| s.view = ViewState::McpDetail(client))
                    }
                    ViewState::McpToolDetail(client, _) => {
                        change(&state, |s| s.view = ViewState::McpTools(client))
                    }
                    _ => {
                        if !current.pending.is_empty() {
                            result(Some("Run /reload-plugins to apply plugin changes.".into()));
                        } else {
                            parent(ParentViewState::Menu);
                        }
                    }
                }
            }
        }
    });
    let runtime = hooks
        .try_use_context::<crate::keybindings::keybinding_context::KeybindingRuntime>()
        .map(|r| r.clone());
    let cancel_active = (!matches!(snapshot.view, ViewState::PluginList) || !searching.get())
        && !matches!(
            snapshot.view,
            ViewState::ConfirmProjectUninstall | ViewState::ConfirmDataCleanup(_)
        );
    use_keybinding(
        &mut hooks,
        runtime.clone(),
        "confirm:no",
        ContextName::Confirmation,
        move || cancel_active,
        move || {
            back(());
            true
        },
    );
    let toggle_mcp =
        crate::services::mcp::mcp_connection_manager::use_mcp_toggle_enabled(&mut hooks);
    let toggles = filtered.clone();
    let list_active = matches!(snapshot.view, ViewState::PluginList) && !searching.get();
    use_keybinding(
        &mut hooks,
        runtime.clone(),
        "plugin:toggle",
        ContextName::Plugin,
        move || list_active,
        {
            let state = state.clone();
            move || {
                let selected_index = state.read().selected_index;
                if let Some(item) = toggles.get(selected_index) {
                    match &item.kind {
                        UnifiedInstalledKind::Plugin { .. }
                            if item.scope == "builtin"
                                || scope_from_text(&item.scope)
                                    .is_some_and(is_installable_scope) =>
                        {
                            let old = state.read().pending.get(&item.id).cloned();
                            let enabled = is_enabled(&item.id);
                            change(&state, |s| {
                                if old.is_some() {
                                    s.pending.remove(&item.id);
                                } else {
                                    s.pending.insert(
                                        item.id.clone(),
                                        if enabled {
                                            "will-disable".into()
                                        } else {
                                            "will-enable".into()
                                        },
                                    );
                                }
                            });
                            let id = item.id.clone();
                            launch(async move {
                                let enable = old.map(|p| p == "will-disable").unwrap_or(!enabled);
                                if let Err(error) = set_plugin_enabled_op(&id, enable, None).await {
                                    crate::utils::log::log_error(crate::utils::log::LogError::new(
                                        error.to_string(),
                                    ));
                                }
                                crate::utils::plugins::cache_utils::clear_all_caches();
                            });
                        }
                        UnifiedInstalledKind::Mcp { client, .. } => {
                            let name = client.name.clone();
                            let toggle = toggle_mcp.clone();
                            launch(async move {
                                let _ = toggle.call(&name).await;
                            });
                        }
                        _ => {}
                    }
                }
                true
            }
        },
    );
    let menu = snapshot
        .selected
        .as_ref()
        .map(|s| details_menu(s, snapshot.selected_has_mcpb))
        .unwrap_or_default();
    let chosen = filtered.clone();
    let menu_for_key = menu.clone();
    let result = props.set_result.clone();
    let complete = props.on_manage_complete.clone();
    let parent = props.set_view_state.clone();
    let search_mode = set_searching.clone();
    let select_active = list_active
        || matches!(
            snapshot.view,
            ViewState::PluginDetails | ViewState::FlaggedDetail(_)
        )
        || matches!(&snapshot.view,ViewState::FailedPluginDetails(item) if item.scope!="managed");
    let handlers: KeybindingHandlers = vec![
        (
            "select:previous".into(),
            Box::new({
                let state = state.clone();
                move || {
                    let current = state.read().clone();
                    if matches!(current.view, ViewState::PluginList) {
                        if current.selected_index == 0 {
                            search_mode(true);
                        } else {
                            pagination.handle_selection_change(
                                current.selected_index as isize - 1,
                                |index| change(&state, |s| s.selected_index = index),
                            );
                        }
                    } else if matches!(current.view, ViewState::PluginDetails)
                        && current.details_index > 0
                    {
                        change(&state, |s| s.details_index -= 1);
                    }
                    true
                }
            }),
        ),
        (
            "select:next".into(),
            Box::new({
                let count = filtered.len();
                let menu_len = menu.len();
                {
                    let state = state.clone();
                    move || {
                        let current = state.read().clone();
                        if matches!(current.view, ViewState::PluginList) {
                            if current.selected_index + 1 < count {
                                pagination.handle_selection_change(
                                    current.selected_index as isize + 1,
                                    |index| change(&state, |s| s.selected_index = index),
                                );
                            }
                        } else if matches!(current.view, ViewState::PluginDetails)
                            && current.details_index + 1 < menu_len
                        {
                            change(&state, |s| s.details_index += 1);
                        }
                        true
                    }
                }
            }),
        ),
        (
            "select:accept".into(),
            Box::new({
                let state = state.clone();
                move || {
                    let current = state.read().clone();
                    match current.view {
                        ViewState::PluginList => {
                            if let Some(item) = chosen.get(current.selected_index) {
                                match &item.kind {
                                    UnifiedInstalledKind::Plugin { .. } => {
                                        if let Some(found) = current
                                            .plugins
                                            .iter()
                                            .find(|s| plugin_id(s) == item.id)
                                            .cloned()
                                        {
                                            change(&state, |s| {
                                                s.selected = Some(found);
                                                s.view = ViewState::PluginDetails;
                                                s.details_index = 0;
                                                s.error = None;
                                            });
                                        }
                                    }
                                    UnifiedInstalledKind::FlaggedPlugin { .. } => {
                                        change(&state, |s| {
                                            s.view = ViewState::FlaggedDetail(item.clone());
                                            s.error = None;
                                        })
                                    }
                                    UnifiedInstalledKind::FailedPlugin { .. } => {
                                        change(&state, |s| {
                                            s.view = ViewState::FailedPluginDetails(item.clone());
                                            s.details_index = 0;
                                            s.error = None;
                                        })
                                    }
                                    UnifiedInstalledKind::Mcp { client, .. } => {
                                        change(&state, |s| {
                                            s.view = ViewState::McpDetail(client.clone());
                                            s.error = None;
                                        })
                                    }
                                }
                            }
                        }
                        ViewState::FlaggedDetail(item) => {
                            launch(async move {
                                remove_flagged_plugin(&item.id).await;
                            });
                            change(&state, |s| s.view = ViewState::PluginList);
                        }
                        ViewState::FailedPluginDetails(item) => {
                            let complete = complete.clone();
                            change(&state, |s| {
                                s.processing = true;
                                s.error = None;
                            });
                            launch({
                                let state = state.clone();
                                async move {
                                    let scope = scope_from_text(&item.scope);
                                    match uninstall_plugin_op(
                                        &item.id,
                                        scope
                                            .filter(|scope| is_installable_scope(*scope))
                                            .unwrap_or(PluginScope::User),
                                        false,
                                    )
                                    .await
                                    {
                                        Ok(answer) => {
                                            let mut success = answer.success;
                                            if !success {
                                                for source in [crate::utils::settings::constants::SettingSource::User,crate::utils::settings::constants::SettingSource::Project,crate::utils::settings::constants::SettingSource::Local]{if let Some(mut entries)=crate::utils::settings::get_settings_for_source(source).and_then(|s|s.enabled_plugins).and_then(|v|v.as_object().cloned()){if entries.contains_key(&item.id){entries.insert(item.id.clone(),Value::Null);let _=crate::utils::settings::update_settings_for_source(source,&Map::from_iter([("enabledPlugins".into(),Value::Object(entries))]));success=true;}}}
                                                crate::utils::plugins::cache_utils::clear_all_caches();
                                            }
                                            if success {
                                                complete(());
                                                change(&state, |s| {
                                                    s.processing = false;
                                                    s.view = ViewState::PluginList;
                                                });
                                            } else {
                                                change(&state, |s| {
                                                    s.processing = false;
                                                    s.error = Some(answer.message);
                                                });
                                            }
                                        }
                                        Err(error) => crate::utils::log::log_error(
                                            crate::utils::log::LogError::new(error.to_string()),
                                        ),
                                    }
                                }
                            });
                        }
                        ViewState::PluginDetails => {
                            if let Some((_, action)) =
                                menu_for_key.get(current.details_index).cloned()
                            {
                                match action {
                                    DetailAction::Operation(op) => operation(op),
                                    DetailAction::Back => change(&state, |s| {
                                        s.view = ViewState::PluginList;
                                        s.selected = None;
                                        s.error = None;
                                    }),
                                    DetailAction::Open(url) => launch(async move {
                                        let _ = crate::utils::browser::open_browser(&url).await;
                                    }),
                                    DetailAction::ConfigureOptions(schema) => change(&state, |s| {
                                        s.view = ViewState::ConfiguringOptions(schema)
                                    }),
                                    DetailAction::MarkUpdate => {
                                        if let Some(selected) = current.selected {
                                            launch({
                                                let state = state.clone();
                                                async move {
                                                    match check_if_local_plugin(
                                                        &selected.plugin.name,
                                                        &selected.marketplace,
                                                    )
                                                    .await
                                                    {
                                                        Ok(Some(error)) => change(&state, |s| {
                                                            s.error = Some(error)
                                                        }),
                                                        Ok(None) => change(&state, |s| {
                                                            if let Some(found) =
                                                                s.plugins.iter_mut().find(|p| {
                                                                    plugin_id(p)
                                                                        == plugin_id(&selected)
                                                                })
                                                            {
                                                                found.pending_update =
                                                                    !selected.pending_update;
                                                            }
                                                            if let Some(selected) = &mut s.selected
                                                            {
                                                                selected.pending_update =
                                                                    !selected.pending_update;
                                                            }
                                                        }),
                                                        Err(error) => change(&state, |s| {
                                                            s.error = Some(error.to_string())
                                                        }),
                                                    }
                                                }
                                            });
                                        }
                                    }
                                    DetailAction::ConfigureMcpb => {
                                        if let Some(selected) = current.selected {
                                            launch({
                                                let state = state.clone();
                                                async move {
                                                    let Some(path) = mcpb_path(&selected.plugin)
                                                    else {
                                                        change(&state, |s| {
                                                            s.error = Some(
                                                                "No MCPB file found in plugin"
                                                                    .into(),
                                                            )
                                                        });
                                                        return;
                                                    };
                                                    match crate::utils::plugins::mcpb_handler::load_mcpb_file(&path,&selected.plugin.path,&plugin_id(&selected),None,None,true).await{Ok(crate::utils::plugins::mcpb_handler::McpbFileResult::NeedsConfig(config))=>change(&state,|s|s.view=ViewState::Configuring(config)),Ok(_)=>change(&state,|s|s.error=Some("Failed to load MCPB for configuration".into())),Err(error)=>change(&state,|s|s.error=Some(format!("Failed to load configuration: {error}")))}
                                                }
                                            });
                                        }
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                    true
                }
            }),
        ),
    ];
    use_keybindings(
        &mut hooks,
        runtime.clone(),
        handlers,
        ContextName::Select,
        move || select_active,
    );
    let project_active = matches!(snapshot.view, ViewState::ConfirmProjectUninstall)
        && snapshot.selected.is_some()
        && !snapshot.processing;
    let result = props.set_result.clone();
    let complete = props.on_manage_complete.clone();
    let parent = props.set_view_state.clone();
    use_keybindings(
        &mut hooks,
        runtime,
        vec![
            (
                "confirm:yes".into(),
                Box::new({
                    let state = state.clone();
                    move || {
                        let Some(selected) = state.read().selected.clone() else {
                            return true;
                        };
                        change(&state, |s| {
                            s.processing = true;
                            s.error = None;
                        });
                        let (result, complete, parent) =
                            (result.clone(), complete.clone(), parent.clone());
                        launch({
                            let state = state.clone();
                            async move {
                                let source =
                                    crate::utils::settings::constants::SettingSource::Local;
                                let mut entries =
                                    crate::utils::settings::get_settings_for_source(source)
                                        .and_then(|s| s.enabled_plugins)
                                        .and_then(|v| v.as_object().cloned())
                                        .unwrap_or_default();
                                entries.insert(plugin_id(&selected), Value::Bool(false));
                                if let Err(error) =
                                    crate::utils::settings::update_settings_for_source(
                                        source,
                                        &Map::from_iter([(
                                            "enabledPlugins".into(),
                                            Value::Object(entries),
                                        )]),
                                    )
                                {
                                    change(&state, |s| {
                                        s.processing = false;
                                        s.error =
                                            Some(format!("Failed to write settings: {error}"));
                                    });
                                    return;
                                }
                                crate::utils::plugins::cache_utils::clear_all_caches();
                                result(Some(format!(
                                    "✓ Disabled {} in .claude/settings.local.json. Run /reload-plugins to apply.",
                                    selected.plugin.name
                                )));
                                complete(());
                                parent(ParentViewState::Menu);
                            }
                        });
                        true
                    }
                }) as Box<dyn FnMut() -> bool + Send>,
            ),
            (
                "confirm:no".into(),
                Box::new({
                    let state = state.clone();
                    move || {
                        change(&state, |s| {
                            s.view = ViewState::PluginDetails;
                            s.error = None;
                        });
                        true
                    }
                }),
            ),
        ],
        ContextName::Confirmation,
        move || project_active,
    );
    let cleanup_active = matches!(snapshot.view, ViewState::ConfirmDataCleanup(_))
        && snapshot.selected.is_some()
        && !snapshot.processing;
    let result = props.set_result.clone();
    let complete = props.on_manage_complete.clone();
    let parent = props.set_view_state.clone();
    let search_active = searching.get();
    let plugin_list = matches!(snapshot.view, ViewState::PluginList);
    let search_toggle = set_searching.clone();
    hooks.use_propagated_terminal_events({let state=state.clone();move |event| {
        if event.is_propagation_stopped() {
            return;
        }
        match event.event() {
            TerminalEvent::Key(key) if key.kind != KeyEventKind::Release => {
                if cleanup_active {
                    let Some(selected) = state.read().selected.clone() else {
                        return;
                    };
                    let scope = scope_from_text(&selected.scope);
                    let Some(scope) = scope.filter(|scope| is_installable_scope(*scope)) else {
                        return;
                    };
                    match key.code {
                        KeyCode::Char('y' | 'Y' | 'n' | 'N') => {
                            let delete = matches!(key.code, KeyCode::Char('y' | 'Y'));
                            let (result, complete, parent) =
                                (result.clone(), complete.clone(), parent.clone());
                            change(&state, |s| {
                                s.processing = true;
                                s.error = None;
                            });
                            launch({let state=state.clone();async move {
                                match uninstall_plugin_op(&plugin_id(&selected), scope, delete)
                                    .await
                                {
                                    Ok(answer) if answer.success => {
                                        crate::utils::plugins::cache_utils::clear_all_caches();
                                        result(Some(format!(
                                            "{} {}{}",
                                            crate::constants::figures::figures().tick,
                                            answer.message,
                                            if delete { "" } else { " · data preserved" }
                                        )));
                                        complete(());
                                        parent(ParentViewState::Menu);
                                    }
                                    answer => {
                                        let error = match answer {
                                            Ok(answer) => answer.message,
                                            Err(error) => error.to_string(),
                                        };
                                        change(&state, |s| {
                                            s.processing = false;
                                            s.error = Some(error);
                                        });
                                    }
                                }
                            }});
                            event.stop_propagation();
                        }
                        KeyCode::Esc => {
                            change(&state, |s| {
                                s.view = ViewState::PluginDetails;
                                s.error = None;
                            });
                            event.stop_propagation();
                        }
                        _ => {}
                    }
                    return;
                }
                if !plugin_list {
                    return;
                }
                if search_active {
                    let mut exit = || search_toggle(false);
                    if search.handle_key_down(
                        &key.code,
                        &key.modifiers,
                        crate::hooks::use_search_input::SearchInputOptions {
                            is_active: true,
                            on_exit: &mut exit,
                            on_cancel: None,
                            on_exit_up: None,
                            passthrough_ctrl_keys: &[],
                            backspace_exits_on_empty: true,
                        },
                    ) {
                        event.stop_propagation();
                    }
                } else if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                {
                    if let KeyCode::Char(c) = key.code {
                        if let Some(query) = search_entry_query(&c.to_string()) {
                            search_toggle(true);
                            search.set(query);
                            change(&state, |s| s.selected_index = 0);
                        }
                    }
                }
            }
            TerminalEvent::Paste(text) if plugin_list => {
                if search_active {
                    search.insert_text(text);
                } else if let Some(query) = search_entry_query(text) {
                    search_toggle(true);
                    search.set(query);
                    change(&state, |s| s.selected_index = 0);
                }
            }
            _ => {}
        }
    }});
    let theme = *hooks.use_context::<crate::utils::theme::Theme>();
    let figures = crate::constants::figures::figures();
    if snapshot.loading {
        return element! {Text(content:"Loading installed plugins…")}.into_any();
    }
    if items.is_empty() {
        return element!{View(flex_direction:FlexDirection::Column){View(margin_bottom:1u32){Text(content:"Manage plugins",weight:Weight::Bold)}Text(content:"No plugins or MCP servers installed.")View(margin_top:1u32){Text(content:"Esc to go back",dim:true)}}}.into_any();
    }
    if let Some(selected) = snapshot.selected.clone() {
        match snapshot.view.clone(){
        ViewState::PluginOptions=>{let result=props.set_result.clone();let complete=props.on_manage_complete.clone();let parent=props.set_view_state.clone();let name=selected.plugin.name.clone();return element!{PluginOptionsFlow(plugin:selected.plugin.clone(),plugin_id:plugin_id(&selected),on_done:move|(outcome,detail):(String,Option<String>)|{let message=match outcome.as_str(){"configured"=>format!("✓ Enabled and configured {name}. Run /reload-plugins to apply."),"skipped"=>format!("✓ Enabled {name}. Run /reload-plugins to apply."),"error"=>format!("Failed to save configuration: {}",detail.as_deref().unwrap_or("undefined")),_=>return};result(Some(message));complete(());parent(ParentViewState::Menu);})}.into_any();},
        ViewState::ConfiguringOptions(schema)=>{let id=plugin_id(&selected);let initial=options::load_plugin_options(&id);let result=props.set_result.clone();return element!{PluginOptionsDialog(title:format!("Configure {}",selected.plugin.name),subtitle:"Plugin options".to_owned(),config_schema:schema.clone(),initial_values:Some(initial),on_save:{let state=state.clone();move|values|{let state=state.clone();let(id,schema,result)=(id.clone(),schema.clone(),result.clone());launch(async move{match options::save_plugin_options(&id,&values,&schema){Ok(())=>{crate::utils::plugins::cache_utils::clear_all_caches();result(Some("Configuration saved. Run /reload-plugins for changes to take effect.".into()));},Err(error)=>change(&state,|s|s.error=Some(format!("Failed to save configuration: {error}")))}change(&state,|s|s.view=ViewState::PluginDetails);});}},on_cancel:{let state=state.clone();move|_|change(&state,|s|s.view=ViewState::PluginDetails)})}.into_any();},
        ViewState::Configuring(config)=>{let result=props.set_result.clone();return element!{PluginOptionsDialog(title:format!("Configure {}",config.manifest["name"].as_str().unwrap_or("undefined")),subtitle:format!("Plugin: {}",selected.plugin.name),config_schema:config.config_schema,initial_values:Some(config.existing_config),on_save:{let state=state.clone();move|values|{let state=state.clone();let(selected,result)=(selected.clone(),result.clone());launch(async move{let Some(path)=mcpb_path(&selected.plugin)else{change(&state,|s|{s.error=Some("No MCPB file found".into());s.view=ViewState::PluginDetails;});return;};match crate::utils::plugins::mcpb_handler::load_mcpb_file(&path,&selected.plugin.path,&plugin_id(&selected),None,Some(&values),false).await{Ok(_)=>{change(&state,|s|{s.error=None;s.view=ViewState::PluginDetails;});result(Some("Configuration saved. Run /reload-plugins for changes to take effect.".into()));},Err(error)=>change(&state,|s|{s.error=Some(format!("Failed to save configuration: {error}"));s.view=ViewState::PluginDetails;})}});}},on_cancel:{let state=state.clone();move|_|change(&state,|s|s.view=ViewState::PluginDetails)})}.into_any();},
        ViewState::ConfirmProjectUninstall=>return element!{View(flex_direction:FlexDirection::Column){Text(content:format!("{} is enabled in .claude/settings.json (shared with your team)",selected.plugin.name),weight:Weight::Bold,color:theme.warning)View(margin_top:1u32,flex_direction:FlexDirection::Column){Text(content:"Disable it just for you in .claude/settings.local.json?")Text(content:"This has the same effect as uninstalling, without affecting other contributors.",dim:true)}#(snapshot.error.clone().map(|error|element!{View(margin_top:1u32){Text(content:error,color:theme.error)}}))View(margin_top:1u32){#(if snapshot.processing{element!{Text(content:"Disabling…",dim:true)}.into_any()}else{element!{ManageShortcuts(kind:"project")}.into_any()})}}}.into_any(),
        ViewState::ConfirmDataCleanup(size)=>return element!{View(flex_direction:FlexDirection::Column){Text(content:format!("{} has {size} of persistent data",selected.plugin.name),weight:Weight::Bold)View(margin_top:1u32,flex_direction:FlexDirection::Column){Text(content:"Delete it along with the plugin?")Text(content:crate::utils::plugins::plugin_directories::plugin_data_dir_path(&plugin_id(&selected)).to_string_lossy().into_owned(),dim:true)}#(snapshot.error.clone().map(|error|element!{View(margin_top:1u32){Text(content:error,color:theme.error)}}))View(margin_top:1u32){#(if snapshot.processing{element!{Text(content:"Uninstalling…",dim:true)}.into_any()}else{element!{View{Text(content:"y",weight:Weight::Bold)Text(content:" to delete · ")Text(content:"n",weight:Weight::Bold)Text(content:" to keep · ")Text(content:"esc",weight:Weight::Bold)Text(content:" to cancel")}}.into_any()})}}}.into_any(),
        ViewState::PluginDetails=>{let enabled=is_enabled(&plugin_id(&selected));let selected_errors=errors.iter().filter(|e|error_plugin(e).as_deref()==Some(&selected.plugin.name)||e.source()==plugin_id(&selected)||e.source().starts_with(&format!("{}@",selected.plugin.name))).collect::<Vec<_>>();return element!{View(flex_direction:FlexDirection::Column){Text(content:format!("{} @ {}",selected.plugin.name,selected.marketplace),weight:Weight::Bold)View{Text(content:"Scope: ",dim:true)Text(content:if selected.scope.is_empty(){"user".into()}else{selected.scope.clone()})}#(selected.plugin.manifest.version.clone().filter(|v|!v.is_empty()).map(|version|element!{View{Text(content:"Version: ",dim:true)Text(content:version)}}))#(selected.plugin.manifest.description.clone().filter(|v|!v.is_empty()).map(|description|element!{View(margin_bottom:1u32){Text(content:description)}}))#(selected.plugin.manifest.author.as_ref().map(|author|element!{View{Text(content:"Author: ",dim:true)Text(content:author["name"].as_str().unwrap_or_default().to_owned())}}))View(margin_bottom:1u32){Text(content:"Status: ",dim:true)Text(content:if enabled{"Enabled"}else{"Disabled"},color:if enabled{theme.success}else{theme.warning})#(selected.pending_update.then(||element!{Text(content:" · Marked for update",color:theme.suggestion)}))}PluginComponentsDisplay(plugin:Some(selected.plugin.clone()),marketplace:selected.marketplace.clone())#((!selected_errors.is_empty()).then(||element!{View(flex_direction:FlexDirection::Column,margin_bottom:1u32){Text(content:format!("{} {}:",selected_errors.len(),if selected_errors.len()==1{"error"}else{"errors"}),weight:Weight::Bold,color:theme.error)#(selected_errors.iter().map(|error|element!{View(flex_direction:FlexDirection::Column,margin_left:2u32){Text(content:format_error_message(error),color:theme.error)#(get_error_guidance(error).map(|guidance|element!{Text(content:format!("{} {guidance}",figures.arrow_right),dim:true,italic:true)}))}}))}}))View(margin_top:1u32,flex_direction:FlexDirection::Column){#(menu.iter().enumerate().map(|(index,(label,_))|element!{View{Text(content:if index==snapshot.details_index{format!("{} ",figures.pointer)}else{"  ".into()})Text(content:label.clone(),weight:if index==snapshot.details_index{Weight::Bold}else{Weight::Normal},color:if label.contains("Uninstall"){Some(theme.error)}else if label.contains("Update"){Some(theme.suggestion)}else{None})}}))}#(snapshot.processing.then(||element!{View(margin_top:1u32){Text(content:"Processing…")}}))#(snapshot.error.clone().map(|error|element!{View(margin_top:1u32){Text(content:error,color:theme.error)}}))View(margin_top:1u32){ManageShortcuts(kind:"details")}}}.into_any();},_=>{}
    }
    }
    match snapshot.view.clone() {
        ViewState::FlaggedDetail(item) => {
            if let UnifiedInstalledKind::FlaggedPlugin {
                reason,
                text,
                flagged_at,
            } = item.kind
            {
                return element!{View(flex_direction:FlexDirection::Column){Text(content:format!("{} @ {}",item.name,item.marketplace.unwrap_or_default()),weight:Weight::Bold)View(margin_bottom:1u32){Text(content:"Status: ",dim:true)Text(content:"Removed",color:theme.error)}View(margin_bottom:1u32,flex_direction:FlexDirection::Column){Text(content:format!("Removed from marketplace · reason: {reason}"),color:theme.error)Text(content:text)Text(content:format!("Flagged on {flagged_at}"),dim:true)}View(margin_top:1u32){Text(content:format!("{} ",figures.pointer))Text(content:"Dismiss",color:theme.suggestion)}ManageShortcuts(kind:"flagged")}}.into_any();
            }
        }
        ViewState::FailedPluginDetails(item) => {
            let message = if let UnifiedInstalledKind::FailedPlugin { errors } = item.kind {
                errors
                    .first()
                    .map(format_error_message)
                    .unwrap_or_else(|| "Failed to load".into())
            } else {
                "Failed to load".into()
            };
            let managed = item.scope == "managed";
            return element!{View(flex_direction:FlexDirection::Column){View{Text(content:item.name,weight:Weight::Bold)Text(content:format!(" @ {} ({})",item.marketplace.unwrap_or_default(),item.scope),dim:true)}Text(content:message,color:theme.error)View(margin_top:1u32){#(if managed{element!{Text(content:"Managed by your organization — contact your admin",dim:true)}.into_any()}else{element!{View{Text(content:format!("{} ",figures.pointer),color:theme.suggestion)Text(content:"Remove",weight:Weight::Bold)}}.into_any()})}#(snapshot.processing.then(||element!{Text(content:"Processing…")}))#(snapshot.error.clone().map(|error|element!{Text(content:error,color:theme.error)}))View(margin_top:1u32){ManageShortcuts(kind:if managed{"back"}else{"failed"})}}}.into_any();
        }
        ViewState::McpDetail(client) => {
            let selected = client.clone();
            let complete = props.set_result.clone();
            let on_complete = Handler::from({
                let state = state.clone();
                move |message: String| {
                    if !message.is_empty() {
                        complete(Some(message));
                    }
                    change(&state, |s| s.view = ViewState::PluginList);
                }
            });
            return match client.transport{crate::services::mcp::types::Transport::Stdio=>element!{crate::components::mcp::mcp_stdio_server_menu::MCPStdioServerMenu(server:Some(client),on_view_tools:{let state=state.clone();move|_|change(&state,|s|s.view=ViewState::McpTools(selected.clone()))},on_cancel:{let state=state.clone();move|_|change(&state,|s|s.view=ViewState::PluginList)},on_complete:on_complete,borderless:true)}.into_any(),crate::services::mcp::types::Transport::Sse|crate::services::mcp::types::Transport::Http|crate::services::mcp::types::Transport::ClaudeAiProxy=>element!{crate::components::mcp::mcp_remote_server_menu::MCPRemoteServerMenu(server:Some(client),on_view_tools:{let state=state.clone();move|_|change(&state,|s|s.view=ViewState::McpTools(selected.clone()))},on_cancel:{let state=state.clone();move|_|change(&state,|s|s.view=ViewState::PluginList)},on_complete:on_complete,borderless:true)}.into_any(),_=>{change(&state,|s|s.view=ViewState::PluginList);element!{View}.into_any()}};
        }
        ViewState::McpTools(client) => {
            let (selected, back) = (client.clone(), client.clone());
            return element!{crate::components::mcp::mcp_tool_list_view::MCPToolListView(server:Some(client),on_select_tool:{let state=state.clone();move|index|change(&state,|s|s.view=ViewState::McpToolDetail(selected.clone(),index))},on_back:{let state=state.clone();move|_|change(&state,|s|s.view=ViewState::McpDetail(back.clone()))})}.into_any();
        }
        ViewState::McpToolDetail(client, index) => {
            let back = client.clone();
            return element!{crate::components::mcp::mcp_tool_detail_view::MCPToolDetailView(tool:client.tools.get(index).cloned(),server:Some(client),on_back:{let state=state.clone();move|_|change(&state,|s|s.view=ViewState::McpTools(back.clone()))})}.into_any();
        }
        _ => {}
    }
    let visible = pagination.get_visible_items(&filtered);
    let cursor_offset = search.offset();
    element!{View(flex_direction:FlexDirection::Column){View(margin_bottom:1u32){crate::components::search_box::SearchBox(query:query.clone(),is_focused:searching.get(),is_terminal_focused:focused,width:Some(u32::from(width.saturating_sub(4))),cursor_offset:Some(cursor_offset))}#((filtered.is_empty()&&!query.is_empty()).then(||element!{View(margin_bottom:1u32){Text(content:format!("No items match \"{query}\""),dim:true)}}))#(pagination.scroll_position.can_scroll_up.then(||element!{Text(content:format!(" {} more above",figures.arrow_up),dim:true)}))#(visible.iter().enumerate().map(|(index,item)|{let header=index==0||visible[index-1].scope!=item.scope;let label=match item.scope.as_str(){"flagged"=>"Flagged","project"=>"Project","local"=>"Local","user"=>"User","enterprise"=>"Enterprise","managed"=>"Managed","builtin"|"dynamic"=>"Built-in",other=>other};element!{View(flex_direction:FlexDirection::Column){#(header.then(||element!{View(margin_top:if index>0{1u32}else{0},padding_left:2u32){Text(content:label.to_owned(),dim:item.scope!="flagged",color:(item.scope=="flagged").then_some(theme.warning),weight:if item.scope=="flagged"{Weight::Bold}else{Weight::Normal})}}))UnifiedInstalledCell(item:Some(item.clone()),is_selected:pagination.to_actual_index(index)==snapshot.selected_index&&!searching.get())}}}))#(pagination.scroll_position.can_scroll_down.then(||element!{Text(content:format!(" {} more below",figures.arrow_down),dim:true)}))View(margin_top:1u32,margin_left:1u32){ManageShortcuts(kind:"list")}#((!snapshot.pending.is_empty()).then(||element!{View(margin_left:1u32){Text(content:"Run /reload-plugins to apply changes",dim:true,italic:true)}}))}}.into_any()
}

// JSX helper for the repeated source Byline subtree; event routing remains the
// canonical ConfigurableShortcutHint/useKeybindings contract.
#[derive(Default, Props)]
struct ManageShortcutsProps {
    kind: &'static str,
}
#[component]
fn ManageShortcuts(props: &ManageShortcutsProps) -> impl Into<AnyElement<'static>> {
    use crate::components::{
        configurable_shortcut_hint::ConfigurableShortcutHint, design_system::byline::Byline,
    };
    let accept = match props.kind {
        "flagged" => Some("dismiss"),
        "failed" => Some("remove"),
        "details" => Some("select"),
        "list" => Some("details"),
        _ => None,
    };
    element!{Byline{#((props.kind=="list").then(||element!{Text(content:"type to search",dim:true,italic:true)}))#((props.kind=="list").then(||element!{ConfigurableShortcutHint(action:"plugin:toggle".to_owned(),context:"Plugin".to_owned(),fallback:"Space".to_owned(),description:"toggle".to_owned(),dim:true,italic:true)}))#((props.kind=="details").then(||element!{ConfigurableShortcutHint(action:"select:previous".to_owned(),context:"Select".to_owned(),fallback:"↑".to_owned(),description:"navigate".to_owned(),dim:true,italic:true)}))#((props.kind=="project").then(||element!{ConfigurableShortcutHint(action:"confirm:yes".to_owned(),context:"Confirmation".to_owned(),fallback:"y".to_owned(),description:"disable".to_owned())}))#(accept.map(|description|element!{ConfigurableShortcutHint(action:"select:accept".to_owned(),context:"Select".to_owned(),fallback:"Enter".to_owned(),description:description.to_owned(),dim:true,italic:true)}))ConfigurableShortcutHint(action:"confirm:no".to_owned(),context:"Confirmation".to_owned(),fallback:"Esc".to_owned(),description:if props.kind=="project"{"cancel".to_owned()}else{"back".to_owned()},dim:props.kind!="project",italic:props.kind!="project")}}.into_any()
}

#[derive(Default, Props)]
struct PluginComponentsDisplayProps {
    plugin: Option<LoadedPlugin>,
    marketplace: String,
}
/// Maps to: CC ManagePlugins.tsx:233-475#PluginComponentsDisplay.
#[component]
fn PluginComponentsDisplay(
    props: &PluginComponentsDisplayProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let mut result = hooks.use_state(|| None::<Result<Vec<(String, Vec<Value>)>, String>>);
    let channel = hooks.use_state(|| {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        (tx, std::sync::Arc::new(tokio::sync::Mutex::new(rx)))
    });
    let (result_tx, result_rx) = channel.read().clone();
    hooks.use_future(async move {
        let mut receiver = result_rx.lock().await;
        while let Some(next) = receiver.recv().await {
            result.set(Some(next));
        }
    });
    let plugin = props.plugin.clone();
    let marketplace = props.marketplace.clone();
    hooks.use_effect(
        move || {
            let Some(plugin) = plugin else {
                return;
            };
            launch(async move {
                let load = async {
                    let mut values = Vec::new();
                    if marketplace == "builtin" {
                        let def = crate::plugins::builtin_plugins::get_builtin_plugin_definition(
                            &plugin.name,
                        )
                        .ok_or_else(|| {
                            anyhow::anyhow!("Built-in plugin {} not found", plugin.name)
                        })?;
                        for (label, value) in
                            [("Hooks", def.hooks), ("MCP Servers", def.mcp_servers)]
                        {
                            let keys = value
                                .and_then(|v| {
                                    v.as_object().map(|v| {
                                        v.keys().cloned().map(Value::String).collect::<Vec<_>>()
                                    })
                                })
                                .unwrap_or_default();
                            if !keys.is_empty() {
                                values.push((label.into(), keys));
                            }
                        }
                        return Ok(values);
                    }
                    let catalog = get_marketplace(&marketplace).await?;
                    let entry = catalog["plugins"]
                        .as_array()
                        .and_then(|entries| {
                            entries
                                .iter()
                                .find(|entry| entry["name"].as_str() == Some(&plugin.name))
                        })
                        .ok_or_else(|| {
                            anyhow::anyhow!("Plugin {} not found in marketplace", plugin.name)
                        })?;
                    for (label, primary, additional, skills) in [
                        (
                            "Commands",
                            plugin.commands_path,
                            plugin.commands_paths,
                            false,
                        ),
                        ("Agents", plugin.agents_path, plugin.agents_paths, false),
                        ("Skills", plugin.skills_path, plugin.skills_paths, true),
                    ] {
                        let mut names = Vec::new();
                        for path in primary.into_iter().chain(additional) {
                            names.extend(if skills {
                                get_skill_dir_names(&path).await
                            } else {
                                get_base_file_names(&path).await
                            });
                        }
                        if !names.is_empty() {
                            values.push((
                                label.into(),
                                names.into_iter().map(Value::String).collect(),
                            ));
                        }
                    }
                    for (label, loaded, key) in [
                        ("Hooks", plugin.hooks_config, "hooks"),
                        ("MCP Servers", plugin.mcp_servers.snapshot(), "mcpServers"),
                    ] {
                        let mut all = Vec::new();
                        if let Some(value) = loaded {
                            all.push(Value::Array(
                                value
                                    .as_object()
                                    .map(|m| m.keys().cloned().map(Value::String).collect())
                                    .unwrap_or_default(),
                            ));
                        }
                        if let Some(value) = entry
                            .get(key)
                            .filter(|v| !v.is_null() && **v != Value::Bool(false))
                        {
                            all.push(value.clone());
                        }
                        if !all.is_empty() {
                            values.push((label.into(), all));
                        }
                    }
                    Ok::<_, anyhow::Error>(values)
                }
                .await;
                let _ = result_tx.send(load.map_err(|error| error.to_string()));
            });
        },
        (
            props.plugin.as_ref().map(|p| {
                (
                    p.name.clone(),
                    (p.commands_path.clone(), p.commands_paths.clone()),
                    (p.agents_path.clone(), p.agents_paths.clone()),
                    (p.skills_path.clone(), p.skills_paths.clone()),
                    p.hooks_config.clone(),
                    // Maps to ManagePlugins.tsx:388 plugin.mcpServers dependency.
                    // Retain the object Arc alongside its identity, not the shared slot.
                    p.mcp_servers
                        .identity_snapshot()
                        .map(|value| (std::sync::Arc::as_ptr(&value) as usize, value)),
                )
            }),
            props.marketplace.clone(),
        ),
    );
    let result = result.read().clone();
    match result{None=>element!{View}.into_any(),Some(Err(error))=>element!{View(flex_direction:FlexDirection::Column,margin_bottom:1u32){Text(content:"Components:",weight:Weight::Bold)Text(content:format!("Error: {error}"),dim:true)}}.into_any(),Some(Ok(values))if values.is_empty()=>element!{View}.into_any(),Some(Ok(values))=>element!{View(flex_direction:FlexDirection::Column,margin_bottom:1u32){Text(content:"Installed components:",weight:Weight::Bold)#(values.into_iter().map(|(label,names)|element!{Text(content:format!("• {label}: {}",names.iter().map(crate::utils::zod::js_string).collect::<Vec<_>>().join(", ")),dim:true)}))}}.into_any()}
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use std::sync::{Arc, Mutex};
    fn plugin(name: &str, marketplace: &str) -> PluginState {
        PluginState {
            plugin: LoadedPlugin {
                name: name.into(),
                source: format!("{name}@{marketplace}"),
                is_builtin: marketplace == "builtin",
                manifest: crate::utils::plugins::schemas::PluginManifest {
                    name: name.into(),
                    ..Default::default()
                },
                ..Default::default()
            },
            marketplace: marketplace.into(),
            scope: if marketplace == "builtin" {
                "builtin"
            } else {
                "user"
            }
            .into(),
            pending_enable: None,
            pending_update: false,
        }
    }
    fn isolated_settings() {
        crate::utils::settings::settings_cache::set_session_settings_cache(
            crate::utils::settings::SettingsWithErrors::default(),
        );
    }

    #[test]
    fn search_entry_payload_matches_source_for_paste_and_js_whitespace() {
        for input in ["", "j", "k", " ", "\u{feff}", "\t\r\n"] {
            assert_eq!(search_entry_query(input), None, "{input:?}");
        }
        assert_eq!(search_entry_query("/"), Some(String::new()));
        for input in ["i", "jk", "/alpha", "\u{0085}"] {
            assert_eq!(search_entry_query(input), Some(input.to_owned()));
        }
    }

    #[test]
    fn scope_projection_does_not_turn_non_installation_labels_into_user_scope() {
        for scope in ["builtin", "flag", "session", "dynamic", "enterprise", ""] {
            assert!(!scope_from_text(scope).is_some_and(is_installable_scope));
        }
        assert!(!scope_from_text("managed").is_some_and(is_installable_scope));
        for scope in ["user", "project", "local"] {
            assert!(scope_from_text(scope).is_some_and(is_installable_scope));
        }
    }

    #[test]
    fn unified_items_keep_source_scope_order_and_group_errors_on_loaded_plugin() {
        isolated_settings();
        let mut project = plugin("Zulu", "market");
        project.scope = "project".into();
        let states = vec![
            plugin("Zulu", "builtin"),
            plugin("Beta", "market"),
            project,
            plugin("Alpha", "market"),
        ];
        let errors = vec![PluginError::GenericError {
            source: "Alpha@other-market".into(),
            plugin: Some("Alpha".into()),
            error: "fixture".into(),
        }];
        let items = unified_items(
            &states,
            &[],
            &errors,
            &HashMap::new(),
            &FlaggedPlugins::new(),
        );
        assert_eq!(
            items
                .iter()
                .map(|item| (item.name.as_str(), item.scope.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("Zulu", "project"),
                ("Alpha", "user"),
                ("Beta", "user"),
                ("Zulu", "builtin")
            ]
        );
        assert!(matches!(&items[1].kind,UnifiedInstalledKind::Plugin{errors,..}if errors.len()==1));
        assert!(
            !items
                .iter()
                .any(|item| matches!(item.kind, UnifiedInstalledKind::FailedPlugin { .. }))
        );
    }

    #[test]
    fn details_menu_matches_builtin_and_optional_configuration_source_order() {
        isolated_settings();
        let mut selected = plugin("Alpha", "market");
        selected.plugin.manifest.user_config = Some(serde_json::json!({"token":{"type":"string"}}));
        selected.plugin.manifest.homepage = Some("https://example.test".into());
        selected.plugin.manifest.repository = Some("https://example.test/repo".into());
        assert_eq!(
            details_menu(&selected, true)
                .iter()
                .map(|(label, _)| label.as_str())
                .collect::<Vec<_>>(),
            vec![
                "Disable plugin",
                "Mark for update",
                "Configure",
                "Configure options",
                "Update now",
                "Uninstall",
                "Open homepage",
                "View repository",
                "Back to plugin list"
            ]
        );
        assert_eq!(
            details_menu(&plugin("Builtin", "builtin"), false)
                .iter()
                .map(|(label, _)| label.as_str())
                .collect::<Vec<_>>(),
            vec!["Disable plugin", "Back to plugin list"]
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn native_setter_mailbox_keeps_concurrent_updates_and_detached_completion() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let state = ManageStateHandle {
            value: Arc::new(Mutex::new(ManageState::default())),
            notify: tx,
        };
        let mut work = Vec::new();
        for index in 0..32 {
            let state = state.clone();
            work.push(tokio::spawn(async move {
                change(&state, |s| {
                    s.pending.insert(index.to_string(), "will-enable".into());
                });
            }));
        }
        for worker in work {
            worker.await.unwrap();
        }
        assert_eq!(state.read().pending.len(), 32);
        for _ in 0..32 {
            assert_eq!(rx.try_recv(), Ok(()));
        }
        drop(rx);
        let detached = state.clone();
        tokio::spawn(async move {
            change(&detached, |s| s.loading = false);
        })
        .await
        .unwrap();
        assert!(!state.read().loading);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn mounted_manage_search_details_and_back_preserve_source_callbacks() {
        crate::utils::process_runtime::initialize_test_process_runtime();
        isolated_settings();
        for name in ["Alpha", "Zulu"] {
            crate::plugins::builtin_plugins::register_builtin_plugin(
                crate::plugins::builtin_plugins::BuiltinPluginDefinition::new(
                    name,
                    format!("{name} description"),
                ),
            );
        }
        let fixture = ManagePluginsTestImports {
            loaded: crate::types::plugin::PluginLoadResult {
                enabled: vec![
                    plugin("Zulu", "builtin").plugin,
                    plugin("Alpha", "builtin").plugin,
                ],
                ..Default::default()
            },
        };
        let events = Arc::new(Mutex::new(Vec::<String>::new()));
        let changes = events.clone();
        let exits = events.clone();
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        let runtime =
            crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings();
        // Match the production REPL provider boundary. No startup config is
        // supplied, so this mounted manager cannot start transports.
        use crate::services::mcp::mcp_connection_manager::McpConnectionManager;
        let mcp_store = store.clone();
        let mut app = element! {ContextProvider(value:Context::owned(*crate::utils::theme::current())){ContextProvider(value:Context::owned(fixture)){ContextProvider(value:Context::owned(store)){ContextProvider(value:Context::owned(runtime)){McpConnectionManager(app_store:Some(mcp_store)){FocusScope(handle_keys:false){ManagePlugins(on_search_mode_change:move|active|changes.lock().unwrap().push(format!("search:{active}")),set_view_state:move|view|{assert!(matches!(view,ParentViewState::Menu));exits.lock().unwrap().push("menu".into());})}}}}}}};
        let (keys, input) = async_channel::unbounded();
        let mut frames = Box::pin(
            app.mock_terminal_render_loop(MockTerminalConfig::with_events(input).with_size(96, 35)),
        );
        let mut last = String::new();
        let deadline = futures_timer::Delay::new(std::time::Duration::from_secs(5));
        tokio::pin!(deadline);
        let mut step = 0usize;
        loop {
            tokio::select! {
                _=&mut deadline=>panic!("Manage fixture timeout at step {step}: {last}"),
                frame=frames.next()=>{last=frame.expect("live Manage frame").to_string();let ready=match step{0=>last.contains("Alpha")&&last.contains("Zulu"),1=>last.contains("Zulu")&&!last.contains("Alpha"),2=>last.contains("Alpha")&&last.contains("Zulu"),3=>events.lock().unwrap().iter().any(|value|value=="search:false"),4=>last.contains("Alpha @ builtin")&&last.contains("Disable plugin"),5=>last.contains("Alpha")&&last.contains("Zulu")&&!last.contains("Scope:"),_=>false};
                    if !ready{continue;}let key=match step{0=>{assert!(last.find("Alpha").unwrap()<last.find("Zulu").unwrap());
                        // ManagePlugins.tsx:2695-2702: the search box is `terminalWidth - 4` wide inside a plain row Box.
                        let search_top=last.lines().find(|line|line.contains('╭')).expect("search box");
                        assert_eq!(search_top.trim().chars().count(),92,"{last}");
                        KeyCode::Char('z')},1|2=>KeyCode::Esc,3=>KeyCode::Enter,4|5=>KeyCode::Esc,_=>unreachable!()};step+=1;keys.send(TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press,key))).await.unwrap();
                },
                _=futures_timer::Delay::new(std::time::Duration::from_millis(5)),if step==6=>{if events.lock().unwrap().iter().any(|event|event=="menu"){break;}}
            }
        }
        assert_eq!(
            *events.lock().unwrap(),
            vec!["search:true", "search:false", "menu"]
        );
    }
}
