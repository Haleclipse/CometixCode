//! Maps to: CC `components/MemoryUsageIndicator.tsx`.
//!
//! Sampling / thresholds live in [`crate::hooks::use_memory_usage`] (CC
//! `hooks/useMemoryUsage.ts`). This component owns the ant gate + 10s poll,
//! mounts under `Notifications`, and bumps the PromptInput-scoped
//! [`FooterLayoutWake`] when height visibility flips (L1 `PromptInput-scoped
//! footer layout wake`, PORTING.md) — AppStore is not involved.

use crate::components::prompt_input::footer_layout_wake::FooterLayoutWake;
use crate::hooks::use_memory_usage::sample_memory_usage;
use crate::utils::format::format_file_size;
use iocraft::prelude::*;
use std::time::Duration;

pub use crate::hooks::use_memory_usage::{
    CRITICAL_MEMORY_THRESHOLD_BYTES, HIGH_MEMORY_THRESHOLD_BYTES, MemoryUsageInfo,
    MemoryUsageStatus, approximate_process_memory_bytes, memory_usage_status_for_heap,
    process_rss_bytes, try_process_rss_bytes,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryUsageIndicatorColor {
    Warning,
    Error,
}

#[derive(Default, Props)]
pub struct MemoryUsageIndicatorProps {
    /// Test-only override for the internal-build gate. Production leaves this
    /// `None` and reads the compile-time distribution capability.
    pub ant_build: Option<bool>,
    /// Test-only override for the memory snapshot. Production leaves this
    /// `None` and self-polls.
    pub memory_usage: Option<MemoryUsageInfo>,
}

fn is_internal_build() -> bool {
    crate::utils::build_profile::has_internal_capability(
        crate::utils::build_profile::InternalCapability::Ui,
    )
}

/// Maps to: CC `MemoryUsageIndicator` render/null branches.
pub fn memory_usage_indicator_text(
    ant_build: bool,
    memory_usage: Option<&MemoryUsageInfo>,
) -> Option<(String, MemoryUsageIndicatorColor)> {
    if !ant_build {
        return None;
    }
    let memory_usage = memory_usage?;
    if memory_usage.status == MemoryUsageStatus::Normal {
        return None;
    }
    if memory_usage.status == MemoryUsageStatus::Unknown {
        return Some((
            "Memory usage unknown".to_string(),
            MemoryUsageIndicatorColor::Warning,
        ));
    }

    let color = if memory_usage.status == MemoryUsageStatus::Critical {
        MemoryUsageIndicatorColor::Error
    } else {
        MemoryUsageIndicatorColor::Warning
    };
    Some((
        format!(
            "High memory usage ({}) · /heapdump",
            format_file_size(memory_usage.heap_used)
        ),
        color,
    ))
}

/// Maps to: CC footer height contribution of `<MemoryUsageIndicator />`.
pub fn memory_hint_row_count(visible: bool) -> usize {
    usize::from(visible)
}

/// Live visibility for PromptInput height (ant builds + above Normal).
pub fn memory_hint_visible_now() -> bool {
    if !is_internal_build() {
        return false;
    }
    let info = sample_memory_usage();
    info.status != MemoryUsageStatus::Normal
}

/// Bump the scoped wake on visibility flips only (visible→visible sample
/// changes must not wake the height owner).
fn bump_memory_layout_if_needed(wake: Option<&FooterLayoutWake>, visible: bool, was_visible: bool) {
    if visible != was_visible
        && let Some(wake) = wake
    {
        wake.bump();
    }
}

/// Maps to: CC `components/MemoryUsageIndicator.tsx#MemoryUsageIndicator`.
#[component]
pub fn MemoryUsageIndicator(
    props: &MemoryUsageIndicatorProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<crate::utils::theme::Theme>();
    let footer_layout_wake = hooks
        .try_use_context::<FooterLayoutWake>()
        .map(|wake| wake.clone());

    let ant_build = props.ant_build.unwrap_or_else(is_internal_build);
    let test_override = props.memory_usage.clone();
    let skip_poll = test_override.is_some() || cfg!(test) || !ant_build;

    let memory_state = hooks.use_state(|| Option::<MemoryUsageInfo>::None);

    // Maps to: CC `useMemoryUsage` 10s interval (ant builds only).
    hooks.use_future({
        let mut memory_state = memory_state;
        let footer_layout_wake = footer_layout_wake.clone();
        async move {
            if skip_poll {
                return;
            }
            loop {
                let info = sample_memory_usage();
                // CC: bail to null when normal to avoid waking Notifications
                // every 10s for users who never hit the threshold. The scoped
                // wake is bumped only on visibility flips; local state owns
                // the bytes.
                if info.status == MemoryUsageStatus::Normal {
                    if memory_state.read().is_some() {
                        memory_state.set(None);
                        bump_memory_layout_if_needed(footer_layout_wake.as_ref(), false, true);
                    }
                } else {
                    let was_visible = memory_state.read().is_some();
                    memory_state.set(Some(info.clone()));
                    bump_memory_layout_if_needed(footer_layout_wake.as_ref(), true, was_visible);
                }
                futures_timer::Delay::new(Duration::from_secs(10)).await;
            }
        }
    });

    let memory_usage = test_override.or_else(|| memory_state.read().clone());
    let Some((text, color)) = memory_usage_indicator_text(ant_build, memory_usage.as_ref()) else {
        return element! { View(width: 0u32, height: 0u32) }.into_any();
    };
    let color = match color {
        MemoryUsageIndicatorColor::Warning => theme.warning,
        MemoryUsageIndicatorColor::Error => theme.error,
    };

    element! {
        View(flex_direction: FlexDirection::Row, height: 1u32, overflow: Overflow::Hidden) {
            Text(content: text, color: color, wrap: TextWrap::Truncate)
        }
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;

    fn render(ant_build: bool, info: Option<MemoryUsageInfo>) -> String {
        element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                MemoryUsageIndicator(
                    ant_build: Some(ant_build),
                    memory_usage: info,
                )
            }
        }
        .render(Some(100))
        .to_string()
    }

    #[test]
    fn memory_usage_indicator_gates_external_and_normal_status_like_official() {
        let high = MemoryUsageInfo {
            heap_used: HIGH_MEMORY_THRESHOLD_BYTES,
            status: MemoryUsageStatus::High,
        };
        assert_eq!(memory_usage_indicator_text(false, Some(&high)), None);
        assert_eq!(
            memory_usage_indicator_text(
                true,
                Some(&MemoryUsageInfo {
                    heap_used: HIGH_MEMORY_THRESHOLD_BYTES - 1,
                    status: MemoryUsageStatus::Normal,
                }),
            ),
            None
        );
        assert_eq!(render(false, Some(high)), "");
    }

    #[test]
    fn memory_usage_indicator_renders_unknown_copy_without_zero_rss() {
        let unknown = memory_usage_indicator_text(
            true,
            Some(&MemoryUsageInfo {
                heap_used: 0,
                status: MemoryUsageStatus::Unknown,
            }),
        )
        .expect("unknown memory should render an explicit status");
        assert_eq!(unknown.0, "Memory usage unknown");
        assert_eq!(unknown.1, MemoryUsageIndicatorColor::Warning);
    }

    #[test]
    fn memory_usage_indicator_renders_high_and_critical_copy() {
        let high = memory_usage_indicator_text(
            true,
            Some(&MemoryUsageInfo {
                heap_used: HIGH_MEMORY_THRESHOLD_BYTES,
                status: MemoryUsageStatus::High,
            }),
        )
        .expect("high memory should render");
        assert_eq!(high.0, "High memory usage (1.5GB) · /heapdump");
        assert_eq!(high.1, MemoryUsageIndicatorColor::Warning);

        let critical_text = render(
            true,
            Some(MemoryUsageInfo {
                heap_used: CRITICAL_MEMORY_THRESHOLD_BYTES,
                status: MemoryUsageStatus::Critical,
            }),
        );
        assert!(
            critical_text.contains("High memory usage (2.5GB) · /heapdump"),
            "canvas=\n{critical_text}"
        );
    }

    #[test]
    fn memory_hint_row_count_matches_visibility_gate() {
        assert_eq!(memory_hint_row_count(false), 0);
        assert_eq!(memory_hint_row_count(true), 1);
    }

    /// The 10s poll bumps the scoped wake only on visibility flips; a
    /// visible→visible sample change (e.g. High→Critical bytes) must not wake
    /// the height owner.
    #[test]
    fn memory_poll_bumps_scoped_wake_on_visibility_flips_only() {
        let wake = FooterLayoutWake::detached();
        bump_memory_layout_if_needed(Some(&wake), true, false);
        assert_eq!(wake.epoch(), 1, "0→1 flip must bump");
        bump_memory_layout_if_needed(Some(&wake), true, true);
        assert_eq!(wake.epoch(), 1, "visible→visible sample change: no bump");
        bump_memory_layout_if_needed(Some(&wake), false, true);
        assert_eq!(wake.epoch(), 2, "1→0 flip must bump");
        bump_memory_layout_if_needed(Some(&wake), false, false);
        assert_eq!(wake.epoch(), 2, "hidden→hidden: no bump");
        // Absent wake (component mounted outside the Footer subtree): no-op.
        bump_memory_layout_if_needed(None, true, false);
    }
}
