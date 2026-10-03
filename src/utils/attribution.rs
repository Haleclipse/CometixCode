//! Maps to: CC `utils/attribution.ts`.
//!
//! Commit/PR attribution text generation. This file owns `getAttributionTexts`
//! (`attribution.ts`:52-98). Not yet ported from the same CC file:
//! `countUserPromptsInMessages`, `getEnhancedPRAttribution` and the transcript
//! stats pipeline (:100-393) — they depend on the transcript reader and
//! `calculateCommitAttribution`, which are follow-ups in
//! `utils/commit_attribution.rs`.

/// Maps to: CC `AttributionTexts`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AttributionTexts {
    pub commit: String,
    pub pr: String,
}

/// Maps to: CC `getAttributionTexts()` (`utils/attribution.ts`:52).
///
/// CC branches not reachable in this build, kept as documented seams:
/// - `USER_TYPE === 'ant' && isUndercover()` → empty texts. `utils/undercover.ts`
///   is internal-only and unported; external builds never take this branch.
/// - `getClientType() === 'remote'` → session URL. Cometix has no remote client
///   runtime (CC `remote/` is unported), so the branch is skipped.
pub fn get_attribution_texts() -> AttributionTexts {
    // @[MODEL LAUNCH]: CC updates the hardcoded fallback model name below
    // (guards against codename leaks). For internal repos CC uses the real
    // model name via isInternalModelRepoCached(); that detector is unported,
    // so like CC's external-repo path we fall back to "Claude Opus 4.6" for
    // unrecognized models.
    let model = crate::utils::model::model::get_main_loop_model();
    let is_known_public_model =
        crate::utils::model::model::get_public_model_display_name(&model).is_some();
    let model_name = if is_known_public_model {
        crate::utils::model::model::get_public_model_name(&model)
    } else {
        "Claude Opus 4.6".to_string()
    };
    let default_attribution = format!(
        "🤖 Generated with [Claude Code]({})",
        crate::constants::product::PRODUCT_URL
    );
    let default_commit = format!("Co-Authored-By: {model_name} <noreply@anthropic.com>");

    let settings = crate::utils::settings::load_settings_from_disk().settings;

    // New attribution setting takes precedence over deprecated includeCoAuthoredBy
    if let Some(attribution) = &settings.attribution {
        return AttributionTexts {
            commit: attribution.commit.clone().unwrap_or(default_commit),
            pr: attribution.pr.clone().unwrap_or(default_attribution),
        };
    }

    // Backward compatibility: deprecated includeCoAuthoredBy setting
    if settings.include_co_authored_by == Some(false) {
        return AttributionTexts::default();
    }

    AttributionTexts {
        commit: default_commit,
        pr: default_attribution,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::TEST_ENV_LOCK;

    #[test]
    fn default_attribution_texts_match_official_shape() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let texts = get_attribution_texts();
        // Either the settings suppress attribution entirely, or the defaults
        // must carry the official Co-Authored-By / Generated-with shapes.
        if !texts.commit.is_empty() {
            assert!(
                texts.commit.starts_with("Co-Authored-By: Claude")
                    || texts.commit.contains("Co-Authored-By:"),
                "commit={}",
                texts.commit
            );
        }
        if !texts.pr.is_empty() {
            assert!(
                texts.pr.contains("Generated with [Claude Code]"),
                "pr={}",
                texts.pr
            );
        }
    }
}
