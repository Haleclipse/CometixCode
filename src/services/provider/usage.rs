//! Unified provider usage and prompt-cache accounting.
//!
//! @nyan: provides normalized cross-provider cache usage parsing and
//! accounting matching Pi Agent's contracts.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::ir::Usage;

/// Per-turn misses at or below this are cache breakpoint granularity noise.
/// Maps to Pi Agent's `NOISE_FLOOR_TOKENS` in `cache-stats.ts`.
pub const CACHE_MISS_NOISE_FLOOR_TOKENS: u64 = 1024;

/// A counted cache miss on a single turn relative to the previous turn.
/// Maps to Pi Agent's `CacheMiss` in `cache-stats.ts`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct CacheMiss {
    /// Prompt tokens that were in the previous turn's prompt but not read from cache.
    pub missed_tokens: u64,
    /// Extra cost paid vs. a full cache hit ($ USD); 0.0 when pricing is unknown.
    pub missed_cost: f64,
}

impl Usage {
    /// Total tokens across all reported dimensions:
    /// `input (uncached) + output + cache_read + cache_write`.
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens
            .saturating_add(self.output_tokens)
            .saturating_add(self.cache_read_tokens)
            .saturating_add(self.cache_write_tokens)
    }

    /// Total prompt tokens presented to the model on this turn:
    /// `input (uncached) + cache_read + cache_write`.
    pub fn prompt_tokens(&self) -> u64 {
        self.input_tokens
            .saturating_add(self.cache_read_tokens)
            .saturating_add(self.cache_write_tokens)
    }

    /// Cache hit rate as a ratio in `[0.0, 1.0]`. Returns 0.0 when prompt tokens are 0.
    pub fn cache_hit_rate(&self) -> f64 {
        let prompt = self.prompt_tokens();
        if prompt > 0 {
            self.cache_read_tokens as f64 / prompt as f64
        } else {
            0.0
        }
    }

    /// Detect a cache miss relative to the previous turn's prompt tokens.
    ///
    /// Follows Pi Agent's `detectMiss` algorithm:
    /// `missedTokens = Math.min(prev.promptTokens, promptTokens) - usage.cacheRead`
    /// Returns `None` if `missed_tokens <= CACHE_MISS_NOISE_FLOOR_TOKENS` (1024)
    /// or if there was no prior request.
    pub fn detect_cache_miss(
        &self,
        prev_prompt_tokens: u64,
        price_per_million_input: f64,
        price_per_million_cache_read: f64,
    ) -> Option<CacheMiss> {
        let current_prompt = self.prompt_tokens();
        if prev_prompt_tokens == 0 || current_prompt == 0 {
            return None;
        }

        let expected_cached = prev_prompt_tokens.min(current_prompt);
        let missed_tokens = expected_cached.saturating_sub(self.cache_read_tokens);
        if missed_tokens <= CACHE_MISS_NOISE_FLOOR_TOKENS {
            return None;
        }

        let paid_rate = price_per_million_input / 1_000_000.0;
        let read_rate = price_per_million_cache_read / 1_000_000.0;
        let extra_rate = (paid_rate - read_rate).max(0.0);
        let missed_cost = (missed_tokens as f64) * extra_rate;

        Some(CacheMiss {
            missed_tokens,
            missed_cost,
        })
    }

    /// Parse OpenAI Chat Completions usage payload, handling standard OpenAI,
    /// DeepSeek, Kimi/Moonshot, and OpenRouter variants.
    ///
    /// In OpenAI semantics, `prompt_tokens` includes cached tokens.
    /// Net uncached input tokens are `prompt_tokens - cache_read - cache_write`.
    pub fn from_openai_chat(value: &Value) -> Self {
        let prompt_tokens = u64_at(value, "/prompt_tokens");
        let output_tokens = u64_at(value, "/completion_tokens");

        // Cache read (hits):
        // 1. OpenAI / OpenRouter standard: /prompt_tokens_details/cached_tokens
        // 2. DeepSeek: /prompt_cache_hit_tokens
        // 3. Kimi / Moonshot: top-level /cached_tokens
        let cache_read = first_u64_at(
            value,
            &[
                "/prompt_tokens_details/cached_tokens",
                "/prompt_cache_hit_tokens",
                "/cached_tokens",
            ],
        );

        // Cache write (creation):
        // 1. OpenRouter: /prompt_tokens_details/cache_write_tokens
        // 2. Extensions: /prompt_cache_write_tokens, /cache_write_tokens
        let cache_write = first_u64_at(
            value,
            &[
                "/prompt_tokens_details/cache_write_tokens",
                "/prompt_cache_write_tokens",
                "/cache_write_tokens",
            ],
        );

        // Reasoning tokens:
        // OpenAI standard: /completion_tokens_details/reasoning_tokens
        let reasoning = first_u64_at(
            value,
            &[
                "/completion_tokens_details/reasoning_tokens",
                "/reasoning_tokens",
            ],
        );

        // If prompt_tokens is 0 but DeepSeek gave hit + miss:
        let net_input = if prompt_tokens == 0 {
            first_u64_at(value, &["/prompt_cache_miss_tokens"])
        } else {
            prompt_tokens
                .saturating_sub(cache_read)
                .saturating_sub(cache_write)
        };

        Self {
            input_tokens: net_input,
            output_tokens,
            cache_read_tokens: cache_read,
            cache_write_tokens: cache_write,
            cache_deleted_tokens: 0,
            reasoning_tokens: reasoning,
        }
    }

    /// Parse OpenAI Responses (API and Codex) usage payload.
    ///
    /// In OpenAI Responses semantics, `input_tokens` includes cached tokens.
    pub fn from_openai_responses(value: &Value) -> Self {
        let input_tokens = u64_at(value, "/input_tokens");
        let output_tokens = u64_at(value, "/output_tokens");

        let cache_read = first_u64_at(
            value,
            &[
                "/input_tokens_details/cached_tokens",
                "/input_token_details/cached_tokens",
            ],
        );
        let cache_write = first_u64_at(
            value,
            &[
                "/input_tokens_details/cache_write_tokens",
                "/input_token_details/cache_write_tokens",
            ],
        );
        let reasoning = first_u64_at(
            value,
            &[
                "/output_tokens_details/reasoning_tokens",
                "/output_token_details/reasoning_tokens",
            ],
        );

        let net_input = input_tokens
            .saturating_sub(cache_read)
            .saturating_sub(cache_write);

        Self {
            input_tokens: net_input,
            output_tokens,
            cache_read_tokens: cache_read,
            cache_write_tokens: cache_write,
            cache_deleted_tokens: 0,
            reasoning_tokens: reasoning,
        }
    }

    /// Parse Google Gemini metadata counts.
    ///
    /// In Google Gemini semantics, `prompt_token_count` includes `cached_content_token_count`.
    pub fn from_gemini_metadata(
        prompt_token_count: Option<u64>,
        candidates_token_count: Option<u64>,
        cached_content_token_count: Option<u64>,
        thoughts_token_count: Option<u64>,
    ) -> Self {
        let prompt = prompt_token_count.unwrap_or(0);
        let cached = cached_content_token_count.unwrap_or(0);
        let output = candidates_token_count.unwrap_or(0);
        let thoughts = thoughts_token_count.unwrap_or(0);

        Self {
            input_tokens: prompt.saturating_sub(cached),
            output_tokens: output,
            cache_read_tokens: cached,
            cache_write_tokens: 0,
            cache_deleted_tokens: 0,
            reasoning_tokens: thoughts,
        }
    }

    /// Parse Google Gemini usage from raw JSON (`usageMetadata`).
    /// Supports both camelCase and snake_case keys.
    pub fn from_gemini_json(value: &Value) -> Self {
        let prompt = first_u64_at(value, &["/promptTokenCount", "/prompt_token_count"]);
        let output = first_u64_at(value, &["/candidatesTokenCount", "/candidates_token_count"]);
        let cached = first_u64_at(
            value,
            &["/cachedContentTokenCount", "/cached_content_token_count"],
        );
        let thoughts = first_u64_at(value, &["/thoughtsTokenCount", "/thoughts_token_count"]);

        Self::from_gemini_metadata(Some(prompt), Some(output), Some(cached), Some(thoughts))
    }

    /// Parse Anthropic Messages usage payload.
    ///
    /// In Anthropic semantics, `input_tokens` is already the net uncached count.
    pub fn from_anthropic_json(value: &Value) -> Self {
        let input = u64_at(value, "/input_tokens");
        let output = u64_at(value, "/output_tokens");
        let cache_read = u64_at(value, "/cache_read_input_tokens");
        let cache_write = first_u64_at(
            value,
            &[
                "/cache_creation_input_tokens",
                "/cache_creation/ephemeral_1h_input_tokens",
                "/cache_creation/ephemeral_5m_input_tokens",
            ],
        );
        let cache_deleted = u64_at(value, "/cache_deleted_input_tokens");

        Self {
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: cache_read,
            cache_write_tokens: cache_write,
            cache_deleted_tokens: cache_deleted,
            reasoning_tokens: 0,
        }
    }
}

