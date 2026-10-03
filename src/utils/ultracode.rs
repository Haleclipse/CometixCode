//! CC 2.1.198 extended effort policy from rebuild `workflow-reconstruction`
//! `src/utils/ultracode.ts`. The baked-in catalog (`Eni`), name
//! canonicalization, organization limits, launch pinning and cache-break
//! decisions share that single file. Workflow orchestration remains
//! unavailable.

use crate::utils::effort::{EffortValue, resolve_applied_effort, to_persistable_effort};
use crate::utils::process_env::JsTruthy;
use crate::utils::workflows::is_dynamic_workflows_enabled;

pub(crate) struct CatalogEntry {
    pub id: &'static str,
    aliases: &'static [&'static str],
    pub capabilities: &'static [&'static str],
    pub default_effort: &'static str,
}

const MODEL_CATALOG: &[CatalogEntry] = &[
    CatalogEntry {
        id: "claude-3-5-haiku",
        aliases: &[
            "claude-3-5-haiku-20241022",
            "us.anthropic.claude-3-5-haiku-20241022-v1:0",
            "claude-3-5-haiku@20241022",
            "claude-3-5-haiku",
            "claude-3-5-haiku-20241022",
            "claude-3-5-haiku-20241022",
        ],
        capabilities: &[],
        default_effort: "high",
    },
    CatalogEntry {
        id: "claude-haiku-4-5",
        aliases: &[
            "claude-haiku-4-5-20251001",
            "us.anthropic.claude-haiku-4-5-20251001-v1:0",
            "claude-haiku-4-5@20251001",
            "claude-haiku-4-5",
            "claude-haiku-4-5-20251001",
            "anthropic.claude-haiku-4-5",
            "claude-haiku-4-5-20251001",
        ],
        capabilities: &["context_management"],
        default_effort: "high",
    },
    CatalogEntry {
        id: "claude-3-5-sonnet",
        aliases: &[
            "claude-3-5-sonnet-20241022",
            "us.anthropic.claude-3-5-sonnet-20241022-v2:0",
            "claude-3-5-sonnet-v2@20241022",
            "claude-3-5-sonnet",
            "claude-3-5-sonnet-20241022",
            "claude-3-5-sonnet-20241022",
        ],
        capabilities: &[],
        default_effort: "high",
    },
    CatalogEntry {
        id: "claude-3-7-sonnet",
        aliases: &[
            "claude-3-7-sonnet-20250219",
            "us.anthropic.claude-3-7-sonnet-20250219-v1:0",
            "claude-3-7-sonnet@20250219",
            "claude-3-7-sonnet",
            "claude-3-7-sonnet-20250219",
            "claude-3-7-sonnet-20250219",
        ],
        capabilities: &[],
        default_effort: "high",
    },
    CatalogEntry {
        id: "claude-sonnet-4-0",
        aliases: &[
            "claude-sonnet-4-20250514",
            "us.anthropic.claude-sonnet-4-20250514-v1:0",
            "claude-sonnet-4@20250514",
            "claude-sonnet-4",
            "claude-sonnet-4-20250514",
            "claude-sonnet-4-20250514",
        ],
        capabilities: &["context_management"],
        default_effort: "high",
    },
    CatalogEntry {
        id: "claude-sonnet-4-5",
        aliases: &[
            "claude-sonnet-4-5-20250929",
            "us.anthropic.claude-sonnet-4-5-20250929-v1:0",
            "claude-sonnet-4-5@20250929",
            "claude-sonnet-4-5",
            "claude-sonnet-4-5-20250929",
            "claude-sonnet-4-5-20250929",
        ],
        capabilities: &["context_management"],
        default_effort: "high",
    },
    CatalogEntry {
        id: "claude-sonnet-4-6",
        aliases: &[
            "claude-sonnet-4-6",
            "us.anthropic.claude-sonnet-4-6",
            "claude-sonnet-4-6",
            "claude-sonnet-4-6",
            "claude-sonnet-4-6",
            "claude-sonnet-4-6",
        ],
        capabilities: &[
            "effort",
            "max_effort",
            "adaptive_thinking",
            "context_management",
        ],
        default_effort: "high",
    },
    CatalogEntry {
        id: "claude-sonnet-5",
        aliases: &[
            "claude-sonnet-5",
            "us.anthropic.claude-sonnet-5",
            "claude-sonnet-5",
            "claude-sonnet-5",
            "claude-sonnet-5",
            "anthropic.claude-sonnet-5",
            "claude-sonnet-5",
        ],
        capabilities: &[
            "effort",
            "max_effort",
            "xhigh_effort",
            "adaptive_thinking",
            "mid_conv_system",
            "context_management",
        ],
        default_effort: "high",
    },
    CatalogEntry {
        id: "claude-opus-4-0",
        aliases: &[
            "claude-opus-4-20250514",
            "us.anthropic.claude-opus-4-20250514-v1:0",
            "claude-opus-4@20250514",
            "claude-opus-4",
            "claude-opus-4-20250514",
            "claude-opus-4-20250514",
        ],
        capabilities: &["context_management"],
        default_effort: "high",
    },
    CatalogEntry {
        id: "claude-opus-4-1",
        aliases: &[
            "claude-opus-4-1-20250805",
            "us.anthropic.claude-opus-4-1-20250805-v1:0",
            "claude-opus-4-1@20250805",
            "claude-opus-4-1",
            "claude-opus-4-1-20250805",
            "claude-opus-4-1-20250805",
        ],
        capabilities: &["context_management"],
        default_effort: "high",
    },
    CatalogEntry {
        id: "claude-opus-4-5",
        aliases: &[
            "claude-opus-4-5-20251101",
            "us.anthropic.claude-opus-4-5-20251101-v1:0",
            "claude-opus-4-5@20251101",
            "claude-opus-4-5",
            "claude-opus-4-5-20251101",
            "claude-opus-4-5-20251101",
        ],
        capabilities: &["context_management"],
        default_effort: "high",
    },
    CatalogEntry {
        id: "claude-opus-4-6",
        aliases: &[
            "claude-opus-4-6",
            "us.anthropic.claude-opus-4-6-v1",
            "claude-opus-4-6",
            "claude-opus-4-6",
            "claude-opus-4-6",
            "claude-opus-4-6",
        ],
        capabilities: &[
            "effort",
            "max_effort",
            "adaptive_thinking",
            "context_management",
        ],
        default_effort: "high",
    },
    CatalogEntry {
        id: "claude-opus-4-7",
        aliases: &[
            "claude-opus-4-7",
            "us.anthropic.claude-opus-4-7",
            "claude-opus-4-7",
            "claude-opus-4-7",
            "claude-opus-4-7",
            "anthropic.claude-opus-4-7",
            "claude-opus-4-7",
        ],
        capabilities: &[
            "effort",
            "max_effort",
            "xhigh_effort",
            "adaptive_thinking",
            "context_management",
            "fast_mode",
        ],
        default_effort: "xhigh",
    },
    CatalogEntry {
        id: "claude-opus-4-8",
        aliases: &[
            "claude-opus-4-8",
            "us.anthropic.claude-opus-4-8",
            "claude-opus-4-8",
            "claude-opus-4-8",
            "claude-opus-4-8",
            "anthropic.claude-opus-4-8",
            "claude-opus-4-8",
        ],
        capabilities: &[
            "effort",
            "max_effort",
            "xhigh_effort",
            "adaptive_thinking",
            "mid_conv_system",
            "context_management",
            "fast_mode",
            "lean_prompt",
        ],
        default_effort: "high",
    },
    CatalogEntry {
        id: "claude-fable-5",
        aliases: &[
            "claude-fable-5",
            "us.anthropic.claude-fable-5",
            "claude-fable-5",
            "claude-fable-5",
            "claude-fable-5",
            "anthropic.claude-fable-5",
            "claude-fable-5",
        ],
        capabilities: &[
            "effort",
            "max_effort",
            "xhigh_effort",
            "adaptive_thinking",
            "rejects_disabled_thinking",
            "mid_conv_system",
            "context_management",
            "lean_prompt",
            "fable_5_mitigations",
        ],
        default_effort: "high",
    },
    CatalogEntry {
        id: "claude-mythos-5",
        aliases: &["claude-mythos-5"],
        capabilities: &[],
        default_effort: "high",
    },
];

