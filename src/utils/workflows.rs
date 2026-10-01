//! Incomplete session gate for Dynamic Workflows.
//! Maps to: rebuild `workflow-reconstruction` `src/utils/workflows.ts`.
//!
//! Official `isDynamicWorkflowsEnabled` also reads managed `disableWorkflows`,
//! org `allow_workflows`, `/config` `enableWorkflows`, and the subscription
//! default. Those are not wired. The GrowthBook `tengu_workflows_enabled`
//! switch in [`crate::utils::feature_flags`] is the control this port honors.

use crate::utils::env_utils::{is_env_defined_falsy, is_env_truthy};
use crate::utils::feature_flags::{FeatureFlag, feature_enabled};

/// Maps to: CC `utils/workflows.ts#isDynamicWorkflowsEnabled` (`gO` / `dS`).
pub fn is_dynamic_workflows_enabled() -> bool {
    if is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_DISABLE_WORKFLOWS")
            .as_deref(),
    ) {
        return false;
    }
    if is_env_defined_falsy(crate::utils::process_env::var("CLAUDE_CODE_WORKFLOWS").as_deref()) {
        return false;
    }
    feature_enabled(FeatureFlag::WorkflowsEnabled)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workflows_gate_follows_tengu_source_switch() {
        assert!(!feature_enabled(FeatureFlag::WorkflowsEnabled));
        assert!(!is_dynamic_workflows_enabled());
    }
}
