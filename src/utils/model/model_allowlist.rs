//! `availableModels` allowlist matching.
//! Maps to CC `utils/model/modelAllowlist.ts`.

use crate::utils::model::aliases::{is_model_alias, is_model_family_alias};
use crate::utils::model::model::parse_user_specified_model;
use crate::utils::model::model_strings::resolve_overridden_model;

/// Maps to: CC `utils/model/modelAllowlist.ts:10-20` `modelBelongsToFamily`.
fn model_belongs_to_family(model: &str, family: &str) -> bool {
    if model.contains(family) {
        return true;
    }
    if is_model_alias(model) {
        return parse_user_specified_model(model)
            .to_ascii_lowercase()
            .contains(family);
    }
    false
}

/// Maps to: CC `utils/model/modelAllowlist.ts:27-32` `prefixMatchesModel` — the
/// prefix has to end at a segment boundary, so `claude-opus-4-5` matches
/// `claude-opus-4-5-20251101` but not `claude-opus-4-50`.
fn prefix_matches_model(model_name: &str, prefix: &str) -> bool {
    let Some(rest) = model_name.strip_prefix(prefix) else {
        return false;
    };
    rest.is_empty() || rest.starts_with('-')
}

/// Maps to: CC `utils/model/modelAllowlist.ts:39-57` `modelMatchesVersionPrefix`.
fn model_matches_version_prefix(model: &str, entry: &str) -> bool {
    let resolved_model = if is_model_alias(model) {
        parse_user_specified_model(model).to_ascii_lowercase()
    } else {
        model.to_string()
    };

    if prefix_matches_model(&resolved_model, entry) {
        return true;
    }
    if !entry.starts_with("claude-")
        && prefix_matches_model(&resolved_model, &format!("claude-{entry}"))
    {
        return true;
    }
    false
}

/// Maps to: CC `utils/model/modelAllowlist.ts:65-87` `familyHasSpecificEntries`
/// — when the allowlist holds both `opus` and `opus-4-5`, the specific entry
/// wins and the family wildcard is ignored.
fn family_has_specific_entries(family: &str, allowlist: &[String]) -> bool {
    for entry in allowlist {
        if is_model_family_alias(entry) {
            continue;
        }
        let Some(index) = entry.find(family) else {
            continue;
        };
        let after_family = index + family.len();
        // Match at a segment boundary so `opusplan` does not narrow `opus`.
        if after_family == entry.len() || entry.as_bytes()[after_family] == b'-' {
            return true;
        }
    }
    false
}

