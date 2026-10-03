use indexmap::IndexMap;
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{LazyLock, RwLock};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::env_utils;

fn serialize_global_config_env<S>(
    env: &Option<IndexMap<String, String>>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match env {
        Some(env) => crate::utils::process_env::serialize_ecmascript_object(env.iter(), serializer),
        None => serializer.serialize_none(),
    }
}

fn deserialize_global_config_env<'de, D>(
    deserializer: D,
) -> Result<Option<IndexMap<String, String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let env = Option::<IndexMap<String, String>>::deserialize(deserializer)?;
    Ok(env.map(|env| {
        crate::utils::process_env::ecmascript_object_entries(env.iter())
            .into_iter()
            .map(|(key, value)| (key.to_string(), value.clone()))
            .collect()
    }))
}

pub const DEFAULT_MESSAGE_IDLE_NOTIF_THRESHOLD_MS: u64 = 60_000;
const MAX_CONFIG_BACKUPS: usize = 5;
const MIN_CONFIG_BACKUP_INTERVAL_MS: u128 = 60_000;
const CONFIG_LOCK_RETRY_INTERVAL_MS: u64 = 25;
const CONFIG_LOCK_TIMEOUT_MS: u64 = 10_000;
const CONFIG_FRESHNESS_POLL_MS: u64 = 5_000;

static GLOBAL_CONFIG_WRITE_COUNT: AtomicU64 = AtomicU64::new(0);
static TRUST_DIALOG_ACCEPTED_CACHE: AtomicBool = AtomicBool::new(false);
static CONFIG_READING_ALLOWED: AtomicBool = AtomicBool::new(false);
static CONFIG_CACHE_HITS: AtomicU64 = AtomicU64::new(0);
static CONFIG_CACHE_MISSES: AtomicU64 = AtomicU64::new(0);
static FRESHNESS_WATCHER_STARTED: AtomicBool = AtomicBool::new(false);

#[derive(Clone)]
struct GlobalConfigCacheEntry {
    path: PathBuf,
    config: GlobalConfig,
    mtime_ms: u128,
}

static GLOBAL_CONFIG_CACHE: LazyLock<RwLock<Option<GlobalConfigCacheEntry>>> =
    LazyLock::new(|| RwLock::new(None));
static LAST_READ_FILE_STATS: LazyLock<RwLock<Option<ConfigFileStats>>> =
    LazyLock::new(|| RwLock::new(None));

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ConfigFileStats {
    mtime_ms: u128,
    size: u64,
}

pub const GLOBAL_CONFIG_KEYS: &[&str] = &[
    "apiKeyHelper",
    "installMethod",
    "autoUpdates",
    "autoUpdatesProtectedForNative",
    "theme",
    "verbose",
    // Cometix extension keys (user-authorized L2, 2026-07-31; no CC counterpart)
    "expandThinking",
    "expandCollapsedReadSearch",
    "preferredNotifChannel",
    "shiftEnterKeyBindingInstalled",
    "editorMode",
    "hasUsedBackslashReturn",
    "autoCompactEnabled",
    "showTurnDuration",
    "diffTool",
    "env",
    "tipsHistory",
    "todoFeatureEnabled",
    "showExpandedTodos",
    "messageIdleNotifThresholdMs",
    "autoConnectIde",
    "autoInstallIdeExtension",
    "fileCheckpointingEnabled",
    "terminalProgressBarEnabled",
    "showStatusInTerminalTab",
    "taskCompleteNotifEnabled",
    "inputNeededNotifEnabled",
    "agentPushNotifEnabled",
    "respectGitignore",
    "claudeInChromeDefaultEnabled",
    "hasCompletedClaudeInChromeOnboarding",
    "lspRecommendationDisabled",
    "lspRecommendationNeverPlugins",
    "lspRecommendationIgnoredCount",
    "copyFullResponse",
    "copyOnSelect",
    "permissionExplainerEnabled",
    "prStatusFooterEnabled",
    "remoteControlAtStartup",
    "remoteDialogSeen",
];

pub const PROJECT_CONFIG_KEYS: &[&str] = &[
    "allowedTools",
    "hasTrustDialogAccepted",
    "hasCompletedProjectOnboarding",
];

pub fn is_global_config_key(key: &str) -> bool {
    GLOBAL_CONFIG_KEYS.contains(&key)
}

pub fn is_project_config_key(key: &str) -> bool {
    PROJECT_CONFIG_KEYS.contains(&key)
}

// ════════════════════════════════════════════════════════════
// ════════════════════════════════════════════════════════════

pub fn get_config_backup_dir() -> PathBuf {
    crate::utils::env_utils::get_claude_config_home_dir().join("backups")
}

pub fn normalize_project_path(path: &str) -> String {
    path.replace('\\', "/")
}

/// Maps to CC `utils/config.ts#getProjectPathForConfig`.
fn get_project_path_for_config() -> PathBuf {
    let original_cwd = crate::bootstrap::state::get_original_cwd();
    let resolved_cwd = original_cwd.canonicalize().unwrap_or(original_cwd);
    crate::utils::git::find_canonical_git_root(&resolved_cwd).unwrap_or(resolved_cwd)
}

/// Maps to CC `utils/config.ts#getMemoryPath`.
pub fn get_memory_path(memory_type: &str) -> PathBuf {
    match memory_type {
        "User" | "user" => crate::utils::env_utils::get_claude_config_home_dir().join("CLAUDE.md"),
        "Local" | "local" => crate::bootstrap::state::get_original_cwd().join("CLAUDE.local.md"),
        "Managed" | "managed" => {
            crate::utils::settings::managed_path::get_managed_file_path().join("CLAUDE.md")
        }
        "AutoMem" | "autoMem" | "automem" => crate::memdir::paths::get_auto_mem_entrypoint(),
        "Project" | "project" => crate::bootstrap::state::get_original_cwd().join("CLAUDE.md"),
        // `TeamMem` only joins `MemoryType` under CC's `feature('TEAMMEM')`
        // (`utils/memory/types.ts:9`); the provider itself is unconditional.
        "TeamMem" | "teamMem" | "teammem" => {
            crate::memdir::team_mem_paths::get_team_mem_entrypoint()
        }
        _ => crate::bootstrap::state::get_original_cwd().join("CLAUDE.md"),
    }
}

pub fn get_managed_claude_rules_dir() -> PathBuf {
    crate::utils::settings::managed_path::get_managed_file_path()
        .join(".claude")
        .join("rules")
}

pub fn get_user_claude_rules_dir() -> PathBuf {
    crate::utils::env_utils::get_claude_config_home_dir().join("rules")
}

// ════════════════════════════════════════════════════════════
// ════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct AccountInfo {
    pub account_uuid: Option<String>,
    pub email_address: Option<String>,
    pub organization_uuid: Option<String>,
    pub organization_name: Option<String>,
    pub organization_role: Option<String>,
    pub workspace_role: Option<String>,
    pub display_name: Option<String>,
    pub has_extra_usage_enabled: Option<bool>,
    pub billing_type: Option<String>,
    pub account_created_at: Option<String>,
    pub subscription_created_at: Option<String>,
}

impl Default for AccountInfo {
    fn default() -> Self {
        Self {
            account_uuid: None,
            email_address: None,
            organization_uuid: None,
            organization_name: None,
            organization_role: None,
            workspace_role: None,
            display_name: None,
            has_extra_usage_enabled: None,
            billing_type: None,
            account_created_at: None,
            subscription_created_at: None,
        }
    }
}

// ════════════════════════════════════════════════════════════
// ════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct McpServerConfig {
    /// Maps to: CC `McpServerConfig.type` transport discriminator.
    #[serde(rename = "type")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_type: Option<String>,
    /// Maps to: CC `services/mcp/types.ts:108-113#McpSdkServerConfigSchema.name`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Maps to: CC remote MCP `headers` static auth/header bag.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<HashMap<String, String>>,
    /// Maps to: CC remote MCP `headersHelper` executable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers_helper: Option<String>,
    /// Maps to: CC remote MCP `oauth` config. Kept as raw JSON until the full
    /// OAuth provider/keychain boundary is ported.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oauth: Option<serde_json::Value>,
    /// Maps to: CC internal IDE MCP `ideName`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ide_name: Option<String>,
    /// Maps to: CC `services/mcp/types.ts:69-87` IDE configuration flag.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ide_running_in_windows: Option<bool>,
    /// Maps to: CC internal IDE MCP `authToken`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_token: Option<String>,
    /// Maps to: CC claude.ai proxy MCP connector `id`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            server_type: None,
            name: None,
            command: None,
            args: None,
            env: None,
            url: None,
            headers: None,
            headers_helper: None,
            oauth: None,
            ide_name: None,
            ide_running_in_windows: None,
            auth_token: None,
            id: None,
        }
    }
}

// ════════════════════════════════════════════════════════════
// ════════════════════════════════════════════════════════════

// CC's DECLARED shape for this slot is a six-field INLINE subset
// (config.ts:126-133: originalCwd/worktreePath/worktreeName/originalBranch?/
// sessionId/hookBased? — anonymous, CC exports no named type for it), but
// the RUNTIME write is a structural assignment of the whole
// `currentWorktreeSession` (worktree.ts:772-775) — TS does not trim excess
// fields, so the on-disk value carries all eleven WorktreeSession fields.
//
// Why the former six-field `ActiveWorktreeSession` struct was removed
// rather than kept: `save_current_project_config` is read-modify-write, so
// a six-field carrier would DROP the other five fields CC wrote on every
// Rust round-trip through the project config — the trimmed mirror does not
// merely under-declare, it destroys CC's disk state. Only the full
// WorktreeSession shape round-trips losslessly. The declared subset remains
// the contract any future reader may rely on. CC has ZERO readers of this
// slot today (writes at worktree.ts:774, clears at :797 and :863 are the
// entire surface), so having no Rust consumer is itself faithful.