/// Maps to: rebuild `workflow-reconstruction` `utils/ultracode.ts#getCatalogEntry`
/// (`KP`) / `isModelInCapabilityAllowlist` (`l$`).
pub(crate) fn catalog_entry(id: &str) -> Option<&'static CatalogEntry> {
    MODEL_CATALOG.iter().find(|entry| entry.id == id)
}

/// Maps to: rebuild `workflow-reconstruction`
/// `utils/ultracode.ts#normalizeModelName` (`so`). Inference-profile
/// backing-model session lookup remains unavailable, as in that reconstruction.
pub(crate) fn normalize_model_name(model: &str) -> String {
    let settings = crate::utils::settings::get_initial_settings();
    let raw = settings
        .model_overrides
        .as_ref()
        .and_then(|entries| entries.iter().find(|(_, value)| value.as_str() == model))
        .map(|(key, _)| key.as_str())
        .unwrap_or(model);
    canonicalize_model_id(raw)
}

/// Maps to: rebuild `workflow-reconstruction`
/// `utils/ultracode.ts#canonicalizeModelId` (`p_`).
fn canonicalize_model_id(raw: &str) -> String {
    let lower = raw.to_lowercase();
    if let Some(entry) = MODEL_CATALOG
        .iter()
        .find(|entry| entry.aliases.contains(&lower.as_str()))
    {
        return entry.id.to_string();
    }
    for region in ["eu", "apac", "jp", "au", "us-gov", "global"] {
        if lower.starts_with(&format!("{region}.anthropic.")) {
            let remapped = format!("us{}", &lower[region.len()..]);
            if let Some(entry) = MODEL_CATALOG
                .iter()
                .find(|entry| entry.aliases.contains(&remapped.as_str()))
            {
                return entry.id.to_string();
            }
            break;
        }
    }
    // Preserve p_'s ordered family chain, including the negative lookahead
    // on the legacy "-4" families (Rust regex does not support lookahead).
    for family in [
        "claude-fable-5",
        "claude-mythos-5",
        "claude-opus-4-8",
        "claude-opus-4-7",
        "claude-opus-4-6",
        "claude-opus-4-5",
        "claude-opus-4-1",
        "claude-opus-4",
        "claude-sonnet-5",
        "claude-sonnet-4-6",
        "claude-sonnet-4-5",
        "claude-sonnet-4",
        "claude-haiku-4-5",
        "claude-3-7-sonnet",
        "claude-3-5-sonnet",
        "claude-3-5-haiku",
        "claude-3-opus",
        "claude-3-sonnet",
        "claude-3-haiku",
    ] {
        if let Some(at) = lower.find(family) {
            if matches!(family, "claude-opus-4" | "claude-sonnet-4") {
                let tail = lower[at + family.len()..].as_bytes();
                if tail.first() == Some(&b'-')
                    && tail.get(1).is_some_and(u8::is_ascii_digit)
                    && !tail.get(2).is_some_and(u8::is_ascii_digit)
                {
                    continue;
                }
                return format!("{family}-0");
            }
            return family.to_string();
        }
    }
    if let Some((prefix, date)) = lower.rsplit_once('-')
        && date.len() == 8
        && date.bytes().all(|ch| ch.is_ascii_digit())
    {
        return prefix.to_string();
    }
    lower
}

