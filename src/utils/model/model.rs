//! Model identity helpers.
//! Maps to CC `utils/model/model.ts`.

/// Maps to: CC `utils/model/configs.ts` `CLAUDE_HAIKU_4_5_CONFIG.firstParty`.
pub const DEFAULT_HAIKU_MODEL: &str = "claude-haiku-4-5-20251001";

/// Maps to: CC `utils/model/configs.ts` `CLAUDE_SONNET_4_6_CONFIG.firstParty`.
pub const DEFAULT_SONNET_MODEL: &str = "claude-sonnet-4-6";

/// Maps to: CC `utils/model/configs.ts` `CLAUDE_OPUS_4_6_CONFIG.firstParty`.
pub const DEFAULT_OPUS_MODEL: &str = "claude-opus-4-6";

use crate::utils::env_utils::truthy_env_var;

/// Maps to: CC `utils/model/model.ts:36-38` `getSmallFastModel()`.
pub fn get_small_fast_model() -> String {
    truthy_env_var("ANTHROPIC_SMALL_FAST_MODEL").unwrap_or_else(get_default_haiku_model)
}

/// Maps to: CC `utils/model/model.ts` `getDefaultSonnetModel()`.
/// TODO: Port provider-specific model string tables; first-party defaults are
/// kept in sync with CC `utils/model/configs.ts`.
pub fn get_default_sonnet_model() -> String {
    truthy_env_var("ANTHROPIC_DEFAULT_SONNET_MODEL")
        .unwrap_or_else(|| DEFAULT_SONNET_MODEL.to_string())
}

/// Maps to: CC `utils/model/model.ts` `getDefaultOpusModel()`.
/// TODO: Port provider-specific model string tables; first-party defaults are
/// kept in sync with CC `utils/model/configs.ts`.
pub fn get_default_opus_model() -> String {
    truthy_env_var("ANTHROPIC_DEFAULT_OPUS_MODEL").unwrap_or_else(|| DEFAULT_OPUS_MODEL.to_string())
}

/// Maps to: CC `utils/model/model.ts:131-138` `getDefaultHaikuModel()`.
/// TODO: Port provider-specific model string tables; first-party defaults are
/// kept in sync with CC `utils/model/configs.ts`.
pub fn get_default_haiku_model() -> String {
    truthy_env_var("ANTHROPIC_DEFAULT_HAIKU_MODEL")
        .unwrap_or_else(|| DEFAULT_HAIKU_MODEL.to_string())
}

/// Maps to: CC `utils/model/model.ts` `getDefaultMainLoopModelSetting()`.
///
/// Cometix does not yet port Max/Team Premium subscriber detection; preserve
/// the source-level split that affects Query fallback most: ants default to
/// Opus with 1M context, all other users default to Sonnet.
pub fn get_default_main_loop_model_setting_for_audience(
    audience: crate::utils::build_profile::BuildAudience,
) -> String {
    if crate::utils::build_profile::audience_has_internal_capability(
        audience,
        crate::utils::build_profile::InternalCapability::Models,
    ) {
        format!("{}[1m]", get_default_opus_model())
    } else {
        get_default_sonnet_model()
    }
}

pub fn get_default_main_loop_model_setting() -> String {
    get_default_main_loop_model_setting_for_audience(crate::utils::build_profile::build_audience())
}

/// Maps to: CC `utils/model/model.ts` `getDefaultMainLoopModel()`.
pub fn get_default_main_loop_model() -> String {
    parse_user_specified_model(&get_default_main_loop_model_setting())
}

