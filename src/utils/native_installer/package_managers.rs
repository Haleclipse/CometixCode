//! Maps to: CC `utils/nativeInstaller/packageManagers.ts` (partial port).
//!
//! Ported: the `PackageManager` union (`:11-20`) and `getPackageManager`
//! (`:302-336`) — the source of the `PackageManagerAutoUpdater` child's
//! `packageManager` state (CC `PackageManagerAutoUpdater.tsx:46-50`).
//!
//! Detection seam: CC's detector chain (detectHomebrew → winget → mise → asdf
//! → pacman → apk → deb → rpm) shells out to package managers and reads
//! `/etc/os-release`; Cometix has no native-installer distribution yet, so
//! detection is driven by the `COMETIX_PACKAGE_MANAGER` env var (same seam
//! `utils/doctor_diagnostic.rs` uses for its diagnostic `package_manager`
//! field). CC memoizes `getPackageManager` because detection spawns processes;
//! the env read is cheap, so memoization is deferred until real detection
//! lands.

/// Maps to: CC `packageManagers.ts:11-20` `PackageManager` union.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PackageManager {
    Homebrew,
    Winget,
    Pacman,
    Deb,
    Rpm,
    Apk,
    Mise,
    Asdf,
    #[default]
    Unknown,
}

impl PackageManager {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Homebrew => "homebrew",
            Self::Winget => "winget",
            Self::Pacman => "pacman",
            Self::Deb => "deb",
            Self::Rpm => "rpm",
            Self::Apk => "apk",
            Self::Mise => "mise",
            Self::Asdf => "asdf",
            Self::Unknown => "unknown",
        }
    }

    /// Parse the CC string union value (used by the env detection seam).
    pub fn from_value(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "homebrew" => Self::Homebrew,
            "winget" => Self::Winget,
            "pacman" => Self::Pacman,
            "deb" => Self::Deb,
            "rpm" => Self::Rpm,
            "apk" => Self::Apk,
            "mise" => Self::Mise,
            "asdf" => Self::Asdf,
            _ => Self::Unknown,
        }
    }
}

/// Maps to: CC `packageManagers.ts:302-336` `getPackageManager` — detect which
/// package manager installed the binary, `unknown` when none is detected.
/// Detection seam (see module docs): reads `COMETIX_PACKAGE_MANAGER` instead
/// of running CC's spawn-based detector chain.
pub async fn get_package_manager() -> PackageManager {
    match crate::utils::process_env::var("COMETIX_PACKAGE_MANAGER") {
        Some(value) => PackageManager::from_value(&value),
        None => PackageManager::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[test]
    fn package_manager_from_value_matches_official_union() {
        assert_eq!(
            PackageManager::from_value("homebrew"),
            PackageManager::Homebrew
        );
        assert_eq!(PackageManager::from_value("winget"), PackageManager::Winget);
        assert_eq!(PackageManager::from_value("pacman"), PackageManager::Pacman);
        assert_eq!(PackageManager::from_value("deb"), PackageManager::Deb);
        assert_eq!(PackageManager::from_value("rpm"), PackageManager::Rpm);
        assert_eq!(PackageManager::from_value("apk"), PackageManager::Apk);
        assert_eq!(PackageManager::from_value("mise"), PackageManager::Mise);
        assert_eq!(PackageManager::from_value("asdf"), PackageManager::Asdf);
        assert_eq!(
            PackageManager::from_value("chocolatey"),
            PackageManager::Unknown
        );
    }

    /// Maps to: CC `PackageManagerAutoUpdater.tsx:46-50` `setPackageManager(pm)`
    /// source — `get_package_manager` resolves the value the PM child commits.
    #[tokio::test]
    async fn get_package_manager_resolves_env_seam_and_defaults_unknown() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _package_manager = EnvVarGuard::unset("COMETIX_PACKAGE_MANAGER");

        assert_eq!(get_package_manager().await, PackageManager::Unknown);

        crate::utils::process_env::set("COMETIX_PACKAGE_MANAGER", "winget");
        assert_eq!(get_package_manager().await, PackageManager::Winget);
    }
}
