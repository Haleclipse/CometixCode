//! Thinking configuration helpers.
//! Maps to CC `utils/thinking.ts`.

use crate::utils::model::model::get_canonical_name;
use crate::utils::process_env::JsTruthy;
use crate::utils::settings::types::SettingsJson;
use crate::utils::theme::Theme;
use iocraft::prelude::Color;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThinkingTriggerPosition {
    pub word: String,
    /// UTF-8 byte offset used for safe Rust slicing.
    pub start: usize,
    pub end: usize,
}

static ULTRATHINK_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\bultrathink\b").expect("valid ultrathink regex"));

/// Maps to CC build-time `feature('ULTRATHINK')` plus its default-true
/// rollout. No telemetry or remote feature service is consulted.
pub fn is_ultrathink_enabled() -> bool {
    cfg!(feature = "ultrathink")
}

pub fn has_ultrathink_keyword(text: &str) -> bool {
    ULTRATHINK_PATTERN.is_match(text)
}

pub fn find_thinking_trigger_positions(text: &str) -> Vec<ThinkingTriggerPosition> {
    ULTRATHINK_PATTERN
        .find_iter(text)
        .map(|matched| ThinkingTriggerPosition {
            word: matched.as_str().to_string(),
            start: matched.start(),
            end: matched.end(),
        })
        .collect()
}

pub fn get_rainbow_color(theme: &Theme, char_index: usize, shimmer: bool) -> Color {
    let normal = [
        theme.rainbow_red,
        theme.rainbow_orange,
        theme.rainbow_yellow,
        theme.rainbow_green,
        theme.rainbow_blue,
        theme.rainbow_indigo,
        theme.rainbow_violet,
    ];
    let shimmered = [
        theme.rainbow_red_shimmer,
        theme.rainbow_orange_shimmer,
        theme.rainbow_yellow_shimmer,
        theme.rainbow_green_shimmer,
        theme.rainbow_blue_shimmer,
        theme.rainbow_indigo_shimmer,
        theme.rainbow_violet_shimmer,
    ];
    let colors = if shimmer { &shimmered } else { &normal };
    colors[char_index % colors.len()]
}

/// Maps to CC `utils/thinking.ts` `ThinkingConfig`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ThinkingConfig {
    #[serde(rename = "disabled")]
    Disabled,
    #[serde(rename = "enabled")]
    Enabled {
        #[serde(skip_serializing_if = "Option::is_none")]
        budget_tokens: Option<i64>,
    },
    #[serde(rename = "adaptive")]
    Adaptive,
}

/// Decimal-prefix parsing equivalent to JavaScript `parseInt(value, 10)` for
/// the i64 range used by thinking budgets.
pub fn parse_js_decimal_i64(value: &str) -> Option<i64> {
    let value = value.trim_start();
    let (negative, digits_source) = match value.as_bytes().first() {
        Some(b'-') => (true, &value[1..]),
        Some(b'+') => (false, &value[1..]),
        _ => (false, value),
    };
    let digit_len = digits_source.bytes().take_while(u8::is_ascii_digit).count();
    if digit_len == 0 {
        return None;
    }
    let magnitude = digits_source[..digit_len].parse::<i128>().ok()?;
    let signed = if negative { -magnitude } else { magnitude };
    i64::try_from(signed).ok()
}

/// Maps to: CC `utils/thinking.ts` `shouldEnableThinkingByDefault()`.
///
/// No GrowthBook / feature gates (ultrathink is separate). Settings source is
/// `alwaysThinkingEnabled` from merged settings, same as CC `getSettingsWithErrors()`.
pub fn should_enable_thinking_by_default(settings: &SettingsJson) -> bool {
    if let Some(value) = crate::utils::process_env::var("MAX_THINKING_TOKENS").truthy() {
        // parseInt → NaN yields false for `NaN > 0`.
        return parse_js_decimal_i64(&value).is_some_and(|tokens| tokens > 0);
    }

    if settings.always_thinking_enabled == Some(false) {
        return false;
    }

    // Enable thinking by default unless explicitly disabled.
    true
}

