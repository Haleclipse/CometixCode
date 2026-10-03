//! Maps to: CC `utils/localInstaller.ts` (partial port).
//!
//! Ported: `getLocalInstallDir` (`:19-21`) and `localInstallationExists`
//! (`:144-151`) — the pure existence probe the `AutoUpdater` child uses for its
//! `hasLocalInstall` state (CC `AutoUpdater.tsx:53-55`). The install/update
//! actions (`ensureLocalPackageEnvironment`, `installOrUpdateClaudePackage`,
//! wrapper-script setup) remain unported; their control-flow stand-ins are
//! short-circuited in [`crate::utils::auto_updater`].
//!
//! Pre-existing seam: `utils/doctor_diagnostic.rs` keeps a private, coarser
//! probe (checks only that the `~/.claude/local` directory exists) where CC
//! doctorDiagnostic imports this util; unify when the doctor slice is ported.

use std::path::PathBuf;

/// Maps to: CC `localInstaller.ts:19-21` `getLocalInstallDir` —
/// `join(getClaudeConfigHomeDir(), 'local')`, evaluated lazily so
/// `CLAUDE_CONFIG_DIR` set after process start is respected.
pub fn get_local_install_dir() -> PathBuf {
    crate::utils::env_utils::get_claude_config_home_dir().join("local")
}

/// Maps to: CC `localInstaller.ts:144-151` `localInstallationExists` —
/// pure existence probe of `<localInstallDir>/node_modules/.bin/claude`;
/// callers use this to choose update path / UI hints.
pub fn local_installation_exists() -> bool {
    get_local_install_dir()
        .join("node_modules")
        .join(".bin")
        .join("claude")
        .exists()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    /// Maps to: CC `localInstaller.ts:144-151` — the probe is an independent
    /// filesystem check of the local npm bin shim, not an installation-type
    /// derivation (slice-3b fix for the `hasLocalInstall` source).
    #[test]
    fn local_installation_exists_probes_node_modules_bin_claude() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = std::env::temp_dir().join(format!(
            "cometix-local-installer-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root);

        assert!(
            !local_installation_exists(),
            "missing local install dir must probe false"
        );

        let bin_dir = root.join("local").join("node_modules").join(".bin");
        std::fs::create_dir_all(&bin_dir).expect("create local install bin dir");
        std::fs::write(bin_dir.join("claude"), b"#!/bin/sh\n").expect("write claude shim");
        assert!(
            local_installation_exists(),
            "existing bin shim must probe true"
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
