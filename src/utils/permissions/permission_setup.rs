//! Permission context setup from settings.
//! Maps to: CC `utils/permissions/permissionSetup.ts`.

use super::permission_mode::permission_mode_from_string;
use super::permission_rule_parser::{
    normalize_legacy_tool_name, permission_rule_value_from_string,
};
use crate::tool::ToolPermissionContext;
use crate::types::permissions::{
    PermissionBehavior, PermissionMode, PermissionRule, PermissionRuleSource, PermissionRuleValue,
    PermissionUpdate, PermissionUpdateDestination,
};
use crate::utils::fs_operations::{get_fs_implementation, safe_resolve_path};
use crate::utils::process_env::JsTruthy;
use crate::utils::settings::types::SettingsJson;
use std::collections::HashMap;
use std::sync::Arc;

/// Maps to: CC `utils/permissions/permissionSetup.ts:652-664` `parseBaseToolsFromCLI(...)`.
pub fn parse_base_tools_from_cli(base_tools: &[String]) -> Vec<String> {
    let joined_input = base_tools.join(" ");
    if crate::tools::parse_tool_preset(joined_input.trim()).is_some() {
        return crate::tools::get_tools_for_default_preset();
    }
    parse_tool_list_from_cli(base_tools)
}

/// Maps to: CC `utils/permissions/permissionSetup.ts:813-860` `parseToolListFromCLI(...)`.
pub fn parse_tool_list_from_cli(tools: &[String]) -> Vec<String> {
    if tools.is_empty() {
        return Vec::new();
    }

    let mut result = Vec::new();
    for tool_string in tools {
        if tool_string.is_empty() {
            continue;
        }

        let mut current = String::new();
        let mut is_in_parens = false;
        for ch in tool_string.chars() {
            match ch {
                '(' => {
                    is_in_parens = true;
                    current.push(ch);
                }
                ')' => {
                    is_in_parens = false;
                    current.push(ch);
                }
                ',' if !is_in_parens => {
                    let trimmed = current.trim();
                    if !trimmed.is_empty() {
                        result.push(trimmed.to_string());
                    }
                    current.clear();
                }
                ' ' if !is_in_parens => {
                    let trimmed = current.trim();
                    if !trimmed.is_empty() {
                        result.push(trimmed.to_string());
                        current.clear();
                    }
                }
                _ => current.push(ch),
            }
        }

        let trimmed = current.trim();
        if !trimmed.is_empty() {
            result.push(trimmed.to_string());
        }
    }

    result
}

/// Maps to CC `permissionSetup.ts#isDangerousBashPermission(...)`.
pub fn is_dangerous_bash_permission(tool_name: &str, rule_content: Option<&str>) -> bool {
    if tool_name != crate::tools::bash_tool::tool_name::BASH_TOOL_NAME {
        return false;
    }
    let Some(rule_content) = rule_content.filter(|content| !content.is_empty()) else {
        return true;
    };
    let content = rule_content.trim().to_ascii_lowercase();
    if content == "*" {
        return true;
    }
    super::dangerous_patterns::dangerous_bash_patterns()
        .into_iter()
        .any(|pattern| dangerous_rule_content_matches_pattern(&content, pattern))
}

/// Maps to CC `permissionSetup.ts#isDangerousPowerShellPermission(...)`.
pub fn is_dangerous_powershell_permission(tool_name: &str, rule_content: Option<&str>) -> bool {
    if tool_name != crate::tools::powershell_tool::tool_name::POWERSHELL_TOOL_NAME {
        return false;
    }
    let Some(rule_content) = rule_content.filter(|content| !content.is_empty()) else {
        return true;
    };
    let content = rule_content.trim().to_ascii_lowercase();
    if content == "*" {
        return true;
    }
    power_shell_dangerous_patterns().into_iter().any(|pattern| {
        dangerous_rule_content_matches_pattern(&content, pattern)
            || dangerous_rule_content_matches_pattern(&content, &exe_variant(pattern))
    })
}

/// Maps to CC `permissionSetup.ts#isDangerousTaskPermission(...)`.
pub fn is_dangerous_task_permission(tool_name: &str, _rule_content: Option<&str>) -> bool {
    normalize_legacy_tool_name(tool_name) == crate::tools::agent_tool::constants::AGENT_TOOL_NAME
}

fn is_dangerous_classifier_permission(tool_name: &str, rule_content: Option<&str>) -> bool {
    if crate::utils::build_profile::has_internal_capability(
        crate::utils::build_profile::InternalCapability::Permissions,
    ) && tool_name == "Tmux"
    {
        return true;
    }
    is_dangerous_bash_permission(tool_name, rule_content)
        || is_dangerous_powershell_permission(tool_name, rule_content)
        || is_dangerous_task_permission(tool_name, rule_content)
}

fn dangerous_rule_content_matches_pattern(content: &str, pattern: &str) -> bool {
    let lower_pattern = pattern.to_ascii_lowercase();
    content == lower_pattern
        || content == format!("{lower_pattern}:*")
        || content == format!("{lower_pattern}*")
        || content == format!("{lower_pattern} *")
        || (content.starts_with(&format!("{lower_pattern} -")) && content.ends_with('*'))
}

fn power_shell_dangerous_patterns() -> Vec<&'static str> {
    let mut patterns = Vec::new();
    patterns.extend_from_slice(super::dangerous_patterns::CROSS_PLATFORM_CODE_EXEC);
    patterns.extend_from_slice(&[
        "pwsh",
        "powershell",
        "cmd",
        "wsl",
        "iex",
        "invoke-expression",
        "icm",
        "invoke-command",
        "start-process",
        "saps",
        "start",
        "start-job",
        "sajb",
        "start-threadjob",
        "register-objectevent",
        "register-engineevent",
        "register-wmievent",
        "register-scheduledjob",
        "new-pssession",
        "nsn",
        "enter-pssession",
        "etsn",
        "add-type",
        "new-object",
    ]);
    patterns
}

fn exe_variant(pattern: &str) -> String {
    if let Some((first, rest)) = pattern.split_once(' ') {
        format!("{first}.exe {rest}")
    } else {
        format!("{pattern}.exe")
    }
}

/// Maps to CC `permissionSetup.ts#DangerousPermissionInfo`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DangerousPermissionInfo {
    pub rule_value: PermissionRuleValue,
    pub source: PermissionRuleSource,
    pub rule_display: String,
    pub source_display: String,
}

/// Maps to CC `permissionSetup.ts#findDangerousClassifierPermissions(...)`.
pub fn find_dangerous_classifier_permissions(
    rules: &[PermissionRule],
    cli_allowed_tools: &[String],
) -> Vec<DangerousPermissionInfo> {
    let mut dangerous = Vec::new();
    for rule in rules {
        if rule.rule_behavior == PermissionBehavior::Allow
            && is_dangerous_classifier_permission(
                &rule.rule_value.tool_name,
                rule.rule_value.rule_content.as_deref(),
            )
        {
            dangerous.push(DangerousPermissionInfo {
                rule_value: rule.rule_value.clone(),
                source: rule.source,
                rule_display: permission_rule_display(&rule.rule_value),
                source_display: format_permission_source(rule.source),
            });
        }
    }
    for tool_spec in cli_allowed_tools {
        let parsed = permission_rule_value_from_string(tool_spec);
        if is_dangerous_classifier_permission(&parsed.tool_name, parsed.rule_content.as_deref()) {
            dangerous.push(DangerousPermissionInfo {
                rule_display: parsed
                    .rule_content
                    .as_ref()
                    .map(|_| tool_spec.clone())
                    .unwrap_or_else(|| format!("{}(*)", parsed.tool_name)),
                rule_value: parsed,
                source: PermissionRuleSource::CliArg,
                source_display: "--allowed-tools".to_string(),
            });
        }
    }
    dangerous
}

/// Maps to CC `permissionSetup.ts#isOverlyBroadBashAllowRule(...)`.
pub fn is_overly_broad_bash_allow_rule(rule_value: &PermissionRuleValue) -> bool {
    rule_value.tool_name == crate::tools::bash_tool::tool_name::BASH_TOOL_NAME
        && rule_value.rule_content.is_none()
}

/// Maps to CC `permissionSetup.ts#isOverlyBroadPowerShellAllowRule(...)`.
pub fn is_overly_broad_powershell_allow_rule(rule_value: &PermissionRuleValue) -> bool {
    rule_value.tool_name == crate::tools::powershell_tool::tool_name::POWERSHELL_TOOL_NAME
        && rule_value.rule_content.is_none()
}

/// Maps to CC `permissionSetup.ts#findOverlyBroadBashPermissions(...)`.
pub fn find_overly_broad_bash_permissions(
    rules: &[PermissionRule],
    cli_allowed_tools: &[String],
) -> Vec<DangerousPermissionInfo> {
    find_overly_broad_permissions(
        rules,
        cli_allowed_tools,
        is_overly_broad_bash_allow_rule,
        crate::tools::bash_tool::tool_name::BASH_TOOL_NAME,
    )
}

/// Maps to CC `permissionSetup.ts#findOverlyBroadPowerShellPermissions(...)`.
pub fn find_overly_broad_powershell_permissions(
    rules: &[PermissionRule],
    cli_allowed_tools: &[String],
) -> Vec<DangerousPermissionInfo> {
    find_overly_broad_permissions(
        rules,
        cli_allowed_tools,
        is_overly_broad_powershell_allow_rule,
        crate::tools::powershell_tool::tool_name::POWERSHELL_TOOL_NAME,
    )
}

