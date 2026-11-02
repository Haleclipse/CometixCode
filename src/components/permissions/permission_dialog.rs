//! Maps to: CC `components/permissions/PermissionDialog.tsx`.
//!
//! Official permission-dialog shell: top-only rounded border, `PermissionRequestTitle`
//! header, optional right-side title content, and padded body. It intentionally
//! does not own Enter/Esc/Ctrl-C handling; child request components and Select
//! controls own their own key handling just like CC call sites pass callbacks to
//! nested components.

use super::permission_request_title::PermissionRequestTitle;
use super::worker_badge::WorkerBadgeProps;
use crate::utils::theme::Theme;
use iocraft::prelude::*;

#[derive(Default, Props)]
pub struct PermissionDialogProps {
    pub title: String,
    pub subtitle: Option<String>,
    pub color: Option<Color>,
    pub title_color: Option<Color>,
    pub inner_padding_x: Option<u32>,
    pub worker_badge: Option<WorkerBadgeProps>,
    /// Rust-side text equivalent of official `titleRight` ReactNode.
    pub title_right: Option<String>,
    /// Preserves callers such as hooks/PromptDialog that pass a dim Text node.
    pub title_right_dim: bool,
    pub children: Vec<AnyElement<'static>>,
}

/// Maps to: CC `components/permissions/PermissionDialog.tsx`
/// `PermissionDialog`.
#[component]
pub fn PermissionDialog(
    props: &mut PermissionDialogProps,
    hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let border_color = props.color.unwrap_or(theme.permission);
    let title_color = props.title_color;
    let inner_padding_x = props.inner_padding_x.unwrap_or(1);
    let body = std::mem::take(&mut props.children);

    element! {
        View(
            flex_direction: FlexDirection::Column,
            width: 100pct,
            border_style: BorderStyle::Round,
            border_color: border_color,
            border_left: false,
            border_right: false,
            border_bottom: false,
            margin_top: 1u32,
        ) {
            View(padding_left: 1u32, padding_right: 1u32, flex_direction: FlexDirection::Column) {
                View(width: 100pct, justify_content: JustifyContent::SPACE_BETWEEN) {
                    PermissionRequestTitle(
                        title: props.title.clone(),
                        subtitle: props.subtitle.clone(),
                        color: title_color,
                        worker_badge: props.worker_badge.clone(),
                    )
                    #(props.title_right.as_ref().map(|title_right| element! {
                        Text(content: title_right.clone(), dim: props.title_right_dim, wrap: TextWrap::NoWrap)
                    }))
                }
            }
            View(
                flex_direction: FlexDirection::Column,
                padding_left: inner_padding_x,
                padding_right: inner_padding_x,
            ) {
                #(body)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;

    fn render_dialog() -> String {
        element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                PermissionDialog(
                    title: "Remote Control".to_string(),
                    subtitle: Some("Requires account access".to_string()),
                    worker_badge: Some(WorkerBadgeProps { name: "agent-a".to_string(), color: None }),
                    title_right: Some("right".to_string()),
                ) {
                    Text(content: "Body".to_string())
                }
            }
        }
        .render(Some(100))
        .to_string()
    }

    #[test]
    fn permission_dialog_renders_official_top_border_title_and_body_shape() {
        let text = render_dialog();

        assert!(text.contains("Remote Control"), "canvas=\n{text}");
        assert!(text.contains("Requires account access"), "canvas=\n{text}");
        assert!(text.contains("· @agent-a"), "canvas=\n{text}");
        assert!(text.contains("right"), "canvas=\n{text}");
        assert!(text.contains("Body"), "canvas=\n{text}");
        assert!(
            text.contains('─'),
            "top-only border should render; canvas=\n{text}"
        );
        assert!(
            !text.contains("Enter to confirm · Esc to cancel"),
            "PermissionDialog does not add Dialog's input guide; canvas=\n{text}"
        );
    }
}
