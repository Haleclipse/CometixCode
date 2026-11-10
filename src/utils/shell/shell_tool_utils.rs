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
        crate::utils::process_env::var("NYAN_USE_POWERSHELL_TOOL").as_deref(),
    )
}

/// Maps to: CC `utils/shell/shellToolUtils.ts` `isPowerShellToolEnabled()`.
pub fn is_powershell_tool_enabled_for_platform(platform: &str, env_value: Option<&str>) -> bool {
    platform == "windows" && crate::utils::env_utils::is_env_truthy(env_value)
}
