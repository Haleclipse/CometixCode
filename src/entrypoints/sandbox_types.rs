//! Sandbox types for the Claude Code Agent SDK.
//!
//! Maps to: CC `entrypoints/sandboxTypes.ts` — the single source of truth for
//! sandbox configuration schemas; both the SDK and the settings validation
//! import from here. (The runtime config structs the adapter consumes still
//! live in `utils/sandbox/sandbox_adapter.rs`; consolidating them here is
//! tracked in MODULE_MAP.)

use std::sync::OnceLock;

use crate::utils::zod::{self, Schema};

/// Maps to: CC `SandboxNetworkConfigSchema` (`sandboxTypes.ts:14-41`).
/// The schema itself carries the trailing `.optional()`.
pub fn sandbox_network_config_schema() -> &'static Schema {
    static SCHEMA: OnceLock<Schema> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        zod::object(vec![
            ("allowedDomains", zod::array(zod::string()).optional()),
            (
                "allowUnixSockets",
                zod::array(zod::string()).optional().describe(
                    "macOS only: Unix socket paths to allow. Ignored on Linux (seccomp cannot filter by path).",
                ),
            ),
            (
                "allowAllUnixSockets",
                zod::boolean().optional().describe(
                    "If true, allow all Unix sockets (disables blocking on both platforms).",
                ),
            ),
            ("allowLocalBinding", zod::boolean().optional()),
            ("httpProxyPort", zod::number().optional()),
            ("socksProxyPort", zod::number().optional()),
        ])
        .optional()
    })
}

/// Maps to: CC `SandboxFilesystemConfigSchema` (`sandboxTypes.ts:46-85`).
/// The schema itself carries the trailing `.optional()`.
pub fn sandbox_filesystem_config_schema() -> &'static Schema {
    static SCHEMA: OnceLock<Schema> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        zod::object(vec![
            (
                "allowWrite",
                zod::array(zod::string()).optional().describe(
                    "Additional paths to allow writing within the sandbox. Merged with paths from Edit(...) allow permission rules.",
                ),
            ),
            (
                "denyWrite",
                zod::array(zod::string()).optional().describe(
                    "Additional paths to deny writing within the sandbox. Merged with paths from Edit(...) deny permission rules.",
                ),
            ),
            (
                "denyRead",
                zod::array(zod::string()).optional().describe(
                    "Additional paths to deny reading within the sandbox. Merged with paths from Read(...) deny permission rules.",
                ),
            ),
            (
                "allowRead",
                zod::array(zod::string()).optional().describe(
                    "Paths to re-allow reading within denyRead regions. Takes precedence over denyRead for matching paths.",
                ),
            ),
        ])
        .optional()
    })
}

/// Maps to: CC `SandboxSettingsSchema` (`sandboxTypes.ts:90-146`).
///
/// `enabledPlatforms` stays undocumented and is read via the outer
/// `.passthrough()` — it restricts sandboxing to specific platforms (added to
/// unblock an enterprise rollout that enables autoAllowBashIfSandboxed on
/// macOS only).
pub fn sandbox_settings_schema() -> &'static Schema {
    static SCHEMA: OnceLock<Schema> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        zod::passthrough_object(vec![
            ("enabled", zod::boolean().optional()),
            (
                "failIfUnavailable",
                zod::boolean().optional().describe(
                    "Exit with an error at startup if sandbox.enabled is true but the sandbox cannot start (missing dependencies, unsupported platform, or platform not in enabledPlatforms). When false (default), a warning is shown and commands run unsandboxed. Intended for managed-settings deployments that require sandboxing as a hard gate.",
                ),
            ),
            ("autoAllowBashIfSandboxed", zod::boolean().optional()),
            (
                "allowUnsandboxedCommands",
                zod::boolean().optional().describe(
                    "Allow commands to run outside the sandbox via the dangerouslyDisableSandbox parameter. When false, the dangerouslyDisableSandbox parameter is completely ignored and all commands must run sandboxed. Default: true.",
                ),
            ),
            ("network", sandbox_network_config_schema().clone()),
            ("filesystem", sandbox_filesystem_config_schema().clone()),
            (
                "ignoreViolations",
                zod::record(zod::array(zod::string())).optional(),
            ),
            ("enableWeakerNestedSandbox", zod::boolean().optional()),
            (
                "enableWeakerNetworkIsolation",
                zod::boolean().optional().describe(
                    "macOS only: Allow access to com.apple.trustd.agent in the sandbox. Needed for Go-based CLI tools (gh, gcloud, terraform, etc.) to verify TLS certificates when using httpProxyPort with a MITM proxy and custom CA. **Reduces security** — opens a potential data exfiltration vector through the trustd service. Default: false",
                ),
            ),
            ("excludedCommands", zod::array(zod::string()).optional()),
            (
                "ripgrep",
                zod::object(vec![
                    ("command", zod::string()),
                    ("args", zod::array(zod::string()).optional()),
                ])
                .optional()
                .describe("Custom ripgrep configuration for bundled ripgrep support"),
            ),
        ])
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::zod::{safe_parse, to_json_schema};
    use serde_json::json;

    #[test]
    fn sandbox_settings_parse_nested_configs_and_pass_unknown_keys() {
        let ok = json!({
            "enabled": true,
            "network": {"allowedDomains": ["example.com"], "httpProxyPort": 8080},
            "filesystem": {"allowWrite": ["/tmp"]},
            "ignoreViolations": {"git": ["network"]},
            "ripgrep": {"command": "/usr/bin/rg"},
            "enabledPlatforms": ["macos"],
        });
        let parsed = safe_parse(sandbox_settings_schema(), &ok).expect("sandbox parses");
        // enabledPlatforms is undocumented and survives via passthrough.
        assert_eq!(parsed["enabledPlatforms"], json!(["macos"]));

        // A wrong nested type fails with the nested path.
        let error = safe_parse(
            sandbox_settings_schema(),
            &json!({"network": {"httpProxyPort": "8080"}}),
        )
        .expect_err("string port fails");
        assert_eq!(error.issues[0].path.len(), 2);

        // ripgrep.command is required inside its object.
        assert!(safe_parse(sandbox_settings_schema(), &json!({"ripgrep": {}})).is_err());
    }

    #[test]
    fn sandbox_projection_keeps_every_field_optional() {
        let projected = to_json_schema(sandbox_settings_schema());
        assert!(
            projected.get("required").is_none(),
            "no required keys: {projected}"
        );
        // The outer passthrough admits unknown keys as an empty subschema.
        assert_eq!(projected["additionalProperties"], json!({}));
    }
}