/// Maps to: CC `utils/ultracode.ts#EXTENDED_EFFORT_LEVELS` (`FI`).
pub use crate::utils::effort::EFFORT_LEVELS as EXTENDED_EFFORT_LEVELS;

/// Maps to: CC `utils/ultracode.ts#XHIGH_CAPABLE_MODELS` (`xMn`).
pub const XHIGH_CAPABLE_MODELS: &str = "Fable 5, Opus 4.7+, Sonnet 5";

/// Maps to: CC `utils/ultracode.ts#MAX_CAPABLE_MODELS` (`i4i`).
pub const MAX_CAPABLE_MODELS: &str = "Fable 5, Opus 4.6+, Sonnet 4.6+";

/// Maps to: CC `utils/ultracode.ts#XHIGH_EFFORT_WARNING` (`H2t`).
pub const XHIGH_EFFORT_WARNING: &str = "May use excessive tokens resulting in long response times or overthinking. Use sparingly for the hardest tasks.";

/// Maps to: CC `utils/ultracode.ts#ULTRACODE_FIGURE` (`aHn`).
pub const ULTRACODE_FIGURE: &str = crate::constants::figures::ULTRACODE_FIGURE;

/// Maps to: CC `utils/ultracode.ts#EFFORT_ALIASES` (`c4i`).
const EFFORT_ALIASES: &[(&str, &str)] = &[("med", "medium")];

