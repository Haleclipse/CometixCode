//! Maps to: CC `components/tasks/RemoteSessionDetailDialog.tsx:1-656`.
//!
//! Browser opening, teleport, and remote termination are emitted as typed
//! actions; this UI boundary does not execute network/process mutation.

use super::remote_session_progress::{
    RemoteSessionProgress, RemoteSessionProgressData, ReviewStage, format_review_stage_counts,
};
use super::task_status_utils::TaskStatus;
use crate::components::agents::new_agent_creation::wizard_steps::choice::WizardChoice;
use crate::components::custom_select::SelectOptionData;
use crate::components::design_system::dialog::Dialog;
use crate::keybindings::keybinding_context::KeybindingRuntime;
use crate::keybindings::types::ContextName;
use crate::keybindings::use_keybinding::use_keybinding;
use crate::utils::theme::Theme;
use crate::utils::truncate::truncate_to_width;
use iocraft::prelude::*;

#[derive(Clone, Debug, PartialEq)]
pub struct RemoteToolUse {
    pub name: String,
    pub input: serde_json::Value,
}
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum RemoteDetailKind {
    #[default]
    Generic,
    Ultraplan {
        phase: Option<String>,
    },
    Review,
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RemoteSessionDetailData {
    pub session_id: String,
    pub title: String,
    pub status: TaskStatus,
    pub elapsed: String,
    pub session_url: String,
    pub kind: RemoteDetailKind,
    pub progress: RemoteSessionProgressData,
    pub tool_uses: Vec<RemoteToolUse>,
    pub recent_messages: Vec<String>,
    pub log_message_count: usize,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoteSessionDetailAction {
    OpenUrl(String),
    Teleport(String),
    Stop(String),
}

fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}
pub fn format_tool_use_summary(name: &str, input: &serde_json::Value) -> String {
    if name == crate::tools::exit_plan_mode_tool::constants::EXIT_PLAN_MODE_V2_TOOL_NAME {
        return "Review the plan in nyan on the web".to_string();
    }
    let Some(object) = input.as_object() else {
        return name.to_string();
    };
    if name == crate::tools::ask_user_question_tool::prompt::ASK_USER_QUESTION_TOOL_NAME
        && let Some(first) = object
            .get("questions")
            .and_then(|value| value.as_array())
            .and_then(|values| values.first())
            .and_then(|value| value.as_object())
    {
        let question = first
            .get("question")
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .or_else(|| first.get("header").and_then(|value| value.as_str()));
        if let Some(question) = question {
            return format!(
                "Answer in browser: {}",
                truncate_to_width(&collapse_whitespace(question), 50)
            );
        }
    }
    object
        .values()
        .find_map(|value| value.as_str().filter(|value| !value.trim().is_empty()))
        .map(|value| {
            format!(
                "{name} {}",
                truncate_to_width(&collapse_whitespace(value), 60)
            )
        })
        .unwrap_or_else(|| name.to_string())
}

pub fn review_counts_line(data: &RemoteSessionDetailData) -> String {
    if !data.progress.has_review_progress {
        return if data.status == TaskStatus::Completed {
            "done".to_string()
        } else {
            "setting up".to_string()
        };
    }
    if data.status == TaskStatus::Completed {
        let mut parts = vec![format!(
            "{} {}",
            data.progress.bugs_verified,
            if data.progress.bugs_verified == 1 {
                "finding"
            } else {
                "findings"
            }
        )];
        if data.progress.bugs_refuted > 0 {
            parts.push(format!("{} refuted", data.progress.bugs_refuted));
        }
        return parts.join(" · ");
    }
    format_review_stage_counts(
        data.progress.review_stage,
        data.progress.bugs_found,
        data.progress.bugs_verified,
        data.progress.bugs_refuted,
    )
}

#[derive(Clone, Copy)]
enum InputAction {
    Done,
    Back,
    Teleport,
}
#[derive(Clone, Copy)]
enum MenuAction {
    Open,
    Stop,
    Back,
    Dismiss,
}
#[derive(Clone, Copy)]
enum StopConfirmAction {
    Confirm,
    Back,
}
#[derive(Default, Props)]
pub struct RemoteSessionDetailDialogProps<'a> {
    pub session: Option<RemoteSessionDetailData>,
    pub on_done: HandlerMut<'a, String>,
    pub on_back: HandlerMut<'a, ()>,
    pub on_kill: HandlerMut<'a, ()>,
    pub on_action: HandlerMut<'a, RemoteSessionDetailAction>,
}

