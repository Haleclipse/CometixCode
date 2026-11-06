//! Agent model and effort selection helpers.
//!
//! Per design §1 and §2:
//! - Subagent model resolution chain:
//!   tool_call parameter -> agent md -> selection.subagents.<type> -> selection.subagent -> inherit (parent model).
//! - Supported format: `ref[:effort]` where ref is profile name | @light | @heavy | @main | inherit | temporary provider/model.
//! - Default by agent type: general-purpose, plan = inherit; explore = @light; review = @heavy.
//! - Effort follows the layer that decides the model:
//!   - That layer has `:effort` -> use it.
//!   - Otherwise `inherit` -> parent session effort.
//!   - Otherwise profile/@tier -> that profile's configured effort.
//!   - Otherwise temporary provider/model -> none.
//!   No effort anywhere means the model default: callers keep their own thinking config.
//! - `inherit` follows the whole parent selection: model, effort and profile.
//! - CC legacy aliases (haiku/sonnet/opus, alias_matches_parent_tier, Bedrock prefixing) are removed.

use crate::utils::effort::Effort;
pub use crate::utils::effort::parse_model_ref_and_effort;
use crate::utils::model::profile::{resolve_profile, resolve_selection_profile_name};
use crate::utils::settings::nyan::NyanSettings;

/// What a request made from a session inherits: the session's current model,
/// effort and profile. `profile` is `None` while the session runs on the
/// configured main profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParentSelection {
    pub model: String,
    pub effort: Option<Effort>,
    pub profile: Option<String>,
}

impl ParentSelection {
    /// The configured selection, for callers with no session to inherit from.
    pub fn configured() -> Self {
        Self {
            model: crate::utils::model::model::get_main_loop_model(),
            effort: None,
            profile: None,
        }
    }
}

/// Resolved model, effort and profile for a subagent or utility task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAgentModel {
    pub model: String,
    /// `None` is the model default: the caller's own thinking config applies.
    pub effort: Option<Effort>,
    /// The profile the request is routed through; `None` follows the session's.
    pub profile: Option<String>,
}

/// Default model reference used by displays that do not have an agent type.
pub fn get_default_subagent_model() -> &'static str {
    "inherit"
}

/// Default subagent spec string by agent type name.
pub fn default_subagent_spec(agent_type: &str) -> &'static str {
    match agent_type.trim().to_ascii_lowercase().as_str() {
        "explore" => "@light",
        "review" => "@heavy",
        "general-purpose" | "plan" | _ => "inherit",
    }
}

/// Default utility spec string by role name.
pub fn default_utility_spec(role: &str) -> &'static str {
    let lower = role.trim().to_ascii_lowercase();
    let norm = lower.replace('-', "_");
    match norm.as_str() {
        "compaction" | "classifier" | "permission_classifier" | "permissions" => "@main",
        _ => "@light",
    }
}

/// Resolves a ref (`@light`, `@heavy`, `@main`, `inherit`, profile name, or a
/// temporary `provider/model`) into the wire model ID, the effort of the layer
/// that decided it, and the profile to route through.
fn resolve_spec_to_agent_model(
    r_ref: &str,
    explicit_effort: Option<Effort>,
    parent: &ParentSelection,
    nyan: Option<&NyanSettings>,
) -> ResolvedAgentModel {
    let inherited = || ResolvedAgentModel {
        model: parent.model.clone(),
        effort: explicit_effort.or(parent.effort),
        profile: parent.profile.clone(),
    };
    if r_ref == "inherit" {
        return inherited();
    }

    // A `@tier` never errors and never reaches the wire as a model id: when it
    // names no usable profile (no `selection`, no `main`, an unknown tier), it
    // follows the session like `inherit` (design §1).
    let is_tier = r_ref.starts_with('@');
    let selection = nyan.and_then(|n| n.selection.as_ref());
    let profile_name = match (is_tier, selection) {
        (false, _) => r_ref,
        (true, None) => return inherited(),
        (true, Some(selection)) => match resolve_selection_profile_name(selection, r_ref) {
            Ok(name) => name,
            Err(_) => return inherited(),
        },
    };

    if let Some(nyan) = nyan
        && let Ok(resolved) = resolve_profile(nyan, profile_name)
    {
        return ResolvedAgentModel {
            model: resolved.model,
            effort: explicit_effort.or(resolved.effort),
            profile: Some(profile_name.to_string()),
        };
    }
    if is_tier {
        return inherited();
    }

    // A temporary `provider/model` (or bare model id): the model default.
    ResolvedAgentModel {
        model: profile_name.to_string(),
        effort: explicit_effort,
        profile: None,
    }
}

