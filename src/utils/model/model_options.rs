//! Model picker option lists.
//! Maps to CC `utils/model/modelOptions.ts`.
//!
//! Every input is process state — environment variables, the locally cached
//! OAuth snapshot's `subscriptionType`/`rateLimitTier`, `~/.claude.json`,
//! settings, and the static tables in `configs.ts`/`modelCost.ts`. Nothing
//! here performs a network round-trip.

use crate::bootstrap::state::get_initial_main_loop_model;
use crate::utils::auth::{is_claude_ai_subscriber, is_max_subscriber, is_team_premium_subscriber};
use crate::utils::model::check_1m_access::{check_opus_1m_access, check_sonnet_1m_access};
use crate::utils::model::model::{
    get_canonical_name, get_claude_ai_user_default_model_description, get_default_haiku_model,
    get_default_opus_model, get_default_sonnet_model, get_marketing_name_for_model,
    get_opus_46_pricing_suffix, get_user_specified_model_setting, is_opus_1m_merge_enabled,
    render_default_model_setting,
};
use crate::utils::model::model_allowlist::is_model_allowed;
use crate::utils::model::model_strings::get_model_strings;
use crate::utils::model::providers::{ApiProvider, get_api_provider};
use crate::utils::model_cost::{
    COST_HAIKU_35, COST_HAIKU_45, COST_TIER_3_15, format_model_pricing,
};
use crate::utils::process_env::JsTruthy;

// @[MODEL LAUNCH]: Update all the available and default model option strings below.

/// Maps to: CC `utils/model/modelOptions.ts:38-43` `ModelOption`.
///
/// `value` is CC's `ModelSetting`, whose `null` selects the default option;
/// serde maps that onto JSON null so the bootstrap cache round-trips.
///
/// The container `default` keeps a malformed `additionalModelOptionsCache`
/// entry from failing the whole `GlobalConfig` parse, which would send
/// `load_global_config` down its corrupted-file path and discard every other
/// setting. CC reads the same cache as untyped JSON and cannot fail here.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct ModelOption {
    pub value: Option<String>,
    pub label: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description_for_model: Option<String>,
}

impl ModelOption {
    fn new(value: Option<&str>, label: &str, description: String) -> Self {
        Self {
            value: value.map(str::to_string),
            label: label.to_string(),
            description,
            description_for_model: None,
        }
    }

    fn with_description_for_model(mut self, description_for_model: &str) -> Self {
        self.description_for_model = Some(description_for_model.to_string());
        self
    }
}

fn is_3p() -> bool {
    get_api_provider() != ApiProvider::FirstParty
}

fn tier_3_15_suffix() -> String {
    if is_3p() {
        String::new()
    } else {
        format!(" · {}", format_model_pricing(COST_TIER_3_15))
    }
}

/// Maps to: CC `utils/model/modelOptions.ts:45-74` `getDefaultOptionForUser`.
pub fn get_default_option_for_user(fast_mode: bool) -> ModelOption {
    get_default_option_for_user_for_audience(
        crate::utils::build_profile::build_audience(),
        fast_mode,
    )
}

/// The audience-parameterized body of [`get_default_option_for_user`]. CC gates
/// its first branch on `process.env.USER_TYPE === 'ant'`, which this port
/// resolves at compile time, so tests reach both profiles through here.
pub fn get_default_option_for_user_for_audience(
    audience: crate::utils::build_profile::BuildAudience,
    fast_mode: bool,
) -> ModelOption {
    if crate::utils::build_profile::audience_has_internal_capability(
        audience,
        crate::utils::build_profile::InternalCapability::Models,
    ) {
        // CC gates this branch and `getDefaultMainLoopModelSetting()`'s ant
        // branch on the same `USER_TYPE === 'ant'`, so they take the audience
        // together.
        let current_model = render_default_model_setting(
            &crate::utils::model::model::get_default_main_loop_model_setting_for_audience(audience),
        );
        return ModelOption::new(
            None,
            "Default (recommended)",
            format!("Use the default model for Ants (currently {current_model})"),
        )
        .with_description_for_model(&format!("Default model (currently {current_model})"));
    }

    if is_claude_ai_subscriber() {
        return ModelOption::new(
            None,
            "Default (recommended)",
            get_claude_ai_user_default_model_description(fast_mode),
        );
    }

    ModelOption::new(
        None,
        "Default (recommended)",
        format!(
            "Use the default model (currently {}){}",
            // The audience must flow into this arm too: the nullary
            // `get_default_main_loop_model_setting()` resolves the COMPILED
            // audience, so under `--features anthropic_internal` a caller
            // asking for the External option still saw the ant default model.
            render_default_model_setting(
                &crate::utils::model::model::get_default_main_loop_model_setting_for_audience(
                    audience
                )
            ),
            tier_3_15_suffix()
        ),
    )
}

/// Maps to: CC `utils/model/modelOptions.ts:76-92` `getCustomSonnetOption`.
fn get_custom_sonnet_option() -> Option<ModelOption> {
    let custom_sonnet_model =
        crate::utils::process_env::var("ANTHROPIC_DEFAULT_SONNET_MODEL").truthy()?;
    if !is_3p() {
        return None;
    }
    let is_1m = crate::utils::context::has_1m_context(&custom_sonnet_model);
    let description = crate::utils::process_env::var("ANTHROPIC_DEFAULT_SONNET_MODEL_DESCRIPTION");
    Some(
        ModelOption::new(
            Some("sonnet"),
            &crate::utils::process_env::var("ANTHROPIC_DEFAULT_SONNET_MODEL_NAME")
                .unwrap_or_else(|| custom_sonnet_model.clone()),
            description.clone().unwrap_or_else(|| {
                format!(
                    "Custom Sonnet model{}",
                    if is_1m { " (1M context)" } else { "" }
                )
            }),
        )
        .with_description_for_model(&format!(
            "{} ({custom_sonnet_model})",
            description.unwrap_or_else(|| format!(
                "Custom Sonnet model{}",
                if is_1m { " with 1M context" } else { "" }
            ))
        )),
    )
}

