//! MCP channel notification helpers.
//!
//! Maps to: CC `services/mcp/channelNotification.ts`.
//!
//! Runtime registration of custom MCP notification handlers is still owned by
//! `use_manage_mcp_connections`; this module contains the official pure
//! wrapping/gating rules and the queue handoff used once a handler is active.

use crate::bootstrap::state::ChannelEntry;
use crate::constants::xml::CHANNEL_TAG;
use crate::utils::message_queue_manager::{QueuePriority, QueuedCommand, enqueue};
use crate::utils::xml::escape_xml_attr;
use serde_json::Value;
use std::collections::BTreeMap;

pub use crate::services::mcp::channel_allowlist::ChannelAllowlistEntry;

/// Maps to: CC `CHANNEL_PERMISSION_METHOD`.
pub const CHANNEL_PERMISSION_METHOD: &str = "notifications/claude/channel/permission";
/// Maps to: CC `CHANNEL_PERMISSION_REQUEST_METHOD`.
pub const CHANNEL_PERMISSION_REQUEST_METHOD: &str =
    "notifications/claude/channel/permission_request";
/// Maps to: CC channel message notification method literal.
pub const CHANNEL_MESSAGE_NOTIFICATION_METHOD: &str = "notifications/claude/channel";
/// Maps to: CC `capabilities.experimental['claude/channel']`.
pub const CHANNEL_EXPERIMENTAL_CAPABILITY: &str = "claude/channel";
/// Maps to: CC `capabilities.experimental['claude/channel/permission']`.
pub const CHANNEL_PERMISSION_EXPERIMENTAL_CAPABILITY: &str = "claude/channel/permission";

/// Maps to: CC `getEffectiveChannelAllowlist(...)` source discriminator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelAllowlistSource {
    Org,
    Ledger,
}

/// Maps to: CC `getEffectiveChannelAllowlist(...)` return shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectiveChannelAllowlist {
    pub entries: Vec<ChannelAllowlistEntry>,
    pub source: ChannelAllowlistSource,
}

/// Maps to: CC `ChannelMessageNotificationSchema` parsed params.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelMessageNotification {
    pub content: String,
    pub meta: Option<BTreeMap<String, String>>,
}

/// Maps to: CC `ChannelPermissionNotificationSchema` parsed params.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelPermissionNotification {
    pub request_id: String,
    pub behavior: crate::services::mcp::channel_permissions::ChannelPermissionBehavior,
}

/// Maps to: CC `gateChannelServer(...)` runtime inputs read by
/// `useManageMCPConnections.ts` before registering/enqueuing channel messages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelRuntimeGateContext {
    pub channels_enabled: bool,
    pub has_claude_ai_oauth: bool,
    pub subscription: Option<String>,
    pub policy_channels_enabled: Option<bool>,
    pub session_channels: Vec<ChannelEntry>,
    pub allowlist: EffectiveChannelAllowlist,
}

/// Maps to: CC `ChannelGateResult['kind']`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelGateSkipKind {
    Capability,
    Disabled,
    Auth,
    Policy,
    Session,
    Marketplace,
    Allowlist,
}

/// Maps to: CC `ChannelGateResult`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChannelGateResult {
    Register,
    Skip {
        kind: ChannelGateSkipKind,
        reason: String,
    },
}

fn is_managed_subscription(subscription: Option<&str>) -> bool {
    matches!(subscription, Some("team" | "enterprise"))
}

fn parse_plugin_marketplace(plugin_source: &str) -> Option<String> {
    // Maps to: CC `utils/plugins/pluginIdentifier.ts#parsePluginIdentifier`.
    crate::utils::plugins::plugin_identifier::parse_plugin_identifier(plugin_source).marketplace
}

fn is_safe_meta_key(key: &str) -> bool {
    let mut chars = key.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first == '_' || first.is_ascii_alphabetic()) {
        return false;
    }
    chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

/// Maps to: CC `ChannelMessageNotificationSchema` zod parsing.
pub fn parse_channel_message_notification_params(
    params: Option<&Value>,
) -> Result<ChannelMessageNotification, String> {
    let object = params
        .and_then(Value::as_object)
        .ok_or_else(|| "channel notification params must be an object".to_string())?;
    let content = object
        .get("content")
        .and_then(Value::as_str)
        .ok_or_else(|| "channel notification content must be a string".to_string())?
        .to_string();
    let meta = match object.get("meta") {
        None => None,
        Some(Value::Object(values)) => {
            let mut parsed = BTreeMap::new();
            for (key, value) in values {
                let Some(value) = value.as_str() else {
                    return Err(format!("channel notification meta.{key} must be a string"));
                };
                parsed.insert(key.clone(), value.to_string());
            }
            Some(parsed)
        }
        Some(_) => return Err("channel notification meta must be an object".to_string()),
    };
    Ok(ChannelMessageNotification { content, meta })
}