/// The `capabilities.thinking` declared by `profile` (`None`: the main
/// profile), when that profile serves `model`. A request that fell back to
/// another model is judged by the built-in rules instead.
fn profile_thinking(model: &str, profile: Option<&str>) -> Option<String> {
    let settings = crate::utils::settings::get_initial_settings();
    let nyan = settings.nyan.as_deref()?;
    let name = match profile {
        Some(name) => name,
        None => nyan.selection.as_ref()?.main.as_deref()?,
    };
    let entry = crate::utils::model::profile::model_profile(nyan, name)?;
    if entry.model.as_deref() != Some(model) {
        return None;
    }
    entry.capabilities.as_ref()?.thinking.clone()
}

static CLAUDE_FAMILY_VERSION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:opus|sonnet|haiku)[-.](\d+)(?:[-.](\d+))?").expect("valid claude version regex")
});

/// The version an Opus / Sonnet / Haiku id spells after its family name:
/// `claude-opus-4-6`, `claude-opus-4.6`, `claude-sonnet-4-5-20250929`,
/// `us.anthropic.claude-opus-4-6-v1:0`. A trailing date is not a minor
/// version (`claude-opus-4-20250514` is 4.0), and Claude 3 ids, which put the
/// version first (`claude-3-5-sonnet-20241022`), spell none.
fn claude_family_version(id: &str) -> Option<(u32, u32)> {
    let captures = CLAUDE_FAMILY_VERSION.captures(id)?;
    let major = captures.get(1)?.as_str();
    if major.len() > 2 {
        return None;
    }
    let minor = captures
        .get(2)
        .map(|minor| minor.as_str())
        .filter(|minor| minor.len() <= 2)
        .unwrap_or("0");
    Some((major.parse().ok()?, minor.parse().ok()?))
}

/// Maps to: CC `utils/thinking.ts:88-109` `modelSupportsThinking(model)`.
///
/// Provider-aware thinking support detection (CC: "aligns with
/// modelSupportsISP in betas.ts"):
///
/// 1. @nyan: the profile's declared `capabilities.thinking` wins; `none` is
///    the only value without thinking.
/// 2. Every model that is not `claude-3-` (including Haiku 4.5 and custom
///    gateway model names). CC's GrowthBook-delivered ant-model override list
///    does not exist here.
pub fn model_supports_thinking(model: &str, profile: Option<&str>) -> bool {
    if let Some(declared) = profile_thinking(model, profile) {
        return declared != "none";
    }
    // IMPORTANT (CC): Do not change thinking support without notifying the
    // model launch DRI and research. This can greatly affect model quality and
    // bashing.
    !get_canonical_name(model).contains("claude-3-")
}

/// Maps to: CC `utils/thinking.ts:112-144` `modelSupportsAdaptiveThinking(model)`.
///
/// - @nyan: the profile's declared `capabilities.thinking` wins; only
///   `adaptive` is adaptive.
/// - Opus / Sonnet / Haiku from 4.6 on: `true`. Earlier versions reject
///   adaptive thinking, and the later ones reject a budget.
/// - Any other known opus/sonnet/haiku string (Claude 3, no version): `false`.
/// - Unknown model strings default to `true`: newer models are trained on
///   adaptive thinking.
pub fn model_supports_adaptive_thinking(model: &str, profile: Option<&str>) -> bool {
    if let Some(declared) = profile_thinking(model, profile) {
        return declared == "adaptive";
    }
    let id = model.to_ascii_lowercase();
    if let Some(version) = claude_family_version(&id) {
        return version >= (4, 6);
    }
    !["opus", "sonnet", "haiku"]
        .iter()
        .any(|family| id.contains(family))
}

