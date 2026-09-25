use serde::{Deserialize, Serialize};
use std::sync::Arc;

use indexmap::IndexMap;

fn serialize_settings_env<S>(
    env: &Option<Arc<IndexMap<String, String>>>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match env.as_deref() {
        Some(env) => crate::utils::process_env::serialize_ecmascript_object(env.iter(), serializer),
        None => serializer.serialize_none(),
    }
}

fn deserialize_settings_env<'de, D>(
    deserializer: D,
) -> Result<Option<Arc<IndexMap<String, String>>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let env = Option::<IndexMap<String, String>>::deserialize(deserializer)?;
    Ok(env.map(|env| {
        Arc::new(
            crate::utils::process_env::ecmascript_object_entries(env.iter())
                .into_iter()
                .map(|(key, value)| (key.to_string(), value.clone()))
                .collect(),
        )
    }))
}

// ════════════════════════════════════════════════════════════
// ════════════════════════════════════════════════════════════

// Maps to CC `utils/settings/types.ts` importing these constants from
// `utils/permissions/PermissionMode.ts`.
pub use crate::utils::permissions::permission_mode::{EXTERNAL_PERMISSION_MODES, PERMISSION_MODES};

pub const CUSTOMIZATION_SURFACES: &[&str] = &["skills", "agents", "hooks", "mcp"];

pub const EFFORT_LEVELS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

pub const DEFAULT_SHELLS: &[&str] = &["bash", "powershell"];

pub const LOGIN_METHODS: &[&str] = &["claudeai", "console"];

pub const UPDATE_CHANNELS: &[&str] = &["latest", "stable"];

pub const SPINNER_VERB_MODES: &[&str] = &["append", "replace"];

pub const DEFAULT_VIEWS: &[&str] = &["chat", "transcript"];

// ════════════════════════════════════════════════════════════
// ════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct SettingsJson {
    #[serde(rename = "$schema")]
    pub schema: Option<String>,

    pub api_key_helper: Option<String>,
    pub aws_credential_export: Option<String>,
    pub aws_auth_refresh: Option<String>,
    pub gcp_auth_refresh: Option<String>,
    pub otel_headers_helper: Option<String>,

    pub model: Option<String>,
    pub available_models: Option<Vec<String>>,
    /// JavaScript object insertion order is observable in
    /// `modelStrings.ts#resolveOverriddenModel` when values collide.
    pub model_overrides: Option<indexmap::IndexMap<String, String>>,
    pub advisor_model: Option<String>,

    #[serde(
        default,
        serialize_with = "serialize_settings_env",
        deserialize_with = "deserialize_settings_env"
    )]
    pub env: Option<Arc<IndexMap<String, String>>>,

    pub permissions: Option<PermissionsSettings>,
    pub default_permission_mode: Option<String>,

    // ── Hooks ──
    pub hooks: Option<serde_json::Value>,
    pub disable_all_hooks: Option<bool>,
    pub allow_managed_hooks_only: Option<bool>,
    pub allowed_http_hook_urls: Option<Vec<String>>,
    pub http_hook_allowed_env_vars: Option<Vec<String>>,

    // ── StatusLine ──
    pub status_line: Option<StatusLineSettings>,

    // ── MCP Servers ──
    pub enable_all_project_mcp_servers: Option<bool>,
    pub enabled_mcpjson_servers: Option<Vec<String>>,
    pub disabled_mcpjson_servers: Option<Vec<String>>,
    pub allowed_mcp_servers: Option<Vec<AllowedMcpServerEntry>>,
    pub denied_mcp_servers: Option<Vec<DeniedMcpServerEntry>>,
    pub allow_managed_mcp_servers_only: Option<bool>,

    pub output_style: Option<String>,
    pub language: Option<String>,
    pub syntax_highlighting_disabled: Option<bool>,
    pub prefers_reduced_motion: Option<bool>,
    /// How the in-progress assistant reply is previewed while streaming:
    /// `"character"` (every delta, Pi-style typewriter) or `"line"` (only
    /// completed lines, CC `REPL.tsx:1997`). `prefersReducedMotion` overrides
    /// both and hides the preview entirely, as in CC.
    ///
    /// @cometix offset: not an official settings.json key. CC has only the
    /// line mode; Cometix keeps character streaming as an option.
    pub streaming_text_display: Option<String>,
    pub show_thinking_summaries: Option<bool>,
    /// Official GrowthBook `tengu_prompt_cache_1h_config` payload.
    ///
    /// @cometix offset: not an official settings.json key. GrowthBook is not
    /// ported, so users set `promptCache1h.allowlist` here.
    pub prompt_cache_1h: Option<PromptCache1hSettings>,

    pub spinner_tips_enabled: Option<bool>,
    pub spinner_verbs: Option<SpinnerVerbsSettings>,
    pub spinner_tips_override: Option<SpinnerTipsOverride>,
    pub always_thinking_enabled: Option<bool>,
    pub fast_mode: Option<bool>,
    pub fast_mode_per_session_opt_in: Option<bool>,
    pub prompt_suggestion_enabled: Option<bool>,
    pub terminal_title_from_rename: Option<bool>,
    pub feedback_survey_rate: Option<f64>,
    pub skip_web_fetch_preflight: Option<bool>,

    pub auto_mode_threshold: Option<String>,
    pub auto_mode: Option<AutoModeSettings>,
    pub disable_auto_mode: Option<String>,
    pub skip_auto_permission_prompt: Option<bool>,
    pub use_auto_mode_during_plan: Option<bool>,

    pub respect_gitignore: Option<bool>,
    pub cleanup_period_days: Option<u32>,
    pub file_suggestion: Option<FileSuggestionSettings>,
    pub attribution: Option<AttributionSettings>,
    pub include_co_authored_by: Option<bool>,
    pub include_git_instructions: Option<bool>,

    // ── Worktree ──
    pub worktree: Option<WorktreeSettings>,

    // ── Sandbox ──
    pub sandbox: Option<serde_json::Value>,

    // ── Shell ──
    pub default_shell: Option<String>,

    pub force_login_method: Option<String>,
    pub force_login_org_uuid: Option<String>,

    // ── Plugins ──
    pub enabled_plugins: Option<serde_json::Value>,
    pub strict_plugin_only_customization: Option<serde_json::Value>,
    pub plugin_configs: Option<serde_json::Value>,
    pub plugin_trust_message: Option<String>,

    // ── Marketplace ──
    pub extra_known_marketplaces: Option<serde_json::Value>,
    pub strict_known_marketplaces: Option<Vec<serde_json::Value>>,
    pub blocked_marketplaces: Option<Vec<serde_json::Value>>,

    pub allow_managed_permission_rules_only: Option<bool>,
    pub skip_dangerous_mode_permission_prompt: Option<bool>,
    pub classifier_permissions_enabled: Option<bool>,

    pub channels_enabled: Option<bool>,
    pub allowed_channel_plugins: Option<Vec<serde_json::Value>>,

    // ── Memory ──
    pub claude_md_excludes: Option<Vec<String>>,
    pub auto_memory_enabled: Option<bool>,
    pub auto_memory_directory: Option<String>,
    pub auto_dream_enabled: Option<bool>,

    // ── Remote ──
    pub remote: Option<RemoteSettings>,

    pub auto_updates_channel: Option<String>,
    pub minimum_version: Option<String>,

    pub plans_directory: Option<String>,
    pub show_clear_context_on_plan_accept: Option<bool>,
    pub agent: Option<String>,
    pub company_announcements: Option<Vec<String>>,
    pub effort_level: Option<String>,
    /// Session seed consumed at startup by the 2.1.198 ultracode chain.
    pub ultracode: Option<bool>,

    pub voice_enabled: Option<bool>,
    pub assistant: Option<bool>,
    pub assistant_name: Option<String>,
    pub default_view: Option<String>,
    pub min_sleep_duration_ms: Option<u64>,
    pub max_sleep_duration_ms: Option<i64>,

    // ── XAA IdP (feature-gated) ──
    pub xaa_idp: Option<XaaIdpSettings>,

    pub disable_deep_link_registration: Option<String>,
}