// ════════════════════════════════════════════════════════════
// ════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct ProjectConfig {
    pub allowed_tools: Vec<String>,

    // ── MCP ──
    pub mcp_context_uris: Vec<String>,
    /// Keep the source JSON object until the MCP owner applies its schema.
    /// Unknown fields and explicit nulls must survive config caching/saving.
    #[serde(default, deserialize_with = "deserialize_present_mcp_servers")]
    pub mcp_servers: Option<serde_json::Value>,
    pub enabled_mcpjson_servers: Option<Vec<String>>,
    pub disabled_mcpjson_servers: Option<Vec<String>>,
    pub enable_all_project_mcp_servers: Option<bool>,
    pub disabled_mcp_servers: Option<Vec<String>>,
    pub enabled_mcp_servers: Option<Vec<String>>,

    pub last_api_duration: Option<f64>,
    pub last_api_duration_without_retries: Option<f64>,
    pub last_tool_duration: Option<f64>,
    pub last_cost: Option<f64>,
    pub last_duration: Option<f64>,
    pub last_lines_added: Option<u64>,
    pub last_lines_removed: Option<u64>,
    pub last_total_input_tokens: Option<u64>,
    pub last_total_output_tokens: Option<u64>,
    pub last_total_cache_creation_input_tokens: Option<u64>,
    pub last_total_cache_read_input_tokens: Option<u64>,
    pub last_total_web_search_requests: Option<u64>,
    pub last_fps_average: Option<f64>,
    pub last_fps_low_1_pct: Option<f64>,
    pub last_session_id: Option<String>,
    pub last_model_usage: Option<HashMap<String, ModelUsageStats>>,
    pub last_session_metrics: Option<HashMap<String, f64>>,

    pub example_files: Option<Vec<String>>,
    pub example_files_generated_at: Option<u64>,

    pub has_trust_dialog_accepted: Option<bool>,
    pub has_completed_project_onboarding: Option<bool>,
    pub project_onboarding_seen_count: u32,
    pub has_claude_md_external_includes_approved: Option<bool>,
    pub has_claude_md_external_includes_warning_shown: Option<bool>,

    // ── Worktree ──
    pub active_worktree_session: Option<crate::utils::worktree::WorktreeSession>,
    pub remote_control_spawn_mode: Option<String>,

    /// Preserve Claude Code project config fields that this Rust port has not
    /// modeled yet. The TypeScript implementation keeps arbitrary object keys
    /// while filtering defaults, so dropping them during serde round-trips would
    /// be a behavioral mismatch.
    #[serde(flatten)]
    pub other_project_fields: HashMap<String, serde_json::Value>,
}

