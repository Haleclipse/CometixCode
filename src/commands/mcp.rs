//! `/mcp` command seam.
//! Maps to official `src/commands/mcp/mcp.tsx`.
//!
//! Item-2 state delegates UI to `components/mcp/MCPSettings.tsx` and
//! `MCPReconnect.tsx` equivalents. Toggle/reconnect dispatch stays at the
//! command/component seam and forwards runtime work to `services/mcp/*`, matching
//! official `useMcpToggleEnabled()` / `useMcpReconnect()` boundaries.

use crate::components::mcp::MCPSettings;
use crate::components::mcp::mcp_reconnect::MCPReconnect;
use crate::services::mcp::types::{McpClientSnapshot, McpServerConnectionType};
use crate::utils::process_env::JsTruthy;
use crate::utils::settings::constants::SettingSource;
use crate::utils::settings::get_settings_file_path_for_source;
use crate::utils::status::StartupDiagnosticsSnapshot;
use iocraft::prelude::*;
use serde_json::Value;

#[cfg(test)]
pub const NO_MCP_SERVERS_CONFIGURED: &str =
    crate::components::mcp::mcp_settings::NO_MCP_SERVERS_CONFIGURED;

/// Maps to: CC `commands/mcp/xaaIdpCommand.ts#registerMcpXaaIdpCommand`.
///
/// This is intentionally a thin CLI surface: it parses flags, writes the
/// non-secret `settings.xaaIdp` config, and dispatches secret/cache/login work
/// to `services/mcp/xaa.rs`.
pub fn maybe_run_mcp_xaa_cli(argv: &[String], rt: &tokio::runtime::Runtime) -> Option<i32> {
    let mcp_index = argv.iter().position(|arg| arg == "mcp")?;
    if argv.get(mcp_index + 1).map(String::as_str) != Some("xaa") {
        return None;
    }
    if !crate::services::mcp::xaa_idp_login::is_xaa_enabled() {
        eprintln!("error: unknown command 'xaa'");
        return Some(1);
    }
    let args = &argv[mcp_index + 2..];
    Some(match run_mcp_xaa_cli(args, rt) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("{error}");
            1
        }
    })
}

fn cli_error(message: impl Into<String>) -> anyhow::Error {
    anyhow::anyhow!(message.into())
}

fn option_value(args: &[String], flag: &str) -> Option<String> {
    let prefix = format!("{flag}=");
    for (index, arg) in args.iter().enumerate() {
        if let Some(value) = arg.strip_prefix(&prefix) {
            return Some(value.to_string());
        }
        if arg == flag {
            return args.get(index + 1).cloned();
        }
    }
    None
}

fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|arg| arg == flag)
}

fn validate_setup_issuer(issuer: &str) -> anyhow::Result<()> {
    let trimmed = issuer.trim();
    let (scheme, rest) = trimmed.split_once("://").ok_or_else(|| {
        cli_error(format!(
            "Error: --issuer must be a valid URL (got \"{issuer}\")"
        ))
    })?;
    let host = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .trim();
    if scheme.is_empty() || host.is_empty() || host.contains(char::is_whitespace) {
        return Err(cli_error(format!(
            "Error: --issuer must be a valid URL (got \"{issuer}\")"
        )));
    }
    let host_no_port = if let Some(stripped) = host.strip_prefix('[') {
        stripped
            .find(']')
            .map(|end| &stripped[..end])
            .unwrap_or(stripped)
    } else {
        host.split(':').next().unwrap_or(host)
    };
    let loopback_http = scheme == "http"
        && matches!(
            host_no_port.to_ascii_lowercase().as_str(),
            "localhost" | "127.0.0.1" | "::1"
        );
    if scheme != "https" && !loopback_http {
        return Err(cli_error(format!(
            "Error: --issuer must use https:// (got \"{scheme}://{host}\")"
        )));
    }
    Ok(())
}

fn user_settings_path() -> anyhow::Result<std::path::PathBuf> {
    get_settings_file_path_for_source(SettingSource::User)
        .ok_or_else(|| anyhow::anyhow!("user settings path is unavailable"))
}

fn read_user_settings_value() -> anyhow::Result<Value> {
    let path = user_settings_path()?;
    match std::fs::read_to_string(&path) {
        Ok(content) => Ok(serde_json::from_str::<Value>(&content)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(serde_json::json!({})),
        Err(error) => Err(error.into()),
    }
}

fn write_user_settings_value(value: &Value) -> anyhow::Result<()> {
    let path = user_settings_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(value)?)?;
    Ok(())
}

