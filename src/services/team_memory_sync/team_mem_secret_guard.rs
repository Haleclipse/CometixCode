//! Write-time secret guard for team-memory files.
//!
//! Maps to: CC `services/teamMemorySync/teamMemSecretGuard.ts`.

use std::path::Path;

/// Maps to CC `checkTeamMemSecrets(filePath, content)`.
#[cfg(feature = "anthropic_internal")]
pub fn check_team_mem_secrets(file_path: &Path, content: &str) -> Result<(), String> {
    if !crate::memdir::team_mem_paths::is_team_mem_path(file_path) {
        return Ok(());
    }
    let matches = super::secret_scanner::scan_for_secrets(content);
    if matches.is_empty() {
        return Ok(());
    }
    let labels = matches
        .iter()
        .map(|secret| secret.label.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    Err(format!(
        "Content contains potential secrets ({labels}) and cannot be written to team memory. Team memory is shared with all repository collaborators. Remove the sensitive content and try again."
    ))
}

#[cfg(not(feature = "anthropic_internal"))]
pub fn check_team_mem_secrets(_file_path: &Path, _content: &str) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "anthropic_internal")]
    use crate::utils::test_env::TEST_ENV_LOCK;

    #[test]
    fn ordinary_project_files_are_not_scanned() {
        assert!(
            check_team_mem_secrets(
                Path::new("/tmp/not-team-memory.md"),
                &format!("ghp_{}", "a".repeat(36)),
            )
            .is_ok()
        );
    }

    #[cfg(feature = "anthropic_internal")]
    #[test]
    fn internal_team_memory_write_is_blocked_with_official_copy() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let path = crate::memdir::team_mem_paths::get_team_mem_path().join("shared.md");
        let content = format!("safe\nghp_{}", "a".repeat(36));
        let error = check_team_mem_secrets(&path, &content).unwrap_err();
        assert_eq!(
            error,
            "Content contains potential secrets (GitHub PAT) and cannot be written to team memory. Team memory is shared with all repository collaborators. Remove the sensitive content and try again."
        );
    }
}