fn find_overly_broad_permissions(
    rules: &[PermissionRule],
    cli_allowed_tools: &[String],
    predicate: fn(&PermissionRuleValue) -> bool,
    tool_name: &str,
) -> Vec<DangerousPermissionInfo> {
    let mut overly_broad = Vec::new();
    for rule in rules {
        if rule.rule_behavior == PermissionBehavior::Allow && predicate(&rule.rule_value) {
            overly_broad.push(DangerousPermissionInfo {
                rule_value: rule.rule_value.clone(),
                source: rule.source,
                rule_display: format!("{tool_name}(*)"),
                source_display: format_permission_source(rule.source),
            });
        }
    }
    for tool_spec in cli_allowed_tools {
        let parsed = permission_rule_value_from_string(tool_spec);
        if predicate(&parsed) {
            overly_broad.push(DangerousPermissionInfo {
                rule_value: parsed,
                source: PermissionRuleSource::CliArg,
                rule_display: format!("{tool_name}(*)"),
                source_display: "--allowed-tools".to_string(),
            });
        }
    }
    overly_broad
}

/// Maps to CC `permissionSetup.ts#removeDangerousPermissions(...)`.
pub fn remove_dangerous_permissions(
    context: &ToolPermissionContext,
    dangerous_permissions: &[DangerousPermissionInfo],
) -> ToolPermissionContext {
    let mut updated = context.clone();
    for permission in dangerous_permissions {
        if is_permission_update_destination(permission.source) {
            if let Some(rules) = updated.always_allow_rules.get_mut(&permission.source) {
                rules.retain(|rule| rule != &permission.rule_value);
            }
        }
    }
    updated
}

/// Maps to CC `permissionSetup.ts#stripDangerousPermissionsForAutoMode(...)`.
pub fn strip_dangerous_permissions_for_auto_mode(
    context: &ToolPermissionContext,
) -> ToolPermissionContext {
    let rules = super::permissions::get_allow_rules(context);
    let dangerous_permissions = find_dangerous_classifier_permissions(&rules, &[]);
    if dangerous_permissions.is_empty() {
        let mut next = context.clone();
        if next.stripped_dangerous_rules.is_none() {
            next.stripped_dangerous_rules = Some(HashMap::new());
        }
        return next;
    }

    let mut stripped: HashMap<PermissionRuleSource, Vec<PermissionRuleValue>> = HashMap::new();
    for permission in &dangerous_permissions {
        // Maps to CC `permissionSetup.ts` stripDangerousPermissionsForAutoMode log.
        crate::utils::debug::log_for_debugging(&format!(
            "Ignoring dangerous permission {} from {} (bypasses classifier)",
            permission.rule_display, permission.source_display
        ));
        if is_permission_update_destination(permission.source) {
            stripped
                .entry(permission.source)
                .or_default()
                .push(permission.rule_value.clone());
        }
    }
    let mut next = remove_dangerous_permissions(context, &dangerous_permissions);
    next.stripped_dangerous_rules = Some(stripped);
    next
}

/// Maps to CC `permissionSetup.ts#restoreDangerousPermissions(...)`.
pub fn restore_dangerous_permissions(context: &ToolPermissionContext) -> ToolPermissionContext {
    let Some(stash) = context.stripped_dangerous_rules.as_ref() else {
        return context.clone();
    };
    let mut result = context.clone();
    for (source, rules) in stash {
        result
            .always_allow_rules
            .entry(*source)
            .or_default()
            .extend(rules.iter().cloned());
    }
    result.stripped_dangerous_rules = None;
    result
}

/// Maps to: CC `utils/permissions/permissionSetup.ts:456-466#isPermissionUpdateDestination`.
fn is_permission_update_destination(source: PermissionRuleSource) -> bool {
    matches!(
        source,
        PermissionRuleSource::UserSettings
            | PermissionRuleSource::ProjectSettings
            | PermissionRuleSource::LocalSettings
            | PermissionRuleSource::Session
            | PermissionRuleSource::CliArg
    )
}

fn permission_rule_display(rule_value: &PermissionRuleValue) -> String {
    rule_value
        .rule_content
        .as_ref()
        .map(|content| format!("{}({content})", rule_value.tool_name))
        .unwrap_or_else(|| format!("{}(*)", rule_value.tool_name))
}

fn format_permission_source(source: PermissionRuleSource) -> String {
    match source {
        PermissionRuleSource::CliArg => "--allowed-tools".to_string(),
        _ => super::permission_rule::permission_rule_source_to_official_str(source).to_string(),
    }
}

/// Maps to CC `permissionSetup.ts#isBypassPermissionsModeDisabled()`.
///
/// Reads the cached security gate and source-merged settings, as startup does.
pub fn is_bypass_permissions_mode_disabled() -> bool {
    crate::services::analytics::growthbook::check_statsig_feature_gate_cached_may_be_stale(
        "tengu_disable_bypass_permissions_mode",
    ) || crate::utils::settings::get_initial_settings()
        .permissions
        .and_then(|p| p.disable_bypass_permissions_mode)
        .as_deref()
        == Some("disable")
}

/// Maps to CC `permissionSetup.ts#createDisabledBypassPermissionsContext(...)`.
pub fn create_disabled_bypass_permissions_context(
    current_context: &ToolPermissionContext,
) -> ToolPermissionContext {
    let mut updated = current_context.clone();
    if updated.mode == PermissionMode::BypassPermissions {
        updated = super::permission_update::apply_permission_update(
            &updated,
            &PermissionUpdate::SetMode {
                destination: PermissionUpdateDestination::Session,
                mode: PermissionMode::Default,
            },
        );
    }
    updated.is_bypass_permissions_mode_available = false;
    updated
}

/// Maps to CC `permissionSetup.ts#isDefaultPermissionModeAuto`.
/// Exact match on `"auto"` (CC `=== 'auto'`), not case-insensitive.
pub fn is_default_permission_mode_auto(settings: &SettingsJson) -> bool {
    if !is_transcript_classifier_feature_enabled() {
        return false;
    }
    settings
        .permissions
        .as_ref()
        .and_then(|permissions| permissions.default_mode.as_deref())
        .or(settings.default_permission_mode.as_deref())
        .map(|mode| mode == "auto")
        .unwrap_or(false)
}

/// Maps to: CC `permissionSetup.ts:689-811#initialPermissionModeFromCLI`.
pub fn initial_permission_mode_from_cli(
    permission_mode_cli: Option<&str>,
    dangerously_skip_permissions: bool,
) -> (PermissionMode, Option<String>) {
    let settings = crate::utils::settings::get_initial_settings();
    let policy_disabled =
        crate::services::analytics::growthbook::check_statsig_feature_gate_cached_may_be_stale(
            "tengu_disable_bypass_permissions_mode",
        );
    let settings_disabled = settings
        .permissions
        .as_ref()
        .and_then(|p| p.disable_bypass_permissions_mode.as_deref())
        == Some("disable");
    let broken = is_transcript_classifier_feature_enabled()
        && get_auto_mode_enabled_state_if_cached() == Some(AutoModeEnabledState::Disabled);
    let mut modes = Vec::new();
    if dangerously_skip_permissions {
        modes.push(PermissionMode::BypassPermissions);
    }
    if let Some(cli) = permission_mode_cli.filter(|v| !v.is_empty()) {
        let mode = permission_mode_from_string(cli);
        if !(is_transcript_classifier_feature_enabled() && mode == PermissionMode::Auto && broken) {
            modes.push(mode);
        }
    }
    if let Some(mode) = settings
        .permissions
        .as_ref()
        .and_then(|p| p.default_mode.as_deref())
        .filter(|v| !v.is_empty())
    {
        let remote = crate::utils::env_utils::is_env_truthy(
            crate::utils::process_env::var("CLAUDE_CODE_REMOTE").as_deref(),
        );
        if remote && !["acceptEdits", "plan", "default"].contains(&mode) {
            crate::utils::debug::log_for_debugging(&format!(
                "settings defaultMode \"{mode}\" is not supported in CLAUDE_CODE_REMOTE — only acceptEdits and plan are allowed"
            ));
            crate::services::analytics::log_event(
                "tengu_ccr_unsupported_default_mode_ignored",
                serde_json::json!({"mode": mode}),
            );
        } else {
            let mode = permission_mode_from_string(mode);
            if !(is_transcript_classifier_feature_enabled()
                && mode == PermissionMode::Auto
                && broken)
            {
                modes.push(mode);
            }
        }
    }
    let mut notification = None;
    for mode in modes {
        if mode == PermissionMode::BypassPermissions && (policy_disabled || settings_disabled) {
            notification = Some(
                if policy_disabled {
                    "Bypass permissions mode was disabled by your organization policy"
                } else {
                    "Bypass permissions mode was disabled by settings"
                }
                .into(),
            );
            continue;
        }
        if is_transcript_classifier_feature_enabled() && mode == PermissionMode::Auto {
            super::auto_mode_state::set_auto_mode_active(true);
        }
        return (mode, notification);
    }
    (PermissionMode::Default, notification)
}

/// Maps to CC `feature('TRANSCRIPT_CLASSIFIER')` for Auto Mode gates.
pub fn is_transcript_classifier_feature_enabled() -> bool {
    crate::utils::feature_flags::feature_enabled(
        crate::utils::feature_flags::FeatureFlag::TranscriptClassifier,
    ) || crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("COMETIX_TRANSCRIPT_CLASSIFIER")
            .as_deref(),
    )
}

/// Maps to CC `hasAutoModeOptIn()` — persistent consent via settings.
pub fn has_auto_mode_opt_in() -> bool {
    crate::utils::settings::get_initial_settings()
        .skip_auto_permission_prompt
        .unwrap_or(false)
}

