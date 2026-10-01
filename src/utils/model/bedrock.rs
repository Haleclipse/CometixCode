//! Bedrock model ID helpers.
//! Maps to CC `utils/model/bedrock.ts` region-prefix helpers.

use crate::utils::process_env::JsTruthy;

/// Maps to CC `utils/model/bedrock.ts#BEDROCK_REGION_PREFIXES`.
pub const BEDROCK_REGION_PREFIXES: &[&str] = &["us", "eu", "apac", "global"];

/// Maps to CC `utils/model/bedrock.ts#isFoundationModel`.
pub fn is_foundation_model(model_id: &str) -> bool {
    model_id.starts_with("anthropic.")
}

/// Maps to CC `utils/model/bedrock.ts#extractModelIdFromArn`.
pub fn extract_model_id_from_arn(model_id: &str) -> String {
    if !model_id.starts_with("arn:") {
        return model_id.to_string();
    }
    model_id
        .rsplit_once('/')
        .map(|(_, id)| id.to_string())
        .unwrap_or_else(|| model_id.to_string())
}

/// Maps to: CC `utils/model/bedrock.ts#getInferenceProfileBackingModel`.
pub async fn get_inference_profile_backing_model(
    profile_id: &str,
    region: &str,
    auth: &crate::services::api::client::BedrockAuth,
) -> Option<String> {
    let endpoint = crate::utils::process_env::var("ANTHROPIC_BEDROCK_BASE_URL")
        .truthy()
        .or_else(|| crate::utils::process_env::var("AWS_ENDPOINT_URL_BEDROCK").truthy())
        .unwrap_or_else(|| format!("https://bedrock.{region}.amazonaws.com"));
    let response = crate::services::api::client::send_bedrock_request(
        reqwest::Method::GET,
        &endpoint,
        &format!(
            "/inference-profiles/{}",
            crate::services::api::client::aws_uri_encode(profile_id)
        ),
        Vec::new(),
        region,
        "bedrock",
        auth,
    )
    .await?;
    let model_arn = response
        .get("models")?
        .as_array()?
        .first()?
        .get("modelArn")?
        .as_str()?;
    Some(
        model_arn
            .rsplit_once('/')
            .map_or(model_arn, |(_, model)| model)
            .to_string(),
    )
}

/// Maps to CC `utils/model/bedrock.ts#getBedrockRegionPrefix`.
pub fn get_bedrock_region_prefix(model_id: &str) -> Option<&'static str> {
    let effective_model_id = extract_model_id_from_arn(model_id);
    BEDROCK_REGION_PREFIXES
        .iter()
        .copied()
        .find(|prefix| effective_model_id.starts_with(&format!("{prefix}.anthropic.")))
}

/// Maps to CC `utils/model/bedrock.ts#applyBedrockRegionPrefix`.
pub fn apply_bedrock_region_prefix(model_id: &str, prefix: &str) -> String {
    if let Some(existing_prefix) = get_bedrock_region_prefix(model_id) {
        return model_id.replacen(&format!("{existing_prefix}."), &format!("{prefix}."), 1);
    }

    if is_foundation_model(model_id) {
        return format!("{prefix}.{model_id}");
    }

    model_id.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bedrock_region_prefix_helpers_match_official_examples() {
        assert_eq!(
            get_bedrock_region_prefix("eu.anthropic.claude-sonnet-4-5-20250929-v1:0"),
            Some("eu")
        );
        assert_eq!(
            get_bedrock_region_prefix(
                "arn:aws:bedrock:ap-northeast-2:123:inference-profile/global.anthropic.claude-opus-4-6-v1"
            ),
            Some("global")
        );
        assert_eq!(
            get_bedrock_region_prefix("anthropic.claude-3-5-sonnet-20241022-v2:0"),
            None
        );
        assert_eq!(
            apply_bedrock_region_prefix("us.anthropic.claude-sonnet-4-5-20250929-v1:0", "eu"),
            "eu.anthropic.claude-sonnet-4-5-20250929-v1:0"
        );
        assert_eq!(
            apply_bedrock_region_prefix("anthropic.claude-sonnet-4-5-v1:0", "eu"),
            "eu.anthropic.claude-sonnet-4-5-v1:0"
        );
        assert_eq!(
            apply_bedrock_region_prefix("claude-sonnet-4-5-20250929", "eu"),
            "claude-sonnet-4-5-20250929"
        );
    }
}
