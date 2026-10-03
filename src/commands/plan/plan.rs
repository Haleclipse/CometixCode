//! Maps to: CC `commands/plan/plan.tsx`.

use crate::tool::ToolPermissionContext;
use crate::tool::ToolUseContext;
use crate::types::permissions::{PermissionMode, PermissionUpdate, PermissionUpdateDestination};
use iocraft::prelude::*;
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlanCall {
    Enabled {
        output: String,
        should_query: bool,
        next_context: ToolPermissionContext,
    },
    Inspect(PlanInspectionRequest),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanInspectionRequest {
    pub args: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlanInspectionResult {
    Output(String),
    Open(PathBuf),
}

#[derive(Default, Props)]
struct PlanDisplayProps {
    plan_content: String,
    plan_path: String,
    editor_name: Option<String>,
}

/// Maps to: CC `commands/plan/plan.tsx#PlanDisplay`.
#[component]
fn PlanDisplay(props: &PlanDisplayProps) -> impl Into<AnyElement<'static>> {
    element! {
        View(flex_direction: FlexDirection::Column) {
            Text(content: "Current Plan".to_string(), weight: Weight::Bold, wrap: TextWrap::NoWrap)
            Text(content: props.plan_path.clone(), dim: true, wrap: TextWrap::Wrap)
            View(margin_top: 1u32) {
                Text(content: props.plan_content.clone(), wrap: TextWrap::Wrap)
            }
            #(props.editor_name.clone().map(|editor_name| element! {
                View(flex_direction: FlexDirection::Row, margin_top: 1u32, flex_wrap: FlexWrap::Wrap) {
                    Text(content: "\"/plan open\"".to_string(), dim: true, wrap: TextWrap::NoWrap)
                    Text(content: " to edit this plan in ".to_string(), dim: true, wrap: TextWrap::NoWrap)
                    Text(content: editor_name, weight: Weight::Bold, dim: true, wrap: TextWrap::NoWrap)
                }
            }))
        }
    }
}

/// Maps to CC `call(...)`'s plan-mode entry branch. AppState mutation remains
/// synchronous and in-memory; no plan/config I/O occurs on this branch.
pub fn call(args: &str, context: &ToolUseContext) -> PlanCall {
    let current_context = context
        .app_store
        .store
        .as_ref()
        .map(|store| (*store.get().tool_permission_context).clone())
        .unwrap_or_else(|| context.tool_permission_context.clone());

    if current_context.mode == PermissionMode::Plan {
        return PlanCall::Inspect(PlanInspectionRequest {
            args: args.to_string(),
        });
    }

    crate::bootstrap::state::handle_plan_mode_transition(
        crate::utils::permissions::permission_mode::permission_mode_internal_name(
            current_context.mode,
        ),
        "plan",
    );
    let prepared = crate::utils::permissions::permission_setup::prepare_context_for_plan_mode(
        &current_context,
    );
    let next_context = crate::utils::permissions::permission_update::apply_permission_update(
        &prepared,
        &PermissionUpdate::SetMode {
            destination: PermissionUpdateDestination::Session,
            mode: PermissionMode::Plan,
        },
    );
    if let Some(store) = context.app_store.store.as_ref() {
        let next_context_for_store = next_context.clone();
        store.replace_with(|state| state.set_tool_permission_context(next_context_for_store));
    }

    let description = args.trim();
    PlanCall::Enabled {
        output: "Enabled plan mode".to_string(),
        should_query: !description.is_empty() && description != "open",
        next_context,
    }
}

/// Maps to the already-in-plan branch of CC `call(...)`. This function is run
/// by the REPL worker so plan/config/editor discovery stays out of update().
pub fn inspect_plan(request: &PlanInspectionRequest) -> PlanInspectionResult {
    let plan_content = crate::utils::plans::get_plan(None);
    let plan_path = crate::utils::plans::get_plan_file_path(None);
    let Some(plan_content) = plan_content else {
        return PlanInspectionResult::Output(
            "Already in plan mode. No plan written yet.".to_string(),
        );
    };

    if request
        .args
        .trim()
        .split_whitespace()
        .next()
        .is_some_and(|arg| arg == "open")
    {
        return PlanInspectionResult::Open(plan_path);
    }

    let editor_name = crate::utils::prompt_editor::external_editor_command()
        .map(|(program, _)| crate::utils::ide::to_ide_display_name(Some(&program)));
    PlanInspectionResult::Output(render_plan_display(
        plan_content,
        plan_path.display().to_string(),
        editor_name,
    ))
}