impl Default for ProjectConfig {
    fn default() -> Self {
        Self {
            allowed_tools: Vec::new(),
            mcp_context_uris: Vec::new(),
            mcp_servers: None,
            enabled_mcpjson_servers: None,
            disabled_mcpjson_servers: None,
            enable_all_project_mcp_servers: None,
            disabled_mcp_servers: None,
            enabled_mcp_servers: None,
            last_api_duration: None,
            last_api_duration_without_retries: None,
            last_tool_duration: None,
            last_cost: None,
            last_duration: None,
            last_lines_added: None,
            last_lines_removed: None,
            last_total_input_tokens: None,
            last_total_output_tokens: None,
            last_total_cache_creation_input_tokens: None,
            last_total_cache_read_input_tokens: None,
            last_total_web_search_requests: None,
            last_fps_average: None,
            last_fps_low_1_pct: None,
            last_session_id: None,
            last_model_usage: None,
            last_session_metrics: None,
            example_files: None,
            example_files_generated_at: None,
            has_trust_dialog_accepted: None,
            has_completed_project_onboarding: None,
            project_onboarding_seen_count: 0,
            has_claude_md_external_includes_approved: None,
            has_claude_md_external_includes_warning_shown: None,
            active_worktree_session: None,
            remote_control_spawn_mode: None,
            other_project_fields: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct ModelUsageStats {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub web_search_requests: u64,
    pub cost_usd: f64,
}

impl Default for ModelUsageStats {
    fn default() -> Self {
        Self {
            input_tokens: 0,
            output_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            web_search_requests: 0,
            cost_usd: 0.0,
        }
    }
}

// ════════════════════════════════════════════════════════════
// ════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct CustomApiKeyResponses {
    pub approved: Option<Vec<String>>,
    pub rejected: Option<Vec<String>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CustomApiKeyStatus {
    Approved,
    Rejected,
    New,
}

/// Maps to: CC `utils/authPortable.ts` `normalizeApiKeyForConfig`.
pub fn normalize_api_key_for_config(api_key: &str) -> String {
    api_key
        .chars()
        .rev()
        .take(20)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

/// Maps to: CC `utils/config.ts` `getCustomApiKeyStatus`.
pub fn get_custom_api_key_status(
    config: &GlobalConfig,
    truncated_api_key: &str,
) -> CustomApiKeyStatus {
    if config
        .custom_api_key_responses
        .as_ref()
        .and_then(|responses| responses.approved.as_ref())
        .is_some_and(|approved| approved.iter().any(|key| key == truncated_api_key))
    {
        return CustomApiKeyStatus::Approved;
    }
    if config
        .custom_api_key_responses
        .as_ref()
        .and_then(|responses| responses.rejected.as_ref())
        .is_some_and(|rejected| rejected.iter().any(|key| key == truncated_api_key))
    {
        return CustomApiKeyStatus::Rejected;
    }
    CustomApiKeyStatus::New
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeCodeHintsConfig {
    #[serde(default)]
    pub plugin: Vec<String>,
    pub disabled: Option<bool>,
}

/// Maps to: CC `utils/config.ts:549-551`
/// `cachedExtraUsageDisabledReason?: string | null`.
///
/// All three JavaScript states are load-bearing at
/// `utils/model/check1mAccess.ts:14-20`: an absent key is "no API response has
/// been observed yet" (conservative — treated as not enabled), an explicit
/// `null` is "a response arrived carrying no disabled-reason header", and a
/// string is the header value. `Option<String>` collapses the first two.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum CachedExtraUsageDisabledReason {
    /// JavaScript `undefined` — the key is absent from `~/.claude.json`.
    #[default]
    Uncached,
    /// JavaScript `null` — extra usage carries no disabled reason.
    Enabled,
    /// The `anthropic-ratelimit-unified-overage-disabled-reason` header value.
    Disabled(String),
}

impl CachedExtraUsageDisabledReason {
    /// The header value, or `None` for either of the two non-string states.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Disabled(reason) => Some(reason),
            Self::Uncached | Self::Enabled => None,
        }
    }

    /// Maps to: CC `services/claudeAiLimits.ts:443-444` — a processed response
    /// records `header ?? null`, so a missing header is `Enabled`, never
    /// `Uncached`.
    pub fn from_header(reason: Option<&str>) -> Self {
        match reason {
            Some(reason) => Self::Disabled(reason.to_string()),
            None => Self::Enabled,
        }
    }
}

impl Serialize for CachedExtraUsageDisabledReason {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        // `Uncached` and `Enabled` both render as JSON null here; the disk
        // pipeline separates them by presence, not by value — see
        // [`global_config_disk_value`].
        match self {
            Self::Disabled(reason) => serializer.serialize_str(reason),
            Self::Uncached | Self::Enabled => serializer.serialize_none(),
        }
    }
}

impl<'de> Deserialize<'de> for CachedExtraUsageDisabledReason {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        // Only reached when the key is present: the container's
        // `#[serde(default)]` resolves an absent key to `Uncached`.
        match serde_json::Value::deserialize(deserializer)? {
            serde_json::Value::Null => Ok(Self::Enabled),
            serde_json::Value::String(reason) => Ok(Self::Disabled(reason)),
            other => Err(serde::de::Error::custom(format!(
                "expected string or null for cachedExtraUsageDisabledReason, got {other}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct GlobalConfig {
    pub num_startups: u64,
    pub install_method: Option<String>,
    pub auto_updates: Option<bool>,
    pub auto_updates_protected_for_native: Option<bool>,
    pub doctor_shown_at_session: Option<u64>,
    pub last_release_notes_seen: Option<String>,
    pub last_onboarding_version: Option<String>,
    pub changelog_last_fetched: Option<u64>,
    pub cached_changelog: Option<String>,
    pub first_start_time: Option<String>,

    pub api_key_helper: Option<String>,
    pub oauth_account: Option<AccountInfo>,
    /// Maps to CC `utils/config.ts:549-551`
    /// `GlobalConfig.cachedExtraUsageDisabledReason`.
    pub cached_extra_usage_disabled_reason: CachedExtraUsageDisabledReason,
    pub primary_api_key: Option<String>,
    pub custom_api_keys: Option<HashMap<String, serde_json::Value>>,
    /// Maps to CC `utils/config.ts` `GlobalConfig.customApiKeyResponses`.
    pub custom_api_key_responses: Option<CustomApiKeyResponses>,
    pub has_completed_onboarding: Option<bool>,
    pub has_acknowledged_cost_threshold: Option<bool>,
    pub has_seen_undercover_auto_notice: Option<bool>,
    pub has_seen_ultraplan_terms: Option<bool>,
    pub has_reset_auto_mode_opt_in_for_default_offer: Option<bool>,
    /// Maps to CC `utils/config.ts` `GlobalConfig.hasCompletedClaudeInChromeOnboarding`.
    pub has_completed_claude_in_chrome_onboarding: Option<bool>,
    /// Maps to CC `utils/config.ts` `GlobalConfig.claudeInChromeDefaultEnabled`.
    pub claude_in_chrome_default_enabled: Option<bool>,

    pub theme: Option<String>,
    pub editor_mode: Option<String>,
    pub verbose: Option<bool>,
    /// Cometix display preference: show thinking blocks expanded (default true;
    /// authoritative owner is the factory default in
    /// `create_official_default_global_config`). Single-source expansion
    /// control at the leaf (replaces CC's verbose gate).
    /// Cometix extension (no CC counterpart) — user-authorized L2 (v3 ruling, 2026-08-01); follows the CC verbose pattern (AppStateStore.ts:91).
    pub expand_thinking: Option<bool>,
    /// Cometix display preference: expand collapsed read/search groups (default
    /// true; authoritative owner is the factory default in
    /// `create_official_default_global_config`). Single-source expansion
    /// control at the leaf (replaces CC's verbose gate).
    /// Cometix extension (no CC counterpart) — user-authorized L2 (v3 ruling, 2026-08-01); follows the CC verbose pattern (AppStateStore.ts:91).
    pub expand_collapsed_read_search: Option<bool>,
    pub preferred_notif_channel: Option<String>,
    pub custom_notify_command: Option<String>,
    pub message_idle_notif_threshold_ms: Option<u64>,

    pub auto_compact_enabled: Option<bool>,
    /// Maps to CC global config `speculationEnabled` (ant-only consumer).
    pub speculation_enabled: Option<bool>,
    pub show_turn_duration: Option<bool>,
    #[serde(
        default,
        serialize_with = "serialize_global_config_env",
        deserialize_with = "deserialize_global_config_env"
    )]
    pub env: Option<IndexMap<String, String>>,
    pub queued_command_up_hint_count: Option<u32>,
    pub diff_tool: Option<String>,
    pub file_checkpointing_enabled: Option<bool>,
    pub terminal_progress_bar_enabled: Option<bool>,
    pub spinner_tips_enabled: Option<bool>,
    pub prefers_reduced_motion: Option<bool>,
    pub thinking_enabled: Option<bool>,
    pub pr_status_footer_enabled: Option<bool>,
    pub copy_full_response: Option<bool>,
    pub copy_on_select: Option<bool>,
    pub respect_gitignore: Option<bool>,
    pub fast_mode: Option<bool>,
    /// Maps to CC `utils/config.ts` `GlobalConfig.permissionExplainerEnabled`.
    pub permission_explainer_enabled: Option<bool>,
    pub auto_connect_ide: Option<bool>,
    /// Maps to CC `utils/config.ts` `GlobalConfig.hasIdeAutoConnectDialogBeenShown`.
    pub has_ide_auto_connect_dialog_been_shown: Option<bool>,
    /// Maps to CC `utils/config.ts` `GlobalConfig.hasIdeOnboardingBeenShown`.
    pub has_ide_onboarding_been_shown: Option<HashMap<String, bool>>,
    pub ide_hint_shown_count: Option<u32>,
    pub auto_install_ide_extension: Option<bool>,
    pub todo_feature_enabled: Option<bool>,
    /// Maps to CC `utils/config.ts` `GlobalConfig.remoteDialogSeen`.
    pub remote_dialog_seen: Option<bool>,
    /// Maps to CC `utils/config.ts` `GlobalConfig.desktopUpsellSeenCount`.
    pub desktop_upsell_seen_count: Option<u32>,
    /// Maps to CC `utils/config.ts` `GlobalConfig.desktopUpsellDismissed`.
    pub desktop_upsell_dismissed: Option<bool>,

    /// Keep the source JSON object until the MCP owner applies its schema.
    /// Unknown fields and explicit nulls must survive config caching/saving.
    #[serde(default, deserialize_with = "deserialize_present_mcp_servers")]
    pub mcp_servers: Option<serde_json::Value>,
    pub claude_ai_mcp_ever_connected: Option<Vec<String>>,

    pub projects: HashMap<String, ProjectConfig>,

    #[serde(rename = "userID", alias = "userId")]
    pub user_id: Option<String>,

    #[serde(default, deserialize_with = "deserialize_tips_history")]
    pub tips_history: Option<HashMap<String, u64>>,
    pub last_tip_time: Option<u64>,
    pub btw_use_count: Option<u32>,
    pub skill_usage: Option<HashMap<String, serde_json::Value>>,
    pub feedback_survey_state: Option<serde_json::Value>,
    pub transcript_share_dismissed: Option<bool>,
    pub companion: Option<serde_json::Value>,
    pub companion_muted: Option<bool>,
    pub has_seen_tasks_hint: Option<bool>,
    pub has_used_stash: Option<bool>,
    pub has_used_background_task: Option<bool>,
    /// Maps to CC `GlobalConfig.claudeCodeHints` show-once state.
    pub claude_code_hints: Option<ClaudeCodeHintsConfig>,
    pub show_expanded_todos: Option<bool>,
    /// Maps to CC `GlobalConfig.showSpinnerTree` — persisted expandedView
    /// 'teammates' leg (see state/onChangeAppState.ts:114-128).
    pub show_spinner_tree: Option<bool>,
    pub memory_usage_count: Option<u32>,
    pub prompt_queue_use_count: Option<u32>,

    pub cached_statsig_gates: Option<HashMap<String, bool>>,
    pub cached_dynamic_configs: Option<HashMap<String, serde_json::Value>>,
    /// Maps to CC `utils/config.ts` `GlobalConfig.lastShownEmergencyTip`.
    pub last_shown_emergency_tip: Option<String>,
    /// Maps to CC `GlobalConfig.cachedGrowthBookFeatures`, populated by
    /// `services/analytics/growthbook.ts` remote-eval disk sync.
    pub cached_growth_book_features: Option<HashMap<String, serde_json::Value>>,
    /// Maps to CC `GlobalConfig.growthBookOverrides`, ant-only local overrides
    /// read by `getFeatureValue_CACHED_MAY_BE_STALE(...)` after env overrides.
    pub growth_book_overrides: Option<HashMap<String, serde_json::Value>>,
    /// Maps to CC `utils/config.ts:563-564`
    /// `GlobalConfig.additionalModelOptionsCache` — model picker entries
    /// fetched during bootstrap (`services/api/bootstrap.ts:136`) and appended
    /// by `utils/model/modelOptions.ts:480`. Absent until that fetch lands;
    /// each fetch overwrites the whole list rather than invalidating entries.
    pub additional_model_options_cache:
        Option<Vec<crate::utils::model::model_options::ModelOption>>,

    pub s1m_access_cache: Option<serde_json::Value>,
    pub s1m_non_subscriber_access_cache: Option<serde_json::Value>,
    pub passes_eligibility_cache: Option<serde_json::Value>,
    /// Maps to CC `utils/config.ts` `GlobalConfig.passesUpsellSeenCount`.
    pub passes_upsell_seen_count: Option<u32>,
    /// Maps to CC `utils/config.ts` `GlobalConfig.hasVisitedPasses`.
    pub has_visited_passes: Option<bool>,
    /// Maps to CC `utils/config.ts` `GlobalConfig.passesLastSeenRemaining`.
    pub passes_last_seen_remaining: Option<u32>,
    pub overage_credit_grant_cache: Option<serde_json::Value>,
    /// Maps to CC `utils/config.ts` `GlobalConfig.overageCreditUpsellSeenCount`.
    pub overage_credit_upsell_seen_count: Option<u32>,
    /// Maps to CC `utils/config.ts` `GlobalConfig.hasVisitedExtraUsage`.
    pub has_visited_extra_usage: Option<bool>,
    /// Maps to CC `utils/config.ts` `GlobalConfig.voiceNoticeSeenCount`.
    pub voice_notice_seen_count: Option<u32>,
    /// Maps to CC `utils/config.ts` `GlobalConfig.opus1mMergeNoticeSeenCount`.
    pub opus_1m_merge_notice_seen_count: Option<u32>,

    pub iterm2_setup_in_progress: Option<bool>,
    pub iterm2_backup_path: Option<String>,
    pub apple_terminal_backup_path: Option<String>,
    pub apple_terminal_setup_in_progress: Option<bool>,
    /// Maps to CC `utils/config.ts` `GlobalConfig.iterm2It2SetupComplete`.
    pub iterm2_it2_setup_complete: Option<bool>,
    /// Maps to CC `utils/config.ts` `GlobalConfig.preferTmuxOverIterm2`.
    pub prefer_tmux_over_iterm2: Option<bool>,
    /// Maps to CC `utils/config.ts` `GlobalConfig.teammateMode`.
    pub teammate_mode: Option<String>,
    /// Maps to CC `utils/config.ts` `GlobalConfig.teammateDefaultModel`.
    /// Missing = hardcoded Opus fallback; JSON null = follow leader model;
    /// string = parse as user-specified model.
    #[serde(default, deserialize_with = "deserialize_nullable_string_option")]
    pub teammate_default_model: Option<Option<String>>,
    pub shift_enter_key_binding_installed: Option<bool>,
    /// Maps to CC `utils/config.ts` `GlobalConfig.hasUsedBackslashReturn`.
    pub has_used_backslash_return: Option<bool>,
    /// Maps to CC `utils/config.ts` `GlobalConfig.optionAsMetaKeyInstalled`.
    pub option_as_meta_key_installed: Option<bool>,

    // ── RC (Remote Control) ──
    pub remote_control: Option<serde_json::Value>,
    pub remote_control_at_startup: Option<bool>,

    /// Preserve Claude Code global config fields that are not yet strongly
    /// modeled by Cometix. This mirrors the TS config object semantics and
    /// prevents future/unknown keys from being lost when saving.
    #[serde(flatten)]
    pub other_global_fields: HashMap<String, serde_json::Value>,
}

// Representation adapter for the optional Global/Project `mcpServers` slot:
// missing stays None; a present JSON null stays Some(Null), as in the JS object.
fn deserialize_present_mcp_servers<'de, D>(
    deserializer: D,
) -> Result<Option<serde_json::Value>, D::Error>
where
    D: Deserializer<'de>,
{
    serde_json::Value::deserialize(deserializer).map(Some)
}

fn deserialize_nullable_string_option<'de, D>(
    deserializer: D,
) -> Result<Option<Option<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Null => Ok(Some(None)),
        serde_json::Value::String(value) => Ok(Some(Some(value))),
        other => Err(serde::de::Error::custom(format!(
            "expected string or null for teammateDefaultModel, got {other}"
        ))),
    }
}

fn deserialize_tips_history<'de, D>(
    deserializer: D,
) -> Result<Option<HashMap<String, u64>>, D::Error>
where
    D: Deserializer<'de>,
{
    let Some(value) = Option::<serde_json::Value>::deserialize(deserializer)? else {
        return Ok(None);
    };
    let Some(object) = value.as_object() else {
        return Ok(None);
    };

    let history = object
        .iter()
        .filter_map(|(key, value)| value.as_u64().map(|session| (key.clone(), session)))
        .collect::<HashMap<_, _>>();
    Ok((!history.is_empty()).then_some(history))
}

impl GlobalConfig {
    pub fn message_idle_notif_threshold_ms(&self) -> u64 {
        self.message_idle_notif_threshold_ms
            .unwrap_or(DEFAULT_MESSAGE_IDLE_NOTIF_THRESHOLD_MS)
    }
}

