//! Maps to CC `utils/shell/shellToolUtils.ts`.
//! Bash read-only validation lives in `tools/bash_tool/read_only_validation.rs`
//! (↔ CC `tools/BashTool/readOnlyValidation.ts`), not here — CC's
//! shellToolUtils only exports the shell tool names and the PowerShell
//! enablement gate.

/// Maps to: CC `utils/shell/shellToolUtils.ts` `SHELL_TOOL_NAMES`.
/// Values match `BASH_TOOL_NAME` / `POWERSHELL_TOOL_NAME`; inlined to avoid
/// a `tools` ↔ `utils::shell` import cycle.
pub const SHELL_TOOL_NAMES: &[&str] = &["Bash", "PowerShell"];

pub fn is_powershell_tool_enabled() -> bool {
    is_powershell_tool_enabled_for_platform(
        std::env::consts::OS,
        crate::utils::build_profile::build_audience(),
        crate::utils::process_env::var("CLAUDE_CODE_USE_POWERSHELL_TOOL")
            .as_deref(),
    )
}

/// Maps to: CC `utils/shell/shellToolUtils.ts` `isPowerShellToolEnabled()`.
pub fn is_powershell_tool_enabled_for_platform(
    platform: &str,
    audience: crate::utils::build_profile::BuildAudience,
    env_value: Option<&str>,
) -> bool {
    if platform != "windows" {
        return false;
    }
    if crate::utils::build_profile::audience_has_internal_capability(
        audience,
        crate::utils::build_profile::InternalCapability::Tools,
    ) {
        !matches!(
            env_value,
            Some("0" | "false" | "FALSE" | "no" | "NO" | "off" | "OFF")
        )
    } else {
        matches!(
            env_value,
            Some("1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON")
        )
    }
}
