//! Effort helpers.
//! Maps to CC `utils/effort.ts`.

use crate::utils::ultracode::{catalog_entry, normalize_model_name};

use crate::constants::figures::{EFFORT_HIGH, EFFORT_LOW, EFFORT_MAX, EFFORT_MEDIUM, EFFORT_XHIGH};
use crate::utils::model::model_support_overrides::{
    ModelCapabilityOverride, get_3p_model_capability_override,
};
use serde::{Deserialize, Serialize};

/// Maps to CC `utils/effort.ts:11` `EffortLevel`, the enumerated half of the
/// level strings in `EFFORT_LEVELS`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ModelEffortLevel {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl ModelEffortLevel {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }

    pub(crate) fn title_label(self) -> &'static str {
        match self {
            Self::Low => "Low",
            Self::Medium => "Medium",
            Self::High => "High",
            Self::Xhigh => "xHigh",
            Self::Max => "Max",
        }
    }

    pub(crate) fn symbol(self) -> &'static str {
        match self {
            Self::Low => EFFORT_LOW,
            Self::Medium => EFFORT_MEDIUM,
            Self::High => EFFORT_HIGH,
            Self::Xhigh => EFFORT_XHIGH,
            Self::Max => EFFORT_MAX,
        }
    }
}

/// Maps to CC `utils/effort.ts` `EFFORT_LEVELS`, widened with 2.1.198 `xhigh`.
pub const EFFORT_LEVELS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

/// Maps to CC `utils/effort.ts` `EffortValue`.
/// Can be "high" | "medium" | "low" | "max" or an integer override.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EffortValue {
    Named(String),
    Numeric(i64),
}

impl EffortValue {
    pub fn as_str(&self) -> String {
        match self {
            Self::Named(value) => value.clone(),
            Self::Numeric(value) => value.to_string(),
        }
    }
}

/// Maps to CC `utils/effort.ts#OpusDefaultEffortConfig`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpusDefaultEffortConfig {
    pub enabled: bool,
    pub dialog_title: String,
    pub dialog_description: String,
}

/// Maps to CC `utils/effort.ts#OPUS_DEFAULT_EFFORT_CONFIG_DEFAULT`.
pub fn opus_default_effort_config_default() -> OpusDefaultEffortConfig {
    OpusDefaultEffortConfig {
        enabled: true,
        dialog_title: "We recommend medium effort for Opus".to_string(),
        dialog_description: "Effort determines how long Claude thinks for when completing your task. We recommend medium effort for most tasks to balance speed and intelligence and maximize rate limits. Use ultrathink to trigger high effort when needed.".to_string(),
    }
}

/// Maps to CC `utils/effort.ts#getOpusDefaultEffortConfig`.
///
/// GrowthBook override lookup is intentionally not performed in Cometix; callers
/// receive the official default config unless a future feature-flag service
/// supplies an override at this same boundary.
pub fn get_opus_default_effort_config() -> OpusDefaultEffortConfig {
    opus_default_effort_config_default()
}

/// Maps to CC `utils/effort.ts#isEffortLevel`.
pub fn is_effort_level(value: &str) -> bool {
    EFFORT_LEVELS.contains(&value)
}

/// Maps to CC `utils/effort.ts#parseEffortValue`.
pub fn parse_effort_value(value: &str) -> Option<EffortValue> {
    if value.is_empty() {
        return None;
    }
    // CC does not trim before checking named levels. JavaScript `parseInt`
    // below still accepts leading whitespace for numeric values.
    let lower = value.to_lowercase();
    if is_effort_level(&lower) {
        return Some(EffortValue::Named(lower));
    }
    let numeric = parse_js_parse_int_base10(&lower)?;
    is_valid_numeric_effort(numeric).then_some(EffortValue::Numeric(numeric))
}

/// Maps to CC `utils/effort.ts#isValidNumericEffort`.
pub fn is_valid_numeric_effort(_value: i64) -> bool {
    true
}