/// Maps to CC `hasAutoModeOptInAnySource()` — CLI flag or settings opt-in.
pub fn has_auto_mode_opt_in_any_source() -> bool {
    crate::utils::permissions::auto_mode_state::get_auto_mode_flag_cli() || has_auto_mode_opt_in()
}

/// Maps to CC `isAutoModeDisabledBySettings()`.
pub fn is_auto_mode_disabled_by_settings() -> bool {
    let settings = crate::utils::settings::get_initial_settings();
    settings.disable_auto_mode.as_deref() == Some("disable")
        || settings
            .permissions
            .as_ref()
            .and_then(|p| p.disable_auto_mode.as_deref())
            == Some("disable")
}

/// Maps to CC `getUseAutoModeDuringPlan()` — default true when unset.
pub fn get_use_auto_mode_during_plan() -> bool {
    crate::utils::settings::get_initial_settings()
        .use_auto_mode_during_plan
        .unwrap_or(true)
}

/// Maps to CC `isAutoModeGateEnabled()`.
pub fn is_auto_mode_gate_enabled() -> bool {
    if !is_transcript_classifier_feature_enabled() {
        return false;
    }
    if crate::utils::permissions::auto_mode_state::is_auto_mode_circuit_broken() {
        return false;
    }
    if is_auto_mode_disabled_by_settings() {
        return false;
    }
    if !crate::utils::betas::model_supports_auto_mode(
        &crate::utils::model::model::get_main_loop_model(),
    ) {
        return false;
    }
    true
}

/// Maps to CC `getAutoModeUnavailableReason()`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutoModeUnavailableReason {
    Settings,
    CircuitBreaker,
    Model,
}

pub fn get_auto_mode_unavailable_reason() -> Option<AutoModeUnavailableReason> {
    if is_auto_mode_disabled_by_settings() {
        return Some(AutoModeUnavailableReason::Settings);
    }
    if crate::utils::permissions::auto_mode_state::is_auto_mode_circuit_broken() {
        return Some(AutoModeUnavailableReason::CircuitBreaker);
    }
    if !crate::utils::betas::model_supports_auto_mode(
        &crate::utils::model::model::get_main_loop_model(),
    ) {
        return Some(AutoModeUnavailableReason::Model);
    }
    None
}

/// Maps to CC `permissionSetup.ts#getAutoModeUnavailableNotification`.
pub fn get_auto_mode_unavailable_notification(reason: AutoModeUnavailableReason) -> String {
    let base = match reason {
        AutoModeUnavailableReason::Settings => "auto mode disabled by settings",
        AutoModeUnavailableReason::CircuitBreaker => "auto mode is unavailable for your plan",
        AutoModeUnavailableReason::Model => "auto mode unavailable for this model",
    };
    if crate::utils::build_profile::has_internal_capability(
        crate::utils::build_profile::InternalCapability::Permissions,
    ) {
        format!("{base} · #claude-code-feedback")
    } else {
        base.to_string()
    }
}

/// Maps to: CC `permissionSetup.ts:1311-1320#AutoModeEnabledState`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutoModeEnabledState {
    Enabled,
    Disabled,
    OptIn,
}

fn parse_auto_mode_enabled_state(value: Option<&serde_json::Value>) -> AutoModeEnabledState {
    match value.and_then(serde_json::Value::as_str) {
        Some("enabled") => AutoModeEnabledState::Enabled,
        Some("opt-in") => AutoModeEnabledState::OptIn,
        _ => AutoModeEnabledState::Disabled,
    }
}

/// Maps to: CC `permissionSetup.ts:1328-1333#getAutoModeEnabledState`.
pub fn get_auto_mode_enabled_state() -> AutoModeEnabledState {
    let config = crate::services::analytics::growthbook::get_feature_value_cached_may_be_stale(
        "tengu_auto_mode_config",
        serde_json::json!({}),
    );
    parse_auto_mode_enabled_state(config.get("enabled"))
}

/// Maps to: CC `permissionSetup.ts:1335-1352#getAutoModeEnabledStateIfCached`.
pub fn get_auto_mode_enabled_state_if_cached() -> Option<AutoModeEnabledState> {
    // L1 presence carrier for the source Symbol sentinel: a present JSON null
    // MUST remain distinct from the absent/default cache value.
    #[derive(Clone)]
    struct CachedConfig(Option<serde_json::Value>);
    impl<'de> serde::Deserialize<'de> for CachedConfig {
        fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            <serde_json::Value as serde::Deserialize>::deserialize(deserializer)
                .map(|value| Self(Some(value)))
        }
    }
    let config = crate::services::analytics::growthbook::get_feature_value_cached_may_be_stale(
        "tengu_auto_mode_config",
        CachedConfig(None),
    );
    config
        .0
        .map(|value| parse_auto_mode_enabled_state(value.get("enabled")))
}

/// Maps to: CC `permissionSetup.ts:1036-1045#AutoModeGateCheckResult`.
/// Arc preserves the source updater's same-context identity on a no-op.
pub struct AutoModeGateCheckResult {
    pub update_context:
        Arc<dyn Fn(&Arc<ToolPermissionContext>) -> Arc<ToolPermissionContext> + Send + Sync>,
    pub notification: Option<String>,
}

/// Maps to: CC `permissionSetup.ts:1078-1261#verifyAutoModeGateAccess`.
/// The returned transform re-checks the fresh AppStore context after the await.
pub async fn verify_auto_mode_gate_access(
    current_context: &ToolPermissionContext,
    fast_mode: Option<bool>,
) -> AutoModeGateCheckResult {
    let config = crate::services::analytics::growthbook::get_dynamic_config_blocks_on_init(
        "tengu_auto_mode_config",
        serde_json::json!({}),
    )
    .await;
    let enabled = parse_auto_mode_enabled_state(config.get("enabled"));
    let disabled_by_settings = is_auto_mode_disabled_by_settings();
    super::auto_mode_state::set_auto_mode_circuit_broken(
        enabled == AutoModeEnabledState::Disabled || disabled_by_settings,
    );
    let main_model = crate::utils::model::model::get_main_loop_model();
    let disable_fast = config
        .get("disableFastMode")
        .is_some_and(|value| match value {
            serde_json::Value::Null => false,
            serde_json::Value::Bool(value) => *value,
            serde_json::Value::Number(value) => value.as_f64() != Some(0.0),
            serde_json::Value::String(value) => !value.is_empty(),
            serde_json::Value::Array(_) | serde_json::Value::Object(_) => true,
        })
        && (fast_mode.unwrap_or(false)
            || (crate::utils::build_profile::has_internal_capability(
                crate::utils::build_profile::InternalCapability::Permissions,
            ) && main_model.to_lowercase().contains("-fast")));
    let model_supported =
        crate::utils::betas::model_supports_auto_mode(&main_model) && !disable_fast;
    let can_enter =
        enabled != AutoModeEnabledState::Disabled && !disabled_by_settings && model_supported;
    let carousel_available = can_enter
        && (enabled == AutoModeEnabledState::Enabled || has_auto_mode_opt_in_any_source());
    let auto_flag_cli = super::auto_mode_state::get_auto_mode_flag_cli();
    crate::utils::debug::log_for_debugging(&format!(
        "[auto-mode] verifyAutoModeGateAccess: enabledState={enabled:?} disabledBySettings={disabled_by_settings} model={main_model} modelSupported={model_supported} disableFastModeBreakerFires={disable_fast} carouselAvailable={carousel_available} canEnterAuto={can_enter}"
    ));
    let set_available = |ctx: &Arc<ToolPermissionContext>, available| {
        if ctx.is_auto_mode_available == Some(available) {
            return ctx.clone();
        }
        let mut next = ctx.as_ref().clone();
        next.is_auto_mode_available = Some(available);
        Arc::new(next)
    };
    if can_enter {
        return AutoModeGateCheckResult {
            update_context: Arc::new(move |ctx| set_available(ctx, carousel_available)),
            notification: None,
        };
    }
    let reason = if disabled_by_settings {
        AutoModeUnavailableReason::Settings
    } else if enabled == AutoModeEnabledState::Disabled {
        AutoModeUnavailableReason::CircuitBreaker
    } else {
        AutoModeUnavailableReason::Model
    };
    let notification = get_auto_mode_unavailable_notification(reason);
    let update_context = Arc::new(move |ctx: &Arc<ToolPermissionContext>| {
        let in_auto = ctx.mode == PermissionMode::Auto;
        let in_plan_auto = ctx.mode == PermissionMode::Plan
            && (ctx.pre_plan_mode == Some(PermissionMode::Auto)
                || ctx.stripped_dangerous_rules.is_some());
        if !in_auto && !in_plan_auto {
            return set_available(ctx, false);
        }
        super::auto_mode_state::set_auto_mode_active(false);
        crate::bootstrap::state::set_needs_auto_mode_exit_attachment(true);
        let mut next = restore_dangerous_permissions(ctx);
        if in_auto {
            next = super::permission_update::apply_permission_update(
                &next,
                &PermissionUpdate::SetMode {
                    mode: PermissionMode::Default,
                    destination: PermissionUpdateDestination::Session,
                },
            );
        } else if next.pre_plan_mode == Some(PermissionMode::Auto) {
            next.pre_plan_mode = Some(PermissionMode::Default);
        }
        next.is_auto_mode_available = Some(false);
        Arc::new(next)
    });
    let was_auto = current_context.mode == PermissionMode::Auto;
    let was_plan_auto = current_context.mode == PermissionMode::Plan
        && (current_context.pre_plan_mode == Some(PermissionMode::Auto)
            || current_context.stripped_dangerous_rules.is_some());
    let should_notify = was_auto
        || was_plan_auto
        || (auto_flag_cli && current_context.is_auto_mode_available == Some(true));
    AutoModeGateCheckResult {
        update_context,
        notification: should_notify.then_some(notification),
    }
}