// ════════════════════════════════════════════════════════════
// ════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct PermissionsSettings {
    pub allow: Option<Vec<String>>,
    pub deny: Option<Vec<String>>,
    pub ask: Option<Vec<String>>,
    pub default_mode: Option<String>,
    pub disable_bypass_permissions_mode: Option<String>,
    pub disable_auto_mode: Option<String>,
    pub additional_directories: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusLineSettings {
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub command: String,
    pub padding: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AllowedMcpServerEntry {
    pub server_name: Option<String>,
    pub server_command: Option<Vec<String>>,
    pub server_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeniedMcpServerEntry {
    pub server_name: Option<String>,
    pub server_command: Option<Vec<String>>,
    pub server_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileSuggestionSettings {
    #[serde(rename = "type")]
    pub kind: String,
    pub command: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttributionSettings {
    pub commit: Option<String>,
    pub pr: Option<String>,
}

/// Official GrowthBook `tengu_prompt_cache_1h_config` object.
///
/// @cometix offset: settings-backed replacement for the remote allowlist.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptCache1hSettings {
    pub allowlist: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeSettings {
    pub symlink_directories: Option<Vec<String>>,
    pub sparse_paths: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpinnerVerbsSettings {
    pub mode: String,
    pub verbs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpinnerTipsOverride {
    pub exclude_default: Option<bool>,
    pub tips: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoModeSettings {
    pub allow: Option<Vec<String>>,
    #[serde(rename = "soft_deny")]
    pub soft_deny: Option<Vec<String>>,
    pub deny: Option<Vec<String>>,
    pub environment: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteSettings {
    pub default_environment_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct XaaIdpSettings {
    pub issuer: String,
    pub client_id: String,
    pub callback_port: Option<u16>,
}

// ════════════════════════════════════════════════════════════
// ════════════════════════════════════════════════════════════

impl StatusLineSettings {
    pub fn is_command_type(&self) -> bool {
        self.kind.as_deref().unwrap_or("command") == "command"
    }
}

impl AllowedMcpServerEntry {
    pub fn is_name_entry(&self) -> bool {
        self.server_name.is_some()
    }
    pub fn is_command_entry(&self) -> bool {
        self.server_command.is_some()
    }
    pub fn is_url_entry(&self) -> bool {
        self.server_url.is_some()
    }
}

impl DeniedMcpServerEntry {
    pub fn is_name_entry(&self) -> bool {
        self.server_name.is_some()
    }
    pub fn is_command_entry(&self) -> bool {
        self.server_command.is_some()
    }
    pub fn is_url_entry(&self) -> bool {
        self.server_url.is_some()
    }
}

// Maps to: CC `utils/settings/types.ts:29` — hook types are imported from
// schemas/hooks and re-exported for the rest of the tree.
pub use crate::schemas::hooks::{HookCommand, HookConfigEntry, HooksConfig};

// ─── Zod carrier schemas ─────────────────────────────────────────────────
//
// Maps to: CC `utils/settings/types.ts` leaf schemas. The serde structs above
// remain the post-parse projections (the `FileReadInput` pattern); these
// carriers own validation copy and the JSON Schema projection. The
// SettingsSchema assembly itself lands with the marketplace family
// (`MarketplaceSourceSchema` is its remaining embedded dependency).

/// Maps to: CC `utils/settings/types.ts:35-37` `EnvironmentVariablesSchema` —
/// `z.record(z.string(), z.coerce.string())`: env values coerce through
/// ECMAScript `String()` (numbers, booleans arrive from JSON authors).
pub fn environment_variables_schema() -> &'static crate::utils::zod::Schema {
    static SCHEMA: std::sync::OnceLock<crate::utils::zod::Schema> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| {
        use crate::utils::zod;
        zod::record(zod::coerce_string())
    })
}

fn mcp_entry_has_exactly_one_matcher(data: &serde_json::Value) -> bool {
    let defined = ["serverName", "serverCommand", "serverUrl"]
        .iter()
        .filter(|key| data.get(**key).is_some())
        .count();
    defined == 1
}

/// Maps to: CC `utils/settings/types.ts:115-160` `AllowedMcpServerEntrySchema`.
pub fn allowed_mcp_server_entry_schema() -> &'static crate::utils::zod::Schema {
    static SCHEMA: std::sync::OnceLock<crate::utils::zod::Schema> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| {
        use crate::utils::zod;
        zod::object(vec![
            (
                "serverName",
                zod::string()
                    .regex_with_message(
                        "^[a-zA-Z0-9_-]+$",
                        "Server name can only contain letters, numbers, hyphens, and underscores",
                    )
                    .optional()
                    .describe("Name of the MCP server that users are allowed to configure"),
            ),
            (
                "serverCommand",
                zod::array(zod::string())
                    .min_with_message(
                        1,
                        "Server command must have at least one element (the command)",
                    )
                    .optional()
                    .describe(
                        "Command array [command, ...args] to match exactly for allowed stdio servers",
                    ),
            ),
            (
                "serverUrl",
                zod::string().optional().describe(
                    "URL pattern with wildcard support (e.g., \"https://*.example.com/*\") for allowed remote MCP servers",
                ),
            ),
        ])
        .refine(
            mcp_entry_has_exactly_one_matcher,
            "Entry must have exactly one of \"serverName\", \"serverCommand\", or \"serverUrl\"",
        )
    })
}

/// Maps to: CC `utils/settings/types.ts:164-206` `DeniedMcpServerEntrySchema`.
pub fn denied_mcp_server_entry_schema() -> &'static crate::utils::zod::Schema {
    static SCHEMA: std::sync::OnceLock<crate::utils::zod::Schema> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| {
        use crate::utils::zod;
        zod::object(vec![
            (
                "serverName",
                zod::string()
                    .regex_with_message(
                        "^[a-zA-Z0-9_-]+$",
                        "Server name can only contain letters, numbers, hyphens, and underscores",
                    )
                    .optional()
                    .describe("Name of the MCP server that is explicitly blocked"),
            ),
            (
                "serverCommand",
                zod::array(zod::string())
                    .min_with_message(
                        1,
                        "Server command must have at least one element (the command)",
                    )
                    .optional()
                    .describe(
                        "Command array [command, ...args] to match exactly for blocked stdio servers",
                    ),
            ),
            (
                "serverUrl",
                zod::string().optional().describe(
                    "URL pattern with wildcard support (e.g., \"https://*.example.com/*\") for blocked remote MCP servers",
                ),
            ),
        ])
        .refine(
            mcp_entry_has_exactly_one_matcher,
            "Entry must have exactly one of \"serverName\", \"serverCommand\", or \"serverUrl\"",
        )
    })
}

/// Maps to: CC `utils/settings/types.ts:42-85` `PermissionsSchema`.
///
/// `defaultMode` and `disableAutoMode` follow CC's
/// `feature('TRANSCRIPT_CLASSIFIER')` gate at build time — the OnceLock
/// freezes the first-access flag state exactly as CC's memoized lazySchema
/// freezes its module-load state.
pub fn permissions_schema() -> &'static crate::utils::zod::Schema {
    static SCHEMA: std::sync::OnceLock<crate::utils::zod::Schema> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| {
        use crate::utils::feature_flags::{FeatureFlag, feature_enabled};
        use crate::utils::settings::permission_validation::permission_rule_schema;
        use crate::utils::zod;
        let transcript_classifier = feature_enabled(FeatureFlag::TranscriptClassifier);
        let mut shape = vec![
            (
                "allow",
                zod::array(permission_rule_schema().clone())
                    .optional()
                    .describe("List of permission rules for allowed operations"),
            ),
            (
                "deny",
                zod::array(permission_rule_schema().clone())
                    .optional()
                    .describe("List of permission rules for denied operations"),
            ),
            (
                "ask",
                zod::array(permission_rule_schema().clone())
                    .optional()
                    .describe(
                        "List of permission rules that should always prompt for confirmation",
                    ),
            ),
            (
                "defaultMode",
                zod::enumeration(if transcript_classifier {
                    crate::types::permissions::PERMISSION_MODES.to_vec()
                } else {
                    crate::types::permissions::EXTERNAL_PERMISSION_MODES.to_vec()
                })
                .optional()
                .describe("Default permission mode when Claude Code needs access"),
            ),
            (
                "disableBypassPermissionsMode",
                zod::enumeration(vec!["disable"])
                    .optional()
                    .describe("Disable the ability to bypass permission prompts"),
            ),
        ];
        if transcript_classifier {
            shape.push((
                "disableAutoMode",
                zod::enumeration(vec!["disable"])
                    .optional()
                    .describe("Disable auto mode"),
            ));
        }
        shape.push((
            "additionalDirectories",
            zod::array(zod::string())
                .optional()
                .describe("Additional directories to include in the permission scope"),
        ));
        zod::passthrough_object(shape)
    })
}

/// Maps to: CC `utils/settings/types.ts:91-110` `ExtraKnownMarketplaceSchema`.
///
/// Extra marketplaces defined in repository settings — same as KnownMarketplace
/// but without lastUpdated (which is managed automatically).
pub fn extra_known_marketplace_schema() -> &'static crate::utils::zod::Schema {
    static SCHEMA: std::sync::OnceLock<crate::utils::zod::Schema> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| {
        use crate::utils::plugins::schemas::marketplace_source_schema;
        use crate::utils::zod;
        zod::object(vec![
            (
                "source",
                marketplace_source_schema()
                    .clone()
                    .describe("Where to fetch the marketplace from"),
            ),
            (
                "installLocation",
                zod::string().optional().describe(
                    "Local cache path where marketplace manifest is stored (auto-generated if not provided)",
                ),
            ),
            (
                "autoUpdate",
                zod::boolean().optional().describe(
                    "Whether to automatically update this marketplace and its installed plugins on startup",
                ),
            ),
        ])
    })
}