impl Default for GlobalConfig {
    fn default() -> Self {
        Self {
            num_startups: 0,
            install_method: None,
            auto_updates: None,
            auto_updates_protected_for_native: None,
            doctor_shown_at_session: None,
            last_release_notes_seen: None,
            last_onboarding_version: None,
            changelog_last_fetched: None,
            cached_changelog: None,
            first_start_time: None,
            api_key_helper: None,
            oauth_account: None,
            cached_extra_usage_disabled_reason: CachedExtraUsageDisabledReason::Uncached,
            primary_api_key: None,
            custom_api_keys: None,
            custom_api_key_responses: None,
            has_completed_onboarding: None,
            has_acknowledged_cost_threshold: None,
            has_seen_undercover_auto_notice: None,
            has_seen_ultraplan_terms: None,
            has_reset_auto_mode_opt_in_for_default_offer: None,
            has_completed_claude_in_chrome_onboarding: None,
            claude_in_chrome_default_enabled: None,
            theme: None,
            editor_mode: None,
            verbose: None,
            expand_thinking: None,
            expand_collapsed_read_search: None,
            preferred_notif_channel: None,
            custom_notify_command: None,
            message_idle_notif_threshold_ms: None,
            auto_compact_enabled: None,
            speculation_enabled: None,
            show_turn_duration: None,
            env: None,
            queued_command_up_hint_count: None,
            diff_tool: None,
            file_checkpointing_enabled: None,
            terminal_progress_bar_enabled: None,
            spinner_tips_enabled: None,
            prefers_reduced_motion: None,
            thinking_enabled: None,
            pr_status_footer_enabled: None,
            copy_full_response: None,
            copy_on_select: None,
            respect_gitignore: None,
            fast_mode: None,
            permission_explainer_enabled: None,
            auto_connect_ide: None,
            has_ide_auto_connect_dialog_been_shown: None,
            has_ide_onboarding_been_shown: None,
            ide_hint_shown_count: None,
            auto_install_ide_extension: None,
            todo_feature_enabled: None,
            remote_dialog_seen: None,
            desktop_upsell_seen_count: None,
            desktop_upsell_dismissed: None,
            mcp_servers: None,
            claude_ai_mcp_ever_connected: None,
            projects: HashMap::new(),
            user_id: None,
            tips_history: None,
            last_tip_time: None,
            btw_use_count: None,
            skill_usage: None,
            feedback_survey_state: None,
            transcript_share_dismissed: None,
            companion: None,
            companion_muted: None,
            has_seen_tasks_hint: None,
            has_used_stash: None,
            has_used_background_task: None,
            claude_code_hints: None,
            show_expanded_todos: None,
            show_spinner_tree: None,
            memory_usage_count: None,
            prompt_queue_use_count: None,
            cached_statsig_gates: None,
            cached_dynamic_configs: None,
            last_shown_emergency_tip: None,
            cached_growth_book_features: None,
            growth_book_overrides: None,
            additional_model_options_cache: None,
            s1m_access_cache: None,
            s1m_non_subscriber_access_cache: None,
            passes_eligibility_cache: None,
            passes_upsell_seen_count: None,
            has_visited_passes: None,
            passes_last_seen_remaining: None,
            overage_credit_grant_cache: None,
            overage_credit_upsell_seen_count: None,
            has_visited_extra_usage: None,
            voice_notice_seen_count: None,
            opus_1m_merge_notice_seen_count: None,
            iterm2_setup_in_progress: None,
            iterm2_backup_path: None,
            apple_terminal_backup_path: None,
            apple_terminal_setup_in_progress: None,
            iterm2_it2_setup_complete: None,
            prefer_tmux_over_iterm2: None,
            teammate_mode: None,
            teammate_default_model: None,
            shift_enter_key_binding_installed: None,
            has_used_backslash_return: None,
            option_as_meta_key_installed: None,
            remote_control: None,
            remote_control_at_startup: None,
            other_global_fields: HashMap::new(),
        }
    }
}

// ════════════════════════════════════════════════════════════
// ════════════════════════════════════════════════════════════

fn create_official_default_global_config() -> GlobalConfig {
    let mut config = GlobalConfig::default();

    config.theme = Some("dark".to_string());
    config.preferred_notif_channel = Some("auto".to_string());
    config.verbose = Some(false);
    config.expand_thinking = Some(true);
    config.expand_collapsed_read_search = Some(true);
    config.editor_mode = Some("normal".to_string());
    config.auto_compact_enabled = Some(true);
    config.show_turn_duration = Some(true);
    config.has_seen_tasks_hint = Some(false);
    config.has_used_stash = Some(false);
    config.has_used_background_task = Some(false);
    config.custom_api_key_responses = Some(CustomApiKeyResponses {
        approved: Some(Vec::new()),
        rejected: Some(Vec::new()),
    });
    config.tips_history = Some(HashMap::new());
    config.btw_use_count = Some(0);
    config.todo_feature_enabled = Some(true);
    config.show_expanded_todos = Some(false);
    config.message_idle_notif_threshold_ms = Some(DEFAULT_MESSAGE_IDLE_NOTIF_THRESHOLD_MS);
    config.auto_connect_ide = Some(false);
    config.auto_install_ide_extension = Some(true);
    config.file_checkpointing_enabled = Some(true);
    config.terminal_progress_bar_enabled = Some(true);
    config.cached_statsig_gates = Some(HashMap::new());
    config.cached_dynamic_configs = Some(HashMap::new());
    config.cached_growth_book_features = Some(HashMap::new());
    config.copy_full_response = Some(false);

    config
}

fn create_official_default_global_config_value() -> anyhow::Result<serde_json::Value> {
    let mut value = serde_json::to_value(create_official_default_global_config())?;
    let Some(object) = value.as_object_mut() else {
        return Ok(value);
    };

    // These fields are present in the TypeScript GlobalConfig default factory,
    // but are not yet strongly modeled by this Rust port. Keeping them in the
    // JSON-level default map lets save filtering match Claude Code without
    // forcing every Rust callsite that constructs GlobalConfig to initialize a
    // large set of fields it does not use.
    object.insert("queuedCommandUpHintCount".to_string(), serde_json::json!(0));
    object.insert("diffTool".to_string(), serde_json::json!("auto"));
    object.insert("env".to_string(), serde_json::json!({}));
    object.insert("memoryUsageCount".to_string(), serde_json::json!(0));
    object.insert("promptQueueUseCount".to_string(), serde_json::json!(0));
    object.insert("respectGitignore".to_string(), serde_json::json!(true));

    Ok(value)
}

fn remove_null_object_fields_recursively(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(object) => {
            object.retain(|_, nested_value| {
                remove_null_object_fields_recursively(nested_value);
                !nested_value.is_null()
            });
        }
        serde_json::Value::Array(array) => {
            for nested_value in array {
                remove_null_object_fields_recursively(nested_value);
            }
        }
        _ => {}
    }
}

/// Keys whose JSON `null` is a distinct persisted state rather than "unset",
/// so neither the disk merge nor the save filter may collapse it into absence.
const NULL_BEARING_GLOBAL_CONFIG_KEYS: &[&str] = &[
    "teammateDefaultModel",
    "cachedExtraUsageDisabledReason",
    "mcpServers",
];

fn merge_disk_config_with_official_defaults(
    mut disk_value: serde_json::Value,
) -> anyhow::Result<GlobalConfig> {
    migrate_config_fields_value(&mut disk_value);
    let mut merged_value = create_official_default_global_config_value()?;
    remove_null_object_fields_recursively(&mut merged_value);

    if let (Some(default_object), Some(disk_object)) =
        (merged_value.as_object_mut(), disk_value.as_object())
    {
        for (key, value) in disk_object {
            if value.is_null() && !NULL_BEARING_GLOBAL_CONFIG_KEYS.contains(&key.as_str()) {
                continue;
            }
            default_object.insert(key.clone(), value.clone());
        }
    } else {
        merged_value = disk_value;
    }

    Ok(serde_json::from_value(merged_value)?)
}

fn migrate_config_fields_value(value: &mut serde_json::Value) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    if object.contains_key("installMethod") {
        return;
    }

    let legacy_status = object
        .get("autoUpdaterStatus")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let Some(legacy_status) = legacy_status else {
        return;
    };

    let mut install_method = "unknown";
    let mut auto_updates = object
        .get("autoUpdates")
        .and_then(|value| value.as_bool())
        .unwrap_or(true);

    match legacy_status.as_str() {
        "migrated" => install_method = "local",
        "installed" => install_method = "native",
        "disabled" => auto_updates = false,
        "enabled" | "no_permissions" | "not_configured" => install_method = "global",
        _ => {}
    }

    object.insert(
        "installMethod".to_string(),
        serde_json::Value::String(install_method.to_string()),
    );
    object.insert(
        "autoUpdates".to_string(),
        serde_json::Value::Bool(auto_updates),
    );
}

fn remove_project_history_from_value(value: &mut serde_json::Value) {
    let Some(projects) = value
        .get_mut("projects")
        .and_then(|value| value.as_object_mut())
    else {
        return;
    };
    for project in projects.values_mut() {
        if let Some(project_object) = project.as_object_mut() {
            project_object.remove("history");
        }
    }
}

fn global_config_disk_value(config: &GlobalConfig) -> anyhow::Result<serde_json::Value> {
    let mut config_value = serde_json::to_value(config)?;
    let mut default_value = create_official_default_global_config_value()?;

    remove_null_object_fields_recursively(&mut config_value);
    remove_null_object_fields_recursively(&mut default_value);

    // These two source-owned slots are already raw JSON, not typed Option
    // fields. Restore their current values after the legacy typed-field
    // cleanup, at the exact Global/Project paths only. This reads the live
    // config being saved, so changes/removals cannot be overridden by old raw
    // disk data or a second mirror of the same configuration.
    if let Some(mcp_servers) = &config.mcp_servers {
        config_value["mcpServers"] = mcp_servers.clone();
    }
    if let Some(projects) = config_value
        .get_mut("projects")
        .and_then(serde_json::Value::as_object_mut)
    {
        for (name, project) in &config.projects {
            if let Some(mcp_servers) = &project.mcp_servers
                && let Some(value) = projects.get_mut(name)
            {
                value["mcpServers"] = mcp_servers.clone();
            }
        }
    }

    if let (Some(config_object), Some(default_object)) =
        (config_value.as_object_mut(), default_value.as_object())
    {
        config_object.retain(|key, value| default_object.get(key) != Some(value));

        // `teammateDefaultModel` intentionally uses JSON null as a distinct
        // persisted state. Missing = hardcoded default; null = follow leader.
        if config.teammate_default_model == Some(None) {
            config_object.insert("teammateDefaultModel".to_string(), serde_json::Value::Null);
        }

        // Likewise for `cachedExtraUsageDisabledReason`: missing = no cache
        // yet; null = a response carried no disabled reason.
        if config.cached_extra_usage_disabled_reason == CachedExtraUsageDisabledReason::Enabled {
            config_object.insert(
                "cachedExtraUsageDisabledReason".to_string(),
                serde_json::Value::Null,
            );
        }
    }

    Ok(config_value)
}