/// Maps to: CC `utils/ultracode.ts#isExtendedEffortLevel` (`Xce`).
pub fn is_extended_effort_level(value: &str) -> bool {
    EXTENDED_EFFORT_LEVELS.contains(&value)
}

/// Maps to: CC `utils/ultracode.ts#parseExtendedEffortLevel` (`Jat`).
pub fn parse_extended_effort_level(value: &str) -> Option<&'static str> {
    let normalized = value.trim().to_lowercase();
    let aliased = EFFORT_ALIASES
        .iter()
        .find_map(|(alias, level)| (*alias == normalized).then_some(*level))
        .unwrap_or(normalized.as_str());
    EXTENDED_EFFORT_LEVELS
        .iter()
        .copied()
        .find(|level| *level == aliased)
}

/// Maps to: CC `utils/ultracode.ts#resolveInitialEffortSetting` (`p4i`).
/// The command-line value wins, followed by the ultracode session seed and
/// then the persistable settings value.
pub fn resolve_initial_effort_setting(
    cli_effort: Option<&str>,
    settings: &crate::utils::settings::SettingsJson,
) -> Option<EffortValue> {
    if let Some(level) = cli_effort.and_then(parse_extended_effort_level) {
        return Some(EffortValue::Named(level.to_string()));
    }
    if settings.ultracode == Some(true) {
        return Some(EffortValue::Named("xhigh".to_string()));
    }
    settings.effort_level.as_deref().and_then(|level| {
        to_persistable_effort(Some(&EffortValue::Named(level.to_string()))).map(EffortValue::Named)
    })
}

/// Maps to: CC `utils/ultracode.ts#consumeUltracodeSetting` (`HQr`).
pub fn consume_ultracode_setting(settings: &crate::utils::settings::SettingsJson) -> bool {
    let enabled = settings.ultracode == Some(true);
    if enabled {
        unpin_launch_effort();
    }
    enabled
}