/// Maps to: CC `permissionSetup.ts:1266-1268#shouldDisableBypassPermissions`.
pub async fn should_disable_bypass_permissions() -> bool {
    crate::services::analytics::growthbook::check_security_restriction_gate(
        "tengu_disable_bypass_permissions_mode",
    )
    .await
}

/// Maps to CC `shouldPlanUseAutoMode()`.
pub fn should_plan_use_auto_mode() -> bool {
    if !is_transcript_classifier_feature_enabled() {
        return false;
    }
    has_auto_mode_opt_in() && is_auto_mode_gate_enabled() && get_use_auto_mode_during_plan()
}

/// Maps to CC `utils/permissions/permissionSetup.ts:1502-1536#transitionPlanAutoMode`.
pub fn transition_plan_auto_mode(context: &ToolPermissionContext) -> ToolPermissionContext {
    if !is_transcript_classifier_feature_enabled()
        || context.mode != PermissionMode::Plan
        || context.pre_plan_mode == Some(PermissionMode::BypassPermissions)
    {
        return context.clone();
    }

    let want = should_plan_use_auto_mode();
    let have = crate::utils::permissions::auto_mode_state::is_auto_mode_active();
    if want && have {
        // A preceding disk sync may have re-added dangerous rules while
        // preserving the stash, so active Plan+Auto must strip again.
        return strip_dangerous_permissions_for_auto_mode(context);
    }
    if !want && !have {
        return context.clone();
    }
    if want {
        crate::utils::permissions::auto_mode_state::set_auto_mode_active(true);
        crate::bootstrap::state::set_needs_auto_mode_exit_attachment(false);
        return strip_dangerous_permissions_for_auto_mode(context);
    }

    crate::utils::permissions::auto_mode_state::set_auto_mode_active(false);
    crate::bootstrap::state::set_needs_auto_mode_exit_attachment(true);
    restore_dangerous_permissions(context)
}

/// Test/helper: transition into Auto then set `mode` (CC callers do the same
/// two steps: `transitionPermissionMode` then assign mode).
/// Prefer call sites using `transition_permission_mode` + set mode explicitly.
pub fn transition_into_auto_mode(context: &ToolPermissionContext) -> ToolPermissionContext {
    let from =
        crate::utils::permissions::permission_mode::permission_mode_internal_name(context.mode);
    let mut next = transition_permission_mode(from, "auto", context);
    next.mode = PermissionMode::Auto;
    next.is_auto_mode_available = Some(true);
    next
}

/// Test/helper: leave Auto via `transition_permission_mode("auto", "default", …)`
/// then set mode. Not a CC export name.
pub fn transition_out_of_auto_mode(context: &ToolPermissionContext) -> ToolPermissionContext {
    let mut next = transition_permission_mode("auto", "default", context);
    next.mode = PermissionMode::Default;
    next
}

/// Maps to: CC `permissionSetup.ts` `transitionPermissionMode(fromMode, toMode, context)`.
///
/// Centralises side-effects so CLI Shift+Tab, SDK, etc. behave identically.
/// - Plan enter/exit attachments (`handlePlanModeTransition`)
/// - Auto enter/exit: `setAutoModeActive`, strip/restore dangerous permissions
///
/// Returns the (possibly modified) context. **Caller is responsible for setting
/// the mode on the returned context** (CC doc).
pub fn transition_permission_mode(
    from_mode: &str,
    to_mode: &str,
    context: &ToolPermissionContext,
) -> ToolPermissionContext {
    // plan→plan (SDK set_permission_mode) would wrongly hit the leave branch below
    if from_mode == to_mode {
        return context.clone();
    }

    crate::bootstrap::state::handle_plan_mode_transition(from_mode, to_mode);
    crate::bootstrap::state::handle_auto_mode_transition(from_mode, to_mode);

    if from_mode == "plan" && to_mode != "plan" {
        crate::bootstrap::state::set_has_exited_plan_mode(true);
    }

    let mut context = context.clone();

    if is_transcript_classifier_feature_enabled() {
        if to_mode == "plan" && from_mode != "plan" {
            // Plan entry: prepareContextForPlanMode handles auto-during-plan
            return prepare_context_for_plan_mode(&context);
        }

        // Plan with auto active counts as using the classifier (for the leaving side).
        // isAutoModeActive() is the authoritative signal — prePlanMode/strippedDangerousRules
        // are unreliable proxies (CC comment).
        let from_uses_classifier = from_mode == "auto"
            || (from_mode == "plan"
                && crate::utils::permissions::auto_mode_state::is_auto_mode_active());
        // plan entry handled above
        let to_uses_classifier = to_mode == "auto";

        if to_uses_classifier && !from_uses_classifier {
            if !is_auto_mode_gate_enabled() {
                // CC throws; we keep context and skip activation (caller should
                // not offer Auto when gate is off via canCycleToAuto).
                return context;
            }
            crate::utils::permissions::auto_mode_state::set_auto_mode_active(true);
            context = strip_dangerous_permissions_for_auto_mode(&context);
        } else if from_uses_classifier && !to_uses_classifier {
            crate::utils::permissions::auto_mode_state::set_auto_mode_active(false);
            crate::bootstrap::state::set_needs_auto_mode_exit_attachment(true);
            context = restore_dangerous_permissions(&context);
        }
    }

    // Only clear prePlanMode when leaving plan (preserves ref equality intent).
    if from_mode == "plan" && to_mode != "plan" && context.pre_plan_mode.is_some() {
        context.pre_plan_mode = None;
    }

    context
}

/// Mark auto mode available on context when gate is open (startup / verify).
pub fn apply_auto_mode_availability(context: &ToolPermissionContext) -> ToolPermissionContext {
    let mut next = context.clone();
    next.is_auto_mode_available = Some(is_auto_mode_gate_enabled());
    next
}

/// Maps to: CC `utils/permissions/permissionSetup.ts` `prepareContextForPlanMode`.
///
/// Stashes `pre_plan_mode` and optionally keeps/activates auto classifier
/// semantics during plan (`shouldPlanUseAutoMode`).
pub fn prepare_context_for_plan_mode(context: &ToolPermissionContext) -> ToolPermissionContext {
    if context.mode == PermissionMode::Plan {
        return context.clone();
    }
    let current_mode = context.mode;

    if is_transcript_classifier_feature_enabled() {
        let plan_auto = should_plan_use_auto_mode();
        if current_mode == PermissionMode::Auto {
            if plan_auto {
                let mut next = context.clone();
                next.pre_plan_mode = Some(PermissionMode::Auto);
                return next;
            }
            crate::utils::permissions::auto_mode_state::set_auto_mode_active(false);
            crate::bootstrap::state::set_needs_auto_mode_exit_attachment(true);
            let mut next = restore_dangerous_permissions(context);
            next.pre_plan_mode = Some(PermissionMode::Auto);
            return next;
        }
        if plan_auto && current_mode != PermissionMode::BypassPermissions {
            crate::utils::permissions::auto_mode_state::set_auto_mode_active(true);
            let mut next = strip_dangerous_permissions_for_auto_mode(context);
            next.pre_plan_mode = Some(current_mode);
            return next;
        }
    }

    let mut next = context.clone();
    next.pre_plan_mode = Some(current_mode);
    next
}

/// Maps to: CC `utils/permissions/permissionSetup.ts:670-687` `isSymlinkTo`.
fn is_symlink_to(process_pwd: &std::path::Path, original_cwd: &std::path::Path) -> bool {
    let resolved = safe_resolve_path(get_fs_implementation().as_ref(), process_pwd);
    // Node path.resolve(originalCwd) is lexical: do not resolve its symlinks.
    let absolute_cwd =
        std::path::absolute(original_cwd).unwrap_or_else(|_| original_cwd.to_path_buf());
    let mut resolved_original_cwd = std::path::PathBuf::new();
    for component in absolute_cwd.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                resolved_original_cwd.pop();
            }
            other => resolved_original_cwd.push(other.as_os_str()),
        }
    }
    resolved.is_symlink && resolved.resolved_path == resolved_original_cwd
}

/// Maps to: CC `utils/permissions/permissionSetup.ts:887-892` anonymous return.
/// Rust names the source object to carry its context and ordered warnings;
/// dangerous-permission metadata remains in the existing startup mode owners.
#[derive(Clone, Debug)]
pub struct InitializeToolPermissionContextResult {
    pub tool_permission_context: ToolPermissionContext,
    pub warnings: Vec<String>,
}