/// Resolves a subagent's model and effort along the priority chain:
/// 1. tool_specified_model (`RunAgentInput.model_override`)
/// 2. agent_md_model (`AgentDefinition.model`)
/// 3. `selection.subagents.<agent_type>`
/// 4. `selection.subagent`
/// 5. default for agent type (explore=@light, review=@heavy, general-purpose/plan=inherit)
pub fn resolve_subagent_model_and_effort(
    tool_specified_model: Option<&str>,
    agent_md_model: Option<&str>,
    agent_type: &str,
    parent: &ParentSelection,
    nyan: Option<&NyanSettings>,
) -> ResolvedAgentModel {
    let selection = nyan.and_then(|n| n.selection.as_ref());

    // 1. Tool-specified model parameter
    if let Some(spec) = tool_specified_model.filter(|s| !s.trim().is_empty()) {
        let (r_ref, effort) = parse_model_ref_and_effort(spec);
        return resolve_spec_to_agent_model(r_ref, effort, parent, nyan);
    }

    // 2. Agent definition frontmatter model:
    if let Some(spec) = agent_md_model.filter(|s| !s.trim().is_empty()) {
        let (r_ref, effort) = parse_model_ref_and_effort(spec);
        return resolve_spec_to_agent_model(r_ref, effort, parent, nyan);
    }

    // 3. selection.subagents.<agent_type>
    if let Some(spec) = selection
        .and_then(|s| s.subagents.as_ref())
        .and_then(|map| map.get(agent_type))
        .filter(|s| !s.trim().is_empty())
    {
        let (r_ref, effort) = parse_model_ref_and_effort(spec);
        return resolve_spec_to_agent_model(r_ref, effort, parent, nyan);
    }

    // 4. selection.subagent
    if let Some(spec) = selection
        .and_then(|s| s.subagent.as_deref())
        .filter(|s| !s.trim().is_empty())
    {
        let (r_ref, effort) = parse_model_ref_and_effort(spec);
        return resolve_spec_to_agent_model(r_ref, effort, parent, nyan);
    }

    // 5. Default by agent type
    let spec = default_subagent_spec(agent_type);
    let (r_ref, effort) = parse_model_ref_and_effort(spec);
    resolve_spec_to_agent_model(r_ref, effort, parent, nyan)
}

/// Resolves a utility role's model and effort along the priority chain:
/// 1. `selection.utility.<role>`
/// 2. default for utility role (compaction/classifier=@main, all others=@light)
pub fn resolve_utility_model_and_effort(
    role: &str,
    parent: &ParentSelection,
    nyan: Option<&NyanSettings>,
) -> ResolvedAgentModel {
    let selection = nyan.and_then(|n| n.selection.as_ref());

    // Try finding the role in selection.utility with loose name matching
    let configured_spec = selection.and_then(|s| s.utility.as_ref()).and_then(|map| {
        let normalized = role.trim().to_ascii_lowercase().replace('-', "_");
        map.iter().find_map(|(k, v)| {
            let k_norm = k.trim().to_ascii_lowercase().replace('-', "_");
            if k_norm == normalized {
                Some(v.as_str())
            } else {
                None
            }
        })
    });

    let spec = configured_spec.unwrap_or_else(|| default_utility_spec(role));
    let (r_ref, effort) = parse_model_ref_and_effort(spec);
    resolve_spec_to_agent_model(r_ref, effort, parent, nyan)
}