/// Maps to: CC `utils/settings/types.ts:255-282` `strictPluginOnlyCustomization`'s
/// preprocess — forwards-compat: drop unknown surface names so a future enum
/// value doesn't fail safeParse and null out the ENTIRE managed-settings file.
/// Degrades to less-locked, never to everything-unlocked.
fn drop_unknown_customization_surfaces(v: &serde_json::Value) -> serde_json::Value {
    match v.as_array() {
        Some(arr) => serde_json::Value::Array(
            arr.iter()
                .filter(|x| {
                    x.as_str()
                        .is_some_and(|s| CUSTOMIZATION_SURFACES.contains(&s))
                })
                .cloned()
                .collect(),
        ),
        None => v.clone(),
    }
}

/// Maps to: CC `utils/settings/types.ts:570-596` — the `extraKnownMarketplaces`
/// `.check`: for settings sources, the dict key must equal `source.name`, or
/// the marketplace reconciler never converges (key-lookup misses every
/// session). For github/git/url the name comes from a fetched
/// marketplace.json, so a mismatch there is expected and benign.
fn extra_known_marketplaces_settings_key_check(
    v: &crate::utils::zod::Value,
) -> Vec<crate::utils::zod::CheckIssue> {
    use crate::utils::zod::{CheckIssue, PathSegment};
    let mut issues = Vec::new();
    if let Some(map) = v.as_object() {
        for (key, entry) in map {
            let source = &entry["source"];
            if source["source"] == "settings" {
                if let Some(name) = source["name"].as_str() {
                    if name != key {
                        issues.push(CheckIssue {
                            path: vec![
                                PathSegment::Key(key.clone()),
                                PathSegment::Key("source".to_string()),
                                PathSegment::Key("name".to_string()),
                            ],
                            message: format!(
                                "Settings-sourced marketplace name must match its extraKnownMarketplaces key (got key \"{key}\" but source.name \"{name}\")"
                            ),
                        });
                    }
                }
            }
        }
    }
    issues
}

