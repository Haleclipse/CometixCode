//! Implementation of `/provider` slash command.

use indexmap::IndexMap;
use tracing::warn;

use crate::commands::Command;
use crate::services::proxy_channel::{
    all_channels, apply_active_channel_env, deactivate_active_channel_env, get_active_channel_id,
    get_channel, load_channel_config, save_channel_config, save_channel_models_cache,
    set_active_channel_id, update_channel_env, ChannelConfigFile, ChannelModelsCache,
};
use crate::tool::ToolUseContext;
use crate::types::message::{RenderableMessage, RenderableMessageKind, SystemMessage};
use crate::utils::process_user_input::ProcessUserInputBaseResult;
use uuid::Uuid;

pub const NAME: &str = "provider";
pub const DESCRIPTION: &str =
    "Manage AI proxy channels (e.g. /provider cpa, /provider off, /provider status)";
pub const ARGUMENT_HINT: &str = "[cpa|off|status] [base_url] [api_key]";

/// Dispatches `/provider` command execution.
pub fn call(
    command: &Command,
    args: &str,
    uuid: Option<String>,
    _context: &ToolUseContext,
) -> ProcessUserInputBaseResult {
    let output = handle_provider_command(args.trim());
    format_command_result(uuid, command.name.as_ref(), args, &output)
}

fn handle_provider_command(args: &str) -> String {
    let tokens: Vec<&str> = args.split_whitespace().collect();
    if tokens.is_empty() || tokens[0].eq_ignore_ascii_case("status") {
        return render_status_view();
    }

    let sub = tokens[0].to_ascii_lowercase();
    match sub.as_str() {
        "off" | "disable" | "none" | "stop" => {
            if let Err(e) = set_active_channel_id(None) {
                return format!("❌ 停用代理渠道失败: {e}");
            }
            deactivate_active_channel_env();
            "✓ 所有代理渠道已停用，已切回原生直连配置。".to_string()
        }
        "refresh" => {
            let Some(active_id) = get_active_channel_id() else {
                return "ℹ 当前未激活任何代理渠道，无需刷新。".to_string();
            };
            let Some(cfg) = load_channel_config(&active_id) else {
                return format!("❌ 活跃渠道 '{active_id}' 无配置文件。");
            };
            let base = cfg
                .env
                .get("ANTHROPIC_BASE_URL")
                .cloned()
                .unwrap_or_default();
            let key = cfg
                .env
                .get("ANTHROPIC_API_KEY")
                .cloned()
                .unwrap_or_default();
            if let Some(channel) = get_channel(&active_id) {
                if let Ok(ep) = channel.normalize_endpoints(&base) {
                    trigger_background_models_refresh(channel.id().to_string(), ep.clone(), key);
                    return format!(
                        "✓ 已触发渠道 '{active_id}' 的模型刷新 ({})，正在后台更新...",
                        ep.models_url
                    );
                }
            }
            "❌ 刷新模型失败：无法解析当前渠道端点。".to_string()
        }
        channel_id if tokens.len() == 2 && tokens[1].eq_ignore_ascii_case("refresh") => {
            if let Some(cfg) = load_channel_config(channel_id) {
                let base = cfg
                    .env
                    .get("ANTHROPIC_BASE_URL")
                    .cloned()
                    .unwrap_or_default();
                let key = cfg
                    .env
                    .get("ANTHROPIC_API_KEY")
                    .cloned()
                    .unwrap_or_default();
                if let Some(channel) = get_channel(channel_id) {
                    if let Ok(ep) = channel.normalize_endpoints(&base) {
                        trigger_background_models_refresh(channel.id().to_string(), ep.clone(), key);
                        return format!(
                            "✓ 已触发渠道 '{channel_id}' 的模型刷新 ({})，正在后台更新...",
                            ep.models_url
                        );
                    }
                }
            }
            format!("❌ 刷新模型失败：渠道 '{channel_id}' 未配置或端点无效。")
        }
        channel_id => {
            let channel_opt = get_channel(channel_id);
            let display_name = channel_opt.map(|c| c.display_name()).unwrap_or(channel_id);

            // Case A: User supplied connection arguments: `/provider <id> <base_url> [api_key]`
            if tokens.len() >= 2 {
                let base_url_input = tokens[1];
                let api_key_input = tokens.get(2).copied().unwrap_or("");

                let endpoints = if let Some(channel) = channel_opt {
                    match channel.normalize_endpoints(base_url_input) {
                        Ok(ep) => ep,
                        Err(e) => return format!("❌ Base URL 解析失败: {e}"),
                    }
                } else {
                    let raw = base_url_input.trim_end_matches('/');
                    let url = if !raw.starts_with("http://") && !raw.starts_with("https://") {
                        format!("http://{raw}")
                    } else {
                        raw.to_string()
                    };
                    crate::services::proxy_channel::types::Endpoints {
                        inference_base_url: url.clone(),
                        models_url: format!("{url}/v1/models"),
                    }
                };

                let mut env = IndexMap::new();
                env.insert(
                    "ANTHROPIC_BASE_URL".to_string(),
                    endpoints.inference_base_url.clone(),
                );
                if !api_key_input.is_empty() {
                    env.insert("ANTHROPIC_API_KEY".to_string(), api_key_input.to_string());
                }

                // Preserve existing model mappings if present
                if let Some(existing) = load_channel_config(channel_id) {
                    for key in &[
                        "ANTHROPIC_DEFAULT_SONNET_MODEL",
                        "ANTHROPIC_DEFAULT_SONNET_1M_MODEL",
                        "ANTHROPIC_DEFAULT_OPUS_MODEL",
                        "ANTHROPIC_DEFAULT_OPUS_1M_MODEL",
                        "ANTHROPIC_DEFAULT_HAIKU_MODEL",
                    ] {
                        if let Some(val) = existing.env.get(*key) {
                            env.insert(key.to_string(), val.clone());
                        }
                    }
                }

                if let Err(e) = save_channel_config(channel_id, &ChannelConfigFile { env }) {
                    return format!("❌ 保存渠道配置失败: {e}");
                }

                if let Err(e) = set_active_channel_id(Some(channel_id)) {
                    return format!("❌ 激活渠道失败: {e}");
                }

                apply_active_channel_env();

                // Spawn background model discovery if channel implementation is present
                trigger_background_models_refresh(channel_id.to_string(), endpoints.clone(), api_key_input.to_string());

                format!(
                    "✓ 代理渠道 '{display_name}' ({channel_id}) 已保存并激活！\n  • 基础地址: {}\n  • 正在后台同步模型列表...",
                    endpoints.inference_base_url
                )
            } else {
                // Case B: User requested activation: `/provider <id>`
                if let Some(config) = load_channel_config(channel_id) {
                    let base_url = config
                        .env
                        .get("ANTHROPIC_BASE_URL")
                        .cloned()
                        .unwrap_or_else(|| "未指定".to_string());

                    if let Err(e) = set_active_channel_id(Some(channel_id)) {
                        return format!("❌ 激活渠道失败: {e}");
                    }
                    apply_active_channel_env();

                    // Trigger background refresh with existing credentials
                    if let Some(channel) = channel_opt {
                        if let Ok(ep) = channel.normalize_endpoints(&base_url) {
                            let key = config.env.get("ANTHROPIC_API_KEY").cloned().unwrap_or_default();
                            trigger_background_models_refresh(channel_id.to_string(), ep, key);
                        }
                    }

                    format!(
                        "✓ 已激活代理渠道 '{display_name}' ({channel_id})。\n  • 当前地址: {base_url}\n  • 如需修改模型档位映射，请在 /model 界面中操作。"
                    )
                } else {
                    let default_url = channel_opt
                        .map(|c| c.default_base_url())
                        .unwrap_or("http://127.0.0.1:8317");
                    format!(
                        "ℹ 渠道 '{display_name}' ({channel_id}) 尚未配置。\n\n请按如下格式快速配置并激活：\n  /provider {channel_id} <base_url> <api_key>\n\n示例：\n  /provider {channel_id} {default_url} 12345"
                    )
                }
            }
        }
    }
}

