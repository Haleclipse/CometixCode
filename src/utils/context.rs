//! Context window helpers.
//! Maps to CC `utils/context.ts`.

pub const MODEL_CONTEXT_WINDOW_DEFAULT: i64 = 200_000;
pub const COMPACT_MAX_OUTPUT_TOKENS: i64 = 20_000;
pub const MAX_OUTPUT_TOKENS_DEFAULT: u32 = 32_000;
pub const MAX_OUTPUT_TOKENS_UPPER_LIMIT: u32 = 64_000;

/// Maps to CC `utils/context.ts` `CAPPED_DEFAULT_MAX_TOKENS`.
pub const CAPPED_DEFAULT_MAX_TOKENS: u32 = 8_000;

/// Maps to CC `utils/context.ts` `ESCALATED_MAX_TOKENS`.
pub const ESCALATED_MAX_TOKENS: u32 = 64_000;

/// Maps to CC `utils/context.ts` `getModelMaxOutputTokens(...)` return shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelMaxOutputTokens {
    pub default: u32,
    pub upper_limit: u32,
}

/// Maps to CC `utils/context.ts` `getModelMaxOutputTokens(...)`.
pub fn get_model_max_output_tokens(model: &str) -> ModelMaxOutputTokens {
    let m = model.to_ascii_lowercase();
    let (default, upper_limit) = if m.contains("opus-4-6") {
        (64_000, 128_000)
    } else if m.contains("sonnet-4-6") {
        (32_000, 128_000)
    } else if m.contains("opus-4-5") || m.contains("sonnet-4") || m.contains("haiku-4") {
        (32_000, 64_000)
    } else if m.contains("opus-4-1") || m.contains("opus-4") {
        (32_000, 32_000)
    } else if m.contains("claude-3-opus") {
        (4_096, 4_096)
    } else if m.contains("claude-3-sonnet") {
        (8_192, 8_192)
    } else if m.contains("claude-3-haiku") {
        (4_096, 4_096)
    } else if m.contains("3-5-sonnet") || m.contains("3-5-haiku") {
        (8_192, 8_192)
    } else if m.contains("3-7-sonnet") {
        (32_000, 64_000)
    } else {
        (MAX_OUTPUT_TOKENS_DEFAULT, MAX_OUTPUT_TOKENS_UPPER_LIMIT)
    };

    ModelMaxOutputTokens {
        default,
        upper_limit,
    }
}

/// Maps to CC `utils/context.ts` `is1mContextDisabled()`.
pub fn is_1m_context_disabled() -> bool {
    crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_DISABLE_1M_CONTEXT")
            .as_deref(),
    )
}

/// Maps to CC `utils/context.ts` `has1mContext(...)`.
pub fn has_1m_context(model: &str) -> bool {
    !is_1m_context_disabled() && model.to_ascii_lowercase().contains("[1m]")
}

/// Maps to CC `utils/context.ts` `modelSupports1M(...)` for the public model
/// patterns Cometix currently supports.
pub fn model_supports_1m(model: &str) -> bool {
    if is_1m_context_disabled() {
        return false;
    }
    let canonical = model.to_ascii_lowercase();
    canonical.contains("claude-sonnet-4") || canonical.contains("opus-4-6")
}

/// Maps to CC `utils/context.ts` `calculateContextPercentages(...)`.
pub fn calculate_context_percentages(
    current_usage: Option<&crate::types::message::TokenUsage>,
    context_window_size: i64,
) -> (Option<u8>, Option<u8>) {
    let Some(usage) = current_usage else {
        return (None, None);
    };
    if context_window_size <= 0 {
        return (None, None);
    }
    let total_input_tokens =
        usage.input_tokens + usage.cache_creation_input_tokens + usage.cache_read_input_tokens;
    let used_percentage =
        ((total_input_tokens as f64 / context_window_size as f64) * 100.0).round() as i64;
    let clamped_used = used_percentage.clamp(0, 100) as u8;
    (Some(clamped_used), Some(100 - clamped_used))
}

