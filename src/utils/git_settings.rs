//! Maps to: CC `utils/gitSettings.ts`.
//!
//! Git-related behaviors that depend on user settings. CC keeps this outside
//! git.ts to avoid a settings dependency cycle; the same separation is kept
//! here so `git.rs` stays settings-free.

/// Maps to: CC `shouldIncludeGitInstructions()`.
pub fn should_include_git_instructions() -> bool {
    if crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_DISABLE_GIT_INSTRUCTIONS")
            .as_deref(),
    ) {
        return false;
    }
    if crate::utils::env_utils::is_env_defined_falsy(
        crate::utils::process_env::var("CLAUDE_CODE_DISABLE_GIT_INSTRUCTIONS")
            .as_deref(),
    ) {
        return true;
    }
    crate::utils::settings::load_settings_from_disk()
        .settings
        .include_git_instructions
        .unwrap_or(true)
}
