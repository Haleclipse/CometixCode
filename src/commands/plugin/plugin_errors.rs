//! Maps to: CC `commands/plugin/PluginErrors.tsx`.
use crate::types::plugin::{
    PluginDependencyReason, PluginError, PluginGitAuthType, PluginGitOperation,
};
use crate::utils::zod::javascript_number_to_string;

/// Maps to: CC PluginErrors.tsx:3-75#formatErrorMessage.
pub fn format_error_message(error: &PluginError) -> String {
    use PluginError::*;
    match error {
        PathNotFound {
            component, path, ..
        } => format!("{} path not found: {path}", component.as_str()),
        GitAuthFailed {
            auth_type, git_url, ..
        } => format!(
            "Git {} authentication failed for {git_url}",
            match auth_type {
                PluginGitAuthType::Ssh => "SSH",
                PluginGitAuthType::Https => "HTTPS",
            }
        ),
        GitTimeout {
            operation, git_url, ..
        } => format!(
            "Git {} timed out for {git_url}",
            match operation {
                PluginGitOperation::Clone => "clone",
                PluginGitOperation::Pull => "pull",
            }
        ),
        NetworkError { url, details, .. } => format!(
            "Network error accessing {url}{}",
            details
                .as_ref()
                .filter(|s| !s.is_empty())
                .map(|s| format!(": {s}"))
                .unwrap_or_default()
        ),
        ManifestParseError {
            manifest_path,
            parse_error,
            ..
        } => format!("Failed to parse manifest at {manifest_path}: {parse_error}"),
        ManifestValidationError {
            manifest_path,
            validation_errors,
            ..
        } => format!(
            "Invalid manifest at {manifest_path}: {}",
            validation_errors.join(", ")
        ),
        PluginNotFound {
            plugin_id,
            marketplace,
            ..
        } => format!("Plugin \"{plugin_id}\" not found in marketplace \"{marketplace}\""),
        MarketplaceNotFound { marketplace, .. } => {
            format!("Marketplace \"{marketplace}\" not found")
        }
        MarketplaceLoadFailed {
            marketplace,
            reason,
            ..
        } => format!("Failed to load marketplace \"{marketplace}\": {reason}"),
        McpConfigInvalid {
            server_name,
            validation_error,
            ..
        } => format!("Invalid MCP server config for \"{server_name}\": {validation_error}"),
        McpServerSuppressedDuplicate {
            server_name,
            duplicate_of,
            ..
        } => {
            let duplicate = if duplicate_of.starts_with("plugin:") {
                format!(
                    "server provided by plugin \"{}\"",
                    duplicate_of.split(':').nth(1).unwrap_or("?")
                )
            } else {
                format!("already-configured \"{duplicate_of}\"")
            };
            format!("MCP server \"{server_name}\" skipped — same command/URL as {duplicate}")
        }
        HookLoadFailed {
            hook_path, reason, ..
        } => format!("Failed to load hooks from {hook_path}: {reason}"),
        ComponentLoadFailed {
            component,
            path,
            reason,
            ..
        } => format!(
            "Failed to load {} from {path}: {reason}",
            component.as_str()
        ),
        McpbDownloadFailed { url, reason, .. } => {
            format!("Failed to download MCPB from {url}: {reason}")
        }
        McpbExtractFailed {
            mcpb_path, reason, ..
        } => format!("Failed to extract MCPB {mcpb_path}: {reason}"),
        McpbInvalidManifest {
            mcpb_path,
            validation_error,
            ..
        } => format!("MCPB manifest invalid at {mcpb_path}: {validation_error}"),
        DependencyUnsatisfied {
            dependency, reason, ..
        } => format!(
            "Dependency \"{dependency}\" is {}",
            if *reason == PluginDependencyReason::NotEnabled {
                "disabled"
            } else {
                "not installed"
            }
        ),
        LspConfigInvalid {
            server_name,
            validation_error,
            ..
        } => format!("Invalid LSP server config for \"{server_name}\": {validation_error}"),
        LspServerStartFailed {
            server_name,
            reason,
            ..
        } => format!("LSP server \"{server_name}\" failed to start: {reason}"),
        LspServerCrashed {
            server_name,
            signal,
            exit_code,
            ..
        } => {
            if let Some(signal) = signal.as_ref().filter(|s| !s.is_empty()) {
                format!("LSP server \"{server_name}\" crashed with signal {signal}")
            } else {
                format!(
                    "LSP server \"{server_name}\" crashed with exit code {}",
                    exit_code
                        .map(javascript_number_to_string)
                        .unwrap_or_else(|| "unknown".into())
                )
            }
        }
        LspRequestTimeout {
            server_name,
            method,
            timeout_ms,
            ..
        } => format!(
            "LSP server \"{server_name}\" timed out on {method} after {}ms",
            javascript_number_to_string(*timeout_ms)
        ),
        LspRequestFailed {
            server_name,
            method,
            error,
            ..
        } => format!("LSP server \"{server_name}\" {method} failed: {error}"),
        PluginCacheMiss {
            plugin,
            install_path,
            ..
        } => format!(
            "Plugin \"{plugin}\" not cached at {}",
            install_path
                .as_ref()
                .map(crate::utils::zod::js_string)
                .unwrap_or_else(|| "undefined".into())
        ),
        GenericError { error, .. } => error.clone(),
    }
}