// @[MODEL LAUNCH]: Update or add model option functions (get_sonnet_xx_option,
// get_opus_xx_option, etc.) with the new model's label and description.
/// Maps to: CC `utils/model/modelOptions.ts:96-105` `getSonnet46Option`.
fn get_sonnet_46_option() -> ModelOption {
    let value = if is_3p() {
        get_model_strings().sonnet46
    } else {
        "sonnet".to_string()
    };
    ModelOption::new(
        Some(&value),
        "Sonnet",
        format!("Sonnet 4.6 · Best for everyday tasks{}", tier_3_15_suffix()),
    )
    .with_description_for_model(
        "Sonnet 4.6 - best for everyday tasks. Generally recommended for most coding tasks",
    )
}

/// Maps to: CC `utils/model/modelOptions.ts:107-122` `getCustomOpusOption`.
fn get_custom_opus_option() -> Option<ModelOption> {
    let custom_opus_model =
        crate::utils::process_env::var("ANTHROPIC_DEFAULT_OPUS_MODEL").truthy()?;
    if !is_3p() {
        return None;
    }
    let is_1m = crate::utils::context::has_1m_context(&custom_opus_model);
    let description = crate::utils::process_env::var("ANTHROPIC_DEFAULT_OPUS_MODEL_DESCRIPTION");
    Some(
        ModelOption::new(
            Some("opus"),
            &crate::utils::process_env::var("ANTHROPIC_DEFAULT_OPUS_MODEL_NAME")
                .unwrap_or_else(|| custom_opus_model.clone()),
            description.clone().unwrap_or_else(|| {
                format!(
                    "Custom Opus model{}",
                    if is_1m { " (1M context)" } else { "" }
                )
            }),
        )
        .with_description_for_model(&format!(
            "{} ({custom_opus_model})",
            description.unwrap_or_else(|| format!(
                "Custom Opus model{}",
                if is_1m { " with 1M context" } else { "" }
            ))
        )),
    )
}

/// Maps to: CC `utils/model/modelOptions.ts:124-131` `getOpus41Option`.
fn get_opus_41_option() -> ModelOption {
    ModelOption::new(Some("opus"), "Opus 4.1", "Opus 4.1 · Legacy".to_string())
        .with_description_for_model("Opus 4.1 - legacy version")
}

/// Maps to: CC `utils/model/modelOptions.ts:133-141` `getOpus46Option`.
fn get_opus_46_option(fast_mode: bool) -> ModelOption {
    let value = if is_3p() {
        get_model_strings().opus46
    } else {
        "opus".to_string()
    };
    ModelOption::new(
        Some(&value),
        "Opus",
        format!(
            "Opus 4.6 · Most capable for complex work{}",
            get_opus_46_pricing_suffix(fast_mode)
        ),
    )
    .with_description_for_model("Opus 4.6 - most capable for complex work")
}

/// Maps to: CC `utils/model/modelOptions.ts:143-152` `getSonnet46_1MOption`.
pub fn get_sonnet_46_1m_option() -> ModelOption {
    let value = if is_3p() {
        format!("{}[1m]", get_model_strings().sonnet46)
    } else {
        "sonnet[1m]".to_string()
    };
    ModelOption::new(
        Some(&value),
        "Sonnet (1M context)",
        format!("Sonnet 4.6 for long sessions{}", tier_3_15_suffix()),
    )
    .with_description_for_model(
        "Sonnet 4.6 with 1M context window - for long sessions with large codebases",
    )
}

/// Maps to: CC `utils/model/modelOptions.ts:154-163` `getOpus46_1MOption`.
pub fn get_opus_46_1m_option(fast_mode: bool) -> ModelOption {
    let value = if is_3p() {
        format!("{}[1m]", get_model_strings().opus46)
    } else {
        "opus[1m]".to_string()
    };
    ModelOption::new(
        Some(&value),
        "Opus (1M context)",
        format!(
            "Opus 4.6 for long sessions{}",
            get_opus_46_pricing_suffix(fast_mode)
        ),
    )
    .with_description_for_model(
        "Opus 4.6 with 1M context window - for long sessions with large codebases",
    )
}

/// Maps to: CC `utils/model/modelOptions.ts:165-179` `getCustomHaikuOption`.
fn get_custom_haiku_option() -> Option<ModelOption> {
    let custom_haiku_model =
        crate::utils::process_env::var("ANTHROPIC_DEFAULT_HAIKU_MODEL").truthy()?;
    if !is_3p() {
        return None;
    }
    let description = crate::utils::process_env::var("ANTHROPIC_DEFAULT_HAIKU_MODEL_DESCRIPTION");
    Some(
        ModelOption::new(
            Some("haiku"),
            &crate::utils::process_env::var("ANTHROPIC_DEFAULT_HAIKU_MODEL_NAME")
                .unwrap_or_else(|| custom_haiku_model.clone()),
            description
                .clone()
                .unwrap_or_else(|| "Custom Haiku model".to_string()),
        )
        .with_description_for_model(&format!(
            "{} ({custom_haiku_model})",
            description.unwrap_or_else(|| "Custom Haiku model".to_string())
        )),
    )
}