/// Maps to CC `utils/ultracode.ts#getOrgMaxEffortLevel` (Xat).
/// The existing provider union has no gateway member; firstParty is live here.
pub fn get_org_max_effort_level(model: &str) -> Option<String> {
    if crate::utils::model::providers::get_api_provider()
        != crate::utils::model::providers::ApiProvider::FirstParty
    {
        return None;
    }
    let config = crate::utils::config::load_global_config();
    let entries = config
        .other_global_fields
        .get("modelAccessCache")?
        .as_array()?;
    let normalized = normalize_model_name(&model.trim().to_lowercase().replace("[1m]", ""));
    let entry = entries.iter().find(|entry| {
        entry
            .get("entitled")
            .and_then(serde_json::Value::as_bool)
            .is_some()
            && entry
                .get("apiName")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|name| {
                    normalize_model_name(&name.trim().to_lowercase().replace("[1m]", ""))
                        == normalized
                })
    })?;
    let cap = entry.get("maxEffortLevel")?.as_str()?;
    is_extended_effort_level(cap).then(|| cap.to_string())
}

/// Maps to CC `utils/ultracode.ts#effortLevelIndex` (F0e).
fn effort_level_index(level: &str) -> Option<usize> {
    EXTENDED_EFFORT_LEVELS
        .iter()
        .position(|candidate| *candidate == level)
}

/// Maps to CC `utils/ultracode.ts#isEffortWithinOrgLimit` (e6e).
pub fn is_effort_within_org_limit(level: &str, model: &str) -> bool {
    get_org_max_effort_level(model)
        .is_none_or(|cap| effort_level_index(level) <= effort_level_index(&cap))
}

/// Maps to CC `utils/ultracode.ts#getEligibleEffortLevels` (Uye).
pub fn get_eligible_effort_levels(model: &str) -> Vec<&'static str> {
    EXTENDED_EFFORT_LEVELS
        .iter()
        .copied()
        .filter(|level| is_effort_within_org_limit(level, model))
        .collect()
}

/// Maps to CC `utils/ultracode.ts#clampEffortToOrgLimit` (B0e).
pub fn clamp_effort_to_org_limit(level: &str, model: &str) -> String {
    get_org_max_effort_level(model)
        .filter(|cap| effort_level_index(level) > effort_level_index(cap))
        .unwrap_or_else(|| level.to_string())
}

/// Maps to CC `utils/ultracode.ts#hasOrgRestrictedHigherEffort` (a4i).
pub fn has_org_restricted_higher_effort(model: &str) -> bool {
    get_org_max_effort_level(model).is_some_and(|cap| {
        EXTENDED_EFFORT_LEVELS.iter().any(|level| {
            effort_level_index(level) > effort_level_index(&cap)
                && match *level {
                    "xhigh" => crate::utils::effort::model_supports_xhigh_effort(model),
                    "max" => crate::utils::effort::model_supports_max_effort(model),
                    _ => true,
                }
        })
    })
}

/// Maps to CC `utils/ultracode.ts#isLaunchEffortPinned` (n6e).
pub fn is_launch_effort_pinned(model: &str) -> bool {
    let normalized = normalize_model_name(model);
    let key = if normalized.contains("opus-4-7") {
        "unpinOpus47LaunchEffort"
    } else if normalized.contains("opus-4-8") {
        "unpinOpus48LaunchEffort"
    } else if normalized.contains("fable-5")
        || crate::utils::process_env::var("ANTHROPIC_DEFAULT_FABLE_MODEL")
            .truthy()
            .is_some_and(|value| {
                value.to_lowercase().trim_end_matches("[1m]")
                    == model.to_lowercase().trim_end_matches("[1m]")
            })
    {
        "unpinFable5LaunchEffort"
    } else {
        return false;
    };
    crate::utils::config::load_global_config()
        .other_global_fields
        .get(key)
        .and_then(serde_json::Value::as_bool)
        != Some(true)
}

/// Maps to CC `utils/ultracode.ts#unpinLaunchEffort` (r9).
pub fn unpin_launch_effort() {
    if let Err(error) = crate::utils::config::save_global_config(|config| {
        for key in [
            "unpinOpus47LaunchEffort",
            "unpinOpus48LaunchEffort",
            "unpinFable5LaunchEffort",
        ] {
            config
                .other_global_fields
                .insert(key.to_string(), true.into());
        }
    }) {
        tracing::warn!("Failed to save launch effort preference: {error}");
    }
}