/// Non-REPL / missing-context fallback for `ThinkingConfig`.
///
/// Maps to: CC `main.tsx:3515-3541` **without** `--thinking` override —
/// `shouldEnableThinkingByDefault` then optional `MAX_THINKING_TOKENS` budget.
/// Interactive REPL always seeds `ToolUseContext.thinking_config` from
/// `ReplProps`; this is only for call sites that never received launch props.
///
/// Does **not** read the invented `NYAN_THINKING` env
/// or `GlobalConfig.thinking_enabled` — those are not CC
/// `shouldEnableThinkingByDefault` / main launch sources.
pub fn production_thinking_config_from_env_and_settings(settings: &SettingsJson) -> ThinkingConfig {
    let enabled = should_enable_thinking_by_default(settings);
    let config = if enabled {
        ThinkingConfig::Adaptive
    } else {
        ThinkingConfig::Disabled
    };

    // main.tsx else branch when no --thinking: env MAX_THINKING_TOKENS only
    // (CLI maxThinkingTokens is applied by resolve_thinking_launch in main).
    if let Some(value) = crate::utils::process_env::var("MAX_THINKING_TOKENS").truthy() {
        if let Some(tokens) = parse_js_decimal_i64(&value) {
            if tokens > 0 {
                return ThinkingConfig::Enabled {
                    budget_tokens: Some(tokens),
                };
            }
            if tokens == 0 {
                return ThinkingConfig::Disabled;
            }
        }
        // Invalid/NaN: keep first-stage config (enable already false).
        return config;
    }

    config
}