/// Maps to: CC `utils/model/modelOptions.ts:181-190` `getHaiku45Option`.
fn get_haiku_45_option() -> ModelOption {
    let pricing = if is_3p() {
        String::new()
    } else {
        format!(" · {}", format_model_pricing(COST_HAIKU_45))
    };
    ModelOption::new(
        Some("haiku"),
        "Haiku",
        format!("Haiku 4.5 · Fastest for quick answers{pricing}"),
    )
    .with_description_for_model(
        "Haiku 4.5 - fastest for quick answers. Lower cost but less capable than Sonnet 4.6.",
    )
}

/// Maps to: CC `utils/model/modelOptions.ts:192-201` `getHaiku35Option`.
fn get_haiku_35_option() -> ModelOption {
    let pricing = if is_3p() {
        String::new()
    } else {
        format!(" · {}", format_model_pricing(COST_HAIKU_35))
    };
    ModelOption::new(
        Some("haiku"),
        "Haiku",
        format!("Haiku 3.5 for simple tasks{pricing}"),
    )
    .with_description_for_model(
        "Haiku 3.5 - faster and lower cost, but less capable than Sonnet. Use for simple tasks.",
    )
}

/// Maps to: CC `utils/model/modelOptions.ts:203-209` `getHaikuOption`.
fn get_haiku_option() -> ModelOption {
    if get_default_haiku_model() == get_model_strings().haiku45 {
        get_haiku_45_option()
    } else {
        get_haiku_35_option()
    }
}

/// Maps to: CC `utils/model/modelOptions.ts:211-217` `getMaxOpusOption`.
fn get_max_opus_option(fast_mode: bool) -> ModelOption {
    let suffix = if fast_mode {
        get_opus_46_pricing_suffix(true)
    } else {
        String::new()
    };
    ModelOption::new(
        Some("opus"),
        "Opus",
        format!("Opus 4.6 · Most capable for complex work{suffix}"),
    )
}

/// Maps to: CC `utils/model/modelOptions.ts:219-227` `getMaxSonnet46_1MOption`.
pub fn get_max_sonnet_46_1m_option() -> ModelOption {
    let billing_info = if is_claude_ai_subscriber() {
        " · Billed as extra usage"
    } else {
        ""
    };
    ModelOption::new(
        Some("sonnet[1m]"),
        "Sonnet (1M context)",
        format!(
            "Sonnet 4.6 with 1M context{billing_info}{}",
            tier_3_15_suffix()
        ),
    )
}

/// Maps to: CC `utils/model/modelOptions.ts:229-236` `getMaxOpus46_1MOption`.
pub fn get_max_opus_46_1m_option(fast_mode: bool) -> ModelOption {
    let billing_info = if is_claude_ai_subscriber() {
        " · Billed as extra usage"
    } else {
        ""
    };
    ModelOption::new(
        Some("opus[1m]"),
        "Opus (1M context)",
        format!(
            "Opus 4.6 with 1M context{billing_info}{}",
            get_opus_46_pricing_suffix(fast_mode)
        ),
    )
}

/// Maps to: CC `utils/model/modelOptions.ts:238-247` `getMergedOpus1MOption`.
fn get_merged_opus_1m_option(fast_mode: bool) -> ModelOption {
    let is_3p = is_3p();
    let value = if is_3p {
        format!("{}[1m]", get_model_strings().opus46)
    } else {
        "opus[1m]".to_string()
    };
    let suffix = if !is_3p && fast_mode {
        get_opus_46_pricing_suffix(fast_mode)
    } else {
        String::new()
    };
    ModelOption::new(
        Some(&value),
        "Opus (1M context)",
        format!("Opus 4.6 with 1M context · Most capable for complex work{suffix}"),
    )
    .with_description_for_model("Opus 4.6 with 1M context - most capable for complex work")
}

/// Maps to: CC `utils/model/modelOptions.ts:249-253` `MaxSonnet46Option`.
fn max_sonnet_46_option() -> ModelOption {
    ModelOption::new(
        Some("sonnet"),
        "Sonnet",
        "Sonnet 4.6 · Best for everyday tasks".to_string(),
    )
}

/// Maps to: CC `utils/model/modelOptions.ts:255-259` `MaxHaiku45Option`.
fn max_haiku_45_option() -> ModelOption {
    ModelOption::new(
        Some("haiku"),
        "Haiku",
        "Haiku 4.5 · Fastest for quick answers".to_string(),
    )
}

/// Maps to: CC `utils/model/modelOptions.ts:261-267` `getOpusPlanOption`.
fn get_opus_plan_option() -> ModelOption {
    ModelOption::new(
        Some("opusplan"),
        "Opus Plan Mode",
        "Use Opus 4.6 in plan mode, Sonnet 4.6 otherwise".to_string(),
    )
}

/// Maps to: CC `utils/model/antModels.ts:44-49` `getAntModels`.
///
/// TODO(parity): the source reads the `tengu_ant_model_override` GrowthBook
/// config (`antModels.ts:34-42`), which is unported. CC returns `[]` for the
/// same reason whenever that config is absent, so the internal-build list
/// below matches an ant whose flag cache has not delivered the override.
fn get_ant_model_options() -> Vec<ModelOption> {
    Vec::new()
}