/// Maps to: CC `utils/model/model.ts` `getUserSpecifiedModelSetting()`.
///
/// Official precedence is the runtime/startup bootstrap override,
/// `ANTHROPIC_MODEL`, then settings. The override's explicit-null value stops
/// lookup rather than falling through.
pub fn get_user_specified_model_setting() -> Option<String> {
    let specified_model =
        if let Some(model_override) = crate::bootstrap::state::get_main_loop_model_override() {
            model_override
        } else {
            truthy_env_var("ANTHROPIC_MODEL")
                .or_else(|| crate::utils::settings::get_initial_settings().model)
        };

    // Ignore the user-specified model if it's not in the availableModels
    // allowlist. The source's truthiness guard also skips an empty setting.
    if let Some(model) = specified_model.as_deref() {
        if !model.is_empty() && !crate::utils::model::model_allowlist::is_model_allowed(model) {
            return None;
        }
    }

    specified_model
}

/// Maps to: CC `utils/model/model.ts` `parseUserSpecifiedModel(...)`.
pub fn parse_user_specified_model(model_input: &str) -> String {
    let trimmed = model_input.trim();
    let normalized = trimmed.to_ascii_lowercase();
    let has_1m_tag = normalized.ends_with("[1m]");
    let model_string = if has_1m_tag {
        normalized
            .strip_suffix("[1m]")
            .unwrap_or(&normalized)
            .trim()
            .to_string()
    } else {
        normalized.clone()
    };
    let suffix = if has_1m_tag { "[1m]" } else { "" };

    match model_string.as_str() {
        "opusplan" => format!("{}{}", get_default_sonnet_model(), suffix),
        "sonnet" => {
            if has_1m_tag {
                if let Some(custom_1m) = truthy_env_var("ANTHROPIC_DEFAULT_SONNET_1M_MODEL") {
                    return custom_1m;
                }
            }
            format!("{}{}", get_default_sonnet_model(), suffix)
        }
        "haiku" => format!("{}{}", get_default_haiku_model(), suffix),
        "opus" => {
            if has_1m_tag {
                if let Some(custom_1m) = truthy_env_var("ANTHROPIC_DEFAULT_OPUS_1M_MODEL") {
                    return custom_1m;
                }
            }
            format!("{}{}", get_default_opus_model(), suffix)
        }
        "best" => get_default_opus_model(),
        _ if has_1m_tag => {
            let without_tag = trimmed
                .strip_suffix("[1m]")
                .or_else(|| trimmed.strip_suffix("[1M]"))
                .unwrap_or(trimmed)
                .trim();
            format!("{without_tag}[1m]")
        }
        _ => trimmed.to_string(),
    }
}

/// Maps to: CC `utils/model/model.ts` `getMainLoopModel()`.
pub fn get_main_loop_model() -> String {
    if let Some(model) = get_user_specified_model_setting() {
        if !model.trim().is_empty() {
            return parse_user_specified_model(&model);
        }
    }
    get_default_main_loop_model()
}

/// Return the live session model without consulting settings or credentials.
///
/// Startup publishes the initial resolved setting and `/model` publishes an
/// explicit override into bootstrap state. Getter-backed command metadata can
/// therefore stay live without introducing filesystem or keychain I/O into
/// REPL render paths.
pub fn get_session_main_loop_model() -> String {
    let selected = match crate::bootstrap::state::get_main_loop_model_override() {
        Some(model) => model,
        None => crate::bootstrap::state::get_initial_main_loop_model(),
    };
    selected
        .filter(|model| !model.trim().is_empty())
        .map(|model| parse_user_specified_model(&model))
        .unwrap_or_else(get_default_main_loop_model)
}