pub fn get_agent_model_display(model: Option<&str>) -> String {
    match model {
        None | Some("inherit") => "Inherit from parent (default)".to_string(),
        Some(value) => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::settings::nyan::{
        ModelCapabilities, ModelProfileSettings, ProviderKind, ProviderProtocol, ProviderSettings,
        SelectionSettings,
    };
    use std::collections::BTreeMap;

    fn test_nyan_settings() -> NyanSettings {
        let mut providers = BTreeMap::new();
        providers.insert(
            "anthropic".to_string(),
            ProviderSettings {
                kind: Some(ProviderKind::Anthropic),
                protocol: Some(ProviderProtocol::AnthropicMessages),
                ..Default::default()
            },
        );
        let mut models = BTreeMap::new();
        models.insert(
            "main-profile".to_string(),
            ModelProfileSettings {
                provider: Some("anthropic".to_string()),
                model: Some("claude-sonnet-4-6".to_string()),
                protocol: Some(ProviderProtocol::AnthropicMessages),
                capabilities: Some(ModelCapabilities {
                    context_window: Some(200_000),
                    max_output_tokens: Some(8192),
                    thinking: Some("adaptive".to_string()),
                    ..Default::default()
                }),
                effort: Some(Effort::High),
                ..Default::default()
            },
        );
        models.insert(
            "fast-profile".to_string(),
            ModelProfileSettings {
                provider: Some("anthropic".to_string()),
                model: Some("claude-haiku-4-5".to_string()),
                protocol: Some(ProviderProtocol::AnthropicMessages),
                capabilities: Some(ModelCapabilities {
                    context_window: Some(200_000),
                    max_output_tokens: Some(4096),
                    thinking: Some("adaptive".to_string()),
                    ..Default::default()
                }),
                effort: Some(Effort::Off),
                ..Default::default()
            },
        );
        models.insert(
            "deep-profile".to_string(),
            ModelProfileSettings {
                provider: Some("anthropic".to_string()),
                model: Some("claude-opus-4-6".to_string()),
                protocol: Some(ProviderProtocol::AnthropicMessages),
                capabilities: Some(ModelCapabilities {
                    context_window: Some(200_000),
                    max_output_tokens: Some(16384),
                    thinking: Some("adaptive".to_string()),
                    ..Default::default()
                }),
                effort: Some(Effort::Max),
                ..Default::default()
            },
        );

        let selection = SelectionSettings {
            main: Some("main-profile".to_string()),
            light: Some("fast-profile".to_string()),
            heavy: Some("deep-profile".to_string()),
            subagents: Some(BTreeMap::from([(
                "review".to_string(),
                "deep-profile:high".to_string(),
            )])),
            utility: Some(BTreeMap::from([(
                "title".to_string(),
                "fast-profile:off".to_string(),
            )])),
            ..Default::default()
        };

        NyanSettings {
            providers: Some(providers),
            models: Some(models),
            selection: Some(selection),
            ..Default::default()
        }
    }

    #[test]
    fn subagent_defaults_by_type() {
        assert_eq!(default_subagent_spec("general-purpose"), "inherit");
        assert_eq!(default_subagent_spec("plan"), "inherit");
        assert_eq!(default_subagent_spec("explore"), "@light");
        assert_eq!(default_subagent_spec("review"), "@heavy");
        assert_eq!(default_subagent_spec("other"), "inherit");
    }

    #[test]
    fn utility_defaults_by_role() {
        assert_eq!(default_utility_spec("compaction"), "@main");
        assert_eq!(default_utility_spec("classifier"), "@main");
        assert_eq!(default_utility_spec("permission_classifier"), "@main");
        assert_eq!(default_utility_spec("title"), "@light");
        assert_eq!(default_utility_spec("summary"), "@light");
        assert_eq!(default_utility_spec("commit_message"), "@light");
        assert_eq!(default_utility_spec("webfetch"), "@light");
        assert_eq!(default_utility_spec("bash_prefix"), "@light");
        assert_eq!(default_utility_spec("prompt_suggestions"), "@light");
        assert_eq!(default_utility_spec("memory"), "@light");
        assert_eq!(default_utility_spec("side_query"), "@light");
    }

    const PARENT: &str = "claude-sonnet-4-6";

    fn parent(effort: Option<Effort>) -> ParentSelection {
        ParentSelection {
            model: PARENT.to_string(),
            effort,
            profile: None,
        }
    }

    fn subagent(
        tool: Option<&str>,
        md: Option<&str>,
        agent_type: &str,
        parent_effort: Option<Effort>,
        nyan: &NyanSettings,
    ) -> ResolvedAgentModel {
        resolve_subagent_model_and_effort(tool, md, agent_type, &parent(parent_effort), Some(nyan))
    }

    #[test]
    fn the_call_parameter_beats_agent_md_and_settings() {
        let nyan = test_nyan_settings();
        let r = subagent(
            Some("fast-profile:max"),
            Some("deep-profile"),
            "review",
            Some(Effort::Medium),
            &nyan,
        );
        assert_eq!(r.model, "claude-haiku-4-5");
        assert_eq!(r.effort, Some(Effort::Max));
        assert_eq!(r.profile.as_deref(), Some("fast-profile"));
    }

    #[test]
    fn agent_md_beats_the_settings_chain() {
        let nyan = test_nyan_settings();
        let r = subagent(None, Some("@heavy:medium"), "review", None, &nyan);
        assert_eq!(r.model, "claude-opus-4-6");
        assert_eq!(r.effort, Some(Effort::Medium));
    }

    #[test]
    fn selection_subagents_beats_selection_subagent_and_the_type_default() {
        let mut nyan = test_nyan_settings();
        nyan.selection.as_mut().unwrap().subagent = Some("fast-profile".to_string());
        // `review` has its own entry (deep-profile:high); `explore` does not.
        let review = subagent(None, None, "review", None, &nyan);
        assert_eq!(review.model, "claude-opus-4-6");
        assert_eq!(review.effort, Some(Effort::High));
        // `selection.subagent` is the fallback for a type with no entry of its
        // own, and it beats the built-in default (`explore` = @light).
        let general = subagent(None, None, "general-purpose", None, &nyan);
        assert_eq!(general.model, "claude-haiku-4-5");
        assert_eq!(general.profile.as_deref(), Some("fast-profile"));
    }

    #[test]
    fn type_defaults_apply_when_nothing_is_configured() {
        let nyan = test_nyan_settings();
        // explore = @light -> fast-profile, whose own effort is off.
        let explore = subagent(None, None, "explore", Some(Effort::Max), &nyan);
        assert_eq!(explore.model, "claude-haiku-4-5");
        assert_eq!(explore.effort, Some(Effort::Off));
        // general-purpose and plan = inherit.
        for agent_type in ["general-purpose", "plan"] {
            let r = subagent(None, None, agent_type, Some(Effort::High), &nyan);
            assert_eq!(r.model, PARENT, "{agent_type}");
            assert_eq!(r.effort, Some(Effort::High), "{agent_type}");
            assert_eq!(r.profile, None, "{agent_type}");
        }
        // An unknown type inherits too.
        assert_eq!(subagent(None, None, "custom", None, &nyan).model, PARENT);
    }

    #[test]
    fn effort_follows_the_layer_that_decided_the_model() {
        let nyan = test_nyan_settings();
        // inherit -> the parent's effort, never a profile's.
        let inherit = subagent(Some("inherit"), None, "explore", Some(Effort::Max), &nyan);
        assert_eq!(inherit.model, PARENT);
        assert_eq!(inherit.effort, Some(Effort::Max));
        // `:effort` alone is `inherit:effort` and overrides the parent's.
        let only_effort = subagent(Some(":off"), None, "plan", Some(Effort::Max), &nyan);
        assert_eq!(only_effort.model, PARENT);
        assert_eq!(only_effort.effort, Some(Effort::Off));
        // A named profile ignores the parent's effort and uses its own...
        let named = subagent(Some("deep-profile"), None, "plan", Some(Effort::Off), &nyan);
        assert_eq!(named.effort, Some(Effort::Max));
        // ...and an explicit `:effort` on that layer beats the profile's.
        let explicit = subagent(Some("deep-profile:medium"), None, "plan", None, &nyan);
        assert_eq!(explicit.effort, Some(Effort::Medium));
    }

    #[test]
    fn a_profile_without_effort_and_a_temporary_model_use_the_model_default() {
        let mut nyan = test_nyan_settings();
        nyan.models
            .as_mut()
            .unwrap()
            .get_mut("deep-profile")
            .unwrap()
            .effort = None;
        // The parent's depth must not leak onto another vendor's model.
        let profile = subagent(Some("deep-profile"), None, "plan", Some(Effort::Max), &nyan);
        assert_eq!(profile.effort, None);
        let temporary = subagent(
            Some("openai/gpt-4o"),
            None,
            "plan",
            Some(Effort::Max),
            &nyan,
        );
        assert_eq!(temporary.model, "openai/gpt-4o");
        assert_eq!(temporary.effort, None);
        assert_eq!(temporary.profile, None);
        let temporary_with_effort = subagent(Some("openai/gpt-4o:high"), None, "plan", None, &nyan);
        assert_eq!(temporary_with_effort.model, "openai/gpt-4o");
        assert_eq!(temporary_with_effort.effort, Some(Effort::High));
    }

    #[test]
    fn inherit_follows_the_parents_profile_not_just_its_model() {
        let nyan = test_nyan_settings();
        let routed = ParentSelection {
            model: "claude-opus-4-6".to_string(),
            effort: Some(Effort::High),
            profile: Some("deep-profile".to_string()),
        };
        let r = resolve_subagent_model_and_effort(None, None, "plan", &routed, Some(&nyan));
        assert_eq!(r.model, "claude-opus-4-6");
        assert_eq!(r.effort, Some(Effort::High));
        assert_eq!(r.profile.as_deref(), Some("deep-profile"));
        // A tier that names no profile follows the parent just the same.
        let tier = resolve_subagent_model_and_effort(
            Some("@nope"),
            None,
            "plan",
            &routed,
            Some(&NyanSettings::default()),
        );
        assert_eq!(tier.profile.as_deref(), Some("deep-profile"));
        // Naming a profile replaces it.
        let named = resolve_subagent_model_and_effort(
            Some("fast-profile"),
            None,
            "plan",
            &routed,
            Some(&nyan),
        );
        assert_eq!(named.profile.as_deref(), Some("fast-profile"));
    }

    #[test]
    fn a_colon_in_a_model_id_is_only_split_for_a_valid_effort() {
        let nyan = test_nyan_settings();
        let tag = subagent(Some("ollama/qwen3:32b"), None, "plan", None, &nyan);
        assert_eq!(tag.model, "ollama/qwen3:32b");
        assert_eq!(tag.effort, None);
        let tag_with_effort = subagent(Some("ollama/qwen3:32b:max"), None, "plan", None, &nyan);
        assert_eq!(tag_with_effort.model, "ollama/qwen3:32b");
        assert_eq!(tag_with_effort.effort, Some(Effort::Max));
    }

    #[test]
    fn blank_specs_fall_through_to_the_next_layer() {
        let nyan = test_nyan_settings();
        let r = subagent(Some("  "), Some(""), "review", None, &nyan);
        assert_eq!(r.model, "claude-opus-4-6");
        assert_eq!(r.effort, Some(Effort::High));
    }

    #[test]
    fn light_and_heavy_fall_back_to_main_when_unset() {
        let mut nyan = test_nyan_settings();
        let selection = nyan.selection.as_mut().unwrap();
        selection.light = None;
        selection.heavy = None;
        selection.subagents = None;
        let light = subagent(None, None, "explore", None, &nyan);
        assert_eq!(light.model, "claude-sonnet-4-6");
        assert_eq!(light.profile.as_deref(), Some("main-profile"));
        assert_eq!(light.effort, Some(Effort::High));
        let heavy = subagent(None, None, "review", None, &nyan);
        assert_eq!(heavy.profile.as_deref(), Some("main-profile"));
    }

    #[test]
    fn tiers_without_any_selection_follow_the_session_instead_of_failing() {
        let nyan = NyanSettings::default();
        for spec in ["@light", "@heavy", "@main"] {
            let r = subagent(Some(spec), None, "explore", Some(Effort::High), &nyan);
            assert_eq!(r.model, PARENT, "{spec}");
            assert_eq!(r.effort, Some(Effort::High), "{spec}");
            assert_eq!(r.profile, None, "{spec}");
        }
        // No nyan settings at all behaves the same.
        let r = resolve_subagent_model_and_effort(
            None,
            None,
            "explore",
            &parent(Some(Effort::Off)),
            None,
        );
        assert_eq!(r.model, PARENT);
        assert_eq!(r.effort, Some(Effort::Off));
    }

    #[test]
    fn a_tier_that_names_no_profile_follows_the_session_and_never_reaches_the_wire() {
        let mut nyan = test_nyan_settings();
        nyan.selection = Some(SelectionSettings::default());
        // A selection with no `main`, and a tier name nobody defined.
        for spec in ["@heavy", "@light", "@main", "@nope"] {
            let r = subagent(Some(spec), None, "plan", Some(Effort::High), &nyan);
            assert_eq!(r.model, PARENT, "{spec}");
            assert_eq!(r.effort, Some(Effort::High), "{spec}");
            assert_eq!(r.profile, None, "{spec}");
        }
        // An explicit `:effort` still applies to the layer.
        let r = subagent(Some("@heavy:off"), None, "plan", Some(Effort::High), &nyan);
        assert_eq!(r.effort, Some(Effort::Off));
    }

    #[test]
    fn utility_roles_use_their_entry_then_the_role_default() {
        let nyan = test_nyan_settings();
        let utility = |role: &str, parent_effort: Option<Effort>| {
            resolve_utility_model_and_effort(role, &parent(parent_effort), Some(&nyan))
        };
        // Configured: title -> fast-profile:off.
        let title = utility("title", Some(Effort::Max));
        assert_eq!(title.model, "claude-haiku-4-5");
        assert_eq!(title.effort, Some(Effort::Off));
        // Unconfigured compaction and classifier = @main -> main-profile (high).
        for role in ["compaction", "classifier"] {
            let r = utility(role, Some(Effort::Off));
            assert_eq!(r.model, "claude-sonnet-4-6", "{role}");
            assert_eq!(r.effort, Some(Effort::High), "{role}");
        }
        // Unconfigured summary = @light -> fast-profile (off).
        let summary = utility("summary", None);
        assert_eq!(summary.model, "claude-haiku-4-5");
        assert_eq!(summary.effort, Some(Effort::Off));
    }

    #[test]
    fn utility_role_names_match_loosely() {
        let mut nyan = test_nyan_settings();
        nyan.selection.as_mut().unwrap().utility = Some(BTreeMap::from([(
            "Commit-Message".to_string(),
            "deep-profile".to_string(),
        )]));
        for role in ["commit_message", "commit-message", " COMMIT_MESSAGE "] {
            let r = resolve_utility_model_and_effort(role, &parent(None), Some(&nyan));
            assert_eq!(r.model, "claude-opus-4-6", "{role:?}");
        }
        let other = resolve_utility_model_and_effort("title", &parent(None), Some(&nyan));
        assert_eq!(other.model, "claude-haiku-4-5");
    }

    #[test]
    fn utility_inherit_follows_the_session_model_and_effort() {
        let mut nyan = test_nyan_settings();
        nyan.selection.as_mut().unwrap().utility = Some(BTreeMap::from([(
            "title".to_string(),
            "inherit".to_string(),
        )]));
        let r = resolve_utility_model_and_effort("title", &parent(Some(Effort::Max)), Some(&nyan));
        assert_eq!(r.model, PARENT);
        assert_eq!(r.effort, Some(Effort::Max));
    }

    #[test]
    fn the_display_text_marks_inherit() {
        assert_eq!(
            get_agent_model_display(None),
            "Inherit from parent (default)"
        );
        assert_eq!(
            get_agent_model_display(Some("inherit")),
            "Inherit from parent (default)"
        );
        assert_eq!(get_agent_model_display(Some("@heavy:high")), "@heavy:high");
    }
}
