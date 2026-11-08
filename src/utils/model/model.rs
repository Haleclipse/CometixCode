//! Model identity helpers.
//! Maps to CC `utils/model/model.ts`.

/// Auxiliary requests use the utility model resolution which defaults to @light (design §1).
pub fn get_small_fast_model() -> String {
    let settings = crate::utils::settings::get_initial_settings();
    crate::utils::model::agent::resolve_utility_model_and_effort(
        "side_query",
        &crate::utils::model::agent::ParentSelection::configured(),
        settings.nyan.as_deref(),
    )
    .model
}

/// Shown wherever a model would be named and no main profile is selected.
/// Matches the request-path error text.
pub const NO_MAIN_PROFILE_PROMPT: &str =
    "no model profile configured — set settings.nyan.selection.main";

/// True when a main model profile is selected and resolves.
pub fn has_main_profile() -> bool {
    let settings = crate::utils::settings::get_initial_settings();
    crate::utils::model::profile::resolve_main_profile(&settings).is_ok()
}

/// The wire model of `profile`, else of `selection.main`; empty when neither
/// resolves, so the request boundary reports the configuration error instead
/// of silently selecting an Anthropic default.
pub fn profile_model(profile: Option<&str>) -> String {
    crate::utils::model::profile::resolve_request_profile(
        &crate::utils::settings::get_initial_settings(),
        profile,
    )
    .map(|profile| profile.model)
    .unwrap_or_default()
}

/// The session's main-loop model: the wire model of the session profile
/// (`--model`, `/model`, a resumed session), else of `selection.main`.
///
/// @nyan: CC resolves aliases and a free model string here; nyan only has
/// profiles, so the model always comes from one.
pub fn get_main_loop_model() -> String {
    profile_model(crate::bootstrap::state::get_session_profile().as_deref())
}

/// Maps to: CC `utils/model/model.ts:217-269#firstPartyNameToCanonical`.
pub fn first_party_name_to_canonical(name: &str) -> String {
    let name = name.to_ascii_lowercase();
    for (needle, canonical) in [
        ("claude-opus-4-6", "claude-opus-4-6"),
        ("claude-opus-4-5", "claude-opus-4-5"),
        ("claude-opus-4-1", "claude-opus-4-1"),
        ("claude-opus-4", "claude-opus-4"),
        ("claude-sonnet-4-6", "claude-sonnet-4-6"),
        ("claude-sonnet-4-5", "claude-sonnet-4-5"),
        ("claude-sonnet-4", "claude-sonnet-4"),
        ("claude-haiku-4-5", "claude-haiku-4-5"),
        ("claude-3-7-sonnet", "claude-3-7-sonnet"),
        ("claude-3-5-sonnet", "claude-3-5-sonnet"),
        ("claude-3-5-haiku", "claude-3-5-haiku"),
        ("claude-3-opus", "claude-3-opus"),
        ("claude-3-sonnet", "claude-3-sonnet"),
        ("claude-3-haiku", "claude-3-haiku"),
    ] {
        if name.contains(needle) {
            return canonical.to_string();
        }
    }

    static FALLBACK: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(claude-(?:\d+-\d+-)?\w+)").expect("canonical model regex")
    });
    if let Some(matched) = FALLBACK
        .captures(&name)
        .and_then(|captures| captures.get(1))
    {
        return matched.as_str().to_string();
    }
    name
}

/// Maps to: CC `utils/model/model.ts:279-283#getCanonicalName`.
pub fn get_canonical_name(full_model_name: &str) -> String {
    first_party_name_to_canonical(full_model_name)
}

/// Maps to: CC `utils/model/model.ts:616-618#normalizeModelStringForAPI`.
pub fn normalize_model_string_for_api(model: &str) -> String {
    model
        .replace("[1m]", "")
        .replace("[1M]", "")
        .replace("[2m]", "")
        .replace("[2M]", "")
}

