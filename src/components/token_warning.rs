//! Maps to: CC `components/TokenWarning.tsx`.
//!
//! The official component computes token warning state from `tokenUsage` and
//! `model`, checks compact-warning suppression, optional reactive/context
//! collapse feature gates, and renders one footer row. Nyan keeps feature
//! flags and context-collapse subscriptions as explicit already-known props: no
//! GrowthBook lookup, no dynamic `require(...)`, and no context-collapse store
//! subscription are started from this render boundary.

use crate::services::compact::auto_compact::{
    calculate_token_warning_state, get_effective_context_window_size, is_auto_compact_enabled,
};
use iocraft::prelude::*;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CollapseLabelSnapshot {
    pub collapsed_spans: u64,
    pub staged_spans: u64,
    pub total_errors: u64,
    pub total_empty_spawns: u64,
    pub empty_spawn_warning_emitted: bool,
}

#[derive(Default, Props)]
pub struct TokenWarningProps {
    pub token_usage: i64,
    pub model: String,
    pub suppress_warning: bool,
    pub auto_compact_enabled: Option<bool>,
    pub reactive_only_mode: bool,
    pub collapse_mode: bool,
    pub collapse_label: Option<CollapseLabelSnapshot>,
    pub upgrade_message: Option<String>,
}

/// Maps to: CC `TokenWarning.tsx#CollapseLabel` render text.
pub fn collapse_label_text(
    snapshot: &CollapseLabelSnapshot,
    upgrade_message: Option<&str>,
) -> Option<(String, bool)> {
    let total = snapshot.collapsed_spans + snapshot.staged_spans;
    if snapshot.total_errors > 0 || snapshot.empty_spawn_warning_emitted {
        let problem = if snapshot.total_errors > 0 {
            format!("collapse errors: {}", snapshot.total_errors)
        } else {
            format!("collapse idle ({} empty runs)", snapshot.total_empty_spawns)
        };
        let text = if total > 0 {
            format!(
                "{} / {total} summarized · {problem}",
                snapshot.collapsed_spans
            )
        } else {
            problem
        };
        return Some((text, true));
    }

    if total == 0 {
        return None;
    }

    let label = format!("{} / {total} summarized", snapshot.collapsed_spans);
    Some((with_upgrade_message(label, upgrade_message), false))
}

/// Maps to: CC `TokenWarning.tsx` final label calculation.
pub fn token_warning_text(
    token_usage: i64,
    model: &str,
    suppress_warning: bool,
    auto_compact_enabled_override: Option<bool>,
    reactive_only_mode: bool,
    collapse_mode: bool,
    collapse_label: Option<&CollapseLabelSnapshot>,
    upgrade_message: Option<&str>,
) -> Option<(String, TokenWarningTone)> {
    let state = calculate_token_warning_state(token_usage, model);
    if !state.is_above_warning_threshold || suppress_warning {
        return None;
    }

    if collapse_mode {
        let (text, warning) = collapse_label_text(collapse_label?, upgrade_message)?;
        return Some((
            text,
            if warning {
                TokenWarningTone::Warning
            } else {
                TokenWarningTone::Dim
            },
        ));
    }

    let auto_compact_enabled =
        auto_compact_enabled_override.unwrap_or_else(is_auto_compact_enabled);
    if auto_compact_enabled {
        let mut display_percent_left = state.percent_left;
        if reactive_only_mode || collapse_mode {
            let effective_window = get_effective_context_window_size(model);
            display_percent_left =
                (((effective_window - token_usage) as f64 / effective_window as f64) * 100.0)
                    .round()
                    .max(0.0) as i64;
        }
        // reactive_only_mode keeps its own copy and dim tone.
        if reactive_only_mode {
            let label = format!(
                "{}% context used",
                100_i64.saturating_sub(display_percent_left)
            );
            return Some((
                with_upgrade_message(label, upgrade_message),
                TokenWarningTone::Dim,
            ));
        }
        // @nyan: short copy + the info line's two-level context color
        // (docs/nyan/tui.md §3.4). CC printed the remaining percentage and
        // always used the dim tone.
        return Some((
            with_upgrade_message("auto-compact soon".to_string(), upgrade_message),
            if state.is_above_error_threshold {
                TokenWarningTone::Error
            } else {
                TokenWarningTone::Warning
            },
        ));
    }

    // @nyan: short copy, same color source as the info line's context segment
    // (docs/nyan/tui.md §3.4).
    let label = if let Some(upgrade_message) = upgrade_message.filter(|value| !value.is_empty()) {
        format!("/compact to free context · {upgrade_message}")
    } else {
        "/compact to free context".to_string()
    };
    Some((
        label,
        if state.is_above_error_threshold {
            TokenWarningTone::Error
        } else {
            TokenWarningTone::Warning
        },
    ))
}

/// Maps to: CC footer height contribution of `<TokenWarning />` (`!isBriefOnly`).
pub fn token_warning_hint_row_count(token_usage: u64, model: &str, is_brief_only: bool) -> usize {
    if is_brief_only {
        return 0;
    }
    usize::from(
        token_warning_text(
            token_usage as i64,
            model,
            false,
            Some(is_auto_compact_enabled()),
            false,
            false,
            None,
            None,
        )
        .is_some(),
    )
}