/// Extract an unsigned integer from a JSON pointer path, returning 0 if missing.
pub fn u64_at(value: &Value, pointer: &str) -> u64 {
    value
        .pointer(pointer)
        .and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_i64().and_then(|i| u64::try_from(i).ok()))
        })
        .unwrap_or(0)
}

/// Try multiple pointer paths in order, returning the first non-zero match, or 0.
pub fn first_u64_at(value: &Value, pointers: &[&str]) -> u64 {
    for pointer in pointers {
        let val = u64_at(value, pointer);
        if val > 0 {
            return val;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn total_and_prompt_tokens_calculation() {
        let usage = Usage {
            input_tokens: 100,
            output_tokens: 25,
            cache_read_tokens: 80,
            cache_write_tokens: 10,
            cache_deleted_tokens: 0,
            reasoning_tokens: 15,
        };
        assert_eq!(usage.prompt_tokens(), 190);
        assert_eq!(usage.total_tokens(), 215);
        assert!((usage.cache_hit_rate() - (80.0 / 190.0)).abs() < 1e-6);
    }

    #[test]
    fn parse_openai_standard_usage() {
        let json = json!({
            "prompt_tokens": 100,
            "completion_tokens": 20,
            "total_tokens": 120,
            "prompt_tokens_details": {
                "cached_tokens": 40
            },
            "completion_tokens_details": {
                "reasoning_tokens": 7
            }
        });
        let usage = Usage::from_openai_chat(&json);
        assert_eq!(
            usage.input_tokens, 60,
            "cached tokens subtracted from input"
        );
        assert_eq!(usage.cache_read_tokens, 40);
        assert_eq!(usage.output_tokens, 20);
        assert_eq!(usage.reasoning_tokens, 7);
        assert_eq!(usage.cache_write_tokens, 0);
    }

    #[test]
    fn parse_deepseek_cache_hit_and_miss_usage() {
        let json = json!({
            "prompt_tokens": 1000,
            "completion_tokens": 150,
            "total_tokens": 1150,
            "prompt_cache_hit_tokens": 800,
            "prompt_cache_miss_tokens": 200
        });
        let usage = Usage::from_openai_chat(&json);
        assert_eq!(usage.input_tokens, 200);
        assert_eq!(usage.cache_read_tokens, 800);
        assert_eq!(usage.output_tokens, 150);
        assert_eq!(usage.cache_write_tokens, 0);
    }

    #[test]
    fn parse_kimi_moonshot_cached_tokens() {
        let json = json!({
            "prompt_tokens": 500,
            "completion_tokens": 50,
            "total_tokens": 550,
            "cached_tokens": 300
        });
        let usage = Usage::from_openai_chat(&json);
        assert_eq!(usage.input_tokens, 200);
        assert_eq!(usage.cache_read_tokens, 300);
        assert_eq!(usage.output_tokens, 50);
    }

    #[test]
    fn parse_openrouter_cache_write_and_read() {
        let json = json!({
            "prompt_tokens": 200,
            "completion_tokens": 30,
            "total_tokens": 230,
            "prompt_tokens_details": {
                "cached_tokens": 150,
                "cache_write_tokens": 20
            }
        });
        let usage = Usage::from_openai_chat(&json);
        assert_eq!(usage.input_tokens, 30);
        assert_eq!(usage.cache_read_tokens, 150);
        assert_eq!(usage.cache_write_tokens, 20);
        assert_eq!(usage.output_tokens, 30);
    }

    #[test]
    fn parse_openai_responses_usage() {
        let json = json!({
            "input_tokens": 300,
            "output_tokens": 40,
            "total_tokens": 340,
            "input_tokens_details": {
                "cached_tokens": 200,
                "cache_write_tokens": 50
            },
            "output_tokens_details": {
                "reasoning_tokens": 15
            }
        });
        let usage = Usage::from_openai_responses(&json);
        assert_eq!(usage.input_tokens, 50);
        assert_eq!(usage.cache_read_tokens, 200);
        assert_eq!(usage.cache_write_tokens, 50);
        assert_eq!(usage.output_tokens, 40);
        assert_eq!(usage.reasoning_tokens, 15);
    }

    #[test]
    fn parse_gemini_metadata_and_json() {
        let meta_usage = Usage::from_gemini_metadata(Some(1000), Some(200), Some(700), Some(50));
        assert_eq!(meta_usage.input_tokens, 300);
        assert_eq!(meta_usage.output_tokens, 200);
        assert_eq!(meta_usage.cache_read_tokens, 700);
        assert_eq!(meta_usage.reasoning_tokens, 50);

        let json = json!({
            "promptTokenCount": 1000,
            "candidatesTokenCount": 200,
            "cachedContentTokenCount": 700,
            "thoughtsTokenCount": 50,
            "totalTokenCount": 1250
        });
        let json_usage = Usage::from_gemini_json(&json);
        assert_eq!(json_usage, meta_usage);
    }

    #[test]
    fn parse_anthropic_json() {
        let json = json!({
            "input_tokens": 50,
            "output_tokens": 80,
            "cache_read_input_tokens": 1200,
            "cache_creation_input_tokens": 200,
            "cache_deleted_input_tokens": 30
        });
        let usage = Usage::from_anthropic_json(&json);
        assert_eq!(usage.input_tokens, 50);
        assert_eq!(usage.output_tokens, 80);
        assert_eq!(usage.cache_read_tokens, 1200);
        assert_eq!(usage.cache_write_tokens, 200);
        assert_eq!(usage.cache_deleted_tokens, 30);
    }

    #[test]
    fn detect_cache_miss_filters_noise_floor() {
        let usage = Usage {
            input_tokens: 300,
            output_tokens: 50,
            cache_read_tokens: 1500,
            cache_write_tokens: 0,
            cache_deleted_tokens: 0,
            reasoning_tokens: 0,
        };
        // Total prompt: 1800. Prev prompt: 2000.
        // Expected cached: min(2000, 1800) = 1800.
        // Cache read: 1500. Missed: 300.
        // 300 <= NOISE_FLOOR (1024) -> None
        assert_eq!(usage.detect_cache_miss(2000, 3.0, 0.3), None);
    }

    #[test]
    fn detect_cache_miss_counts_significant_miss_and_cost() {
        let usage = Usage {
            input_tokens: 4000,
            output_tokens: 50,
            cache_read_tokens: 1000,
            cache_write_tokens: 0,
            cache_deleted_tokens: 0,
            reasoning_tokens: 0,
        };
        // Total prompt: 5000. Prev prompt: 5000.
        // Expected cached: 5000. Cache read: 1000.
        // Missed: 4000 > 1024 -> Some!
        // Price: input $3/M, read $0.3/M -> extra $2.7/M
        // Extra cost: 4000 * 2.7 / 1_000_000 = 0.0108 USD
        let miss = usage
            .detect_cache_miss(5000, 3.0, 0.3)
            .expect("miss detected");
        assert_eq!(miss.missed_tokens, 4000);
        assert!((miss.missed_cost - 0.0108).abs() < 1e-6);
    }

    #[test]
    fn cross_protocol_two_turn_system_and_tools_prefix_stability() {
        use crate::constants::prompts::SYSTEM_PROMPT_DYNAMIC_BOUNDARY;
        use crate::services::provider::gemini::wire::build_system_instruction;
        use crate::services::provider::ir::{
            CachePolicy, ClientContext, ModelRequest, ToolChoice, ToolSpec,
        };
        use crate::services::provider::openai::system_text;
        use crate::utils::api::{append_system_context, split_sys_prompt_prefix};
        use std::collections::BTreeMap;

        // Base system prompt: static sections + boundary + dynamic tail.
        let base_system = vec![
            "You are a helpful assistant.".to_string(),
            "# Tone and style\n - Be concise.".to_string(),
            "# Output efficiency\n - Straight to the point.".to_string(),
            SYSTEM_PROMPT_DYNAMIC_BOUNDARY.to_string(),
            "# Environment\n - CWD: /path/to/project".to_string(),
        ];

        // Tools for the session:
        let tools = vec![
            ToolSpec {
                name: "Read".into(),
                description: "Read file contents".into(),
                input_schema: json!({"type": "object", "properties": {"path": {"type": "string"}}}),
                strict: false,
                deferrable: false,
                is_mcp: false,
            },
            ToolSpec {
                name: "Edit".into(),
                description: "Edit file".into(),
                input_schema: json!({"type": "object", "properties": {"path": {"type": "string"}, "text": {"type": "string"}}}),
                strict: false,
                deferrable: false,
                is_mcp: false,
            },
        ];

        // Turn 1: git status is clean.
        let mut context_turn1 = BTreeMap::new();
        context_turn1.insert("gitStatus".to_string(), "clean".to_string());
        let sys_turn1 = append_system_context(&base_system, &context_turn1);

        // Turn 2: tool executed, file modified, git status changed!
        let mut context_turn2 = BTreeMap::new();
        context_turn2.insert(
            "gitStatus".to_string(),
            "modified: foo.rs\n M foo.rs".to_string(),
        );
        let sys_turn2 = append_system_context(&base_system, &context_turn2);

        // 1. Anthropic: Verify split_sys_prompt_prefix assigns identical global static block.
        // With global cache feature, the block before boundary gets Global cache scope.
        let blocks_turn1 = split_sys_prompt_prefix(&sys_turn1, false);
        let blocks_turn2 = split_sys_prompt_prefix(&sys_turn2, false);

        // The static prefix blocks must be 100% byte-for-byte identical.
        let static_prefix1 = &blocks_turn1[0];
        let static_prefix2 = &blocks_turn2[0];
        assert_eq!(static_prefix1.text, static_prefix2.text);
        assert_eq!(static_prefix1.cache_scope, static_prefix2.cache_scope);

        // 2. OpenAI: Verify system_text preserves the byte-level static prefix.
        let make_request = |sys: Vec<String>| ModelRequest {
            model: "gpt-5".into(),
            system: sys,
            messages: Vec::new(),
            tools: tools.clone(),
            tool_choice: ToolChoice::Auto,
            effort: crate::utils::effort::Effort::Off,
            max_output_tokens: 1000,
            temperature: None,
            structured_output: None,
            cache: CachePolicy::default(),
            query_source: "test".into(),
            agent_id: None,
            disable_parallel_tools: false,
            discovered_tools: Vec::new(),
            tools_pending: false,
            client: ClientContext::default(),
            extensions: Vec::new(),
        };

        let req1 = make_request(sys_turn1.clone());
        let req2 = make_request(sys_turn2.clone());

        let text1 = system_text(&req1).expect("turn 1 text");
        let text2 = system_text(&req2).expect("turn 2 text");

        let static_prefix_str = "You are a helpful assistant.\n\n# Tone and style\n - Be concise.\n\n# Output efficiency\n - Straight to the point.";
        assert!(text1.starts_with(static_prefix_str));
        assert!(text2.starts_with(static_prefix_str));
        assert!(!text1.contains(SYSTEM_PROMPT_DYNAMIC_BOUNDARY));
        assert!(!text2.contains(SYSTEM_PROMPT_DYNAMIC_BOUNDARY));

        // Tools must serialize to identical JSON.
        let tools1_json = serde_json::to_string(&req1.tools).unwrap();
        let tools2_json = serde_json::to_string(&req2.tools).unwrap();
        assert_eq!(tools1_json, tools2_json);

        // 3. Gemini: Verify build_system_instruction preserves the byte-level static prefix.
        let gemini_content1 = build_system_instruction(&sys_turn1).expect("gemini content 1");
        let gemini_content2 = build_system_instruction(&sys_turn2).expect("gemini content 2");

        let gemini_text1 = gemini_content1.parts[0].text.as_ref().unwrap();
        let gemini_text2 = gemini_content2.parts[0].text.as_ref().unwrap();
        assert!(gemini_text1.starts_with(static_prefix_str));
        assert!(gemini_text2.starts_with(static_prefix_str));
        assert!(!gemini_text1.contains(SYSTEM_PROMPT_DYNAMIC_BOUNDARY));
        assert!(!gemini_text2.contains(SYSTEM_PROMPT_DYNAMIC_BOUNDARY));
    }
}
