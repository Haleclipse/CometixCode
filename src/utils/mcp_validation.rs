//! MCP result sizing and truncation.
//!
//! Maps to: CC `utils/mcpValidation.ts` (complete file).

use crate::utils::process_env::JsTruthy;
use serde_json::Value;
use std::collections::HashMap;

/// Maps to: CC `utils/mcpValidation.ts:12` `MCP_TOKEN_COUNT_THRESHOLD_FACTOR`.
pub const MCP_TOKEN_COUNT_THRESHOLD_FACTOR: f64 = 0.5;
/// Maps to: CC `utils/mcpValidation.ts:13` `IMAGE_TOKEN_ESTIMATE`.
pub const IMAGE_TOKEN_ESTIMATE: usize = 1_600;
/// Maps to: CC `utils/mcpValidation.ts:14` `DEFAULT_MAX_MCP_OUTPUT_TOKENS`.
const DEFAULT_MAX_MCP_OUTPUT_TOKENS: f64 = 25_000.0;

/// Maps to: CC `utils/mcpValidation.ts:26-49` `getMaxMcpOutputTokens`.
///
/// Cometix resolves the `tengu_satin_quoll` override map from the
/// source-controlled switch table instead of GrowthBook.
pub fn get_max_mcp_output_tokens() -> f64 {
    if let Some(value) = crate::utils::process_env::var("MAX_MCP_OUTPUT_TOKENS").truthy() {
        let bytes = value.as_bytes();
        let mut index = 0usize;
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        let sign = if bytes.get(index) == Some(&b'-') {
            index += 1;
            -1.0
        } else {
            if bytes.get(index) == Some(&b'+') {
                index += 1;
            }
            1.0
        };
        let start = index;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
        if index > start {
            if let Ok(parsed) = value[start..index].parse::<f64>() {
                let parsed = sign * parsed;
                if parsed.is_finite() && parsed > 0.0 {
                    return parsed;
                }
            }
        }
    }

    let overrides = crate::utils::feature_flags::feature_enabled(
        crate::utils::feature_flags::FeatureFlag::PersistThresholdOverrides,
    )
    .then(HashMap::<String, f64>::new);
    if let Some(override_value) = overrides
        .as_ref()
        .and_then(|values| values.get("mcp_tool"))
        .copied()
        .filter(|value| value.is_finite() && *value > 0.0)
    {
        return override_value;
    }

    DEFAULT_MAX_MCP_OUTPUT_TOKENS
}

/// Maps to: CC `utils/mcpValidation.ts:51-53` `isTextBlock`.
fn is_text_block(block: &Value) -> bool {
    block.get("type").and_then(Value::as_str) == Some("text")
}

/// Maps to: CC `utils/mcpValidation.ts:55-57` `isImageBlock`.
fn is_image_block(block: &Value) -> bool {
    block.get("type").and_then(Value::as_str) == Some("image")
}

/// Maps to: CC `utils/mcpValidation.ts:59-77` `getContentSizeEstimate`.
pub fn get_content_size_estimate(content: &Value) -> usize {
    if content.is_null() || content.as_str() == Some("") {
        return 0;
    }

    if let Some(text) = content.as_str() {
        return crate::services::token_estimation::rough_token_count_estimation(text) as usize;
    }

    content
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .map(|block| {
                    if is_text_block(block) {
                        block
                            .get("text")
                            .and_then(Value::as_str)
                            .map(|text| {
                                crate::services::token_estimation::rough_token_count_estimation(
                                    text,
                                ) as usize
                            })
                            .unwrap_or(0)
                    } else if is_image_block(block) {
                        IMAGE_TOKEN_ESTIMATE
                    } else {
                        0
                    }
                })
                .sum()
        })
        .unwrap_or(0)
}

/// Maps to: CC `utils/mcpValidation.ts:78-80` `getMaxMcpOutputChars`.
fn get_max_mcp_output_chars() -> usize {
    (get_max_mcp_output_tokens() * 4.0).trunc() as usize
}

/// Maps to: CC `utils/mcpValidation.ts:82-88` `getTruncationMessage`.
fn get_truncation_message() -> String {
    let mut buffer = ryu_js::Buffer::new();
    let max_tokens = buffer.format(get_max_mcp_output_tokens());
    format!(
        "\n\n[OUTPUT TRUNCATED - exceeded {max_tokens} token limit]\n\nThe tool output was truncated. If this MCP server provides pagination or filtering tools, use them to retrieve specific portions of the data. If pagination is not available, inform the user that you are working with truncated output and results may be incomplete."
    )
}

/// Maps to: CC `utils/mcpValidation.ts:90-95` `truncateString`.
fn truncate_string(content: &str, max_chars: usize) -> String {
    if content.encode_utf16().count() <= max_chars {
        return content.to_string();
    }

    // Rust strings cannot retain an isolated UTF-16 surrogate. Preserve the
    // source's UTF-16 budget while stopping at the last valid scalar boundary.
    let mut used = 0usize;
    content
        .chars()
        .take_while(|character| {
            let width = character.len_utf16();
            if used + width > max_chars {
                return false;
            }
            used += width;
            true
        })
        .collect()
}