/// Maps to CC 2.1.198 `iv` / `modelSupportsEffortExtended`.
pub fn model_supports_effort(model: &str) -> bool {
    if let Some(value) = get_3p_model_capability_override(model, ModelCapabilityOverride::Effort) {
        return value;
    }
    let n = normalize_model_name(model);
    if n.contains("claude-3-")
        || matches!(
            n.as_str(),
            "claude-opus-4-0"
                | "claude-opus-4-1"
                | "claude-sonnet-4-0"
                | "claude-sonnet-4-5"
                | "claude-haiku-4-5"
        )
    {
        return false;
    }
    if crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_ALWAYS_ENABLE_EFFORT")
            .as_deref(),
    ) {
        return true;
    }
    catalog_entry(&n).is_some_and(|entry| entry.capabilities.contains(&"effort"))
        || n == "claude-mythos-5"
        || provider_supports_effort_default()
}

/// Maps to CC 2.1.198 `$0e` / `modelSupportsMaxEffortExtended`.
pub fn model_supports_max_effort(model: &str) -> bool {
    if let Some(value) = get_3p_model_capability_override(model, ModelCapabilityOverride::MaxEffort)
    {
        return value;
    }
    let n = normalize_model_name(model);
    if n.contains("claude-3-")
        || matches!(
            n.as_str(),
            "claude-opus-4-0"
                | "claude-opus-4-1"
                | "claude-opus-4-5"
                | "claude-sonnet-4-0"
                | "claude-sonnet-4-5"
                | "claude-haiku-4-5"
        )
    {
        return false;
    }
    catalog_entry(&n).is_some_and(|entry| entry.capabilities.contains(&"max_effort"))
        || n == "claude-mythos-5"
        || provider_supports_effort_default()
}

/// Maps to CC 2.1.198 `Zne` / `modelSupportsXhighEffort`.
pub fn model_supports_xhigh_effort(model: &str) -> bool {
    if let Some(value) =
        get_3p_model_capability_override(model, ModelCapabilityOverride::XhighEffort)
    {
        return value;
    }
    let n = normalize_model_name(model);
    if n.contains("claude-3-")
        || matches!(
            n.as_str(),
            "claude-opus-4-0"
                | "claude-opus-4-1"
                | "claude-opus-4-5"
                | "claude-opus-4-6"
                | "claude-sonnet-4-0"
                | "claude-sonnet-4-5"
                | "claude-sonnet-4-6"
                | "claude-haiku-4-5"
        )
    {
        return false;
    }
    catalog_entry(&n).is_some_and(|entry| entry.capabilities.contains(&"xhigh_effort"))
        || n == "claude-mythos-5"
        || provider_supports_effort_default()
}

/// Maps to reconstruction `providerSupportsXhighDefault` / CC `IN`.
/// Keep the provider gate; do not copy ALLOW_THIRD_PARTY_WORKFLOWS's bypass.
fn provider_supports_effort_default() -> bool {
    use crate::utils::model::providers::{ApiProvider, get_api_provider};
    matches!(
        get_api_provider(),
        ApiProvider::FirstParty | ApiProvider::Bedrock | ApiProvider::Foundry
    )
}

/// Maps to: CC `utils/effort.ts:136-142#getEffortEnvOverride`.
///
/// Rust uses `Option<Option<EffortValue>>` for the official three states:
/// `None` = absent/invalid, `Some(None)` = `auto`/`unset`, and
/// `Some(Some(value))` = an explicit override.
pub fn get_effort_env_override() -> Option<Option<EffortValue>> {
    let env_override = crate::utils::process_env::var("CLAUDE_CODE_EFFORT_LEVEL")?;
    let lower = env_override.to_lowercase();
    if matches!(lower.as_str(), "unset" | "auto") {
        return Some(None);
    }
    crate::utils::ultracode::parse_extended_effort_level(&env_override)
        .map(|level| Some(EffortValue::Named(level.to_string())))
}

/// Maps to CC `utils/effort.ts#resolveAppliedEffort`.
pub fn resolve_applied_effort(
    model: &str,
    app_state_effort: Option<&EffortValue>,
) -> Option<EffortValue> {
    if !model_supports_effort(model) {
        return None;
    }
    let pinned = crate::utils::ultracode::is_launch_effort_pinned(model);
    let model_default = get_default_effort_for_model(model);
    let mut resolved = match get_effort_env_override() {
        Some(None) if !pinned => return None,
        Some(Some(effort)) => Some(effort),
        _ => if pinned { model_default.clone() } else { None }
            .or_else(|| app_state_effort.cloned())
            .or(model_default),
    }?;
    if let EffortValue::Named(level) = &resolved {
        resolved = EffortValue::Named(crate::utils::ultracode::clamp_effort_to_org_limit(
            level, model,
        ));
    }
    clamp_max_effort(model, resolved)
}

