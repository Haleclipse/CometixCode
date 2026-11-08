//! Maps to: CC `components/LogoV2/ChannelsNotice.tsx`.
//!
//! The official module is conditionally required behind feature flags and reads
//! bootstrap/auth/settings/MCP/plugin state once at mount. nyan exposes the
//! same snapshot, formatting, unmatched-entry, and render branches as pure data;
//! callers own feature gating and runtime state collection.

use std::collections::HashSet;

use iocraft::prelude::*;

pub use crate::bootstrap::state::ChannelEntry;
use crate::services::mcp::channel_allowlist::{
    ChannelAllowlistEntry, get_channel_allowlist, is_channels_enabled,
};
use crate::services::mcp::channel_notification::EffectiveChannelAllowlist;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnmatchedChannelEntry {
    pub entry: ChannelEntry,
    pub why: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChannelsNoticeSnapshot {
    pub channels: Vec<ChannelEntry>,
    pub has_dev_channels: bool,
    pub disabled: bool,
    pub no_auth: bool,
    pub unmatched: Vec<UnmatchedChannelEntry>,
}

#[derive(Default, Props)]
pub struct ChannelsNoticeProps {
    pub snapshot: ChannelsNoticeSnapshot,
}

pub fn format_entry(entry: &ChannelEntry) -> String {
    match entry {
        ChannelEntry::Plugin {
            name, marketplace, ..
        } => format!("plugin:{name}@{marketplace}"),
        ChannelEntry::Server { name, .. } => format!("server:{name}"),
    }
}

pub fn channel_flag(channels: &[ChannelEntry], has_dev_channels: bool) -> &'static str {
    let has_non_dev = channels.iter().any(|entry| !entry.is_dev());
    if has_dev_channels && has_non_dev {
        "Channels"
    } else if has_dev_channels {
        "--dangerously-load-development-channels"
    } else {
        "--channels"
    }
}

pub fn find_unmatched_channels(
    entries: &[ChannelEntry],
    configured_servers: &HashSet<String>,
    installed_plugin_ids: &HashSet<String>,
    allowlist: &EffectiveChannelAllowlist,
) -> Vec<UnmatchedChannelEntry> {
    let mut out = Vec::new();

    for entry in entries {
        match entry {
            ChannelEntry::Server { name, dev } => {
                if !configured_servers.contains(name) {
                    out.push(UnmatchedChannelEntry {
                        entry: entry.clone(),
                        why: "no MCP server configured with that name".to_string(),
                    });
                }
                if !dev {
                    out.push(UnmatchedChannelEntry {
                        entry: entry.clone(),
                        why: "server: entries need --dangerously-load-development-channels"
                            .to_string(),
                    });
                }
            }
            ChannelEntry::Plugin {
                name,
                marketplace,
                dev,
            } => {
                if !installed_plugin_ids.contains(&format!("{name}@{marketplace}")) {
                    out.push(UnmatchedChannelEntry {
                        entry: entry.clone(),
                        why: "plugin not installed".to_string(),
                    });
                }
                if !dev
                    && !allowlist.entries.iter().any(|allowed| {
                        allowed.plugin == *name && allowed.marketplace == *marketplace
                    })
                {
                    out.push(UnmatchedChannelEntry {
                        entry: entry.clone(),
                        why: "not on the approved channels allowlist".to_string(),
                    });
                }
            }
        }
    }

    out
}

// Existing partial startup snapshot: only the supplied user/local raw objects.
// CC ChannelsNotice.findUnmatched uses getMcpConfigsByScope(...).servers for
// four scopes after validation; this adapter does not claim that full chain.
fn configured_mcp_server_names_readonly(
    global_config: &crate::utils::config::GlobalConfig,
    project_config: &crate::utils::config::ProjectConfig,
) -> HashSet<String> {
    let mut names = HashSet::new();
    if let Some(servers) = global_config
        .mcp_servers
        .as_ref()
        .and_then(serde_json::Value::as_object)
    {
        names.extend(servers.keys().cloned());
    }
    if let Some(servers) = project_config
        .mcp_servers
        .as_ref()
        .and_then(serde_json::Value::as_object)
    {
        names.extend(servers.keys().cloned());
    }
    names
}