fn known_global_config_keys() -> anyhow::Result<Vec<String>> {
    let mut known_value = serde_json::to_value(GlobalConfig::default())?;
    let mut default_value = create_official_default_global_config_value()?;
    remove_null_object_fields_recursively(&mut default_value);

    let mut keys = Vec::new();
    if let Some(object) = known_value.as_object() {
        keys.extend(object.keys().cloned());
    }
    if let Some(object) = default_value.as_object() {
        for key in object.keys() {
            if !keys.contains(key) {
                keys.push(key.clone());
            }
        }
    }
    Ok(keys)
}

fn known_project_config_keys() -> anyhow::Result<Vec<String>> {
    let value = serde_json::to_value(ProjectConfig::default())?;
    Ok(value
        .as_object()
        .map(|object| object.keys().cloned().collect())
        .unwrap_or_default())
}

fn preserve_unknown_project_fields(
    output_value: &mut serde_json::Value,
    original_projects: Option<serde_json::Value>,
) -> anyhow::Result<()> {
    let Some(original_projects) = original_projects.and_then(|value| value.as_object().cloned())
    else {
        return Ok(());
    };
    let Some(output_projects) = output_value
        .get_mut("projects")
        .and_then(|value| value.as_object_mut())
    else {
        return Ok(());
    };

    let known_project_keys = known_project_config_keys()?;
    for (project_key, output_project_value) in output_projects.iter_mut() {
        let Some(original_project_object) = original_projects
            .get(project_key)
            .and_then(|value| value.as_object())
        else {
            continue;
        };
        let Some(output_project_object) = output_project_value.as_object_mut() else {
            continue;
        };

        for (field_key, field_value) in original_project_object {
            if !known_project_keys.contains(field_key) && field_key != "history" {
                output_project_object
                    .entry(field_key.clone())
                    .or_insert_with(|| field_value.clone());
            }
        }
    }

    Ok(())
}

fn filtered_config_value_preserving_unknown_fields(
    original_disk_value: serde_json::Value,
    config: &GlobalConfig,
) -> anyhow::Result<serde_json::Value> {
    let original_projects = original_disk_value.get("projects").cloned();
    let mut output_value = match original_disk_value {
        serde_json::Value::Object(object) => serde_json::Value::Object(object),
        _ => serde_json::json!({}),
    };

    if let Some(output_object) = output_value.as_object_mut() {
        for key in known_global_config_keys()? {
            output_object.remove(&key);
        }

        if let Some(filtered_object) = global_config_disk_value(config)?.as_object() {
            for (key, value) in filtered_object {
                output_object.insert(key.clone(), value.clone());
            }
        }
    }

    preserve_unknown_project_fields(&mut output_value, original_projects)?;
    remove_project_history_from_value(&mut output_value);

    Ok(output_value)
}

fn current_time_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

fn system_time_to_millis(time: SystemTime) -> u128 {
    time.duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

fn read_config_file_stats(path: &Path) -> Option<ConfigFileStats> {
    let metadata = fs::metadata(path).ok()?;
    Some(ConfigFileStats {
        mtime_ms: metadata
            .modified()
            .ok()
            .map(system_time_to_millis)
            .unwrap_or(0),
        size: metadata.len(),
    })
}

fn remember_last_read_file_stats(stats: Option<ConfigFileStats>) {
    if let Ok(mut slot) = LAST_READ_FILE_STATS.write() {
        *slot = stats;
    }
}

fn read_cached_global_config(path: &Path, stats: Option<ConfigFileStats>) -> Option<GlobalConfig> {
    let cache = GLOBAL_CONFIG_CACHE.read().ok()?;
    let cache = cache.as_ref()?;
    if cache.path != path {
        return None;
    }
    if let Some(stats) = stats {
        if stats.mtime_ms > cache.mtime_ms {
            return None;
        }
    }
    CONFIG_CACHE_HITS.fetch_add(1, Ordering::Relaxed);
    Some(cache.config.clone())
}

fn write_through_global_config_cache(path: &Path, config: GlobalConfig) {
    if let Ok(mut cache) = GLOBAL_CONFIG_CACHE.write() {
        *cache = Some(GlobalConfigCacheEntry {
            path: path.to_path_buf(),
            config,
            // Match Claude Code's overshoot strategy: our in-memory snapshot is
            // authoritative immediately after a successful write.
            mtime_ms: current_time_millis(),
        });
    }
    remember_last_read_file_stats(None);
}

fn cache_global_config(path: PathBuf, config: GlobalConfig, stats: Option<ConfigFileStats>) {
    if let Ok(mut cache) = GLOBAL_CONFIG_CACHE.write() {
        *cache = Some(GlobalConfigCacheEntry {
            path,
            config,
            mtime_ms: stats
                .map(|file_stats| file_stats.mtime_ms)
                .unwrap_or_else(current_time_millis),
        });
    }
    remember_last_read_file_stats(stats);
}

fn cached_global_config_mtime_ms(path: &Path) -> Option<u128> {
    let cache = GLOBAL_CONFIG_CACHE.read().ok()?;
    let cache = cache.as_ref()?;
    (cache.path == path).then_some(cache.mtime_ms)
}

fn start_global_config_freshness_watcher() {
    if cfg!(test) {
        return;
    }
    if FRESHNESS_WATCHER_STARTED.swap(true, Ordering::Relaxed) {
        return;
    }

    let path = crate::utils::env::get_global_claude_file();
    // The launchers' first config load starts this thread before
    // `process_env::publish_startup_environment` writes the real environment,
    // so it must never reach libc's getenv (no `node_os::homedir`, local time
    // or DNS here).
    let _ = thread::Builder::new()
        .name("cometix-global-config-freshness".to_string())
        .spawn(move || {
            loop {
                thread::sleep(Duration::from_millis(CONFIG_FRESHNESS_POLL_MS));

                let Some(stats) = read_config_file_stats(&path) else {
                    continue;
                };
                if cached_global_config_mtime_ms(&path)
                    .is_some_and(|mtime_ms| stats.mtime_ms <= mtime_ms)
                {
                    continue;
                }

                let Ok(config) =
                    read_global_config_value(&path).and_then(load_global_config_from_value)
                else {
                    continue;
                };
                if cached_global_config_mtime_ms(&path)
                    .is_some_and(|mtime_ms| stats.mtime_ms <= mtime_ms)
                {
                    continue;
                }

                cache_global_config(path.clone(), config, Some(stats));
            }
        });
}

pub fn get_config_cache_stats() -> (u64, u64) {
    (
        CONFIG_CACHE_HITS.load(Ordering::Relaxed),
        CONFIG_CACHE_MISSES.load(Ordering::Relaxed),
    )
}

pub fn report_config_cache_stats() -> (u64, u64) {
    let hits = CONFIG_CACHE_HITS.swap(0, Ordering::Relaxed);
    let misses = CONFIG_CACHE_MISSES.swap(0, Ordering::Relaxed);
    (hits, misses)
}

fn read_global_config_value(path: &Path) -> anyhow::Result<serde_json::Value> {
    let content = fs::read_to_string(path)?;
    let content = content.strip_prefix('\u{feff}').unwrap_or(&content);
    Ok(serde_json::from_str(content)?)
}

fn load_global_config_from_value(value: serde_json::Value) -> anyhow::Result<GlobalConfig> {
    merge_disk_config_with_official_defaults(value)
}

fn backup_corrupted_config(path: &Path, content: &str) {
    let backup_dir = get_config_backup_dir();
    if fs::create_dir_all(&backup_dir).is_err() {
        return;
    }
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(".claude.json");
    let backup_path = backup_dir.join(format!("{file_name}.corrupted.{}", current_time_millis()));
    let _ = fs::write(backup_path, content);
}

fn read_global_config_value_or_default(path: &Path) -> serde_json::Value {
    match fs::read_to_string(path) {
        Ok(content) => {
            let content_without_bom = content.strip_prefix('\u{feff}').unwrap_or(&content);
            match serde_json::from_str::<serde_json::Value>(content_without_bom) {
                Ok(value) => value,
                Err(error) => {
                    tracing::warn!("Failed to parse {}: {}", path.display(), error);
                    backup_corrupted_config(path, &content);
                    serde_json::json!({})
                }
            }
        }
        Err(_) => serde_json::json!({}),
    }
}

fn create_config_backup(path: &Path) {
    if !path.exists() {
        return;
    }

    let backup_dir = get_config_backup_dir();
    if let Err(error) = fs::create_dir_all(&backup_dir) {
        tracing::warn!(
            "Failed to create config backup dir {}: {}",
            backup_dir.display(),
            error
        );
        return;
    }

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(".claude.json");
    let backup_prefix = format!("{file_name}.backup.");
    let now = current_time_millis();

    let mut backups = match fs::read_dir(&backup_dir) {
        Ok(entries) => entries
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().to_string();
                name.starts_with(&backup_prefix)
                    .then_some((name, entry.path()))
            })
            .collect::<Vec<_>>(),
        Err(_) => Vec::new(),
    };

    backups.sort_by(|left, right| right.0.cmp(&left.0));
    let should_create_backup = backups
        .first()
        .and_then(|(name, _)| name.rsplit('.').next())
        .and_then(|timestamp| timestamp.parse::<u128>().ok())
        .map(|timestamp| now.saturating_sub(timestamp) >= MIN_CONFIG_BACKUP_INTERVAL_MS)
        .unwrap_or(true);

    if should_create_backup {
        let backup_path = backup_dir.join(format!("{backup_prefix}{now}"));
        if let Err(error) = fs::copy(path, backup_path) {
            tracing::warn!("Failed to backup config {}: {}", path.display(), error);
        }
    }

    let mut cleanup_backups = match fs::read_dir(&backup_dir) {
        Ok(entries) => entries
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().to_string();
                name.starts_with(&backup_prefix)
                    .then_some((name, entry.path()))
            })
            .collect::<Vec<_>>(),
        Err(_) => Vec::new(),
    };
    cleanup_backups.sort_by(|left, right| right.0.cmp(&left.0));

    for (_, old_backup_path) in cleanup_backups.into_iter().skip(MAX_CONFIG_BACKUPS) {
        let _ = fs::remove_file(old_backup_path);
    }
}

struct ConfigLockGuard {
    path: PathBuf,
}

impl Drop for ConfigLockGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn acquire_config_lock(path: &Path) -> anyhow::Result<ConfigLockGuard> {
    let lock_path = PathBuf::from(format!("{}.lock", path.display()));
    let start = current_time_millis();

