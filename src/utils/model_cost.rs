//! Per-model pricing tables and display formatting.
//! Maps to CC `utils/modelCost.ts`.
//!
//! TODO(parity): the token-accounting half of the source (`tokensToUSDCost`
//! `:131-142`, `getModelCosts` `:144-164`, `calculateUSDCost` `:177-180`,
//! `calculateCostFromTokens` `:186-202`) is unported. `getModelCosts`'
//! unknown-model path calls `bootstrap/state.ts#setHasUnknownModelCost`
//! (`:172`), which has no Rust counterpart, and `src/cost_tracker.rs` only
//! accumulates USD computed elsewhere — so nothing here would consume it yet.
//! The pricing tables and formatters below are complete.

use crate::utils::model::configs::{
    CLAUDE_3_5_HAIKU_CONFIG, CLAUDE_3_5_V2_SONNET_CONFIG, CLAUDE_3_7_SONNET_CONFIG,
    CLAUDE_HAIKU_4_5_CONFIG, CLAUDE_OPUS_4_1_CONFIG, CLAUDE_OPUS_4_5_CONFIG,
    CLAUDE_OPUS_4_6_CONFIG, CLAUDE_OPUS_4_CONFIG, CLAUDE_SONNET_4_5_CONFIG,
    CLAUDE_SONNET_4_6_CONFIG, CLAUDE_SONNET_4_CONFIG,
};

/// Maps to: CC `utils/modelCost.ts:27-33` `ModelCosts`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModelCosts {
    pub input_tokens: f64,
    pub output_tokens: f64,
    pub prompt_cache_write_tokens: f64,
    pub prompt_cache_read_tokens: f64,
    pub web_search_requests: f64,
}

/// Maps to: CC `utils/modelCost.ts:36-42` `COST_TIER_3_15` — the standard
/// Sonnet tier, $3 input / $15 output per Mtok.
pub const COST_TIER_3_15: ModelCosts = ModelCosts {
    input_tokens: 3.0,
    output_tokens: 15.0,
    prompt_cache_write_tokens: 3.75,
    prompt_cache_read_tokens: 0.3,
    web_search_requests: 0.01,
};

/// Maps to: CC `utils/modelCost.ts:45-51` `COST_TIER_15_75` — Opus 4/4.1.
pub const COST_TIER_15_75: ModelCosts = ModelCosts {
    input_tokens: 15.0,
    output_tokens: 75.0,
    prompt_cache_write_tokens: 18.75,
    prompt_cache_read_tokens: 1.5,
    web_search_requests: 0.01,
};

/// Maps to: CC `utils/modelCost.ts:54-60` `COST_TIER_5_25` — Opus 4.5.
pub const COST_TIER_5_25: ModelCosts = ModelCosts {
    input_tokens: 5.0,
    output_tokens: 25.0,
    prompt_cache_write_tokens: 6.25,
    prompt_cache_read_tokens: 0.5,
    web_search_requests: 0.01,
};

/// Maps to: CC `utils/modelCost.ts:63-69` `COST_TIER_30_150` — Opus 4.6 in
/// fast mode.
pub const COST_TIER_30_150: ModelCosts = ModelCosts {
    input_tokens: 30.0,
    output_tokens: 150.0,
    prompt_cache_write_tokens: 37.5,
    prompt_cache_read_tokens: 3.0,
    web_search_requests: 0.01,
};

/// Maps to: CC `utils/modelCost.ts:72-78` `COST_HAIKU_35`.
pub const COST_HAIKU_35: ModelCosts = ModelCosts {
    input_tokens: 0.8,
    output_tokens: 4.0,
    prompt_cache_write_tokens: 1.0,
    prompt_cache_read_tokens: 0.08,
    web_search_requests: 0.01,
};

/// Maps to: CC `utils/modelCost.ts:81-87` `COST_HAIKU_45`.
pub const COST_HAIKU_45: ModelCosts = ModelCosts {
    input_tokens: 1.0,
    output_tokens: 5.0,
    prompt_cache_write_tokens: 1.25,
    prompt_cache_read_tokens: 0.1,
    web_search_requests: 0.01,
};

/// Maps to: CC `utils/modelCost.ts:94-99` `getOpus46CostTier`.
pub fn get_opus_46_cost_tier(fast_mode: bool) -> ModelCosts {
    if crate::utils::fast_mode::is_fast_mode_enabled() && fast_mode {
        return COST_TIER_30_150;
    }
    COST_TIER_5_25
}

