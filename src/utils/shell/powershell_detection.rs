//! Maps to: CC `utils/shell/powershellDetection.ts`.
//!
//! Resolves the PowerShell executable used by the AST parse bridge. Prefers
//! `pwsh` (PowerShell Core 7+) and falls back to `powershell` (5.1).

use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

static CACHED_POWERSHELL_PATH: LazyLock<Mutex<Option<Option<String>>>> =
    LazyLock::new(|| Mutex::new(None));

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        return metadata.permissions().mode() & 0o111 != 0;
    }
    #[cfg(not(unix))]
    true
}

/// Maps to: CC `utils/which.ts#which`, scoped to the PowerShell lookup.
fn which(executable: &str) -> Option<PathBuf> {
    let path = crate::utils::process_env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|directory| {
            if cfg!(windows) {
                directory.join(format!("{executable}.exe"))
            } else {
                directory.join(executable)
            }
        })
        .find(|candidate| is_executable_file(candidate))
}

/// Maps to: CC `powershellDetection.ts:5-11#probePath`.
fn probe_path(path: &str) -> Option<String> {
    let path = Path::new(path);
    path.metadata()
        .ok()
        .filter(|metadata| metadata.is_file())
        .map(|_| path.display().to_string())
}

fn real_path_or_self(path: &str) -> String {
    Path::new(path)
        .canonicalize()
        .map(|resolved| resolved.display().to_string())
        .unwrap_or_else(|_| path.to_string())
}

/// Maps to: CC `powershellDetection.ts:24-57#findPowerShell`.
///
/// On Linux a snap launcher can hang inside subprocesses while snapd
/// initializes confinement, so a `/snap/` resolution is redirected to the
/// distro package location when one exists.
pub fn find_powershell() -> Option<String> {
    if let Some(pwsh_path) = which("pwsh") {
        let pwsh_path = pwsh_path.display().to_string();
        if cfg!(target_os = "linux") {
            let resolved = real_path_or_self(&pwsh_path);
            if pwsh_path.starts_with("/snap/") || resolved.starts_with("/snap/") {
                if let Some(direct) = probe_path("/opt/microsoft/powershell/7/pwsh")
                    .or_else(|| probe_path("/usr/bin/pwsh"))
                {
                    let direct_resolved = real_path_or_self(&direct);
                    if !direct.starts_with("/snap/") && !direct_resolved.starts_with("/snap/") {
                        return Some(direct);
                    }
                }
            }
        }
        return Some(pwsh_path);
    }

    if let Some(powershell_path) = which("powershell") {
        return Some(powershell_path.display().to_string());
    }

    None
}

/// Maps to: CC `powershellDetection.ts:65-70#getCachedPowerShellPath`.
pub fn get_cached_powershell_path() -> Option<String> {
    let mut cached = CACHED_POWERSHELL_PATH
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cached.get_or_insert_with(find_powershell).clone()
}

/// Maps to: CC `powershellDetection.ts:72#PowerShellEdition`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerShellEdition {
    Core,
    Desktop,
}

/// Maps to: CC `powershellDetection.ts:87-100#getPowerShellEdition`.
pub fn get_powershell_edition() -> Option<PowerShellEdition> {
    let path = get_cached_powershell_path()?;
    let base = path
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(&path)
        .to_lowercase();
    let base = base.strip_suffix(".exe").unwrap_or(&base);
    Some(if base == "pwsh" {
        PowerShellEdition::Core
    } else {
        PowerShellEdition::Desktop
    })
}

/// Maps to: CC `powershellDetection.ts:105-107#resetPowerShellCache`.
pub fn reset_powershell_cache() {
    *CACHED_POWERSHELL_PATH
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_lookup_agrees_with_a_fresh_probe() {
        reset_powershell_cache();
        let fresh = find_powershell();
        assert_eq!(get_cached_powershell_path(), fresh);
        assert_eq!(get_cached_powershell_path(), fresh);
    }

    #[test]
    fn edition_presence_tracks_executable_availability() {
        reset_powershell_cache();
        assert_eq!(
            get_powershell_edition().is_some(),
            get_cached_powershell_path().is_some()
        );
    }
}