/// Maps to: CC `components/LogoV2/ChannelsNotice.tsx` mount-time snapshot.
///
/// A `get_env` parameter was removed here: the body never read it, and the
/// subscription facts it appears to gate (formerly `get_subscription_type`,
/// `get_allowed_channels`) come from process state exactly as at the source.
/// Leaving it in place made callers — and their tests — look like they could
/// steer the result.
pub fn channels_notice_snapshot_from_readonly_sources(
    global_config: &crate::utils::config::GlobalConfig,
    project_config: &crate::utils::config::ProjectConfig,
) -> ChannelsNoticeSnapshot {
    let channels = crate::bootstrap::state::get_allowed_channels();
    if channels.is_empty() {
        return ChannelsNoticeSnapshot::default();
    }

    // @nyan: no claude.ai subscription or managed settings, so channels are
    // never org-managed and the allowlist is always the ledger.
    let allowlist = EffectiveChannelAllowlist {
        entries: get_channel_allowlist(),
    };
    let configured_servers = configured_mcp_server_names_readonly(global_config, project_config);
    // CC ChannelsNotice.tsx#findUnmatched reads loadInstalledPluginsV2().plugins.
    let installed = crate::utils::plugins::installed_plugins_manager::load_installed_plugins_v2();
    let installed_plugins: HashSet<String> = installed.lock().unwrap()["plugins"]
        .as_object()
        .expect("canonical V2 plugins record")
        .keys()
        .cloned()
        .collect();

    ChannelsNoticeSnapshot {
        unmatched: find_unmatched_channels(
            &channels,
            &configured_servers,
            &installed_plugins,
            &allowlist,
        ),
        channels,
        has_dev_channels: crate::bootstrap::state::get_has_dev_channels(),
        disabled: !is_channels_enabled(),
        no_auth: true,
    }
}

