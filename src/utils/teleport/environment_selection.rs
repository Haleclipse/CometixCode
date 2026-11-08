//! Maps to: CC `utils/teleport/environmentSelection.ts`.
//!
//! The official `getEnvironmentSelectionInfo()` fetches environments from the
//! Claude.ai API and reads merged/per-source settings. Nyan keeps the pure
//! selection semantics here from already-known inputs; OAuth/network/settings
//! writes are deferred to the remote runtime/settings slice.

use super::environments::{EnvironmentKind, EnvironmentResource};
use crate::utils::settings::SettingSource;

/// Maps to: CC `utils/teleport/environmentSelection.ts`
/// `EnvironmentSelectionInfo`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EnvironmentSelectionInfo {
    pub available_environments: Vec<EnvironmentResource>,
    pub selected_environment: Option<EnvironmentResource>,
    pub selected_environment_source: Option<SettingSource>,
}

fn source_priority_high_to_low() -> [SettingSource; 3] {
    // Official SETTING_SOURCES is [user, project, local, flag, policy] and the
    // source lookup iterates from the end, skipping flagSettings.
    // @nyan: no policySettings tier.
    [
        SettingSource::Local,
        SettingSource::Project,
        SettingSource::User,
    ]
}

/// Maps to: CC `getEnvironmentSelectionInfo` selection/source resolution.
pub fn get_environment_selection_info_from_snapshot(
    environments: Vec<EnvironmentResource>,
    merged_default_environment_id: Option<&str>,
    source_default_environment_ids: &[(SettingSource, Option<String>)],
) -> EnvironmentSelectionInfo {
    if environments.is_empty() {
        return EnvironmentSelectionInfo {
            available_environments: Vec::new(),
            selected_environment: None,
            selected_environment_source: None,
        };
    }

    let mut selected_environment = environments
        .iter()
        .find(|environment| environment.kind != EnvironmentKind::Bridge)
        .cloned()
        .unwrap_or_else(|| environments[0].clone());
    let mut selected_environment_source = None;

    if let Some(default_environment_id) = merged_default_environment_id.filter(|id| !id.is_empty())
        && let Some(matching_environment) = environments
            .iter()
            .find(|environment| environment.environment_id == default_environment_id)
            .cloned()
    {
        selected_environment = matching_environment;

        for source in source_priority_high_to_low() {
            let source_matches =
                source_default_environment_ids
                    .iter()
                    .any(|(candidate_source, candidate_id)| {
                        *candidate_source == source
                            && candidate_id.as_deref() == Some(default_environment_id)
                    });
            if source_matches {
                selected_environment_source = Some(source);
                break;
            }
        }
    }

    EnvironmentSelectionInfo {
        available_environments: environments,
        selected_environment: Some(selected_environment),
        selected_environment_source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::settings::get_setting_source_name;

    fn env(kind: EnvironmentKind, id: &str, name: &str) -> EnvironmentResource {
        EnvironmentResource::new(kind, id, name)
    }

    #[test]
    fn environment_selection_empty_returns_null_selection() {
        assert_eq!(
            get_environment_selection_info_from_snapshot(Vec::new(), None, &[]),
            EnvironmentSelectionInfo::default()
        );
    }

    #[test]
    fn environment_selection_defaults_to_first_non_bridge_like_official() {
        let bridge = env(EnvironmentKind::Bridge, "bridge_1", "Bridge");
        let cloud = env(EnvironmentKind::AnthropicCloud, "env_cloud", "Cloud");
        let info = get_environment_selection_info_from_snapshot(
            vec![bridge.clone(), cloud.clone()],
            None,
            &[],
        );

        assert_eq!(info.selected_environment, Some(cloud));
        assert_eq!(info.selected_environment_source, None);
    }

    #[test]
    fn environment_selection_uses_matching_default_and_highest_non_flag_source() {
        let cloud = env(EnvironmentKind::AnthropicCloud, "env_cloud", "Cloud");
        let byoc = env(EnvironmentKind::Byoc, "env_byoc", "BYOC");
        let info = get_environment_selection_info_from_snapshot(
            vec![cloud, byoc.clone()],
            Some("env_byoc"),
            &[
                (SettingSource::User, Some("env_byoc".to_string())),
                (SettingSource::Flag, Some("env_byoc".to_string())),
                (SettingSource::Local, Some("env_byoc".to_string())),
            ],
        );

        assert_eq!(info.selected_environment, Some(byoc));
        assert_eq!(info.selected_environment_source, Some(SettingSource::Local));
        assert_eq!(
            get_setting_source_name(SettingSource::Local),
            "project, gitignored"
        );
    }

    #[test]
    fn environment_selection_ignores_unknown_default_id() {
        let cloud = env(EnvironmentKind::AnthropicCloud, "env_cloud", "Cloud");
        let info = get_environment_selection_info_from_snapshot(
            vec![cloud.clone()],
            Some("missing"),
            &[(SettingSource::Local, Some("missing".to_string()))],
        );

        assert_eq!(info.selected_environment, Some(cloud));
        assert_eq!(info.selected_environment_source, None);
    }
}
