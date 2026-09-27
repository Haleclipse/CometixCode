//! Maps to: CC `components/permissions/WorkerPendingPermission.tsx`.
//!
//! Visual-only pending permission indicator shown in worker sessions while the
//! leader handles the approval. It reads the official teammate identity helpers
//! and does not make permission decisions.

use super::worker_badge::WorkerBadge;
use crate::components::spinner::Spinner;
use crate::tools::agent_tool::agent_color_manager::parse_agent_color_name;
use crate::utils::theme::Theme;
use iocraft::prelude::*;

#[derive(Default, Props)]
pub struct WorkerPendingPermissionProps {
    pub tool_name: String,
    pub description: String,
}

/// Maps to: CC `WorkerPendingPermission` render path.
#[component]
pub fn WorkerPendingPermission(
    props: &WorkerPendingPermissionProps,
    hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let team_name = crate::utils::teammate::get_team_name(None);
    let agent_name = crate::utils::teammate::get_agent_name();
    let agent_color = crate::utils::teammate::get_teammate_color()
        .and_then(|color| parse_agent_color_name(&color))
        .map(|color| {
            crate::utils::iocraft_color::to_iocraft_color(Some(color.official_name()), *theme)
        });

    element! {
        View(
            flex_direction: FlexDirection::Column,
            border_style: BorderStyle::Round,
            border_color: theme.warning,
            padding_left: 1u32,
            padding_right: 1u32,
        ) {
            View(margin_bottom: 1u32) {
                Spinner
                Text(
                    content: " Waiting for team lead approval".to_string(),
                    color: theme.warning,
                    weight: Weight::Bold,
                    wrap: TextWrap::NoWrap,
                )
            }
            #(agent_name.clone().zip(agent_color).map(|(name, color)| element! {
                View(margin_bottom: 1u32) {
                    WorkerBadge(badge: super::worker_badge::WorkerBadgeProps { name: name, color: Some(color) })
                }
            }))
            View() {
                Text(content: "Tool: ".to_string(), dim: true, wrap: TextWrap::NoWrap)
                Text(content: props.tool_name.clone(), wrap: TextWrap::Wrap)
            }
            View() {
                Text(content: "Action: ".to_string(), dim: true, wrap: TextWrap::NoWrap)
                Text(content: props.description.clone(), wrap: TextWrap::Wrap)
            }
            #(team_name.map(|team_name| element! {
                View(margin_top: 1u32) {
                    Text(
                        content: format!("Permission request sent to team \"{team_name}\" leader"),
                        dim: true,
                        wrap: TextWrap::Wrap,
                    )
                }
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::teammate::{
        DynamicTeamContext, TEST_TEAMMATE_CONTEXT_LOCK, set_dynamic_team_context,
    };
    use crate::utils::theme;

    #[test]
    fn worker_pending_permission_renders_official_pending_copy_and_team_context() {
        let _guard = TEST_TEAMMATE_CONTEXT_LOCK.lock().unwrap();
        set_dynamic_team_context(Some(DynamicTeamContext {
            agent_id: "agent-1".to_string(),
            agent_name: "reviewer".to_string(),
            team_name: "alpha".to_string(),
            color: Some("blue".to_string()),
            plan_mode_required: false,
            parent_session_id: None,
        }));

        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                WorkerPendingPermission(
                    tool_name: "Bash".to_string(),
                    description: "cargo test".to_string(),
                )
            }
        }
        .render(Some(100))
        .to_string();

        set_dynamic_team_context(None);

        assert!(
            text.contains("Waiting for team lead approval"),
            "canvas=\n{text}"
        );
        assert!(text.contains("Tool:"), "canvas=\n{text}");
        assert!(text.contains("Bash"), "canvas=\n{text}");
        assert!(text.contains("Action:"), "canvas=\n{text}");
        assert!(text.contains("cargo test"), "canvas=\n{text}");
        assert!(text.contains("@reviewer"), "canvas=\n{text}");
        assert!(
            text.contains("Permission request sent to team \"alpha\" leader"),
            "canvas=\n{text}"
        );
    }
}