#[component]
pub fn RemoteSessionDetailDialog<'a>(
    props: &mut RemoteSessionDetailDialogProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let Some(session) = props.session.clone() else {
        return element! { Fragment }.into_any();
    };
    let mut pending = hooks.use_state(|| None::<InputAction>);
    let mut pending_menu = hooks.use_state(|| None::<MenuAction>);
    let mut confirming_stop = hooks.use_state(|| false);
    let mut pending_stop = hooks.use_state(|| None::<StopConfirmAction>);
    let stop_action = { *pending_stop.read() };
    if let Some(action) = stop_action {
        pending_stop.set(None);
        match action {
            StopConfirmAction::Back => confirming_stop.set(false),
            StopConfirmAction::Confirm => {
                confirming_stop.set(false);
                if !props.on_kill.is_default() {
                    (props.on_kill)(());
                }
                (props.on_action)(RemoteSessionDetailAction::Stop(session.session_id.clone()));
                if props.on_back.is_default() {
                    (props.on_done)("Remote session details dismissed".to_string());
                } else {
                    (props.on_back)(());
                }
            }
        }
    }
    let menu_action = { *pending_menu.read() };
    if let Some(action) = menu_action {
        pending_menu.set(None);
        match action {
            MenuAction::Open => {
                (props.on_action)(RemoteSessionDetailAction::OpenUrl(
                    session.session_url.clone(),
                ));
                (props.on_done)(String::new());
            }
            MenuAction::Stop => confirming_stop.set(true),
            MenuAction::Back => {
                if props.on_back.is_default() {
                    (props.on_done)("Remote session details dismissed".to_string());
                } else {
                    (props.on_back)(());
                }
            }
            MenuAction::Dismiss => (props.on_done)("Remote session details dismissed".to_string()),
        }
    }
    let action = { *pending.read() };
    if let Some(action) = action {
        pending.set(None);
        match action {
            InputAction::Done => (props.on_done)("Remote session details dismissed".to_string()),
            InputAction::Back if !props.on_back.is_default() => (props.on_back)(()),
            InputAction::Back => (props.on_done)("Remote session details dismissed".to_string()),
            InputAction::Teleport => (props.on_action)(RemoteSessionDetailAction::Teleport(
                session.session_id.clone(),
            )),
        }
    }
    let runtime = hooks
        .try_use_context::<KeybindingRuntime>()
        .map(|runtime| runtime.clone());
    for (name, context, action) in [
        ("confirm:yes", ContextName::Confirmation, InputAction::Done),
        ("task:close", ContextName::Task, InputAction::Done),
        ("task:back", ContextName::Task, InputAction::Back),
        ("task:teleport", ContextName::Task, InputAction::Teleport),
    ] {
        let active = name != "task:teleport" || session.kind == RemoteDetailKind::Generic;
        let mut pending = pending;
        use_keybinding(
            &mut hooks,
            runtime.clone(),
            name,
            context,
            move || active,
            move || {
                pending.set(Some(action));
                true
            },
        );
    }
    let theme = hooks.use_context::<Theme>();
    if confirming_stop.get() {
        let (title, message, label) = match session.kind {
            RemoteDetailKind::Review => (
                "Stop ultrareview?",
                "This archives the remote session and stops local tracking. Findings so far are discarded.",
                "Stop ultrareview",
            ),
            RemoteDetailKind::Ultraplan { .. } => (
                "Stop ultraplan?",
                "This will terminate the nyan on the web session.",
                "Terminate session",
            ),
            RemoteDetailKind::Generic => (
                "Stop remote session?",
                "This will stop the remote session.",
                "Stop session",
            ),
        };
        let mut select = pending_stop;
        let mut cancel = pending_stop;
        return element! { Dialog(title: title.to_string(), color: Some(theme.background), on_cancel: move |_| cancel.set(Some(StopConfirmAction::Back))) {
            View(flex_direction: FlexDirection::Column, row_gap: 1u32) {
                Text(content: message.to_string(), dim: true)
                WizardChoice(options: vec![option(label, "stop"), option("Back", "back")], on_select: move |value: String| select.set(Some(if value == "stop" { StopConfirmAction::Confirm } else { StopConfirmAction::Back })))
            }
        }}.into_any();
    }
    match &session.kind {
        RemoteDetailKind::Generic => {
            let status_label = match session.status {
                TaskStatus::Pending => "starting",
                TaskStatus::Running => "running",
                TaskStatus::Completed => "completed",
                TaskStatus::Failed => "failed",
                TaskStatus::Killed => "killed",
            };
            let status_color = match session.status {
                TaskStatus::Pending | TaskStatus::Running => theme.background,
                TaskStatus::Completed => theme.success,
                TaskStatus::Failed | TaskStatus::Killed => theme.error,
            };
            let messages = session
                .recent_messages
                .iter()
                .map(|message| element! { Text(content: message.clone()) })
                .collect::<Vec<_>>();
            let guide = format!(
                "{}Esc/Enter/Space to close · t to teleport",
                if props.on_back.is_default() {
                    ""
                } else {
                    "← to go back · "
                }
            );
            let mut done = pending;
            element! { Dialog(title: "Remote session details".to_string(), color: Some(theme.background), input_guide: Some(guide), on_cancel: move |_| done.set(Some(InputAction::Done))) {
                View(flex_direction: FlexDirection::Column) {
                    MixedText(contents: vec![MixedTextContent::new("Status: ").weight(Weight::Bold), MixedTextContent::new(status_label).color(status_color)])
                    MixedText(contents: vec![MixedTextContent::new("Runtime: ").weight(Weight::Bold), MixedTextContent::new(session.elapsed.clone())])
                    MixedText(contents: vec![MixedTextContent::new("Title: ").weight(Weight::Bold), MixedTextContent::new(truncate_to_width(&session.title, 50))])
                    View(flex_direction: FlexDirection::Row) { Text(content: "Progress: ".to_string(), weight: Weight::Bold) RemoteSessionProgress(session: Some(session.progress.clone())) }
                    MixedText(contents: vec![MixedTextContent::new("Session URL: ").weight(Weight::Bold), MixedTextContent::new(session.session_url.clone()).weight(Weight::Light)])
                    #((session.log_message_count > 0).then(|| element! { View(flex_direction: FlexDirection::Column, margin_top: 1u32) {
                        Text(content: "Recent messages:".to_string(), weight: Weight::Bold) View(flex_direction: FlexDirection::Column, height: 10u32, overflow: Overflow::Hidden) { #(messages) }
                        Text(content: format!("Showing last {} of {} messages", session.recent_messages.len(), session.log_message_count), dim: true, italic: true)
                    }}))
                }
            }}.into_any()
        }
        RemoteDetailKind::Review => render_review_detail(
            &session,
            !props.on_kill.is_default(),
            pending,
            pending_menu,
            *theme,
        ),
        RemoteDetailKind::Ultraplan { phase } => render_ultraplan_detail(
            &session,
            phase.as_deref(),
            !props.on_kill.is_default(),
            pending,
            pending_menu,
            *theme,
        ),
    }
}

