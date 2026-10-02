//! Maps to: CC `utils/autoUpdater.ts` plus the capture/IO phases of the child
//! `checkForUpdates` callbacks (`AutoUpdater.tsx` / `NativeAutoUpdater.tsx` /
//! `PackageManagerAutoUpdater.tsx`).
//!
//! Check/install **control flow** matches Claude Code (guards, channel, maxVersion
//! cap, shouldSkipVersion, install-type branching, 30-minute interval). The
//! `check_for_*` functions follow the CC three-phase protocol (capture → async
//! IO → commit-in-component): they **return outcome values** and never commit —
//! commits happen through callback props / component-local state in the updater
//! child components ([`crate::components::auto_updater`],
//! [`crate::components::native_auto_updater`],
//! [`crate::components::package_manager_auto_updater`]). External network /
//! package-manager / native-download calls are **short-circuited** until
//! Cometix has update infrastructure — enable later via
//! `COMETIX_AUTO_UPDATER_NETWORK=1` (still no-op stubs today).

use crate::utils::config::{is_auto_updater_disabled, load_global_config};
use crate::utils::debug::log_for_debugging;
use crate::utils::doctor_diagnostic::{InstallationType, get_current_installation_type};
use crate::utils::native_installer::package_managers::{PackageManager, get_package_manager};
use crate::utils::settings::load_settings_from_disk;
use std::time::Duration;

/// Official npm package URL (CC `MACRO.PACKAGE_URL`).
pub const OFFICIAL_PACKAGE_URL: &str = "@anthropic-ai/claude-code";

/// Maps to: CC `utils/autoUpdater.ts:35-39` `InstallStatus`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallStatus {
    Success,
    NoPermissions,
    InstallFailed,
    InProgress,
}

impl Default for InstallStatus {
    fn default() -> Self {
        Self::InProgress
    }
}

/// Maps to: CC `utils/autoUpdater.ts:41-45` `AutoUpdaterResult`.
///
/// Producer seam (`notifications`): CC declares `notifications?: string[]` on
/// the type, but no producer in CC 2.1.88 ever populates it — all three child
/// emitters pass only `{version, status}` (`AutoUpdater.tsx:198-201`,
/// `NativeAutoUpdater.tsx:133-136,161-164`). The REPL fan-out effect
/// (`REPL.tsx:1411-1421` ↔ `screens/repl.rs`) is wired on both sides and
/// dormant on both sides; Rust producers keep `Vec::new()` accordingly.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AutoUpdaterResult {
    pub version: Option<String>,
    pub status: InstallStatus,
    pub notifications: Vec<String>,
}

/// Maps to: CC `autoUpdater.ts#MaxVersionConfig`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MaxVersionConfig {
    pub external: Option<String>,
    pub ant: Option<String>,
    pub external_message: Option<String>,
    pub ant_message: Option<String>,
}

/// Maps to: CC release channel (`latest` | `stable`); CC defaults every
/// `autoUpdatesChannel` read with `?? 'latest'`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReleaseChannel {
    #[default]
    Latest,
    Stable,
}

impl ReleaseChannel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Latest => "latest",
            Self::Stable => "stable",
        }
    }

    pub fn from_settings(value: Option<&str>) -> Self {
        match value.map(str::trim) {
            Some("stable") => Self::Stable,
            _ => Self::Latest,
        }
    }
}

/// Gate for real npm/GCS/native download. Always off until facilities exist;
/// opt-in env reserved for future wiring (stubs still return empty).
pub fn auto_updater_network_enabled() -> bool {
    crate::utils::process_env::var("COMETIX_AUTO_UPDATER_NETWORK")
        .is_some_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
}

/// Maps to: CC `MACRO.VERSION` (compile-time running version).
pub fn current_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// Maps to: CC `"production" === 'test' || "production" === 'development'`
/// build-time guard inside each child `checkForUpdates`.
pub(crate) fn is_test_or_dev_env() -> bool {
    cfg!(test)
        || crate::utils::process_env::var("NODE_ENV")
            .is_some_and(|v| matches!(v.as_str(), "test" | "development"))
        || crate::utils::process_env::var("COMETIX_ENV")
            .is_some_and(|v| matches!(v.as_str(), "test" | "development"))
}

/// Maps to: CC `shouldSkipVersion` (settings.minimumVersion gate).
pub fn should_skip_version(target_version: &str) -> bool {
    let settings = load_settings_from_disk().settings;
    let Some(minimum) = settings
        .minimum_version
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    else {
        return false;
    };
    let skip = !version_gte(target_version, minimum);
    if skip {
        log_for_debugging(&format!(
            "Skipping update to {target_version} - below minimumVersion {minimum}"
        ));
    }
    skip
}