/// Pure audience/environment form of CC `utils/context.ts#getContextWindowForModel`.
pub fn get_context_window_for_model_for_audience(
    model: &str,
    betas: &[String],
    audience: crate::utils::build_profile::BuildAudience,
    get_env: &impl Fn(&str) -> Option<String>,
) -> i64 {
    if crate::utils::build_profile::audience_has_internal_capability(
        audience,
        crate::utils::build_profile::InternalCapability::Context,
    ) {
        if let Some(value) = get_env("CLAUDE_CODE_MAX_CONTEXT_TOKENS") {
            if let Ok(tokens) = value.parse::<i64>() {
                if tokens > 0 {
                    return tokens;
                }
            }
        }
    }

    if has_1m_context(model) {
        return 1_000_000;
    }

    if betas
        .iter()
        .any(|beta| beta.to_ascii_lowercase().contains("1m"))
        && model_supports_1m(model)
    {
        return 1_000_000;
    }

    MODEL_CONTEXT_WINDOW_DEFAULT
}

/// Maps to CC `utils/context.ts#getContextWindowForModel`.
pub fn get_context_window_for_model(model: &str, betas: &[String]) -> i64 {
    get_context_window_for_model_for_audience(
        model,
        betas,
        crate::utils::build_profile::build_audience(),
        &|key| crate::utils::process_env::var(key),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::TEST_ENV_LOCK;

    #[test]
    fn model_max_output_tokens_matches_official_known_models() {
        assert_eq!(
            get_model_max_output_tokens("claude-opus-4-6"),
            ModelMaxOutputTokens {
                default: 64_000,
                upper_limit: 128_000,
            }
        );
        assert_eq!(
            get_model_max_output_tokens("claude-sonnet-4-6"),
            ModelMaxOutputTokens {
                default: 32_000,
                upper_limit: 128_000,
            }
        );
        assert_eq!(
            get_model_max_output_tokens("claude-3-haiku-20240307"),
            ModelMaxOutputTokens {
                default: 4_096,
                upper_limit: 4_096,
            }
        );
        assert_eq!(
            get_model_max_output_tokens("unknown"),
            ModelMaxOutputTokens {
                default: MAX_OUTPUT_TOKENS_DEFAULT,
                upper_limit: MAX_OUTPUT_TOKENS_UPPER_LIMIT,
            }
        );
    }

    #[test]
    fn context_window_defaults_to_official_200k() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("CLAUDE_CODE_DISABLE_1M_CONTEXT");
        crate::utils::process_env::remove("CLAUDE_CODE_MAX_CONTEXT_TOKENS");

        assert_eq!(
            get_context_window_for_model("claude-sonnet-4-20250514", &[]),
            MODEL_CONTEXT_WINDOW_DEFAULT
        );
    }

    #[test]
    fn context_window_honors_internal_override_and_1m_suffix() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("CLAUDE_CODE_DISABLE_1M_CONTEXT");
        assert_eq!(
            get_context_window_for_model_for_audience(
                "claude-sonnet-4-20250514[1m]",
                &[],
                crate::utils::build_profile::BuildAudience::AnthropicInternal,
                &|key| (key == "CLAUDE_CODE_MAX_CONTEXT_TOKENS").then(|| "123456".to_string()),
            ),
            123_456
        );

        assert_eq!(
            get_context_window_for_model_for_audience(
                "claude-sonnet-4-20250514[1m]",
                &[],
                crate::utils::build_profile::BuildAudience::AnthropicInternal,
                &|_| None,
            ),
            1_000_000
        );

        crate::utils::process_env::set("CLAUDE_CODE_DISABLE_1M_CONTEXT", "1");
        assert_eq!(
            get_context_window_for_model("claude-sonnet-4-20250514[1m]", &[]),
            MODEL_CONTEXT_WINDOW_DEFAULT
        );
        crate::utils::process_env::remove("CLAUDE_CODE_DISABLE_1M_CONTEXT");
    }
}