    loop {
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
        {
            Ok(mut file) => {
                let _ = writeln!(
                    file,
                    "pid={} created_at={}",
                    std::process::id(),
                    current_time_millis()
                );
                return Ok(ConfigLockGuard { path: lock_path });
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if current_time_millis().saturating_sub(start) > CONFIG_LOCK_TIMEOUT_MS as u128 {
                    anyhow::bail!("Timed out waiting for config lock {}", lock_path.display());
                }
                thread::sleep(Duration::from_millis(CONFIG_LOCK_RETRY_INTERVAL_MS));
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn write_config_value(path: &Path, value: &serde_json::Value) -> anyhow::Result<()> {
    let json = serde_json::to_string_pretty(value)?;
    fs::write(path, json)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }

    GLOBAL_CONFIG_WRITE_COUNT.fetch_add(1, Ordering::Relaxed);
    Ok(())
}

fn would_lose_auth_state(cached: &GlobalConfig, fresh: &GlobalConfig) -> bool {
    let lost_oauth = cached.oauth_account.is_some() && fresh.oauth_account.is_none();
    let lost_onboarding = cached.has_completed_onboarding == Some(true)
        && fresh.has_completed_onboarding != Some(true);
    lost_oauth || lost_onboarding
}

/// Maps to: CC `config.ts` `TEST_GLOBAL_CONFIG_FOR_TESTING` — test-only
/// override returned by `getGlobalConfig()` under NODE_ENV=test so component
/// harnesses can inject a custom global config without touching disk.
/// Thread-local because Rust tests run in parallel.
#[cfg(test)]
thread_local! {
    static TEST_GLOBAL_CONFIG: std::cell::RefCell<Option<GlobalConfig>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub fn set_test_global_config(config: Option<GlobalConfig>) {
    TEST_GLOBAL_CONFIG.with(|cell| *cell.borrow_mut() = config);
}

/// Atomically replaces the thread-local test override and returns its previous
/// value so global-config tests can restore nested state with RAII.
#[cfg(test)]
pub fn replace_test_global_config(config: Option<GlobalConfig>) -> Option<GlobalConfig> {
    TEST_GLOBAL_CONFIG.with(|cell| std::mem::replace(&mut *cell.borrow_mut(), config))
}

pub fn load_global_config() -> GlobalConfig {
    #[cfg(test)]
    {
        if let Some(config) = TEST_GLOBAL_CONFIG.with(|cell| cell.borrow().clone()) {
            return config;
        }
    }
    let path = crate::utils::env::get_global_claude_file();

    // Claude Code keeps the normal read path as a pure memory hit after the
    // startup load. A background freshness watcher refreshes external writes.
    // In tests, keep the stat guard so individual cases can mutate temp config
    // files without coordinating with a background thread.
    if !cfg!(test) {
        if let Some(config) = read_cached_global_config(&path, None) {
            return config;
        }
    }

    let stats = read_config_file_stats(&path);
    if let Some(config) = read_cached_global_config(&path, stats) {
        return config;
    }

    CONFIG_CACHE_MISSES.fetch_add(1, Ordering::Relaxed);
    match read_global_config_value(&path).and_then(load_global_config_from_value) {
        Ok(config) => {
            cache_global_config(path, config.clone(), stats);
            start_global_config_freshness_watcher();
            config
        }
        Err(error) => {
            if path.exists() {
                tracing::warn!("Failed to parse {}: {}", path.display(), error);
                if let Ok(content) = fs::read_to_string(&path) {
                    backup_corrupted_config(&path, &content);
                }
            }
            let config = create_official_default_global_config();
            cache_global_config(path, config.clone(), stats);
            start_global_config_freshness_watcher();
            config
        }
    }
}

pub fn enable_configs() -> anyhow::Result<()> {
    let path = crate::utils::env::get_global_claude_file();
    if path.exists() {
        let config = read_global_config_value(&path).and_then(load_global_config_from_value)?;
        write_through_global_config_cache(&path, config);
    }
    CONFIG_READING_ALLOWED.store(true, Ordering::Relaxed);
    Ok(())
}

pub fn configs_are_enabled() -> bool {
    CONFIG_READING_ALLOWED.load(Ordering::Relaxed)
}

pub fn clear_global_config_cache_for_testing() {
    if let Ok(mut cache) = GLOBAL_CONFIG_CACHE.write() {
        *cache = None;
    }
    remember_last_read_file_stats(None);
    CONFIG_CACHE_HITS.store(0, Ordering::Relaxed);
    CONFIG_CACHE_MISSES.store(0, Ordering::Relaxed);
}

pub fn save_global_config(updater: impl FnOnce(&mut GlobalConfig)) -> anyhow::Result<()> {
    if !is_write_enabled() {
        tracing::info!(
            "[DRY_RUN] save_global_config: writes are disabled (set COMETIX_WRITE_ENABLED=1 to enable)"
        );
        return Ok(());
    }

    let path = crate::utils::env::get_global_claude_file();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let cached_config = load_global_config();
    let _lock = acquire_config_lock(&path)?;
    let original_disk_value = read_global_config_value_or_default(&path);
    let mut current_config = load_global_config_from_value(original_disk_value.clone())
        .unwrap_or_else(|_| create_official_default_global_config());

    if would_lose_auth_state(&cached_config, &current_config) {
        tracing::warn!(
            "Refusing to write {} because the re-read config lost auth/onboarding state",
            path.display()
        );
        return Ok(());
    }

    updater(&mut current_config);
    let output_value = filtered_config_value_preserving_unknown_fields(
        original_disk_value.clone(),
        &current_config,
    )?;
    if output_value == original_disk_value {
        return Ok(());
    }

    create_config_backup(&path);
    write_config_value(&path, &output_value)?;
    write_through_global_config_cache(&path, current_config);
    Ok(())
}

pub fn get_current_project_config() -> ProjectConfig {
    let config = load_global_config();
    let project_path = get_project_path_for_config();
    let key = normalize_project_path(&project_path.to_string_lossy());
    config.projects.get(&key).cloned().unwrap_or_default()
}

pub fn save_current_project_config(updater: impl FnOnce(&mut ProjectConfig)) -> anyhow::Result<()> {
    let project_path = get_project_path_for_config();
    let key = normalize_project_path(&project_path.to_string_lossy());

    save_global_config(|config| {
        let project = config.projects.entry(key.clone()).or_default();
        updater(project);
    })
}

pub fn get_global_config_write_count() -> u64 {
    GLOBAL_CONFIG_WRITE_COUNT.load(Ordering::Relaxed)
}

/// Maps to: CC `utils/config.ts:1763` `randomBytes(32).toString('hex')` —
/// 64 lowercase hex chars. Two UUID `simple()` strings are the same width.
fn generate_official_user_id() -> String {
    let first_half = uuid::Uuid::new_v4().simple().to_string();
    let second_half = uuid::Uuid::new_v4().simple().to_string();
    format!("{first_half}{second_half}")
}

/// Maps to: CC `utils/config.ts:1757-1766` `getOrCreateUserID`.
///
/// Returns the persisted `~/.claude.json` `userID`, minting one on first use.
/// `getAPIMetadata` (`services/api/claude.ts:522`) puts this in
/// `metadata.user_id.device_id`; a new value on every request is treated as a
/// new user by any gateway that keys on `user_id`.
pub fn get_or_create_user_id() -> String {
    let config = load_global_config();
    if let Some(ref id) = config.user_id {
        return id.clone();
    }

    let new_id = generate_official_user_id();
    let persisted = new_id.clone();
    let _ = save_global_config(|config| {
        config.user_id = Some(persisted);
    });
    // CC `saveGlobalConfig` always write-throughs the in-memory config
    // (test: `TEST_GLOBAL_CONFIG_FOR_TESTING`; prod: `globalConfigCache`).
    // The port's disk write can be gated; the minted id must still be the
    // value the next `getOrCreateUserID` / `getAPIMetadata` call sees.
    pin_created_user_id(&new_id);
    new_id
}

fn pin_created_user_id(user_id: &str) {
    #[cfg(test)]
    {
        let updated_override = TEST_GLOBAL_CONFIG.with(|cell| {
            if let Some(config) = cell.borrow_mut().as_mut() {
                if config.user_id.is_none() {
                    config.user_id = Some(user_id.to_string());
                }
                true
            } else {
                false
            }
        });
        if updated_override {
            return;
        }
    }
    let mut config = load_global_config();
    if config.user_id.as_deref() == Some(user_id) {
        return;
    }
    config.user_id = Some(user_id.to_string());
    write_through_global_config_cache(&crate::utils::env::get_global_claude_file(), config);
}

pub fn record_first_start_time() {
    let config = load_global_config();
    if config.first_start_time.is_some() {
        return;
    }
    let first_start_time = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let _ = save_global_config(|c| {
        if c.first_start_time.is_none() {
            c.first_start_time = Some(first_start_time.clone());
        }
    });
}

pub fn is_config_write_enabled() -> bool {
    crate::utils::env_utils::is_env_truthy(crate::utils::process_env::var("COMETIX_WRITE_ENABLED").as_deref())
}

fn is_write_enabled() -> bool {
    // Same gate as session persistence (`PORTING.md`): production writes
    // unless `COMETIX_WRITE_ENABLED=0`; tests must opt in. The previous
    // `is_env_truthy` check made every production `saveGlobalConfig` a
    // no-op, so `getOrCreateUserID` minted a new `device_id` per request.
    crate::utils::env_utils::is_cometix_write_enabled()
}

pub fn should_skip_plugin_autoupdate() -> bool {
    if crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("FORCE_AUTOUPDATE_PLUGINS").as_deref(),
    ) {
        return false;
    }
    get_auto_updater_disabled_reason().is_some()
}

pub fn get_auto_updater_disabled_reason() -> Option<&'static str> {
    if crate::utils::env_utils::is_env_truthy(crate::utils::process_env::var("DISABLE_AUTOUPDATER").as_deref())
        || crate::utils::env_utils::is_env_truthy(
            crate::utils::process_env::var("CLAUDE_CODE_DISABLE_AUTOUPDATER")
                .as_deref(),
        )
    {
        return Some("environment");
    }

    if cfg!(debug_assertions)
        && !crate::utils::env_utils::is_env_truthy(
            crate::utils::process_env::var("ENABLE_AUTOUPDATER_IN_DEVELOPMENT")
                .as_deref(),
        )
    {
        return Some("development");
    }

    let config = load_global_config();
    if config.auto_updates == Some(false) {
        return Some(if config.auto_updates_protected_for_native == Some(true) {
            "native-protection"
        } else {
            "configuration"
        });
    }

    None
}

pub fn format_auto_updater_disabled_reason(reason: &str) -> String {
    match reason {
        "environment" => "Auto-updates are disabled by environment variable.".to_string(),
        "development" => "Auto-updates are disabled in development builds.".to_string(),
        "native-protection" => {
            "Auto-updates are disabled because the native install is protected.".to_string()
        }
        "configuration" => "Auto-updates are disabled by configuration.".to_string(),
        other => format!("Auto-updates are disabled by {other}."),
    }
}

pub fn is_auto_updater_disabled() -> bool {
    get_auto_updater_disabled_reason().is_some()
}

pub fn get_remote_control_at_startup() -> bool {
    load_global_config()
        .remote_control_at_startup
        .unwrap_or(false)
}

pub fn find_most_recent_backup() -> Option<PathBuf> {
    let backup_dir = get_config_backup_dir();
    let global_config_file_name = crate::utils::env::get_global_claude_file()
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(".claude.json")
        .to_string();
    let backup_prefix = format!("{global_config_file_name}.backup.");

    let mut backups = fs::read_dir(backup_dir)
        .ok()?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let file_name = entry.file_name().to_string_lossy().to_string();
            file_name
                .starts_with(&backup_prefix)
                .then_some((file_name, entry.path()))
        })
        .collect::<Vec<_>>();
    backups.sort_by(|left, right| right.0.cmp(&left.0));
    backups.into_iter().next().map(|(_, path)| path)
}

