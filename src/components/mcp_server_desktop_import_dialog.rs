//! Maps to: CC `components/MCPServerDesktopImportDialog.tsx`.
//!
//! Safety boundary: the official dialog reads existing MCP configs, writes
//! selected servers with `addMcpConfig`, writes status text to stdout, then
//! calls `gracefulShutdown`. Nyan preserves visible copy, collision labeling,
//! default non-colliding selection, final-name suffix calculation, cancel path,
//! and emits the import result via callback. MCP config mutation/stdout/shutdown
//! are deferred to the MCP runtime slice.

use crate::components::configurable_shortcut_hint::ConfigurableShortcutHint;
use crate::components::custom_select::{SelectMulti, SelectOptionData};
use crate::components::design_system::byline::Byline;
use crate::components::design_system::dialog::Dialog;
use crate::components::design_system::keyboard_shortcut_hint::KeyboardShortcutHint;
use crate::utils::theme::Theme;
use iocraft::prelude::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum McpConfigScope {
    Local,
    #[default]
    User,
    Project,
    Dynamic,
    ClaudeAi,
}

impl McpConfigScope {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::User => "user",
            Self::Project => "project",
            Self::Dynamic => "dynamic",
            Self::ClaudeAi => "claudeai",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MCPServerDesktopImportResult {
    pub imported_count: usize,
    pub final_names: Vec<String>,
    pub scope: McpConfigScope,
}

pub fn plural_server(count: usize) -> &'static str {
    if count == 1 { "server" } else { "servers" }
}

pub fn mcp_desktop_import_collisions(
    server_names: &[String],
    existing_server_names: &[String],
) -> Vec<String> {
    server_names
        .iter()
        .filter(|name| {
            existing_server_names
                .iter()
                .any(|existing| existing == *name)
        })
        .cloned()
        .collect()
}

pub fn mcp_desktop_import_default_selection(
    server_names: &[String],
    collisions: &[String],
) -> Vec<String> {
    server_names
        .iter()
        .filter(|name| !collisions.iter().any(|collision| collision == *name))
        .cloned()
        .collect()
}

pub fn mcp_desktop_import_options(
    server_names: &[String],
    collisions: &[String],
) -> Vec<SelectOptionData> {
    server_names
        .iter()
        .map(|server| SelectOptionData {
            label: if collisions.iter().any(|collision| collision == server) {
                format!("{server} (already exists)")
            } else {
                server.clone()
            },
            value: server.clone(),
            ..SelectOptionData::default()
        })
        .collect()
}

/// Maps to: CC `MCPServerDesktopImportDialog.tsx` suffix loop for colliding
/// selected server names.
pub fn mcp_desktop_import_final_name(
    server_name: &str,
    existing_server_names: &[String],
) -> String {
    if !existing_server_names
        .iter()
        .any(|existing| existing == server_name)
    {
        return server_name.to_string();
    }

    let mut counter = 1usize;
    loop {
        let candidate = format!("{server_name}_{counter}");
        if !existing_server_names
            .iter()
            .any(|existing| existing == &candidate)
        {
            return candidate;
        }
        counter += 1;
    }
}

pub fn mcp_desktop_import_result(
    selected_servers: &[String],
    existing_server_names: &[String],
    scope: McpConfigScope,
) -> MCPServerDesktopImportResult {
    let final_names = selected_servers
        .iter()
        .map(|server| mcp_desktop_import_final_name(server, existing_server_names))
        .collect::<Vec<_>>();
    MCPServerDesktopImportResult {
        imported_count: final_names.len(),
        final_names,
        scope,
    }
}

pub fn mcp_desktop_import_success_message(count: usize, scope: McpConfigScope) -> String {
    if count > 0 {
        format!(
            "Successfully imported {count} MCP {} to {} config.",
            plural_server(count),
            scope.as_str()
        )
    } else {
        "No servers were imported.".to_string()
    }
}