/// Maps to: CC `utils/model/model.ts` `getPublicModelDisplayName(...)`.
///
/// Nyan matches first-party marketing names by substring until provider
/// model-string tables are fully ported.
pub fn get_public_model_display_name(model: &str) -> Option<String> {
    let has_1m = crate::utils::context::has_1m_context(model);
    let lower = model.to_ascii_lowercase();
    let name = if lower.contains("opus-4-6") {
        "Opus 4.6"
    } else if lower.contains("opus-4-5") {
        "Opus 4.5"
    } else if lower.contains("opus-4-1") {
        "Opus 4.1"
    } else if lower.contains("claude-opus-4") || lower.contains("opus-4") {
        "Opus 4"
    } else if lower.contains("sonnet-4-6") {
        "Sonnet 4.6"
    } else if lower.contains("sonnet-4-5") {
        "Sonnet 4.5"
    } else if lower.contains("sonnet-4") {
        "Sonnet 4"
    } else if lower.contains("haiku-4-5") || lower.contains("haiku-4") {
        "Haiku 4.5"
    } else {
        return None;
    };
    Some(if has_1m && !name.contains("1M") {
        format!("{name} 1M")
    } else {
        name.to_string()
    })
}

/// Maps to: CC `utils/model/model.ts` `getPublicModelName(...)` (:425).
pub fn get_public_model_name(model: &str) -> String {
    if let Some(public_name) = get_public_model_display_name(model) {
        return format!("Claude {public_name}");
    }
    format!("Claude ({model})")
}

/// Maps to: CC `utils/model/model.ts` `renderModelName(...)`.
pub fn render_model_name(model: &str) -> String {
    get_public_model_display_name(model).unwrap_or_else(|| model.to_string())
}

/// Maps to: CC `utils/model/model.ts` `isNonCustomOpusModel(...)`.
pub fn is_non_custom_opus_model(model: &str) -> bool {
    let model = model.trim().to_ascii_lowercase();
    if model.is_empty() || model.starts_with("custom-") {
        return false;
    }

    // CC compares against provider-specific `getModelStrings().opus*` values.
    // Nyan does not yet port provider model string profiles, so match the
    // canonical non-custom Opus 4 family names and provider IDs by substring.
    model.contains("claude-opus-4")
}

/// Maps to: CC `utils/model/model.ts` `isOpus1mMergeEnabled()` — a zero-argument
/// predicate over context disabling, subscription type, API provider, and a
/// fail-closed guard for subscribers whose subscription type cannot be read.
/// Performs no model migration, config writes, or network calls.
///
/// It previously took `config` and `get_env` parameters that the body never
/// read: every input is resolved from process state, exactly as at the source.
/// The signature implied the caller could steer the result, so its test built
/// an "unknown subscriber" config that had no effect and the assertion actually
/// depended on the developer's real credentials. Removed rather than wired up —
/// CC takes no arguments here, so adding real ones would be the deviation.
pub fn is_opus_1m_merge_enabled() -> bool {
    !crate::utils::context::is_1m_context_disabled()
}

