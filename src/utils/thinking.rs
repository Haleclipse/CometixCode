//! Thinking configuration helpers.
//! Maps to CC `utils/thinking.ts`.

use crate::utils::model::model::get_canonical_name;
use crate::utils::model::model_support_overrides::{
    ModelCapabilityOverride, get_3p_model_capability_override,
};
use crate::utils::model::providers::{ApiProvider, get_api_provider};
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

/// Maps to: CC `utils/thinking.ts:88-109` `modelSupportsThinking(model)`.
///
/// Provider-aware thinking support detection (CC: "aligns with
/// modelSupportsISP in betas.ts"):
///
/// 1. 3P capability override pinned via `ANTHROPIC_DEFAULT_*_MODEL` wins.
/// 2. Internal builds: any configured ant model supports thinking
///    (`process.env.USER_TYPE === 'ant' && resolveAntModel(...)`; the L1
///    build-profile gate lives inside `resolve_ant_model`).
/// 3. 1P and Foundry: every model that is not `claude-3-` (including Haiku
///    4.5 and custom gateway model names).
/// 4. 3P (Bedrock/Vertex): only Opus 4+ and Sonnet 4+.
pub fn model_supports_thinking(model: &str) -> bool {
    if let Some(supported_3p) =
        get_3p_model_capability_override(model, ModelCapabilityOverride::Thinking)
    {
        return supported_3p;
    }
    if crate::utils::model::ant_models::resolve_ant_model(Some(&model.to_lowercase())).is_some() {
        return true;
    }
    // IMPORTANT (CC): Do not change thinking support without notifying the
    // model launch DRI and research. This can greatly affect model quality and
    // bashing.
    let canonical = get_canonical_name(model);
    let provider = get_api_provider();
    // 1P and Foundry: all Claude 4+ models (including Haiku 4.5)
    if matches!(provider, ApiProvider::Foundry | ApiProvider::FirstParty) {
        return !canonical.contains("claude-3-");
    }
    // 3P (Bedrock/Vertex): only Opus 4+ and Sonnet 4+
    canonical.contains("sonnet-4") || canonical.contains("opus-4")
}

/// Maps to: CC `utils/thinking.ts:112-144` `modelSupportsAdaptiveThinking(model)`.
///
/// - 3P capability override wins.
/// - Opus 4.6 / Sonnet 4.6 variants: `true`.
/// - Any other known opus/sonnet/haiku string: `false`.
/// - Unknown model strings default to `true` on 1P and Foundry (Foundry is a
///   proxy; newer models are trained on adaptive thinking) and `false` on the
///   other 3P providers, whose model strings have different formats.
pub fn model_supports_adaptive_thinking(model: &str) -> bool {
    if let Some(supported_3p) =
        get_3p_model_capability_override(model, ModelCapabilityOverride::AdaptiveThinking)
    {
        return supported_3p;
    }
    let canonical = get_canonical_name(model);
    // Supported by a subset of Claude 4 models
    if canonical.contains("opus-4-6") || canonical.contains("sonnet-4-6") {
        return true;
    }
    // Exclude any other known legacy models (allowlist above catches 4-6 variants first)
    if canonical.contains("opus") || canonical.contains("sonnet") || canonical.contains("haiku") {
        return false;
    }
    let provider = get_api_provider();
    matches!(provider, ApiProvider::FirstParty | ApiProvider::Foundry)
}

/// Non-REPL / missing-context fallback for `ThinkingConfig`.
///
/// Maps to: CC `main.tsx:3515-3541` **without** `--thinking` override —
/// `shouldEnableThinkingByDefault` then optional `MAX_THINKING_TOKENS` budget.
/// Interactive REPL always seeds `ToolUseContext.thinking_config` from
/// `ReplProps`; this is only for call sites that never received launch props.
///
/// Does **not** read invented envs (`CLAUDE_CODE_THINKING` / `COMETIX_THINKING`)
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
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

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

    fn clear_provider_and_pin_env() -> Vec<EnvVarGuard> {
        vec![
            EnvVarGuard::unset("CLAUDE_CODE_USE_BEDROCK"),
            EnvVarGuard::unset("CLAUDE_CODE_USE_VERTEX"),
            EnvVarGuard::unset("CLAUDE_CODE_USE_FOUNDRY"),
            EnvVarGuard::unset("ANTHROPIC_DEFAULT_OPUS_MODEL"),
            EnvVarGuard::unset("ANTHROPIC_DEFAULT_SONNET_MODEL"),
            EnvVarGuard::unset("ANTHROPIC_DEFAULT_HAIKU_MODEL"),
        ]
    }

    #[test]
    fn model_supports_thinking_matches_official_first_party_rule() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let _env = clear_provider_and_pin_env();
        // 1P: everything that is not claude-3-, including Haiku 4.5 and
        // gateway-custom names.
        assert!(model_supports_thinking("claude-haiku-4-5-20251001"));
        assert!(model_supports_thinking("claude-sonnet-4-5"));
        assert!(model_supports_thinking("deepseek-v4-flash"));
        assert!(!model_supports_thinking("claude-3-5-sonnet-20241022"));
    }

    #[test]
    fn model_supports_thinking_matches_official_bedrock_rule_and_override() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let _env = clear_provider_and_pin_env();
        let _bedrock = EnvVarGuard::set("CLAUDE_CODE_USE_BEDROCK", "1");
        // 3P: only Opus 4+ / Sonnet 4+.
        assert!(model_supports_thinking(
            "us.anthropic.claude-sonnet-4-20250514-v1:0"
        ));
        assert!(!model_supports_thinking(
            "us.anthropic.claude-haiku-4-5-20251001-v1:0"
        ));
        assert!(!model_supports_thinking("custom-gateway-model"));
        // Pinned capability override wins in both directions.
        let _pinned = EnvVarGuard::set("ANTHROPIC_DEFAULT_HAIKU_MODEL", "custom-gateway-model");
        let _caps = EnvVarGuard::set(
            "ANTHROPIC_DEFAULT_HAIKU_MODEL_SUPPORTED_CAPABILITIES",
            "thinking",
        );
        assert!(model_supports_thinking("custom-gateway-model"));
    }

    #[test]
    fn model_supports_adaptive_thinking_matches_official_defaults() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let _env = clear_provider_and_pin_env();
        assert!(model_supports_adaptive_thinking("claude-opus-4-6"));
        assert!(model_supports_adaptive_thinking("claude-sonnet-4-6"));
        assert!(!model_supports_adaptive_thinking(
            "claude-haiku-4-5-20251001"
        ));
        assert!(!model_supports_adaptive_thinking("claude-opus-4-5"));
        // Unknown strings default to true on 1P ...
        assert!(model_supports_adaptive_thinking("deepseek-v4-flash"));
        // ... and false on Bedrock/Vertex.
        let _bedrock = EnvVarGuard::set("CLAUDE_CODE_USE_BEDROCK", "1");
        assert!(!model_supports_adaptive_thinking("deepseek-v4-flash"));
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
        crate::utils::process_env::remove("CLAUDE_CODE_THINKING");
        crate::utils::process_env::remove("COMETIX_THINKING");

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
