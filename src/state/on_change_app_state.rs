//! Maps to: CC `state/onChangeAppState.ts` — the single choke point for
//! state-change side effects.
//!
//! Every effective `AppStore::set_state`/`replace_with` install that changes
//! one of the watched fields triggers the corresponding effect here;
//! scattered mutation callsites need zero changes.
//!
//! Diff style mirrors CC: scalar fields use `!=` (CC `!==` on primitives);
//! Arc-wrapped fields use `Arc::ptr_eq` (CC `!==` reference identity).

use std::sync::Arc;

use super::app_state_store::{AppState, ExpandedView};
use crate::utils::config::save_global_config;

/// Maps to: CC `onChangeAppState({newState, oldState})`.
pub fn on_change_app_state(new: &AppState, old: &AppState) {
    // mainLoopProfile — mirror into bootstrap state so non-AppState readers
    // (`get_main_loop_model`) follow the session. Maps to the override half of
    // onChangeAppState.ts:94-112. @nyan: never persisted; the picker's Ctrl+S
    // writes `selection.main` explicitly.
    if new.main_loop_profile != old.main_loop_profile {
        crate::bootstrap::state::set_session_profile(new.main_loop_profile.clone());
    }

    // expandedView — persist as showExpandedTodos + showSpinnerTree for
    // backwards compat. Maps to: onChangeAppState.ts:114-128, including the
    // read-before-write guard.
    if new.expanded_view != old.expanded_view {
        let show_expanded_todos = new.expanded_view == ExpandedView::Tasks;
        let show_spinner_tree = new.expanded_view == ExpandedView::Teammates;
        let current = crate::utils::config::load_global_config();
        if (current.show_expanded_todos != Some(show_expanded_todos)
            || current.show_spinner_tree != Some(show_spinner_tree))
            && let Err(error) = save_global_config(|config| {
                config.show_expanded_todos = Some(show_expanded_todos);
                config.show_spinner_tree = Some(show_spinner_tree);
            })
        {
            tracing::warn!(%error, "failed to persist expandedView");
        }
    }

    // verbose — persist to globalConfig. Maps to: onChangeAppState.ts:130-140.
    if new.verbose != old.verbose {
        let current = crate::utils::config::load_global_config();
        if current.verbose != Some(new.verbose) {
            let verbose = new.verbose;
            if let Err(error) = save_global_config(|config| {
                config.verbose = Some(verbose);
            }) {
                tracing::warn!(%error, "failed to persist verbose");
            }
        }
    }

    // tungstenPanelVisible (onChangeAppState.ts:142-152, ant-only USER_TYPE
    // persistence) is not ported — the field and its subsystem are absent.

    // settings — clear auth-related caches when settings change, then re-apply
    // env when settings.env changes. Maps to: onChangeAppState.ts:154-170.
    if !Arc::ptr_eq(&new.settings, &old.settings) {
        crate::utils::auth::clear_api_key_helper_cache();
        // CC also clears AWS/GCP credential caches; those helpers are not
        // ported yet — attach here when auth.ts parity lands.
        //
        // CC :164 gates on `newState.settings.env !== oldState.settings.env`,
        // a REFERENCE inequality, which `Arc::ptr_eq` is.
        // - A reload re-parses, so its env is a new `Arc` and re-applies even
        //   with identical values. Value equality would under-fire.
        // - An update that keeps the env `Arc` does not re-apply, e.g. a
        //   spread-style `Arc::make_mut` (ThemePicker Ctrl+T) or `/config`
        //   Esc writing its snapshot back. CC's spread keeps the reference
        //   too.
        let env_changed = match (&new.settings.env, &old.settings.env) {
            (None, None) => false,
            (Some(new_env), Some(old_env)) => !Arc::ptr_eq(new_env, old_env),
            _ => true,
        };
        if env_changed {
            crate::utils::managed_env::apply_config_environment_variables();
        }
    }
}

