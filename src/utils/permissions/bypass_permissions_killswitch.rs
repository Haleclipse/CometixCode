//! Maps to: CC `utils/permissions/bypassPermissionsKillswitch.ts`.
//! Gate results are applied to the current AppStore context after async reads.

use crate::context::notifications::{Notification, NotificationColor, NotificationPriority};
use crate::state::store::{AppStore, UpdateDecision};
use crate::tool::ToolPermissionContext;
use iocraft::prelude::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

static BYPASS_PERMISSIONS_CHECK_RAN: AtomicBool = AtomicBool::new(false);
static AUTO_MODE_CHECK_RAN: AtomicBool = AtomicBool::new(false);

/// Maps to: CC `bypassPermissionsKillswitch.ts:19-49#checkAndDisableBypassPermissionsIfNeeded`.
pub async fn check_and_disable_bypass_permissions_if_needed(
    tool_permission_context: &ToolPermissionContext,
    store: &AppStore,
) {
    if BYPASS_PERMISSIONS_CHECK_RAN.swap(true, Ordering::SeqCst) {
        return;
    }
    if !tool_permission_context.is_bypass_permissions_mode_available {
        return;
    }
    if !super::permission_setup::should_disable_bypass_permissions().await {
        return;
    }
    store.set_state(|prev| {
        let mut next = prev.as_ref().clone();
        next.tool_permission_context = Arc::new(
            super::permission_setup::create_disabled_bypass_permissions_context(
                &prev.tool_permission_context,
            ),
        );
        UpdateDecision::Replace {
            next: Arc::new(next),
            result: (),
        }
    });
}

/// Maps to: CC `bypassPermissionsKillswitch.ts:55-57#resetBypassPermissionsCheck`.
pub fn reset_bypass_permissions_check() {
    BYPASS_PERMISSIONS_CHECK_RAN.store(false, Ordering::SeqCst);
}

/// Maps to: CC `bypassPermissionsKillswitch.ts:81-128#checkAndDisableAutoModeIfNeeded`.
pub async fn check_and_disable_auto_mode_if_needed(
    tool_permission_context: &ToolPermissionContext,
    store: &AppStore,
    fast_mode: Option<bool>,
) {
    if !super::permission_setup::is_transcript_classifier_feature_enabled() {
        return;
    }
    if AUTO_MODE_CHECK_RAN.swap(true, Ordering::SeqCst) {
        return;
    }
    let result =
        super::permission_setup::verify_auto_mode_gate_access(tool_permission_context, fast_mode)
            .await;
    store.set_state(|prev| {
        let context = (result.update_context)(&prev.tool_permission_context);
        if Arc::ptr_eq(&context, &prev.tool_permission_context) && result.notification.is_none() {
            return UpdateDecision::Same(());
        }
        let mut next = prev.as_ref().clone();
        next.tool_permission_context = context;
        if let Some(notification) = result.notification {
            // CC appends directly. The general notification helper has separate
            // dedup/promotion behavior and is not this producer's operation.
            Arc::make_mut(&mut next.notifications).queue.push(
                Notification::text(
                    "auto-mode-gate-notification",
                    notification,
                    NotificationPriority::High,
                )
                .with_color(NotificationColor::Warning),
            );
        }
        UpdateDecision::Replace {
            next: Arc::new(next),
            result: (),
        }
    });
}

/// Maps to: CC `bypassPermissionsKillswitch.ts:134-136#resetAutoModeGateCheck`.
pub fn reset_auto_mode_gate_check() {
    AUTO_MODE_CHECK_RAN.store(false, Ordering::SeqCst);
}

/// Maps to: CC `bypassPermissionsKillswitch.ts:59-77#useKickOffCheckAndDisableBypassPermissionsIfNeeded`.
/// L1 A3: source `void` effect work lives on the published process runtime.
pub fn use_kick_off_check_and_disable_bypass_permissions_if_needed(hooks: &mut Hooks) {
    let context =
        crate::state::app_state::use_app_state(hooks, |s| s.tool_permission_context.clone());
    let store = crate::state::app_state::use_app_state_store(hooks);
    hooks.use_effect(
        move || {
            if crate::bootstrap::state::get_is_remote_mode() {
                return;
            }
            if let Some(runtime) = crate::utils::process_runtime::runtime_handle_for_detached_work()
            {
                runtime.spawn(async move {
                    check_and_disable_bypass_permissions_if_needed(&context, &store).await;
                });
            }
        },
        (),
    );
}

