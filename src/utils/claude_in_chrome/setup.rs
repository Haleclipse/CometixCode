//! Maps to: CC `utils/claudeInChrome/setup.ts` readonly enablement helpers.
//!
//! Full MCP/browser launch side effects remain unported; this module only
//! derives whether Claude-in-Chrome should be offered (CLI / env / config).

use crate::utils::config::GlobalConfig;

/// Maps to CC commander `--chrome` / `--no-chrome` (last explicit wins).
pub fn chrome_flag_from_args(args: impl IntoIterator<Item = String>) -> Option<bool> {
    args.into_iter().fold(None, |flag, arg| match arg.as_str() {
        "--chrome" => Some(true),
        "--no-chrome" => Some(false),
        _ => flag,
    })
}

/// Maps to CC `shouldEnableClaudeInChrome` (read-only; no MCP setup side effects).
pub fn should_enable_claude_in_chrome_from_readonly_runtime(
    global_config: &GlobalConfig,
    args: impl IntoIterator<Item = String>,
    get_env: &impl Fn(&str) -> Option<String>,
) -> bool {
    if let Some(value) = chrome_flag_from_args(args) {
        return value;
    }
    let cfc = get_env("CLAUDE_CODE_ENABLE_CFC");
    if crate::utils::env_utils::is_env_truthy(cfc.as_deref()) {
        return true;
    }
    if crate::utils::env_utils::is_env_defined_falsy(cfc.as_deref()) {
        return false;
    }
    global_config
        .claude_in_chrome_default_enabled
        .unwrap_or(false)
}