/// Maps to: CC PluginErrors.tsx:77-140#getErrorGuidance.
pub fn get_error_guidance(error: &PluginError) -> Option<String> {
    use PluginError::*;
    Some(match error {
        PathNotFound { .. } => {
            "Check that the path in your manifest or marketplace config is correct".into()
        }
        GitAuthFailed { auth_type, .. } => if *auth_type == PluginGitAuthType::Ssh {
            "Configure SSH keys or use HTTPS URL instead"
        } else {
            "Configure credentials or use SSH URL instead"
        }
        .into(),
        GitTimeout { .. } | NetworkError { .. } => {
            "Check your internet connection and try again".into()
        }
        ManifestParseError { .. } => "Check manifest file syntax in the plugin directory".into(),
        ManifestValidationError { .. } => "Check manifest file follows the required schema".into(),
        PluginNotFound { marketplace, .. } => {
            format!("Plugin may not exist in marketplace \"{marketplace}\"")
        }
        MarketplaceNotFound {
            available_marketplaces,
            ..
        } => {
            if available_marketplaces.is_empty() {
                "Add the marketplace first using /plugin marketplace add".into()
            } else {
                format!(
                    "Available marketplaces: {}",
                    available_marketplaces.join(", ")
                )
            }
        }
        McpConfigInvalid { .. } => "Check MCP server configuration in .mcp.json or manifest".into(),
        McpServerSuppressedDuplicate { duplicate_of, .. } => {
            if duplicate_of.starts_with("plugin:") {
                format!(
                    "Disable plugin \"{}\" if you want this plugin's version instead",
                    duplicate_of.split(':').nth(1).unwrap_or("the other plugin")
                )
            } else {
                format!(
                    "Remove \"{duplicate_of}\" from your MCP config if you want the plugin's version instead"
                )
            }
        }
        HookLoadFailed { .. } => "Check hooks.json file syntax and structure".into(),
        ComponentLoadFailed { component, .. } => format!(
            "Check {} directory structure and file permissions",
            component.as_str()
        ),
        McpbDownloadFailed { .. } => "Check your internet connection and URL accessibility".into(),
        McpbExtractFailed { .. } => "Verify the MCPB file is valid and not corrupted".into(),
        McpbInvalidManifest { .. } => "Contact the plugin author about the invalid manifest".into(),
        DependencyUnsatisfied {
            dependency,
            plugin,
            reason,
            ..
        } => format!(
            "{} \"{dependency}\" or uninstall \"{plugin}\"",
            if *reason == PluginDependencyReason::NotEnabled {
                "Enable"
            } else {
                "Install"
            }
        ),
        LspConfigInvalid { .. } => "Check LSP server configuration in the plugin manifest".into(),
        LspServerStartFailed { .. }
        | LspServerCrashed { .. }
        | LspRequestTimeout { .. }
        | LspRequestFailed { .. } => "Check LSP server logs with --debug for details".into(),
        PluginCacheMiss { .. } => "Run /plugins to refresh the plugin cache".into(),
        MarketplaceLoadFailed { .. } | GenericError { .. } => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn duplicate_guidance_matches_official_plugin_and_external_owners() {
        // PluginErrors.tsx:24-29,98-110: empty split field remains empty (??).
        for (duplicate, expected, guidance) in [
            (
                "plugin:one:srv",
                "server provided by plugin \"one\"",
                "Disable plugin \"one\" if you want this plugin's version instead",
            ),
            (
                "plugin:",
                "server provided by plugin \"\"",
                "Disable plugin \"\" if you want this plugin's version instead",
            ),
            (
                "local",
                "already-configured \"local\"",
                "Remove \"local\" from your MCP config if you want the plugin's version instead",
            ),
        ] {
            let error = PluginError::McpServerSuppressedDuplicate {
                source: "p@m".into(),
                plugin: "p".into(),
                server_name: "srv".into(),
                duplicate_of: duplicate.into(),
            };
            assert_eq!(
                format_error_message(&error),
                format!("MCP server \"srv\" skipped — same command/URL as {expected}")
            );
            assert_eq!(get_error_guidance(&error).as_deref(), Some(guidance));
        }
    }
}