/// Maps to: CC `utils/model/model.ts` `getRuntimeMainLoopModel(...)`.
pub fn get_runtime_main_loop_model(
    permission_mode: crate::types::permissions::PermissionMode,
    main_loop_model: String,
    exceeds_200k_tokens: bool,
) -> String {
    let user_setting = get_user_specified_model_setting()
        .map(|model| model.trim().to_ascii_lowercase())
        .unwrap_or_default();

    if user_setting == "opusplan"
        && permission_mode == crate::types::permissions::PermissionMode::Plan
        && !exceeds_200k_tokens
    {
        return get_default_opus_model();
    }

    if user_setting == "haiku" && permission_mode == crate::types::permissions::PermissionMode::Plan
    {
        return get_default_sonnet_model();
    }

    main_loop_model
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
    first_party_name_to_canonical(
        &crate::utils::model::model_strings::resolve_overridden_model(full_model_name),
    )
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
/// Cometix matches first-party marketing names by substring until provider
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
    } else if lower == "opus" || lower == "opus[1m]" {
        if has_1m { "Opus 1M" } else { "Opus" }
    } else if lower == "sonnet" || lower == "sonnet[1m]" {
        if has_1m { "Sonnet 1M" } else { "Sonnet" }
    } else if lower == "haiku" {
        "Haiku"
    } else if lower == "default" || lower == "__no_preference__" {
        "Default"
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
    if let Some(public) = get_public_model_display_name(model) {
        return public;
    }
    if crate::utils::build_profile::has_internal_capability(
        crate::utils::build_profile::InternalCapability::Models,
    ) {
        let resolved = parse_user_specified_model(model);
        if resolved != model {
            return format!("{model} ({resolved})");
        }
        return resolved;
    }
    model.to_string()
}

/// Maps to: CC `utils/model/model.ts` `isNonCustomOpusModel(...)`.
pub fn is_non_custom_opus_model(model: &str) -> bool {
    let model = model.trim().to_ascii_lowercase();
    if model.is_empty() || model.starts_with("custom-") {
        return false;
    }

    // CC compares against provider-specific `getModelStrings().opus*` values.
    // Cometix does not yet port provider model string profiles, so match the
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
    if crate::utils::context::is_1m_context_disabled()
        || crate::utils::model::providers::get_api_provider()
            != crate::utils::model::providers::ApiProvider::FirstParty
    {
        return false;
    }

    let subscription_type = crate::utils::auth::get_subscription_type();
    if subscription_type.as_deref() == Some("pro") {
        return false;
    }

    if crate::utils::auth::is_claude_ai_subscriber() && subscription_type.is_none() {
        return false;
    }

    true
}

// @[MODEL LAUNCH]: Update the default model description strings shown to users.
/// Maps to: CC `utils/model/model.ts:286-296#getClaudeAiUserDefaultModelDescription`.
pub fn get_claude_ai_user_default_model_description(fast_mode: bool) -> String {
    if crate::utils::auth::is_max_subscriber() || crate::utils::auth::is_team_premium_subscriber() {
        let suffix = if fast_mode {
            get_opus_46_pricing_suffix(true)
        } else {
            String::new()
        };
        if is_opus_1m_merge_enabled() {
            return format!("Opus 4.6 with 1M context · Most capable for complex work{suffix}");
        }
        return format!("Opus 4.6 · Most capable for complex work{suffix}");
    }
    "Sonnet 4.6 · Best for everyday tasks".to_string()
}

/// Maps to: CC `utils/model/model.ts:298-305#renderDefaultModelSetting`.
pub fn render_default_model_setting(setting: &str) -> String {
    if setting == "opusplan" {
        return "Opus 4.6 in plan mode, else Sonnet 4.6".to_string();
    }
    render_model_name(&parse_user_specified_model(setting))
}

/// Maps to: CC `utils/model/model.ts:307-312#getOpus46PricingSuffix`.
pub fn get_opus_46_pricing_suffix(fast_mode: bool) -> String {
    if crate::utils::model::providers::get_api_provider()
        != crate::utils::model::providers::ApiProvider::FirstParty
    {
        return String::new();
    }
    let pricing = crate::utils::model_cost::format_model_pricing(
        crate::utils::model_cost::get_opus_46_cost_tier(fast_mode),
    );
    let fast_mode_indicator = if fast_mode {
        format!(" ({})", crate::constants::figures::LIGHTNING_BOLT)
    } else {
        String::new()
    };
    format!(" ·{fast_mode_indicator} {pricing}")
}

