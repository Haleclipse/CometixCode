//! Maps to: CC `bridge/bridgeEnabled.ts` — the runtime entitlement check
//! subset. The blocking/diagnostic variants (`isBridgeEnabledBlocking`,
//! `getBridgeDisabledReason`) and the v2/env-less/CCR-mirror gates stay with
//! the unported bridge runtime (see MODULE_MAP `bridge/*` rows).

/// Maps to: CC `bridge/bridgeEnabled.ts:28-36` `isBridgeEnabled`.
///
/// Remote Control authenticates to CCR with the claude.ai OAuth token. Anthropic
/// is API-key only here, so there is no such token and the bridge is never
/// enabled.
pub fn is_bridge_enabled() -> bool {
    false
}