/// Maps to: CC `getMaxVersion` — GrowthBook short-circuited → always `None`.
pub async fn get_max_version() -> Option<String> {
    let _ = get_max_version_config().await;
    None
}

/// Maps to: CC `getMaxVersionMessage` — short-circuited → always `None`.
pub async fn get_max_version_message() -> Option<String> {
    let _ = get_max_version_config().await;
    None
}

async fn get_max_version_config() -> MaxVersionConfig {
    // CC: getDynamicConfig_BLOCKS_ON_INIT('tengu_max_version_config').
    // No GrowthBook in Cometix yet.
    MaxVersionConfig::default()
}

/// Maps to: CC `getLatestVersion` (npm view). Short-circuited → `None`.
pub async fn get_latest_version(channel: ReleaseChannel) -> Option<String> {
    let _ = channel;
    if !auto_updater_network_enabled() {
        log_for_debugging(
            "AutoUpdater: getLatestVersion short-circuited (no update network facilities)",
        );
        return None;
    }
    // Future: npm view @anthropic-ai/claude-code@<tag> version
    None
}

/// Maps to: CC `getLatestVersionFromGcs`. Short-circuited → `None`.
pub async fn get_latest_version_from_gcs(channel: ReleaseChannel) -> Option<String> {
    let _ = channel;
    if !auto_updater_network_enabled() {
        log_for_debugging(
            "AutoUpdater: getLatestVersionFromGcs short-circuited (no update network facilities)",
        );
        return None;
    }
    None
}

/// Maps to: CC `installGlobalPackage`. Short-circuited → `install_failed`.
pub async fn install_global_package(_specific_version: Option<&str>) -> InstallStatus {
    if !auto_updater_network_enabled() {
        log_for_debugging(
            "AutoUpdater: installGlobalPackage short-circuited (no update network facilities)",
        );
        return InstallStatus::InstallFailed;
    }
    InstallStatus::InstallFailed
}

/// Maps to: CC localInstaller `installOrUpdateClaudePackage`. Short-circuited.
pub async fn install_or_update_claude_package(_channel: ReleaseChannel) -> InstallStatus {
    if !auto_updater_network_enabled() {
        log_for_debugging(
            "AutoUpdater: installOrUpdateClaudePackage short-circuited (no update network facilities)",
        );
        return InstallStatus::InstallFailed;
    }
    InstallStatus::InstallFailed
}

/// Maps to: CC nativeInstaller `installLatest` outcome (subset).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NativeInstallLatestResult {
    pub lock_failed: bool,
    pub was_updated: bool,
    pub latest_version: Option<String>,
    pub error: Option<String>,
}

/// Maps to: CC `nativeInstaller.installLatest`. Short-circuited → no-op success path.
pub async fn install_latest_native(_channel: ReleaseChannel) -> NativeInstallLatestResult {
    if !auto_updater_network_enabled() {
        log_for_debugging(
            "NativeAutoUpdater: installLatest short-circuited (no update network facilities)",
        );
        return NativeInstallLatestResult::default();
    }
    NativeInstallLatestResult::default()
}

/// Maps to: CC `assertMinVersion` — GrowthBook short-circuited no-op.
pub async fn assert_min_version() {
    if is_test_or_dev_env() {
        return;
    }
    // CC would fetch tengu_version_config and exit if too old.
}

fn auto_updates_channel() -> ReleaseChannel {
    let settings = load_settings_from_disk().settings;
    ReleaseChannel::from_settings(settings.auto_updates_channel.as_deref())
}

/// Cap `latest` by maxVersion kill-switch (CC AutoUpdater / PackageManager paths).
pub async fn apply_max_version_cap(
    current: &str,
    mut latest: Option<String>,
) -> (Option<String>, bool) {
    let Some(max_version) = get_max_version().await else {
        return (latest, false);
    };
    let Some(latest_version) = latest.as_deref() else {
        return (latest, false);
    };
    if !version_gt(latest_version, &max_version) {
        return (latest, false);
    }
    log_for_debugging(&format!(
        "AutoUpdater: maxVersion {max_version} is set, capping update from {latest_version} to {max_version}"
    ));
    if version_gte(current, &max_version) {
        log_for_debugging(&format!(
            "AutoUpdater: current version {current} is already at or above maxVersion {max_version}, skipping update"
        ));
        return (Some(latest_version.to_string()), true);
    }
    latest = Some(max_version);
    (latest, false)
}