fn option(label: &str, value: &str) -> SelectOptionData {
    SelectOptionData {
        label: label.to_string(),
        value: value.to_string(),
        ..Default::default()
    }
}

fn render_review_detail(
    session: &RemoteSessionDetailData,
    can_kill: bool,
    mut pending: State<Option<InputAction>>,
    mut pending_menu: State<Option<MenuAction>>,
    theme: Theme,
) -> AnyElement<'static> {
    let completed = session.status == TaskStatus::Completed;
    let running = matches!(session.status, TaskStatus::Pending | TaskStatus::Running);
    let stages = [
        ("Setup", None),
        ("Find", Some(ReviewStage::Finding)),
        ("Verify", Some(ReviewStage::Verifying)),
        ("Dedupe", Some(ReviewStage::Synthesizing)),
    ];
    let stage_contents = stages
        .into_iter()
        .enumerate()
        .flat_map(|(index, (label, stage))| {
            let current = !completed
                && if !session.progress.has_review_progress {
                    stage.is_none()
                } else {
                    stage == session.progress.review_stage
                };
            let mut values = Vec::new();
            if index > 0 {
                values.push(MixedTextContent::new(" → ").weight(Weight::Light));
            }
            let mut value = MixedTextContent::new(label);
            value.color = current.then_some(theme.background);
            value.weight = if current {
                Weight::Normal
            } else {
                Weight::Light
            };
            values.push(value);
            values
        })
        .chain(completed.then(|| MixedTextContent::new(" ✓").color(theme.success)))
        .collect::<Vec<_>>();
    let mut menu = pending;
    let can_kill = can_kill && running;
    let options = if completed {
        vec![
            option("Open in nyan on the web", "open"),
            option("Dismiss", "dismiss"),
        ]
    } else {
        let mut options = vec![option("Open in nyan on the web", "open")];
        if can_kill {
            options.push(option("Stop ultrareview", "stop"));
        }
        options.push(option("Back", "back"));
        options
    };
    element! { Dialog(title: format!("{} ultrareview · {} · {}", if completed { crate::constants::figures::DIAMOND_FILLED } else { crate::constants::figures::DIAMOND_OPEN }, session.elapsed, if completed { "ready" } else if running { "running" } else { "failed" }), color: Some(theme.background), on_cancel: move |_| menu.set(Some(InputAction::Back))) {
        View(flex_direction: FlexDirection::Column, row_gap: 1u32) {
            MixedText(contents: stage_contents) Text(content: review_counts_line(session)) Text(content: session.session_url.clone(), dim: true)
            WizardChoice(options: options, on_select: move |value: String| pending_menu.set(Some(match value.as_str() {
                "open" => MenuAction::Open, "stop" => MenuAction::Stop, "dismiss" => MenuAction::Dismiss, _ => MenuAction::Back,
            })))
        }
    }}.into_any()
}