#[derive(Default, Props)]
pub struct MCPServerDesktopImportDialogProps<'a> {
    pub server_names: Vec<String>,
    pub existing_server_names: Vec<String>,
    pub scope: McpConfigScope,
    pub on_import: HandlerMut<'a, MCPServerDesktopImportResult>,
    pub on_done: HandlerMut<'a, ()>,
}

/// Maps to: CC `components/MCPServerDesktopImportDialog.tsx`.
#[component]
pub fn MCPServerDesktopImportDialog<'a>(
    props: &mut MCPServerDesktopImportDialogProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let mut pending_result = hooks.use_state(|| Option::<MCPServerDesktopImportResult>::None);
    let server_names = props.server_names.clone();
    let existing_server_names = props.existing_server_names.clone();
    let scope = props.scope;
    let collisions = mcp_desktop_import_collisions(&server_names, &existing_server_names);
    let options = mcp_desktop_import_options(&server_names, &collisions);
    let default_value = mcp_desktop_import_default_selection(&server_names, &collisions);

    let pending = {
        let result = pending_result.read();
        result.clone()
    };
    if let Some(result) = pending {
        pending_result.set(None);
        (props.on_import)(result);
        (props.on_done)(());
    }

    let dialog_cancel_scope = scope;
    let select_cancel_scope = scope;
    let submit_scope = scope;
    let submit_existing_server_names = existing_server_names.clone();
    let mut dialog_cancel_pending = pending_result;
    let mut submit_pending = pending_result;
    let mut select_cancel_pending = pending_result;

    element! {
        View(flex_direction: FlexDirection::Column) {
            Dialog(
                title: "Import MCP Servers from Claude Desktop".to_string(),
                subtitle: Some(format!("Found {} MCP {} in Claude Desktop.", server_names.len(), plural_server(server_names.len()))),
                color: Some(theme.success),
                hide_input_guide: true,
                on_cancel: move |_| {
                    dialog_cancel_pending.set(Some(MCPServerDesktopImportResult {
                        imported_count: 0,
                        final_names: Vec::new(),
                        scope: dialog_cancel_scope,
                    }));
                },
            ) {
                #(if collisions.is_empty() {
                    None
                } else {
                    Some(element! {
                        Text(
                            content: "Note: Some servers already exist with the same name. If selected, they will be imported with a numbered suffix.".to_string(),
                            color: theme.warning,
                            wrap: TextWrap::Wrap,
                        )
                    })
                })
                Text(content: "Please select the servers you want to import:".to_string(), wrap: TextWrap::Wrap)
                SelectMulti(
                    options: options,
                    default_value: default_value,
                    hide_indexes: true,
                    handle_escape: Some(false),
                    on_submit: move |selected_servers: Vec<String>| {
                        submit_pending.set(Some(mcp_desktop_import_result(&selected_servers, &submit_existing_server_names, submit_scope)));
                    },
                    on_cancel: move |_| {
                        select_cancel_pending.set(Some(MCPServerDesktopImportResult {
                            imported_count: 0,
                            final_names: Vec::new(),
                            scope: select_cancel_scope,
                        }));
                    },
                )
            }
            View(padding_left: 1u32, padding_right: 1u32) {
                Byline {
                    KeyboardShortcutHint(shortcut: "Space".to_string(), action: "select".to_string())
                    KeyboardShortcutHint(shortcut: "Enter".to_string(), action: "confirm".to_string())
                    ConfigurableShortcutHint(
                        action: "confirm:no".to_string(),
                        context: "Confirmation".to_string(),
                        fallback: "Esc".to_string(),
                        description: "cancel".to_string(),
                    )
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;
    use futures::{StreamExt, stream};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn key(code: KeyCode) -> TerminalEvent {
        TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code))
    }

    #[test]
    fn mcp_desktop_import_helpers_match_official_collision_suffix_and_copy() {
        let names = vec!["fs".to_string(), "git".to_string()];
        let existing = vec!["fs".to_string(), "fs_1".to_string()];
        let collisions = mcp_desktop_import_collisions(&names, &existing);
        assert_eq!(collisions, vec!["fs".to_string()]);
        assert_eq!(
            mcp_desktop_import_default_selection(&names, &collisions),
            vec!["git".to_string()]
        );
        assert_eq!(
            mcp_desktop_import_options(&names, &collisions)[0].label,
            "fs (already exists)"
        );
        assert_eq!(mcp_desktop_import_final_name("fs", &existing), "fs_2");
        assert_eq!(mcp_desktop_import_final_name("git", &existing), "git");
        assert_eq!(
            mcp_desktop_import_success_message(2, McpConfigScope::Project),
            "Successfully imported 2 MCP servers to project config."
        );
        assert_eq!(
            mcp_desktop_import_success_message(0, McpConfigScope::User),
            "No servers were imported."
        );
    }

    #[test]
    fn mcp_desktop_import_dialog_renders_collisions_defaults_and_hints() {
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                MCPServerDesktopImportDialog(
                    server_names: vec!["fs".to_string(), "git".to_string()],
                    existing_server_names: vec!["fs".to_string()],
                    scope: McpConfigScope::Project,
                )
            }
        }
        .render(Some(120))
        .to_string();

        assert!(
            text.contains("Import MCP Servers from Claude Desktop"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("Found 2 MCP servers in Claude Desktop."),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("already exist with the same name"),
            "canvas=\n{text}"
        );
        assert!(text.contains("[ ] fs (already exists)"), "canvas=\n{text}");
        assert!(text.contains("[✓] git"), "canvas=\n{text}");
        assert!(text.contains("Esc to cancel"), "canvas=\n{text}");
    }

    #[test]
    fn mcp_desktop_import_enter_submits_default_non_colliding_servers() {
        let imports = Arc::new(Mutex::new(Vec::<MCPServerDesktopImportResult>::new()));
        let imports_for_handler = Arc::clone(&imports);

        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(*theme::current())) {
                    MCPServerDesktopImportDialog(
                        server_names: vec!["fs".to_string(), "git".to_string()],
                        existing_server_names: vec!["fs".to_string(), "fs_1".to_string()],
                        scope: McpConfigScope::Project,
                        on_import: move |result| imports_for_handler.lock().expect("imports mutex").push(result),
                    )
                }
            };
            let mut render_loop = Box::pin(
                app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(stream::iter(vec![key(KeyCode::Enter)]))
                        .with_size(120, 24),
                ),
            );
            for _ in 0..8 {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(100)).await;
                    None
                })
                .await;
                if next.is_none() {
                    break;
                }
            }
        });

        assert_eq!(
            imports.lock().expect("imports mutex").as_slice(),
            &[MCPServerDesktopImportResult {
                imported_count: 1,
                final_names: vec!["git".to_string()],
                scope: McpConfigScope::Project,
            }]
        );
    }

    #[test]
    fn mcp_desktop_import_escape_cancels_with_zero_imports() {
        let imports = Arc::new(Mutex::new(Vec::<MCPServerDesktopImportResult>::new()));
        let imports_for_handler = Arc::clone(&imports);

        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(
                    crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings()
                )) {
                    ContextProvider(value: Context::owned(*theme::current())) {
                        MCPServerDesktopImportDialog(
                            server_names: vec!["fs".to_string()],
                            scope: McpConfigScope::User,
                            on_import: move |result| imports_for_handler.lock().expect("imports mutex").push(result),
                        )
                    }
                }
            };
            let mut render_loop = Box::pin(
                app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(stream::iter(vec![key(KeyCode::Esc)]))
                        .with_size(120, 24),
                ),
            );
            for _ in 0..8 {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(100)).await;
                    None
                })
                .await;
                if next.is_none() {
                    break;
                }
            }
        });

        assert_eq!(
            imports.lock().expect("imports mutex").as_slice(),
            &[MCPServerDesktopImportResult {
                imported_count: 0,
                final_names: Vec::new(),
                scope: McpConfigScope::User,
            }]
        );
    }
}