/// Maps to: CC `utils/settings/types.ts:255-1148` `SettingsSchema`.
///
/// Build-time gates, frozen at first access exactly as CC's memoized
/// lazySchema freezes module-load state:
/// - `CLAUDE_CODE_ENABLE_XAA` env → `xaaIdp`
/// - `USER_TYPE=ant` → `classifierPermissionsEnabled`, the `max` effort
///   level, and the `autoMode.deny` back-compat alias
/// - `feature('TRANSCRIPT_CLASSIFIER')` (on) → `skipAutoPermissionPrompt`,
///   `useAutoModeDuringPlan`, `autoMode`
/// - `feature('KAIROS') || feature('KAIROS_BRIEF')` (Brief on) → `defaultView`
/// - `feature('LODESTONE')` / `feature('PROACTIVE')` / `feature('KAIROS')` /
///   `feature('VOICE_MODE')` are absent or off here, so
///   `disableDeepLinkRegistration`, the Sleep bounds, `voiceEnabled`,
///   `assistant`, and `assistantName` do not enter the shape — same result as
///   CC with those build features off.
///
/// `enabledPlugins`' `z.undefined()` union arm is unreachable from JSON input
/// (no JSON value parses to undefined) and is not mirrored.
pub fn settings_schema() -> &'static crate::utils::zod::Schema {
    static SCHEMA: std::sync::OnceLock<crate::utils::zod::Schema> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| {
        use crate::entrypoints::sandbox_types::sandbox_settings_schema;
        use crate::schemas::hooks::hooks_schema;
        use crate::utils::env_utils::is_env_truthy;
        use crate::utils::feature_flags::{feature_enabled, FeatureFlag};
        use crate::utils::plugins::schemas::marketplace_source_schema;
        use crate::utils::zod;
        use serde_json::json;

        let transcript_classifier = feature_enabled(FeatureFlag::TranscriptClassifier);
        let kairos_or_brief =
            feature_enabled(FeatureFlag::Kairos) || feature_enabled(FeatureFlag::KairosBrief);
        // USER_TYPE is a build-time define in CC. Keep the same axis for the
        // remaining internal-only schema fields.
        // @cometix: persist `max` as a production effortLevel (CC 2.1.88: ant-only).
        let is_ant = crate::utils::build_profile::build_audience().is_internal();
        let xaa = is_env_truthy(std::env::var("CLAUDE_CODE_ENABLE_XAA").ok().as_deref());

        let mut shape = vec![
            (
                "$schema",
                zod::literal(json!(super::constants::CLAUDE_CODE_SETTINGS_SCHEMA_URL))
                    .optional()
                    .describe("JSON Schema reference for Claude Code settings"),
            ),
            (
                "apiKeyHelper",
                zod::string()
                    .optional()
                    .describe("Path to a script that outputs authentication values"),
            ),
            (
                "awsCredentialExport",
                zod::string()
                    .optional()
                    .describe("Path to a script that exports AWS credentials"),
            ),
            (
                "awsAuthRefresh",
                zod::string()
                    .optional()
                    .describe("Path to a script that refreshes AWS authentication"),
            ),
            (
                "gcpAuthRefresh",
                zod::string().optional().describe(
                    "Command to refresh GCP authentication (e.g., gcloud auth application-default login)",
                ),
            ),
        ];
        // Gated so the SDK generator (which runs without CLAUDE_CODE_ENABLE_XAA)
        // doesn't surface this; the outer .passthrough() keeps an existing key
        // alive across env-var-off sessions.
        if xaa {
            shape.push((
                "xaaIdp",
                zod::object(vec![
                    (
                        "issuer",
                        zod::string()
                            .url()
                            .describe("IdP issuer URL for OIDC discovery"),
                    ),
                    (
                        "clientId",
                        zod::string().describe("Claude Code's client_id registered at the IdP"),
                    ),
                    (
                        "callbackPort",
                        zod::number().int().positive().optional().describe(
                            "Fixed loopback callback port for the IdP OIDC login. Only needed if the IdP does not honor RFC 8252 port-any matching.",
                        ),
                    ),
                ])
                .optional()
                .describe(
                    "XAA (SEP-990) IdP connection. Configure once; all XAA-enabled MCP servers reuse this.",
                ),
            ));
        }
        shape.extend(vec![
            (
                "fileSuggestion",
                zod::object(vec![
                    ("type", zod::literal(json!("command"))),
                    ("command", zod::string()),
                ])
                .optional()
                .describe("Custom file suggestion configuration for @ mentions"),
            ),
            (
                "respectGitignore",
                zod::boolean().optional().describe(
                    "Whether file picker should respect .gitignore files (default: true). Note: .ignore files are always respected.",
                ),
            ),
            (
                "cleanupPeriodDays",
                zod::number().nonnegative().int().optional().describe(
                    "Number of days to retain chat transcripts (default: 30). Setting to 0 disables session persistence entirely: no transcripts are written and existing transcripts are deleted at startup.",
                ),
            ),
            (
                "env",
                environment_variables_schema()
                    .clone()
                    .optional()
                    .describe("Environment variables to set for Claude Code sessions"),
            ),
            (
                "attribution",
                zod::object(vec![
                    (
                        "commit",
                        zod::string().optional().describe(
                            "Attribution text for git commits, including any trailers. Empty string hides attribution.",
                        ),
                    ),
                    (
                        "pr",
                        zod::string().optional().describe(
                            "Attribution text for pull request descriptions. Empty string hides attribution.",
                        ),
                    ),
                ])
                .optional()
                .describe(
                    "Customize attribution text for commits and PRs. Each field defaults to the standard Claude Code attribution if not set.",
                ),
            ),
            (
                "includeCoAuthoredBy",
                zod::boolean().optional().describe(
                    "Deprecated: Use attribution instead. Whether to include Claude's co-authored by attribution in commits and PRs (defaults to true)",
                ),
            ),
            (
                "includeGitInstructions",
                zod::boolean().optional().describe(
                    "Include built-in commit and PR workflow instructions in Claude's system prompt (default: true)",
                ),
            ),
            (
                "permissions",
                permissions_schema()
                    .clone()
                    .optional()
                    .describe("Tool usage permissions configuration"),
            ),
            (
                "model",
                zod::string()
                    .optional()
                    .describe("Override the default model used by Claude Code"),
            ),
            (
                "availableModels",
                zod::array(zod::string()).optional().describe(
                    "Allowlist of models that users can select. Accepts family aliases (\"opus\" allows any opus version), version prefixes (\"opus-4-5\" allows only that version), and full model IDs. If undefined, all models are available. If empty array, only the default model is available. Typically set in managed settings by enterprise administrators.",
                ),
            ),
            (
                "modelOverrides",
                zod::record(zod::string()).optional().describe(
                    "Override mapping from Anthropic model ID (e.g. \"claude-opus-4-6\") to provider-specific model ID (e.g. a Bedrock inference profile ARN). Typically set in managed settings by enterprise administrators.",
                ),
            ),
            (
                "enableAllProjectMcpServers",
                zod::boolean()
                    .optional()
                    .describe("Whether to automatically approve all MCP servers in the project"),
            ),
            (
                "enabledMcpjsonServers",
                zod::array(zod::string())
                    .optional()
                    .describe("List of approved MCP servers from .mcp.json"),
            ),
            (
                "disabledMcpjsonServers",
                zod::array(zod::string())
                    .optional()
                    .describe("List of rejected MCP servers from .mcp.json"),
            ),
            (
                "allowedMcpServers",
                zod::array(allowed_mcp_server_entry_schema().clone())
                    .optional()
                    .describe(
                        "Enterprise allowlist of MCP servers that can be used. Applies to all scopes including enterprise servers from managed-mcp.json. If undefined, all servers are allowed. If empty array, no servers are allowed. Denylist takes precedence - if a server is on both lists, it is denied.",
                    ),
            ),
            (
                "deniedMcpServers",
                zod::array(denied_mcp_server_entry_schema().clone())
                    .optional()
                    .describe(
                        "Enterprise denylist of MCP servers that are explicitly blocked. If a server is on the denylist, it will be blocked across all scopes including enterprise. Denylist takes precedence over allowlist - if a server is on both lists, it is denied.",
                    ),
            ),
            (
                "hooks",
                hooks_schema()
                    .clone()
                    .optional()
                    .describe("Custom commands to run before/after tool executions"),
            ),
            (
                "worktree",
                zod::object(vec![
                    (
                        "symlinkDirectories",
                        zod::array(zod::string()).optional().describe(
                            "Directories to symlink from main repository to worktrees to avoid disk bloat. Must be explicitly configured - no directories are symlinked by default. Common examples: \"node_modules\", \".cache\", \".bin\"",
                        ),
                    ),
                    (
                        "sparsePaths",
                        zod::array(zod::string()).optional().describe(
                            "Directories to include when creating worktrees, via git sparse-checkout (cone mode). Dramatically faster in large monorepos — only the listed paths are written to disk.",
                        ),
                    ),
                ])
                .optional()
                .describe("Git worktree configuration for --worktree flag."),
            ),
            (
                "disableAllHooks",
                zod::boolean()
                    .optional()
                    .describe("Disable all hooks and statusLine execution"),
            ),
            (
                "defaultShell",
                zod::enumeration(vec!["bash", "powershell"]).optional().describe(
                    "Default shell for input-box ! commands. Defaults to 'bash' on all platforms (no Windows auto-flip).",
                ),
            ),
            (
                "allowManagedHooksOnly",
                zod::boolean().optional().describe(
                    "When true (and set in managed settings), only hooks from managed settings run. User, project, and local hooks are ignored.",
                ),
            ),
            (
                "allowedHttpHookUrls",
                zod::array(zod::string()).optional().describe(
                    "Allowlist of URL patterns that HTTP hooks may target. Supports * as a wildcard (e.g. \"https://hooks.example.com/*\"). When set, HTTP hooks with non-matching URLs are blocked. If undefined, all URLs are allowed. If empty array, no HTTP hooks are allowed. Arrays merge across settings sources (same semantics as allowedMcpServers).",
                ),
            ),
            (
                "httpHookAllowedEnvVars",
                zod::array(zod::string()).optional().describe(
                    "Allowlist of environment variable names HTTP hooks may interpolate into headers. When set, each hook's effective allowedEnvVars is the intersection with this list. If undefined, no restriction is applied. Arrays merge across settings sources (same semantics as allowedMcpServers).",
                ),
            ),
            (
                "allowManagedPermissionRulesOnly",
                zod::boolean().optional().describe(
                    "When true (and set in managed settings), only permission rules (allow/deny/ask) from managed settings are respected. User, project, local, and CLI argument permission rules are ignored.",
                ),
            ),
            (
                "allowManagedMcpServersOnly",
                zod::boolean().optional().describe(
                    "When true (and set in managed settings), allowedMcpServers is only read from managed settings. deniedMcpServers still merges from all sources, so users can deny servers for themselves. Users can still add their own MCP servers, but only the admin-defined allowlist applies.",
                ),
            ),
            (
                "strictPluginOnlyCustomization",
                zod::preprocess(
                    drop_unknown_customization_surfaces,
                    zod::union(vec![
                        zod::boolean(),
                        zod::array(zod::enumeration(CUSTOMIZATION_SURFACES.to_vec())),
                    ]),
                )
                .optional()
                // Non-array invalid values would fail the union and null the
                // whole managed-settings file; .catch drops the field instead.
                .catch_undefined()
                .describe(
                    "When set in managed settings, blocks non-plugin customization sources for the listed surfaces. Array form locks specific surfaces (e.g. [\"skills\", \"hooks\"]); `true` locks all four; `false` is an explicit no-op. Blocked: ~/.claude/{surface}/, .claude/{surface}/ (project), settings.json hooks, .mcp.json. NOT blocked: managed (policySettings) sources, plugin-provided customizations. Composes with strictKnownMarketplaces for end-to-end admin control — plugins gated by marketplace allowlist, everything else blocked here.",
                ),
            ),
            (
                "statusLine",
                zod::object(vec![
                    ("type", zod::literal(json!("command"))),
                    ("command", zod::string()),
                    ("padding", zod::number().optional()),
                ])
                .optional()
                .describe("Custom status line display configuration"),
            ),
            (
                "enabledPlugins",
                zod::record(zod::union(vec![
                    zod::array(zod::string()),
                    zod::boolean(),
                ]))
                .optional()
                .describe(
                    "Enabled plugins using plugin-id@marketplace-id format. Example: { \"formatter@anthropic-tools\": true }. Also supports extended format with version constraints.",
                ),
            ),
            (
                "extraKnownMarketplaces",
                zod::record(extra_known_marketplace_schema().clone())
                    .check(extra_known_marketplaces_settings_key_check)
                    .optional()
                    .describe(
                        "Additional marketplaces to make available for this repository. Typically used in repository .claude/settings.json to ensure team members have required plugin sources.",
                    ),
            ),
            (
                "strictKnownMarketplaces",
                zod::array(marketplace_source_schema().clone())
                    .optional()
                    .describe(
                        "Enterprise strict list of allowed marketplace sources. When set in managed settings, ONLY these exact sources can be added as marketplaces. The check happens BEFORE downloading, so blocked sources never touch the filesystem. Note: this is a policy gate only — it does NOT register marketplaces. To pre-register allowed marketplaces for users, also set extraKnownMarketplaces.",
                    ),
            ),
            (
                "blockedMarketplaces",
                zod::array(marketplace_source_schema().clone())
                    .optional()
                    .describe(
                        "Enterprise blocklist of marketplace sources. When set in managed settings, these exact sources are blocked from being added as marketplaces. The check happens BEFORE downloading, so blocked sources never touch the filesystem.",
                    ),
            ),
            (
                "forceLoginMethod",
                zod::enumeration(vec!["claudeai", "console"]).optional().describe(
                    "Force a specific login method: \"claudeai\" for Claude Pro/Max, \"console\" for Console billing",
                ),
            ),
            (
                "forceLoginOrgUUID",
                zod::string()
                    .optional()
                    .describe("Organization UUID to use for OAuth login"),
            ),
            (
                "otelHeadersHelper",
                zod::string()
                    .optional()
                    .describe("Path to a script that outputs OpenTelemetry headers"),
            ),
            (
                "outputStyle",
                zod::string()
                    .optional()
                    .describe("Controls the output style for assistant responses"),
            ),
            (
                "language",
                zod::string().optional().describe(
                    "Preferred language for Claude responses and voice dictation (e.g., \"japanese\", \"spanish\")",
                ),
            ),
            (
                "skipWebFetchPreflight",
                zod::boolean().optional().describe(
                    "Skip the WebFetch blocklist check for enterprise environments with restrictive security policies",
                ),
            ),
            ("sandbox", sandbox_settings_schema().clone().optional()),
            (
                "feedbackSurveyRate",
                zod::number().min(0).max(1).optional().describe(
                    "Probability (0–1) that the session quality survey appears when eligible. 0.05 is a reasonable starting point.",
                ),
            ),
            (
                "spinnerTipsEnabled",
                zod::boolean()
                    .optional()
                    .describe("Whether to show tips in the spinner"),
            ),
            (
                "spinnerVerbs",
                zod::object(vec![
                    ("mode", zod::enumeration(vec!["append", "replace"])),
                    ("verbs", zod::array(zod::string())),
                ])
                .optional()
                .describe(
                    "Customize spinner verbs. mode: \"append\" adds verbs to defaults, \"replace\" uses only your verbs.",
                ),
            ),
            (
                "spinnerTipsOverride",
                zod::object(vec![
                    ("excludeDefault", zod::boolean().optional()),
                    ("tips", zod::array(zod::string())),
                ])
                .optional()
                .describe(
                    "Override spinner tips. tips: array of tip strings. excludeDefault: if true, only show custom tips (default: false).",
                ),
            ),
            (
                "syntaxHighlightingDisabled",
                zod::boolean()
                    .optional()
                    .describe("Whether to disable syntax highlighting in diffs"),
            ),
            (
                "terminalTitleFromRename",
                zod::boolean().optional().describe(
                    "Whether /rename updates the terminal tab title (defaults to true). Set to false to keep auto-generated topic titles.",
                ),
            ),
            (
                "alwaysThinkingEnabled",
                zod::boolean().optional().describe(
                    "When false, thinking is disabled. When absent or true, thinking is enabled automatically for supported models.",
                ),
            ),
            (
                "effortLevel",
                zod::enumeration(vec!["low", "medium", "high", "xhigh", "max"])
                .optional()
                .catch_undefined()
                .describe("Persisted effort level for supported models."),
            ),
            (
                "ultracode",
                zod::boolean()
                    .optional()
                    .describe("Start this session with ultracode enabled."),
            ),
            (
                "advisorModel",
                zod::string()
                    .optional()
                    .describe("Advisor model for the server-side advisor tool."),
            ),
            (
                "fastMode",
                zod::boolean().optional().describe(
                    "When true, fast mode is enabled. When absent or false, fast mode is off.",
                ),
            ),
            (
                "fastModePerSessionOptIn",
                zod::boolean().optional().describe(
                    "When true, fast mode does not persist across sessions. Each session starts with fast mode off.",
                ),
            ),
            (
                "promptSuggestionEnabled",
                zod::boolean().optional().describe(
                    "When false, prompt suggestions are disabled. When absent or true, prompt suggestions are enabled.",
                ),
            ),
            (
                "showClearContextOnPlanAccept",
                zod::boolean().optional().describe(
                    "When true, the plan-approval dialog offers a \"clear context\" option. Defaults to false.",
                ),
            ),
            (
                "agent",
                zod::string().optional().describe(
                    "Name of an agent (built-in or custom) to use for the main thread. Applies the agent's system prompt, tool restrictions, and model.",
                ),
            ),
            (
                "companyAnnouncements",
                zod::array(zod::string()).optional().describe(
                    "Company announcements to display at startup (one will be randomly selected if multiple are provided)",
                ),
            ),
            (
                "pluginConfigs",
                zod::record(zod::object(vec![
                    (
                        "mcpServers",
                        zod::record(zod::record(zod::union(vec![
                            zod::string(),
                            zod::number(),
                            zod::boolean(),
                            zod::array(zod::string()),
                        ])))
                        .optional()
                        .describe("User configuration values for MCP servers keyed by server name"),
                    ),
                    (
                        "options",
                        zod::record(zod::union(vec![
                            zod::string(),
                            zod::number(),
                            zod::boolean(),
                            zod::array(zod::string()),
                        ]))
                        .optional()
                        .describe(
                            "Non-sensitive option values from plugin manifest userConfig, keyed by option name. Sensitive values go to secure storage instead.",
                        ),
                    ),
                ]))
                .optional()
                .describe(
                    "Per-plugin configuration including MCP server user configs, keyed by plugin ID (plugin@marketplace format)",
                ),
            ),
            (
                "remote",
                zod::object(vec![(
                    "defaultEnvironmentId",
                    zod::string()
                        .optional()
                        .describe("Default environment ID to use for remote sessions"),
                )])
                .optional()
                .describe("Remote session configuration"),
            ),
            (
                "autoUpdatesChannel",
                zod::enumeration(vec!["latest", "stable"])
                    .optional()
                    .describe("Release channel for auto-updates (latest or stable)"),
            ),
            (
                "minimumVersion",
                zod::string().optional().describe(
                    "Minimum version to stay on - prevents downgrades when switching to stable channel",
                ),
            ),
            (
                "plansDirectory",
                zod::string().optional().describe(
                    "Custom directory for plan files, relative to project root. If not set, defaults to ~/.claude/plans/",
                ),
            ),
        ]);
        if is_ant {
            shape.push((
                "classifierPermissionsEnabled",
                zod::boolean().optional().describe(
                    "Enable AI-based classification for Bash(prompt:...) permission rules",
                ),
            ));
        }
        shape.extend(vec![
            (
                "channelsEnabled",
                zod::boolean().optional().describe(
                    "Teams/Enterprise opt-in for channel notifications (MCP servers with the claude/channel capability pushing inbound messages). Default off. Set true to allow; users then select servers via --channels.",
                ),
            ),
            (
                "allowedChannelPlugins",
                zod::array(zod::object(vec![
                    ("marketplace", zod::string()),
                    ("plugin", zod::string()),
                ]))
                .optional()
                .describe(
                    "Teams/Enterprise allowlist of channel plugins. When set, replaces the default Anthropic allowlist — admins decide which plugins may push inbound messages. Undefined falls back to the default. Requires channelsEnabled: true.",
                ),
            ),
        ]);
        if kairos_or_brief {
            shape.push((
                "defaultView",
                zod::enumeration(vec!["chat", "transcript"]).optional().describe(
                    "Default transcript view: chat (SendUserMessage checkpoints only) or transcript (full)",
                ),
            ));
        }
        shape.extend(vec![
            (
                "prefersReducedMotion",
                zod::boolean().optional().describe(
                    "Reduce or disable animations for accessibility (spinner shimmer, flash effects, etc.)",
                ),
            ),
            (
                "autoMemoryEnabled",
                zod::boolean().optional().describe(
                    "Enable auto-memory for this project. When false, Claude will not read from or write to the auto-memory directory.",
                ),
            ),
            (
                "autoMemoryDirectory",
                zod::string().optional().describe(
                    "Custom directory path for auto-memory storage. Supports ~/ prefix for home directory expansion. Ignored if set in projectSettings (checked-in .claude/settings.json) for security. When unset, defaults to ~/.claude/projects/<sanitized-cwd>/memory/.",
                ),
            ),
            (
                "autoDreamEnabled",
                zod::boolean().optional().describe(
                    "Enable background memory consolidation (auto-dream). When set, overrides the server-side default.",
                ),
            ),
            (
                "showThinkingSummaries",
                zod::boolean().optional().describe(
                    "Show thinking summaries in the transcript view (ctrl+o). Default: false.",
                ),
            ),
            (
                "promptCache1h",
                zod::object(vec![(
                    "allowlist",
                    zod::array(zod::string()).optional().describe(
                        "Query-source prefixes that receive the 1h prompt-cache TTL. A trailing * is a prefix match (for example repl_main_thread*).",
                    ),
                )])
                .optional()
                .describe(
                    "1h prompt-cache TTL allowlist. Official Claude Code receives this from GrowthBook (`tengu_prompt_cache_1h_config`); Cometix reads it from settings.",
                ),
            ),
            (
                "skipDangerousModePermissionPrompt",
                zod::boolean()
                    .optional()
                    .describe("Whether the user has accepted the bypass permissions mode dialog"),
            ),
        ]);
        if transcript_classifier {
            let mut auto_mode_shape = vec![
                (
                    "allow",
                    zod::array(zod::string())
                        .optional()
                        .describe("Rules for the auto mode classifier allow section"),
                ),
                (
                    "soft_deny",
                    zod::array(zod::string())
                        .optional()
                        .describe("Rules for the auto mode classifier deny section"),
                ),
            ];
            if is_ant {
                // Back-compat alias for ant users; external users use soft_deny.
                auto_mode_shape.push(("deny", zod::array(zod::string()).optional()));
            }
            auto_mode_shape.push((
                "environment",
                zod::array(zod::string())
                    .optional()
                    .describe("Entries for the auto mode classifier environment section"),
            ));
            shape.extend(vec![
                (
                    "skipAutoPermissionPrompt",
                    zod::boolean()
                        .optional()
                        .describe("Whether the user has accepted the auto mode opt-in dialog"),
                ),
                (
                    "useAutoModeDuringPlan",
                    zod::boolean().optional().describe(
                        "Whether plan mode uses auto mode semantics when auto mode is available (default: true)",
                    ),
                ),
                (
                    "autoMode",
                    zod::object(auto_mode_shape)
                        .optional()
                        .describe("Auto mode classifier prompt customization"),
                ),
            ]);
        }
        shape.extend(vec![
            (
                "disableAutoMode",
                zod::enumeration(vec!["disable"])
                    .optional()
                    .describe("Disable auto mode"),
            ),
            (
                "sshConfigs",
                zod::array(zod::object(vec![
                    (
                        "id",
                        zod::string().describe(
                            "Unique identifier for this SSH config. Used to match configs across settings sources.",
                        ),
                    ),
                    (
                        "name",
                        zod::string().describe("Display name for the SSH connection"),
                    ),
                    (
                        "sshHost",
                        zod::string().describe(
                            "SSH host in format \"user@hostname\" or \"hostname\", or a host alias from ~/.ssh/config",
                        ),
                    ),
                    (
                        "sshPort",
                        zod::number()
                            .int()
                            .optional()
                            .describe("SSH port (default: 22)"),
                    ),
                    (
                        "sshIdentityFile",
                        zod::string()
                            .optional()
                            .describe("Path to SSH identity file (private key)"),
                    ),
                    (
                        "startDirectory",
                        zod::string().optional().describe(
                            "Default working directory on the remote host. Supports tilde expansion (e.g. ~/projects). If not specified, defaults to the remote user home directory. Can be overridden by the [dir] positional argument in `claude ssh <config> [dir]`.",
                        ),
                    ),
                ]))
                .optional()
                .describe(
                    "SSH connection configurations for remote environments. Typically set in managed settings by enterprise administrators to pre-configure SSH connections for team members.",
                ),
            ),
            (
                "claudeMdExcludes",
                zod::array(zod::string()).optional().describe(
                    "Glob patterns or absolute paths of CLAUDE.md files to exclude from loading. Patterns are matched against absolute file paths using picomatch. Only applies to User, Project, and Local memory types (Managed/policy files cannot be excluded). Examples: \"/home/user/monorepo/CLAUDE.md\", \"**/code/CLAUDE.md\", \"**/some-dir/.claude/rules/**\"",
                ),
            ),
            (
                "pluginTrustMessage",
                zod::string().optional().describe(
                    "Custom message to append to the plugin trust warning shown before installation. Only read from policy settings (managed-settings.json / MDM). Useful for enterprise administrators to add organization-specific context (e.g., \"All plugins from our internal marketplace are vetted and approved.\").",
                ),
            ),
        ]);
        zod::passthrough_object(shape)
    })
}

