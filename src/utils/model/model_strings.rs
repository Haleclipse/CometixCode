//! Provider model-string helpers.
//! Maps to: CC `utils/model/modelStrings.ts`.

use crate::utils::model::configs::{ALL_MODEL_CONFIGS, ModelKey};

/// Maps to: CC `utils/model/modelStrings.ts:21` `ModelStrings` — each model
/// version's ID for the active provider, after `modelOverrides` are layered on.
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

/// Maps to: CC `utils/model/modelStrings.ts:63-76` `applyModelOverrides`.
fn apply_model_overrides(mut strings: ModelStrings) -> ModelStrings {
    let Some(overrides) = crate::utils::settings::get_initial_settings().model_overrides else {
        return strings;
    };
    for (canonical_id, model_override) in overrides {
        if model_override.is_empty() {
            continue;
        }
        if let Some(key) = crate::utils::model::configs::canonical_id_to_key(&canonical_id) {
            strings.set(key, model_override);
        }
    }
    strings
}

/// Maps to: CC `utils/model/modelStrings.ts:136-145` `getModelStrings`.
///
/// DEVIATION: CC memoizes the table in `bootstrap/state.ts` and, on Bedrock,
/// replaces the hardcoded IDs with the caller's inference profiles via an
/// awaited `ListInferenceProfiles` call (`modelStrings.ts:33-55, 102-116`).
/// That refresh is network-bound and its cache lives in `bootstrap/state.rs`,
/// so only the synchronous branch is ported: the builtin per-provider table
/// with `modelOverrides` layered on. This is byte-for-byte what CC itself
/// returns before the background fetch resolves (`modelStrings.ts:140-142`),
/// so Bedrock users see the hardcoded `us.anthropic.*` IDs rather than a
/// region-matched profile until the refresh is ported.
pub fn get_model_strings() -> ModelStrings {
    apply_model_overrides(builtin_model_strings(
        crate::utils::model::providers::get_api_provider(),
    ))
}

/// Maps to: CC `utils/model/modelStrings.ts:84-99#resolveOverriddenModel`.
///
/// Resolves a configured provider-specific override back to the canonical
/// first-party model ID whose key owns it. If no current override value matches,
/// the input is returned unchanged.
pub fn resolve_overridden_model(model_id: &str) -> String {
    let settings = crate::utils::settings::get_initial_settings();
    let Some(overrides) = settings.model_overrides else {
        return model_id.to_string();
    };
    overrides
        .into_iter()
        .find_map(|(canonical_id, model_override)| {
            (model_override == model_id).then_some(canonical_id)
        })
        .unwrap_or_else(|| model_id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[test]
    fn resolve_overridden_model_matches_official_first_object_entry_for_duplicate_values() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-model-overrides-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("settings.json"),
            r#"{"modelOverrides":{"claude-first":"provider-id","claude-second":"provider-id"}}"#,
        )
        .unwrap();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root);

        assert_eq!(resolve_overridden_model("provider-id"), "claude-first");

        let _ = std::fs::remove_dir_all(root);
    }

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

    /// Maps to: CC `utils/model/modelStrings.ts:63-76` — overrides are keyed by
    /// canonical first-party ID and replace the provider value in place.
    #[test]
    fn model_overrides_replace_the_provider_value_for_their_canonical_key() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-model-strings-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("settings.json"),
            r#"{"modelOverrides":{"claude-opus-4-6":"arn:aws:bedrock:us-east-1::opus","claude-not-a-model":"ignored"}}"#,
        )
        .unwrap();
        let config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root);
        crate::utils::settings::settings_cache::reset_settings_cache();

        let strings = get_model_strings();
        assert_eq!(strings.opus46, "arn:aws:bedrock:us-east-1::opus");
        assert_eq!(strings.sonnet46, "claude-sonnet-4-6");

        drop(config);
        crate::utils::settings::settings_cache::reset_settings_cache();
        let _ = std::fs::remove_dir_all(root);
    }
}
