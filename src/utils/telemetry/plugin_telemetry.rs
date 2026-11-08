//! Maps to: CC `utils/telemetry/pluginTelemetry.ts`.
use crate::types::plugin::{LoadedPlugin, PluginError};
use crate::utils::plugins::plugin_identifier::{
    is_official_marketplace_name, parse_plugin_identifier,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// Maps to: CC `utils/telemetry/pluginTelemetry.ts#hashPluginId`.
pub fn hash_plugin_id(name: &str, marketplace: Option<&str>) -> String {
    let key = match marketplace.filter(|m| !m.is_empty()) {
        Some(m) => format!("{name}@{}", m.to_lowercase()),
        None => name.to_owned(),
    };
    Sha256::digest(format!("{key}claude-plugin-telemetry-v1"))
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
/// Maps to: CC `utils/telemetry/pluginTelemetry.ts#TelemetryPluginScope`.
pub type TelemetryPluginScope = &'static str;
/// Maps to: CC `utils/telemetry/pluginTelemetry.ts#getTelemetryPluginScope`.
/// @nyan: no managed policy layer, so CC's `org` scope never applies.
pub fn get_telemetry_plugin_scope(marketplace: Option<&str>) -> TelemetryPluginScope {
    if marketplace == Some("builtin") {
        return "default-bundle";
    }
    if is_official_marketplace_name(marketplace) {
        return "official";
    }
    "user-local"
}
/// Maps to: CC `utils/telemetry/pluginTelemetry.ts#EnabledVia`.
pub type EnabledVia = &'static str;
/// Maps to: CC `utils/telemetry/pluginTelemetry.ts#getEnabledVia`.
/// @nyan: no managed policy layer, so CC's `org-policy` never applies.
pub fn get_enabled_via(plugin: &LoadedPlugin, seed_dirs: &[String]) -> EnabledVia {
    if plugin.is_builtin {
        return "default-enable";
    }
    let path = plugin.path.to_string_lossy();
    if seed_dirs.iter().any(|dir| {
        let prefix = if dir.ends_with(std::path::MAIN_SEPARATOR) {
            dir.clone()
        } else {
            format!("{dir}{}", std::path::MAIN_SEPARATOR)
        };
        path.starts_with(&prefix)
    }) {
        return "seed-mount";
    }
    "user-install"
}
/// Maps to: CC `utils/telemetry/pluginTelemetry.ts#buildPluginTelemetryFields`.
pub fn build_plugin_telemetry_fields(name: &str, marketplace: Option<&str>) -> Value {
    let scope = get_telemetry_plugin_scope(marketplace);
    let controlled = matches!(scope, "official" | "default-bundle");
    json!({"plugin_id_hash": hash_plugin_id(name, marketplace), "plugin_scope": scope,
        "plugin_name_redacted": if controlled {name} else {"third-party"},
        "marketplace_name_redacted": if controlled {marketplace.filter(|s| !s.is_empty()).unwrap_or("third-party")} else {"third-party"},
        "is_official_plugin": controlled})
}
/// Maps to: CC `utils/telemetry/pluginTelemetry.ts#buildPluginCommandTelemetryFields`.
pub fn build_plugin_command_telemetry_fields(
    plugin_manifest: &crate::utils::plugins::schemas::PluginManifest,
    repository: &str,
) -> Value {
    let id = parse_plugin_identifier(repository);
    build_plugin_telemetry_fields(&plugin_manifest.name, id.marketplace.as_deref())
}
/// Maps to: CC `utils/telemetry/pluginTelemetry.ts#logPluginsEnabledForSession`.
pub fn log_plugins_enabled_for_session(plugins: &[LoadedPlugin], seed_dirs: &[String]) {
    for plugin in plugins {
        let id = parse_plugin_identifier(&plugin.repository);
        let mut fields = build_plugin_telemetry_fields(&plugin.name, id.marketplace.as_deref());
        fields["_PROTO_plugin_name"] = json!(plugin.name);
        if let Some(m) = id.marketplace.filter(|s| !s.is_empty()) {
            fields["_PROTO_marketplace_name"] = json!(m);
        }
        fields["enabled_via"] = json!(get_enabled_via(plugin, seed_dirs));
        fields["skill_path_count"] =
            json!(usize::from(plugin.skills_path.is_some()) + plugin.skills_paths.len());
        fields["command_path_count"] =
            json!(usize::from(plugin.commands_path.is_some()) + plugin.commands_paths.len());
        fields["has_mcp"] = json!(plugin.manifest.mcp_servers.is_some());
        fields["has_hooks"] = json!(plugin.hooks_config.is_some());
        if let Some(v) = plugin.manifest.version.as_ref().filter(|s| !s.is_empty()) {
            fields["version"] = json!(v);
        }
        crate::services::analytics::log_event("tengu_plugin_enabled_for_session", fields);
    }
}
/// Maps to: CC `utils/telemetry/pluginTelemetry.ts#PluginCommandErrorCategory`.
pub type PluginCommandErrorCategory = &'static str;
/// Maps to: CC `utils/telemetry/pluginTelemetry.ts#classifyPluginCommandError`.
/// The caller supplies the native error's message (the source String(error.message ?? error)).
pub fn classify_plugin_command_error(message: &str) -> PluginCommandErrorCategory {
    for (pattern, category) in [
        (
            r"(?i)ENOTFOUND|ECONNREFUSED|EAI_AGAIN|ETIMEDOUT|ECONNRESET|network|Could not resolve|Connection refused|timed out",
            "network",
        ),
        (
            r"(?i)\b404\b|not found|does not exist|no such plugin",
            "not-found",
        ),
        (
            r"(?i)\b40[13]\b|EACCES|EPERM|permission denied|unauthorized",
            "permission",
        ),
        (
            r"(?i)invalid|malformed|schema|validation|parse error",
            "validation",
        ),
    ] {
        if regex::Regex::new(pattern)
            .expect("source regex")
            .is_match(message)
        {
            return category;
        }
    }
    "unknown"
}
/// Maps to: CC `utils/telemetry/pluginTelemetry.ts#logPluginLoadErrors`.
pub fn log_plugin_load_errors(errors: &[PluginError]) {
    for error in errors {
        let value = serde_json::to_value(error).expect("PluginError serializes");
        let id = parse_plugin_identifier(value["source"].as_str().unwrap_or_default());
        let name = value["plugin"]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or(&id.name);
        let mut fields = build_plugin_telemetry_fields(name, id.marketplace.as_deref());
        fields["error_category"] = value["type"].clone();
        fields["_PROTO_plugin_name"] = json!(name);
        if let Some(m) = id.marketplace.filter(|s| !s.is_empty()) {
            fields["_PROTO_marketplace_name"] = json!(m);
        }
        crate::services::analytics::log_event("tengu_plugin_load_failed", fields);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identity_and_privacy_match_bun_source_oracle() {
        // Actual source executed by research/proof/plugin-panel-0914/telemetry-oracle.ts.
        for (m, hash, scope, controlled) in [
            ("", "f119456b1c528520", "user-local", false),
            ("builtin", "30e3ffbf95789011", "default-bundle", true),
            ("BUILTIN", "30e3ffbf95789011", "user-local", false),
            (
                "CLAUDE-PLUGINS-OFFICIAL",
                "75712fb13e04c41a",
                "official",
                true,
            ),
            ("private", "81551a8eaf85f87b", "user-local", false),
        ] {
            let v = build_plugin_telemetry_fields("Sample", Some(m));
            assert_eq!(v["plugin_id_hash"], hash);
            assert_eq!(v["plugin_scope"], scope);
            assert_eq!(v["is_official_plugin"], controlled);
            assert_eq!(
                v["plugin_name_redacted"],
                if controlled { "Sample" } else { "third-party" }
            );
        }
    }
}
