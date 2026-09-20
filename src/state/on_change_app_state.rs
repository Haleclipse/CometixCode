//! Maps to: CC `state/onChangeAppState.ts` — the single choke point for
//! state-change side effects.
//!
//! Every effective `AppStore::set_state`/`replace_with` install that changes
//! one of the watched fields triggers the corresponding effect here;
//! scattered mutation callsites need zero changes (CC's comment block at onChangeAppState.ts:50-64 documents
//! how permission-mode sync was previously missed by 6 of 8+ mutation paths
//! until it was centralized in this diff).
//!
//! Diff style mirrors CC: scalar fields use `!=` (CC `!==` on primitives);
//! Arc-wrapped fields use `Arc::ptr_eq` (CC `!==` reference identity).

use std::sync::Arc;

use super::app_state_store::{AppState, ExpandedView};
use crate::utils::config::save_global_config;
use crate::utils::settings::SettingSource;
use crate::utils::settings::update_settings_for_source;

/// Maps to: CC `onChangeAppState.ts:24-40 externalMetadataToAppState` — the
/// inverse of the push below, applied on worker restart.
///
/// CC returns a `(prev: AppState) => AppState` updater and the single caller
/// does `setAppState(externalMetadataToAppState(metadata))`
/// (`cli/print.ts:5057`). This returns the `&mut AppState` mutator that
/// `AppStore::replace_with` takes, which is the same position in the Rust write
/// channel.
///
/// Both fields are applied only when actually present as a value. CC tests
/// `typeof metadata.permission_mode === 'string'` / `=== 'boolean'`, so an
/// explicit JSON `null` does NOT clear the field — it is simply not applied.
/// In the RFC 7396 three-state encoding that is `Some(Some(_))` only; both
/// `Some(None)` (null) and `None` (absent) fall through.
pub fn external_metadata_to_app_state(
    metadata: &crate::utils::session_state::SessionExternalMetadata,
) -> impl FnOnce(&mut AppState) + '_ {
    move |state| {
        // CC `:29-36`.
        if let Some(Some(mode)) = metadata.permission_mode.as_ref() {
            let mut context = (*state.tool_permission_context).clone();
            context.mode =
                crate::utils::permissions::permission_mode::permission_mode_from_string(mode);
            state.set_tool_permission_context(context);
        }
        // CC `:37-39`.
        if let Some(Some(is_ultraplan_mode)) = metadata.is_ultraplan_mode {
            state.is_ultraplan_mode = Some(is_ultraplan_mode);
        }
    }
}