/// Maps to: CC `utils/effort.ts:107-111` `getInitialEffortSetting`.
pub fn get_initial_effort_setting() -> Option<EffortValue> {
    crate::utils::settings::get_initial_settings()
        .effort_level
        .as_deref()
        .and_then(parse_effort_value)
        .and_then(|effort| to_persistable_effort(Some(&effort)).map(|_| effort))
}

/// Maps to CC `utils/effort.ts#getDisplayedEffortLevel`.
pub fn get_displayed_effort_level(
    model: &str,
    app_state_effort: Option<&EffortValue>,
) -> &'static str {
    let resolved = resolve_applied_effort(model, app_state_effort)
        .unwrap_or_else(|| EffortValue::Named("high".to_string()));
    convert_effort_value_to_level(&resolved)
}

/// Maps to CC `utils/effort.ts#getEffortSuffix`.
pub fn get_effort_suffix(model: &str, effort_value: Option<&EffortValue>) -> String {
    let Some(effort_value) = effort_value else {
        return String::new();
    };
    let Some(resolved) = resolve_applied_effort(model, Some(effort_value)) else {
        return String::new();
    };
    format!(" with {} effort", convert_effort_value_to_level(&resolved))
}

/// CC's `process.env.USER_TYPE === 'ant'` (`effort.ts:61/:101/:209/:244/:282`)
/// — a build-time `--define` constant, not a runtime env read
/// (`constants/keys.ts:4`, `REPLTool/constants.ts:19`). Maps to the
/// `anthropic_internal` Cargo feature.
fn is_ant_user() -> bool {
    crate::utils::build_profile::has_internal_capability(
        crate::utils::build_profile::InternalCapability::Models,
    )
}

/// Maps to CC `utils/effort.ts#toPersistableEffort`.
/// 2.1.241 folded the ultracode `w5e` allowlist into this same filter;
/// the 2.1.88 name is kept as the single persist entry.
pub fn to_persistable_effort(value: Option<&EffortValue>) -> Option<String> {
    match value {
        Some(EffortValue::Named(level))
            // @cometix: persist `max` as a production effortLevel (CC 2.1.88: ant-only).
            if matches!(level.as_str(), "low" | "medium" | "high" | "xhigh" | "max") =>
        {
            Some(level.clone())
        }
        _ => None,
    }
}

/// Maps to CC `utils/effort.ts#resolvePickerEffortPersistence`.
/// A disk-persisted choice or an explicit picker adjustment remains sticky;
/// an untouched model default stays implicit.
pub fn resolve_picker_effort_persistence(
    picked: Option<&EffortValue>,
    model_default: &EffortValue,
    prior_persisted: Option<&str>,
    toggled_in_picker: bool,
) -> Option<EffortValue> {
    let had_explicit = prior_persisted.is_some() || toggled_in_picker;
    (had_explicit || picked != Some(model_default))
        .then(|| picked.cloned())
        .flatten()
}

/// Maps to CC `utils/effort.ts#convertEffortValueToLevel`.
pub fn convert_effort_value_to_level(value: &EffortValue) -> &'static str {
    match value {
        EffortValue::Named(level) => {
            if is_effort_level(level) {
                match level.as_str() {
                    "low" => "low",
                    "medium" => "medium",
                    "high" => "high",
                    "xhigh" => "xhigh",
                    "max" => "max",
                    _ => "high",
                }
            } else {
                "high"
            }
        }
        // CC `:209` — `process.env.USER_TYPE === 'ant' && typeof value === 'number'`.
        EffortValue::Numeric(value) if is_ant_user() => {
            if *value <= 50 {
                "low"
            } else if *value <= 85 {
                "medium"
            } else if *value <= 100 {
                "high"
            } else {
                "max"
            }
        }
        EffortValue::Numeric(_) => "high",
    }
}