fn trigger_background_models_refresh(channel_id: String, ep: crate::services::proxy_channel::types::Endpoints, api_key: String) {
    tokio::spawn(async move {
        if let Some(channel) = get_channel(&channel_id) {
            match channel.fetch_models(&ep, &api_key).await {
                Ok(models) => {
                    let fetched_at = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as u64)
                        .unwrap_or(0);

                    let cache = ChannelModelsCache {
                        base_url: ep.inference_base_url.clone(),
                        fetched_at,
                        models,
                    };
                    let _ = save_channel_models_cache(&channel_id, &cache);
                }
                Err(err) => {
                    warn!("Background fetch models for '{channel_id}' failed: {err}");
                }
            }
        }
    });
}

fn render_status_view() -> String {
    let active_id = get_active_channel_id();
    let mut out = String::new();
    out.push_str("=== 代理渠道状态看板 ===\n");

    match &active_id {
        Some(id) => {
            let channel_name = get_channel(id).map(|c| c.display_name()).unwrap_or(id.as_str());
            out.push_str(&format!("● 当前激活渠道: {channel_name} ({id})\n"));
            if let Some(cfg) = load_channel_config(id) {
                let base = cfg.env.get("ANTHROPIC_BASE_URL").map(|s| s.as_str()).unwrap_or("(未设置)");
                out.push_str(&format!("  • 推理地址 (Base URL): {base}\n"));

                let sonnet = cfg.env.get("ANTHROPIC_DEFAULT_SONNET_MODEL").map(|s| s.as_str()).unwrap_or("默认");
                let sonnet_1m = cfg.env.get("ANTHROPIC_DEFAULT_SONNET_1M_MODEL").map(|s| s.as_str()).unwrap_or("(跟随 Sonnet[1m])");
                let opus = cfg.env.get("ANTHROPIC_DEFAULT_OPUS_MODEL").map(|s| s.as_str()).unwrap_or("默认");
                let opus_1m = cfg.env.get("ANTHROPIC_DEFAULT_OPUS_1M_MODEL").map(|s| s.as_str()).unwrap_or("(跟随 Opus[1m])");
                let haiku = cfg.env.get("ANTHROPIC_DEFAULT_HAIKU_MODEL").map(|s| s.as_str()).unwrap_or("默认");

                out.push_str("  • 档位模型映射:\n");
                out.push_str(&format!("    - Sonnet    : {sonnet}\n"));
                out.push_str(&format!("    - Sonnet 1M : {sonnet_1m}\n"));
                out.push_str(&format!("    - Opus      : {opus}\n"));
                out.push_str(&format!("    - Opus 1M   : {opus_1m}\n"));
                out.push_str(&format!("    - Haiku     : {haiku}\n"));
            }
        }
        None => {
            out.push_str("○ 当前激活渠道: 原生直连 (未激活代理)\n");
        }
    }

    out.push_str("\n可用指令:\n");
    out.push_str("  /provider cpa                      - 激活 CLIProxyAPI (未配置时提示语法)\n");
    out.push_str("  /provider cpa <base_url> <api_key> - 配置并激活 CLIProxyAPI\n");
    out.push_str("  /provider refresh                  - 刷新当前活跃渠道的远端模型列表\n");
    out.push_str("  /provider other                    - 激活通用或自定义代理渠道\n");
    out.push_str("  /provider off                      - 停用所有代理，切回原生直连\n");
    out.push_str("  /model                             - 选择模型或按 M / Tab 键修改档位模型映射\n");

    out
}