// @[MODEL LAUNCH]: Update the model picker lists below to include/reorder
// options for the new model. Each user tier (ant, Max/Team Premium, Pro/Team
// Standard/Enterprise, PAYG 1P, PAYG 3P) has its own list.
/// Maps to: CC `utils/model/modelOptions.ts:271-376` `getModelOptionsBase`.
fn get_model_options_base(
    audience: crate::utils::build_profile::BuildAudience,
    fast_mode: bool,
) -> Vec<ModelOption> {
    if crate::utils::build_profile::audience_has_internal_capability(
        audience,
        crate::utils::build_profile::InternalCapability::Models,
    ) {
        let mut options = vec![get_default_option_for_user_for_audience(audience, false)];
        options.extend(get_ant_model_options());
        options.push(get_merged_opus_1m_option(fast_mode));
        options.push(get_sonnet_46_option());
        options.push(get_sonnet_46_1m_option());
        options.push(get_haiku_45_option());
        return options;
    }

    if is_claude_ai_subscriber() {
        if is_max_subscriber() || is_team_premium_subscriber() {
            // Max and Team Premium: Opus is the default, Sonnet the alternative.
            let mut premium_options = vec![get_default_option_for_user_for_audience(
                audience, fast_mode,
            )];
            if !is_opus_1m_merge_enabled() && check_opus_1m_access() {
                premium_options.push(get_max_opus_46_1m_option(fast_mode));
            }

            premium_options.push(max_sonnet_46_option());
            if check_sonnet_1m_access() {
                premium_options.push(get_max_sonnet_46_1m_option());
            }

            premium_options.push(max_haiku_45_option());
            return premium_options;
        }

        // Pro/Team Standard/Enterprise: Sonnet is the default, Opus the alternative.
        let mut standard_options = vec![get_default_option_for_user_for_audience(
            audience, fast_mode,
        )];
        if check_sonnet_1m_access() {
            standard_options.push(get_max_sonnet_46_1m_option());
        }

        if is_opus_1m_merge_enabled() {
            standard_options.push(get_merged_opus_1m_option(fast_mode));
        } else {
            standard_options.push(get_max_opus_option(fast_mode));
            if check_opus_1m_access() {
                standard_options.push(get_max_opus_46_1m_option(fast_mode));
            }
        }

        standard_options.push(max_haiku_45_option());
        return standard_options;
    }

    // PAYG 1P API: Default (Sonnet) + Sonnet 1M + Opus 4.6 + Opus 1M + Haiku.
    if get_api_provider() == ApiProvider::FirstParty {
        let mut payg_1p_options = vec![get_default_option_for_user_for_audience(
            audience, fast_mode,
        )];
        if check_sonnet_1m_access() {
            payg_1p_options.push(get_sonnet_46_1m_option());
        }
        if is_opus_1m_merge_enabled() {
            payg_1p_options.push(get_merged_opus_1m_option(fast_mode));
        } else {
            payg_1p_options.push(get_opus_46_option(fast_mode));
            if check_opus_1m_access() {
                payg_1p_options.push(get_opus_46_1m_option(fast_mode));
            }
        }
        payg_1p_options.push(get_haiku_45_option());
        return payg_1p_options;
    }

    // PAYG 3P: Default (Sonnet 4.5) + Sonnet (3P custom) or Sonnet 4.6/1M +
    // Opus (3P custom) or Opus 4.1/Opus 4.6/Opus 1M + Haiku.
    let mut payg_3p_options = vec![get_default_option_for_user_for_audience(
        audience, fast_mode,
    )];

    match get_custom_sonnet_option() {
        Some(custom_sonnet) => payg_3p_options.push(custom_sonnet),
        None => {
            // Sonnet 4.5 is the 3P default, so surface 4.6 explicitly.
            payg_3p_options.push(get_sonnet_46_option());
            if check_sonnet_1m_access() {
                payg_3p_options.push(get_sonnet_46_1m_option());
            }
        }
    }

    match get_custom_opus_option() {
        Some(custom_opus) => payg_3p_options.push(custom_opus),
        None => {
            payg_3p_options.push(get_opus_41_option()); // The default opus.
            payg_3p_options.push(get_opus_46_option(fast_mode));
            if check_opus_1m_access() {
                payg_3p_options.push(get_opus_46_1m_option(fast_mode));
            }
        }
    }

    match get_custom_haiku_option() {
        Some(custom_haiku) => payg_3p_options.push(custom_haiku),
        None => payg_3p_options.push(get_haiku_option()),
    }
    payg_3p_options
}

// @[MODEL LAUNCH]: Add the new model ID to the appropriate family pattern below
// so the "newer version available" hint works correctly.
/// Maps to: CC `utils/model/modelOptions.ts:385-424` `getModelFamilyInfo` —
/// maps a full model name to its family alias and the marketing name the alias
/// currently resolves to, so a pinned older version can be flagged.
fn get_model_family_info(model: &str) -> Option<(&'static str, String)> {
    let canonical = get_canonical_name(model);

    if canonical.contains("claude-sonnet-4-6")
        || canonical.contains("claude-sonnet-4-5")
        || canonical.contains("claude-sonnet-4-")
        || canonical.contains("claude-3-7-sonnet")
        || canonical.contains("claude-3-5-sonnet")
    {
        if let Some(current_name) = get_marketing_name_for_model(&get_default_sonnet_model()) {
            return Some(("Sonnet", current_name));
        }
    }

    if canonical.contains("claude-opus-4") {
        if let Some(current_name) = get_marketing_name_for_model(&get_default_opus_model()) {
            return Some(("Opus", current_name));
        }
    }

    if canonical.contains("claude-haiku") || canonical.contains("claude-3-5-haiku") {
        if let Some(current_name) = get_marketing_name_for_model(&get_default_haiku_model()) {
            return Some(("Haiku", current_name));
        }
    }

    None
}