/// Capture/IO outcome of the JS updater check (CC three-phase: capture →
/// async IO → commit-in-component; commits live with the caller).
///
/// Maps to: CC `AutoUpdater.tsx:65-110` — env guard, version/channel capture,
/// `getLatestVersion` + maxVersion cap, and the install decision (`:104-110`).
/// `install_target` is `Some(version)` exactly when CC would enter the install
/// branch.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JsUpdateCheckOutcome {
    /// CC `setVersions({ global: currentVersion, ... })`.
    pub global_version: String,
    /// CC `setVersions({ ..., latest: latestVersion })` (post max-version cap;
    /// the uncapped value on the capped-skip early return, matching `:95`).
    pub latest_version: Option<String>,
    pub channel: ReleaseChannel,
    /// `Some` = perform the install for this version (CC `:104-110` decision).
    pub install_target: Option<String>,
}

/// Maps to: CC `AutoUpdater.tsx:65-110` `checkForUpdates` capture/IO phases.
/// Returns `None` when skipped (test/dev environment). The `isUpdatingRef`
/// guard and every commit (`setVersions`, `onChangeIsUpdating`,
/// `onAutoUpdaterResult`) live with the caller — the `AutoUpdater` child's
/// own check loop (`components/auto_updater.rs`).
pub async fn check_for_js_updates() -> Option<JsUpdateCheckOutcome> {
    if is_test_or_dev_env() {
        log_for_debugging("AutoUpdater: Skipping update check in test/dev environment");
        return None;
    }

    let current = current_version();
    let channel = auto_updates_channel();
    let latest = get_latest_version(channel).await;
    let is_disabled = is_auto_updater_disabled();

    let (latest, skip_for_max) = apply_max_version_cap(&current, latest).await;
    let install_target = if skip_for_max {
        None
    } else {
        latest.clone().filter(|latest_version| {
            !is_disabled
                && !version_gte(&current, latest_version)
                && !should_skip_version(latest_version)
        })
    };

    Some(JsUpdateCheckOutcome {
        global_version: current,
        latest_version: latest,
        channel,
        install_target,
    })
}

/// Maps to: CC `AutoUpdater.tsx:111-201` — symlink cleanup, installation-type
/// detection, and the install itself (analytics omitted). Returns `None` on
/// the official no-result early returns (development build, unexpected native
/// installation); the caller mirrors CC by emitting no result for those.
pub async fn perform_js_update_install(channel: ReleaseChannel) -> Option<InstallStatus> {
    let config = load_global_config();
    if config.install_method.as_deref() != Some("native") {
        // CC: removeInstalledSymlink() — no-op stub until native installer lands.
        log_for_debugging("AutoUpdater: removeInstalledSymlink short-circuited");
    }

    let installation_type = get_current_installation_type();
    log_for_debugging(&format!(
        "AutoUpdater: Detected installation type: {installation_type:?}"
    ));

    match installation_type {
        InstallationType::Development => {
            log_for_debugging("AutoUpdater: Cannot auto-update development build");
            None
        }
        InstallationType::NpmLocal => {
            log_for_debugging("AutoUpdater: Using local update method");
            Some(install_or_update_claude_package(channel).await)
        }
        InstallationType::NpmGlobal => {
            log_for_debugging("AutoUpdater: Using global update method");
            Some(install_global_package(None).await)
        }
        InstallationType::Native => {
            log_for_debugging("AutoUpdater: Unexpected native installation in non-native updater");
            None
        }
        _ => {
            log_for_debugging("AutoUpdater: Unknown installation type, falling back to config");
            let is_migrated = config.install_method.as_deref() == Some("local");
            if is_migrated {
                Some(install_or_update_claude_package(channel).await)
            } else {
                Some(install_global_package(None).await)
            }
        }
    }
}

/// Capture/IO outcome of the native updater check.
///
/// Maps to: CC `NativeAutoUpdater.tsx:100-142` — maxVersion probe +
/// `installLatest`. Guards (`isUpdatingRef`, test/dev, disabled) and the
/// `onChangeIsUpdating(true/false)` bracketing stay with the caller, matching
/// CC's placement around this IO.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NativeUpdateCheckOutcome {
    pub current_version: String,
    /// CC `setMaxVersionIssue(msg ?? 'affects your version')` input (`:106-111`).
    pub max_version_issue: Option<String>,
    pub install: NativeInstallLatestResult,
}

