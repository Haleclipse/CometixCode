//! The workspace-trust hook gate.
//! Maps to: CC `utils/hooks.ts:286-300` `shouldSkipHookDueToTrust()`.
//!
//! All hooks require workspace trust in interactive mode.
//!
//! Ownership: this file is a port-side home, not a CC one — CC has no
//! `utils/hooks/security.ts` (`ls ../rebuild/src/utils/hooks/ | rg -i secur`
//! matches nothing), and its one resident is declared in `utils/hooks.ts`,
//! which this port maps to `services/hooks/mod.rs`. Booked as a relocation
//! rather than done here, because `mod.rs` is the shared hook entry.

/// Whether to skip hook execution due to missing workspace trust.
/// Maps to: CC `shouldSkipHookDueToTrust()` (hooks.ts:286-300).
///
/// In non-interactive (SDK) mode, trust is implicit → skip = false.
/// In interactive mode, skip if trust dialog has not been accepted.
pub fn should_skip_hook_due_to_trust(workspace_trusted: bool, is_non_interactive: bool) -> bool {
    if is_non_interactive {
        return false;
    }
    !workspace_trusted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trust_skips_in_interactive_untrusted() {
        assert!(should_skip_hook_due_to_trust(false, false));
    }

    #[test]
    fn trust_allows_in_interactive_trusted() {
        assert!(!should_skip_hook_due_to_trust(true, false));
    }

    #[test]
    fn trust_allows_in_non_interactive() {
        assert!(!should_skip_hook_due_to_trust(false, true));
    }
}