/// Maps to: CC `utils/permissions/permissionSetup.ts:872-1033`
/// `initializeToolPermissionContext(...)`.
///
/// Rust startup injects already-loaded source-attributed settings/rules. Mode
/// resolution remains in its existing caller; directory validation is awaited
/// here, before a completed context can be returned.
pub async fn initialize_tool_permission_context(
    settings: &SettingsJson,
    loaded_rules: &[PermissionRule],
    allowed_tools_cli: &[String],
    disallowed_tools_cli: &[String],
    base_tools_cli: Option<&[String]>,
    add_dirs: &[String],
) -> anyhow::Result<InitializeToolPermissionContextResult> {
    use crate::commands::add_dir::validation::{
        AddDirectoryResult, add_dir_help_message, validate_directory_for_workspace,
    };
    let mut context = ToolPermissionContext::default();
    context.mode = settings
        .permissions
        .as_ref()
        .and_then(|permissions| permissions.default_mode.as_deref())
        .or(settings.default_permission_mode.as_deref())
        .map(permission_mode_from_string)
        .unwrap_or_default();

    let allowed = parse_tool_list_from_cli(allowed_tools_cli)
        .into_iter()
        .map(|rule| permission_rule_value_from_string(&rule))
        .collect::<Vec<_>>();
    context
        .always_allow_rules
        .insert(PermissionRuleSource::CliArg, allowed);
    let mut denied = parse_tool_list_from_cli(disallowed_tools_cli)
        .into_iter()
        .map(|rule| permission_rule_value_from_string(&rule))
        .collect::<Vec<_>>();
    // Maps to: CC `permissionSetup.ts:900-910` base-tools CLI branch. Keep
    // explicit disallows first, normalize legacy selected names, then append
    // omitted enabled base tools in canonical registry order.
    if let Some(base_tools_cli) = base_tools_cli.filter(|tools| !tools.is_empty()) {
        let base_tools_set = parse_base_tools_from_cli(base_tools_cli)
            .into_iter()
            .map(|name| normalize_legacy_tool_name(&name))
            .collect::<std::collections::HashSet<_>>();
        denied.extend(
            crate::tools::get_tools_for_default_preset()
                .into_iter()
                .filter(|name| !base_tools_set.contains(name))
                .map(|name| PermissionRuleValue::new(name, None)),
        );
    }
    context
        .always_deny_rules
        .insert(PermissionRuleSource::CliArg, denied);

    // Maps to: CC permissionSetup.ts:912-931. Keep the original logical PWD
    // spelling and the session source; validation below sees this initial map.
    if let Some(process_pwd) = crate::utils::process_env::var_os("PWD").as_deref().truthy() {
        let original_cwd = crate::bootstrap::state::get_original_cwd();
        let process_pwd_path = std::path::Path::new(&process_pwd);
        if process_pwd_path.as_os_str() != original_cwd.as_os_str()
            && is_symlink_to(process_pwd_path, &original_cwd)
        {
            let process_pwd = process_pwd.to_string_lossy().into_owned();
            context.additional_working_directories.insert(
                process_pwd.clone(),
                crate::types::permissions::AdditionalWorkingDirectory {
                    path: process_pwd,
                    source: PermissionRuleSource::Session,
                },
            );
        }
    }

    // Maps to: CC `utils/permissions/permissionSetup.ts:944-990` source-attributed disk load.
    // The merged `settings` value determines mode; rule ownership comes from
    // each canonical settings source so immutable flag/policy rules stay live.
    context =
        super::permissions::apply_permission_rules_to_permission_context(&context, loaded_rules);

    context.is_auto_mode_available = Some(is_auto_mode_gate_enabled());

    // Maps to: CC permissionSetup.ts:993-1025. Every validator receives the
    // same initial snapshot; only the application phase is cumulative.
    // PORTING.md A6: all tasks are started before awaiting. Dropping these
    // JoinHandles on the first rejection detaches, rather than cancels, sibling
    // stats, matching Promise.all. This is one startup operation, not detached
    // cross-query work; the owning runtime lives until startup settles. An A4
    // scratch runtime shutting down after a fatal error is the process-exit
    // boundary here; no permission changes occur until all validations succeed.
    use futures::StreamExt as _;
    let settings_directories = settings
        .permissions
        .as_ref()
        .and_then(|permissions| permissions.additional_directories.as_deref())
        .unwrap_or_default();
    let mut pending = futures::stream::FuturesUnordered::new();
    for (index, directory) in settings_directories.iter().chain(add_dirs).enumerate() {
        let directory = directory.clone();
        let initial_context = context.clone();
        pending.push(tokio::spawn(async move {
            (
                index,
                validate_directory_for_workspace(&directory, &initial_context).await,
            )
        }));
    }
    let mut results = vec![None; pending.len()];
    while let Some(completed) = pending.next().await {
        let (index, result) = completed?;
        results[index] = Some(result?);
    }
    let mut warnings = Vec::new();
    for result in results {
        let result = result.expect("all directory validations completed");
        match &result {
            AddDirectoryResult::Success { absolute_path } => {
                context = super::permission_update::apply_permission_update(
                    &context,
                    &PermissionUpdate::AddDirectories {
                        directories: vec![absolute_path.clone()],
                        destination: PermissionUpdateDestination::CliArg,
                    },
                );
            }
            AddDirectoryResult::AlreadyInWorkingDirectory { .. }
            | AddDirectoryResult::PathNotFound { .. } => {}
            _ => warnings.push(add_dir_help_message(&result)),
        }
    }
    Ok(InitializeToolPermissionContextResult {
        tool_permission_context: context,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::permissions::permissions::{
        has_in_memory_allow_rule, has_in_memory_ask_rule, has_in_memory_deny_rule,
    };
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[test]
    fn parse_tool_list_from_cli_preserves_parenthesized_rule_content() {
        assert_eq!(
            parse_tool_list_from_cli(&[
                "Read, Bash(git commit, git status)".to_string(),
                "Write NotebookEdit".to_string(),
            ]),
            vec![
                "Read".to_string(),
                "Bash(git commit, git status)".to_string(),
                "Write".to_string(),
                "NotebookEdit".to_string(),
            ]
        );
    }

    #[test]
    fn parse_base_tools_from_cli_matches_official_preset_custom_and_empty_inputs() {
        let _guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());

        let default_tools = parse_base_tools_from_cli(&[" DeFaUlT ".to_string()]);
        assert_eq!(default_tools, crate::tools::get_tools_for_default_preset());
        let selected = default_tools
            .iter()
            .map(String::as_str)
            .collect::<std::collections::HashSet<_>>();
        assert!(
            crate::tools::get_tools(&ToolPermissionContext::default())
                .iter()
                .all(|tool| selected.contains(tool.name.as_str())),
            "the default preset must be a no-op for the active built-in pool"
        );

        assert_eq!(
            parse_base_tools_from_cli(&["Read, Bash(git commit, git status)".to_string()]),
            vec![
                "Read".to_string(),
                "Bash(git commit, git status)".to_string(),
            ]
        );
        assert!(parse_base_tools_from_cli(&[String::new()]).is_empty());
    }

    #[tokio::test]
    async fn initialize_tool_permission_context_base_tools_matches_official_normalization_source_order_and_precedence()
     {
        let _guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let loaded_rules = vec![PermissionRule {
            source: PermissionRuleSource::UserSettings,
            rule_behavior: PermissionBehavior::Allow,
            rule_value: PermissionRuleValue::new("Bash", None),
        }];
        let explicit_disallowed = vec!["Write".to_string()];
        let base_tools = vec!["Read,Task".to_string()];
        let context = initialize_tool_permission_context(
            &SettingsJson::default(),
            &loaded_rules,
            &[],
            &explicit_disallowed,
            Some(&base_tools),
            &[],
        )
        .await
        .expect("permission setup")
        .tool_permission_context;

        let selected = ["Read", crate::tools::agent_tool::constants::AGENT_TOOL_NAME]
            .into_iter()
            .collect::<std::collections::HashSet<_>>();
        let expected_cli_denies = std::iter::once(PermissionRuleValue::new("Write", None))
            .chain(
                crate::tools::get_tools_for_default_preset()
                    .into_iter()
                    .filter(|name| !selected.contains(name.as_str()))
                    .map(|name| PermissionRuleValue::new(name, None)),
            )
            .collect::<Vec<_>>();
        assert_eq!(
            context.always_deny_rules[&PermissionRuleSource::CliArg],
            expected_cli_denies
        );
        assert!(has_in_memory_allow_rule(
            &context,
            &PermissionRuleValue::new("Bash", None)
        ));

        let expected_active = crate::tools::get_tools(&ToolPermissionContext::default())
            .into_iter()
            .filter(|tool| selected.contains(tool.name.as_str()))
            .map(|tool| tool.name)
            .collect::<Vec<_>>();
        let active = crate::tools::get_tools(&context)
            .into_iter()
            .map(|tool| tool.name)
            .collect::<Vec<_>>();
        assert_eq!(active, expected_active);
    }

    #[tokio::test]
    async fn initialize_tool_permission_context_empty_and_default_base_tools_match_official_boundaries()
     {
        let _guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let baseline =
            initialize_tool_permission_context(&SettingsJson::default(), &[], &[], &[], None, &[])
                .await
                .expect("permission setup")
                .tool_permission_context;
        let default_cli = vec!["default".to_string()];
        let default_selected = initialize_tool_permission_context(
            &SettingsJson::default(),
            &[],
            &[],
            &[],
            Some(&default_cli),
            &[],
        )
        .await
        .expect("permission setup")
        .tool_permission_context;
        assert_eq!(
            default_selected.always_deny_rules[&PermissionRuleSource::CliArg],
            baseline.always_deny_rules[&PermissionRuleSource::CliArg]
        );
        assert_eq!(
            crate::tools::get_tools(&default_selected),
            crate::tools::get_tools(&baseline)
        );

        let empty_cli = vec![String::new()];
        let disabled = initialize_tool_permission_context(
            &SettingsJson::default(),
            &[],
            &[],
            &[],
            Some(&empty_cli),
            &[],
        )
        .await
        .expect("permission setup")
        .tool_permission_context;
        let expected_denies = crate::tools::get_tools_for_default_preset()
            .into_iter()
            .map(|name| PermissionRuleValue::new(name, None))
            .collect::<Vec<_>>();
        assert_eq!(
            disabled.always_deny_rules[&PermissionRuleSource::CliArg],
            expected_denies
        );
        assert!(crate::tools::get_tools(&disabled).is_empty());
    }

    #[tokio::test]
    async fn initialize_tool_permission_context_maps_default_mode_and_rules() {
        let settings = SettingsJson {
            default_permission_mode: Some("default".to_string()),
            permissions: Some(crate::utils::settings::types::PermissionsSettings {
                allow: Some(vec!["Bash(cargo test)".to_string(), "Read".to_string()]),
                deny: Some(vec!["Write(src/secret.txt)".to_string()]),
                ask: Some(vec!["PowerShell(Get-Process)".to_string()]),
                default_mode: Some("acceptEdits".to_string()),
                ..Default::default()
            }),
            ..SettingsJson::default()
        };

        let rules = crate::utils::permissions::permissions_loader::settings_json_to_rules(
            Some(&settings),
            PermissionRuleSource::UserSettings,
        );
        let context = initialize_tool_permission_context(&settings, &rules, &[], &[], None, &[])
            .await
            .expect("permission setup")
            .tool_permission_context;

        assert_eq!(context.mode, PermissionMode::AcceptEdits);
        assert!(has_in_memory_allow_rule(
            &context,
            &PermissionRuleValue::new("Bash", Some("cargo test".to_string()))
        ));
        assert!(has_in_memory_allow_rule(
            &context,
            &PermissionRuleValue::new("Read", Some("README.md".to_string()))
        ));
        assert!(has_in_memory_deny_rule(
            &context,
            &PermissionRuleValue::new("Write", Some("src/secret.txt".to_string()))
        ));
        assert!(has_in_memory_ask_rule(
            &context,
            &PermissionRuleValue::new("PowerShell", Some("Get-Process".to_string()))
        ));
    }

    #[tokio::test]
    async fn initialize_tool_permission_context_defers_auto_transition_until_rules_are_complete() {
        crate::utils::permissions::auto_mode_state::reset_for_testing();
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("COMETIX_TRANSCRIPT_CLASSIFIER", "1");
        let settings = SettingsJson {
            permissions: Some(crate::utils::settings::types::PermissionsSettings {
                allow: Some(vec!["Bash(python:*)".to_string(), "Read".to_string()]),
                default_mode: Some("auto".to_string()),
                ..Default::default()
            }),
            ..SettingsJson::default()
        };
        if is_auto_mode_gate_enabled() {
            let rules = crate::utils::permissions::permissions_loader::settings_json_to_rules(
                Some(&settings),
                PermissionRuleSource::UserSettings,
            );
            let context =
                initialize_tool_permission_context(&settings, &rules, &[], &[], None, &[])
                    .await
                    .expect("permission setup")
                    .tool_permission_context;
            assert_eq!(context.mode, PermissionMode::Auto);
            assert!(!crate::utils::permissions::auto_mode_state::is_auto_mode_active());
            assert!(has_in_memory_allow_rule(
                &context,
                &PermissionRuleValue::new("Bash", Some("python:*".to_string()))
            ));
            assert!(has_in_memory_allow_rule(
                &context,
                &PermissionRuleValue::new("Read", None)
            ));
        }
        crate::utils::process_env::remove("COMETIX_TRANSCRIPT_CLASSIFIER");
        crate::utils::permissions::auto_mode_state::reset_for_testing();
    }

    #[test]
    fn initial_permission_mode_from_cli_auto_sets_active_when_gate_open() {
        crate::utils::permissions::auto_mode_state::reset_for_testing();
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("COMETIX_TRANSCRIPT_CLASSIFIER", "1");
        if is_auto_mode_gate_enabled() {
            let (mode, _) = initial_permission_mode_from_cli(Some("auto"), false);
            assert_eq!(mode, PermissionMode::Auto);
            assert!(crate::utils::permissions::auto_mode_state::is_auto_mode_active());
        }
        crate::utils::process_env::remove("COMETIX_TRANSCRIPT_CLASSIFIER");
        crate::utils::permissions::auto_mode_state::reset_for_testing();
    }

    #[test]
    fn is_default_permission_mode_auto_reads_settings() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("COMETIX_TRANSCRIPT_CLASSIFIER", "1");
        let settings = SettingsJson {
            permissions: Some(crate::utils::settings::types::PermissionsSettings {
                default_mode: Some("auto".to_string()),
                ..Default::default()
            }),
            ..SettingsJson::default()
        };
        assert!(is_default_permission_mode_auto(&settings));
        crate::utils::process_env::remove("COMETIX_TRANSCRIPT_CLASSIFIER");
    }

    #[test]
    fn dangerous_bash_and_powershell_rules_match_official_shapes() {
        assert!(is_dangerous_bash_permission("Bash", None));
        assert!(is_dangerous_bash_permission("Bash", Some("python:*")));
        assert!(is_dangerous_bash_permission("Bash", Some("node -*")));
        assert!(!is_dangerous_bash_permission("Bash", Some("git status")));
        assert!(is_dangerous_powershell_permission("PowerShell", None));
        assert!(is_dangerous_powershell_permission(
            "PowerShell",
            Some("iex:*")
        ));
        assert!(is_dangerous_powershell_permission(
            "PowerShell",
            Some("python.exe -*")
        ));
        assert!(!is_dangerous_powershell_permission(
            "PowerShell",
            Some("Get-ChildItem")
        ));
        assert!(is_dangerous_task_permission("Task", None));
    }

    #[test]
    fn finds_removes_strips_and_restores_dangerous_classifier_permissions() {
        let rules = vec![
            PermissionRule {
                source: PermissionRuleSource::Session,
                rule_behavior: PermissionBehavior::Allow,
                rule_value: PermissionRuleValue::new("Bash", Some("python:*".to_string())),
            },
            PermissionRule {
                source: PermissionRuleSource::PolicySettings,
                rule_behavior: PermissionBehavior::Allow,
                rule_value: PermissionRuleValue::new("PowerShell", None),
            },
        ];
        let dangerous = find_dangerous_classifier_permissions(&rules, &["Agent".to_string()]);
        assert_eq!(dangerous.len(), 3);
        assert_eq!(dangerous[0].rule_display, "Bash(python:*)");
        assert_eq!(dangerous[2].source_display, "--allowed-tools");

        let mut context = ToolPermissionContext::default();
        let bash_rule = PermissionRuleValue::new("Bash", Some("python:*".to_string()));
        let read_rule = PermissionRuleValue::new("Read", None);
        context.always_allow_rules.insert(
            PermissionRuleSource::Session,
            vec![bash_rule.clone(), read_rule.clone()],
        );
        let stripped = strip_dangerous_permissions_for_auto_mode(&context);
        assert!(!has_in_memory_allow_rule(&stripped, &bash_rule));
        assert!(has_in_memory_allow_rule(&stripped, &read_rule));
        assert!(
            stripped
                .stripped_dangerous_rules
                .as_ref()
                .is_some_and(|rules| rules
                    .get(&PermissionRuleSource::Session)
                    .is_some_and(|rules| rules.contains(&bash_rule)))
        );

        let restored = restore_dangerous_permissions(&stripped);
        assert!(has_in_memory_allow_rule(&restored, &bash_rule));
        assert!(restored.stripped_dangerous_rules.is_none());
    }

    #[test]
    fn transition_plan_auto_mode_matches_official_settings_reload_activation_and_restrip() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let _provider_vars = [
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
            "CLAUDE_CODE_USE_FOUNDRY",
        ]
        .map(EnvVarGuard::unset);
        let root = std::env::temp_dir().join(format!(
            "cometix-plan-auto-reload-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("settings.json"),
            r#"{"skipAutoPermissionPrompt":true,"useAutoModeDuringPlan":true}"#,
        )
        .unwrap();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root);
        crate::bootstrap::state::set_main_loop_model_override(Some(Some(
            "claude-sonnet-4-6".to_string(),
        )));
        crate::utils::permissions::auto_mode_state::reset_for_testing();

        let dangerous = PermissionRuleValue::new("Bash", Some("python:*".to_string()));
        let mut context = ToolPermissionContext {
            mode: PermissionMode::Plan,
            pre_plan_mode: Some(PermissionMode::Default),
            ..ToolPermissionContext::default()
        };
        context
            .always_allow_rules
            .insert(PermissionRuleSource::UserSettings, vec![dangerous.clone()]);
        let transitioned = transition_plan_auto_mode(&context);

        assert!(crate::utils::permissions::auto_mode_state::is_auto_mode_active());
        assert!(!has_in_memory_allow_rule(&transitioned, &dangerous));
        assert!(
            transitioned
                .stripped_dangerous_rules
                .as_ref()
                .is_some_and(
                    |by_source| by_source[&PermissionRuleSource::UserSettings].contains(&dangerous)
                )
        );

        crate::utils::permissions::auto_mode_state::reset_for_testing();
        crate::bootstrap::state::set_main_loop_model_override(None);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn auto_strip_stashes_only_mutable_sources_and_leaves_immutable_rules_live() {
        let dangerous = PermissionRuleValue::new("Bash", Some("python:*".to_string()));
        let mut context = ToolPermissionContext::default();
        for source in [
            PermissionRuleSource::UserSettings,
            PermissionRuleSource::ProjectSettings,
            PermissionRuleSource::LocalSettings,
            PermissionRuleSource::CliArg,
            PermissionRuleSource::Session,
            PermissionRuleSource::FlagSettings,
            PermissionRuleSource::PolicySettings,
            PermissionRuleSource::Command,
        ] {
            context
                .always_allow_rules
                .insert(source, vec![dangerous.clone()]);
        }

        let stripped = strip_dangerous_permissions_for_auto_mode(&context);
        for source in [
            PermissionRuleSource::UserSettings,
            PermissionRuleSource::ProjectSettings,
            PermissionRuleSource::LocalSettings,
            PermissionRuleSource::CliArg,
            PermissionRuleSource::Session,
        ] {
            assert!(stripped.always_allow_rules[&source].is_empty());
            assert!(
                stripped.stripped_dangerous_rules.as_ref().unwrap()[&source].contains(&dangerous)
            );
        }
        for source in [
            PermissionRuleSource::FlagSettings,
            PermissionRuleSource::PolicySettings,
            PermissionRuleSource::Command,
        ] {
            assert!(stripped.always_allow_rules[&source].contains(&dangerous));
            assert!(
                !stripped
                    .stripped_dangerous_rules
                    .as_ref()
                    .unwrap()
                    .contains_key(&source)
            );
        }
    }

    #[tokio::test]
    async fn managed_only_startup_matches_official_source_attributed_loader_boundary() {
        let merged_settings = SettingsJson {
            permissions: Some(crate::utils::settings::types::PermissionsSettings {
                allow: Some(vec!["Edit".to_string()]),
                ..Default::default()
            }),
            ..SettingsJson::default()
        };
        let loaded_policy_rule = PermissionRule {
            source: PermissionRuleSource::PolicySettings,
            rule_behavior: PermissionBehavior::Allow,
            rule_value: PermissionRuleValue::new("Read", None),
        };

        let context = initialize_tool_permission_context(
            &merged_settings,
            &[loaded_policy_rule],
            &[],
            &[],
            None,
            &[],
        )
        .await
        .expect("permission setup")
        .tool_permission_context;

        assert!(
            context
                .always_allow_rules
                .get(&PermissionRuleSource::UserSettings)
                .is_none_or(Vec::is_empty)
        );
        assert_eq!(
            context.always_allow_rules[&PermissionRuleSource::PolicySettings],
            vec![PermissionRuleValue::new("Read", None)]
        );
    }

    #[tokio::test]
    async fn cli_lists_create_source_shaped_empty_buckets() {
        let context =
            initialize_tool_permission_context(&SettingsJson::default(), &[], &[], &[], None, &[])
                .await
                .expect("permission setup")
                .tool_permission_context;
        assert_eq!(
            context
                .always_allow_rules
                .get(&PermissionRuleSource::CliArg),
            Some(&Vec::new())
        );
        assert_eq!(
            context.always_deny_rules.get(&PermissionRuleSource::CliArg),
            Some(&Vec::new())
        );
    }

    #[test]
    fn finds_overly_broad_shell_permissions_from_rules_and_cli() {
        let rules = vec![PermissionRule {
            source: PermissionRuleSource::UserSettings,
            rule_behavior: PermissionBehavior::Allow,
            rule_value: PermissionRuleValue::new("Bash", None),
        }];
        let broad = find_overly_broad_bash_permissions(&rules, &["PowerShell".to_string()]);
        assert_eq!(broad.len(), 1);
        assert_eq!(broad[0].rule_display, "Bash(*)");
        let ps = find_overly_broad_powershell_permissions(&[], &["PowerShell".to_string()]);
        assert_eq!(ps.len(), 1);
        assert_eq!(ps[0].source, PermissionRuleSource::CliArg);
    }

    #[test]
    fn disabled_bypass_context_exits_bypass_mode_and_hides_availability() {
        let context = ToolPermissionContext {
            mode: PermissionMode::BypassPermissions,
            is_bypass_permissions_mode_available: true,
            ..ToolPermissionContext::default()
        };
        let disabled = create_disabled_bypass_permissions_context(&context);
        assert_eq!(disabled.mode, PermissionMode::Default);
        assert!(!disabled.is_bypass_permissions_mode_available);
    }
}