/// Maps to: CC `NativeAutoUpdater.tsx:100-142` capture/IO phases (analytics
/// omitted). Commits live with the caller — the `NativeAutoUpdater` child's
/// own check loop (`components/native_auto_updater.rs`).
pub async fn check_for_native_updates() -> NativeUpdateCheckOutcome {
    let current = current_version();
    let channel = auto_updates_channel();

    let mut max_version_issue = None;
    if let Some(max_version) = get_max_version().await {
        if version_gt(&current, &max_version) {
            max_version_issue = Some(
                get_max_version_message()
                    .await
                    .unwrap_or_else(|| "affects your version".to_string()),
            );
        }
    }

    let install = install_latest_native(channel).await;
    NativeUpdateCheckOutcome {
        current_version: current,
        max_version_issue,
        install,
    }
}

/// Capture/IO outcome of the package-manager updater check.
///
/// Maps to: CC `PackageManagerAutoUpdater.tsx:34-81` `checkForUpdates`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PackageManagerUpdateCheckOutcome {
    /// CC `setUpdateAvailable(!!hasUpdate)` (`:65`, `:74`).
    pub update_available: bool,
    /// CC `setPackageManager(pm)` (`:46-50`) — resolved before the GCS fetch,
    /// committed on every non-skipped check (including the capped-skip path).
    pub package_manager: PackageManager,
}

/// Maps to: CC `PackageManagerAutoUpdater.tsx:34-81`. Returns `None` when
/// skipped (test/dev environment, auto-updates disabled — CC returns before
/// any commit on those guards).
pub async fn check_for_package_manager_updates() -> Option<PackageManagerUpdateCheckOutcome> {
    if is_test_or_dev_env() {
        return None;
    }
    if is_auto_updater_disabled() {
        return None;
    }

    // Maps to: CC :46-49 `Promise.all([channel, getPackageManager()])`.
    let channel = auto_updates_channel();
    let package_manager = get_package_manager().await;
    let current = current_version();
    let latest = get_latest_version_from_gcs(channel).await;
    let (latest, skip_for_max) = apply_max_version_cap(&current, latest).await;

    if skip_for_max {
        return Some(PackageManagerUpdateCheckOutcome {
            update_available: false,
            package_manager,
        });
    }

    let has_update = latest
        .as_deref()
        .is_some_and(|latest| !version_gte(&current, latest) && !should_skip_version(latest));

    Some(PackageManagerUpdateCheckOutcome {
        update_available: has_update,
        package_manager,
    })
}

/// CC `useInterval(checkForUpdates, 30 * 60 * 1000)`.
pub const CHECK_INTERVAL: Duration = Duration::from_secs(30 * 60);

/// SemVer-ish compare ignoring `+build` metadata (CC `semver` gte/gt for versions).
pub(crate) fn parse_triplet(value: &str) -> Option<[u64; 3]> {
    let base = value.split('+').next()?.split('-').next()?.trim();
    let mut parts = base.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().unwrap_or(0);
    let patch = parts.next().unwrap_or("0").parse().unwrap_or(0);
    Some([major, minor, patch])
}

pub fn version_gte(a: &str, b: &str) -> bool {
    match (parse_triplet(a), parse_triplet(b)) {
        (Some(left), Some(right)) => left >= right,
        _ => a >= b,
    }
}

pub fn version_gt(a: &str, b: &str) -> bool {
    match (parse_triplet(a), parse_triplet(b)) {
        (Some(left), Some(right)) => left > right,
        _ => a > b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_skip_version_without_minimum_is_false() {
        assert!(!should_skip_version("9.9.9"));
    }

    #[test]
    fn version_compare_matches_semver_triplets() {
        assert!(version_gte("1.2.3", "1.2.3"));
        assert!(version_gte("1.2.4", "1.2.3"));
        assert!(!version_gte("1.2.2", "1.2.3"));
        assert!(version_gt("2.0.0", "1.9.9"));
        assert!(version_gte("1.2.3+sha", "1.2.3"));
    }

    #[test]
    fn network_gate_defaults_off() {
        assert!(!auto_updater_network_enabled());
    }

    #[tokio::test]
    async fn get_latest_version_short_circuits() {
        assert!(get_latest_version(ReleaseChannel::Latest).await.is_none());
        assert!(
            get_latest_version_from_gcs(ReleaseChannel::Stable)
                .await
                .is_none()
        );
        assert_eq!(
            install_global_package(None).await,
            InstallStatus::InstallFailed
        );
    }

    /// Maps to: CC `AutoUpdater.tsx:70-78` test/dev skip — the reshaped check
    /// returns `None` instead of committing anything (no store involved).
    #[tokio::test]
    async fn js_check_returns_none_in_test_env() {
        assert!(check_for_js_updates().await.is_none());
    }

    /// Maps to: CC `PackageManagerAutoUpdater.tsx:35-44` test/dev skip.
    #[tokio::test]
    async fn package_manager_check_returns_none_in_test_env() {
        assert!(check_for_package_manager_updates().await.is_none());
    }
}