fn update_user_xaa_idp(
    settings: Option<crate::utils::settings::types::XaaIdpSettings>,
) -> anyhow::Result<()> {
    // Maps to: CC `updateSettingsForSource('userSettings', { xaaIdp: ... })`.
    let mut value = read_user_settings_value()?;
    if !value.is_object() {
        value = serde_json::json!({});
    }
    let object = value.as_object_mut().expect("object checked above");
    if let Some(settings) = settings {
        let mut xaa = serde_json::Map::new();
        xaa.insert("issuer".to_string(), Value::String(settings.issuer));
        xaa.insert("clientId".to_string(), Value::String(settings.client_id));
        if let Some(port) = settings.callback_port {
            xaa.insert(
                "callbackPort".to_string(),
                Value::Number(serde_json::Number::from(port)),
            );
        }
        object.insert("xaaIdp".to_string(), Value::Object(xaa));
    } else {
        object.remove("xaaIdp");
    }
    write_user_settings_value(&value)
}

fn parse_callback_port(args: &[String]) -> anyhow::Result<Option<u16>> {
    let Some(raw) = option_value(args, "--callback-port") else {
        return Ok(None);
    };
    raw.parse::<u16>()
        .ok()
        .filter(|port| *port > 0)
        .map(Some)
        .ok_or_else(|| cli_error("Error: --callback-port must be a positive integer"))
}

fn format_iso_millis(ms: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms)
        .unwrap_or_else(chrono::Utc::now)
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn run_xaa_setup(args: &[String]) -> anyhow::Result<()> {
    let issuer = option_value(args, "--issuer")
        .ok_or_else(|| cli_error("Error: missing required option '--issuer <url>'"))?;
    let client_id = option_value(args, "--client-id")
        .ok_or_else(|| cli_error("Error: missing required option '--client-id <id>'"))?;
    validate_setup_issuer(&issuer)?;
    let callback_port = parse_callback_port(args)?;
    let secret = if has_flag(args, "--client-secret") {
        Some(
            crate::utils::process_env::var("MCP_XAA_IDP_CLIENT_SECRET")
                .truthy()
                .ok_or_else(|| {
                    cli_error("Error: --client-secret requires MCP_XAA_IDP_CLIENT_SECRET env var")
                })?,
        )
    } else {
        None
    };

    let old = crate::services::mcp::xaa_idp_login::get_xaa_idp_settings();
    let old_issuer = old.as_ref().map(|value| value.issuer.clone());
    let old_client_id = old.as_ref().map(|value| value.client_id.clone());

    update_user_xaa_idp(Some(crate::utils::settings::types::XaaIdpSettings {
        issuer: issuer.clone(),
        client_id: client_id.clone(),
        callback_port,
    }))
    .map_err(|error| cli_error(format!("Error writing settings: {error}")))?;

    if let Some(old_issuer) = old_issuer {
        if crate::services::mcp::xaa_idp_login::issuer_key(&old_issuer)
            != crate::services::mcp::xaa_idp_login::issuer_key(&issuer)
        {
            crate::services::mcp::xaa_idp_login::clear_idp_id_token(&old_issuer)?;
            crate::services::mcp::xaa_idp_login::clear_idp_client_secret(&old_issuer)?;
        } else if old_client_id.as_deref() != Some(client_id.as_str()) {
            crate::services::mcp::xaa_idp_login::clear_idp_id_token(&old_issuer)?;
            crate::services::mcp::xaa_idp_login::clear_idp_client_secret(&old_issuer)?;
        }
    }

    if let Some(secret) = secret {
        crate::services::mcp::xaa_idp_login::save_idp_client_secret(&issuer, &secret).map_err(|error| {
            cli_error(format!(
                "Error: settings written but keychain save failed — {error}. Re-run with --client-secret once keychain is available."
            ))
        })?;
    }
    println!("XAA IdP connection configured for {issuer}");
    Ok(())
}