// ════════════════════════════════════════════════════════════
// ════════════════════════════════════════════════════════════

fn has_trust_dialog_accepted_for_path(config: &GlobalConfig, cwd: &Path) -> bool {
    let mut path = cwd;
    loop {
        let key = normalize_project_path(&path.to_string_lossy());
        if let Some(project) = config.projects.get(&key) {
            if project.has_trust_dialog_accepted == Some(true) {
                return true;
            }
        }
        match path.parent() {
            Some(parent) if parent != path => path = parent,
            _ => break,
        }
    }
    false
}

pub fn is_path_trusted(path: &Path) -> bool {
    let config = load_global_config();
    has_trust_dialog_accepted_for_path(&config, path)
}

pub fn reset_trust_dialog_accepted_cache_for_testing() {
    TRUST_DIALOG_ACCEPTED_CACHE.store(false, Ordering::Relaxed);
}

pub fn check_has_trust_dialog_accepted() -> bool {
    // Maps to: CC `utils/config.ts#computeTrustDialogAccepted` — session trust
    // covers home-dir accept (memory only); then project path / parent walk.
    if crate::bootstrap::state::get_session_trust_accepted() {
        TRUST_DIALOG_ACCEPTED_CACHE.store(true, Ordering::Relaxed);
        return true;
    }
    if TRUST_DIALOG_ACCEPTED_CACHE.load(Ordering::Relaxed) {
        return true;
    }
    let config = load_global_config();
    let project_path = get_project_path_for_config();
    let cwd = crate::bootstrap::state::get_original_cwd();
    let accepted = has_trust_dialog_accepted_for_path(&config, &project_path)
        || has_trust_dialog_accepted_for_path(&config, &cwd);
    if accepted {
        TRUST_DIALOG_ACCEPTED_CACHE.store(true, Ordering::Relaxed);
    }
    accepted
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};
    use std::fs;

    fn unique_temp_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("cometix-config-{name}-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn memory_path_routes_team_mem_to_its_own_entrypoint() {
        assert_eq!(
            get_memory_path("TeamMem"),
            crate::memdir::team_mem_paths::get_team_mem_entrypoint()
        );
        assert_ne!(get_memory_path("TeamMem"), get_memory_path("Project"));
    }

    /// CC `utils/managedEnv.ts#applyConfigEnvironmentVariables` feeds
    /// `getGlobalConfig().env` through `Object.assign(process.env, ...)`; the
    /// Node v24 own-key oracle is recorded in process-env-carrier.md §1.2.
    #[test]
    fn global_config_env_order_matches_official_object_assign_input() {
        let config: GlobalConfig = serde_json::from_str(
            r#"{"env":{"1\u0000tail":"malformed","2":"two","SECOND":"2","1":"one","FIRST":"1"}}"#,
        )
        .unwrap();
        assert_eq!(
            config
                .env
                .as_ref()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["1", "2", "1\0tail", "SECOND", "FIRST"]
        );
        assert_eq!(
            serde_json::to_value(&config).unwrap()["env"]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["1", "2", "1\0tail", "SECOND", "FIRST"]
        );
    }

    #[test]
    fn global_config_deserializes_official_btw_use_count_for_spinner_tips() {
        let config: GlobalConfig = serde_json::from_str(r#"{"btwUseCount": 2}"#)
            .expect("global config should deserialize");

        assert_eq!(config.btw_use_count, Some(2));
    }

    #[test]
    fn global_config_reads_official_message_idle_notification_threshold() {
        let config: GlobalConfig =
            serde_json::from_str(r#"{"messageIdleNotifThresholdMs": 120000}"#)
                .expect("global config should deserialize");

        assert_eq!(config.message_idle_notif_threshold_ms(), 120_000);
        assert_eq!(
            GlobalConfig::default().message_idle_notif_threshold_ms(),
            DEFAULT_MESSAGE_IDLE_NOTIF_THRESHOLD_MS
        );
    }

    #[test]
    fn global_config_deserializes_official_permission_explainer_enabled() {
        let disabled: GlobalConfig =
            serde_json::from_str(r#"{"permissionExplainerEnabled": false}"#)
                .expect("global config should deserialize");
        assert_eq!(disabled.permission_explainer_enabled, Some(false));

        let defaulted: GlobalConfig =
            serde_json::from_str(r#"{}"#).expect("global config should deserialize");
        assert_eq!(defaulted.permission_explainer_enabled, None);
    }

    #[test]
    fn merge_fills_absent_display_pref_keys_with_ruled_factory_true() {
        // Pins the ruled default (v3, 2026-08-01) at the live merge path:
        // a disk config without the Cometix display-pref keys must load with
        // both effective true (factory-authoritative, CC {...DEFAULT, ...disk}
        // spread shape). Explicit disk values must still win.
        let merged = load_global_config_from_value(serde_json::json!({}))
            .expect("empty disk config should merge with factory defaults");
        assert_eq!(merged.expand_thinking, Some(true));
        assert_eq!(merged.expand_collapsed_read_search, Some(true));

        let overridden = load_global_config_from_value(serde_json::json!({
            "expandThinking": false,
            "expandCollapsedReadSearch": false,
        }))
        .expect("disk overrides should merge");
        assert_eq!(overridden.expand_thinking, Some(false));
        assert_eq!(overridden.expand_collapsed_read_search, Some(false));
    }

    #[test]
    fn global_config_deserializes_official_teammate_model_tristate() {
        let missing: GlobalConfig =
            serde_json::from_str(r#"{}"#).expect("global config should deserialize");
        assert_eq!(missing.teammate_default_model, None);

        let inherits_leader: GlobalConfig =
            serde_json::from_str(r#"{"teammateDefaultModel": null}"#)
                .expect("global config should deserialize");
        assert_eq!(inherits_leader.teammate_default_model, Some(None));

        let configured: GlobalConfig = serde_json::from_str(r#"{"teammateDefaultModel": "haiku"}"#)
            .expect("global config should deserialize");
        assert_eq!(
            configured.teammate_default_model,
            Some(Some("haiku".to_string()))
        );
    }

    /// Maps to: CC `utils/config.ts:549-551` — `string | null | undefined`,
    /// with the `null`/`undefined` split consumed at
    /// `utils/model/check1mAccess.ts:14-20`. JSON renders `undefined` as an
    /// absent key and `null` as a present one, so the disk round-trip has to
    /// preserve presence, not just value.
    #[test]
    fn global_config_round_trips_official_extra_usage_reason_tristate() {
        let missing: GlobalConfig =
            serde_json::from_str(r#"{}"#).expect("global config should deserialize");
        assert_eq!(
            missing.cached_extra_usage_disabled_reason,
            CachedExtraUsageDisabledReason::Uncached
        );

        let enabled: GlobalConfig =
            serde_json::from_str(r#"{"cachedExtraUsageDisabledReason": null}"#)
                .expect("global config should deserialize");
        assert_eq!(
            enabled.cached_extra_usage_disabled_reason,
            CachedExtraUsageDisabledReason::Enabled
        );

        let disabled: GlobalConfig =
            serde_json::from_str(r#"{"cachedExtraUsageDisabledReason": "out_of_credits"}"#)
                .expect("global config should deserialize");
        assert_eq!(
            disabled.cached_extra_usage_disabled_reason,
            CachedExtraUsageDisabledReason::Disabled("out_of_credits".to_string())
        );

        // The save filter strips nulls, so `Enabled` survives only because it
        // is re-inserted; `Uncached` must stay absent.
        let uncached_disk = global_config_disk_value(&GlobalConfig {
            cached_extra_usage_disabled_reason: CachedExtraUsageDisabledReason::Uncached,
            ..Default::default()
        })
        .expect("disk value");
        assert!(
            uncached_disk
                .get("cachedExtraUsageDisabledReason")
                .is_none()
        );

        for state in [
            CachedExtraUsageDisabledReason::Enabled,
            CachedExtraUsageDisabledReason::Disabled("out_of_credits".to_string()),
        ] {
            let disk = global_config_disk_value(&GlobalConfig {
                cached_extra_usage_disabled_reason: state.clone(),
                ..Default::default()
            })
            .expect("disk value");
            let reloaded =
                merge_disk_config_with_official_defaults(disk).expect("disk value should reload");
            assert_eq!(reloaded.cached_extra_usage_disabled_reason, state);
        }
    }

    #[test]
    fn global_config_deserializes_official_tips_history_map_readonly() {
        let config: GlobalConfig = serde_json::from_str(
            r#"{"numStartups": 9, "tipsHistory": {"custom-tip-0": 2, "custom-tip-1": 8}}"#,
        )
        .expect("global config should deserialize");

        let history = config.tips_history.expect("tips history");
        assert_eq!(history.get("custom-tip-0"), Some(&2));
        assert_eq!(history.get("custom-tip-1"), Some(&8));
    }

    #[test]
    fn global_config_ignores_legacy_array_tips_history_without_parse_failure() {
        let config: GlobalConfig = serde_json::from_str(r#"{"tipsHistory": ["legacy"]}"#)
            .expect("global config should deserialize");

        assert!(config.tips_history.is_none());
    }

    #[test]
    fn custom_api_key_status_matches_official_truncated_response_lists() {
        let mut config = GlobalConfig::default();
        let truncated = normalize_api_key_for_config("sk-ant-abcdefghijklmnopqrstuvwxyz");
        assert_eq!(truncated, "ghijklmnopqrstuvwxyz");
        assert_eq!(
            get_custom_api_key_status(&config, &truncated),
            CustomApiKeyStatus::New
        );

        config.custom_api_key_responses = Some(CustomApiKeyResponses {
            approved: Some(vec![truncated.clone()]),
            rejected: None,
        });
        assert_eq!(
            get_custom_api_key_status(&config, &truncated),
            CustomApiKeyStatus::Approved
        );

        config.custom_api_key_responses = Some(CustomApiKeyResponses {
            approved: None,
            rejected: Some(vec![truncated.clone()]),
        });
        assert_eq!(
            get_custom_api_key_status(&config, &truncated),
            CustomApiKeyStatus::Rejected
        );
    }

    #[test]
    fn global_config_deserializes_official_startup_dialog_tracking_fields() {
        let config: GlobalConfig = serde_json::from_str(
            r#"{
                "remoteDialogSeen": true,
                "desktopUpsellSeenCount": 2,
                "desktopUpsellDismissed": true,
                "hasCompletedClaudeInChromeOnboarding": true,
                "claudeInChromeDefaultEnabled": false
            }"#,
        )
        .expect("global config should deserialize startup dialog tracking fields");

        assert_eq!(config.remote_dialog_seen, Some(true));
        assert_eq!(config.desktop_upsell_seen_count, Some(2));
        assert_eq!(config.desktop_upsell_dismissed, Some(true));
        assert_eq!(config.has_completed_claude_in_chrome_onboarding, Some(true));
        assert_eq!(config.claude_in_chrome_default_enabled, Some(false));
    }

    #[test]
    fn global_config_deserializes_official_growthbook_cache_and_overrides() {
        let config: GlobalConfig = serde_json::from_str(
            r#"{
                "cachedGrowthBookFeatures": {
                    "tengu_prompt_cache_1h_config": {"allowlist": ["repl_main_thread*"]}
                },
                "growthBookOverrides": {
                    "tengu_prompt_cache_1h_config": {"allowlist": ["sdk"]}
                },
                "lastShownEmergencyTip": "old tip",
                "voiceNoticeSeenCount": 2,
                "opus1mMergeNoticeSeenCount": 5
            }"#,
        )
        .expect("global config should deserialize GrowthBook cache fields");

        assert_eq!(
            config
                .cached_growth_book_features
                .as_ref()
                .and_then(|features| {
                    features
                        .get("tengu_prompt_cache_1h_config")
                        .and_then(|value| value.get("allowlist"))
                        .and_then(|value| value.as_array())
                        .map(|values| values.len())
                }),
            Some(1)
        );
        assert_eq!(
            config.growth_book_overrides.as_ref().and_then(|features| {
                features
                    .get("tengu_prompt_cache_1h_config")
                    .and_then(|value| value.get("allowlist"))
                    .and_then(|value| value.as_array())
                    .and_then(|values| values.first())
                    .and_then(|value| value.as_str())
            }),
            Some("sdk")
        );
        assert_eq!(config.last_shown_emergency_tip.as_deref(), Some("old tip"));
        assert_eq!(config.voice_notice_seen_count, Some(2));
        assert_eq!(config.opus_1m_merge_notice_seen_count, Some(5));
    }

    #[test]
    fn trust_dialog_acceptance_walks_parent_project_keys_like_official() {
        let mut config = GlobalConfig::default();
        let mut parent = ProjectConfig::default();
        parent.has_trust_dialog_accepted = Some(true);
        config
            .projects
            .insert("/workspace/parent".to_string(), parent);

        assert!(has_trust_dialog_accepted_for_path(
            &config,
            std::path::Path::new("/workspace/parent/child/project")
        ));
        assert!(!has_trust_dialog_accepted_for_path(
            &config,
            std::path::Path::new("/workspace/other")
        ));
    }

    #[test]
    fn get_or_create_user_id_is_stable_across_calls_without_a_preseeded_id() {
        let _env = TEST_ENV_LOCK.lock().unwrap();
        let previous = replace_test_global_config(Some(GlobalConfig::default()));
        let first = get_or_create_user_id();
        let second = get_or_create_user_id();
        replace_test_global_config(previous);
        assert_eq!(first.len(), 64);
        assert!(
            first.chars().all(|ch| ch.is_ascii_hexdigit()),
            "CC `randomBytes(32).toString('hex')` is 64 hex chars, got {first}"
        );
        assert_eq!(first, second);
    }

    #[test]
    fn get_or_create_user_id_persists_user_id_to_global_config_like_official() {
        let _env = TEST_ENV_LOCK.lock().unwrap();
        let root = unique_temp_dir("user-id-persist");
        fs::create_dir_all(&root).unwrap();
        let _config_dir = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root);
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let previous = replace_test_global_config(None);
        clear_global_config_cache_for_testing();

        let minted = get_or_create_user_id();
        clear_global_config_cache_for_testing();
        let reloaded = load_global_config();
        replace_test_global_config(previous);
        clear_global_config_cache_for_testing();
        let _ = fs::remove_dir_all(&root);

        assert_eq!(reloaded.user_id.as_deref(), Some(minted.as_str()));
    }

    #[test]
    fn first_start_time_deserializes_official_iso_string() {
        let json = r#"{"firstStartTime": "2025-07-29T04:29:26.827Z", "projects": {}}"#;
        let config: GlobalConfig =
            serde_json::from_str(json).expect("ISO string firstStartTime must not fail");
        assert_eq!(
            config.first_start_time,
            Some("2025-07-29T04:29:26.827Z".to_string())
        );
    }

    #[test]
    fn mcp_raw_snapshot_survives_disk_cache_save_and_live_replacement() {
        use crate::services::mcp::config::get_mcp_configs_by_scope_readonly;
        use crate::services::mcp::types::ConfigScope;
        let _env = TEST_ENV_LOCK.lock().unwrap();
        let root = unique_temp_dir("mcp-raw-snapshot");
        fs::create_dir_all(&root).unwrap();
        let _config_dir = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root);
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        clear_global_config_cache_for_testing();
        let raw = serde_json::json!({
            "sdk":{"type":"sdk","name":"sdk-host","future":{"nested":null}},
            "bad":{"type":"http","url":"u","oauth":null},
            "nullArgs":{"command":"g","args":null,"env":null,"type":null}
        });
        let file = crate::utils::env::get_global_claude_file();
        let project_key = normalize_project_path(&get_project_path_for_config().to_string_lossy());
        let initial = serde_json::json!({"mcpServers":raw,"projects":{project_key.clone():{"mcpServers":raw}}});
        fs::write(&file, initial.to_string()).unwrap();
        let loaded = load_global_config();
        assert_eq!(loaded.mcp_servers, Some(raw.clone()));
        assert_eq!(loaded.projects[&project_key].mcp_servers, Some(raw.clone()));
        let cached = load_global_config();
        assert_eq!(cached.mcp_servers, loaded.mcp_servers);
        for scope in [ConfigScope::User, ConfigScope::Local] {
            let result =
                get_mcp_configs_by_scope_readonly(scope, &cached, &cached.projects[&project_key]);
            assert!(result.servers.is_empty());
            assert_eq!(result.errors.len(), 2);
        }

        // The actual source saveGlobalConfig oracle preserves these raw slots
        // when an unrelated setting changes; no parallel raw mirror exists.
        save_global_config(|config| config.theme = Some("light".to_string())).unwrap();
        let saved: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(saved["mcpServers"], raw);
        assert_eq!(saved["projects"][&project_key]["mcpServers"], raw);
        clear_global_config_cache_for_testing();
        let reread = load_global_config();
        assert_eq!(reread.mcp_servers, Some(raw.clone()));
        assert_eq!(reread.projects[&project_key].mcp_servers, Some(raw));

        let global_replacement = serde_json::json!({"changed":{"type":"sdk","name":"new-host"}});
        let project_replacement = serde_json::json!({"changed":{"command":"new-command"}});
        save_global_config(|config| {
            config.mcp_servers = Some(global_replacement.clone());
        })
        .unwrap();
        save_current_project_config(|project| {
            project.mcp_servers = Some(project_replacement.clone())
        })
        .unwrap();
        let cached = load_global_config();
        assert_eq!(cached.mcp_servers, Some(global_replacement.clone()));
        assert_eq!(
            cached.projects[&project_key].mcp_servers,
            Some(project_replacement.clone())
        );
        let user = get_mcp_configs_by_scope_readonly(
            ConfigScope::User,
            &cached,
            &cached.projects[&project_key],
        );
        assert!(user.errors.is_empty());
        assert_eq!(user.servers["changed"].name.as_deref(), Some("new-host"));
        let local = get_mcp_configs_by_scope_readonly(
            ConfigScope::Local,
            &cached,
            &cached.projects[&project_key],
        );
        assert!(local.errors.is_empty());
        assert_eq!(
            local.servers["changed"].command.as_deref(),
            Some("new-command")
        );
        let saved: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(saved["mcpServers"], global_replacement);
        assert_eq!(
            saved["projects"][&project_key]["mcpServers"],
            project_replacement
        );

        save_global_config(|config| {
            config.mcp_servers = Some(serde_json::Value::Null);
        })
        .unwrap();
        save_current_project_config(|project| project.mcp_servers = Some(serde_json::Value::Null))
            .unwrap();
        clear_global_config_cache_for_testing();
        let reread = load_global_config();
        assert_eq!(reread.mcp_servers, Some(serde_json::Value::Null));
        assert_eq!(
            reread.projects[&project_key].mcp_servers,
            Some(serde_json::Value::Null)
        );
        save_global_config(|config| {
            config.mcp_servers = None;
        })
        .unwrap();
        save_current_project_config(|project| project.mcp_servers = None).unwrap();
        let saved: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
        assert!(saved.get("mcpServers").is_none());
        assert!(saved["projects"][&project_key].get("mcpServers").is_none());
        clear_global_config_cache_for_testing();
        let _ = fs::remove_dir_all(root);
    }
}