#[cfg(test)]
mod zod_schema_tests {
    use super::*;

    /// Maps to: CC `EnvironmentVariablesSchema` — `z.coerce.string()` values:
    /// JSON authors write numbers/booleans, the schema coerces them through
    /// ECMAScript `String()`.
    #[test]
    fn environment_variables_coerce_values_to_strings_like_official() {
        let parsed = crate::utils::zod::safe_parse(
            environment_variables_schema(),
            &serde_json::json!({
                "PORT": 8080,
                "DEBUG": true,
                "NAME": "claude",
                "RATIO": 0.5
            }),
        )
        .expect("record parses");
        assert_eq!(parsed["PORT"], "8080");
        assert_eq!(parsed["DEBUG"], "true");
        assert_eq!(parsed["NAME"], "claude");
        assert_eq!(parsed["RATIO"], "0.5");
    }

    /// Maps to: CC `AllowedMcpServerEntrySchema` / `DeniedMcpServerEntrySchema`
    /// — the refine copy and the per-field custom messages are product copy
    /// (they surface through formatZodError), verbatim from
    /// `settings/types.ts:115-206`.
    #[test]
    fn mcp_server_entry_schemas_report_official_copy_verbatim() {
        for schema in [
            allowed_mcp_server_entry_schema(),
            denied_mcp_server_entry_schema(),
        ] {
            // Exactly one matcher passes.
            assert!(
                crate::utils::zod::safe_parse(schema, &serde_json::json!({"serverName": "linear"}))
                    .is_ok()
            );

            // Zero or two matchers hit the refine, verbatim.
            let refine_message = "Entry must have exactly one of \"serverName\", \"serverCommand\", or \"serverUrl\"";
            let error = crate::utils::zod::safe_parse(schema, &serde_json::json!({}))
                .expect_err("zero matchers must fail");
            assert_eq!(error.issues[0].message, refine_message);
            let error = crate::utils::zod::safe_parse(
                schema,
                &serde_json::json!({"serverName": "a", "serverUrl": "https://x"}),
            )
            .expect_err("two matchers must fail");
            assert_eq!(error.issues[0].message, refine_message);

            // The regex custom message, verbatim.
            let error = crate::utils::zod::safe_parse(
                schema,
                &serde_json::json!({"serverName": "bad name!"}),
            )
            .expect_err("invalid name must fail");
            assert_eq!(
                error.issues[0].message,
                "Server name can only contain letters, numbers, hyphens, and underscores"
            );

            // The array min custom message, verbatim.
            let error =
                crate::utils::zod::safe_parse(schema, &serde_json::json!({"serverCommand": []}))
                    .expect_err("empty command must fail");
            assert_eq!(
                error.issues[0].message,
                "Server command must have at least one element (the command)"
            );
        }
    }