/// Maps to: CC `onChangeAppState({newState, oldState})`.
pub fn on_change_app_state(new: &AppState, old: &AppState) {
    // toolPermissionContext.mode — single choke point for external mode sync.
    // Maps to: onChangeAppState.ts:65-92.
    let prev_mode = old.tool_permission_context.mode;
    let new_mode = new.tool_permission_context.mode;
    if prev_mode != new_mode {
        // CC: externalize mode, notifySessionMetadataChanged (CCR) +
        // notifyPermissionModeChanged (SDK status stream). Those transport
        // channels (CCR client / SDK control stream) do not exist yet in
        // Cometix; this seam is where they attach. The diff-driven trigger
        // is in place so no mutation path needs revisiting later.
        tracing::debug!(
            from = ?prev_mode,
            to = ?new_mode,
            "permission mode changed (external notify seam)"
        );
    }

    // mainLoopModel — persist to userSettings and update the bootstrap override.
    // Maps to: onChangeAppState.ts:94-112.
    if new.main_loop_model != old.main_loop_model {
        // The outer Some records CC's explicit null when /model clears the
        // setting, so env/settings do not become active again during the same
        // session. This in-memory update is independent of the write gate.
        crate::bootstrap::state::set_main_loop_model_override(Some(new.main_loop_model.clone()));

        // Settings persistence respects COMETIX_WRITE_ENABLED like
        // save_global_config.
        if crate::utils::env_utils::is_env_truthy(
            std::env::var("COMETIX_WRITE_ENABLED").ok().as_deref(),
        ) {
            let mut updates = serde_json::Map::new();
            match &new.main_loop_model {
                Some(model) => {
                    updates.insert(
                        "model".to_string(),
                        serde_json::Value::String(model.clone()),
                    );
                }
                None => {
                    // CC passes `undefined` to remove; JSON null clears the key
                    // value in our patch writer (preserve-unknowns path).
                    updates.insert("model".to_string(), serde_json::Value::Null);
                }
            }
            if let Err(error) = update_settings_for_source(SettingSource::User, &updates) {
                tracing::warn!(%error, "failed to persist mainLoopModel to userSettings");
            }
        } else {
            tracing::debug!(
                model = ?new.main_loop_model,
                "mainLoopModel changed (write gate closed; userSettings not persisted)"
            );
        }
    }

    // expandedView — persist as showExpandedTodos + showSpinnerTree for
    // backwards compat. Maps to: onChangeAppState.ts:114-128, including the
    // read-before-write guard.
    if new.expanded_view != old.expanded_view {
        let show_expanded_todos = new.expanded_view == ExpandedView::Tasks;
        let show_spinner_tree = new.expanded_view == ExpandedView::Teammates;
        let current = crate::utils::config::load_global_config();
        if current.show_expanded_todos != Some(show_expanded_todos)
            || current.show_spinner_tree != Some(show_spinner_tree)
        {
            if let Err(error) = save_global_config(|config| {
                config.show_expanded_todos = Some(show_expanded_todos);
                config.show_spinner_tree = Some(show_spinner_tree);
            }) {
                tracing::warn!(%error, "failed to persist expandedView");
            }
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
        // CC :164 gates on `newState.settings.env !== oldState.settings.env`
        // — REFERENCE inequality. Every settings reload re-parses, so the env
        // references differ whenever either side carries an env object; the
        // gate only short-circuits when both are undefined. Value equality
        // here would under-fire: a reload with identical env values must
        // still re-apply (and re-run the source effects inside
        // applyConfigEnvironmentVariables when those owners land).
        if new.settings.env.is_some() || old.settings.env.is_some() {
            crate::utils::managed_env::apply_config_environment_variables();
            // Cometix extension (no CC counterpart): keep active proxy channel env overrides.
            crate::services::proxy_channel::apply_active_channel_env();
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
    use crate::utils::session_state::SessionExternalMetadata;

    fn apply(metadata: &SessionExternalMetadata) -> AppState {
        let mut state = AppState::default();
        external_metadata_to_app_state(metadata)(&mut state);
        state
    }

    /// Maps to: CC `:29-39` — a present string/boolean is applied.
    #[test]
    fn present_values_are_applied() {
        let state = apply(&SessionExternalMetadata {
            permission_mode: Some(Some("plan".to_string())),
            is_ultraplan_mode: Some(Some(true)),
            ..SessionExternalMetadata::default()
        });

        assert_eq!(state.tool_permission_context.mode, PermissionMode::Plan);
        assert_eq!(state.is_ultraplan_mode, Some(true));
    }

    /// Maps to: CC `:29`/`:37` — the guard is `typeof x === 'string'` /
    /// `'boolean'`, so an explicit JSON `null` does NOT clear the field. In the
    /// RFC 7396 encoding that is `Some(None)`, and it must fall through exactly
    /// like an absent key.
    #[test]
    fn explicit_null_does_not_clear_because_official_tests_typeof() {
        let mut state = AppState::default();
        state.is_ultraplan_mode = Some(true);
        state.set_tool_permission_context(ToolPermissionContext {
            mode: PermissionMode::Plan,
            ..ToolPermissionContext::default()
        });

        external_metadata_to_app_state(&SessionExternalMetadata {
            permission_mode: Some(None),
            is_ultraplan_mode: Some(None),
            ..SessionExternalMetadata::default()
        })(&mut state);

        assert_eq!(
            state.tool_permission_context.mode,
            PermissionMode::Plan,
            "null is not a value, so nothing is applied"
        );
        assert_eq!(state.is_ultraplan_mode, Some(true));
    }

    /// Absent keys leave everything alone — the same fall-through as null, via
    /// a different arm.
    #[test]
    fn absent_keys_leave_state_untouched() {
        let baseline = AppState::default();

        let state = apply(&SessionExternalMetadata::default());

        assert_eq!(
            state.tool_permission_context.mode,
            baseline.tool_permission_context.mode
        );
        assert_eq!(state.is_ultraplan_mode, baseline.is_ultraplan_mode);
    }

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
    fn model_changes_update_runtime_override_matches_official_even_when_writes_are_disabled() {
        let _env_guard = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let _write = crate::utils::env_utils::EnvVarGuard::unset("COMETIX_WRITE_ENABLED");
        let previous_override = crate::bootstrap::state::get_main_loop_model_override();

        let store = AppStore::new(AppState::default(), Some(default_on_change()));
        store.replace_with(|state| state.main_loop_model = Some("sonnet".to_string()));
        assert_eq!(
            crate::bootstrap::state::get_main_loop_model_override(),
            Some(Some("sonnet".to_string()))
        );

        store.replace_with(|state| state.main_loop_model = None);
        assert_eq!(
            crate::bootstrap::state::get_main_loop_model_override(),
            Some(None),
            "clearing /model must retain CC's explicit-null override identity"
        );

        crate::bootstrap::state::set_main_loop_model_override(previous_override);
    }

    #[test]
    fn settings_pointer_change_clears_api_key_helper_cache_seam() {
        // Smoke: updating settings Arc must not panic through on_change.
        let store = AppStore::new(AppState::default(), Some(default_on_change()));
        store.replace_with(|state| {
            let mut settings = (*state.settings).clone();
            settings.model = Some("claude-sonnet-4-6".to_string());
            state.settings = Arc::new(settings);
        });
        assert_eq!(
            store.get().settings.model.as_deref(),
            Some("claude-sonnet-4-6")
        );
    }
}
