//! Plugin recommendations emitted through the Claude Code shell-hint protocol.
//!
//! Maps to: CC `utils/plugins/hintRecommendation.ts:1-174`.

use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};

const MAX_SHOWN_PLUGINS: usize = 100;

static TRIED_THIS_SESSION: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginHintRecommendation {
    pub plugin_id: String,
    pub plugin_name: String,
    pub marketplace_name: String,
    pub plugin_description: Option<String>,
    pub source_command: String,
}

/// Maps to CC `getFeatureValue_CACHED_MAY_BE_STALE('tengu_lapis_finch', false)`,
/// resolved from the source-controlled switch table instead of GrowthBook.
fn feature_enabled() -> bool {
    crate::utils::feature_flags::feature_enabled(
        crate::utils::feature_flags::FeatureFlag::PluginHintRecommendation,
    )
}

/// Synchronous pre-store gate called by Bash/PowerShell after stripping a
/// `type="plugin"` hint. Maps to CC `maybeRecordPluginHint`.
pub fn maybe_record_plugin_hint(hint: crate::utils::claude_code_hints::ClaudeCodeHint) {
    if hint.hint_type != "plugin" {
        return;
    }
    if !feature_enabled() || crate::utils::claude_code_hints::has_shown_hint_this_session() {
        return;
    }
    let config = crate::utils::config::load_global_config();
    let state = config.claude_code_hints.as_ref();
    if state.and_then(|state| state.disabled) == Some(true) {
        return;
    }
    let shown = state
        .map(|state| state.plugin.as_slice())
        .unwrap_or_default();
    if shown.len() >= MAX_SHOWN_PLUGINS || shown.iter().any(|plugin| plugin == &hint.value) {
        return;
    }

    let parsed = crate::utils::plugins::plugin_identifier::parse_plugin_identifier(&hint.value);
    if parsed.name.is_empty()
        || !crate::utils::plugins::plugin_identifier::is_official_marketplace_name(
            parsed.marketplace.as_deref(),
        )
        || crate::utils::plugins::installed_plugins_manager::is_plugin_installed(&hint.value)
        || crate::utils::plugins::plugin_policy::is_plugin_blocked_by_policy(&hint.value)
    {
        return;
    }

    let Ok(mut tried) = TRIED_THIS_SESSION.lock() else {
        return;
    };
    if !tried.insert(hint.value.clone()) {
        return;
    }
    drop(tried);
    crate::utils::claude_code_hints::set_pending_hint(hint);
}

/// Maps to: CC `utils/plugins/hintRecommendation.ts:103-135#resolvePluginHint`.
/// Partial: CC awaits getPluginById (cache, then source fetch). This path awaits
/// the canonical cache-only lookup; a cache miss does not fetch a marketplace.
/// The pre-store gate owns type/official/installed/policy checks, not resolution.
pub async fn resolve_plugin_hint(
    hint: &crate::utils::claude_code_hints::ClaudeCodeHint,
) -> Option<PluginHintRecommendation> {
    let plugin_id = &hint.value;
    let parsed = crate::utils::plugins::plugin_identifier::parse_plugin_identifier(plugin_id);
    let plugin_data =
        crate::utils::plugins::marketplace_manager::get_plugin_by_id_cache_only(plugin_id).await;
    crate::services::analytics::log_event(
        "tengu_plugin_hint_detected",
        serde_json::json!({
            "_PROTO_plugin_name": parsed.name,
            "_PROTO_marketplace_name": parsed.marketplace.as_deref().unwrap_or(""),
            "result": if plugin_data.is_some() { "passed" } else { "not_in_cache" },
        }),
    );
    let Some(plugin_data) = plugin_data else {
        crate::utils::debug::log_for_debugging(&format!(
            "[hintRecommendation] {plugin_id} not found in marketplace cache"
        ));
        return None;
    };
    Some(PluginHintRecommendation {
        plugin_id: plugin_id.clone(),
        plugin_name: plugin_data.entry.get("name")?.as_str()?.to_string(),
        marketplace_name: parsed.marketplace.unwrap_or_default(),
        plugin_description: plugin_data
            .entry
            .get("description")
            .and_then(serde_json::Value::as_str)
            .map(ToString::to_string),
        source_command: hint.source_command.clone(),
    })
}

fn mark_hint_plugin_shown_in_config(
    config: &mut crate::utils::config::GlobalConfig,
    plugin_id: &str,
) {
    let state = config
        .claude_code_hints
        .get_or_insert_with(Default::default);
    if !state.plugin.iter().any(|shown| shown == plugin_id) {
        state.plugin.push(plugin_id.to_string());
    }
}

/// Maps to CC `markHintPluginShown`; recording occurs regardless of the
/// eventual yes/no response once the dialog is surfaced.
pub fn mark_hint_plugin_shown(plugin_id: &str) -> anyhow::Result<()> {
    if !crate::utils::session_storage::is_session_write_enabled() {
        anyhow::bail!(crate::tools::shared::write_gate::PLUGIN_HINT_PERSISTENCE_DISABLED_ERROR);
    }
    crate::utils::config::save_global_config(|config| {
        mark_hint_plugin_shown_in_config(config, plugin_id);
    })
}

