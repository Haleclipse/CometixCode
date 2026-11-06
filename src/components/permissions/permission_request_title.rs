//! Maps to: CC `components/permissions/PermissionRequestTitle.tsx`.
//!
//! Shared title block for permission dialogs. It mirrors the official title,
//! optional subtitle, and inline worker badge shape; the richer worker badge
//! component remains in `worker_badge.rs` for call sites that render it directly.

use super::worker_badge::WorkerBadgeProps;
use crate::utils::theme::Theme;
use iocraft::prelude::*;

#[derive(Default, Props)]
pub struct PermissionRequestTitleProps {
    pub title: String,
    pub subtitle: Option<String>,
    pub color: Option<Color>,
    pub worker_badge: Option<WorkerBadgeProps>,
}

/// Maps to: CC `components/permissions/PermissionRequestTitle.tsx`
/// `PermissionRequestTitle`.
#[component]
pub fn PermissionRequestTitle(
    props: &PermissionRequestTitleProps,
    hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let color = props.color.unwrap_or(theme.permission);

    element! {
        View(flex_direction: FlexDirection::Column) {
            View(flex_direction: FlexDirection::Row, column_gap: 1u32) {
                Text(content: props.title.clone(), color: color, weight: Weight::Bold, wrap: TextWrap::NoWrap)
                #(props.worker_badge.as_ref().map(|badge| {
                    let display_badge = if badge.name.starts_with('[') {
                        format!("· {}", badge.name)
                    } else {
                        format!("· @{}", badge.name)
                    };
                    element! {
                        Text(content: display_badge, dim: true, wrap: TextWrap::NoWrap)
                    }
                }))
            }
            #(props.subtitle.as_ref().map(|subtitle| element! {
                Text(content: subtitle.clone(), dim: true, wrap: TextWrap::TruncateStart)
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;

    #[test]
    fn permission_request_title_matches_official_title_subtitle_and_worker_badge() {
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                PermissionRequestTitle(
                    title: "Bash command".to_string(),
                    subtitle: Some("/very/long/path".to_string()),
                    worker_badge: Some(WorkerBadgeProps { name: "agent-a".to_string(), color: None }),
                )
            }
        }
        .render(Some(80))
        .to_string();

        assert!(text.contains("Bash command"), "canvas=\n{text}");
        assert!(text.contains("/very/long/path"), "canvas=\n{text}");
        assert!(text.contains("· @agent-a"), "canvas=\n{text}");
    }

    #[test]
    fn bracketed_subagent_badge_is_not_prefixed_with_at() {
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                PermissionRequestTitle(
                    title: "Bash command".to_string(),
                    worker_badge: Some(WorkerBadgeProps { name: "[explore]".to_string(), color: None }),
                )
            }
        }
        .render(Some(80))
        .to_string();

        assert!(text.contains("· [explore]"), "canvas=\n{text}");
        assert!(!text.contains("@["), "canvas=\n{text}");
    }
}