fn run_xaa_login(args: &[String], rt: &tokio::runtime::Runtime) -> anyhow::Result<()> {
    let idp = crate::services::mcp::xaa_idp_login::get_xaa_idp_settings().ok_or_else(|| {
        cli_error("Error: no XAA IdP connection. Run 'claude mcp xaa setup' first.")
    })?;
    if let Some(id_token) = option_value(args, "--id-token") {
        let expires_at = crate::services::mcp::xaa_idp_login::save_idp_id_token_from_jwt(
            &idp.issuer,
            &id_token,
        )?;
        println!(
            "id_token cached for {} (expires {})",
            idp.issuer,
            format_iso_millis(expires_at)
        );
        return Ok(());
    }
    if has_flag(args, "--force") {
        crate::services::mcp::xaa_idp_login::clear_idp_id_token(&idp.issuer)?;
    }
    if crate::services::mcp::xaa_idp_login::get_cached_idp_id_token(&idp.issuer).is_some() {
        println!(
            "Already logged in to {} (cached id_token still valid). Use --force to re-login.",
            idp.issuer
        );
        return Ok(());
    }
    println!("Opening browser for IdP login at {}…", idp.issuer);
    let issuer_for_callback = idp.issuer.clone();
    let result = rt.block_on(crate::services::mcp::xaa_idp_login::acquire_idp_id_token(
        crate::services::mcp::xaa_idp_login::IdpLoginOptions {
            idp_issuer: idp.issuer.clone(),
            idp_client_id: idp.client_id,
            idp_client_secret: crate::services::mcp::xaa_idp_login::get_idp_client_secret(
                &idp.issuer,
            ),
            callback_port: idp.callback_port,
            on_authorization_url: Some(std::sync::Arc::new(move |url| {
                let _ = &issuer_for_callback;
                println!("If the browser did not open, visit:\n  {url}");
            })),
            skip_browser_open: false,
        },
    ));
    match result {
        Ok(_) => {
            println!("Logged in. MCP servers with --xaa will now authenticate silently.");
            Ok(())
        }
        Err(error) => Err(cli_error(format!("IdP login failed: {error}"))),
    }
}

fn run_xaa_show() -> anyhow::Result<()> {
    let Some(idp) = crate::services::mcp::xaa_idp_login::get_xaa_idp_settings() else {
        println!("No XAA IdP connection configured.");
        return Ok(());
    };
    let has_secret =
        crate::services::mcp::xaa_idp_login::get_idp_client_secret(&idp.issuer).is_some();
    let has_id_token =
        crate::services::mcp::xaa_idp_login::get_cached_idp_id_token(&idp.issuer).is_some();
    println!("Issuer:        {}", idp.issuer);
    println!("Client ID:     {}", idp.client_id);
    if let Some(port) = idp.callback_port {
        println!("Callback port: {port}");
    }
    println!(
        "Client secret: {}",
        if has_secret {
            "(stored in keychain)"
        } else {
            "(not set — PKCE-only)"
        }
    );
    println!(
        "Logged in:     {}",
        if has_id_token {
            "yes (id_token cached)"
        } else {
            "no — run 'claude mcp xaa login'"
        }
    );
    Ok(())
}

fn run_xaa_clear() -> anyhow::Result<()> {
    let idp = crate::services::mcp::xaa_idp_login::get_xaa_idp_settings();
    update_user_xaa_idp(None)
        .map_err(|error| cli_error(format!("Error writing settings: {error}")))?;
    if let Some(idp) = idp {
        crate::services::mcp::xaa_idp_login::clear_idp_id_token(&idp.issuer)?;
        crate::services::mcp::xaa_idp_login::clear_idp_client_secret(&idp.issuer)?;
    }
    println!("XAA IdP connection cleared");
    Ok(())
}

fn run_mcp_xaa_cli(args: &[String], rt: &tokio::runtime::Runtime) -> anyhow::Result<()> {
    match args.first().map(String::as_str) {
        Some("setup") => run_xaa_setup(&args[1..]),
        Some("login") => run_xaa_login(&args[1..], rt),
        Some("show") => run_xaa_show(),
        Some("clear") => run_xaa_clear(),
        Some(other) => Err(cli_error(format!("error: unknown command '{other}'"))),
        None => Err(cli_error(
            "error: missing XAA command (expected setup, login, show, or clear)",
        )),
    }
}

fn parts(args: &str) -> Vec<&str> {
    args.trim().split_whitespace().collect()
}