fn with_upgrade_message(label: String, upgrade_message: Option<&str>) -> String {
    match upgrade_message {
        Some(message) if !message.is_empty() => format!("{label} · {message}"),
        _ => label,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenWarningTone {
    Dim,
    Warning,
    Error,
}

/// Maps to: CC `components/TokenWarning.tsx#TokenWarning`.
#[component]
pub fn TokenWarning(props: &TokenWarningProps, hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<crate::utils::theme::Theme>();
    let body = token_warning_text(
        props.token_usage,
        &props.model,
        props.suppress_warning,
        props.auto_compact_enabled,
        props.reactive_only_mode,
        props.collapse_mode,
        props.collapse_label.as_ref(),
        props.upgrade_message.as_deref(),
    )
    .map(|(text, tone)| {
        let (color, dim) = match tone {
            TokenWarningTone::Dim => (None, true),
            TokenWarningTone::Warning => (Some(theme.warning), false),
            TokenWarningTone::Error => (Some(theme.error), false),
        };
        element! {
            View(flex_direction: FlexDirection::Row) {
                Text(content: text, color: color, dim: dim, wrap: TextWrap::Truncate)
            }
        }
        .into_any()
    })
    .unwrap_or_else(|| element! { View(width: 0u32, height: 0u32) }.into_any());
    let children = vec![body];

    element! { Fragment { #(children) } }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;

    #[test]
    fn collapse_label_text_matches_error_idle_and_summary_branches() {
        assert_eq!(
            collapse_label_text(
                &CollapseLabelSnapshot {
                    collapsed_spans: 2,
                    staged_spans: 1,
                    total_errors: 1,
                    ..Default::default()
                },
                None,
            ),
            Some(("2 / 3 summarized · collapse errors: 1".to_string(), true))
        );
        assert_eq!(
            collapse_label_text(
                &CollapseLabelSnapshot {
                    total_empty_spawns: 3,
                    empty_spawn_warning_emitted: true,
                    ..Default::default()
                },
                None,
            ),
            Some(("collapse idle (3 empty runs)".to_string(), true))
        );
        assert_eq!(
            collapse_label_text(
                &CollapseLabelSnapshot {
                    collapsed_spans: 1,
                    staged_spans: 1,
                    ..Default::default()
                },
                Some("upgrade"),
            ),
            Some(("1 / 2 summarized · upgrade".to_string(), false))
        );
        assert!(collapse_label_text(&CollapseLabelSnapshot::default(), None).is_none());
    }

    #[test]
    fn token_warning_text_matches_short_copy_and_two_level_tone() {
        // 167k threshold: warning at 127k, error at 147k.
        let warning = token_warning_text(
            130_000,
            "claude-sonnet-4-20250514",
            false,
            Some(true),
            false,
            false,
            None,
            None,
        )
        .expect("warning text");
        assert_eq!(warning.0, "auto-compact soon");
        assert_eq!(warning.1, TokenWarningTone::Warning);

        let error = token_warning_text(
            170_000,
            "claude-sonnet-4-20250514",
            false,
            Some(true),
            false,
            false,
            None,
            None,
        )
        .expect("error text");
        assert_eq!(error.0, "auto-compact soon");
        assert_eq!(error.1, TokenWarningTone::Error);

        let manual = token_warning_text(
            130_000,
            "claude-sonnet-4-20250514",
            false,
            Some(false),
            false,
            false,
            None,
            None,
        )
        .expect("manual warning");
        assert_eq!(manual.0, "/compact to free context");
        assert_eq!(manual.1, TokenWarningTone::Warning);
    }

    #[test]
    fn token_warning_component_renders_short_hint_row() {
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                TokenWarning(
                    token_usage: 190000i64,
                    model: "claude-sonnet-4-20250514".to_string(),
                    auto_compact_enabled: Some(false),
                )
            }
        }
        .render(Some(120))
        .to_string();

        assert!(text.contains("/compact to free context"), "canvas=\n{text}");
        assert!(!text.contains("Context low"), "canvas=\n{text}");
    }

    #[test]
    fn token_warning_text_needs_no_opt_in_beyond_the_official_threshold() {
        let (label, _) = token_warning_text(
            130_000,
            "claude-sonnet-4-20250514",
            false,
            Some(false),
            false,
            false,
            None,
            None,
        )
        .expect("CC gates this row on the warning threshold alone");
        assert_eq!(label, "/compact to free context");

        assert!(
            token_warning_text(
                1_000,
                "claude-sonnet-4-20250514",
                false,
                Some(false),
                false,
                false,
                None,
                None,
            )
            .is_none()
        );
    }

    #[test]
    fn token_warning_text_respects_suppression() {
        assert!(
            token_warning_text(
                190_000,
                "claude-sonnet-4-20250514",
                true,
                Some(false),
                false,
                false,
                None,
                None,
            )
            .is_none()
        );
    }
}
