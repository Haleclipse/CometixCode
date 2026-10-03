//! Maps to: CC `components/AutoUpdaterWrapper.tsx`.
//!
//! Detects the installation type in a mount effect (wrapper-local flags,
//! `None` until known — CC `useNativeInstaller`/`isPackageManager`, `:28-58`)
//! and chooses the PackageManager/Native/JS updater child (`:61-89`),
//! forwarding exactly the six CC props. Slice 3b: each child owns its own
//! display state and check loop (CC parity); the wrapper owns nothing but the
//! probe flags and renderer selection.

use crate::components::auto_updater::AutoUpdater;
use crate::components::native_auto_updater::NativeAutoUpdater;
use crate::components::package_manager_auto_updater::PackageManagerAutoUpdater;
use crate::utils::auto_updater::{AutoUpdaterResult, InstallStatus};
use crate::utils::config::is_auto_updater_disabled;
use crate::utils::debug::log_for_debugging;
use crate::utils::doctor_diagnostic::{InstallationType, get_current_installation_type};
use iocraft::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutoUpdaterRenderer {
    Js,
    Native,
    PackageManager,
}

/// Maps to: CC `AutoUpdaterWrapper.tsx:11-18` Props — exactly the six CC
/// fields (identical shape for the wrapper and all three children).
#[derive(Default, Props)]
pub struct AutoUpdaterWrapperProps {
    /// Maps to: CC `isUpdating` (PromptInput-owned `isAutoUpdating`).
    pub is_updating: bool,
    /// Maps to: CC `onChangeIsUpdating` (PromptInput `setIsAutoUpdating`).
    pub on_change_is_updating: Handler<bool>,
    /// Maps to: CC `onAutoUpdaterResult` (REPL `setAutoUpdaterResult`).
    pub on_auto_updater_result: Handler<AutoUpdaterResult>,
    /// Maps to: CC `autoUpdaterResult` (REPL-owned state).
    pub auto_updater_result: Option<AutoUpdaterResult>,
    pub show_success_message: bool,
    pub verbose: bool,
}

/// Maps to: CC `AutoUpdaterWrapper.tsx:61-78` — `null` until both flags are
/// known, then PackageManager > Native > Js.
pub fn select_auto_updater_renderer(
    use_native_installer: Option<bool>,
    is_package_manager: Option<bool>,
) -> Option<AutoUpdaterRenderer> {
    let use_native_installer = use_native_installer?;
    let is_package_manager = is_package_manager?;
    if is_package_manager {
        Some(AutoUpdaterRenderer::PackageManager)
    } else if use_native_installer {
        Some(AutoUpdaterRenderer::Native)
    } else {
        Some(AutoUpdaterRenderer::Js)
    }
}

/// Maps to: CC `AutoUpdaterWrapper.tsx:35-58` `checkInstallation`.
///
/// Returns `None` when auto-updates are disabled: CC (`:39-47`) skips the
/// potentially slow detection entirely, leaving both flags `null` so no
/// updater child ever mounts (nor any child check loop starts).
pub fn probe_installation_type() -> Option<InstallationType> {
    if is_auto_updater_disabled() {
        log_for_debugging("AutoUpdaterWrapper: Skipping detection, auto-updates disabled");
        return None;
    }
    let installation_type = get_current_installation_type();
    log_for_debugging(&format!(
        "AutoUpdaterWrapper: Installation type: {installation_type:?}"
    ));
    Some(installation_type)
}

#[component]
pub fn AutoUpdaterWrapper(
    props: &AutoUpdaterWrapperProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    // Maps to: CC AutoUpdaterWrapper.tsx:28-33 — `useState<boolean | null>(null)`
    // for both flags; the mount probe below fills them in.
    let use_native_installer = hooks.use_state(|| Option::<bool>::None);
    let is_package_manager = hooks.use_state(|| Option::<bool>::None);

    // Maps to: CC AutoUpdaterWrapper.tsx:35-58 mount effect `checkInstallation`.
    // Skipped in unit tests so canvases stay deterministic.
    hooks.use_future({
        let mut use_native_installer = use_native_installer;
        let mut is_package_manager = is_package_manager;
        async move {
            if cfg!(test) {
                return;
            }
            let Some(installation_type) = probe_installation_type() else {
                return;
            };
            use_native_installer.set(Some(installation_type == InstallationType::Native));
            is_package_manager.set(Some(installation_type == InstallationType::PackageManager));
        }
    });

    let renderer =
        select_auto_updater_renderer(use_native_installer.get(), is_package_manager.get());

    // Maps to: CC AutoUpdaterWrapper.tsx:60-89 — render nothing until the
    // installation type is known, then forward the six CC props unchanged to
    // the selected child (CC passes the identical prop set to all three).
    let child = match renderer {
        None => None,
        Some(AutoUpdaterRenderer::PackageManager) => Some(
            element! {
                PackageManagerAutoUpdater(
                    is_updating: props.is_updating,
                    on_change_is_updating: props.on_change_is_updating.clone(),
                    on_auto_updater_result: props.on_auto_updater_result.clone(),
                    auto_updater_result: props.auto_updater_result.clone(),
                    show_success_message: props.show_success_message,
                    verbose: props.verbose,
                )
            }
            .into_any(),
        ),
        Some(AutoUpdaterRenderer::Native) => Some(
            element! {
                NativeAutoUpdater(
                    is_updating: props.is_updating,
                    on_change_is_updating: props.on_change_is_updating.clone(),
                    on_auto_updater_result: props.on_auto_updater_result.clone(),
                    auto_updater_result: props.auto_updater_result.clone(),
                    show_success_message: props.show_success_message,
                    verbose: props.verbose,
                )
            }
            .into_any(),
        ),
        Some(AutoUpdaterRenderer::Js) => Some(
            element! {
                AutoUpdater(
                    is_updating: props.is_updating,
                    on_change_is_updating: props.on_change_is_updating.clone(),
                    on_auto_updater_result: props.on_auto_updater_result.clone(),
                    auto_updater_result: props.auto_updater_result.clone(),
                    show_success_message: props.show_success_message,
                    verbose: props.verbose,
                )
            }
            .into_any(),
        ),
    };

    element! {
        View {
            #(child)
        }
    }
}

