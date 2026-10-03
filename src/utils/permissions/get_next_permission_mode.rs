//! Permission mode cycling helpers.
//! Maps to: CC `utils/permissions/getNextPermissionMode.ts`.

use crate::tool::ToolPermissionContext;
use crate::types::permissions::PermissionMode;
use crate::utils::permissions::permission_mode::permission_mode_internal_name;
use crate::utils::permissions::permission_setup::{
    is_auto_mode_gate_enabled, is_transcript_classifier_feature_enabled, transition_permission_mode,
};

/// Maps to CC `canCycleToAuto(ctx)` (private in getNextPermissionMode.ts).
///
/// Requires **both** cached `isAutoModeAvailable` (startup verify) and live
/// `isAutoModeGateEnabled()` — they can diverge mid-session (circuit breaker /
/// settings). Live gate prevents `transitionPermissionMode` throwing and
/// freezing Shift+Tab.
fn can_cycle_to_auto(ctx: &ToolPermissionContext) -> bool {
    if !is_transcript_classifier_feature_enabled() {
        return false;
    }
    // Maps to: CC getNextPermissionMode.ts:17-28.
    let gate_enabled = is_auto_mode_gate_enabled();
    // `!!ctx.isAutoModeAvailable` — None/false both mean unavailable.
    let available = ctx.is_auto_mode_available.unwrap_or(false);
    let can = available && gate_enabled;
    if !can {
        crate::utils::debug::log_for_debugging(&format!(
            "[auto-mode] canCycleToAuto=false: ctx.isAutoModeAvailable={:?} isAutoModeGateEnabled={} reason={:?}",
            ctx.is_auto_mode_available,
            gate_enabled,
            crate::utils::permissions::permission_setup::get_auto_mode_unavailable_reason(),
        ));
    }
    can
}

/// Determines the next permission mode when cycling through modes.
/// Maps to CC `getNextPermissionMode(...)`.
pub fn get_next_permission_mode(tool_permission_context: &ToolPermissionContext) -> PermissionMode {
    match tool_permission_context.mode {
        PermissionMode::Default => {
            // Maps to: CC getNextPermissionMode.ts:40-49. BuildAudience is
            // the established compile-time carrier for the source ant branch.
            if crate::utils::build_profile::build_audience().is_internal() {
                if tool_permission_context.is_bypass_permissions_mode_available {
                    return PermissionMode::BypassPermissions;
                }
                if can_cycle_to_auto(tool_permission_context) {
                    return PermissionMode::Auto;
                }
                return PermissionMode::Default;
            }
            PermissionMode::AcceptEdits
        }
        PermissionMode::AcceptEdits => PermissionMode::Plan,
        PermissionMode::Plan => {
            if tool_permission_context.is_bypass_permissions_mode_available {
                PermissionMode::BypassPermissions
            } else if can_cycle_to_auto(tool_permission_context) {
                PermissionMode::Auto
            } else {
                PermissionMode::Default
            }
        }
        PermissionMode::BypassPermissions => {
            if can_cycle_to_auto(tool_permission_context) {
                PermissionMode::Auto
            } else {
                PermissionMode::Default
            }
        }
        PermissionMode::DontAsk => PermissionMode::Default,
        // CC `default:` arm — covers auto (when TRANSCRIPT_CLASSIFIER is
        // enabled), the internal bubble mode, and any future modes.
        PermissionMode::Auto | PermissionMode::Bubble => PermissionMode::Default,
    }
}