pub fn render_plan_display(
    plan_content: String,
    plan_path: String,
    editor_name: Option<String>,
) -> String {
    let mut element = element! {
        PlanDisplay(
            plan_content: plan_content,
            plan_path: plan_path,
            editor_name: editor_name,
        )
    };
    element.render(Some(80)).to_string().trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    struct PlanEnvRestore {
        config_dir: Option<EnvVarGuard>,
        session_id: String,
        temp_dir: PathBuf,
    }

    impl Drop for PlanEnvRestore {
        fn drop(&mut self) {
            drop(self.config_dir.take());
            crate::bootstrap::state::set_session_id(&self.session_id);
            crate::utils::config::clear_global_config_cache_for_testing();
            let _ = std::fs::remove_dir_all(&self.temp_dir);
        }
    }

    #[test]
    fn plan_call_enables_plan_mode_and_only_queries_for_description() {
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        let context = ToolUseContext::default().with_app_store(store.clone());
        let result = call("implement the migration", &context);
        assert!(matches!(
            result,
            PlanCall::Enabled {
                output,
                should_query: true,
                next_context,
            } if output == "Enabled plan mode"
                && next_context.mode == PermissionMode::Plan
                && next_context.pre_plan_mode == Some(PermissionMode::Default)
        ));
        assert_eq!(
            store.get().tool_permission_context.mode,
            PermissionMode::Plan
        );
    }

    #[test]
    fn plan_call_open_while_entering_does_not_query_like_official() {
        let context = ToolUseContext::default();
        assert!(matches!(
            call(" open ", &context),
            PlanCall::Enabled {
                should_query: false,
                ..
            }
        ));
    }

    #[test]
    fn plan_inspection_reads_current_plan_and_routes_open_argument() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _plan_lock = crate::utils::plans::test_plan_state_lock();
        let temp_dir =
            std::env::temp_dir().join(format!("cometix-plan-command-{}", uuid::Uuid::new_v4()));
        let restore = PlanEnvRestore {
            config_dir: Some(EnvVarGuard::set("CLAUDE_CONFIG_DIR", &temp_dir)),
            session_id: crate::bootstrap::state::get_session_id(),
            temp_dir: temp_dir.clone(),
        };
        crate::utils::config::clear_global_config_cache_for_testing();
        let session_id = format!("plan-command-{}", uuid::Uuid::new_v4());
        crate::bootstrap::state::set_session_id(&session_id);
        crate::utils::plans::set_plan_slug(&session_id, "calm-river");
        let plan_path = temp_dir.join("plans").join("calm-river.md");
        std::fs::create_dir_all(plan_path.parent().unwrap()).unwrap();
        std::fs::write(&plan_path, "# Current\n\n- inspect").unwrap();

        let display = inspect_plan(&PlanInspectionRequest {
            args: String::new(),
        });
        let PlanInspectionResult::Output(output) = display else {
            panic!("expected rendered current plan");
        };
        assert!(output.contains("Current Plan"), "output={output:?}");
        assert!(output.contains("# Current"), "output={output:?}");
        let output_without_layout_whitespace = output
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>();
        let path_without_whitespace = plan_path
            .display()
            .to_string()
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>();
        assert!(
            output_without_layout_whitespace.contains(&path_without_whitespace),
            "output={output:?}"
        );
        assert_eq!(
            inspect_plan(&PlanInspectionRequest {
                args: "open extra".to_string(),
            }),
            PlanInspectionResult::Open(plan_path)
        );

        crate::utils::plans::clear_plan_slug(Some(&session_id));
        drop(restore);
    }

    #[test]
    fn plan_display_matches_official_heading_path_content_and_editor_hint() {
        let output = render_plan_display(
            "# Steps\n\n1. Implement".to_string(),
            "/tmp/plan.md".to_string(),
            Some("VS Code".to_string()),
        );
        assert!(output.contains("Current Plan"), "output=\n{output}");
        assert!(output.contains("/tmp/plan.md"), "output=\n{output}");
        assert!(output.contains("# Steps"), "output=\n{output}");
        assert!(
            output.contains("\"/plan open\" to edit this plan in VS Code"),
            "output=\n{output}"
        );
    }
}