/// Maps to: CC `utils/mcpValidation.ts:97-149` `truncateContentBlocks`.
async fn truncate_content_blocks(blocks: &[Value], max_chars: usize) -> Vec<Value> {
    let mut result = Vec::new();
    let mut current_chars = 0usize;

    for block in blocks {
        if is_text_block(block) {
            let remaining_chars = max_chars.saturating_sub(current_chars);
            if remaining_chars == 0 {
                break;
            }
            let text = block
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let text_chars = text.encode_utf16().count();
            if text_chars <= remaining_chars {
                result.push(block.clone());
                current_chars += text_chars;
            } else {
                result.push(serde_json::json!({
                    "type": "text",
                    "text": truncate_string(text, remaining_chars),
                }));
                break;
            }
        } else if is_image_block(block) {
            let image_chars = IMAGE_TOKEN_ESTIMATE * 4;
            if current_chars + image_chars <= max_chars {
                result.push(block.clone());
                current_chars += image_chars;
            } else {
                let remaining_chars = max_chars.saturating_sub(current_chars);
                if remaining_chars > 0 {
                    let remaining_bytes = (remaining_chars as f64 * 0.75).floor() as usize;
                    if let Ok(compressed_block) =
                        crate::utils::image_resizer::compress_image_block(block, remaining_bytes)
                    {
                        current_chars += compressed_block
                            .get("source")
                            .filter(|source| {
                                source.get("type").and_then(Value::as_str) == Some("base64")
                            })
                            .and_then(|source| source.get("data"))
                            .and_then(Value::as_str)
                            .map(str::len)
                            .unwrap_or(image_chars);
                        result.push(compressed_block);
                    }
                }
            }
        } else {
            result.push(block.clone());
        }
    }

    result
}

/// Maps to: CC `utils/mcpValidation.ts:151-176` `mcpContentNeedsTruncation`.
pub async fn mcp_content_needs_truncation(content: &Value) -> bool {
    if content.is_null() || content.as_str() == Some("") {
        return false;
    }

    let content_size_estimate = get_content_size_estimate(content);
    if content_size_estimate as f64
        <= get_max_mcp_output_tokens() * MCP_TOKEN_COUNT_THRESHOLD_FACTOR
    {
        return false;
    }

    let message_content = if let Some(text) = content.as_str() {
        anthropic_sdk::resources::beta::messages::BetaMessageContent::Text(text.to_string())
    } else if let Some(blocks) = content.as_array() {
        let blocks = blocks
            .iter()
            .filter_map(|block| {
                serde_json::from_value::<
                    anthropic_sdk::resources::beta::messages::BetaContentBlockParam,
                >(block.clone())
                .ok()
            })
            .collect::<Vec<_>>();
        if blocks.is_empty() {
            return false;
        }
        anthropic_sdk::resources::beta::messages::BetaMessageContent::Blocks(blocks)
    } else {
        return false;
    };
    let messages = vec![anthropic_sdk::resources::beta::messages::BetaMessageParam {
        role: "user".to_string(),
        content: message_content,
    }];
    crate::services::token_estimation::count_messages_tokens_with_api(messages, Vec::new())
        .await
        .is_some_and(|token_count| token_count as f64 > get_max_mcp_output_tokens())
}

/// Maps to: CC `utils/mcpValidation.ts:180-197` `truncateMcpContent`.
pub async fn truncate_mcp_content(content: &Value) -> Value {
    if content.is_null() || content.as_str() == Some("") {
        return content.clone();
    }

    let max_chars = get_max_mcp_output_chars();
    let truncation_message = get_truncation_message();
    if let Some(text) = content.as_str() {
        return Value::String(format!(
            "{}{}",
            truncate_string(text, max_chars),
            truncation_message
        ));
    }
    if let Some(blocks) = content.as_array() {
        let mut truncated_blocks = truncate_content_blocks(blocks, max_chars).await;
        truncated_blocks.push(serde_json::json!({
            "type": "text",
            "text": truncation_message,
        }));
        return Value::Array(truncated_blocks);
    }
    content.clone()
}

/// Maps to: CC `utils/mcpValidation.ts:199-208` `truncateMcpContentIfNeeded`.
pub async fn truncate_mcp_content_if_needed(content: &Value) -> Value {
    if !mcp_content_needs_truncation(content).await {
        return content.clone();
    }
    truncate_mcp_content(content).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[test]
    fn max_tokens_and_size_estimate_match_official_env_and_utf16_rules() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _max = EnvVarGuard::set("MAX_MCP_OUTPUT_TOKENS", "  +5tail");
        assert_eq!(get_max_mcp_output_tokens(), 5.0);
        assert_eq!(get_content_size_estimate(&Value::String("😀😀".into())), 1);
    }

    #[tokio::test]
    async fn truncate_mcp_content_matches_official_copy() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _max = EnvVarGuard::set("MAX_MCP_OUTPUT_TOKENS", "5");
        let content = Value::String("abcdefghij".repeat(12));
        let truncated = truncate_mcp_content(&content).await;
        let text = truncated.as_str().expect("truncated string");
        assert!(text.starts_with("abcdefghijabcdefghij"));
        assert!(text.contains("[OUTPUT TRUNCATED - exceeded 5 token limit]"));
        assert!(text.contains("If this MCP server provides pagination or filtering tools"));
    }
}