/// Computes the next mode and context (with strip/restore side effects).
/// Maps to CC `cyclePermissionMode` (`getNextPermissionMode.ts`:88-101).
///
/// ```ts
/// context: transitionPermissionMode(
///   toolPermissionContext.mode,
///   nextMode,
///   toolPermissionContext,
/// )
/// ```
/// Caller is responsible for setting the mode on the returned context
/// (CC `transitionPermissionMode` doc).
pub fn cycle_permission_mode(
    tool_permission_context: &ToolPermissionContext,
) -> (PermissionMode, ToolPermissionContext) {
    let from_mode = tool_permission_context.mode;
    let next_mode = get_next_permission_mode(tool_permission_context);
    let context = transition_permission_mode(
        permission_mode_internal_name(from_mode),
        permission_mode_internal_name(next_mode),
        tool_permission_context,
    );
    (next_mode, context)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::permissions::auto_mode_state;
    use crate::utils::permissions::permission_setup::transition_into_auto_mode;
    use crate::utils::test_env::TEST_ENV_LOCK;

    #[test]
    fn next_permission_mode_default_cycle_without_auto_available() {
        let mut context = ToolPermissionContext::default();
        context.is_auto_mode_available = Some(false);
        assert_eq!(
            get_next_permission_mode(&context),
            if crate::utils::build_profile::build_audience().is_internal() {
                PermissionMode::Default
            } else {
                PermissionMode::AcceptEdits
            }
        );
        context.mode = PermissionMode::AcceptEdits;
        assert_eq!(get_next_permission_mode(&context), PermissionMode::Plan);
        context.mode = PermissionMode::Plan;
        assert_eq!(get_next_permission_mode(&context), PermissionMode::Default);
        context.mode = PermissionMode::BypassPermissions;
        assert_eq!(get_next_permission_mode(&context), PermissionMode::Default);
        context.mode = PermissionMode::DontAsk;
        assert_eq!(get_next_permission_mode(&context), PermissionMode::Default);
        context.mode = PermissionMode::Auto;
        assert_eq!(get_next_permission_mode(&context), PermissionMode::Default);
        context.mode = PermissionMode::Bubble;
        assert_eq!(get_next_permission_mode(&context), PermissionMode::Default);
    }

    #[test]
    fn next_permission_mode_includes_auto_when_available_and_gate_on() {
        auto_mode_state::reset_for_testing();
        let mut context = ToolPermissionContext::default();
        context.is_auto_mode_available = Some(true);
        context.is_bypass_permissions_mode_available = false;
        context.mode = PermissionMode::Plan;
        if is_auto_mode_gate_enabled() {
            assert_eq!(get_next_permission_mode(&context), PermissionMode::Auto);
            context.mode = PermissionMode::BypassPermissions;
            assert_eq!(get_next_permission_mode(&context), PermissionMode::Auto);
        }
    }

    #[test]
    fn cycle_from_accept_edits_to_plan_does_not_call_auto_active() {
        auto_mode_state::reset_for_testing();
        let mut context = ToolPermissionContext::default();
        context.mode = PermissionMode::AcceptEdits;
        let (next, _) = cycle_permission_mode(&context);
        assert_eq!(next, PermissionMode::Plan);
        assert!(!auto_mode_state::is_auto_mode_active());
    }

    #[test]
    fn cycle_into_auto_uses_from_mode_not_external_default() {
        auto_mode_state::reset_for_testing();
        crate::bootstrap::state::set_needs_auto_mode_exit_attachment(true);
        let mut context = ToolPermissionContext::default();
        context.is_auto_mode_available = Some(true);
        context.is_bypass_permissions_mode_available = false;
        context.mode = PermissionMode::Plan;
        if !is_auto_mode_gate_enabled() {
            return;
        }
        let (next, next_ctx) = cycle_permission_mode(&context);
        assert_eq!(next, PermissionMode::Auto);
        assert_eq!(next_ctx.mode, PermissionMode::Plan);
        assert!(auto_mode_state::is_auto_mode_active());
        // CC handleAutoModeTransition skips plan↔auto (prepareContextForPlanMode /
        // ExitPlanMode own those). Attachment flag is NOT cleared on plan→auto.
        assert!(crate::bootstrap::state::needs_auto_mode_exit_attachment());
    }

    #[test]
    fn cycle_out_of_auto_restores_and_sets_exit_attachment() {
        auto_mode_state::reset_for_testing();
        let mut context = ToolPermissionContext::default();
        context = transition_into_auto_mode(&context);
        assert!(auto_mode_state::is_auto_mode_active());
        let (next, next_ctx) = cycle_permission_mode(&context);
        assert_eq!(next, PermissionMode::Default);
        assert_eq!(next_ctx.mode, PermissionMode::Auto);
        assert!(!auto_mode_state::is_auto_mode_active());
        assert!(crate::bootstrap::state::needs_auto_mode_exit_attachment());
    }

    #[test]
    fn cycle_permission_mode_matches_official_caller_owned_mode_assignment() {
        let context = ToolPermissionContext::default();
        let (next_mode, next_context) = cycle_permission_mode(&context);
        assert_eq!(
            next_mode,
            if crate::utils::build_profile::build_audience().is_internal() {
                PermissionMode::Default
            } else {
                PermissionMode::AcceptEdits
            }
        );
        assert_eq!(next_context.mode, PermissionMode::Default);
        assert_eq!(context.mode, PermissionMode::Default);
    }

    #[test]
    fn next_permission_mode_matches_official_audience_and_default_priority() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let internal = crate::utils::build_profile::build_audience().is_internal();
        let mut context = ToolPermissionContext::default();
        for auto_available in [Some(false), Some(true), None] {
            context.is_auto_mode_available = auto_available;
            context.is_bypass_permissions_mode_available = true;
            assert_eq!(
                get_next_permission_mode(&context),
                if internal {
                    PermissionMode::BypassPermissions
                } else {
                    PermissionMode::AcceptEdits
                }
            );
        }
        context.is_bypass_permissions_mode_available = false;
        context.is_auto_mode_available = Some(true);
        let expected = if !internal {
            PermissionMode::AcceptEdits
        } else if is_transcript_classifier_feature_enabled() && is_auto_mode_gate_enabled() {
            PermissionMode::Auto
        } else {
            PermissionMode::Default
        };
        assert_eq!(get_next_permission_mode(&context), expected);
        context.is_auto_mode_available = Some(false);
        assert_eq!(
            get_next_permission_mode(&context),
            if internal {
                PermissionMode::Default
            } else {
                PermissionMode::AcceptEdits
            }
        );
    }
}
