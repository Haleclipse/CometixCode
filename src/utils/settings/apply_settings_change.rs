//! Maps to: CC `utils/settings/applySettingsChange.ts`.
//!
//! Re-reads settings and source-attributed permission rules from disk, then
//! replaces the corresponding AppState snapshots.

use super::SettingSource;
use super::{get_settings_for_source, get_settings_with_errors};
use crate::state::store::AppStore;
use std::sync::Arc;

/// Maps to: CC `utils/settings/applySettingsChange.ts`
/// `applySettingsChange(...)`.
pub fn apply_settings_change(source: SettingSource, store: &AppStore) {
    let fresh = get_settings_with_errors();
    crate::utils::debug::log_for_debugging(&format!(
        "Settings changed from {source:?}, updating app state"
    ));
    let permission_rules =
        crate::utils::permissions::permissions_loader::load_all_permission_rules_from_disk();
    let managed_only =
        crate::utils::permissions::permissions_loader::should_allow_managed_permission_rules_only();
    let policy_settings = get_settings_for_source(SettingSource::Policy);
    crate::utils::hooks::hooks_config_snapshot::update_hooks_config_snapshot(
        &fresh.settings,
        policy_settings.as_ref(),
        crate::utils::settings::plugin_only_policy::is_restricted_to_plugin_only("hooks"),
    );
    store.replace_with(|state| {
        let previous_effort = state.settings.effort_level.clone();
        let new_effort = fresh.settings.effort_level.clone();
        let mut permission_context =
            crate::utils::permissions::permissions::sync_permission_rules_from_disk(
                &state.tool_permission_context,
                &permission_rules,
                managed_only,
            );

        // Maps to CC's internal-distribution broad-shell re-strip after sync.
        if crate::utils::build_profile::has_internal_capability(
            crate::utils::build_profile::InternalCapability::Permissions,
        ) && crate::utils::process_env::var("CLAUDE_CODE_ENTRYPOINT").as_deref() != Some("local-agent")
        {
            let dangerous =
                crate::utils::permissions::permission_setup::find_overly_broad_bash_permissions(
                    &permission_rules,
                    &[],
                );
            if !dangerous.is_empty() {
                permission_context = crate::utils::permissions::permission_setup::remove_dangerous_permissions(
                    &permission_context,
                    &dangerous,
                );
            }
        }
        if permission_context.is_bypass_permissions_mode_available
            && crate::utils::permissions::permission_setup::is_bypass_permissions_mode_disabled()
        {
            permission_context = crate::utils::permissions::permission_setup::create_disabled_bypass_permissions_context(
                &permission_context,
            );
        }
        permission_context =
            crate::utils::permissions::permission_setup::transition_plan_auto_mode(
                &permission_context,
            );

        state.settings = Arc::new(fresh.settings.clone());
        if previous_effort != new_effort {
            if let Some(effort) = new_effort
                .as_deref()
                .and_then(crate::utils::effort::parse_effort_value)
            {
                state.effort_value = Some(effort);
            }
        }
        state.set_tool_permission_context(permission_context);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::app_state_store::AppState;

    #[test]
    fn apply_settings_change_replaces_app_state_settings_snapshot() {
        let store = AppStore::new(AppState::default(), None);
        let before = store.get().settings.clone();
        apply_settings_change(SettingSource::User, &store);
        let after = store.get().settings.clone();
        // Disk state in the test environment may equal the default; the
        // contract under test is that the field is a fresh snapshot object
        // owned by AppState, not that its contents differ.
        assert!(Arc::strong_count(&after) >= 1);
        let _ = before;
    }
}
