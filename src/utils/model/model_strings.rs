//! Provider model-string helpers.
//! Maps to: CC `utils/model/modelStrings.ts`.

use crate::utils::model::configs::{ALL_MODEL_CONFIGS, ModelKey};

/// Maps to: CC `utils/model/modelStrings.ts:21` `ModelStrings` — each model
/// version's ID for the active provider.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelStrings {
    pub haiku35: String,
    pub haiku45: String,
    pub sonnet35: String,
    pub sonnet37: String,
    pub sonnet40: String,
    pub sonnet45: String,
    pub sonnet46: String,
    pub opus40: String,
    pub opus41: String,
    pub opus45: String,
    pub opus46: String,
}

impl ModelStrings {
    fn set(&mut self, key: ModelKey, value: String) {
        match key {
            ModelKey::Haiku35 => self.haiku35 = value,
            ModelKey::Haiku45 => self.haiku45 = value,
            ModelKey::Sonnet35 => self.sonnet35 = value,
            ModelKey::Sonnet37 => self.sonnet37 = value,
            ModelKey::Sonnet40 => self.sonnet40 = value,
            ModelKey::Sonnet45 => self.sonnet45 = value,
            ModelKey::Sonnet46 => self.sonnet46 = value,
            ModelKey::Opus40 => self.opus40 = value,
            ModelKey::Opus41 => self.opus41 = value,
            ModelKey::Opus45 => self.opus45 = value,
            ModelKey::Opus46 => self.opus46 = value,
        }
    }
}

/// Maps to: CC `utils/model/modelStrings.ts:25-31` `getBuiltinModelStrings`.
fn builtin_model_strings(provider: crate::utils::model::providers::ApiProvider) -> ModelStrings {
    let mut strings = ModelStrings {
        haiku35: String::new(),
        haiku45: String::new(),
        sonnet35: String::new(),
        sonnet37: String::new(),
        sonnet40: String::new(),
        sonnet45: String::new(),
        sonnet46: String::new(),
        opus40: String::new(),
        opus41: String::new(),
        opus45: String::new(),
        opus46: String::new(),
    };
    for (key, config) in ALL_MODEL_CONFIGS {
        strings.set(*key, config.for_provider(provider).to_string());
    }
    strings
}

/// Maps to: CC `utils/model/modelStrings.ts:136-145` `getModelStrings`.
///
/// PR 1 removes the old settings override entry. Provider-specific
/// model ids now belong to the selected model profile; this table remains only
/// for legacy UI helpers until the profile picker is completed.
pub fn get_model_strings() -> ModelStrings {
    builtin_model_strings(crate::utils::model::providers::get_api_provider())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Maps to: CC `utils/model/modelStrings.ts:25-31` — the per-provider
    /// projection of `configs.ts`, which the 3P model option values read.
    #[test]
    fn builtin_model_strings_project_the_official_per_provider_ids() {
        use crate::utils::model::providers::ApiProvider;

        let first_party = builtin_model_strings(ApiProvider::FirstParty);
        assert_eq!(first_party.opus46, "claude-opus-4-6");
        assert_eq!(first_party.sonnet46, "claude-sonnet-4-6");
        assert_eq!(first_party.haiku45, "claude-haiku-4-5-20251001");

        let bedrock = builtin_model_strings(ApiProvider::Bedrock);
        assert_eq!(bedrock.opus46, "us.anthropic.claude-opus-4-6-v1");
        assert_eq!(bedrock.sonnet46, "us.anthropic.claude-sonnet-4-6");
        assert_eq!(
            bedrock.haiku45,
            "us.anthropic.claude-haiku-4-5-20251001-v1:0"
        );

        let vertex = builtin_model_strings(ApiProvider::Vertex);
        assert_eq!(vertex.sonnet45, "claude-sonnet-4-5@20250929");
        assert_eq!(vertex.haiku35, "claude-3-5-haiku@20241022");
    }
}