/// Returns the official `onComplete(...)` output for the current no-client MCP
/// state. This mirrors the visible strings from `MCPSettings`, `MCPToggle`, and
/// `MCPReconnect` without instantiating MCP clients or touching settings.
#[cfg(test)]
fn local_output_for_args(args: &str) -> String {
    let parts = parts(args);
    match parts.as_slice() {
        [] | ["no-redirect"] => NO_MCP_SERVERS_CONFIGURED.to_string(),
        ["reconnect", rest @ ..] if !rest.is_empty() => {
            format!("MCP server \"{}\" not found", rest.join(" "))
        }
        ["enable"] => "All MCP servers are already enabled".to_string(),
        ["disable"] => "All MCP servers are already disabled".to_string(),
        ["enable", rest @ ..] | ["disable", rest @ ..] if !rest.is_empty() => {
            let target = rest.join(" ");
            if target == "all" {
                if parts[0] == "enable" {
                    "All MCP servers are already enabled".to_string()
                } else {
                    "All MCP servers are already disabled".to_string()
                }
            } else {
                format!("MCP server \"{target}\" not found")
            }
        }
        _ => NO_MCP_SERVERS_CONFIGURED.to_string(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct McpTogglePlan {
    names: Vec<String>,
    result: String,
}

/// Maps to: CC `commands/mcp/mcp.tsx#MCPToggle` target selection and
/// `onComplete(...)` string generation.
fn mcp_toggle_plan_for_clients(
    action: &str,
    target: &str,
    clients: &[McpClientSnapshot],
) -> McpTogglePlan {
    let is_enabling = action == "enable";
    let clients = clients
        .iter()
        .filter(|client| client.name != "ide")
        .collect::<Vec<_>>();
    let names = if target == "all" {
        clients
            .iter()
            .filter(|client| {
                if is_enabling {
                    client.status == McpServerConnectionType::Disabled
                } else {
                    client.status != McpServerConnectionType::Disabled
                }
            })
            .map(|client| client.name.clone())
            .collect::<Vec<_>>()
    } else {
        clients
            .iter()
            .filter(|client| client.name == target)
            .map(|client| client.name.clone())
            .collect::<Vec<_>>()
    };

    let result = if names.is_empty() {
        if target == "all" {
            format!(
                "All MCP servers are already {}",
                if is_enabling { "enabled" } else { "disabled" }
            )
        } else {
            format!("MCP server \"{target}\" not found")
        }
    } else if target == "all" {
        format!(
            "{} {} MCP server(s)",
            if is_enabling { "Enabled" } else { "Disabled" },
            names.len()
        )
    } else {
        format!(
            "MCP server \"{target}\" {}",
            if is_enabling { "enabled" } else { "disabled" }
        )
    };

    McpTogglePlan { names, result }
}

fn dispatch_mcp_toggle_plan(
    plan: &McpTogglePlan,
    toggle_mcp_server: crate::services::mcp::mcp_connection_manager::ToggleMcpServer,
) {
    if plan.names.is_empty() {
        return;
    }

    for name in plan.names.clone() {
        // Maps to: CC `commands/mcp/mcp.tsx:21,` — dispatch through
        // `useMcpToggleEnabled()`. The manager owns config lookup (cached
        // getGlobalConfig-equivalent), pending update, persisted enable
        // mutation, and reconnect/disable lifecycle.
        let toggle_mcp_server = toggle_mcp_server.clone();
        tokio::spawn(async move {
            let _ = toggle_mcp_server.call(&name).await;
        });
    }
}

#[derive(Default, Props)]
pub struct McpCommandPanelProps<'a> {
    pub args: String,
    pub on_close: HandlerMut<'a, ()>,
    pub on_result: HandlerMut<'a, String>,
}

#[component]
pub fn McpCommandPanel<'a>(
    props: &mut McpCommandPanelProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    // Maps to: CC `commands/mcp/mcp.tsx:20-21` — `useAppState(s => s.mcp.clients)`
    // for the read and `useMcpToggleEnabled()` for the write. There is no
    // `useMcp` at the source.
    let runtime_mcp_clients =
        crate::state::app_state::use_app_state(&mut hooks, |state| state.mcp.client_snapshots());
    let toggle_mcp_server =
        crate::services::mcp::mcp_connection_manager::use_mcp_toggle_enabled(&mut hooks);
    let runtime_config = hooks
        .try_use_context::<StartupDiagnosticsSnapshot>()
        .map(|ctx| (*ctx).clone());
    let mut pending_result = hooks.use_state(|| Option::<String>::None);
    let mut did_run_effect = hooks.use_state(|| false);

    let pending_result_value = { pending_result.read().clone() };
    if let Some(result) = pending_result_value {
        pending_result.set(None);
        (props.on_result)(result);
    }

    let args = props.args.trim().to_string();
    let arg_parts = parts(&args);

    if arg_parts.is_empty() || arg_parts == ["no-redirect"] {
        let complete_state = pending_result;
        return element! {
            MCPSettings(on_complete: move |result: String| {
                let mut complete_state = complete_state;
                complete_state.set(Some(result));
            })
        }
        .into_any();
    }

    if let ["reconnect", rest @ ..] = arg_parts.as_slice() {
        if !rest.is_empty() {
            let complete_state = pending_result;
            return element! {
                MCPReconnect(
                    server_name: rest.join(" "),
                    on_complete: move |result: String| {
                        let mut complete_state = complete_state;
                        complete_state.set(Some(result));
                    },
                )
            }
            .into_any();
        }
    }

    if matches!(arg_parts.first().copied(), Some("enable" | "disable")) {
        let action = arg_parts[0];
        let target = if arg_parts.len() > 1 {
            arg_parts[1..].join(" ")
        } else {
            "all".to_string()
        };
        // CC `commands/mcp/mcp.tsx:20` reads `s.mcp.clients` and has no fallback
        // — the startup snapshot only covers the window before any client has
        // been recorded in AppState.
        let clients = if runtime_mcp_clients.is_empty() {
            runtime_config
                .as_ref()
                .map(|ctx| ctx.mcp_clients.clone())
                .unwrap_or_default()
        } else {
            runtime_mcp_clients.clone()
        };
        let plan = mcp_toggle_plan_for_clients(action, &target, &clients);
        if !did_run_effect.get() {
            did_run_effect.set(true);
            dispatch_mcp_toggle_plan(&plan, toggle_mcp_server);
            (props.on_result)(plan.result.clone());
        }
        return element! { View {} }.into_any();
    }

    let complete_state = pending_result;
    element! {
        MCPSettings(on_complete: move |result: String| {
            let mut complete_state = complete_state;
            complete_state.set(Some(result));
        })
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};
    use crate::utils::theme;
    use futures::{StreamExt, stream};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn with_temp_config_home(test: impl FnOnce(&std::path::Path)) {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!(
            "cometix-mcp-xaa-command-test-{}-{}",
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let _config_guard = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &dir);
        test(&dir);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn canvas_text(canvas: &Canvas) -> String {
        canvas.to_string()
    }

    #[test]
    fn xaa_setup_validation_matches_official_https_and_loopback_rules() {
        assert!(validate_setup_issuer("https://idp.example.com/tenant").is_ok());
        assert!(validate_setup_issuer("http://localhost:8080/issuer").is_ok());
        assert!(validate_setup_issuer("http://127.0.0.1:8080/issuer").is_ok());
        assert!(validate_setup_issuer("http://[::1]:8080/issuer").is_ok());
        let error = validate_setup_issuer("http://idp.example.com/issuer")
            .expect_err("remote plaintext issuer must be rejected");
        assert!(
            error.to_string().contains("must use https://"),
            "error={error}"
        );
    }

    #[test]
    fn xaa_setup_writes_user_settings_and_removes_stale_callback_port() {
        with_temp_config_home(|home| {
            std::fs::write(
                home.join("settings.json"),
                r#"{"model":"sonnet","xaaIdp":{"issuer":"https://old.example.com/","clientId":"old","callbackPort":3118}}"#,
            )
            .unwrap();

            run_xaa_setup(&[
                "--issuer".to_string(),
                "https://IDP.example.com/tenant/".to_string(),
                "--client-id".to_string(),
                "cc-id".to_string(),
            ])
            .unwrap();

            let value: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(home.join("settings.json")).unwrap())
                    .unwrap();
            assert_eq!(
                value.get("model").and_then(serde_json::Value::as_str),
                Some("sonnet")
            );
            let xaa = value.get("xaaIdp").expect("xaaIdp written");
            assert_eq!(
                xaa.get("issuer").and_then(serde_json::Value::as_str),
                Some("https://IDP.example.com/tenant/")
            );
            assert_eq!(
                xaa.get("clientId").and_then(serde_json::Value::as_str),
                Some("cc-id")
            );
            assert!(
                xaa.get("callbackPort").is_none(),
                "callbackPort must not leak from old setup: {xaa}"
            );
        });
    }

    #[test]
    fn mcp_outputs_match_official_no_client_on_complete_paths() {
        assert_eq!(local_output_for_args(""), NO_MCP_SERVERS_CONFIGURED);
        assert_eq!(
            local_output_for_args("no-redirect"),
            NO_MCP_SERVERS_CONFIGURED
        );
        assert_eq!(
            local_output_for_args("enable"),
            "All MCP servers are already enabled"
        );
        assert_eq!(
            local_output_for_args("disable"),
            "All MCP servers are already disabled"
        );
        assert_eq!(
            local_output_for_args("enable docs server"),
            "MCP server \"docs server\" not found"
        );
        assert_eq!(
            local_output_for_args("reconnect docs"),
            "MCP server \"docs\" not found"
        );
    }

    #[test]
    fn mcp_toggle_plan_matches_official_mcp_toggle_selection_and_copy() {
        let clients = vec![
            McpClientSnapshot {
                name: "enabled".to_string(),
                status: McpServerConnectionType::Connected,
                reconnect_attempt: None,
                max_reconnect_attempts: None,
                ide_name: None,
                server_version: None,
                error: None,
            },
            McpClientSnapshot {
                name: "disabled".to_string(),
                status: McpServerConnectionType::Disabled,
                reconnect_attempt: None,
                max_reconnect_attempts: None,
                ide_name: None,
                server_version: None,
                error: None,
            },
            McpClientSnapshot {
                name: "ide".to_string(),
                status: McpServerConnectionType::Connected,
                reconnect_attempt: None,
                max_reconnect_attempts: None,
                ide_name: Some("IDE".to_string()),
                server_version: None,
                error: None,
            },
        ];

        assert_eq!(
            mcp_toggle_plan_for_clients("enable", "all", &clients),
            McpTogglePlan {
                names: vec!["disabled".to_string()],
                result: "Enabled 1 MCP server(s)".to_string(),
            }
        );
        assert_eq!(
            mcp_toggle_plan_for_clients("disable", "all", &clients),
            McpTogglePlan {
                names: vec!["enabled".to_string()],
                result: "Disabled 1 MCP server(s)".to_string(),
            }
        );
        assert_eq!(
            mcp_toggle_plan_for_clients("enable", "enabled", &clients),
            McpTogglePlan {
                names: vec!["enabled".to_string()],
                result: "MCP server \"enabled\" enabled".to_string(),
            }
        );
        assert_eq!(
            mcp_toggle_plan_for_clients("disable", "missing", &clients).result,
            "MCP server \"missing\" not found"
        );
    }

    #[test]
    fn mcp_panel_no_servers_completes_with_official_message_without_ui_only_panel() {
        let results = Arc::new(Mutex::new(Vec::<String>::new()));
        let results_for_handler = Arc::clone(&results);
        let last = Arc::new(Mutex::new(String::new()));
        let last_for_loop = Arc::clone(&last);

        futures::executor::block_on(async move {
            let current_theme = *theme::current();
            let mut app = element! {
                ContextProvider(value: Context::owned(current_theme)) {
                    // Default state IS the fixture: this asserts the
                    // no-servers path completes, and default AppState has no
                    // MCP clients.
                    crate::state::app_state::AppStateProvider(
                        children: crate::state::app_state::ProviderChildren::new(move || {
                            let results_for_handler = Arc::clone(&results_for_handler);
                            element! {
                                crate::services::mcp::mcp_connection_manager::McpConnectionManager() {
                                    McpCommandPanel(
                                        args: String::new(),
                                        on_result: move |result: String| results_for_handler.lock().expect("results mutex").push(result),
                                    )
                                }
                            }.into_any()
                        }),
                    )
                }
            };
            let mut render_loop = Box::pin(
                app.mock_terminal_render_loop(MockTerminalConfig::default().with_size(120, 24)),
            );
            for _ in 0..6 {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(100)).await;
                    None
                })
                .await;
                let Some(canvas) = next else {
                    break;
                };
                *last_for_loop.lock().expect("last canvas mutex") = canvas.to_string();
            }
        });

        let last = last.lock().expect("last canvas mutex").clone();
        assert_eq!(
            results.lock().expect("results mutex").as_slice(),
            &[NO_MCP_SERVERS_CONFIGURED.to_string()]
        );
        assert!(!last.contains("UI-only build"), "canvas=\n{last}");
        assert!(!last.contains("Manage MCP servers"), "canvas=\n{last}");
    }

    #[test]
    fn mcp_panel_unknown_args_falls_back_to_live_settings_state_like_official_call() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let temp_dir =
            std::env::temp_dir().join(format!("cometix-mcp-panel-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();

        struct ProcessStateGuard(std::path::PathBuf);
        impl Drop for ProcessStateGuard {
            fn drop(&mut self) {
                let _ = std::env::set_current_dir(&self.0);
                crate::utils::config::set_test_global_config(None);
            }
        }

        let _process_state = ProcessStateGuard(std::env::current_dir().unwrap());
        std::env::set_current_dir(&temp_dir).unwrap();

        let current_theme = *theme::current();
        let mut global_config = crate::utils::config::GlobalConfig::default();
        global_config.mcp_servers = Some(serde_json::json!({"docs":{"command":"docs-mcp"}}));
        crate::utils::config::set_test_global_config(Some(global_config));
        let runtime_config = crate::utils::status::StartupDiagnosticsSnapshot::default();

        let canvas = element! {
            ContextProvider(value: Context::owned(current_theme)) {
                ContextProvider(value: Context::owned(runtime_config)) {
                    // AppStateProvider is OUTSIDE the manager, matching
                    // production nesting (Provider -> REPL -> manager).
                    // Default state is the fixture: the servers this test
                    // asserts on come from the global config, not AppState.
                    crate::state::app_state::AppStateProvider(
                        children: crate::state::app_state::ProviderChildren::new(|| element! {
                            crate::services::mcp::mcp_connection_manager::McpConnectionManager() {
                                McpCommandPanel(args: "unexpected".to_string())
                            }
                        }.into_any()),
                    )
                }
            }
        }
        .render(Some(120));
        let text = canvas_text(&canvas);

        assert!(text.contains("Manage MCP servers"), "canvas=\n{text}");
        assert!(text.contains("1 server"), "canvas=\n{text}");
        assert!(text.contains("docs"), "canvas=\n{text}");
        assert!(!text.contains("0 servers"), "canvas=\n{text}");
        assert_eq!(
            text.matches("↑↓ to navigate").count(),
            1,
            "the command wrapper must not duplicate MCPListPanel's footer: canvas=\n{text}"
        );

        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn mcp_panel_esc_completes_through_mcp_settings_without_outer_footer_or_cancel_owner() {
        let current_theme = *theme::current();
        let close_count = Arc::new(Mutex::new(0usize));
        let close_for_handler = Arc::clone(&close_count);
        let results = Arc::new(Mutex::new(Vec::<String>::new()));
        let results_for_handler = Arc::clone(&results);
        let diagnostics = StartupDiagnosticsSnapshot {
            mcp_clients: vec![McpClientSnapshot {
                name: "docs".to_string(),
                status: McpServerConnectionType::Pending,
                reconnect_attempt: None,
                max_reconnect_attempts: None,
                ide_name: None,
                server_version: None,
                error: None,
            }],
            ..StartupDiagnosticsSnapshot::default()
        };

        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(
                    crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings()
                )) {
                    ContextProvider(value: Context::owned(current_theme)) {
                        ContextProvider(value: Context::owned(diagnostics)) {
                            // Default state is the fixture: the pending client
                            // this test drives Esc against comes from the
                            // diagnostics snapshot above, not from AppState.
                            crate::state::app_state::AppStateProvider(
                                children: crate::state::app_state::ProviderChildren::new(move || {
                                    let close_for_handler = Arc::clone(&close_for_handler);
                                    let results_for_handler = Arc::clone(&results_for_handler);
                                    element! {
                                        crate::services::mcp::mcp_connection_manager::McpConnectionManager() {
                                            McpCommandPanel(
                                            args: String::new(),
                                            on_close: move |_| {
                                                *close_for_handler.lock().expect("close mutex") += 1;
                                            },
                                            on_result: move |result| {
                                                results_for_handler.lock().expect("result mutex").push(result);
                                            },
                                            )
                                        }
                                    }.into_any()
                                }),
                            )
                        }
                    }
                }
            };
            let mut render_loop = Box::pin(
                app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(stream::iter(vec![TerminalEvent::Key(
                        KeyEvent::new(KeyEventKind::Press, KeyCode::Esc),
                    )]))
                    .with_size(100, 24),
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

        assert_eq!(*close_count.lock().expect("close mutex"), 0);
        assert_eq!(
            results.lock().expect("result mutex").as_slice(),
            &["MCP dialog dismissed".to_string()]
        );
    }
}