    /// Maps to: zod/v4 `.catch(undefined)` (`settings/types.ts:540,710`) — a
    /// failing parse resolves to undefined (the key vanishes), issues never
    /// surface, and the projection stays the inner schema.
    #[test]
    fn catch_undefined_swallows_failures_like_official() {
        use crate::utils::zod;
        let schema = zod::object(vec![(
            "effortLevel",
            zod::enumeration(vec!["low", "medium", "high"])
                .catch_undefined()
                .optional(),
        )]);
        let parsed = zod::safe_parse(&schema, &serde_json::json!({"effortLevel": "bogus"}))
            .expect("catch swallows the enum failure");
        assert!(parsed.get("effortLevel").is_none(), "the key vanishes");
        let parsed = zod::safe_parse(&schema, &serde_json::json!({"effortLevel": "high"}))
            .expect("valid value parses");
        assert_eq!(parsed["effortLevel"], "high");
    }

    /// Maps to: zod/v4 `.check(ctx)` (`settings/types.ts:571-596`) — the
    /// checker pushes multiple custom issues with their own paths and
    /// formatted messages.
    #[test]
    fn check_pushes_multi_issue_with_paths_like_official() {
        use crate::utils::zod;
        fn keys_must_match(value: &serde_json::Value) -> Vec<zod::CheckIssue> {
            let mut found = Vec::new();
            if let Some(map) = value.as_object() {
                for (key, entry) in map {
                    let name = entry.get("name").and_then(serde_json::Value::as_str);
                    if name != Some(key.as_str()) {
                        found.push(zod::CheckIssue {
                            path: vec![
                                crate::utils::zod::PathSegment::Key(key.clone()),
                                crate::utils::zod::PathSegment::Key("name".to_string()),
                            ],
                            message: format!(
                                "name must match its key (got key \"{key}\" but name {name:?})"
                            ),
                        });
                    }
                }
            }
            found
        }
        let schema = zod::record(zod::object(vec![("name", zod::string())])).check(keys_must_match);

        assert!(
            zod::safe_parse(
                &schema,
                &serde_json::json!({"a": {"name": "a"}, "b": {"name": "b"}})
            )
            .is_ok()
        );

        let error = zod::safe_parse(
            &schema,
            &serde_json::json!({"a": {"name": "x"}, "b": {"name": "y"}}),
        )
        .expect_err("both mismatches must report");
        assert_eq!(error.issues.len(), 2);
        assert!(error.issues.iter().all(|issue| issue.path.len() == 2));
    }