/// Maps to CC `utils/ultracode.ts#shouldConfirmEffortCacheBreak` (_2t).
/// The unattached local session path matches the reference reconstruction;
/// there is no writable remote control-channel singleton in this port.
pub fn should_confirm_effort_cache_break(
    new_value: Option<&EffortValue>,
    current_value: Option<&EffortValue>,
    model: &str,
    acked_output_tokens: i64,
    has_conversation_messages: bool,
) -> bool {
    if !has_conversation_messages {
        return false;
    }
    let output_tokens = crate::cost_tracker::get_total_output_tokens();
    if output_tokens == 0
        || i64::try_from(output_tokens).unwrap_or(i64::MAX) == acked_output_tokens
        || !crate::utils::effort::model_supports_effort(model)
    {
        return false;
    }
    if is_launch_effort_pinned(model) {
        if new_value.is_none()
            || new_value == crate::utils::effort::get_default_effort_for_model(model).as_ref()
        {
            return false;
        }
    } else if resolve_applied_effort(model, new_value)
        == resolve_applied_effort(model, current_value)
    {
        return false;
    }
    true
}

/// Maps to: CC `utils/ultracode.ts#isUltracodeAvailable` (`WV`).
///
/// Official: `isDynamicWorkflowsEnabled() && (model === undefined ||
/// (modelSupportsXhighEffort(model) && isEffortWithinOrgLimit('xhigh', model)))`.
/// Availability includes the organization cap.
pub fn is_ultracode_available(model: Option<&str>) -> bool {
    if !is_dynamic_workflows_enabled() {
        return false;
    }
    match model {
        None => true,
        Some(model) => {
            crate::utils::effort::model_supports_xhigh_effort(model)
                && is_effort_within_org_limit("xhigh", model)
        }
    }
}

