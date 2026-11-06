//! Anthropic mapping of the unified effort (`off < medium < high < max`).
//!
//! `output_config.effort` exists only on newer models. Older ones get a thinking
//! budget scaled by the tier instead, and tiers a model does not accept are
//! clamped to the nearest one it does, silently (provider-redesign §5.2).

use crate::utils::effort::Effort;
use crate::utils::thinking::ThinkingConfig;

/// Thinking budget for `medium` / `high` on models without adaptive thinking.
/// `max` takes the model's full budget (`None` lets the caller derive it).
const MEDIUM_THINKING_BUDGET: i64 = 8_192;
const HIGH_THINKING_BUDGET: i64 = 24_576;

/// Model ids that predate `output_config.effort`, matched as substrings so
/// dated ids, Bedrock / Vertex prefixes and `[1m]` suffixes all resolve. The
/// family is named by its full version: `claude-opus-4-1` and `claude-opus-4-5`
/// must not be mistaken for `claude-opus-4-6` and later.
const WITHOUT_EFFORT_PARAM: &[&str] = &[
    "claude-3-",
    "claude-opus-4-0",
    "claude-opus-4-1",
    "claude-opus-4-2025",
    "claude-sonnet-4-0",
    "claude-sonnet-4-5",
    "claude-sonnet-4-2025",
    "claude-haiku-4-5",
];

/// Opus 4.5 takes effort but has no `max` tier.
const WITHOUT_MAX_TIER: &str = "claude-opus-4-5";

/// The `output_config.effort` value for `effort`, or `None` when the request
/// must not carry one: thinking is off, or the model has no such parameter.
pub fn output_config_effort(model: &str, effort: Effort) -> Option<&'static str> {
    let id = model.to_ascii_lowercase();
    if effort.is_off() || WITHOUT_EFFORT_PARAM.iter().any(|old| id.contains(old)) {
        return None;
    }
    Some(match effort {
        Effort::Max if id.contains(WITHOUT_MAX_TIER) => Effort::High.as_str(),
        other => other.as_str(),
    })
}

/// The thinking configuration an effort asks for. Adaptive models decide the
/// depth themselves (the tier rides on `output_config.effort`); the others get
/// a budget scaled by the tier.
pub fn thinking_for_effort(effort: Effort, adaptive: bool) -> ThinkingConfig {
    match effort {
        Effort::Off => ThinkingConfig::Disabled,
        _ if adaptive => ThinkingConfig::Adaptive,
        Effort::Medium => ThinkingConfig::Enabled {
            budget_tokens: Some(MEDIUM_THINKING_BUDGET),
        },
        Effort::High => ThinkingConfig::Enabled {
            budget_tokens: Some(HIGH_THINKING_BUDGET),
        },
        Effort::Max => ThinkingConfig::Enabled {
            budget_tokens: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_never_sends_the_effort_parameter() {
        for model in ["claude-opus-4-6", "claude-sonnet-4-6", "claude-haiku-4-5"] {
            assert_eq!(output_config_effort(model, Effort::Off), None, "{model}");
        }
    }

    #[test]
    fn models_without_the_parameter_never_receive_it() {
        for model in [
            "claude-haiku-4-5",
            "claude-haiku-4-5-20251001",
            "claude-sonnet-4-5-20250929",
            "claude-sonnet-4-20250514",
            "claude-opus-4-1-20250805",
            "claude-opus-4-20250514",
            "claude-3-5-haiku-latest",
            "claude-3-7-sonnet-20250219",
        ] {
            for effort in [Effort::Medium, Effort::High, Effort::Max] {
                assert_eq!(output_config_effort(model, effort), None, "{model}");
            }
        }
    }

    #[test]
    fn supported_models_get_the_tier_as_written() {
        for model in ["claude-opus-4-6", "claude-sonnet-4-6", "claude-opus-4-8"] {
            assert_eq!(output_config_effort(model, Effort::Medium), Some("medium"));
            assert_eq!(output_config_effort(model, Effort::High), Some("high"));
            assert_eq!(output_config_effort(model, Effort::Max), Some("max"));
        }
    }

    #[test]
    fn max_clamps_to_high_on_opus_4_5() {
        assert_eq!(
            output_config_effort("claude-opus-4-5-20251101", Effort::Max),
            Some("high")
        );
        assert_eq!(
            output_config_effort("claude-opus-4-5-20251101", Effort::Medium),
            Some("medium")
        );
    }

    #[test]
    fn adaptive_models_ride_the_tier_on_the_parameter_not_a_budget() {
        for effort in [Effort::Medium, Effort::High, Effort::Max] {
            assert_eq!(thinking_for_effort(effort, true), ThinkingConfig::Adaptive);
        }
        assert_eq!(
            thinking_for_effort(Effort::Off, true),
            ThinkingConfig::Disabled
        );
    }

    #[test]
    fn budget_models_scale_the_budget_by_tier() {
        assert_eq!(
            thinking_for_effort(Effort::Off, false),
            ThinkingConfig::Disabled
        );
        assert_eq!(
            thinking_for_effort(Effort::Medium, false),
            ThinkingConfig::Enabled {
                budget_tokens: Some(8_192)
            }
        );
        assert_eq!(
            thinking_for_effort(Effort::High, false),
            ThinkingConfig::Enabled {
                budget_tokens: Some(24_576)
            }
        );
        assert_eq!(
            thinking_for_effort(Effort::Max, false),
            ThinkingConfig::Enabled {
                budget_tokens: None
            }
        );
    }
}