    /// The carrier projections stay transparent: catch/check never enter the
    /// JSON Schema, and coerce projects the inner string.
    #[test]
    fn new_combinators_are_invisible_in_the_projection() {
        use crate::utils::zod;
        let projected = zod::to_json_schema(environment_variables_schema());
        assert_eq!(
            projected["additionalProperties"]["type"], "string",
            "coerce projects the inner string: {projected}"
        );
    }

    /// Maps to: CC `PermissionsSchema` — rule arrays validate through
    /// `PermissionRuleSchema`, unknown keys pass through, and every field is
    /// optional.
    #[test]
    fn permissions_schema_validates_rules_and_passes_unknown_keys() {
        let schema = permissions_schema();
        let parsed = crate::utils::zod::safe_parse(
            schema,
            &serde_json::json!({
                "allow": ["Bash(npm run:*)", "Read(src/**)"],
                "defaultMode": "acceptEdits",
                "futureKey": {"kept": true},
            }),
        )
        .expect("valid permissions parse");
        assert_eq!(
            parsed["futureKey"]["kept"], true,
            "passthrough keeps unknown keys"
        );

        let error = crate::utils::zod::safe_parse(schema, &serde_json::json!({"deny": ["Bash()"]}))
            .expect_err("invalid rule fails");
        assert_eq!(error.issues[0].path.len(), 2, "path is deny[0]");
        assert!(
            error.issues[0]
                .message
                .starts_with("Empty parentheses. Either specify a pattern")
        );
        // TranscriptClassifier is on (feature_flags.rs), so CC's gated enum
        // admits auto and the disableAutoMode field exists.
        assert!(
            crate::utils::zod::safe_parse(
                schema,
                &serde_json::json!({"defaultMode": "auto", "disableAutoMode": "disable"}),
            )
            .is_ok()
        );
        // bubble stays outside the user-addressable set.
        assert!(
            crate::utils::zod::safe_parse(schema, &serde_json::json!({"defaultMode": "bubble"}),)
                .is_err()
        );
    }