/// Maps to: CC `utils/ultracode.ts#isUltracodeActive` (`ere`).
pub fn is_ultracode_active(
    model: &str,
    effort_value: Option<&EffortValue>,
    ultracode_flag: bool,
) -> bool {
    ultracode_flag
        && is_dynamic_workflows_enabled()
        && crate::utils::effort::model_supports_xhigh_effort(model)
        && resolve_applied_effort(model, effort_value)
            .is_some_and(|value| matches!(value, EffortValue::Named(level) if level == "xhigh"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::config::{GlobalConfig, replace_test_global_config};
    use crate::utils::effort::{
        model_supports_effort, model_supports_max_effort, model_supports_xhigh_effort,
        resolve_applied_effort,
    };
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[test]
    fn parse_extended_effort_level_accepts_med_alias_and_xhigh() {
        assert_eq!(parse_extended_effort_level("MED"), Some("medium"));
        assert_eq!(parse_extended_effort_level("xhigh"), Some("xhigh"));
        assert_eq!(parse_extended_effort_level("ultracode"), None);
        assert_eq!(parse_extended_effort_level("nope"), None);
    }

    #[test]
    fn initial_effort_uses_cli_then_ultracode_then_persisted_level() {
        let settings = crate::utils::settings::SettingsJson {
            effort_level: Some("medium".to_string()),
            ultracode: Some(true),
            ..Default::default()
        };
        assert_eq!(
            resolve_initial_effort_setting(Some(" high "), &settings),
            Some(EffortValue::Named("high".to_string()))
        );
        assert_eq!(
            resolve_initial_effort_setting(None, &settings),
            Some(EffortValue::Named("xhigh".to_string()))
        );

        // @cometix: persist `max` as a production effortLevel (CC 2.1.88: ant-only).
        let settings = crate::utils::settings::SettingsJson {
            effort_level: Some("max".to_string()),
            ..Default::default()
        };
        assert_eq!(
            resolve_initial_effort_setting(None, &settings),
            Some(EffortValue::Named("max".to_string()))
        );
    }

    #[test]
    fn argument_hint_includes_extended_levels() {
        let hint = crate::commands::effort::effort_argument_hint("claude-opus-4-7");
        assert!(hint.contains("xhigh"));
        assert!(hint.contains("max"));
        assert!(hint.contains("auto"));
        assert_eq!(
            hint.contains("ultracode"),
            is_ultracode_available(Some("claude-opus-4-7"))
        );
        assert!(!is_ultracode_available(Some("claude-opus-4-7")));
    }

    struct ConfigGuard(Option<GlobalConfig>);
    impl ConfigGuard {
        fn new(value: serde_json::Value) -> Self {
            Self(replace_test_global_config(Some(
                serde_json::from_value(value).unwrap(),
            )))
        }
    }
    impl Drop for ConfigGuard {
        fn drop(&mut self) {
            replace_test_global_config(self.0.take());
        }
    }
    fn named(value: &str) -> EffortValue {
        EffortValue::Named(value.into())
    }

    #[test]
    fn capability_catalog_matches_official_provider_aliases_and_46_xhigh_exclusion() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _bedrock = EnvVarGuard::unset("CLAUDE_CODE_USE_BEDROCK");
        let _foundry = EnvVarGuard::unset("CLAUDE_CODE_USE_FOUNDRY");
        let _vertex = EnvVarGuard::set("CLAUDE_CODE_USE_VERTEX", "1");
        for model in [
            "claude-opus-4-6",
            "claude-opus-4-6@20260101",
            "eu.anthropic.claude-opus-4-6-v1",
            "claude-sonnet-4-6",
            "us.anthropic.claude-sonnet-4-6",
        ] {
            assert!(model_supports_effort(model), "{model}");
            assert!(model_supports_max_effort(model), "{model}");
            assert!(!model_supports_xhigh_effort(model), "{model}");
        }
        for model in [
            "claude-opus-4-7",
            "us.anthropic.claude-opus-4-7",
            "claude-fable-5",
            "claude-sonnet-5",
        ] {
            assert!(model_supports_effort(model), "{model}");
            assert!(model_supports_max_effort(model), "{model}");
            assert!(model_supports_xhigh_effort(model), "{model}");
        }
        for model in [
            "claude-opus-4-20250514",
            "claude-opus-4@20250514",
            "us.anthropic.claude-sonnet-4-20250514-v1:0",
            "unknown-vertex-model",
        ] {
            assert!(!model_supports_max_effort(model), "{model}");
            assert!(!model_supports_xhigh_effort(model), "{model}");
        }
    }

    #[test]
    fn org_cap_matches_official_picker_geometry_resolution_and_provider_gate() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _env = EnvVarGuard::unset("CLAUDE_CODE_EFFORT_LEVEL");
        let _bedrock = EnvVarGuard::unset("CLAUDE_CODE_USE_BEDROCK");
        let _vertex = EnvVarGuard::unset("CLAUDE_CODE_USE_VERTEX");
        let _foundry = EnvVarGuard::unset("CLAUDE_CODE_USE_FOUNDRY");
        let _config = ConfigGuard::new(serde_json::json!({
            "modelAccessCache": [
                {"apiName":"claude-opus-4-7", "entitled":"invalid", "maxEffortLevel":"low"},
                {"apiName":"claude-opus-4-7[1m]", "entitled":true, "maxEffortLevel":"medium"}
            ]
        }));
        assert_eq!(
            get_eligible_effort_levels("claude-opus-4-7"),
            vec!["low", "medium"]
        );
        assert_eq!(
            resolve_applied_effort("claude-opus-4-7", Some(&named("max"))),
            Some(named("medium"))
        );
        let geometry = crate::components::effort_picker::get_slider_geometry("claude-opus-4-7");
        assert_eq!(geometry.levels.len(), 2);
        assert_eq!(geometry.width, 14);
        assert_eq!(
            geometry.cap_note.as_deref(),
            Some("Higher effort levels are restricted by your organization.")
        );
        let _vertex = EnvVarGuard::set("CLAUDE_CODE_USE_VERTEX", "1");
        assert_eq!(get_org_max_effort_level("claude-opus-4-7"), None);
    }

    #[test]
    fn launch_pin_matches_official_precedence_and_picker_default() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _env = EnvVarGuard::unset("CLAUDE_CODE_EFFORT_LEVEL");
        let _config = ConfigGuard::new(serde_json::json!({}));
        assert_eq!(
            resolve_applied_effort("claude-opus-4-7", Some(&named("low"))),
            Some(named("xhigh"))
        );
        let geometry = crate::components::effort_picker::get_slider_geometry("claude-opus-4-7");
        assert_eq!(
            crate::components::effort_picker::initial_focus_index(
                &geometry,
                "claude-opus-4-7",
                Some(&named("low")),
                false
            ),
            3
        );
        let _env = EnvVarGuard::set("CLAUDE_CODE_EFFORT_LEVEL", "MED");
        assert_eq!(
            resolve_applied_effort("claude-opus-4-7", Some(&named("low"))),
            Some(named("medium"))
        );
        let _env = EnvVarGuard::set("CLAUDE_CODE_EFFORT_LEVEL", "auto");
        assert_eq!(
            resolve_applied_effort("claude-opus-4-7", Some(&named("low"))),
            Some(named("xhigh"))
        );
        let _config = ConfigGuard::new(serde_json::json!({"unpinOpus47LaunchEffort":true}));
        assert_eq!(
            resolve_applied_effort("claude-opus-4-7", Some(&named("low"))),
            None
        );
        let _env = EnvVarGuard::unset("CLAUDE_CODE_EFFORT_LEVEL");
        assert_eq!(
            resolve_applied_effort("claude-opus-4-7", Some(&named("low"))),
            Some(named("low"))
        );
    }

    #[test]
    fn cache_break_gate_matches_official_tokens_effective_change_and_launch_pin() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _env = EnvVarGuard::unset("CLAUDE_CODE_EFFORT_LEVEL");
        let _config = ConfigGuard::new(serde_json::json!({}));
        crate::cost_tracker::reset_cost_state_for_tests();
        let high = named("high");
        let low = named("low");
        assert!(!should_confirm_effort_cache_break(
            Some(&low),
            Some(&high),
            "claude-opus-4-6",
            0,
            true
        ));
        crate::cost_tracker::add_to_total_tokens(0, 10);
        assert!(!should_confirm_effort_cache_break(
            Some(&low),
            Some(&high),
            "claude-opus-4-6",
            0,
            false
        ));
        assert!(should_confirm_effort_cache_break(
            Some(&low),
            Some(&high),
            "claude-opus-4-6",
            0,
            true
        ));
        assert!(!should_confirm_effort_cache_break(
            Some(&low),
            Some(&high),
            "claude-opus-4-6",
            10,
            true
        ));
        assert!(!should_confirm_effort_cache_break(
            Some(&named("xhigh")),
            Some(&high),
            "claude-opus-4-6",
            0,
            true
        ));
        assert!(!should_confirm_effort_cache_break(
            None,
            Some(&low),
            "claude-opus-4-7",
            0,
            true
        ));
        assert!(should_confirm_effort_cache_break(
            Some(&low),
            Some(&high),
            "claude-opus-4-7",
            0,
            true
        ));
        let _env = EnvVarGuard::set("CLAUDE_CODE_EFFORT_LEVEL", "high");
        assert!(!should_confirm_effort_cache_break(
            Some(&low),
            Some(&high),
            "claude-opus-4-6",
            0,
            true
        ));
        crate::cost_tracker::reset_cost_state_for_tests();
    }
}