/// Maps to: CC `bypassPermissionsKillswitch.ts:138-172#useKickOffCheckAndDisableAutoModeIfNeeded`.
/// L1 useRef is an Arc atomic; model dependencies trigger a fresh check.
pub fn use_kick_off_check_and_disable_auto_mode_if_needed(hooks: &mut Hooks) {
    let dependencies = crate::state::app_state::use_app_state(hooks, |s| {
        (
            s.main_loop_model.clone(),
            s.main_loop_model_for_session.clone(),
            s.fast_mode,
        )
    });
    let fast_mode = dependencies.2;
    let store = crate::state::app_state::use_app_state_store(hooks);
    let first_run = hooks.use_const(|| Arc::new(AtomicBool::new(true))).clone();
    hooks.use_effect(
        move || {
            if crate::bootstrap::state::get_is_remote_mode() {
                return;
            }
            if !first_run.swap(false, Ordering::SeqCst) {
                reset_auto_mode_gate_check();
            }
            let context = store.get().tool_permission_context.clone();
            if let Some(runtime) = crate::utils::process_runtime::runtime_handle_for_detached_work()
            {
                runtime.spawn(async move {
                    check_and_disable_auto_mode_if_needed(&context, &store, Some(fast_mode)).await;
                });
            }
        },
        dependencies,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::permissions::PermissionMode;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[tokio::test]
    async fn bypass_gate_matches_official_run_once_and_fresh_store_transform() {
        // CC :19-49: the input controls whether to check; CURRENT state is modified.
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _telemetry = EnvVarGuard::unset("DISABLE_TELEMETRY");
        let _traffic = EnvVarGuard::unset("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC");
        let _node = EnvVarGuard::unset("NODE_ENV");
        let _override = EnvVarGuard::unset("CLAUDE_INTERNAL_FC_OVERRIDES");
        crate::services::analytics::growthbook::reset_growth_book();
        let mut config = crate::utils::config::GlobalConfig::default();
        config.cached_statsig_gates = Some(std::collections::HashMap::from([(
            "tengu_disable_bypass_permissions_mode".into(),
            true,
        )]));
        crate::utils::config::set_test_global_config(Some(config));
        let stale = ToolPermissionContext {
            mode: PermissionMode::BypassPermissions,
            is_bypass_permissions_mode_available: true,
            ..Default::default()
        };
        let mut state = crate::state::app_state_store::AppState::default();
        state.tool_permission_context = Arc::new(ToolPermissionContext {
            mode: PermissionMode::AcceptEdits,
            is_bypass_permissions_mode_available: true,
            ..Default::default()
        });
        let store = AppStore::new(state, None);
        reset_bypass_permissions_check();
        check_and_disable_bypass_permissions_if_needed(&stale, &store).await;
        assert_eq!(
            store.tool_permission_context().mode,
            PermissionMode::AcceptEdits
        );
        assert!(
            !store
                .tool_permission_context()
                .is_bypass_permissions_mode_available
        );
        let revision = store.revision();
        check_and_disable_bypass_permissions_if_needed(&stale, &store).await;
        assert_eq!(revision, store.revision());
        reset_bypass_permissions_check();
        check_and_disable_bypass_permissions_if_needed(&ToolPermissionContext::default(), &store)
            .await;
        check_and_disable_bypass_permissions_if_needed(&stale, &store).await;
        assert_eq!(
            revision,
            store.revision(),
            "unavailable first call still consumes run-once flag"
        );
        reset_bypass_permissions_check();
        crate::utils::config::set_test_global_config(None);
    }

    #[tokio::test]
    async fn auto_gate_matches_official_notification_queue_and_noop_store_identity() {
        // CC :94-125: unchanged context returns prev; notification appends to queue.
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _override = EnvVarGuard::unset("CLAUDE_INTERNAL_FC_OVERRIDES");
        crate::services::analytics::growthbook::reset_growth_book();
        crate::utils::config::set_test_global_config(Some(Default::default()));
        crate::utils::settings::settings_cache::set_session_settings_cache(
            crate::utils::settings::validation::SettingsWithErrors {
                settings: serde_json::from_value(serde_json::json!({"disableAutoMode":"disable"}))
                    .unwrap(),
                errors: vec![],
                policy_settings: None,
            },
        );
        let mut state = crate::state::app_state_store::AppState::default();
        state.tool_permission_context = Arc::new(ToolPermissionContext {
            is_auto_mode_available: Some(false),
            ..Default::default()
        });
        let store = AppStore::new(state, None);
        reset_auto_mode_gate_check();
        check_and_disable_auto_mode_if_needed(&store.tool_permission_context(), &store, None).await;
        assert_eq!(store.revision(), 0);
        reset_auto_mode_gate_check();
        let stale = ToolPermissionContext {
            mode: PermissionMode::Auto,
            ..Default::default()
        };
        check_and_disable_auto_mode_if_needed(&stale, &store, None).await;
        assert_eq!(
            store.tool_permission_context().mode,
            PermissionMode::Default
        );
        assert_eq!(store.get().notifications.queue.len(), 1);
        let state = store.get();
        let notification = &state.notifications.queue[0];
        assert_eq!(notification.key, "auto-mode-gate-notification");
        assert_eq!(notification.priority, NotificationPriority::High);
        assert_eq!(notification.color, Some(NotificationColor::Warning));
        reset_auto_mode_gate_check();
        crate::utils::config::set_test_global_config(None);
    }
}