/// Compatibility entry for callers that historically passed `GlobalConfig`.
/// Loads merged settings so enable uses `alwaysThinkingEnabled` (CC), not
/// `GlobalConfig.thinking_enabled`.
pub fn production_thinking_config_from_env_and_config(
    _config: &crate::utils::config::GlobalConfig,
) -> ThinkingConfig {
    let settings = crate::utils::settings::get_initial_settings();
    production_thinking_config_from_env_and_settings(&settings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::settings::types::SettingsJson;
    use crate::utils::test_env::TEST_ENV_LOCK;

    #[test]
    fn ultrathink_matching_uses_fresh_case_insensitive_word_boundaries() {
        let positions = find_thinking_trigger_positions("ultrathink, ULTRATHINKER ultrathink");
        assert_eq!(positions.len(), 2);
        assert_eq!(positions[0].word, "ultrathink");
        assert_eq!(positions[1].word, "ultrathink");
        assert!(has_ultrathink_keyword("please UltraThink now"));
        assert!(!has_ultrathink_keyword("ultrathinker"));
    }

    #[test]
    fn rainbow_color_cycles_every_seven_characters() {
        let theme = *crate::utils::theme::current();
        assert_eq!(get_rainbow_color(&theme, 0, false), theme.rainbow_red);
        assert_eq!(get_rainbow_color(&theme, 7, false), theme.rainbow_red);
        assert_eq!(
            get_rainbow_color(&theme, 0, true),
            theme.rainbow_red_shimmer
        );
    }

    #[test]
    fn model_supports_thinking_matches_official_first_party_rule() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        // 1P: everything that is not claude-3-, including Haiku 4.5 and
        // gateway-custom names.
        assert!(model_supports_thinking("claude-haiku-4-5-20251001", None));
        assert!(model_supports_thinking("claude-sonnet-4-5", None));
        assert!(model_supports_thinking("deepseek-v4-flash", None));
        assert!(!model_supports_thinking("claude-3-5-sonnet-20241022", None));
    }

    #[test]
    fn model_supports_adaptive_thinking_matches_official_defaults() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        assert!(model_supports_adaptive_thinking("claude-opus-4-6", None));
        assert!(model_supports_adaptive_thinking("claude-sonnet-4-6", None));
        assert!(!model_supports_adaptive_thinking(
            "claude-haiku-4-5-20251001",
            None
        ));
        assert!(!model_supports_adaptive_thinking("claude-opus-4-5", None));
        // Unknown strings default to true: newer models are trained on
        // adaptive thinking.
        assert!(model_supports_adaptive_thinking("deepseek-v4-flash", None));
    }

    #[test]
    fn adaptive_thinking_starts_at_version_4_6_in_every_family() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        for model in [
            "claude-opus-4-6",
            "claude-opus-4-7",
            "claude-opus-5",
            "claude-opus-5-5",
            "claude-sonnet-5-5",
            "claude-haiku-5-5",
            "claude-haiku-5.5",
            "anthropic/claude-haiku-5-5",
            "us.anthropic.claude-haiku-5-5-v1:0",
            "claude-opus-4.6",
            "claude-opus-4-6[1m]",
            "claude-sonnet-4-6-20260217",
            "anthropic/claude-opus-5-5",
            "us.anthropic.claude-opus-4-6-v1:0",
            "claude-opus-6",
        ] {
            assert!(model_supports_adaptive_thinking(model, None), "{model}");
        }
        for model in [
            "claude-opus-4-5",
            "claude-opus-4-5-20251101",
            "claude-sonnet-4-5-20250929",
            "claude-haiku-4-5-20251001",
            "claude-haiku-4-5",
            "us.anthropic.claude-haiku-4-5-20251001-v1:0",
            "claude-3-5-haiku-20241022",
            "claude-opus-4-1",
            "claude-opus-4-20250514",
            "claude-sonnet-4",
            "claude-3-7-sonnet-20250219",
            "us.anthropic.claude-sonnet-4-5-20250929-v1:0",
        ] {
            assert!(!model_supports_adaptive_thinking(model, None), "{model}");
        }
    }

    fn install_profile(model: &str, thinking: Option<&str>) {
        let mut capabilities =
            serde_json::json!({"contextWindow": 200000, "maxOutputTokens": 64000});
        if let Some(thinking) = thinking {
            capabilities["thinking"] = thinking.into();
        }
        crate::utils::settings::settings_cache::install_test_settings(
            serde_json::from_value(serde_json::json!({"nyan": {
                "providers": {"p": {"kind": "anthropic", "protocol": "anthropicMessages"}},
                "models": {
                    "main": {"provider": "p", "model": model, "capabilities": capabilities},
                    "side": {"provider": "p", "model": "claude-opus-4-5",
                        "capabilities": {"contextWindow": 200000, "maxOutputTokens": 64000,
                            "thinking": "adaptive"}}
                },
                "selection": {"main": "main"}
            }}))
            .unwrap(),
        );
    }

    struct ResetSettings;
    impl Drop for ResetSettings {
        fn drop(&mut self) {
            crate::utils::settings::settings_cache::reset_settings_cache();
        }
    }

    #[test]
    fn a_profile_declared_thinking_wins_over_the_built_in_table() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let _reset = ResetSettings;

        // `budgeted` keeps a table-adaptive model on a budget.
        install_profile("claude-opus-5-5", Some("budgeted"));
        assert!(!model_supports_adaptive_thinking("claude-opus-5-5", None));
        assert!(!model_supports_adaptive_thinking(
            "claude-opus-5-5",
            Some("main")
        ));
        assert!(model_supports_thinking("claude-opus-5-5", None));

        // `adaptive` is believed for a model the table does not know.
        install_profile("gateway-reasoner", Some("adaptive"));
        assert!(model_supports_adaptive_thinking("gateway-reasoner", None));
        assert!(model_supports_thinking("gateway-reasoner", None));

        // `none` turns thinking off for a model that has it.
        install_profile("claude-opus-4-6", Some("none"));
        assert!(!model_supports_thinking("claude-opus-4-6", None));
        assert!(!model_supports_adaptive_thinking("claude-opus-4-6", None));

        // A named profile answers for its own model.
        assert!(model_supports_adaptive_thinking(
            "claude-opus-4-5",
            Some("side")
        ));
    }

    #[test]
    fn a_profile_without_thinking_or_for_another_model_falls_back_to_the_table() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let _reset = ResetSettings;

        install_profile("claude-opus-5-5", None);
        assert!(model_supports_adaptive_thinking("claude-opus-5-5", None));

        // A request that fell back to another model is not judged by the
        // profile's declaration.
        install_profile("claude-opus-5-5", Some("budgeted"));
        assert!(model_supports_adaptive_thinking("claude-sonnet-5-5", None));
        assert!(!model_supports_adaptive_thinking("claude-opus-4-5", None));
    }

    #[test]
    fn a_profile_declaration_wins_over_model_name_heuristics() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let _reset = ResetSettings;

        install_profile("claude-opus-5-5", Some("adaptive"));
        assert!(model_supports_adaptive_thinking("claude-opus-5-5", None));

        install_profile("claude-opus-5-5", Some("none"));
        assert!(!model_supports_thinking("claude-opus-5-5", None));

        // Without a declaration the version table decides.
        install_profile("claude-opus-5-5", None);
        assert!(model_supports_adaptive_thinking("claude-opus-5-5", None));
        assert!(model_supports_thinking("claude-opus-5-5", None));
    }

    #[test]
    fn should_enable_thinking_by_default_matches_official() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("MAX_THINKING_TOKENS");

        assert!(should_enable_thinking_by_default(&SettingsJson::default()));

        let mut settings = SettingsJson::default();
        settings.always_thinking_enabled = Some(false);
        assert!(!should_enable_thinking_by_default(&settings));

        settings.always_thinking_enabled = Some(true);
        assert!(should_enable_thinking_by_default(&settings));

        crate::utils::process_env::set("MAX_THINKING_TOKENS", "100");
        assert!(should_enable_thinking_by_default(&settings));

        crate::utils::process_env::set("MAX_THINKING_TOKENS", "0");
        assert!(!should_enable_thinking_by_default(&SettingsJson::default()));

        crate::utils::process_env::set("MAX_THINKING_TOKENS", "invalid");
        assert!(!should_enable_thinking_by_default(&SettingsJson::default()));

        crate::utils::process_env::set("MAX_THINKING_TOKENS", "");
        assert!(should_enable_thinking_by_default(&SettingsJson::default()));
        crate::utils::process_env::remove("MAX_THINKING_TOKENS");
    }

    #[test]
    fn production_thinking_config_from_settings_matches_main_without_thinking_flag() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("MAX_THINKING_TOKENS");
        crate::utils::process_env::remove("NYAN_THINKING");
        crate::utils::process_env::remove("NYAN_THINKING");

        assert!(matches!(
            production_thinking_config_from_env_and_settings(&SettingsJson::default()),
            ThinkingConfig::Adaptive
        ));

        let mut settings = SettingsJson::default();
        settings.always_thinking_enabled = Some(false);
        assert!(matches!(
            production_thinking_config_from_env_and_settings(&settings),
            ThinkingConfig::Disabled
        ));

        crate::utils::process_env::set("MAX_THINKING_TOKENS", "2048");
        assert!(matches!(
            production_thinking_config_from_env_and_settings(&settings),
            ThinkingConfig::Enabled {
                budget_tokens: Some(2048)
            }
        ));

        crate::utils::process_env::set("MAX_THINKING_TOKENS", "0");
        assert!(matches!(
            production_thinking_config_from_env_and_settings(&SettingsJson::default()),
            ThinkingConfig::Disabled
        ));

        crate::utils::process_env::set("MAX_THINKING_TOKENS", "invalid");
        assert!(matches!(
            production_thinking_config_from_env_and_settings(&SettingsJson::default()),
            ThinkingConfig::Disabled
        ));
        crate::utils::process_env::remove("MAX_THINKING_TOKENS");
    }
}
