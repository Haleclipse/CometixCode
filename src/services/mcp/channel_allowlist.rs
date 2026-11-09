//! Maps to: CC `services/mcp/channelAllowlist.ts`.
//!
//! Nyan does not initialize or refresh GrowthBook here. `get_channel_allowlist`
//! resolves the `tengu_harbor_ledger` payload from the source-controlled switch
//! table, so a ledger written to the state file by anything else on the
//! machine cannot allowlist a channel.

use serde_json::Value;

/// Maps to: CC `ChannelAllowlistEntry`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelAllowlistEntry {
    pub marketplace: String,
    pub plugin: String,
}

fn channel_allowlist_entry_from_value(value: &Value) -> Option<ChannelAllowlistEntry> {
    // Maps to: CC `ChannelAllowlistSchema` zod object. String values are
    // preserved exactly; a non-string field makes the whole parsed list fail.
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

/// Maps to: CC `services/mcp/channelAllowlist.ts#getChannelAllowlist`.
pub fn get_channel_allowlist() -> Vec<ChannelAllowlistEntry> {
    let raw = crate::utils::feature_flags::feature_enabled(
        crate::utils::feature_flags::FeatureFlag::ChannelAllowlistLedger,
    )
    .then(|| Value::Array(Vec::new()));
    raw.as_ref()
        .and_then(Value::as_array)
        .and_then(|entries| channel_allowlist_entries_from_values(entries))
        .unwrap_or_default()
}

/// Maps to: CC `services/mcp/channelAllowlist.ts#isChannelsEnabled`.
pub fn is_channels_enabled() -> bool {
    crate::utils::feature_flags::feature_enabled(
        crate::utils::feature_flags::FeatureFlag::ChannelsEnabled,
    )
}

/// Maps to: CC `services/mcp/channelAllowlist.ts#isChannelAllowlisted`.
pub fn is_channel_allowlisted(plugin_source: Option<&str>) -> bool {
    is_channel_allowlisted_in(plugin_source, &get_channel_allowlist())
}

fn is_channel_allowlisted_in(
    plugin_source: Option<&str>,
    allowlist: &[ChannelAllowlistEntry],
) -> bool {
    let Some(plugin_source) = plugin_source else {
        return false;
    };
    let parsed = crate::utils::plugins::plugin_identifier::parse_plugin_identifier(plugin_source);
    let Some(marketplace) = parsed.marketplace else {
        return false;
    };
    allowlist
        .iter()
        .any(|entry| entry.plugin == parsed.name && entry.marketplace == marketplace)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_allowlist_entry_parsing_matches_official_zod_schema() {
        let parsed = channel_allowlist_entries_from_values(&[
            serde_json::json!({ "marketplace": "anthropic", "plugin": "mailbox" }),
            serde_json::json!({ "marketplace": "", "plugin": "preserved-empty-like-zod" }),
        ])
        .expect("all-string entries parse");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].plugin, "mailbox");
        assert_eq!(parsed[1].marketplace, "");

        assert!(
            channel_allowlist_entries_from_values(&[serde_json::json!({
                "marketplace": "anthropic",
                "plugin": 7
            })])
            .is_none()
        );
    }

    #[test]
    fn get_channel_allowlist_ships_the_official_empty_ledger() {
        assert!(get_channel_allowlist().is_empty());
        assert!(!is_channel_allowlisted(Some("mailbox@anthropic")));
    }

    #[test]
    fn is_channel_allowlisted_matches_plugin_source_gate() {
        let ledger = vec![ChannelAllowlistEntry {
            marketplace: "anthropic".to_string(),
            plugin: "mailbox".to_string(),
        }];

        assert!(is_channel_allowlisted_in(
            Some("mailbox@anthropic"),
            &ledger
        ));
        assert!(is_channel_allowlisted_in(
            Some("mailbox@anthropic@ignored"),
            &ledger
        ));
        assert!(!is_channel_allowlisted_in(Some("mailbox"), &ledger));
        assert!(!is_channel_allowlisted_in(Some("mailbox@evil"), &ledger));
        assert!(!is_channel_allowlisted_in(None, &ledger));
    }

    #[test]
    fn is_channels_enabled_uses_official_growthbook_gate_default() {
        assert_eq!(
            is_channels_enabled(),
            crate::utils::feature_flags::feature_enabled(
                crate::utils::feature_flags::FeatureFlag::ChannelsEnabled,
            )
        );
    }
}