/// Maps to: CC `utils/modelCost.ts:104-126` `MODEL_COSTS`, keyed by the
/// canonical name of each config's `firstParty` ID.
fn model_costs() -> [(String, ModelCosts); 11] {
    let canonical = crate::utils::model::model::first_party_name_to_canonical;
    [
        (
            canonical(CLAUDE_3_5_HAIKU_CONFIG.first_party),
            COST_HAIKU_35,
        ),
        (
            canonical(CLAUDE_HAIKU_4_5_CONFIG.first_party),
            COST_HAIKU_45,
        ),
        (
            canonical(CLAUDE_3_5_V2_SONNET_CONFIG.first_party),
            COST_TIER_3_15,
        ),
        (
            canonical(CLAUDE_3_7_SONNET_CONFIG.first_party),
            COST_TIER_3_15,
        ),
        (
            canonical(CLAUDE_SONNET_4_CONFIG.first_party),
            COST_TIER_3_15,
        ),
        (
            canonical(CLAUDE_SONNET_4_5_CONFIG.first_party),
            COST_TIER_3_15,
        ),
        (
            canonical(CLAUDE_SONNET_4_6_CONFIG.first_party),
            COST_TIER_3_15,
        ),
        (canonical(CLAUDE_OPUS_4_CONFIG.first_party), COST_TIER_15_75),
        (
            canonical(CLAUDE_OPUS_4_1_CONFIG.first_party),
            COST_TIER_15_75,
        ),
        (
            canonical(CLAUDE_OPUS_4_5_CONFIG.first_party),
            COST_TIER_5_25,
        ),
        (
            canonical(CLAUDE_OPUS_4_6_CONFIG.first_party),
            COST_TIER_5_25,
        ),
    ]
}

fn lookup_model_costs(short_name: &str) -> Option<ModelCosts> {
    model_costs()
        .into_iter()
        .find_map(|(key, costs)| (key == short_name).then_some(costs))
}

/// Maps to: CC `utils/modelCost.ts:204-211` `formatPrice` — integers without
/// decimals, everything else to two places.
fn format_price(price: f64) -> String {
    if price.fract() == 0.0 {
        return format!("${}", price as i64);
    }
    format!("${price:.2}")
}

/// Maps to: CC `utils/modelCost.ts:217-219` `formatModelPricing`.
pub fn format_model_pricing(costs: ModelCosts) -> String {
    format!(
        "{}/{} per Mtok",
        format_price(costs.input_tokens),
        format_price(costs.output_tokens)
    )
}

/// Maps to: CC `utils/modelCost.ts:226-231` `getModelPricingString`.
pub fn get_model_pricing_string(model: &str) -> Option<String> {
    let short_name = crate::utils::model::model::get_canonical_name(model);
    lookup_model_costs(&short_name).map(format_model_pricing)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::TEST_ENV_LOCK;

    /// Maps to: CC `utils/modelCost.ts:204-219`. The `$3`/`$0.80` split is what
    /// the model picker and ConfigTool prompt render, so pin the exact copy.
    #[test]
    fn pricing_strings_match_the_official_integer_and_two_decimal_split() {
        assert_eq!(format_model_pricing(COST_TIER_3_15), "$3/$15 per Mtok");
        assert_eq!(format_model_pricing(COST_TIER_5_25), "$5/$25 per Mtok");
        assert_eq!(format_model_pricing(COST_TIER_15_75), "$15/$75 per Mtok");
        assert_eq!(format_model_pricing(COST_TIER_30_150), "$30/$150 per Mtok");
        assert_eq!(format_model_pricing(COST_HAIKU_35), "$0.80/$4 per Mtok");
        assert_eq!(format_model_pricing(COST_HAIKU_45), "$1/$5 per Mtok");
    }

    /// Maps to: CC `utils/modelCost.ts:94-99` — fast pricing applies only when
    /// the caller asks for it *and* the feature gate is on.
    #[test]
    fn opus_46_cost_tier_follows_the_official_fast_mode_gate() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("CLAUDE_CODE_DISABLE_FAST_MODE");
        assert_eq!(get_opus_46_cost_tier(true), COST_TIER_30_150);
        assert_eq!(get_opus_46_cost_tier(false), COST_TIER_5_25);

        crate::utils::process_env::set("CLAUDE_CODE_DISABLE_FAST_MODE", "1");
        assert_eq!(get_opus_46_cost_tier(true), COST_TIER_5_25);
        crate::utils::process_env::remove("CLAUDE_CODE_DISABLE_FAST_MODE");
    }

    /// Maps to: CC `utils/modelCost.ts:104-126` — the table is keyed by
    /// canonical name, so full IDs and provider-prefixed IDs both resolve.
    #[test]
    fn model_cost_table_covers_every_official_config() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        assert_eq!(model_costs().len(), 11);
        assert_eq!(
            get_model_pricing_string("claude-opus-4-1-20250805").as_deref(),
            Some("$15/$75 per Mtok")
        );
        assert_eq!(
            get_model_pricing_string("us.anthropic.claude-haiku-4-5-20251001-v1:0").as_deref(),
            Some("$1/$5 per Mtok")
        );
        assert_eq!(get_model_pricing_string("gpt-4").as_deref(), None);
    }
}