fn render_ultraplan_detail(
    session: &RemoteSessionDetailData,
    phase: Option<&str>,
    can_kill: bool,
    mut pending: State<Option<InputAction>>,
    mut pending_menu: State<Option<MenuAction>>,
    theme: Theme,
) -> AnyElement<'static> {
    let running = matches!(session.status, TaskStatus::Pending | TaskStatus::Running);
    let spawns = session
        .tool_uses
        .iter()
        .filter(|tool| matches!(tool.name.as_str(), "Agent" | "Task"))
        .count();
    let last = session
        .tool_uses
        .last()
        .map(|tool| format_tool_use_summary(&tool.name, &tool.input));
    let status = phase
        .map(|phase| {
            if phase == "needs_input" {
                "input required"
            } else {
                "ready"
            }
        })
        .unwrap_or(if running { "running" } else { "completed" });
    let verb = phase
        .map(|phase| {
            if phase == "needs_input" {
                "waiting"
            } else {
                "done"
            }
        })
        .unwrap_or("working");
    let mut menu = pending;
    let can_kill = can_kill && running;
    let mut options = vec![option("Review in nyan on the web", "open")];
    if can_kill {
        options.push(option("Stop ultraplan", "stop"));
    }
    options.push(option("Back", "back"));
    element! { Dialog(title: format!("{} ultraplan · {} · {status}", if phase == Some("plan_ready") { crate::constants::figures::DIAMOND_FILLED } else { crate::constants::figures::DIAMOND_OPEN }, session.elapsed), color: Some(theme.background), on_cancel: move |_| menu.set(Some(InputAction::Back))) {
        View(flex_direction: FlexDirection::Column, row_gap: 1u32) {
            Text(content: format!("{} {} {verb} · {} {}", 1 + spawns, if spawns == 0 { "agent" } else { "agents" }, session.tool_uses.len(), if session.tool_uses.len() == 1 { "tool call" } else { "tool calls" }))
            #(last.map(|last| element! { Text(content: last, dim: true) })) Text(content: session.session_url.clone(), dim: true)
            WizardChoice(options: options, on_select: move |value: String| pending_menu.set(Some(match value.as_str() {
                "open" => MenuAction::Open, "stop" => MenuAction::Stop, _ => MenuAction::Back,
            })))
        }
    }}.into_any()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tool_summary_prefers_plan_question_and_first_string() {
        assert_eq!(
            format_tool_use_summary("ExitPlanMode", &serde_json::json!({})),
            "Review the plan in nyan on the web"
        );
        assert_eq!(
            format_tool_use_summary(
                "AskUserQuestion",
                &serde_json::json!({"questions":[{"question":"Which   option?","header":"Choice"}]})
            ),
            "Answer in browser: Which option?"
        );
        assert_eq!(
            format_tool_use_summary("Bash", &serde_json::json!({"command":"echo   hi"})),
            "Bash echo hi"
        );
    }
    #[test]
    fn review_counts_distinguish_setup_completed_and_refuted() {
        let mut data = RemoteSessionDetailData {
            status: TaskStatus::Running,
            ..Default::default()
        };
        assert_eq!(review_counts_line(&data), "setting up");
        data.status = TaskStatus::Completed;
        assert_eq!(review_counts_line(&data), "done");
        data.progress.has_review_progress = true;
        data.progress.bugs_verified = 2;
        data.progress.bugs_refuted = 1;
        assert_eq!(review_counts_line(&data), "2 findings · 1 refuted");
    }
}
