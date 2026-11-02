//! MCP channel permission-relay helpers.
//!
//! Maps to: CC `services/mcp/channelPermissions.ts`.
//!
//! This module owns the pure permission-relay pieces: runtime gate lookup,
//! short request IDs, preview truncation, relay-client filtering, and the
//! per-session callback registry shape. Sending outbound MCP notifications and
//! racing permission responses still belongs to the official interactive
//! permission handler boundary.

use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Maps to: CC `ChannelPermissionResponse['behavior']`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelPermissionBehavior {
    Allow,
    Deny,
}

impl ChannelPermissionBehavior {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Deny => "deny",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "allow" => Some(Self::Allow),
            "deny" => Some(Self::Deny),
            _ => None,
        }
    }
}

/// Maps to: CC `ChannelPermissionResponse`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelPermissionResponse {
    pub behavior: ChannelPermissionBehavior,
    pub from_server: String,
}

/// Maps to: CC `ChannelPermissionRequestParams`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ChannelPermissionRequestParams {
    pub request_id: String,
    pub tool_name: String,
    pub description: String,
    pub input_preview: String,
}

/// Maps to: CC `interactiveHandler.ts` construction of outbound
/// `ChannelPermissionRequestParams`.
pub fn channel_permission_request_params(
    tool_use_id: &str,
    tool_name: &str,
    description: &str,
    display_input: &impl Serialize,
) -> ChannelPermissionRequestParams {
    ChannelPermissionRequestParams {
        request_id: short_request_id(tool_use_id),
        tool_name: tool_name.to_string(),
        description: description.to_string(),
        input_preview: truncate_for_preview(display_input),
    }
}

/// Maps to: CC outbound `client.client.notification({ method, params })`
/// payload in `interactiveHandler.ts`.
pub fn channel_permission_request_notification_value(
    params: &ChannelPermissionRequestParams,
) -> serde_json::Value {
    serde_json::json!({
        "method": crate::services::mcp::channel_notification::CHANNEL_PERMISSION_REQUEST_METHOD,
        "params": params,
    })
}

type ChannelPermissionHandler = Arc<dyn Fn(ChannelPermissionResponse) + Send + Sync + 'static>;

/// Maps to: CC `ChannelPermissionCallbacks` returned by
/// `createChannelPermissionCallbacks()`.
#[derive(Clone, Default)]
pub struct ChannelPermissionCallbacks {
    pending: Arc<Mutex<HashMap<String, ChannelPermissionHandler>>>,
}

impl std::fmt::Debug for ChannelPermissionCallbacks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelPermissionCallbacks")
            .field("pending_len", &self.pending_len())
            .finish()
    }
}

impl PartialEq for ChannelPermissionCallbacks {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.pending, &other.pending)
    }
}

impl Eq for ChannelPermissionCallbacks {}

impl ChannelPermissionCallbacks {
    /// Maps to: CC `ChannelPermissionCallbacks.onResponse(...)`.
    pub fn on_response(
        &self,
        request_id: &str,
        handler: impl Fn(ChannelPermissionResponse) + Send + Sync + 'static,
    ) -> ChannelPermissionUnsubscribe {
        let key = request_id.to_lowercase();
        with_pending(&self.pending, |pending| {
            pending.insert(key.clone(), Arc::new(handler));
        });
        ChannelPermissionUnsubscribe {
            callbacks: self.clone(),
            key,
        }
    }

    /// Maps to: CC `ChannelPermissionCallbacks.resolve(...)`.
    pub fn resolve(
        &self,
        request_id: &str,
        behavior: ChannelPermissionBehavior,
        from_server: &str,
    ) -> bool {
        let key = request_id.to_lowercase();
        let handler = with_pending(&self.pending, |pending| pending.remove(&key));
        let Some(handler) = handler else {
            return false;
        };
        handler(ChannelPermissionResponse {
            behavior,
            from_server: from_server.to_string(),
        });
        true
    }

    pub fn pending_len(&self) -> usize {
        with_pending(&self.pending, |pending| pending.len())
    }
}