/// Maps to: CC `utils/model/model.ts:508-536#resolveSkillModelOverride`.
///
/// Resolves a skill's `model:` frontmatter against the current model, carrying
/// the `[1m]` suffix over when the target family supports it.
///
/// A skill author writing `model: opus` means "use opus-class reasoning" — not
/// "downgrade to 200K". If the user is on `opus[1m]` at 230K tokens and invokes
/// a skill with `model: opus`, passing the bare alias through drops the
/// effective context window from 1M to 200K, which trips autocompact at 23%
/// apparent usage and surfaces "Context limit reached" even though nothing
/// overflowed.
///
/// The suffix only carries when the target actually supports it (sonnet/opus).
/// `model: haiku` on a 1M session still downgrades — haiku has no 1M variant, so
/// the autocompact that follows is correct. Skills that already specify `[1m]`
/// are left untouched.
///
/// CC's `currentModel` is typed `string` but the call sites pass
/// `options.mainLoopModel`, which can be undefined; `has1mContext(undefined)`
/// stringifies to a non-matching value, so the Rust `""` for an absent model
/// takes the same first branch.
pub fn resolve_skill_model_override(skill_model: &str, current_model: &str) -> String {
    if crate::utils::context::has_1m_context(skill_model)
        || !crate::utils::context::has_1m_context(current_model)
    {
        return skill_model.to_string();
    }
    // modelSupports1M matches on canonical IDs ('claude-opus-4-6',
    // 'claude-sonnet-4'); a bare 'opus' alias falls through unmatched. Resolve
    // first.
    if crate::utils::context::model_supports_1m(&parse_user_specified_model(skill_model)) {
        return format!("{skill_model}[1m]");
    }
    skill_model.to_string()
}