// @[MODEL LAUNCH]: Add a marketing name mapping for the new model below.
/// Maps to: CC `utils/model/model.ts:570-614#getMarketingNameForModel`.
pub fn get_marketing_name_for_model(model_id: &str) -> Option<String> {
    let has_1m = model_id.to_ascii_lowercase().contains("[1m]");
    let canonical = get_canonical_name(model_id);
    let name = if canonical.contains("claude-opus-4-6") {
        if has_1m {
            "Opus 4.6 (with 1M context)"
        } else {
            "Opus 4.6"
        }
    } else if canonical.contains("claude-opus-4-5") {
        "Opus 4.5"
    } else if canonical.contains("claude-opus-4-1") {
        "Opus 4.1"
    } else if canonical.contains("claude-opus-4") {
        "Opus 4"
    } else if canonical.contains("claude-sonnet-4-6") {
        if has_1m {
            "Sonnet 4.6 (with 1M context)"
        } else {
            "Sonnet 4.6"
        }
    } else if canonical.contains("claude-sonnet-4-5") {
        if has_1m {
            "Sonnet 4.5 (with 1M context)"
        } else {
            "Sonnet 4.5"
        }
    } else if canonical.contains("claude-sonnet-4") {
        if has_1m {
            "Sonnet 4 (with 1M context)"
        } else {
            "Sonnet 4"
        }
    } else if canonical.contains("claude-3-7-sonnet") {
        "Claude 3.7 Sonnet"
    } else if canonical.contains("claude-3-5-sonnet") {
        "Claude 3.5 Sonnet"
    } else if canonical.contains("claude-haiku-4-5") {
        "Haiku 4.5"
    } else if canonical.contains("claude-3-5-haiku") {
        "Claude 3.5 Haiku"
    } else {
        return None;
    };
    Some(name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| {
            pairs
                .iter()
                .find_map(|(name, value)| (*name == key).then(|| (*value).to_string()))
        }
    }

    /// Maps to: CC `isOpus1mMergeEnabled()` (`utils/model/model.ts`). Every
    /// input is process state at the source, so the test drives process state.
    ///
    /// CC also closes the gate for a Pro subscriber and fails closed for a
    /// subscriber whose tokens carry no `subscriptionType`. There are no
    /// claude.ai subscribers any more, so those cases are gone: the gate depends
    /// only on the 1M opt-out, and stored or ambient claude.ai credentials
    /// (here a Pro subscriber and a bare OAuth token) are ignored.
    #[test]
    fn opus_1m_merge_gate_depends_only_on_opt_out_not_on_claude_ai_credentials() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _opt_out = EnvVarGuard::unset("NYAN_DISABLE_1M_CONTEXT");

        let config_dir =
            std::env::temp_dir().join(format!("nyan-opus1m-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&config_dir).unwrap();
        let _config = EnvVarGuard::set("NYAN_CONFIG_DIR", &config_dir);

        // First-party with no credentials at all: the merge is on.
        assert!(is_opus_1m_merge_enabled());

        // A stored Pro subscription and an ambient OAuth token are ignored.
        std::fs::write(
            config_dir.join(".credentials.json"),
            serde_json::json!({ "claudeAiOauth": {
                "accessToken": "sk-ant-oat01-test",
                "refreshToken": "sk-ant-ort01-test",
                "expiresAt": 99_999_999_999_999u64,
                "scopes": ["user:inference"],
                "subscriptionType": "pro",
            }})
            .to_string(),
        )
        .unwrap();
        let _oauth_token = EnvVarGuard::set("CLAUDE_CODE_OAUTH_TOKEN", "test-oauth-token");
        crate::bootstrap::state::reset_auth_file_descriptor_caches_for_testing();
        assert!(is_opus_1m_merge_enabled());

        // Explicit opt-out closes the gate.
        crate::utils::process_env::set("NYAN_DISABLE_1M_CONTEXT", "1");
        assert!(!is_opus_1m_merge_enabled());
        crate::utils::process_env::remove("NYAN_DISABLE_1M_CONTEXT");

        let _ = std::fs::remove_dir_all(&config_dir);
    }

    /// Auxiliary requests follow the main profile and never read the removed
    /// `ANTHROPIC_SMALL_FAST_MODEL`; without a profile they stay empty.
    #[test]
    fn small_fast_model_follows_main_profile_and_ignores_legacy_env() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let previous_override = crate::bootstrap::state::get_session_profile();
        let root =
            std::env::temp_dir().join(format!("nyan-small-fast-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("settings.json"), "{}").unwrap();
        let _config = EnvVarGuard::set("NYAN_CONFIG_DIR", &root);
        let _legacy = EnvVarGuard::set("ANTHROPIC_SMALL_FAST_MODEL", "legacy-haiku");
        crate::utils::settings::settings_cache::reset_settings_cache();
        crate::bootstrap::state::set_session_profile(None);

        assert_eq!(get_small_fast_model(), "");

        std::fs::write(
            root.join("settings.json"),
            r#"{
                "nyan": {
                    "providers": {"anthropic": {"kind": "anthropic", "protocol": "anthropicMessages"}},
                    "models": {"main": {"provider": "anthropic", "model": "claude-sonnet-4-6", "capabilities": {"contextWindow": 200000, "maxOutputTokens": 128000}}},
                    "selection": {"main": "main"}
                }
            }"#,
        )
        .unwrap();
        crate::utils::settings::settings_cache::reset_settings_cache();
        assert_eq!(get_small_fast_model(), "claude-sonnet-4-6");

        crate::bootstrap::state::set_session_profile(previous_override);
        crate::utils::settings::settings_cache::reset_settings_cache();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn profile_selection_resolves_the_wire_model_and_ignores_legacy_model_env() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let previous_override = crate::bootstrap::state::get_session_profile();
        let root = std::env::temp_dir().join(format!(
            "nyan-model-profile-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("settings.json"),
            r#"{
                "nyan": {
                    "providers": {"anthropic": {"kind": "anthropic", "protocol": "anthropicMessages"}},
                    "models": {"main": {"provider": "anthropic", "model": "claude-sonnet-4-6", "capabilities": {"contextWindow": 200000, "maxOutputTokens": 128000}}},
                    "selection": {"main": "main"}
                }
            }"#,
        )
        .unwrap();
        let _config = EnvVarGuard::set("NYAN_CONFIG_DIR", &root);
        let _legacy = EnvVarGuard::set("ANTHROPIC_MODEL", "legacy-model");
        crate::utils::settings::settings_cache::reset_settings_cache();
        crate::bootstrap::state::set_session_profile(None);

        assert_eq!(get_main_loop_model(), "claude-sonnet-4-6");

        crate::bootstrap::state::set_session_profile(previous_override);
        crate::utils::settings::settings_cache::reset_settings_cache();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn missing_profile_stays_unset_even_when_legacy_model_env_is_present() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let previous_override = crate::bootstrap::state::get_session_profile();
        let root = std::env::temp_dir().join(format!(
            "nyan-model-no-profile-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("settings.json"), "{}").unwrap();
        let _config = EnvVarGuard::set("NYAN_CONFIG_DIR", &root);
        let _legacy = EnvVarGuard::set("ANTHROPIC_MODEL", "legacy-model");
        crate::utils::settings::settings_cache::reset_settings_cache();
        crate::bootstrap::state::set_session_profile(None);

        assert_eq!(get_main_loop_model(), "");

        crate::bootstrap::state::set_session_profile(previous_override);
        crate::utils::settings::settings_cache::reset_settings_cache();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn canonical_model_names_match_official_provider_and_date_stripping() {
        assert_eq!(
            first_party_name_to_canonical("us.anthropic.claude-opus-4-6-v1:0"),
            "claude-opus-4-6"
        );
        assert_eq!(
            first_party_name_to_canonical("claude-3-5-haiku-20241022"),
            "claude-3-5-haiku"
        );
        assert_eq!(
            first_party_name_to_canonical("claude-strudel-v6-p"),
            "claude-strudel"
        );
    }

    #[test]
    fn normalize_model_string_for_api_strips_context_suffixes() {
        assert_eq!(
            normalize_model_string_for_api("claude-sonnet-4-20250514[1m]"),
            "claude-sonnet-4-20250514"
        );
        assert_eq!(
            normalize_model_string_for_api("claude-opus-4-20250514[2M]"),
            "claude-opus-4-20250514"
        );
    }

    #[test]
    fn is_non_custom_opus_model_matches_canonical_opus_family() {
        assert!(is_non_custom_opus_model("claude-opus-4-20250514"));
        assert!(is_non_custom_opus_model("us.anthropic.claude-opus-4-6-v1"));
        assert!(!is_non_custom_opus_model("claude-sonnet-4-20250514"));
        assert!(!is_non_custom_opus_model("custom-claude-opus-4"));
    }
}