/// Maps to CC `utils/effort.ts#getEffortLevelDescription`.
pub fn get_effort_level_description(level: &str) -> &'static str {
    match level {
        "low" => "Quick, straightforward implementation with minimal overhead",
        "medium" => "Balanced approach with standard implementation and testing",
        "high" => "Comprehensive implementation with extensive testing and documentation",
        "xhigh" => "Extended reasoning with thorough analysis (Fable 5, Opus 4.7+, Sonnet 5)",
        "max" => "Maximum capability with deepest reasoning (Fable 5, Opus 4.6+, Sonnet 4.6+)",
        _ => "Balanced approach with standard implementation and testing",
    }
}

/// Maps to CC `utils/effort.ts#getEffortValueDescription`.
pub fn get_effort_value_description(value: &EffortValue) -> String {
    match value {
        // CC `:244` — the same runtime env gate.
        EffortValue::Numeric(value) if is_ant_user() => {
            format!("[ANT-ONLY] Numeric effort value of {value}")
        }
        EffortValue::Named(level) => get_effort_level_description(level).to_string(),
        EffortValue::Numeric(_) => {
            "Balanced approach with standard implementation and testing".to_string()
        }
    }
}

/// Maps to CC `utils/effort.ts#getDefaultEffortForModel`.
pub fn get_default_effort_for_model(model: &str) -> Option<EffortValue> {
    let n = normalize_model_name(model);
    Some(EffortValue::Named(
        catalog_entry(&n)
            .map_or("high", |entry| entry.default_effort)
            .to_string(),
    ))
}

fn clamp_max_effort(model: &str, effort: EffortValue) -> Option<EffortValue> {
    match effort {
        EffortValue::Named(level) if level == "max" && !model_supports_max_effort(model) => {
            Some(EffortValue::Named("high".to_string()))
        }
        EffortValue::Named(level) if level == "xhigh" && !model_supports_xhigh_effort(model) => {
            Some(EffortValue::Named("high".to_string()))
        }
        other => Some(other),
    }
}