/// Maps to: CC unsubscribe function returned by
/// `ChannelPermissionCallbacks.onResponse(...)`.
#[derive(Clone, Debug)]
pub struct ChannelPermissionUnsubscribe {
    callbacks: ChannelPermissionCallbacks,
    key: String,
}

impl ChannelPermissionUnsubscribe {
    pub fn unsubscribe(&self) {
        with_pending(&self.callbacks.pending, |pending| {
            pending.remove(&self.key);
        });
    }
}

fn with_pending<T>(
    pending: &Arc<Mutex<HashMap<String, ChannelPermissionHandler>>>,
    f: impl FnOnce(&mut HashMap<String, ChannelPermissionHandler>) -> T,
) -> T {
    match pending.lock() {
        Ok(mut guard) => f(&mut guard),
        Err(poisoned) => {
            let mut guard = poisoned.into_inner();
            f(&mut guard)
        }
    }
}

/// Maps to: CC `isChannelPermissionRelayEnabled()`.
pub fn is_channel_permission_relay_enabled() -> bool {
    crate::utils::feature_flags::feature_enabled(
        crate::utils::feature_flags::FeatureFlag::ChannelPermissionRelay,
    )
}

const ID_ALPHABET: &[u8; 25] = b"abcdefghijkmnopqrstuvwxyz";
const ID_AVOID_SUBSTRINGS: &[&str] = &[
    "fuck", "shit", "cunt", "cock", "dick", "twat", "piss", "crap", "bitch", "whore", "ass", "tit",
    "cum", "fag", "dyke", "nig", "kike", "rape", "nazi", "damn", "poo", "pee", "wank", "anus",
];

fn hash_to_id(input: &str) -> String {
    // Maps to: CC `hashToId(...)` FNV-1a uint32 + base-25 encoding.
    let mut hash: u32 = 0x811c9dc5;
    for byte in input.bytes() {
        hash ^= byte as u32;
        hash = hash.wrapping_mul(0x0100_0193);
    }
    let mut out = String::with_capacity(5);
    for _ in 0..5 {
        out.push(ID_ALPHABET[(hash % 25) as usize] as char);
        hash /= 25;
    }
    out
}

/// Maps to: CC `shortRequestId(toolUseID)`.
pub fn short_request_id(tool_use_id: &str) -> String {
    let mut candidate = hash_to_id(tool_use_id);
    for salt in 0..10 {
        if !ID_AVOID_SUBSTRINGS
            .iter()
            .any(|blocked| candidate.contains(blocked))
        {
            return candidate;
        }
        candidate = hash_to_id(&format!("{tool_use_id}:{salt}"));
    }
    candidate
}

/// Maps to: CC exported `PERMISSION_REPLY_RE`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedPermissionReply {
    pub behavior: ChannelPermissionBehavior,
    pub request_id: String,
}

/// Rust parser counterpart for CC `PERMISSION_REPLY_RE`.
pub fn parse_permission_reply(value: &str) -> Option<ParsedPermissionReply> {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?i)^\s*(y|yes|n|no)\s+([a-km-z]{5})\s*$")
            .expect("permission reply regex")
    });
    let captures = RE.captures(value)?;
    let verb = captures.get(1)?.as_str().to_ascii_lowercase();
    let behavior = match verb.as_str() {
        "y" | "yes" => ChannelPermissionBehavior::Allow,
        "n" | "no" => ChannelPermissionBehavior::Deny,
        _ => return None,
    };
    Some(ParsedPermissionReply {
        behavior,
        request_id: captures.get(2)?.as_str().to_ascii_lowercase(),
    })
}

/// Maps to: CC `useManageMCPConnections.ts` dedicated
/// `notifications/claude/channel/permission` handler.
pub fn handle_channel_permission_notification(
    callbacks: Option<&ChannelPermissionCallbacks>,
    request_id: &str,
    behavior: ChannelPermissionBehavior,
    from_server: &str,
) -> bool {
    callbacks
        .map(|callbacks| callbacks.resolve(request_id, behavior, from_server))
        .unwrap_or(false)
}