// @[MODEL LAUNCH]: Add a marketing name mapping for the new model below.
/// Maps to: CC `utils/model/model.ts:570-614#getMarketingNameForModel`.
pub fn get_marketing_name_for_model(model_id: &str) -> Option<String> {
    if crate::utils::model::providers::get_api_provider()
        == crate::utils::model::providers::ApiProvider::Foundry
    {
        // A Foundry deployment ID is user-defined, so it may bear no relation
        // to the actual model.
        return None;
    }

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
    /// The fourth case is CC's fail-closed guard and the reason it exists: a
    /// subscriber whose OAuth tokens carry valid scopes but NO
    /// `subscriptionType` (the source cites a stale/partial refresh in the VS
    /// Code config subprocess). Without the guard `isProSubscriber()` is false
    /// for those users and `opus[1m]` leaks into the dropdown, where the API
    /// rejects it as a misleading "rate limit reached".
    #[test]
    fn opus_1m_merge_gate_matches_official_provider_and_unknown_subscriber_guards() {
        let _env_guard = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let _providers = [
            "CLAUDE_CODE_DISABLE_1M_CONTEXT",
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
            "CLAUDE_CODE_USE_FOUNDRY",
        ]
        .map(crate::utils::env_utils::EnvVarGuard::unset);

        let config_dir =
            std::env::temp_dir().join(format!("cometix-opus1m-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&config_dir).unwrap();
        let _config = crate::utils::env_utils::EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config_dir);
        let write_credentials = |oauth: serde_json::Value| {
            std::fs::write(
                config_dir.join(".credentials.json"),
                serde_json::json!({ "claudeAiOauth": oauth }).to_string(),
            )
            .unwrap();
        };

        // A max subscriber on first-party: the merge is on.
        write_credentials(serde_json::json!({
            "accessToken": "sk-ant-oat01-test",
            "refreshToken": "sk-ant-ort01-test",
            "expiresAt": 99_999_999_999_999u64,
            "scopes": ["user:inference"],
            "subscriptionType": "max",
        }));
        assert!(is_opus_1m_merge_enabled());

        // Non-first-party provider closes the gate.
        crate::utils::process_env::set("CLAUDE_CODE_USE_BEDROCK", "1");
        assert!(!is_opus_1m_merge_enabled());
        crate::utils::process_env::remove("CLAUDE_CODE_USE_BEDROCK");

        // Explicit opt-out closes the gate.
        crate::utils::process_env::set("CLAUDE_CODE_DISABLE_1M_CONTEXT", "1");
        assert!(!is_opus_1m_merge_enabled());
        crate::utils::process_env::remove("CLAUDE_CODE_DISABLE_1M_CONTEXT");

        // Subscriber with tokens but no subscriptionType: fail closed.
        write_credentials(serde_json::json!({
            "accessToken": "sk-ant-oat01-test",
            "refreshToken": "sk-ant-ort01-test",
            "expiresAt": 99_999_999_999_999u64,
            "scopes": ["user:inference"],
        }));
        assert!(!is_opus_1m_merge_enabled());

        let _ = std::fs::remove_dir_all(&config_dir);
    }

    /// CC reads exactly one env name per helper (`model.ts:36-38,131-138`),
    /// and `process.env.X || fallback` treats an empty value as unset.
    #[test]
    fn default_model_helpers_read_official_env_names_with_js_truthiness() {
        let _env_guard = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("ANTHROPIC_DEFAULT_SONNET_MODEL", "official-sonnet");
        crate::utils::process_env::set("ANTHROPIC_DEFAULT_OPUS_MODEL", "official-opus");
        crate::utils::process_env::set("ANTHROPIC_SMALL_FAST_MODEL", "official-haiku");

        assert_eq!(get_default_sonnet_model(), "official-sonnet");
        assert_eq!(get_default_opus_model(), "official-opus");
        assert_eq!(get_small_fast_model(), "official-haiku");

        crate::utils::process_env::set("ANTHROPIC_SMALL_FAST_MODEL", "");
        assert_eq!(
            get_small_fast_model(),
            DEFAULT_HAIKU_MODEL,
            "an empty env value is falsy in CC and must fall through"
        );

        crate::utils::process_env::remove("ANTHROPIC_DEFAULT_SONNET_MODEL");
        crate::utils::process_env::remove("ANTHROPIC_DEFAULT_OPUS_MODEL");
        crate::utils::process_env::remove("ANTHROPIC_SMALL_FAST_MODEL");
    }

    #[test]
    fn default_model_helpers_match_current_official_first_party_defaults() {
        let _env_guard = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("ANTHROPIC_DEFAULT_SONNET_MODEL");
        crate::utils::process_env::remove("ANTHROPIC_DEFAULT_OPUS_MODEL");
        crate::utils::process_env::remove("ANTHROPIC_DEFAULT_HAIKU_MODEL");
        crate::utils::process_env::remove("ANTHROPIC_SMALL_FAST_MODEL");

        assert_eq!(get_default_sonnet_model(), DEFAULT_SONNET_MODEL);
        assert_eq!(get_default_opus_model(), DEFAULT_OPUS_MODEL);
        assert_eq!(get_default_haiku_model(), DEFAULT_HAIKU_MODEL);
        assert_eq!(get_small_fast_model(), DEFAULT_HAIKU_MODEL);
    }

    #[test]
    fn default_main_loop_model_keeps_query_fallback_distribution_split() {
        let _env_guard = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("ANTHROPIC_DEFAULT_SONNET_MODEL", "default-sonnet");
        crate::utils::process_env::set("ANTHROPIC_DEFAULT_OPUS_MODEL", "default-opus");
        assert_eq!(
            get_default_main_loop_model_setting_for_audience(
                crate::utils::build_profile::BuildAudience::External,
            ),
            "default-sonnet"
        );
        assert_eq!(
            get_default_main_loop_model_setting_for_audience(
                crate::utils::build_profile::BuildAudience::AnthropicInternal,
            ),
            "default-opus[1m]"
        );
        crate::utils::process_env::remove("ANTHROPIC_DEFAULT_SONNET_MODEL");
        crate::utils::process_env::remove("ANTHROPIC_DEFAULT_OPUS_MODEL");
    }

    #[test]
    fn parse_user_specified_model_resolves_official_aliases_and_preserves_custom_case() {
        let _env_guard = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("ANTHROPIC_DEFAULT_SONNET_MODEL", "default-sonnet");
        crate::utils::process_env::set("ANTHROPIC_DEFAULT_OPUS_MODEL", "default-opus");
        crate::utils::process_env::set("ANTHROPIC_DEFAULT_HAIKU_MODEL", "default-haiku");

        assert_eq!(parse_user_specified_model("sonnet"), "default-sonnet");
        assert_eq!(parse_user_specified_model("opus[1M]"), "default-opus[1m]");
        assert_eq!(parse_user_specified_model("haiku"), "default-haiku");
        assert_eq!(parse_user_specified_model("opusplan"), "default-sonnet");
        assert_eq!(parse_user_specified_model("best"), "default-opus");
        assert_eq!(
            parse_user_specified_model("AzureDeployment"),
            "AzureDeployment"
        );
        assert_eq!(
            parse_user_specified_model("AzureDeployment[1M]"),
            "AzureDeployment[1m]"
        );

        crate::utils::process_env::remove("ANTHROPIC_DEFAULT_SONNET_MODEL");
        crate::utils::process_env::remove("ANTHROPIC_DEFAULT_OPUS_MODEL");
        crate::utils::process_env::remove("ANTHROPIC_DEFAULT_HAIKU_MODEL");
    }

    /// Maps to: CC `utils/model/model.ts:508-536#resolveSkillModelOverride`.
    /// The `[1m]` suffix is a context-window tag, not a model family: a skill
    /// declaring `model: opus` on an `opus[1m]` session means "opus-class
    /// reasoning", so dropping the tag would silently cut the window from 1M to
    /// 200K and trip autocompact at ~23% apparent usage.
    #[test]
    fn resolve_skill_model_override_carries_the_1m_suffix_only_where_supported() {
        let _env_guard = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let _model_env = [
            crate::utils::env_utils::EnvVarGuard::unset("CLAUDE_CODE_DISABLE_1M_CONTEXT"),
            crate::utils::env_utils::EnvVarGuard::set(
                "ANTHROPIC_DEFAULT_SONNET_MODEL",
                "claude-sonnet-4-5",
            ),
            crate::utils::env_utils::EnvVarGuard::set(
                "ANTHROPIC_DEFAULT_OPUS_MODEL",
                "claude-opus-4-6",
            ),
            crate::utils::env_utils::EnvVarGuard::set(
                "ANTHROPIC_DEFAULT_HAIKU_MODEL",
                "claude-haiku-4-5",
            ),
        ];

        // 1M session + 1M-capable target → carry the tag.
        assert_eq!(resolve_skill_model_override("opus", "opus[1m]"), "opus[1m]");
        assert_eq!(
            resolve_skill_model_override("sonnet", "claude-opus-4-6[1m]"),
            "sonnet[1m]"
        );
        // haiku has no 1M variant — the downgrade (and the autocompact that
        // follows it) is correct.
        assert_eq!(resolve_skill_model_override("haiku", "opus[1m]"), "haiku");
        // Already tagged, or a non-1M session: untouched.
        assert_eq!(
            resolve_skill_model_override("opus[1m]", "opus[1m]"),
            "opus[1m]"
        );
        assert_eq!(
            resolve_skill_model_override("opus", "claude-sonnet-4-5"),
            "opus"
        );
        // CC's callers pass `options.mainLoopModel`, which can be undefined;
        // `has1mContext(undefined)` is false, so the alias passes through.
        assert_eq!(resolve_skill_model_override("opus", ""), "opus");
    }

    /// Maps to: CC `utils/model/model.ts:72-75` — a user-specified model that
    /// the `availableModels` allowlist forbids is dropped, so every consumer
    /// (main loop selection included) falls back to the default rather than
    /// pinning a model policy has ruled out. The source's truthiness guard
    /// leaves an empty setting alone.
    #[test]
    fn user_specified_model_setting_drops_models_the_allowlist_forbids() {
        let _env_guard = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let previous_override = crate::bootstrap::state::get_main_loop_model_override();
        let model = crate::utils::env_utils::EnvVarGuard::unset("ANTHROPIC_MODEL");
        let root = std::env::temp_dir().join(format!(
            "cometix-model-allowlist-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("settings.json"),
            r#"{"availableModels":["haiku"]}"#,
        )
        .unwrap();
        let config = crate::utils::env_utils::EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root);
        let haiku = crate::utils::env_utils::EnvVarGuard::set(
            "ANTHROPIC_DEFAULT_HAIKU_MODEL",
            "default-haiku",
        );
        crate::utils::settings::settings_cache::reset_settings_cache();
        crate::bootstrap::state::set_main_loop_model_override(None);

        crate::utils::process_env::set("ANTHROPIC_MODEL", "haiku");
        assert_eq!(get_user_specified_model_setting().as_deref(), Some("haiku"));
        assert_eq!(get_main_loop_model(), "default-haiku");

        crate::utils::process_env::set("ANTHROPIC_MODEL", "opus");
        assert_eq!(get_user_specified_model_setting(), None);
        assert_eq!(get_main_loop_model(), get_default_main_loop_model());

        // An empty setting stays empty: the source guards the allowlist check
        // behind a truthiness test, so it never reaches `isModelAllowed`.
        crate::bootstrap::state::set_main_loop_model_override(Some(Some(String::new())));
        assert_eq!(get_user_specified_model_setting().as_deref(), Some(""));

        crate::bootstrap::state::set_main_loop_model_override(previous_override);
        drop((haiku, model, config));
        crate::utils::settings::settings_cache::reset_settings_cache();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn get_main_loop_model_parses_official_env_aliases_before_defaults() {
        let _env_guard = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("ANTHROPIC_MODEL", "haiku");
        crate::utils::process_env::set("ANTHROPIC_DEFAULT_HAIKU_MODEL", "default-haiku");

        assert_eq!(get_main_loop_model(), "default-haiku");

        crate::utils::process_env::remove("ANTHROPIC_MODEL");
        crate::utils::process_env::remove("ANTHROPIC_DEFAULT_HAIKU_MODEL");
    }

    #[test]
    fn runtime_main_loop_model_matches_official_plan_mode_alias_overrides() {
        let _env_guard = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("ANTHROPIC_MODEL", "opusplan");
        crate::utils::process_env::set("ANTHROPIC_DEFAULT_OPUS_MODEL", "default-opus");
        crate::utils::process_env::set("ANTHROPIC_DEFAULT_SONNET_MODEL", "default-sonnet");

        assert_eq!(get_main_loop_model(), "default-sonnet");
        assert_eq!(
            get_runtime_main_loop_model(
                crate::types::permissions::PermissionMode::Plan,
                "default-sonnet".to_string(),
                false,
            ),
            "default-opus"
        );
        assert_eq!(
            get_runtime_main_loop_model(
                crate::types::permissions::PermissionMode::Plan,
                "default-sonnet".to_string(),
                true,
            ),
            "default-sonnet"
        );

        crate::utils::process_env::set("ANTHROPIC_MODEL", "haiku");
        assert_eq!(
            get_runtime_main_loop_model(
                crate::types::permissions::PermissionMode::Plan,
                "default-haiku".to_string(),
                false,
            ),
            "default-sonnet"
        );

        crate::utils::process_env::remove("ANTHROPIC_MODEL");
        crate::utils::process_env::remove("ANTHROPIC_DEFAULT_OPUS_MODEL");
        crate::utils::process_env::remove("ANTHROPIC_DEFAULT_SONNET_MODEL");
    }

    #[test]
    fn get_main_loop_model_prefers_anthropic_model_env() {
        let _env_guard = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("ANTHROPIC_MODEL", "official-main-model");

        assert_eq!(get_main_loop_model(), "official-main-model");

        crate::utils::process_env::remove("ANTHROPIC_MODEL");
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