/// Maps to: CC `utils/model/modelOptions.ts:431-459` `getKnownModelOption`.
fn get_known_model_option(model: &str) -> Option<ModelOption> {
    let marketing_name = get_marketing_name_for_model(model)?;

    let Some((alias, current_version_name)) = get_model_family_info(model) else {
        return Some(ModelOption::new(
            Some(model),
            &marketing_name,
            model.to_string(),
        ));
    };

    if marketing_name != current_version_name {
        return Some(ModelOption::new(
            Some(model),
            &marketing_name,
            format!("Newer version available · select {alias} for {current_version_name}"),
        ));
    }

    Some(ModelOption::new(
        Some(model),
        &marketing_name,
        model.to_string(),
    ))
}

/// Maps to: CC `utils/model/modelOptions.ts:461-525` `getModelOptions`.
pub fn get_model_options(fast_mode: bool) -> Vec<ModelOption> {
    get_model_options_for_audience(crate::utils::build_profile::build_audience(), fast_mode)
}

/// The audience-parameterized body of [`get_model_options`]; see
/// [`get_default_option_for_user_for_audience`] for why the split exists.
pub fn get_model_options_for_audience(
    audience: crate::utils::build_profile::BuildAudience,
    fast_mode: bool,
) -> Vec<ModelOption> {
    let mut options = get_model_options_base(audience, fast_mode);

    // The custom model from the ANTHROPIC_CUSTOM_MODEL_OPTION env var.
    if let Some(env_custom_model) =
        crate::utils::process_env::var("ANTHROPIC_CUSTOM_MODEL_OPTION").truthy()
    {
        if !options
            .iter()
            .any(|existing| existing.value.as_deref() == Some(env_custom_model.as_str()))
        {
            options.push(ModelOption::new(
                Some(&env_custom_model),
                &crate::utils::process_env::var("ANTHROPIC_CUSTOM_MODEL_OPTION_NAME")
                    .unwrap_or_else(|| env_custom_model.clone()),
                crate::utils::process_env::var("ANTHROPIC_CUSTOM_MODEL_OPTION_DESCRIPTION")
                    .unwrap_or_else(|| format!("Custom model ({env_custom_model})")),
            ));
        }
    }

    // Additional model options fetched during bootstrap.
    for option in crate::utils::config::load_global_config()
        .additional_model_options_cache
        .unwrap_or_default()
    {
        if !options
            .iter()
            .any(|existing| existing.value == option.value)
        {
            options.push(option);
        }
    }

    // Add the custom model from either the current model value or the initial
    // one if it is not already in the options.
    let custom_model = get_user_specified_model_setting().or_else(get_initial_main_loop_model);
    let Some(custom_model) = custom_model else {
        return filter_model_options_by_allowlist(options);
    };
    if options
        .iter()
        .any(|option| option.value.as_deref() == Some(custom_model.as_str()))
    {
        return filter_model_options_by_allowlist(options);
    }

    if custom_model == "opusplan" {
        options.push(get_opus_plan_option());
    } else if custom_model == "opus" && get_api_provider() == ApiProvider::FirstParty {
        options.push(get_max_opus_option(fast_mode));
    } else if custom_model == "opus[1m]" && get_api_provider() == ApiProvider::FirstParty {
        options.push(get_merged_opus_1m_option(fast_mode));
    } else if let Some(known_option) = get_known_model_option(&custom_model) {
        // Show a human-readable label for known Anthropic models, with an
        // upgrade hint if the alias now resolves to a newer version.
        options.push(known_option);
    } else {
        options.push(ModelOption::new(
            Some(&custom_model),
            &custom_model,
            "Custom model".to_string(),
        ));
    }
    filter_model_options_by_allowlist(options)
}