fn parse_js_parse_int_base10(value: &str) -> Option<i64> {
    let trimmed = value.trim_start();
    let bytes = trimmed.as_bytes();
    if bytes.is_empty() {
        return None;
    }

    let mut index = 0;
    let sign = match bytes[0] {
        b'-' => {
            index = 1;
            -1_i128
        }
        b'+' => {
            index = 1;
            1_i128
        }
        _ => 1_i128,
    };

    let mut value: i128 = 0;
    let mut saw_digit = false;
    while let Some(byte) = bytes.get(index) {
        if !byte.is_ascii_digit() {
            break;
        }
        saw_digit = true;
        value = value.checked_mul(10)?.checked_add((byte - b'0') as i128)?;
        index += 1;
    }
    if !saw_digit {
        return None;
    }
    let signed = value.checked_mul(sign)?;
    i64::try_from(signed).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[test]
    fn parse_effort_value_matches_official_parse_int_behavior() {
        assert_eq!(
            parse_effort_value("MEDIUM"),
            Some(EffortValue::Named("medium".to_string()))
        );
        assert_eq!(parse_effort_value("101"), Some(EffortValue::Numeric(101)));
        assert_eq!(parse_effort_value("12.9"), Some(EffortValue::Numeric(12)));
        assert_eq!(parse_effort_value("0x10"), Some(EffortValue::Numeric(0)));
        assert_eq!(parse_effort_value("invalid"), None);
        assert_eq!(parse_effort_value(" low "), None);
        assert_eq!(parse_effort_value(" 101 "), Some(EffortValue::Numeric(101)));
        assert_eq!(parse_effort_value(""), None);
    }

    #[test]
    fn picker_persistence_keeps_explicit_values_and_drops_untouched_defaults() {
        let high = EffortValue::Named("high".to_string());
        let xhigh = EffortValue::Named("xhigh".to_string());

        assert_eq!(
            resolve_picker_effort_persistence(Some(&high), &high, None, false),
            None
        );
        assert_eq!(
            resolve_picker_effort_persistence(Some(&high), &high, Some("high"), false),
            Some(high.clone())
        );
        assert_eq!(
            resolve_picker_effort_persistence(Some(&xhigh), &high, None, false),
            Some(xhigh.clone())
        );
        assert_eq!(
            resolve_picker_effort_persistence(Some(&high), &high, None, true),
            Some(high)
        );
    }

    #[test]
    fn model_supports_effort_matches_official_public_model_gates() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("CLAUDE_CODE_ALWAYS_ENABLE_EFFORT");
        crate::utils::process_env::remove("CLAUDE_CODE_USE_BEDROCK");
        crate::utils::process_env::remove("CLAUDE_CODE_USE_VERTEX");
        crate::utils::process_env::remove("CLAUDE_CODE_USE_FOUNDRY");

        assert!(model_supports_effort("claude-opus-4-6-20260101"));
        assert!(model_supports_effort("claude-sonnet-4-6-20260101"));
        assert!(model_supports_max_effort("claude-opus-4-6"));
        assert!(model_supports_max_effort("claude-sonnet-4-6"));
        assert!(model_supports_max_effort("unknown-first-party-model"));
        assert!(!model_supports_xhigh_effort("claude-opus-4-6"));
        assert!(!model_supports_xhigh_effort("claude-sonnet-4-6"));
        assert!(!model_supports_max_effort("claude-sonnet-4-5"));
        assert!(!model_supports_effort("claude-sonnet-4-20250514"));
        assert!(!model_supports_effort("claude-3-haiku-20240307"));
        assert!(model_supports_effort("unknown-first-party-model"));

        crate::utils::process_env::set("CLAUDE_CODE_USE_BEDROCK", "1");
        assert!(model_supports_effort("unknown-third-party-model"));

        crate::utils::process_env::set("CLAUDE_CODE_ALWAYS_ENABLE_EFFORT", "1");
        assert!(!model_supports_effort("claude-3-haiku-20240307"));

        crate::utils::process_env::remove("CLAUDE_CODE_ALWAYS_ENABLE_EFFORT");
        crate::utils::process_env::remove("CLAUDE_CODE_USE_BEDROCK");
    }

    #[test]
    fn effort_suffix_matches_official_explicit_effort_display() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("CLAUDE_CODE_EFFORT_LEVEL");

        assert_eq!(get_effort_suffix("claude-opus-4-6", None), "");
        assert_eq!(
            get_effort_suffix(
                "claude-opus-4-5",
                Some(&EffortValue::Named("max".to_string()))
            ),
            " with high effort"
        );
        assert_eq!(
            get_effort_suffix(
                "claude-opus-4-6",
                Some(&EffortValue::Named("medium".to_string()))
            ),
            " with medium effort"
        );

        crate::utils::process_env::set("CLAUDE_CODE_EFFORT_LEVEL", "low");
        assert_eq!(
            get_effort_suffix(
                "claude-opus-4-6",
                Some(&EffortValue::Named("high".to_string()))
            ),
            " with low effort"
        );
        crate::utils::process_env::remove("CLAUDE_CODE_EFFORT_LEVEL");
    }

    struct InitialSettingsGuard {
        previous_allowed_sources: Vec<String>,
        previous_flag_path: Option<std::path::PathBuf>,
        previous_flag_inline: Option<serde_json::Value>,
        managed_root: std::path::PathBuf,
        _managed_path_env: EnvVarGuard,
    }

    impl InitialSettingsGuard {
        fn new() -> Self {
            let managed_root = std::env::temp_dir().join(format!(
                "cometix-effort-settings-{}",
                uuid::Uuid::new_v4().simple()
            ));
            std::fs::create_dir_all(&managed_root).expect("create isolated managed settings root");
            let managed_path_env =
                EnvVarGuard::set("CLAUDE_CODE_MANAGED_SETTINGS_PATH", &managed_root);
            let previous_allowed_sources = crate::bootstrap::state::get_allowed_setting_sources();
            let previous_flag_path = crate::utils::settings::get_flag_settings_path();
            let previous_flag_inline = crate::utils::settings::get_flag_settings_inline();
            crate::bootstrap::state::set_allowed_setting_sources(Vec::new());
            crate::utils::settings::set_flag_settings_path(None);
            crate::utils::settings::set_flag_settings_inline(None);
            Self {
                previous_allowed_sources,
                previous_flag_path,
                previous_flag_inline,
                managed_root,
                _managed_path_env: managed_path_env,
            }
        }

        fn set_effort_level(&self, effort_level: &str) {
            crate::utils::settings::set_flag_settings_inline(Some(serde_json::json!({
                "effortLevel": effort_level,
            })));
        }
    }

    impl Drop for InitialSettingsGuard {
        fn drop(&mut self) {
            crate::bootstrap::state::set_allowed_setting_sources(
                self.previous_allowed_sources.clone(),
            );
            crate::utils::settings::set_flag_settings_path(self.previous_flag_path.clone());
            crate::utils::settings::set_flag_settings_inline(self.previous_flag_inline.clone());
            let _ = std::fs::remove_dir_all(&self.managed_root);
        }
    }

    #[test]
    fn initial_effort_setting_matches_official_canonical_settings() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let settings = InitialSettingsGuard::new();

        settings.set_effort_level("medium");
        assert_eq!(
            get_initial_effort_setting(),
            Some(EffortValue::Named("medium".to_string()))
        );

        // @cometix: persist `max` as a production effortLevel (CC 2.1.88: ant-only).
        settings.set_effort_level("max");
        assert_eq!(
            get_initial_effort_setting(),
            Some(EffortValue::Named("max".to_string()))
        );
    }

    #[test]
    fn effort_env_override_preserves_official_absent_auto_and_explicit_states() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _restore = EnvVarGuard::unset("CLAUDE_CODE_EFFORT_LEVEL");

        crate::utils::process_env::remove("CLAUDE_CODE_EFFORT_LEVEL");
        assert_eq!(get_effort_env_override(), None);
        crate::utils::process_env::set("CLAUDE_CODE_EFFORT_LEVEL", "AUTO");
        assert_eq!(get_effort_env_override(), Some(None));
        crate::utils::process_env::set("CLAUDE_CODE_EFFORT_LEVEL", "medium");
        assert_eq!(
            get_effort_env_override(),
            Some(Some(EffortValue::Named("medium".to_string())))
        );
        crate::utils::process_env::set("CLAUDE_CODE_EFFORT_LEVEL", "not-an-effort");
        assert_eq!(get_effort_env_override(), None);
        crate::utils::process_env::set("CLAUDE_CODE_EFFORT_LEVEL", "101");
        assert_eq!(get_effort_env_override(), None);
    }

    #[test]
    fn resolve_applied_effort_honors_env_unset_and_max_downgrade() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("CLAUDE_CODE_EFFORT_LEVEL");

        assert_eq!(
            resolve_applied_effort(
                "claude-opus-4-5-20251101",
                Some(&EffortValue::Named("max".to_string()))
            ),
            Some(EffortValue::Named("high".to_string()))
        );
        assert_eq!(
            resolve_applied_effort(
                "claude-opus-4-6-20260101",
                Some(&EffortValue::Named("max".to_string()))
            ),
            Some(EffortValue::Named("max".to_string()))
        );
        assert_eq!(
            resolve_applied_effort(
                "claude-opus-4-6-20260101",
                Some(&EffortValue::Named("xhigh".to_string()))
            ),
            Some(EffortValue::Named("high".to_string()))
        );
        assert_eq!(
            resolve_applied_effort(
                "claude-opus-4-7",
                Some(&EffortValue::Named("xhigh".to_string()))
            ),
            Some(EffortValue::Named("xhigh".to_string()))
        );

        crate::utils::process_env::set("CLAUDE_CODE_EFFORT_LEVEL", "unset");
        assert_eq!(
            resolve_applied_effort(
                "claude-opus-4-6-20260101",
                Some(&EffortValue::Named("high".to_string()))
            ),
            None
        );
        crate::utils::process_env::set("CLAUDE_CODE_EFFORT_LEVEL", "low");
        assert_eq!(
            resolve_applied_effort(
                "claude-opus-4-6-20260101",
                Some(&EffortValue::Named("high".to_string()))
            ),
            Some(EffortValue::Named("low".to_string()))
        );

        crate::utils::process_env::remove("CLAUDE_CODE_EFFORT_LEVEL");
    }
}