/// Maps to: CC `utils/model/modelAllowlist.ts:100-170` `isModelAllowed`.
///
/// Matching tiers, in order: family aliases as wildcards (unless narrowed by a
/// more specific entry for the same family), version prefixes, and full model
/// IDs. An unset `availableModels` allows everything; an empty one blocks every
/// user-specified model.
pub fn is_model_allowed(model: &str) -> bool {
    let Some(available_models) = crate::utils::settings::get_initial_settings().available_models
    else {
        return true;
    };
    if available_models.is_empty() {
        return false;
    }

    let resolved_model = resolve_overridden_model(model);
    let normalized_model = resolved_model.trim().to_ascii_lowercase();
    let normalized_allowlist: Vec<String> = available_models
        .iter()
        .map(|entry| entry.trim().to_ascii_lowercase())
        .collect();

    // Direct match, skipping family aliases that specific entries have narrowed.
    if normalized_allowlist.contains(&normalized_model)
        && (!is_model_family_alias(&normalized_model)
            || !family_has_specific_entries(&normalized_model, &normalized_allowlist))
    {
        return true;
    }

    for entry in &normalized_allowlist {
        if is_model_family_alias(entry)
            && !family_has_specific_entries(entry, &normalized_allowlist)
            && model_belongs_to_family(&normalized_model, entry)
        {
            return true;
        }
    }

    // Bidirectional alias resolution for non-family entries.
    if is_model_alias(&normalized_model) {
        let resolved = parse_user_specified_model(&normalized_model).to_ascii_lowercase();
        if normalized_allowlist.contains(&resolved) {
            return true;
        }
    }

    for entry in &normalized_allowlist {
        if !is_model_family_alias(entry) && is_model_alias(entry) {
            if parse_user_specified_model(entry).to_ascii_lowercase() == normalized_model {
                return true;
            }
        }
    }

    for entry in &normalized_allowlist {
        if !is_model_family_alias(entry)
            && !is_model_alias(entry)
            && model_matches_version_prefix(&normalized_model, entry)
        {
            return true;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    struct SettingsFixture {
        root: std::path::PathBuf,
        config_dir: Option<EnvVarGuard>,
    }

    impl SettingsFixture {
        fn new(available_models: Option<&[&str]>) -> Self {
            let root = std::env::temp_dir().join(format!(
                "cometix-allowlist-{}",
                uuid::Uuid::new_v4().simple()
            ));
            std::fs::create_dir_all(&root).unwrap();
            let settings = match available_models {
                Some(models) => serde_json::json!({ "availableModels": models }),
                None => serde_json::json!({}),
            };
            std::fs::write(root.join("settings.json"), settings.to_string()).unwrap();
            let config_dir = Some(EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root));
            crate::utils::settings::settings_cache::reset_settings_cache();
            Self { root, config_dir }
        }
    }

    impl Drop for SettingsFixture {
        fn drop(&mut self) {
            drop(self.config_dir.take());
            crate::utils::settings::settings_cache::reset_settings_cache();
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// Maps to: CC `utils/model/modelAllowlist.ts:101-108` — unset allows all,
    /// empty blocks all.
    #[test]
    fn absent_allowlist_allows_everything_and_empty_allowlist_blocks_everything() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        {
            let _fixture = SettingsFixture::new(None);
            assert!(is_model_allowed("opus"));
            assert!(is_model_allowed("anything-at-all"));
        }
        {
            let _fixture = SettingsFixture::new(Some(&[]));
            assert!(!is_model_allowed("opus"));
            assert!(!is_model_allowed("claude-opus-4-6"));
        }
    }

    /// Maps to: CC `utils/model/modelAllowlist.ts:127-138` — a bare family
    /// alias is a wildcard over that family.
    #[test]
    fn family_alias_acts_as_a_wildcard_over_its_family() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _fixture = SettingsFixture::new(Some(&["opus"]));
        assert!(is_model_allowed("opus"));
        assert!(is_model_allowed("claude-opus-4-6"));
        assert!(is_model_allowed("claude-opus-4-1-20250805"));
        assert!(!is_model_allowed("sonnet"));
        assert!(!is_model_allowed("claude-haiku-4-5-20251001"));
    }

    /// Maps to: CC `utils/model/modelAllowlist.ts:115-125, 65-87` — the
    /// narrowing rule, plus the `opusplan`-does-not-narrow-`opus` boundary.
    #[test]
    fn specific_entries_narrow_their_family_alias() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        {
            let _fixture = SettingsFixture::new(Some(&["opus", "opus-4-5"]));
            assert!(is_model_allowed("claude-opus-4-5-20251101"));
            assert!(!is_model_allowed("opus"));
            assert!(!is_model_allowed("claude-opus-4-6"));
        }
        {
            // `opusplan` shares the `opus` prefix but not at a boundary, so the
            // wildcard stays a wildcard.
            let _fixture = SettingsFixture::new(Some(&["opus", "opusplan"]));
            assert!(is_model_allowed("opus"));
            assert!(is_model_allowed("claude-opus-4-6"));
        }
    }

    /// Maps to: CC `utils/model/modelAllowlist.ts:159-167, 27-32` — prefixes
    /// match at segment boundaries only, with or without the `claude-` prefix.
    #[test]
    fn version_prefixes_match_at_segment_boundaries_only() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _fixture = SettingsFixture::new(Some(&["claude-opus-4-5", "sonnet-4-6"]));
        assert!(is_model_allowed("claude-opus-4-5-20251101"));
        assert!(is_model_allowed("claude-opus-4-5"));
        assert!(!is_model_allowed("claude-opus-4-50"));
        assert!(is_model_allowed("claude-sonnet-4-6"));
        assert!(!is_model_allowed("claude-sonnet-4-5-20250929"));
    }

    /// Maps to: CC `utils/model/modelAllowlist.ts:140-157` — aliases resolve in
    /// both directions, and matching is trim + lowercase insensitive
    /// (`:111-112`).
    #[test]
    fn alias_resolution_is_bidirectional_and_case_insensitive() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("ANTHROPIC_DEFAULT_OPUS_MODEL", "claude-opus-4-6");
        {
            // Allowlist holds the full ID; the user asks with the alias.
            let _fixture = SettingsFixture::new(Some(&["  CLAUDE-OPUS-4-6  "]));
            assert!(is_model_allowed("best"));
        }
        {
            // Allowlist holds a non-family alias; the user asks with the ID it
            // resolves to.
            let _fixture = SettingsFixture::new(Some(&["best"]));
            assert!(is_model_allowed("claude-opus-4-6"));
            assert!(!is_model_allowed("claude-sonnet-4-6"));
        }
        crate::utils::process_env::remove("ANTHROPIC_DEFAULT_OPUS_MODEL");
    }
}