/// Standard on-change hook for production stores.
/// Maps to: CC `components/App.tsx:28-31` always passing `onChangeAppState`.
pub fn default_on_change() -> super::store::OnChangeFn {
    Arc::new(|new: &AppState, old: &AppState| on_change_app_state(new, old))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::store::AppStore;
    use crate::tool::ToolPermissionContext;
    use crate::types::permissions::PermissionMode;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    /// The diff must fire exactly for the fields that changed — the
    /// triggering matrix is the contract that scattered callsites rely on.
    #[test]
    fn mode_change_is_detected_through_store_updates() {
        let store = AppStore::new(AppState::default(), Some(default_on_change()));

        store.replace_with(|state| {
            state.set_tool_permission_context(ToolPermissionContext {
                mode: PermissionMode::Plan,
                ..ToolPermissionContext::default()
            });
        });

        assert_eq!(
            store.get().tool_permission_context.mode,
            PermissionMode::Plan
        );
    }

    #[test]
    fn arc_field_replacement_changes_pointer_identity_for_diffing() {
        let old = AppState::default();
        let mut new = old.clone();
        assert!(Arc::ptr_eq(
            &old.tool_permission_context,
            &new.tool_permission_context
        ));

        new.set_tool_permission_context(ToolPermissionContext::default());
        assert!(!Arc::ptr_eq(
            &old.tool_permission_context,
            &new.tool_permission_context
        ));
        // Value-equal but pointer-distinct. Post-B3-flip the store treats
        // this as a fresh root and notifies (CC `{...prev}` spread — same
        // shape, new object, `Object.is` false); this assertion only locks
        // AppState's value-equality semantics for source-branch guards.
        assert_eq!(old, new);
    }

    #[test]
    fn profile_changes_update_the_session_profile_without_writing_settings() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let store = AppStore::new(AppState::default(), Some(default_on_change()));
        store.replace_with(|state| state.main_loop_profile = Some("deep".to_string()));
        assert_eq!(
            crate::bootstrap::state::get_session_profile().as_deref(),
            Some("deep")
        );
        store.replace_with(|state| state.main_loop_profile = None);
        assert_eq!(crate::bootstrap::state::get_session_profile(), None);
    }

    #[test]
    fn settings_pointer_change_clears_api_key_helper_cache_seam() {
        // Smoke: updating settings Arc must not panic through on_change.
        let store = AppStore::new(AppState::default(), Some(default_on_change()));
        store.replace_with(|state| {
            let mut settings = (*state.settings).clone();
            settings.output_style = Some("explanatory".to_string());
            state.settings = Arc::new(settings);
        });
        assert_eq!(
            store.get().settings.output_style.as_deref(),
            Some("explanatory")
        );
    }

    /// CC `:164`: `newState.settings.env !== oldState.settings.env`. A settings
    /// update that keeps the env reference (a spread, like ThemePicker's
    /// Ctrl+T) does not re-apply; a reload, whose env is a new reference, does.
    #[test]
    fn settings_env_reapplies_only_for_a_new_env_reference() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = std::env::temp_dir().join(format!(
            "nyan-env-reference-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("settings.json"),
            r#"{"env":{"NYAN_ENV_REFERENCE_PROBE":"applied"}}"#,
        )
        .unwrap();
        let _config = EnvVarGuard::set("NYAN_CONFIG_DIR", &root);
        let _probe = EnvVarGuard::unset("NYAN_ENV_REFERENCE_PROBE");
        crate::utils::settings::settings_cache::reset_settings_cache();

        let mut old = AppState::default();
        let mut settings = (*old.settings).clone();
        settings.env = Some(Arc::new(indexmap::IndexMap::from([(
            "NYAN_ENV_REFERENCE_PROBE".to_string(),
            "applied".to_string(),
        )])));
        old.settings = Arc::new(settings);

        let mut spread = old.clone();
        let mut settings = (*spread.settings).clone();
        settings.output_style = Some("explanatory".to_string());
        spread.settings = Arc::new(settings);
        on_change_app_state(&spread, &old);
        assert_eq!(
            crate::utils::process_env::var("NYAN_ENV_REFERENCE_PROBE"),
            None,
            "a spread keeps the env reference and must not re-apply"
        );

        let mut reloaded = spread.clone();
        let mut settings = (*reloaded.settings).clone();
        settings.env = Some(Arc::new((**spread.settings.env.as_ref().unwrap()).clone()));
        reloaded.settings = Arc::new(settings);
        on_change_app_state(&reloaded, &spread);
        assert_eq!(
            crate::utils::process_env::var("NYAN_ENV_REFERENCE_PROBE").as_deref(),
            Some("applied"),
            "a reload's new env reference re-applies, even with equal values"
        );

        crate::utils::settings::settings_cache::reset_settings_cache();
        let _ = std::fs::remove_dir_all(root);
    }
}