#[cfg(test)]
mod directory_tests {
    //! Maps to CC `utils/permissions/permissionSetup.ts:912-931,993-1033`.

    use super::*;
    use crate::utils::settings::types::PermissionsSettings;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};
    use std::path::{Path, PathBuf};

    struct DirectoryFixture {
        root: PathBuf,
        previous_original_cwd: PathBuf,
    }

    impl DirectoryFixture {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("permission-setup-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(root.join("workspace")).unwrap();
            let root = root.canonicalize().unwrap();
            let previous_original_cwd = crate::bootstrap::state::get_original_cwd();
            crate::bootstrap::state::set_original_cwd(root.join("workspace"));
            Self {
                root,
                previous_original_cwd,
            }
        }

        fn directory(&self, relative: &str) -> String {
            let path = self.root.join(relative);
            std::fs::create_dir_all(&path).unwrap();
            path.to_string_lossy().into_owned()
        }

        fn file(&self, relative: &str) -> String {
            let path = self.root.join(relative);
            std::fs::write(&path, "file").unwrap();
            path.to_string_lossy().into_owned()
        }
    }

    impl Drop for DirectoryFixture {
        fn drop(&mut self) {
            crate::bootstrap::state::set_original_cwd(&self.previous_original_cwd);
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[tokio::test]
    async fn startup_directories_matches_official_initial_snapshot_and_map_insertion_order() {
        // CC permissionSetup.ts:993-1013: overlapping inputs all validate against
        // the initial context, while repeat Map.set preserves the first key position.
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let fixture = DirectoryFixture::new();
        let _pwd = EnvVarGuard::unset("PWD");
        let parent = fixture.directory("z-parent");
        let child = fixture.directory("z-parent/child");
        let sibling = fixture.directory("a-sibling");
        let settings = SettingsJson {
            permissions: Some(PermissionsSettings {
                additional_directories: Some(vec![parent.clone(), child.clone()]),
                ..Default::default()
            }),
            ..Default::default()
        };
        let result = initialize_tool_permission_context(
            &settings,
            &[],
            &[],
            &[],
            None,
            &[format!("{parent}/"), sibling.clone(), child.clone()],
        )
        .await
        .unwrap();
        assert!(result.warnings.is_empty());
        let entries = result
            .tool_permission_context
            .additional_working_directories;
        assert_eq!(
            entries.keys().cloned().collect::<Vec<_>>(),
            vec![parent, child, sibling]
        );
        assert!(entries.iter().all(|(path, entry)| {
            entry.path == *path && entry.source == PermissionRuleSource::CliArg
        }));
    }

    #[tokio::test]
    async fn startup_warnings_matches_official_input_order_and_silent_missing_or_covered_paths() {
        // CC permissionSetup.ts:1014-1025: file/empty warn; missing and already
        // covered paths are silent, even when interspersed across settings and CLI.
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let fixture = DirectoryFixture::new();
        let _pwd = EnvVarGuard::unset("PWD");
        let _color = EnvVarGuard::set("FORCE_COLOR", "0");
        let first_file = fixture.file("first.txt");
        let second_file = fixture.file("second.txt");
        let missing = fixture.root.join("missing").to_string_lossy().into_owned();
        let covered = fixture.directory("workspace/covered");
        let settings = SettingsJson {
            permissions: Some(PermissionsSettings {
                additional_directories: Some(vec![first_file.clone(), missing, String::new()]),
                ..Default::default()
            }),
            ..Default::default()
        };
        let result = initialize_tool_permission_context(
            &settings,
            &[],
            &[],
            &[],
            None,
            &[covered, second_file.clone()],
        )
        .await
        .unwrap();
        assert_eq!(
            result.warnings,
            vec![
                format!(
                    "{first_file} is not a directory. Did you mean to add the parent directory {}?",
                    fixture.root.display()
                ),
                "Please provide a directory path.".to_string(),
                format!(
                    "{second_file} is not a directory. Did you mean to add the parent directory {}?",
                    fixture.root.display()
                ),
            ]
        );
        assert!(
            result
                .tool_permission_context
                .additional_working_directories
                .is_empty()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn startup_pwd_matches_official_symlink_session_identity_before_cli_validation() {
        // CC permissionSetup.ts:670-687,912-931: only a differing PWD resolving to
        // originalCwd is seeded, under its literal logical spelling and session source.
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let fixture = DirectoryFixture::new();
        let real = fixture.root.join("workspace");
        let logical = fixture.root.join("logical-workspace");
        std::os::unix::fs::symlink(&real, &logical).unwrap();
        let other = fixture.directory("other");
        assert!(is_symlink_to(&logical, &real));
        assert!(!is_symlink_to(&real, &real));
        assert!(!is_symlink_to(Path::new(&other), &real));
        assert!(!is_symlink_to(&logical, Path::new(&other)));
        let _pwd = EnvVarGuard::set("PWD", &logical);
        let result = initialize_tool_permission_context(
            &SettingsJson::default(),
            &[],
            &[],
            &[],
            None,
            &[logical.to_string_lossy().into_owned(), other.clone()],
        )
        .await
        .unwrap();
        let entries = result
            .tool_permission_context
            .additional_working_directories;
        let logical = logical.to_string_lossy().into_owned();
        assert_eq!(
            entries.keys().cloned().collect::<Vec<_>>(),
            vec![logical.clone(), other.clone()]
        );
        assert_eq!(entries[&logical].source, PermissionRuleSource::Session);
        assert_eq!(entries[&other].source, PermissionRuleSource::CliArg);
        assert!(result.warnings.is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn startup_validation_matches_official_fatal_stat_error_propagation() {
        // CC permissionSetup.ts:1001-1005 awaits Promise.all before emitting
        // warnings or installing any successful updates. validation.ts:56-75 throws ELOOP.
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let fixture = DirectoryFixture::new();
        let _pwd = EnvVarGuard::unset("PWD");
        let valid = fixture.directory("valid");
        let cycle = fixture.root.join("cycle");
        std::os::unix::fs::symlink(&cycle, &cycle).unwrap();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            initialize_tool_permission_context(
                &SettingsJson::default(),
                &[],
                &[],
                &[],
                None,
                &[valid, String::new(), cycle.to_string_lossy().into_owned()],
            ),
        )
        .await
        .expect("startup validation must settle");
        let error = result.expect_err("fatal errno must propagate rather than becoming a warning");
        assert_eq!(crate::utils::errors::get_errno_code(&error), Some("ELOOP"));
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn startup_pwd_matches_official_nfc_original_cwd() {
        // CC bootstrap/state.ts:516 stores NFC; FsOperations.realpathSync:523-525
        // returns NFC even when a macOS directory was created with decomposed bytes.
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let fixture = DirectoryFixture::new();
        let decomposed = fixture.directory("cafe\u{301}");
        crate::bootstrap::state::set_original_cwd(&decomposed);
        let expected = fixture.root.join("caf\u{e9}");
        assert_eq!(
            crate::bootstrap::state::get_original_cwd().as_os_str(),
            expected.as_os_str()
        );
        let logical = fixture.root.join("logical-unicode");
        std::os::unix::fs::symlink(&decomposed, &logical).unwrap();
        let _pwd = EnvVarGuard::set("PWD", &logical);
        let result =
            initialize_tool_permission_context(&SettingsJson::default(), &[], &[], &[], None, &[])
                .await
                .unwrap();
        let key = logical.to_string_lossy().into_owned();
        assert_eq!(
            result
                .tool_permission_context
                .additional_working_directories[&key]
                .source,
            PermissionRuleSource::Session
        );
    }
}

#[cfg(test)]
mod gate_tests {
    use super::*;
    use crate::utils::settings::{settings_cache, validation::SettingsWithErrors};
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    fn settings(value: serde_json::Value) {
        settings_cache::set_session_settings_cache(SettingsWithErrors {
            settings: serde_json::from_value(value).unwrap(),
            errors: vec![],
            policy_settings: None,
        });
    }

    #[test]
    fn auto_mode_cached_state_matches_official_cold_cache_vs_fetched_null() {
        // permissionSetup.ts:1315-1352: only the absent Symbol allows deferral.
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _telemetry = EnvVarGuard::unset("DISABLE_TELEMETRY");
        let _traffic = EnvVarGuard::unset("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC");
        let _node = EnvVarGuard::unset("NODE_ENV");
        crate::services::analytics::growthbook::reset_growth_book();
        for (value, expected) in [
            (None, None),
            (
                Some(serde_json::Value::Null),
                Some(AutoModeEnabledState::Disabled),
            ),
            (
                Some(serde_json::json!({})),
                Some(AutoModeEnabledState::Disabled),
            ),
            (
                Some(serde_json::json!({"enabled":"enabled"})),
                Some(AutoModeEnabledState::Enabled),
            ),
            (
                Some(serde_json::json!({"enabled":"opt-in"})),
                Some(AutoModeEnabledState::OptIn),
            ),
            (
                Some(serde_json::json!({"enabled":true})),
                Some(AutoModeEnabledState::Disabled),
            ),
        ] {
            let mut config = crate::utils::config::GlobalConfig::default();
            config.cached_growth_book_features =
                value.map(|v| HashMap::from([("tengu_auto_mode_config".into(), v)]));
            crate::utils::config::set_test_global_config(Some(config));
            assert_eq!(get_auto_mode_enabled_state_if_cached(), expected);
        }
        crate::utils::config::set_test_global_config(None);
    }

    #[test]
    fn initial_mode_matches_official_policy_priority_and_nested_auto_setting() {
        // permissionSetup.ts:699-811,1270-1280.
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _telemetry = EnvVarGuard::unset("DISABLE_TELEMETRY");
        let _traffic = EnvVarGuard::unset("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC");
        let _node = EnvVarGuard::unset("NODE_ENV");
        crate::services::analytics::growthbook::reset_growth_book();
        settings(
            serde_json::json!({"permissions":{"defaultMode":"plan","disableAutoMode":"disable"}}),
        );
        assert!(is_auto_mode_disabled_by_settings());
        let mut config = crate::utils::config::GlobalConfig::default();
        config.cached_growth_book_features = Some(HashMap::from([
            (
                "tengu_disable_bypass_permissions_mode".into(),
                serde_json::json!(true),
            ),
            (
                "tengu_auto_mode_config".into(),
                serde_json::json!({"enabled":"disabled"}),
            ),
        ]));
        crate::utils::config::set_test_global_config(Some(config));
        assert_eq!(
            initial_permission_mode_from_cli(Some("auto"), true),
            (
                PermissionMode::Plan,
                Some("Bypass permissions mode was disabled by your organization policy".into())
            )
        );
        settings(serde_json::json!({"disableAutoMode":"true"}));
        assert!(!is_auto_mode_disabled_by_settings());
        crate::utils::config::set_test_global_config(None);
    }

    #[tokio::test]
    async fn auto_gate_matches_official_fresh_context_kickout_and_identity() {
        // permissionSetup.ts:1183-1261: notification uses old intent; mutations use fresh ctx.
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _overrides = EnvVarGuard::unset("CLAUDE_INTERNAL_FC_OVERRIDES");
        crate::services::analytics::growthbook::reset_growth_book();
        crate::utils::config::set_test_global_config(Some(Default::default()));
        settings(serde_json::json!({"permissions":{"disableAutoMode":"disable"}}));
        super::super::auto_mode_state::reset_for_testing();
        let old = ToolPermissionContext::default();
        let result = verify_auto_mode_gate_access(&old, None).await;
        assert!(result.notification.is_none());
        let fresh = Arc::new(ToolPermissionContext {
            mode: PermissionMode::Auto,
            ..Default::default()
        });
        super::super::auto_mode_state::set_auto_mode_active(true);
        let next = (result.update_context)(&fresh);
        assert_eq!(next.mode, PermissionMode::Default);
        assert_eq!(next.is_auto_mode_available, Some(false));
        assert!(!super::super::auto_mode_state::is_auto_mode_active());
        assert!(crate::bootstrap::state::needs_auto_mode_exit_attachment());
        assert!(Arc::ptr_eq(&next, &(result.update_context)(&next)));
        let result = verify_auto_mode_gate_access(&fresh, None).await;
        assert_eq!(
            result.notification,
            Some(get_auto_mode_unavailable_notification(
                AutoModeUnavailableReason::Settings
            ))
        );
        let accept = Arc::new(ToolPermissionContext {
            mode: PermissionMode::AcceptEdits,
            ..Default::default()
        });
        crate::bootstrap::state::set_needs_auto_mode_exit_attachment(false);
        assert_eq!(
            (result.update_context)(&accept).mode,
            PermissionMode::AcceptEdits
        );
        assert!(!crate::bootstrap::state::needs_auto_mode_exit_attachment());
        let plan = Arc::new(ToolPermissionContext {
            mode: PermissionMode::Plan,
            pre_plan_mode: Some(PermissionMode::Auto),
            ..Default::default()
        });
        let next = (result.update_context)(&plan);
        assert_eq!(next.mode, PermissionMode::Plan);
        assert_eq!(next.pre_plan_mode, Some(PermissionMode::Default));
        super::super::auto_mode_state::reset_for_testing();
        crate::utils::config::set_test_global_config(None);
    }

    #[cfg(feature = "anthropic_internal")]
    #[tokio::test]
    async fn auto_gate_matches_official_opt_in_carousel_and_runtime_fast_breaker() {
        // permissionSetup.ts:1091-1141: opt-in allows explicit entry, but not carousel without consent.
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _overrides = EnvVarGuard::set(
            "CLAUDE_INTERNAL_FC_OVERRIDES",
            r#"{"tengu_auto_mode_config":{"enabled":"opt-in","disableFastMode":true}}"#,
        );
        crate::services::analytics::growthbook::reset_growth_book();
        settings(serde_json::json!({}));
        super::super::auto_mode_state::reset_for_testing();
        let old_model = crate::bootstrap::state::get_main_loop_model_override();
        crate::bootstrap::state::set_main_loop_model_override(Some(Some(
            "claude-sonnet-4-6".into(),
        )));
        let ctx = Arc::new(ToolPermissionContext {
            mode: PermissionMode::Auto,
            ..Default::default()
        });
        let result = verify_auto_mode_gate_access(&ctx, Some(false)).await;
        let next = (result.update_context)(&ctx);
        assert_eq!(next.mode, PermissionMode::Auto);
        assert_eq!(next.is_auto_mode_available, Some(false));
        super::super::auto_mode_state::set_auto_mode_flag_cli(true);
        let result = verify_auto_mode_gate_access(&ctx, Some(false)).await;
        assert_eq!(
            (result.update_context)(&ctx).is_auto_mode_available,
            Some(true)
        );
        let result = verify_auto_mode_gate_access(&ctx, Some(true)).await;
        assert_eq!((result.update_context)(&ctx).mode, PermissionMode::Default);
        assert!(
            !super::super::auto_mode_state::is_auto_mode_circuit_broken(),
            "fast breaker is model eligibility, not global disabled"
        );
        crate::bootstrap::state::set_main_loop_model_override(old_model);
        crate::services::analytics::growthbook::reset_growth_book();
        super::super::auto_mode_state::reset_for_testing();
    }
}
