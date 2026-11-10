//! Maps to: CC `constants/system.ts`.
//!
//! Critical system constants extracted to break circular dependencies
//! (CC file header). Used by side queries and main API attribution.

const DEFAULT_PREFIX: &str = "You are nyan, an AI assistant.";
const AGENT_SDK_CLAUDE_CODE_PRESET_PREFIX: &str =
    "You are nyan, an AI assistant running in a non-interactive session.";
const AGENT_SDK_PREFIX: &str = "You are a nyan agent.";

/// Maps to: CC `constants/system.ts:14-18` `CLI_SYSPROMPT_PREFIX_VALUES`.
const CLI_SYSPROMPT_PREFIX_VALUES: [&str; 3] = [
    DEFAULT_PREFIX,
    AGENT_SDK_CLAUDE_CODE_PRESET_PREFIX,
    AGENT_SDK_PREFIX,
];

/// Maps to: CC `CLISyspromptPrefix` / values in `CLI_SYSPROMPT_PREFIXES`.
pub type CliSyspromptPrefix = &'static str;

/// Maps to: CC `constants/system.ts:26-28` `CLI_SYSPROMPT_PREFIXES`.
///
/// All possible CLI sysprompt prefix values, used by `splitSysPromptPrefix`
/// (`utils/api.rs`) to identify prefix blocks by content rather than position.
pub static CLI_SYSPROMPT_PREFIXES: std::sync::LazyLock<std::collections::HashSet<&'static str>> =
    std::sync::LazyLock::new(|| CLI_SYSPROMPT_PREFIX_VALUES.into_iter().collect());

/// Maps to: CC `getCLISyspromptPrefix(options?)`.
///
/// @nyan: all session modes identify as nyan, without vendor or SDK claims.
/// - Non-interactive + append system prompt: non-interactive assistant prefix
/// - Non-interactive: agent prefix
/// - Interactive: DEFAULT_PREFIX
pub fn get_cli_sysprompt_prefix(
    is_non_interactive: bool,
    has_append_system_prompt: bool,
) -> CliSyspromptPrefix {
    if is_non_interactive {
        if has_append_system_prompt {
            return AGENT_SDK_CLAUDE_CODE_PRESET_PREFIX;
        }
        return AGENT_SDK_PREFIX;
    }
    DEFAULT_PREFIX
}

/// Check if attribution header is enabled.
/// Maps to: CC `isAttributionHeaderEnabled()` — env falsy gate, then
/// `getFeatureValue_CACHED_MAY_BE_STALE('tengu_attribution_header', true)`.
/// The GrowthBook read is the source-controlled
/// `FeatureFlag::AttributionHeader` switch, not a live GrowthBook client.
///
/// @nyan offset: default `NYAN_ATTRIBUTION_HEADER` to `0` when unset
/// so the official gate skips assembling the header on production requests.
fn is_attribution_header_enabled() -> bool {
    let value = crate::utils::process_env::var("NYAN_ATTRIBUTION_HEADER")
        .unwrap_or_else(|| "0".to_string());
    if crate::utils::env_utils::is_env_defined_falsy(Some(&value)) {
        return false;
    }
    crate::utils::feature_flags::feature_enabled(
        crate::utils::feature_flags::FeatureFlag::AttributionHeader,
    )
}

/// Get attribution header for API requests.
/// Returns a header string with `cc_version` (including fingerprint) and
/// `cc_entrypoint`. Empty string when disabled.
///
/// Maps to: CC `getAttributionHeader(fingerprint)`.
///
/// Format:
/// `x-anthropic-billing-header: cc_version={VERSION}.{fp}; cc_entrypoint={ep};`
///
/// Note: CC may also inject `cch=00000` (NATIVE_CLIENT_ATTESTATION, Bun-only)
/// and `cc_workload=...` (turn-scoped QoS). Those are not yet ported.
pub fn get_attribution_header(fingerprint: &str) -> String {
    if !is_attribution_header_enabled() {
        return String::new();
    }
    // Maps to: CC `const version = \`${MACRO.VERSION}.${fingerprint}\``
    let version = format!("{}.{}", crate::constants::product::VERSION, fingerprint);
    // Maps to: CC `process.env.NYAN_ENTRYPOINT ?? 'unknown'`
    let entrypoint =
        crate::utils::process_env::var("NYAN_ENTRYPOINT").unwrap_or_else(|| "unknown".to_string());
    // cch=00000 placeholder is Bun-native attestation — omitted in Nyan.
    format!("x-anthropic-billing-header: cc_version={version}; cc_entrypoint={entrypoint};")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[test]
    fn cli_sysprompt_prefix_interactive_is_default() {
        assert_eq!(
            get_cli_sysprompt_prefix(false, false),
            "You are nyan, an AI assistant."
        );
    }

    #[test]
    fn cli_sysprompt_prefix_print_mode_uses_nyan_identity() {
        let _env = TEST_ENV_LOCK.lock().unwrap();
        // CC `getCLISyspromptPrefix({ isNonInteractive: true })` — `-p` / SDK.
        assert_eq!(
            get_cli_sysprompt_prefix(true, false),
            "You are a nyan agent."
        );
        // CC `hasAppendSystemPrompt: true` — `-p --append-system-prompt`.
        assert_eq!(
            get_cli_sysprompt_prefix(true, true),
            "You are nyan, an AI assistant running in a non-interactive session."
        );
    }

    #[test]
    fn cli_sysprompt_prefixes_contains_every_nyan_variant() {
        assert!(CLI_SYSPROMPT_PREFIXES.contains(get_cli_sysprompt_prefix(false, false)));
        assert!(CLI_SYSPROMPT_PREFIXES.contains(get_cli_sysprompt_prefix(false, true)));
        assert!(CLI_SYSPROMPT_PREFIXES.contains(get_cli_sysprompt_prefix(true, false)));
        assert!(CLI_SYSPROMPT_PREFIXES.contains(get_cli_sysprompt_prefix(true, true)));
        assert_eq!(CLI_SYSPROMPT_PREFIXES.len(), 3);
        assert!(!CLI_SYSPROMPT_PREFIXES.contains("You are Claude Code."));
        for prefix in CLI_SYSPROMPT_PREFIXES.iter() {
            assert!(prefix.contains("nyan"));
            assert!(!prefix.contains("Claude"));
            assert!(!prefix.contains("Anthropic"));
            assert!(!prefix.contains("SDK"));
        }
    }

    #[test]
    fn attribution_header_includes_fingerprint_when_enabled() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("NYAN_ATTRIBUTION_HEADER", "1");
        let header = get_attribution_header("abc");
        assert!(header.contains("cc_version="), "header={header:?}");
        assert!(header.contains(".abc"));
        assert!(header.contains("cc_entrypoint="));
    }

    #[test]
    fn attribution_header_defaults_to_disabled_when_env_unset() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("NYAN_ATTRIBUTION_HEADER");
        assert!(get_attribution_header("abc").is_empty());
    }
}
