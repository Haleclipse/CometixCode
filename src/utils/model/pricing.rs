//! Per-request price resolution for cost accounting.
//!
//! @nyan: replaces CC `utils/modelCost.ts` (hardcoded Anthropic tiers). Prices
//! come from the model metadata catalog by (provider kind, model id); a
//! profile's `cost` overrides any field; local servers are free; anything else
//! without a price is unknown. Tiered (context-length) pricing is not modeled.

use crate::services::provider::ir::Usage;
use crate::utils::settings::nyan::{ModelCostSettings, ProviderKind};

/// USD per million tokens. Cache prices are always filled in: a missing cache
/// price falls back to the plain input price.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelPrice {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    /// 5-minute (or provider-default) cache write.
    pub cache_write: f64,
}

impl ModelPrice {
    const FREE: Self = Self {
        input: 0.0,
        output: 0.0,
        cache_read: 0.0,
        cache_write: 0.0,
    };

    /// Cost of one request. Reasoning tokens are already part of
    /// `output_tokens`, so they are not priced again. Anthropic 1h cache
    /// writes are billed at twice the input price.
    pub fn cost_usd(&self, usage: &Usage) -> f64 {
        let write_1h = usage.cache_write_1h_tokens.min(usage.cache_write_tokens);
        let write_5m = usage.cache_write_tokens - write_1h;
        let per_token = |tokens: u64, price: f64| tokens as f64 * price / 1_000_000.0;
        per_token(usage.input_tokens, self.input)
            + per_token(usage.output_tokens, self.output)
            + per_token(usage.cache_read_tokens, self.cache_read)
            + per_token(write_5m, self.cache_write)
            + per_token(write_1h, self.input * 2.0)
    }
}

/// Price for `model` on a provider of `kind`, with the profile's `cost`
/// fields layered over the catalog entry. `None` means the price is unknown.
pub fn resolve_price(
    kind: Option<ProviderKind>,
    model: &str,
    profile_cost: Option<&ModelCostSettings>,
) -> Option<ModelPrice> {
    let base = match kind {
        Some(kind) if kind.is_local() => Some(ModelPrice::FREE),
        Some(kind) => crate::utils::model::metadata::metadata_provider_for_kind(kind)
            .and_then(|provider| {
                crate::utils::model::metadata::find_model_metadata(provider, model)
            })
            .and_then(|entry| entry.cost.as_ref())
            .map(|cost| ModelPrice {
                input: cost.input,
                output: cost.output,
                cache_read: cost.cache_read.unwrap_or(cost.input),
                cache_write: cost.cache_write.unwrap_or(cost.input),
            }),
        None => None,
    };
    let Some(cost) = profile_cost else {
        return base;
    };
    let input = cost.input.or(base.map(|price| price.input))?;
    let output = cost.output.or(base.map(|price| price.output))?;
    Some(ModelPrice {
        input,
        output,
        cache_read: cost
            .cache_read
            .or(base.map(|price| price.cache_read))
            .unwrap_or(input),
        cache_write: cost
            .cache_write
            .or(base.map(|price| price.cache_write))
            .unwrap_or(input),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(input: u64, output: u64, read: u64, write: u64, write_1h: u64) -> Usage {
        Usage {
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: read,
            cache_write_tokens: write,
            cache_write_1h_tokens: write_1h,
            ..Usage::default()
        }
    }

    #[test]
    fn catalog_hit_prices_every_bucket() {
        let price = resolve_price(Some(ProviderKind::Anthropic), "claude-opus-4-5", None).unwrap();
        assert_eq!(
            price,
            ModelPrice {
                input: 5.0,
                output: 25.0,
                cache_read: 0.5,
                cache_write: 6.25,
            }
        );
        let cost = price.cost_usd(&usage(1_000_000, 1_000_000, 1_000_000, 1_000_000, 0));
        assert!((cost - 36.75).abs() < 1e-9);
    }

    #[test]
    fn missing_catalog_cache_prices_fall_back_to_input() {
        // The catalog has no cacheWrite price for gpt-5.
        let price = resolve_price(Some(ProviderKind::Openai), "gpt-5", None).unwrap();
        assert_eq!(price.cache_read, 0.125);
        assert_eq!(price.cache_write, price.input);
    }

    #[test]
    fn profile_cost_overrides_per_field() {
        let cost = ModelCostSettings {
            output: Some(30.0),
            ..Default::default()
        };
        let price = resolve_price(
            Some(ProviderKind::Anthropic),
            "claude-opus-4-5",
            Some(&cost),
        )
        .unwrap();
        assert_eq!(price.input, 5.0);
        assert_eq!(price.output, 30.0);
        assert_eq!(price.cache_read, 0.5);
        assert_eq!(price.cache_write, 6.25);
    }

    #[test]
    fn handwritten_gateway_cost_fills_missing_cache_prices_from_input() {
        assert_eq!(
            resolve_price(Some(ProviderKind::Gateway), "claude-opus-4-5", None),
            None
        );
        let cost = ModelCostSettings {
            input: Some(2.0),
            output: Some(8.0),
            ..Default::default()
        };
        let price = resolve_price(Some(ProviderKind::Gateway), "anything", Some(&cost)).unwrap();
        assert_eq!(price.cache_read, 2.0);
        assert_eq!(price.cache_write, 2.0);
        // Input alone is not a price.
        let partial = ModelCostSettings {
            input: Some(2.0),
            ..Default::default()
        };
        assert_eq!(
            resolve_price(Some(ProviderKind::Gateway), "anything", Some(&partial)),
            None
        );
    }

    #[test]
    fn local_kinds_are_free_and_unknown_models_have_no_price() {
        let price = resolve_price(Some(ProviderKind::Ollama), "qwen3", None).unwrap();
        assert_eq!(price.cost_usd(&usage(1_000, 1_000, 0, 0, 0)), 0.0);
        assert_eq!(
            resolve_price(Some(ProviderKind::Vllm), "x", None),
            Some(ModelPrice::FREE)
        );
        assert_eq!(
            resolve_price(Some(ProviderKind::Anthropic), "no-such-model", None),
            None
        );
        assert_eq!(resolve_price(None, "claude-opus-4-5", None), None);
    }

    #[test]
    fn one_hour_cache_writes_cost_twice_input_and_five_minute_use_cache_write() {
        let price = resolve_price(Some(ProviderKind::Anthropic), "claude-opus-4-5", None).unwrap();
        let five_minute = price.cost_usd(&usage(0, 0, 0, 1_000_000, 0));
        let one_hour = price.cost_usd(&usage(0, 0, 0, 1_000_000, 1_000_000));
        let mixed = price.cost_usd(&usage(0, 0, 0, 1_000_000, 400_000));
        assert!((five_minute - 6.25).abs() < 1e-9);
        assert!((one_hour - 10.0).abs() < 1e-9);
        assert!((mixed - (0.6 * 6.25 + 0.4 * 10.0)).abs() < 1e-9);
    }

    #[test]
    fn reasoning_tokens_are_not_priced_twice() {
        let price = resolve_price(Some(ProviderKind::Anthropic), "claude-opus-4-5", None).unwrap();
        let plain = price.cost_usd(&usage(0, 1_000, 0, 0, 0));
        let with_reasoning = price.cost_usd(&Usage {
            reasoning_tokens: 800,
            ..usage(0, 1_000, 0, 0, 0)
        });
        assert_eq!(plain, with_reasoning);
    }
}