    /// Maps to: CC `SettingsSchema` — the full assembly parses a realistic
    /// settings.json, keeps unknown keys (outer passthrough), and routes the
    /// nested families through their own schemas.
    #[test]
    fn settings_schema_parses_a_realistic_file_end_to_end() {
        let ok = serde_json::json!({
            "$schema": "https://json.schemastore.org/claude-code-settings.json",
            "model": "claude-opus-4-6",
            "env": {"PORT": 8080},
            "permissions": {"allow": ["Bash(npm run:*)"], "defaultMode": "plan"},
            "hooks": {"PreToolUse": [{"hooks": [{"type": "command", "command": "echo"}]}]},
            "sandbox": {"enabled": true},
            "statusLine": {"type": "command", "command": "starship"},
            "spinnerVerbs": {"mode": "append", "verbs": ["Reticulating"]},
            "someFutureKey": {"kept": true},
        });
        let parsed = crate::utils::zod::safe_parse(settings_schema(), &ok)
            .expect("realistic settings parse");
        assert_eq!(parsed["someFutureKey"]["kept"], true, "outer passthrough");
        // The env family coerced through the carrier.
        assert_eq!(parsed["env"]["PORT"], "8080");

        let prompt_cache = crate::utils::zod::safe_parse(
            settings_schema(),
            &serde_json::json!({
                "promptCache1h": {"allowlist": ["repl_main_thread*", "sdk"]}
            }),
        )
        .expect("promptCache1h is a first-class settings key");
        assert_eq!(
            prompt_cache["promptCache1h"]["allowlist"],
            serde_json::json!(["repl_main_thread*", "sdk"])
        );

        // A wrong $schema literal fails.
        assert!(
            crate::utils::zod::safe_parse(
                settings_schema(),
                &serde_json::json!({"$schema": "https://example.com/other.json"}),
            )
            .is_err()
        );
    }

    /// Maps to: CC `strictPluginOnlyCustomization` — the preprocess drops
    /// unknown surfaces (forwards-compat) and `.catch(undefined)` swallows
    /// invalid raw values instead of nulling the whole file. `effortLevel`
    /// shares the catch behavior.
    #[test]
    fn settings_schema_degrades_gated_fields_like_official() {
        let filtered = crate::utils::zod::safe_parse(
            settings_schema(),
            &serde_json::json!({"strictPluginOnlyCustomization": ["skills", "commands"]}),
        )
        .expect("unknown surface filtered, not fatal");
        assert_eq!(
            filtered["strictPluginOnlyCustomization"],
            serde_json::json!(["skills"])
        );

        let swallowed = crate::utils::zod::safe_parse(
            settings_schema(),
            &serde_json::json!({"strictPluginOnlyCustomization": "skills", "model": "opus"}),
        )
        .expect("invalid raw value swallowed by catch");
        assert!(swallowed.get("strictPluginOnlyCustomization").is_none());
        assert_eq!(swallowed["model"], "opus");

        let effort = crate::utils::zod::safe_parse(
            settings_schema(),
            &serde_json::json!({"effortLevel": "ultra"}),
        )
        .expect("invalid effort level swallowed");
        assert!(effort.get("effortLevel").is_none());
        // @cometix: persist `max` as a production effortLevel (CC 2.1.88: ant-only).
        let max = crate::utils::zod::safe_parse(
            settings_schema(),
            &serde_json::json!({"effortLevel": "max"}),
        )
        .expect("max is a production effortLevel");
        assert_eq!(max["effortLevel"], "max");
    }

    /// Maps to: CC `extraKnownMarketplaces` `.check` — a settings-sourced
    /// entry whose dict key differs from source.name reports the reconciler
    /// mismatch with the key/source/name path.
    #[test]
    fn extra_known_marketplaces_key_mismatch_reports_check_issue() {
        let mismatch = serde_json::json!({
            "extraKnownMarketplaces": {
                "team-tools": {
                    "source": {
                        "source": "settings",
                        "name": "other-name",
                        "plugins": [],
                    },
                },
            },
        });
        let error = crate::utils::zod::safe_parse(settings_schema(), &mismatch)
            .expect_err("key mismatch fails");
        let issue = &error.issues[0];
        assert_eq!(
            issue.message,
            "Settings-sourced marketplace name must match its extraKnownMarketplaces key (got key \"team-tools\" but source.name \"other-name\")"
        );
        assert_eq!(
            issue.path.len(),
            4,
            "extraKnownMarketplaces.<key>.source.name"
        );

        // A github-sourced entry with any name is benign (no name to match).
        let ok = serde_json::json!({
            "extraKnownMarketplaces": {
                "team-tools": {"source": {"source": "github", "repo": "o/r"}},
            },
        });
        assert!(crate::utils::zod::safe_parse(settings_schema(), &ok).is_ok());
    }

    /// CC settings env objects retain ECMAScript own-key order when consumed by
    /// `utils/managedEnv.ts`, while `state/onChangeAppState.ts:164` gates on the
    /// nested env object's reference identity.
    #[test]
    fn settings_env_matches_official_own_key_order_and_reference_identity() {
        let settings: SettingsJson = serde_json::from_str(
            r#"{"env":{"1\u0000tail":"malformed","2":"two","SECOND":"2","1":"one","FIRST":"1"}}"#,
        )
        .unwrap();
        let cloned = settings.clone();
        assert!(Arc::ptr_eq(
            settings.env.as_ref().unwrap(),
            cloned.env.as_ref().unwrap()
        ));
        assert_eq!(
            settings
                .env
                .as_deref()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["1", "2", "1\0tail", "SECOND", "FIRST"]
        );
        let serialized = serde_json::to_value(&settings).unwrap();
        assert_eq!(
            serialized["env"]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["1", "2", "1\0tail", "SECOND", "FIRST"]
        );

        let replacement: SettingsJson = serde_json::from_str(
            r#"{"env":{"1\u0000tail":"malformed","2":"two","SECOND":"2","1":"one","FIRST":"1"}}"#,
        )
        .unwrap();
        assert!(!Arc::ptr_eq(
            settings.env.as_ref().unwrap(),
            replacement.env.as_ref().unwrap()
        ));
    }

    /// Maps to: CC `ExtraKnownMarketplaceSchema` — a marketplace source plus
    /// the two optional bookkeeping fields; only `source` is required.
    #[test]
    fn extra_known_marketplace_requires_only_the_source() {
        let schema = extra_known_marketplace_schema();
        assert!(
            crate::utils::zod::safe_parse(
                schema,
                &serde_json::json!({"source": {"source": "github", "repo": "o/r"}}),
            )
            .is_ok()
        );
        assert!(crate::utils::zod::safe_parse(
            schema,
            &serde_json::json!({"source": {"source": "file", "path": "/tmp/m.json"}, "autoUpdate": true}),
        )
        .is_ok());
        // Missing source fails; an unknown discriminator names the tag path.
        assert!(crate::utils::zod::safe_parse(schema, &serde_json::json!({})).is_err());
        let error = crate::utils::zod::safe_parse(
            schema,
            &serde_json::json!({"source": {"source": "gopher", "path": "x"}}),
        )
        .expect_err("unknown discriminator fails");
        assert_eq!(error.issues[0].path.len(), 2, "path is source.source");
    }
}