/// Maps to: CC `utils/model/modelOptions.ts:531-540`
/// `filterModelOptionsByAllowlist` — always preserves the default option.
fn filter_model_options_by_allowlist(options: Vec<ModelOption>) -> Vec<ModelOption> {
    if crate::utils::settings::get_initial_settings()
        .available_models
        .is_none()
    {
        return options;
    }
    options
        .into_iter()
        .filter(|option| match &option.value {
            None => true,
            Some(value) => is_model_allowed(value),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bootstrap::state::set_initial_main_loop_model;
    use crate::utils::build_profile::BuildAudience;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    /// Drives `getModelOptions()` from a clean, credential-free process state:
    /// no OAuth (so not a subscriber), first-party provider, no settings file.
    struct OptionsFixture {
        root: std::path::PathBuf,
        config_dir: Option<EnvVarGuard>,
        cleared_env: Vec<EnvVarGuard>,
    }

    const CLEARED_ENV: &[&str] = &[
        "ANTHROPIC_MODEL",
        "ANTHROPIC_DEFAULT_SONNET_MODEL",
        "ANTHROPIC_DEFAULT_OPUS_MODEL",
        "ANTHROPIC_DEFAULT_HAIKU_MODEL",
        "ANTHROPIC_DEFAULT_SONNET_MODEL_NAME",
        "ANTHROPIC_DEFAULT_SONNET_MODEL_DESCRIPTION",
        "ANTHROPIC_DEFAULT_OPUS_MODEL_NAME",
        "ANTHROPIC_DEFAULT_OPUS_MODEL_DESCRIPTION",
        "ANTHROPIC_DEFAULT_HAIKU_MODEL_NAME",
        "ANTHROPIC_DEFAULT_HAIKU_MODEL_DESCRIPTION",
        "ANTHROPIC_CUSTOM_MODEL_OPTION",
        "ANTHROPIC_CUSTOM_MODEL_OPTION_NAME",
        "ANTHROPIC_CUSTOM_MODEL_OPTION_DESCRIPTION",
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_VERTEX",
        "CLAUDE_CODE_USE_FOUNDRY",
        "CLAUDE_CODE_DISABLE_1M_CONTEXT",
        "CLAUDE_CODE_DISABLE_FAST_MODE",
    ];

    impl OptionsFixture {
        fn new(settings: serde_json::Value) -> Self {
            let root = std::env::temp_dir().join(format!(
                "cometix-model-options-{}",
                uuid::Uuid::new_v4().simple()
            ));
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(root.join("settings.json"), settings.to_string()).unwrap();
            let config_dir = Some(EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root));

            let cleared_env = CLEARED_ENV
                .iter()
                .map(|name| EnvVarGuard::unset(*name))
                .collect();

            crate::utils::settings::settings_cache::reset_settings_cache();
            crate::utils::config::set_test_global_config(Some(Default::default()));
            crate::bootstrap::state::set_main_loop_model_override(None);
            set_initial_main_loop_model(None);

            Self {
                root,
                config_dir,
                cleared_env,
            }
        }
    }

    impl Drop for OptionsFixture {
        fn drop(&mut self) {
            self.cleared_env.clear();
            drop(self.config_dir.take());
            crate::utils::settings::settings_cache::reset_settings_cache();
            crate::utils::config::set_test_global_config(None);
            crate::bootstrap::state::set_main_loop_model_override(None);
            set_initial_main_loop_model(None);
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn values(options: &[ModelOption]) -> Vec<Option<&str>> {
        options
            .iter()
            .map(|option| option.value.as_deref())
            .collect()
    }

    /// Maps to: CC `utils/model/modelOptions.ts:327-342` — the PAYG 1P list.
    /// No credentials means no subscription, so this is the branch a plain
    /// `ANTHROPIC_API_KEY` user lands on.
    #[test]
    fn payg_first_party_list_matches_the_official_order_and_copy() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _fixture = OptionsFixture::new(serde_json::json!({}));

        let options = get_model_options_for_audience(BuildAudience::External, false);
        assert_eq!(
            values(&options),
            [None, Some("sonnet[1m]"), Some("opus[1m]"), Some("haiku"),]
        );
        assert_eq!(
            options[0].description,
            "Use the default model (currently Sonnet 4.6) · $3/$15 per Mtok"
        );
        assert_eq!(
            options[1].description,
            "Sonnet 4.6 for long sessions · $3/$15 per Mtok"
        );
        assert_eq!(
            options[1].description_for_model.as_deref(),
            Some("Sonnet 4.6 with 1M context window - for long sessions with large codebases")
        );
        assert_eq!(
            options[2].description,
            "Opus 4.6 with 1M context · Most capable for complex work"
        );
        assert_eq!(
            options[3].description,
            "Haiku 4.5 · Fastest for quick answers · $1/$5 per Mtok"
        );
    }

    /// Maps to: CC `utils/model/modelOptions.ts:332-339` — with the merge off,
    /// Opus splits into the base option plus a separate 1M entry.
    #[test]
    fn disabling_1m_context_collapses_the_official_opus_merge_branch() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _fixture = OptionsFixture::new(serde_json::json!({}));
        crate::utils::process_env::set("CLAUDE_CODE_DISABLE_1M_CONTEXT", "1");

        let options = get_model_options_for_audience(BuildAudience::External, false);
        assert_eq!(values(&options), [None, Some("opus"), Some("haiku")]);
        assert_eq!(
            options[1].description,
            "Opus 4.6 · Most capable for complex work · $5/$25 per Mtok"
        );

        crate::utils::process_env::remove("CLAUDE_CODE_DISABLE_1M_CONTEXT");
    }

    /// Maps to: CC `utils/model/modelOptions.ts:344-375` — the 3P list, whose
    /// option values are provider-specific model IDs rather than aliases.
    #[test]
    fn third_party_list_uses_provider_model_ids_and_drops_first_party_pricing() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _fixture = OptionsFixture::new(serde_json::json!({}));
        crate::utils::process_env::set("CLAUDE_CODE_USE_BEDROCK", "1");

        let options = get_model_options_for_audience(BuildAudience::External, false);
        assert_eq!(
            values(&options),
            [
                None,
                Some("us.anthropic.claude-sonnet-4-6"),
                Some("us.anthropic.claude-sonnet-4-6[1m]"),
                Some("opus"),
                Some("us.anthropic.claude-opus-4-6-v1"),
                Some("us.anthropic.claude-opus-4-6-v1[1m]"),
                Some("haiku"),
            ]
        );
        // 3P suppresses the first-party pricing suffix everywhere.
        assert_eq!(
            options[1].description,
            "Sonnet 4.6 · Best for everyday tasks"
        );
        assert_eq!(options[3].description, "Opus 4.1 · Legacy");
        assert_eq!(
            options[4].description,
            "Opus 4.6 · Most capable for complex work"
        );

        crate::utils::process_env::remove("CLAUDE_CODE_USE_BEDROCK");
    }

    /// Maps to: CC `utils/model/modelOptions.ts:76-92, 107-122, 165-179` — a 3P
    /// user with custom model strings sees those instead of the built-ins.
    #[test]
    fn third_party_custom_model_env_replaces_the_official_family_entries() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _fixture = OptionsFixture::new(serde_json::json!({}));
        crate::utils::process_env::set("CLAUDE_CODE_USE_VERTEX", "1");
        crate::utils::process_env::set("ANTHROPIC_DEFAULT_SONNET_MODEL", "my-sonnet[1m]");
        crate::utils::process_env::set("ANTHROPIC_DEFAULT_OPUS_MODEL", "my-opus");
        crate::utils::process_env::set("ANTHROPIC_DEFAULT_HAIKU_MODEL", "my-haiku");
        crate::utils::process_env::set("ANTHROPIC_DEFAULT_HAIKU_MODEL_NAME", "House Haiku");

        let options = get_model_options_for_audience(BuildAudience::External, false);
        assert_eq!(
            values(&options),
            [None, Some("sonnet"), Some("opus"), Some("haiku")]
        );
        assert_eq!(options[1].label, "my-sonnet[1m]");
        assert_eq!(options[1].description, "Custom Sonnet model (1M context)");
        assert_eq!(
            options[1].description_for_model.as_deref(),
            Some("Custom Sonnet model with 1M context (my-sonnet[1m])")
        );
        assert_eq!(options[2].description, "Custom Opus model");
        assert_eq!(options[3].label, "House Haiku");
        assert_eq!(
            options[3].description_for_model.as_deref(),
            Some("Custom Haiku model (my-haiku)")
        );

        for name in [
            "CLAUDE_CODE_USE_VERTEX",
            "ANTHROPIC_DEFAULT_SONNET_MODEL",
            "ANTHROPIC_DEFAULT_OPUS_MODEL",
            "ANTHROPIC_DEFAULT_HAIKU_MODEL",
            "ANTHROPIC_DEFAULT_HAIKU_MODEL_NAME",
        ] {
            crate::utils::process_env::remove(name);
        }
    }

    /// Maps to: CC `utils/model/modelOptions.ts:464-477` — the env custom model
    /// is appended once, keyed by value.
    #[test]
    fn env_custom_model_option_is_appended_with_the_official_fallback_copy() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _fixture = OptionsFixture::new(serde_json::json!({}));
        crate::utils::process_env::set("ANTHROPIC_CUSTOM_MODEL_OPTION", "internal-eval-1");

        let options = get_model_options_for_audience(BuildAudience::External, false);
        let custom = options.last().expect("custom option");
        assert_eq!(custom.value.as_deref(), Some("internal-eval-1"));
        assert_eq!(custom.label, "internal-eval-1");
        assert_eq!(custom.description, "Custom model (internal-eval-1)");

        crate::utils::process_env::remove("ANTHROPIC_CUSTOM_MODEL_OPTION");
    }

    /// Maps to: CC `services/api/bootstrap.ts:22-36` — the API's
    /// `{model,name,description}` is transformed to `{value,label,description}`
    /// before it is persisted, and `descriptionForModel` never comes from the
    /// wire. A partial entry must degrade rather than fail the config parse.
    #[test]
    fn cached_option_json_matches_the_official_persisted_shape() {
        let option = ModelOption::new(Some("m"), "Model", "Desc".to_string());
        assert_eq!(
            serde_json::to_value(&option).unwrap(),
            serde_json::json!({ "value": "m", "label": "Model", "description": "Desc" })
        );
        assert_eq!(
            serde_json::from_value::<ModelOption>(
                serde_json::json!({ "value": "m", "label": "Model", "description": "Desc" })
            )
            .unwrap(),
            option
        );

        let config: crate::utils::config::GlobalConfig =
            serde_json::from_str(r#"{"additionalModelOptionsCache":[{"label":"Only a label"}]}"#)
                .expect("a partial cache entry must not fail the whole config parse");
        let cached = config.additional_model_options_cache.expect("cache");
        assert_eq!(cached[0].label, "Only a label");
        assert_eq!(cached[0].value, None);
        assert_eq!(cached[0].description, "");
    }

    /// Maps to: CC `utils/model/modelOptions.ts:479-484` — bootstrap-cached
    /// options are appended, deduped by value.
    #[test]
    fn bootstrap_cached_options_are_appended_and_deduped_by_value() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _fixture = OptionsFixture::new(serde_json::json!({}));
        crate::utils::config::set_test_global_config(Some(crate::utils::config::GlobalConfig {
            additional_model_options_cache: Some(vec![
                ModelOption::new(
                    Some("server-model"),
                    "Server Model",
                    "From bootstrap".to_string(),
                ),
                ModelOption::new(Some("haiku"), "Shadowed", "Should be skipped".to_string()),
            ]),
            ..Default::default()
        }));

        let options = get_model_options_for_audience(BuildAudience::External, false);
        assert_eq!(
            values(&options),
            [
                None,
                Some("sonnet[1m]"),
                Some("opus[1m]"),
                Some("haiku"),
                Some("server-model"),
            ]
        );
        assert_eq!(options[3].label, "Haiku");
        assert_eq!(options[4].description, "From bootstrap");
    }

    /// Maps to: CC `utils/model/modelOptions.ts:486-524` — a pinned model that
    /// is not already in the list gets appended, with `opusplan` and the known
    /// Anthropic IDs taking their own branches.
    #[test]
    fn pinned_models_take_the_official_append_branches() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _fixture = OptionsFixture::new(serde_json::json!({}));

        crate::utils::process_env::set("ANTHROPIC_MODEL", "opusplan");
        let options = get_model_options_for_audience(BuildAudience::External, false);
        let last = options.last().expect("opusplan option");
        assert_eq!(last.value.as_deref(), Some("opusplan"));
        assert_eq!(last.label, "Opus Plan Mode");
        assert_eq!(
            last.description,
            "Use Opus 4.6 in plan mode, Sonnet 4.6 otherwise"
        );

        crate::utils::process_env::set("ANTHROPIC_MODEL", "opus");
        let options = get_model_options_for_audience(BuildAudience::External, false);
        let last = options.last().expect("opus option");
        assert_eq!(last.value.as_deref(), Some("opus"));
        assert_eq!(last.description, "Opus 4.6 · Most capable for complex work");

        // A pinned older version is labelled and flagged against the alias.
        crate::utils::process_env::set("ANTHROPIC_MODEL", "claude-opus-4-1-20250805");
        let options = get_model_options_for_audience(BuildAudience::External, false);
        let last = options.last().expect("known model option");
        assert_eq!(last.label, "Opus 4.1");
        assert_eq!(
            last.description,
            "Newer version available · select Opus for Opus 4.6"
        );

        crate::utils::process_env::set("ANTHROPIC_MODEL", "some-internal-deployment");
        let options = get_model_options_for_audience(BuildAudience::External, false);
        let last = options.last().expect("custom model option");
        assert_eq!(last.value.as_deref(), Some("some-internal-deployment"));
        assert_eq!(last.label, "some-internal-deployment");
        assert_eq!(last.description, "Custom model");

        crate::utils::process_env::remove("ANTHROPIC_MODEL");
    }

    /// Maps to: CC `utils/model/modelOptions.ts:486-495` — the startup model is
    /// the fallback source once the live setting is cleared.
    #[test]
    fn initial_main_loop_model_backs_the_pinned_model_lookup() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _fixture = OptionsFixture::new(serde_json::json!({}));

        set_initial_main_loop_model(Some("claude-sonnet-4-5-20250929".to_string()));
        let options = get_model_options_for_audience(BuildAudience::External, false);
        let last = options.last().expect("initial model option");
        assert_eq!(last.value.as_deref(), Some("claude-sonnet-4-5-20250929"));
        assert_eq!(last.label, "Sonnet 4.5");
        assert_eq!(
            last.description,
            "Newer version available · select Sonnet for Sonnet 4.6"
        );
    }

    /// Maps to: CC `utils/model/modelOptions.ts:531-540` — the allowlist filters
    /// every entry except the default.
    #[test]
    fn allowlist_filters_options_but_always_keeps_the_default() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        {
            let _fixture = OptionsFixture::new(serde_json::json!({ "availableModels": ["haiku"] }));
            assert_eq!(
                values(&get_model_options_for_audience(
                    BuildAudience::External,
                    false
                )),
                [None, Some("haiku")]
            );
        }
        {
            // An empty allowlist blocks every user-specified model.
            let _fixture = OptionsFixture::new(serde_json::json!({ "availableModels": [] }));
            assert_eq!(
                values(&get_model_options_for_audience(
                    BuildAudience::External,
                    false
                )),
                [None]
            );
        }
    }

    /// Maps to: CC `utils/model/modelOptions.ts:272-288` — the ant list, which
    /// CC gates on `USER_TYPE === 'ant'` and this port gates on the
    /// `anthropic_internal` build feature. `getAntModels()` is unported, so the
    /// flag-driven entries between the default and the merged Opus option are
    /// absent (see [`get_ant_model_options`]); everything else is pinned.
    ///
    /// The default entry reads `Opus 4.6 1M` where CC renders
    /// `Opus 4.6 (1M context)`. That gap predates this list: `model.rs`'s
    /// `get_public_model_display_name` matches by substring instead of CC's
    /// exact `getModelStrings()` switch (`model.ts:349-384`), and it is shared
    /// with commit trailers and the components, so it is not retargeted here.
    /// External builds are unaffected — their default entries come from
    /// `getClaudeAiUserDefaultModelDescription` or a suffix-free Sonnet ID.
    #[test]
    fn internal_build_list_matches_the_official_ant_order_minus_flag_models() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _fixture = OptionsFixture::new(serde_json::json!({}));

        let options = get_model_options_for_audience(BuildAudience::AnthropicInternal, false);
        assert_eq!(
            values(&options),
            [
                None,
                Some("opus[1m]"),
                Some("sonnet"),
                Some("sonnet[1m]"),
                Some("haiku"),
            ]
        );
        assert_eq!(
            options[0].description,
            "Use the default model for Ants (currently Opus 4.6 1M)"
        );
        assert_eq!(
            options[0].description_for_model.as_deref(),
            Some("Default model (currently Opus 4.6 1M)")
        );
        assert_eq!(
            options[1].description,
            "Opus 4.6 with 1M context · Most capable for complex work"
        );
    }

    /// Maps to: CC `utils/model/modelOptions.ts:154-163, 229-236` and
    /// `model.ts:307-312` — fast mode swaps in the 30/150 tier and prefixes the
    /// lightning bolt.
    #[test]
    fn fast_mode_applies_the_official_opus_pricing_suffix() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _fixture = OptionsFixture::new(serde_json::json!({}));
        crate::utils::process_env::set("CLAUDE_CODE_DISABLE_1M_CONTEXT", "1");

        let options = get_model_options_for_audience(BuildAudience::External, true);
        assert_eq!(
            options[1].description,
            format!(
                "Opus 4.6 · Most capable for complex work · ({}) $30/$150 per Mtok",
                crate::constants::figures::LIGHTNING_BOLT
            )
        );

        crate::utils::process_env::remove("CLAUDE_CODE_DISABLE_1M_CONTEXT");
    }
}
