//! Maps to: CC `components/DevBar.tsx`.
//!
//! Official `DevBar` polls `bootstrap/state.ts#getSlowOperations` every 500ms
//! only when `shouldShowDevBar()` is true. Nyan keeps polling outside this
//! render boundary; callers pass the current slow-operation snapshot and the
//! already-resolved visibility gate.

use iocraft::prelude::*;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SlowOperationSnapshot {
    pub operation: String,
    pub duration_ms: f64,
    pub timestamp: u64,
}

#[derive(Default, Props)]
pub struct DevBarProps {
    pub slow_ops: Vec<SlowOperationSnapshot>,
    pub show_dev_bar: bool,
}

/// Maps to: CC `components/DevBar.tsx#shouldShowDevBar`.
pub fn should_show_dev_bar(build_mode: &str) -> bool {
    build_mode == "development"
}

/// Maps to: CC `components/DevBar.tsx` `recentOps` formatting.
pub fn dev_bar_recent_ops(slow_ops: &[SlowOperationSnapshot]) -> Option<String> {
    if slow_ops.is_empty() {
        return None;
    }
    Some(
        slow_ops
            .iter()
            .rev()
            .take(3)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(|op| format!("{} ({:.0}ms)", op.operation, op.duration_ms.round()))
            .collect::<Vec<_>>()
            .join(" · "),
    )
}

/// Maps to: CC `components/DevBar.tsx#DevBar`.
#[component]
pub fn DevBar(props: &DevBarProps, hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<crate::utils::theme::Theme>();
    if !props.show_dev_bar {
        return element! { View(width: 0u32, height: 0u32) }.into_any();
    }
    let Some(recent_ops) = dev_bar_recent_ops(&props.slow_ops) else {
        return element! { View(width: 0u32, height: 0u32) }.into_any();
    };

    element! {
        Text(
            content: format!("[ANT-ONLY] slow sync: {recent_ops}"),
            color: theme.warning,
            wrap: TextWrap::TruncateEnd,
        )
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;

    #[test]
    fn dev_bar_visibility_gate_matches_official_constants() {
        assert!(should_show_dev_bar("development"));
        assert!(!should_show_dev_bar("production"));
    }

    #[test]
    fn dev_bar_recent_ops_keeps_last_three_and_rounds_durations() {
        let ops = (1..=4)
            .map(|i| SlowOperationSnapshot {
                operation: format!("op{i}"),
                duration_ms: 10.4 + i as f64,
                timestamp: i,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            dev_bar_recent_ops(&ops),
            Some("op2 (12ms) · op3 (13ms) · op4 (14ms)".to_string())
        );
        assert_eq!(dev_bar_recent_ops(&[]), None);
    }

    #[test]
    fn dev_bar_renders_ant_only_warning_line() {
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                DevBar(
                    show_dev_bar: true,
                    slow_ops: vec![SlowOperationSnapshot {
                        operation: "load config".to_string(),
                        duration_ms: 42.2,
                        timestamp: 1,
                    }],
                )
            }
        }
        .render(Some(100))
        .to_string();

        assert!(
            text.contains("[ANT-ONLY] slow sync: load config (42ms)"),
            "canvas=\n{text}"
        );
    }
}