#[component]
pub fn ChannelsNotice(props: &ChannelsNoticeProps, hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<crate::utils::theme::Theme>();
    let snapshot = &props.snapshot;

    if snapshot.channels.is_empty() {
        return element! { View {} };
    }

    let list = snapshot
        .channels
        .iter()
        .map(format_entry)
        .collect::<Vec<_>>()
        .join(", ");
    let flag = channel_flag(&snapshot.channels, snapshot.has_dev_channels);

    let title = if snapshot.disabled {
        format!("{flag} ignored ({list})")
    } else if snapshot.no_auth {
        format!("{flag} ignored ({list})")
    } else {
        format!("Listening for channel messages from: {list}")
    };

    let dim_lines: Vec<String> = if snapshot.disabled {
        vec!["Channels are not currently available".to_string()]
    } else if snapshot.no_auth {
        vec!["Channels require claude.ai authentication, which nyan does not support".to_string()]
    } else {
        vec![format!(
            "Experimental · inbound messages will be pushed into this session, this carries prompt injection risks. Restart nyan without {flag} to disable."
        )]
    };

    element! {
        View(padding_left: 2u32, flex_direction: FlexDirection::Column) {
            Text(content: title, color: theme.error, wrap: TextWrap::Wrap)
            #(dim_lines.into_iter().map(|line| element! {
                Text(content: line, color: theme.inactive, wrap: TextWrap::Wrap)
            }))
            #(snapshot.unmatched.iter().map(|unmatched| element! {
                Text(content: format!("{} · {}", format_entry(&unmatched.entry), unmatched.why), color: theme.warning, wrap: TextWrap::Wrap)
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};
    use crate::utils::theme;

    fn render(snapshot: ChannelsNoticeSnapshot) -> String {
        element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                ChannelsNotice(snapshot: snapshot)
            }
        }
        .render(Some(160))
        .to_string()
    }

    #[test]
    fn channels_notice_formats_entries_and_flag_like_official() {
        let server = ChannelEntry::server("planner", true);
        let plugin = ChannelEntry::plugin("mailbox", "anthropic", false);
        assert_eq!(format_entry(&server), "server:planner");
        assert_eq!(format_entry(&plugin), "plugin:mailbox@anthropic");
        assert_eq!(
            channel_flag(std::slice::from_ref(&server), true),
            "--dangerously-load-development-channels"
        );
        assert_eq!(channel_flag(&[server.clone(), plugin], true), "Channels");
        assert_eq!(channel_flag(&[server], false), "--channels");
    }

    #[test]
    fn channels_notice_find_unmatched_matches_official_independent_checks() {
        let entries = vec![
            ChannelEntry::server("missing", false),
            ChannelEntry::plugin("todo", "market", false),
        ];
        let configured = HashSet::from(["other".to_string()]);
        let installed = HashSet::new();
        let allowlist = EffectiveChannelAllowlist {
            entries: Vec::new(),
        };
        let unmatched = find_unmatched_channels(&entries, &configured, &installed, &allowlist);
        assert_eq!(unmatched.len(), 4);
        assert!(
            unmatched
                .iter()
                .any(|u| u.why == "no MCP server configured with that name")
        );
        assert!(
            unmatched
                .iter()
                .any(|u| u.why == "server: entries need --dangerously-load-development-channels")
        );
        assert!(unmatched.iter().any(|u| u.why == "plugin not installed"));
        assert!(
            unmatched
                .iter()
                .any(|u| u.why == "not on the approved channels allowlist")
        );
    }

    #[test]
    fn channels_notice_cached_ledger_matches_official_empty_payload() {
        let mut config = crate::utils::config::GlobalConfig::default();
        config.cached_growth_book_features = Some(std::collections::HashMap::from([(
            "tengu_harbor_ledger".to_string(),
            serde_json::json!([{"plugin": "mailbox", "marketplace": "anthropic"}]),
        )]));
        crate::utils::config::set_test_global_config(Some(config));
        let ledger = get_channel_allowlist();
        assert_eq!(
            ledger.len(),
            0,
            "the cached GrowthBook ledger is inert; the switch table ships the official empty payload"
        );
        crate::utils::config::set_test_global_config(None);
    }

    #[test]
    fn channels_notice_snapshot_reads_bootstrap_and_config_without_mcp_or_plugin_runtime() {
        struct BootstrapChannelsGuard;
        impl Drop for BootstrapChannelsGuard {
            fn drop(&mut self) {
                crate::bootstrap::state::set_allowed_channels(Vec::new());
                crate::bootstrap::state::set_has_dev_channels(false);
            }
        }

        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        // An ambient OAuth token must not count as authentication: channels
        // need a claude.ai login, which nyan does not support.
        let _oauth = EnvVarGuard::set("CLAUDE_CODE_OAUTH_TOKEN", "sk-ant-oat01-channels-notice");

        let _channels = BootstrapChannelsGuard;
        crate::bootstrap::state::set_allowed_channels(vec![
            ChannelEntry::server("planner", true),
            ChannelEntry::plugin("mailbox", "anthropic", false),
        ]);
        crate::bootstrap::state::set_has_dev_channels(true);

        let global_config = crate::utils::config::GlobalConfig::default();
        let project_config = crate::utils::config::ProjectConfig {
            mcp_servers: Some(serde_json::json!({"planner":{}})),
            ..crate::utils::config::ProjectConfig::default()
        };
        let snapshot =
            channels_notice_snapshot_from_readonly_sources(&global_config, &project_config);

        assert_eq!(snapshot.channels.len(), 2);
        assert!(snapshot.has_dev_channels);
        assert!(
            snapshot.disabled,
            "source-controlled tengu_harbor default is off"
        );
        assert!(snapshot.no_auth);
        assert!(
            snapshot
                .unmatched
                .iter()
                .any(|entry| entry.why == "plugin not installed")
        );
        assert!(
            !snapshot
                .unmatched
                .iter()
                .any(|entry| entry.why == "no MCP server configured with that name")
        );
    }

    #[test]
    fn channels_notice_render_branches_match_official_copy() {
        let channel = ChannelEntry::plugin("mailbox", "anthropic", false);
        let active = render(ChannelsNoticeSnapshot {
            channels: vec![channel.clone()],
            unmatched: vec![UnmatchedChannelEntry {
                entry: channel.clone(),
                why: "plugin not installed".to_string(),
            }],
            ..ChannelsNoticeSnapshot::default()
        });
        assert!(
            active.contains("Listening for channel messages from: plugin:mailbox@anthropic"),
            "canvas=\n{active}"
        );
        assert!(
            active.contains("prompt injection risks"),
            "canvas=\n{active}"
        );
        assert!(
            active.contains("plugin:mailbox@anthropic · plugin not installed"),
            "canvas=\n{active}"
        );
    }
}