/// Maps to CC `disableHintRecommendations`.
pub fn disable_hint_recommendations() -> anyhow::Result<()> {
    if !crate::utils::session_storage::is_session_write_enabled() {
        anyhow::bail!(crate::tools::shared::write_gate::PLUGIN_HINT_PERSISTENCE_DISABLED_ERROR);
    }
    crate::utils::config::save_global_config(|config| {
        config
            .claude_code_hints
            .get_or_insert_with(Default::default)
            .disabled = Some(true);
    })
}

#[cfg(test)]
pub fn reset_hint_recommendation_for_test() {
    if let Ok(mut tried) = TRIED_THIS_SESSION.lock() {
        tried.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::EnvVarGuard;

    #[test]
    fn feature_gate_reads_switch_table_and_ignores_growthbook_delivery() {
        let mut config = crate::utils::config::GlobalConfig::default();
        config.cached_growth_book_features = Some(std::collections::HashMap::from([(
            "tengu_lapis_finch".to_string(),
            serde_json::json!(true),
        )]));
        config.growth_book_overrides = Some(std::collections::HashMap::from([(
            "tengu_lapis_finch".to_string(),
            serde_json::json!(true),
        )]));
        crate::utils::config::set_test_global_config(Some(config));

        assert_eq!(
            feature_enabled(),
            crate::utils::feature_flags::feature_enabled(
                crate::utils::feature_flags::FeatureFlag::PluginHintRecommendation,
            )
        );
        assert!(!feature_enabled());

        crate::utils::config::set_test_global_config(None);
    }

    #[test]
    fn shown_plugin_config_update_is_idempotent() {
        let mut config = crate::utils::config::GlobalConfig::default();
        mark_hint_plugin_shown_in_config(&mut config, "mail@claude-plugins-official");
        mark_hint_plugin_shown_in_config(&mut config, "mail@claude-plugins-official");
        assert_eq!(
            config.claude_code_hints.unwrap().plugin,
            ["mail@claude-plugins-official"]
        );
    }

    #[tokio::test]
    async fn async_hint_resolution_matches_official_cached_entry_projection() {
        use crate::utils::claude_code_hints::ClaudeCodeHint;
        use serde_json::json;
        let root = std::env::temp_dir().join(format!("hint-resolution-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let _cache = EnvVarGuard::set("CLAUDE_CODE_PLUGIN_CACHE_DIR", &root);
        let catalog = root.join("catalog.json");
        std::fs::write(
            &catalog,
            json!({
                "name":"custom","owner":{"name":"Owner"},
                "plugins":[{"name":"p","source":"./p","description":"  Preserved description  "}]
            })
            .to_string(),
        )
        .unwrap();
        let config = json!({"custom":{
            "source":{"source":"directory","path":root},
            "installLocation":catalog,"lastUpdated":"now"
        }})
        .to_string();
        std::fs::write(root.join("known_marketplaces.json"), &config).unwrap();
        // Actual Bun resolvePluginHint oracle, hintRecommendation.ts:103-135:
        // resolver does not repeat the official/id gates owned by pre-store;
        // plugin name/description come directly from the validated entry.
        for plugin_id in ["p@custom", "p@custom@ignored"] {
            let hint = ClaudeCodeHint {
                version: 1,
                hint_type: "plugin".into(),
                value: plugin_id.into(),
                source_command: "tool".into(),
            };
            assert_eq!(
                resolve_plugin_hint(&hint).await,
                Some(PluginHintRecommendation {
                    plugin_id: plugin_id.into(),
                    plugin_name: "p".into(),
                    marketplace_name: "custom".into(),
                    plugin_description: Some("  Preserved description  ".into()),
                    source_command: "tool".into(),
                })
            );
        }
        let missing = ClaudeCodeHint {
            version: 1,
            hint_type: "plugin".into(),
            value: "missing@unregistered".into(),
            source_command: "tool".into(),
        };
        assert!(resolve_plugin_hint(&missing).await.is_none());
        let events = crate::services::analytics::queued_events_for_test();
        assert!(events.iter().any(|(name, metadata)| name == "tengu_plugin_hint_detected"
            && metadata == &json!({"_PROTO_plugin_name":"p","_PROTO_marketplace_name":"custom","result":"passed"})));
        assert!(events.iter().any(|(name, metadata)| name == "tengu_plugin_hint_detected"
            && metadata == &json!({"_PROTO_plugin_name":"missing","_PROTO_marketplace_name":"unregistered","result":"not_in_cache"})));
        assert_eq!(
            std::fs::read_to_string(root.join("known_marketplaces.json")).unwrap(),
            config
        );
        // Direct resolution is exercised even though production pre-store is
        // currently feature-disabled; this test does not claim a live prompt.
        assert!(!feature_enabled());
        std::fs::remove_dir_all(root).unwrap();
    }
}