// `should_show_auto_updater` is defined by its CC owner, the Notifications
// consumer (`prompt_input/notifications.rs`; CC Notifications.tsx:119-122).
use crate::components::prompt_input::notifications::should_show_auto_updater;

/// Footer height for `<AutoUpdaterWrapper />` after the Notifications gate.
///
/// Maps to: CC Notifications.tsx:119-122 `shouldShowAutoUpdater` + the child
/// render bodies (`AutoUpdater.tsx:226-263`, `NativeAutoUpdater.tsx:196-230`,
/// `PackageManagerAutoUpdater.tsx:107-118`) — each child renders a single
/// horizontal row, so the count is 0 or 1. Child-local display state
/// (versions, maxVersionIssue, updateAvailable) is unreachable from height
/// callers; the count approximates the as-executed row content with the
/// owner-threaded values plus AppState verbose:
/// - `is_updating`: "Auto-updating…"/"Checking for updates" rows. The gate
///   also needs child-local versions, but `true` windows are yield-free
///   (update IO short-circuited), so no frame ever renders mid-window.
/// - failure statuses with a result version: the failure row renders.
/// - success/in-progress results render an empty row (height 0) unless
///   verbose: the success copy never commits as executed
///   (`useUpdateNotification` committed-null — see
///   hooks/use_update_notification.rs), and verbose adds the versions row
///   whenever the child gate passes.
/// Blind spots that cannot produce lasting rows today: native
/// maxVersionIssue-only row (`get_max_version` → `None`), the ant-only
/// known-issue row (external-build DCE), and the package-manager row
/// (`get_latest_version_from_gcs` → `None`) — revisit when real update IO
/// lands.
///
/// `ide_visible` is caller-derived from the REPL-owned `ideSelection` prop
/// plus the mcp-derived connected status (CC Notifications.tsx:113-121 —
/// props, not AppState), following the tokenUsage/auto-updater threading
/// precedent.
pub fn auto_update_hint_row_count_from_app(
    state: &crate::state::app_state_store::AppState,
    auto_updater_result: Option<&AutoUpdaterResult>,
    is_auto_updating: bool,
    ide_visible: bool,
) -> usize {
    if !should_show_auto_updater(
        ide_visible,
        is_auto_updating,
        auto_updater_result.map(|result| result.status),
    ) {
        return 0;
    }

    let has_result_version = auto_updater_result
        .and_then(|result| result.version.as_deref())
        .is_some();
    let failure_status = matches!(
        auto_updater_result.map(|result| result.status),
        Some(InstallStatus::InstallFailed) | Some(InstallStatus::NoPermissions)
    );
    usize::from(is_auto_updating || (has_result_version && (state.verbose || failure_status)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};
    use crate::utils::theme;

    #[test]
    fn auto_updater_wrapper_selection_matches_official_branches() {
        assert_eq!(select_auto_updater_renderer(None, Some(false)), None);
        assert_eq!(select_auto_updater_renderer(Some(false), None), None);
        assert_eq!(
            select_auto_updater_renderer(Some(false), Some(true)),
            Some(AutoUpdaterRenderer::PackageManager)
        );
        assert_eq!(
            select_auto_updater_renderer(Some(true), Some(false)),
            Some(AutoUpdaterRenderer::Native)
        );
        assert_eq!(
            select_auto_updater_renderer(Some(false), Some(false)),
            Some(AutoUpdaterRenderer::Js)
        );
    }

    /// Maps to: CC AutoUpdaterWrapper.tsx:39-47 — with auto-updates disabled
    /// the installation probe is skipped entirely, the flags stay `null`, and
    /// no updater child is ever selected.
    #[test]
    fn installer_probe_skipped_when_auto_updates_disabled_matches_official() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _disabled = EnvVarGuard::set("DISABLE_AUTOUPDATER", "1");
        let probed = probe_installation_type();
        assert!(probed.is_none(), "disabled updater must skip detection");
        assert_eq!(select_auto_updater_renderer(None, None), None);
    }

    /// Maps to: CC AutoUpdaterWrapper.tsx:61-63 — nothing renders until both
    /// installer flags resolve (structure check for the 3b split: the wrapper
    /// owns only the probe flags; it holds no child display state that could
    /// produce output on its own).
    #[test]
    fn auto_updater_wrapper_renders_nothing_until_flags_resolve() {
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                AutoUpdaterWrapper
            }
        }
        .render(Some(120))
        .to_string();
        assert!(text.trim().is_empty(), "canvas=\n{text}");
    }
}