/// Maps to: CC `ChannelPermissionNotificationSchema` zod parsing.
pub fn parse_channel_permission_notification_params(
    params: Option<&Value>,
) -> Result<ChannelPermissionNotification, String> {
    let object = params
        .and_then(Value::as_object)
        .ok_or_else(|| "channel permission notification params must be an object".to_string())?;
    let request_id = object
        .get("request_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "channel permission notification request_id must be a string".to_string())?
        .to_string();
    let behavior = object
        .get("behavior")
        .and_then(Value::as_str)
        .and_then(crate::services::mcp::channel_permissions::ChannelPermissionBehavior::from_str)
        .ok_or_else(|| {
            "channel permission notification behavior must be allow or deny".to_string()
        })?;
    Ok(ChannelPermissionNotification {
        request_id,
        behavior,
    })
}

fn channel_allowlist_entry_from_value(value: &Value) -> Option<ChannelAllowlistEntry> {
    // Maps to: CC managed setting `allowedChannelPlugins` entries, which use
    // the same `{ marketplace, plugin }` shape as `ChannelAllowlistEntry`.
    Some(ChannelAllowlistEntry {
        marketplace: value
            .get("marketplace")
            .and_then(Value::as_str)?
            .to_string(),
        plugin: value.get("plugin").and_then(Value::as_str)?.to_string(),
    })
}

fn channel_allowlist_entries_from_values(values: &[Value]) -> Option<Vec<ChannelAllowlistEntry>> {
    values
        .iter()
        .map(channel_allowlist_entry_from_value)
        .collect::<Option<Vec<_>>>()
}

/// Maps to: CC managed setting `allowedChannelPlugins` parsing.
pub fn allowed_channel_plugins_from_settings_values(
    values: Option<&[Value]>,
) -> Vec<ChannelAllowlistEntry> {
    values
        .and_then(channel_allowlist_entries_from_values)
        .unwrap_or_default()
}

/// Maps to: CC `gateChannelServer(...)` caller-side reads in
/// `useManageMCPConnections.ts`.
/// A `get_env` parameter was removed: the body never read it, and the auth
/// facts it appeared to gate (`get_subscription_type`, the OAuth check) resolve
/// from process state exactly as at the source. A `global_config` parameter was
/// removed likewise once the channel ledger moved to the switch table.
pub fn channel_gate_context_from_readonly_runtime(
    policy_settings: Option<&crate::utils::settings::SettingsJson>,
) -> ChannelRuntimeGateContext {
    let subscription = crate::utils::auth::get_subscription_type();
    let org_list = policy_settings
        .and_then(|settings| settings.allowed_channel_plugins.as_deref())
        .map(|values| allowed_channel_plugins_from_settings_values(Some(values)));
    ChannelRuntimeGateContext {
        channels_enabled: crate::services::mcp::channel_allowlist::is_channels_enabled(),
        has_claude_ai_oauth: crate::utils::auth::get_claude_ai_oauth_tokens().is_some(),
        policy_channels_enabled: policy_settings.and_then(|settings| settings.channels_enabled),
        session_channels: crate::bootstrap::state::get_allowed_channels(),
        allowlist: get_effective_channel_allowlist(
            subscription.as_deref(),
            org_list,
            crate::services::mcp::channel_allowlist::get_channel_allowlist(),
        ),
        subscription,
    }
}

/// Maps to: CC `wrapChannelMessage(...)`.
pub fn wrap_channel_message_from_pairs(
    server_name: &str,
    content: &str,
    meta: &[(&str, &str)],
) -> String {
    let attrs = meta
        .iter()
        .filter(|(key, _)| is_safe_meta_key(key))
        .map(|(key, value)| format!(" {key}=\"{}\"", escape_xml_attr(value)))
        .collect::<String>();
    format!(
        "<{CHANNEL_TAG} source=\"{}\"{attrs}>\n{content}\n</{CHANNEL_TAG}>",
        escape_xml_attr(server_name)
    )
}

/// Maps to: CC `wrapChannelMessage(...)` for parsed JSON object metadata.
pub fn wrap_channel_message(
    server_name: &str,
    content: &str,
    meta: Option<&BTreeMap<String, String>>,
) -> String {
    let pairs = meta
        .map(|meta| {
            meta.iter()
                .map(|(key, value)| (key.as_str(), value.as_str()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    wrap_channel_message_from_pairs(server_name, content, &pairs)
}

/// Maps to: CC `findChannelEntry(...)`.
pub fn find_channel_entry<'a>(
    server_name: &str,
    channels: &'a [ChannelEntry],
) -> Option<&'a ChannelEntry> {
    let mut parts = server_name.split(':');
    let first = parts.next();
    let plugin_name = parts.next();
    channels.iter().find(|entry| match entry {
        ChannelEntry::Server { name, .. } => server_name == name,
        ChannelEntry::Plugin { name, .. } => first == Some("plugin") && plugin_name == Some(name),
    })
}

/// Maps to: CC `getEffectiveChannelAllowlist(...)`.
pub fn get_effective_channel_allowlist(
    subscription: Option<&str>,
    org_list: Option<Vec<ChannelAllowlistEntry>>,
    ledger: Vec<ChannelAllowlistEntry>,
) -> EffectiveChannelAllowlist {
    if is_managed_subscription(subscription) {
        if let Some(entries) = org_list {
            return EffectiveChannelAllowlist {
                entries,
                source: ChannelAllowlistSource::Org,
            };
        }
    }
    EffectiveChannelAllowlist {
        entries: ledger,
        source: ChannelAllowlistSource::Ledger,
    }
}

/// Pure counterpart of CC `gateChannelServer(...)`.
pub fn gate_channel_server_from_context(
    server_name: &str,
    has_channel_capability: bool,
    channels_enabled: bool,
    has_claude_ai_oauth: bool,
    subscription: Option<&str>,
    policy_channels_enabled: Option<bool>,
    session_channels: &[ChannelEntry],
    plugin_source: Option<&str>,
    allowlist: &EffectiveChannelAllowlist,
) -> ChannelGateResult {
    if !has_channel_capability {
        return ChannelGateResult::Skip {
            kind: ChannelGateSkipKind::Capability,
            reason: "server did not declare claude/channel capability".to_string(),
        };
    }

    if !channels_enabled {
        return ChannelGateResult::Skip {
            kind: ChannelGateSkipKind::Disabled,
            reason: "channels feature is not currently available".to_string(),
        };
    }

    if !has_claude_ai_oauth {
        return ChannelGateResult::Skip {
            kind: ChannelGateSkipKind::Auth,
            reason: "channels requires claude.ai authentication (run /login)".to_string(),
        };
    }

    let managed = is_managed_subscription(subscription);
    if managed && policy_channels_enabled != Some(true) {
        return ChannelGateResult::Skip {
            kind: ChannelGateSkipKind::Policy,
            reason:
                "channels not enabled by org policy (set channelsEnabled: true in managed settings)"
                    .to_string(),
        };
    }

    let Some(entry) = find_channel_entry(server_name, session_channels) else {
        return ChannelGateResult::Skip {
            kind: ChannelGateSkipKind::Session,
            reason: format!("server {server_name} not in --channels list for this session"),
        };
    };

    match entry {
        ChannelEntry::Plugin {
            name,
            marketplace,
            dev,
        } => {
            let actual = plugin_source.and_then(parse_plugin_marketplace);
            if actual.as_deref() != Some(marketplace.as_str()) {
                return ChannelGateResult::Skip {
                    kind: ChannelGateSkipKind::Marketplace,
                    reason: format!(
                        "you asked for plugin:{name}@{marketplace} but the installed {name} plugin is from {}",
                        actual.as_deref().unwrap_or("an unknown source")
                    ),
                };
            }

            if !*dev
                && !allowlist
                    .entries
                    .iter()
                    .any(|entry| entry.plugin == *name && entry.marketplace == *marketplace)
            {
                return ChannelGateResult::Skip {
                    kind: ChannelGateSkipKind::Allowlist,
                    reason: match allowlist.source {
                        ChannelAllowlistSource::Org => format!(
                            "plugin {name}@{marketplace} is not on your org's approved channels list (set allowedChannelPlugins in managed settings)"
                        ),
                        ChannelAllowlistSource::Ledger => format!(
                            "plugin {name}@{marketplace} is not on the approved channels allowlist (use --dangerously-load-development-channels for local dev)"
                        ),
                    },
                };
            }
        }
        ChannelEntry::Server { name, dev } => {
            if !*dev {
                return ChannelGateResult::Skip {
                    kind: ChannelGateSkipKind::Allowlist,
                    reason: format!(
                        "server {name} is not on the approved channels allowlist (use --dangerously-load-development-channels for local dev)"
                    ),
                };
            }
        }
    }

    ChannelGateResult::Register
}

/// Maps to: CC `useManageMCPConnections.ts` channel notification handler's
/// `enqueue({ mode: 'prompt', priority: 'next', isMeta: true,
/// skipSlashCommands: true })` call.
pub fn enqueue_channel_message_notification(
    server_name: &str,
    content: &str,
    meta: Option<&BTreeMap<String, String>>,
) {
    let mut command =
        QueuedCommand::new(wrap_channel_message(server_name, content, meta), "prompt");
    command.priority = QueuePriority::Next;
    command.is_meta = true;
    command.skip_slash_commands = true;
    enqueue(command);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::message_queue_manager::{
        TEST_QUEUE_LOCK, clear_command_queue, get_command_queue,
    };
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    struct AllowedChannelsGuard(Vec<ChannelEntry>);

    impl AllowedChannelsGuard {
        fn set(entries: Vec<ChannelEntry>) -> Self {
            let previous = crate::bootstrap::state::get_allowed_channels();
            crate::bootstrap::state::set_allowed_channels(entries);
            Self(previous)
        }
    }

    impl Drop for AllowedChannelsGuard {
        fn drop(&mut self) {
            crate::bootstrap::state::set_allowed_channels(self.0.clone());
        }
    }

    #[test]
    fn wrap_channel_message_filters_meta_and_escapes_attrs_like_official() {
        let wrapped = wrap_channel_message_from_pairs(
            "plugin:slack:bot&\"",
            "hello <world>",
            &[
                ("thread_ts", "1&2\"3"),
                ("x-injected", "blocked"),
                ("_ok9", "yes"),
            ],
        );
        assert_eq!(
            wrapped,
            "<channel source=\"plugin:slack:bot&amp;&quot;\" thread_ts=\"1&amp;2&quot;3\" _ok9=\"yes\">\nhello <world>\n</channel>"
        );
    }

    #[test]
    fn channel_gate_matches_official_plugin_and_server_branches() {
        let ledger = EffectiveChannelAllowlist {
            entries: vec![ChannelAllowlistEntry {
                plugin: "slack".to_string(),
                marketplace: "anthropic".to_string(),
            }],
            source: ChannelAllowlistSource::Ledger,
        };
        let channels = vec![ChannelEntry::plugin("slack", "anthropic", false)];
        assert_eq!(
            gate_channel_server_from_context(
                "plugin:slack:bot",
                true,
                true,
                true,
                Some("max"),
                None,
                &channels,
                Some("slack@anthropic"),
                &ledger,
            ),
            ChannelGateResult::Register
        );
        assert_eq!(
            gate_channel_server_from_context(
                "plugin:slack:bot",
                true,
                true,
                true,
                Some("max"),
                None,
                &channels,
                Some("slack@anthropic@ignored"),
                &ledger,
            ),
            ChannelGateResult::Register
        );

        let marketplace_skip = gate_channel_server_from_context(
            "plugin:slack:bot",
            true,
            true,
            true,
            Some("max"),
            None,
            &channels,
            Some("slack@evil"),
            &ledger,
        );
        assert!(matches!(
            marketplace_skip,
            ChannelGateResult::Skip {
                kind: ChannelGateSkipKind::Marketplace,
                ..
            }
        ));

        let server_skip = gate_channel_server_from_context(
            "planner",
            true,
            true,
            true,
            Some("max"),
            None,
            &[ChannelEntry::server("planner", false)],
            None,
            &ledger,
        );
        assert!(matches!(
            server_skip,
            ChannelGateResult::Skip {
                kind: ChannelGateSkipKind::Allowlist,
                ..
            }
        ));
        assert_eq!(
            gate_channel_server_from_context(
                "planner",
                true,
                true,
                true,
                Some("max"),
                None,
                &[ChannelEntry::server("planner", true)],
                None,
                &ledger,
            ),
            ChannelGateResult::Register
        );
    }

    #[test]
    fn effective_channel_allowlist_uses_org_list_only_for_managed_subscriptions() {
        let ledger = vec![ChannelAllowlistEntry {
            plugin: "ledger".to_string(),
            marketplace: "market".to_string(),
        }];
        let org = vec![ChannelAllowlistEntry {
            plugin: "org".to_string(),
            marketplace: "market".to_string(),
        }];
        assert_eq!(
            get_effective_channel_allowlist(Some("team"), Some(org.clone()), ledger.clone())
                .entries[0]
                .plugin,
            "org"
        );
        assert_eq!(
            get_effective_channel_allowlist(Some("max"), Some(org), ledger.clone()).entries[0]
                .plugin,
            "ledger"
        );
    }

    #[test]
    fn parse_channel_message_notification_params_matches_official_schema() {
        let parsed = parse_channel_message_notification_params(Some(&serde_json::json!({
            "content": "hello",
            "meta": { "thread_ts": "123", "user": "ada" }
        })))
        .expect("valid channel notification");
        assert_eq!(parsed.content, "hello");
        assert_eq!(
            parsed.meta.as_ref().and_then(|meta| meta.get("thread_ts")),
            Some(&"123".to_string())
        );

        assert!(
            parse_channel_message_notification_params(Some(&serde_json::json!({
                "content": "hello",
                "meta": { "thread_ts": 123 }
            })))
            .is_err()
        );
    }

    #[test]
    fn parse_channel_permission_notification_params_matches_official_schema() {
        let parsed = parse_channel_permission_notification_params(Some(&serde_json::json!({
            "request_id": "TbXkQ",
            "behavior": "allow"
        })))
        .expect("valid channel permission notification");
        assert_eq!(parsed.request_id, "TbXkQ");
        assert_eq!(
            parsed.behavior,
            crate::services::mcp::channel_permissions::ChannelPermissionBehavior::Allow
        );

        assert!(
            parse_channel_permission_notification_params(Some(&serde_json::json!({
                "request_id": "TbXkQ",
                "behavior": "maybe"
            })))
            .is_err()
        );
    }

    #[test]
    fn allowed_channel_plugins_parser_reads_policy_values() {
        let policy = allowed_channel_plugins_from_settings_values(Some(&[
            serde_json::json!({ "marketplace": "org", "plugin": "pager" }),
        ]));
        assert_eq!(policy[0].marketplace, "org");
    }

    #[test]
    fn channel_gate_context_reads_cached_ledger_policy_oauth_and_session_channels() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _channels_guard =
            AllowedChannelsGuard::set(vec![ChannelEntry::server("planner", true)]);
        // The OAuth check resolves from process state (`utils/auth.rs:701`), so
        // the logged-in identity is this test's to establish. Reading whatever
        // credential file the ambient config home happens to hold makes the
        // `has_claude_ai_oauth` assertion depend on the runner's setup.
        let _oauth_guard = EnvVarGuard::set("CLAUDE_CODE_OAUTH_TOKEN", "sk-ant-oat01-channel-gate");
        let _bare_guard = EnvVarGuard::set("CLAUDE_CODE_SIMPLE", "0");

        let mut config = crate::utils::config::GlobalConfig::default();
        config.cached_growth_book_features = Some(std::collections::HashMap::from([(
            "tengu_harbor_ledger".to_string(),
            serde_json::json!([{ "marketplace": "anthropic", "plugin": "mailbox" }]),
        )]));
        crate::utils::config::set_test_global_config(Some(config));
        let mut policy = crate::utils::settings::SettingsJson::default();
        policy.channels_enabled = Some(true);
        policy.allowed_channel_plugins = Some(vec![
            serde_json::json!({ "marketplace": "org", "plugin": "pager" }),
        ]);

        let context = channel_gate_context_from_readonly_runtime(Some(&policy));
        crate::utils::config::set_test_global_config(None);
        assert!(!context.channels_enabled);
        assert!(context.has_claude_ai_oauth);
        assert_eq!(context.policy_channels_enabled, Some(true));
        assert_eq!(
            context.session_channels,
            vec![ChannelEntry::server("planner", true)]
        );
        assert_eq!(context.allowlist.source, ChannelAllowlistSource::Ledger);
        assert!(
            context.allowlist.entries.is_empty(),
            "the cached GrowthBook ledger is inert; the switch table ships the official empty payload"
        );
    }

    #[test]
    fn enqueue_channel_message_notification_uses_prompt_meta_queue_shape() {
        let _lock = TEST_QUEUE_LOCK.lock().unwrap();
        clear_command_queue();
        enqueue_channel_message_notification("slack", "/not-a-command", None);
        let queue = get_command_queue();
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].mode, "prompt");
        assert!(queue[0].is_meta);
        assert!(queue[0].skip_slash_commands);
        assert!(queue[0].value.contains("<channel source=\"slack\">"));
        clear_command_queue();
    }
}