/// Maps to: CC `truncateForPreview(input)`.
pub fn truncate_for_preview(input: &impl Serialize) -> String {
    match serde_json::to_string(input) {
        Ok(value) => {
            if value.chars().count() > 200 {
                let prefix = value.chars().take(200).collect::<String>();
                format!("{prefix}…")
            } else {
                value
            }
        }
        Err(_) => "(unserializable)".to_string(),
    }
}

/// Maps to: CC `filterPermissionRelayClients(clients, isInAllowlist)`
/// (`channelPermissions.ts:177-194`): the connected clients on the
/// allowlist that declare both `claude/channel` and
/// `claude/channel/permission` in their experimental capabilities.
pub fn filter_permission_relay_clients(
    clients: &[crate::services::mcp::types::McpServerConnection],
    is_in_allowlist: impl Fn(&str) -> bool,
) -> Vec<&crate::services::mcp::types::ConnectedMcpServer> {
    clients
        .iter()
        .filter_map(crate::services::mcp::types::McpServerConnection::as_connected)
        .filter(|client| {
            let experimental = client.capabilities.experimental.as_ref();
            is_in_allowlist(&client.name)
                && experimental.is_some_and(|experimental| {
                    experimental.contains_key(
                        crate::services::mcp::channel_notification::CHANNEL_EXPERIMENTAL_CAPABILITY,
                    )
                })
                && experimental.is_some_and(|experimental| {
                    experimental.contains_key(
                        crate::services::mcp::channel_notification::CHANNEL_PERMISSION_EXPERIMENTAL_CAPABILITY,
                    )
                })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn short_request_id_matches_official_hash_and_blocklist_retry() {
        assert_eq!(short_request_id("toolu_123"), "vydqs");
        assert_eq!(short_request_id("toolu_abcdef"), "pqbue");
        // `toolu_447` hashes to a blocked `zypee`; official re-hashes with
        // salt `:0` and returns this safe ID.
        assert_eq!(short_request_id("toolu_447"), "rzbdn");
    }

    #[test]
    fn permission_reply_regex_matches_official_yes_no_shape() {
        assert_eq!(
            parse_permission_reply(" yes TbXkq "),
            Some(ParsedPermissionReply {
                behavior: ChannelPermissionBehavior::Allow,
                request_id: "tbxkq".to_string(),
            })
        );
        assert_eq!(
            parse_permission_reply("n abcde"),
            Some(ParsedPermissionReply {
                behavior: ChannelPermissionBehavior::Deny,
                request_id: "abcde".to_string(),
            })
        );
        assert!(parse_permission_reply("yes abc1e").is_none());
        assert!(parse_permission_reply("yes abcle").is_none());
        assert!(parse_permission_reply("yes").is_none());
    }

    #[test]
    fn channel_permission_notification_handler_resolves_pending_callbacks() {
        let callbacks = ChannelPermissionCallbacks::default();
        let seen = Arc::new(Mutex::new(Vec::<ChannelPermissionResponse>::new()));
        let seen_for_handler = Arc::clone(&seen);
        let _unsubscribe = callbacks.on_response("tbxkq", move |response| {
            seen_for_handler.lock().unwrap().push(response);
        });

        assert!(handle_channel_permission_notification(
            Some(&callbacks),
            "TbXkQ",
            ChannelPermissionBehavior::Deny,
            "plugin:telegram:tg",
        ));
        assert!(!handle_channel_permission_notification(
            Some(&callbacks),
            "TbXkQ",
            ChannelPermissionBehavior::Deny,
            "plugin:telegram:tg",
        ));
        assert!(!handle_channel_permission_notification(
            None,
            "abcde",
            ChannelPermissionBehavior::Allow,
            "plugin:telegram:tg",
        ));
        assert_eq!(seen.lock().unwrap().len(), 1);
        assert_eq!(
            seen.lock().unwrap()[0].behavior,
            ChannelPermissionBehavior::Deny
        );
        assert_eq!(seen.lock().unwrap()[0].from_server, "plugin:telegram:tg");
    }

    #[test]
    fn truncate_for_preview_matches_phone_sized_json_preview() {
        assert_eq!(
            truncate_for_preview(&serde_json::json!({"a":1})),
            r#"{"a":1}"#
        );
        let value = serde_json::json!({"text": "x".repeat(240)});
        let preview = truncate_for_preview(&value);
        assert!(preview.ends_with('…'));
        assert_eq!(preview.chars().count(), 201);
    }

    #[test]
    fn permission_request_params_and_notification_payload_match_official_shape() {
        let params = channel_permission_request_params(
            "toolu_123",
            "Bash",
            "Run command?",
            &serde_json::json!({ "command": "echo hi" }),
        );
        assert_eq!(params.request_id, "vydqs");
        assert_eq!(params.tool_name, "Bash");
        assert_eq!(params.description, "Run command?");
        assert_eq!(params.input_preview, r#"{"command":"echo hi"}"#);

        let payload = channel_permission_request_notification_value(&params);
        assert_eq!(
            payload["method"],
            crate::services::mcp::channel_notification::CHANNEL_PERMISSION_REQUEST_METHOD
        );
        assert_eq!(payload["params"]["request_id"], "vydqs");
        assert_eq!(payload["params"]["tool_name"], "Bash");
    }

    #[test]
    fn filter_permission_relay_clients_requires_connected_allowlist_and_both_capabilities() {
        use crate::services::mcp::channel_notification::{
            CHANNEL_EXPERIMENTAL_CAPABILITY as CHANNEL,
            CHANNEL_PERMISSION_EXPERIMENTAL_CAPABILITY as PERMISSION,
        };
        use crate::services::mcp::types::{McpServerConnection, ScopedMcpServerConfig};

        let relay_client = |name: &str, capabilities: &[&str]| {
            let mut client = McpServerConnection::connected_for_test(
                name,
                ScopedMcpServerConfig::stdio_for_test(name),
            );
            if let McpServerConnection::Connected(connected) = &mut client {
                connected.capabilities.experimental = Some(
                    capabilities
                        .iter()
                        .map(|capability| (capability.to_string(), Default::default()))
                        .collect(),
                );
            }
            client
        };
        // Each one but the first misses exactly one condition.
        let clients = vec![
            relay_client("telegram", &[CHANNEL, PERMISSION]),
            relay_client("channel-only", &[CHANNEL]),
            relay_client("permission-only", &[PERMISSION]),
            relay_client("blocked", &[CHANNEL, PERMISSION]),
            McpServerConnection::pending("discord", ScopedMcpServerConfig::stdio_for_test("d")),
        ];
        let filtered = filter_permission_relay_clients(&clients, |name| name != "blocked");
        assert_eq!(
            filtered
                .iter()
                .map(|client| client.name.as_str())
                .collect::<Vec<_>>(),
            ["telegram"]
        );
    }

    #[test]
    fn channel_permission_callbacks_lowercase_delete_before_call_and_unsubscribe() {
        let callbacks = ChannelPermissionCallbacks::default();
        let seen = Arc::new(Mutex::new(Vec::<ChannelPermissionResponse>::new()));
        let seen_for_handler = Arc::clone(&seen);
        let unsubscribe = callbacks.on_response("TbXkQ", move |response| {
            seen_for_handler.lock().unwrap().push(response);
        });
        assert_eq!(callbacks.pending_len(), 1);
        assert!(callbacks.resolve(
            "tbxkq",
            ChannelPermissionBehavior::Allow,
            "plugin:telegram:tg"
        ));
        assert_eq!(callbacks.pending_len(), 0);
        assert!(!callbacks.resolve(
            "tbxkq",
            ChannelPermissionBehavior::Deny,
            "plugin:telegram:tg"
        ));
        assert_eq!(seen.lock().unwrap().len(), 1);
        assert_eq!(
            seen.lock().unwrap()[0].behavior,
            ChannelPermissionBehavior::Allow
        );

        let unsubscribe2 = callbacks.on_response("abcde", |_| {});
        assert_eq!(callbacks.pending_len(), 1);
        unsubscribe2.unsubscribe();
        assert_eq!(callbacks.pending_len(), 0);
        // Original unsubscribe is idempotent after resolve.
        unsubscribe.unsubscribe();
    }

    #[test]
    fn channel_permission_relay_gate_defaults_off_like_growthbook() {
        assert!(!is_channel_permission_relay_enabled());
    }
}