fn format_command_result(
    uuid: Option<String>,
    command_name: &str,
    args: &str,
    output: &str,
) -> ProcessUserInputBaseResult {
    use crate::constants::xml::{
        COMMAND_ARGS_TAG, COMMAND_MESSAGE_TAG, COMMAND_NAME_TAG, LOCAL_COMMAND_STDOUT_TAG,
    };

    let input = format!(
        "<{COMMAND_NAME_TAG}>/{command_name}</{COMMAND_NAME_TAG}>\n<{COMMAND_MESSAGE_TAG}>{command_name}</{COMMAND_MESSAGE_TAG}>\n<{COMMAND_ARGS_TAG}>{args}</{COMMAND_ARGS_TAG}>"
    );

    ProcessUserInputBaseResult {
        messages: vec![
            RenderableMessage {
                uuid: uuid.unwrap_or_else(|| Uuid::new_v4().to_string()),
                kind: RenderableMessageKind::System(SystemMessage::local_command(input)),
            },
            RenderableMessage {
                uuid: Uuid::new_v4().to_string(),
                kind: RenderableMessageKind::System(SystemMessage::local_command(format!(
                    "<{LOCAL_COMMAND_STDOUT_TAG}>{output}</{LOCAL_COMMAND_STDOUT_TAG}>"
                ))),
            },
        ],
        should_query: false,
        allowed_tools: None,
        local_action: None,
        query_source: crate::constants::query_source::QuerySource::Prompt,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_handle_provider_off() {
        let temp = tempfile::tempdir().unwrap();
        let _guard = crate::utils::env_utils::EnvVarGuard::set("CLAUDE_CONFIG_DIR", temp.path());

        let res = handle_provider_command("off");
        assert!(res.contains("切回原生"));
        assert_eq!(get_active_channel_id(), None);
    }

    #[test]
    fn test_handle_provider_status() {
        let temp = tempfile::tempdir().unwrap();
        let _guard = crate::utils::env_utils::EnvVarGuard::set("CLAUDE_CONFIG_DIR", temp.path());

        let res = handle_provider_command("");
        assert!(res.contains("代理渠道状态看板"));
        assert!(res.contains("原生直连"));
    }

    #[test]
    fn test_handle_provider_configure_and_activate() {
        let temp = tempfile::tempdir().unwrap();
        let _guard = crate::utils::env_utils::EnvVarGuard::set("CLAUDE_CONFIG_DIR", temp.path());

        let res = handle_provider_command("cpa http://127.0.0.1:8317 12345");
        assert!(res.contains("已保存并激活"));
        assert_eq!(get_active_channel_id(), Some("cpa".to_string()));

        let cfg = load_channel_config("cpa").expect("config should exist");
        assert_eq!(
            cfg.env.get("ANTHROPIC_BASE_URL"),
            Some(&"http://127.0.0.1:8317".to_string())
        );
        assert_eq!(cfg.env.get("ANTHROPIC_API_KEY"), Some(&"12345".to_string()));
    }
}
