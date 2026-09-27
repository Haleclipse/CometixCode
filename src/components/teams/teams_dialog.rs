//! Maps to: CC `components/teams/TeamsDialog.tsx:1-820`.
//!
//! The external build keeps this component read-only: pane focus/kill,
//! mailbox shutdown, hide/show, task mutation, and permission synchronization
//! are emitted as typed actions for a safety-reviewed owner instead of being
//! executed from rendering code.

use crate::components::design_system::dialog::Dialog;
use crate::keybindings::keybinding_context::KeybindingRuntime;
use crate::keybindings::types::ContextName;
use crate::keybindings::use_keybinding::use_keybinding;
use crate::types::permissions::PermissionMode;
use crate::utils::permissions::permission_mode::{
    get_mode_color, permission_mode_from_string, permission_mode_symbol,
};
use crate::utils::theme::{Theme, ThemeColorKey, mode_color};
use crate::utils::truncate::truncate_to_width;
use iocraft::prelude::*;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TeammateTaskData {
    pub id: String,
    pub subject: String,
    pub completed: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TeammateStatusData {
    pub agent_id: String,
    pub name: String,
    pub status: String,
    pub is_hidden: bool,
    pub mode: Option<String>,
    pub model: Option<String>,
    pub color: Option<ThemeColorKey>,
    pub worktree_path: Option<String>,
    pub cwd: Option<String>,
    pub prompt: Option<String>,
    pub tasks: Vec<TeammateTaskData>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TeamsDialogData {
    pub team_name: String,
    pub teammates: Vec<TeammateStatusData>,
    pub supports_hide_show: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TeamsDialogAction {
    View(String),
    Kill(String),
    Shutdown(String),
    ToggleVisibility(String),
    ToggleAllVisibility,
    PruneIdle(Vec<String>),
    CycleMode(String),
    CycleAllModes,
}
#[derive(Clone, Debug, PartialEq, Eq)]
enum Level {
    List,
    Detail(String),
}
#[derive(Clone, Copy)]
enum InputAction {
    Previous,
    Next,
    Accept,
    Back,
    Kill,
    Shutdown,
    Toggle,
    ToggleAll,
    PromptOrPrune,
    Cycle,
}

#[derive(Default, Props)]
pub struct TeamsDialogProps<'a> {
    pub data: Option<TeamsDialogData>,
    pub on_done: HandlerMut<'a, ()>,
    pub on_action: HandlerMut<'a, TeamsDialogAction>,
}

#[component]
pub fn TeamsDialog<'a>(
    props: &mut TeamsDialogProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let Some(data) = props.data.clone() else {
        return element! { Fragment }.into_any();
    };
    let mut level = hooks.use_state(|| Level::List);
    let mut selected = hooks.use_state(|| 0usize);
    let mut prompt_expanded = hooks.use_state(|| false);
    let mut pending = hooks.use_state(|| None::<InputAction>);
    let action = { *pending.read() };
    if let Some(action) = action {
        pending.set(None);
        let current = level.read().clone();
        let current_member = match &current {
            Level::List => data.teammates.get(selected.get()),
            Level::Detail(name) => data.teammates.iter().find(|member| &member.name == name),
        };
        match action {
            InputAction::Previous if current == Level::List => {
                selected.set(selected.get().saturating_sub(1))
            }
            InputAction::Next if current == Level::List => {
                selected.set((selected.get() + 1).min(data.teammates.len().saturating_sub(1)))
            }
            InputAction::Accept => match current {
                Level::List => {
                    if let Some(member) = current_member {
                        level.set(Level::Detail(member.name.clone()));
                    }
                }
                Level::Detail(_) => {
                    if let Some(member) = current_member {
                        (props.on_action)(TeamsDialogAction::View(member.name.clone()));
                        (props.on_done)(());
                    }
                }
            },
            InputAction::Back => match current {
                Level::Detail(_) => {
                    level.set(Level::List);
                    selected.set(0);
                }
                Level::List => (props.on_done)(()),
            },
            InputAction::Kill => {
                if let Some(member) = current_member {
                    (props.on_action)(TeamsDialogAction::Kill(member.name.clone()));
                }
            }
            InputAction::Shutdown => {
                if let Some(member) = current_member {
                    (props.on_action)(TeamsDialogAction::Shutdown(member.name.clone()));
                }
            }
            InputAction::Toggle if data.supports_hide_show => {
                if let Some(member) = current_member {
                    (props.on_action)(TeamsDialogAction::ToggleVisibility(member.name.clone()));
                }
            }
            InputAction::ToggleAll if current == Level::List && data.supports_hide_show => {
                (props.on_action)(TeamsDialogAction::ToggleAllVisibility)
            }
            InputAction::PromptOrPrune => match current {
                Level::Detail(_) => prompt_expanded.set(!prompt_expanded.get()),
                Level::List => {
                    let names = data
                        .teammates
                        .iter()
                        .filter(|member| member.status == "idle")
                        .map(|member| member.name.clone())
                        .collect::<Vec<_>>();
                    if !names.is_empty() {
                        (props.on_action)(TeamsDialogAction::PruneIdle(names));
                    }
                }
            },
            InputAction::Cycle => match current {
                Level::Detail(name) => (props.on_action)(TeamsDialogAction::CycleMode(name)),
                Level::List => {
                    if !data.teammates.is_empty() {
                        (props.on_action)(TeamsDialogAction::CycleAllModes);
                    }
                }
            },
            _ => {}
        }
    }
    let runtime = hooks
        .try_use_context::<KeybindingRuntime>()
        .map(|runtime| runtime.clone());
    let team_context = ContextName::Custom("Teams".to_string());
    for (name, context, action) in [
        (
            "teams:previous",
            team_context.clone(),
            InputAction::Previous,
        ),
        ("teams:next", team_context.clone(), InputAction::Next),
        ("teams:accept", team_context.clone(), InputAction::Accept),
        ("teams:back", team_context.clone(), InputAction::Back),
        ("teams:kill", team_context.clone(), InputAction::Kill),
        (
            "teams:shutdown",
            team_context.clone(),
            InputAction::Shutdown,
        ),
        (
            "teams:toggleVisibility",
            team_context.clone(),
            InputAction::Toggle,
        ),
        (
            "teams:toggleAllVisibility",
            team_context.clone(),
            InputAction::ToggleAll,
        ),
        (
            "teams:promptOrPrune",
            team_context.clone(),
            InputAction::PromptOrPrune,
        ),
        (
            "confirm:cycleMode",
            ContextName::Confirmation,
            InputAction::Cycle,
        ),
        ("confirm:no", ContextName::Confirmation, InputAction::Back),
    ] {
        let mut pending = pending;
        use_keybinding(
            &mut hooks,
            runtime.clone(),
            name,
            context,
            || true,
            move || {
                pending.set(Some(action));
                true
            },
        );
    }

    let theme = hooks.use_context::<Theme>();
    let current_level = { level.read().clone() };
    match current_level {
        Level::List => {
            let rows = data
                .teammates
                .iter()
                .enumerate()
                .map(|(index, teammate)| {
                    let selected_row = index == selected.get();
                    let idle = teammate.status == "idle";
                    let mode = teammate
                        .mode
                        .as_deref()
                        .map(permission_mode_from_string)
                        .unwrap_or(PermissionMode::Default);
                    let symbol = permission_mode_symbol(mode);
                    let mut pointer =
                        MixedTextContent::new(if selected_row { "❯ " } else { "  " });
                    pointer.color = selected_row.then_some(theme.suggestion);
                    let mut contents = vec![pointer];
                    if teammate.is_hidden {
                        contents.push(MixedTextContent::new("[hidden] ").weight(Weight::Light));
                    }
                    if idle {
                        contents.push(MixedTextContent::new("[idle] ").weight(Weight::Light));
                    }
                    if !symbol.is_empty() {
                        contents.push(
                            MixedTextContent::new(format!("{symbol} "))
                                .color(mode_color(&theme, get_mode_color(mode))),
                        );
                    }
                    let mut name = MixedTextContent::new(format!("@{}", teammate.name)).weight(
                        if idle && !selected_row {
                            Weight::Light
                        } else {
                            Weight::Normal
                        },
                    );
                    name.color = selected_row.then_some(theme.suggestion);
                    contents.push(name);
                    if let Some(model) = &teammate.model {
                        contents.push(
                            MixedTextContent::new(format!(" ({model})")).weight(Weight::Light),
                        );
                    }
                    element! { MixedText(contents: contents) }.into_any()
                })
                .collect::<Vec<_>>();
            let rows: Vec<AnyElement<'static>> = if rows.is_empty() {
                vec![element! { Text(content: "No teammates".to_string(), dim: true) }.into_any()]
            } else {
                rows
            };
            let mut done = pending;
            let footer = format!(
                "↑/↓ select · Enter view · k kill · s shutdown · p prune idle{} · shift+tab sync cycle modes for all · Esc close",
                if data.supports_hide_show {
                    " · h hide/show · H hide/show all"
                } else {
                    ""
                }
            );
            element! { View(flex_direction: FlexDirection::Column) {
                Dialog(title: format!("Team {}", data.team_name), subtitle: Some(format!("{} {}", data.teammates.len(), if data.teammates.len() == 1 { "teammate" } else { "teammates" })), color: Some(theme.background), hide_input_guide: true, on_cancel: move |_| done.set(Some(InputAction::Back))) {
                    #(rows)
                }
                View(margin_left: 1u32) { Text(content: footer, dim: true) }
            }}.into_any()
        }
        Level::Detail(name) => {
            let Some(teammate) = data.teammates.iter().find(|member| member.name == name) else {
                return element! { Fragment }.into_any();
            };
            let mode = teammate
                .mode
                .as_deref()
                .map(permission_mode_from_string)
                .unwrap_or(PermissionMode::Default);
            let symbol = permission_mode_symbol(mode);
            let mut title = Vec::new();
            if !symbol.is_empty() {
                title.push(
                    MixedTextContent::new(format!("{symbol} "))
                        .color(mode_color(&theme, get_mode_color(mode))),
                );
            }
            let mut teammate_title = MixedTextContent::new(format!("@{}", teammate.name));
            teammate_title.color = teammate.color.map(|key| theme.color(key));
            title.push(teammate_title);
            let working = teammate
                .worktree_path
                .as_ref()
                .map(|path| format!("worktree: {path}"))
                .or_else(|| teammate.cwd.clone());
            let subtitle = [teammate.model.clone(), working]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" · ");
            let task_rows = teammate
                .tasks
                .iter()
                .map(|task| {
                    let mut icon = MixedTextContent::new(if task.completed {
                        format!("{} ", crate::constants::figures::get().tick)
                    } else {
                        "◼ ".to_string()
                    });
                    let mut subject = MixedTextContent::new(task.subject.clone());
                    if task.completed {
                        icon.color = Some(theme.success);
                        subject.color = Some(theme.success);
                    }
                    element! { MixedText(contents: vec![icon, subject]) }
                })
                .collect::<Vec<_>>();
            let prompt = teammate.prompt.clone();
            let prompt_long = prompt
                .as_ref()
                .is_some_and(|prompt| unicode_width::UnicodeWidthStr::width(prompt.as_str()) > 80);
            let prompt_text = prompt.as_ref().map(|prompt| {
                if prompt_expanded.get() {
                    prompt.clone()
                } else {
                    truncate_to_width(prompt, 80)
                }
            });
            let mut back = pending;
            let footer = format!(
                "← back · Esc close · k kill · s shutdown{} · shift+tab cycle mode",
                if data.supports_hide_show {
                    " · h hide/show"
                } else {
                    ""
                }
            );
            element! { View(flex_direction: FlexDirection::Column) {
                Dialog(title_children: vec![element! { MixedText(contents: title) }.into_any()], subtitle: (!subtitle.is_empty()).then_some(subtitle), color: Some(theme.background), hide_input_guide: true, on_cancel: move |_| back.set(Some(InputAction::Back))) {
                    #((!task_rows.is_empty()).then(|| element! { View(flex_direction: FlexDirection::Column) { Text(content: "Tasks".to_string(), weight: Weight::Bold) #(task_rows) } }))
                    #(prompt_text.map(|prompt| element! { View(flex_direction: FlexDirection::Column) { Text(content: "Prompt".to_string(), weight: Weight::Bold) MixedText(contents: vec![MixedTextContent::new(prompt), MixedTextContent::new(if prompt_long && !prompt_expanded.get() { " (p to expand)" } else { "" }).weight(Weight::Light)]) } }))
                }
                View(margin_left: 1u32) { Text(content: footer, dim: true) }
            }}.into_any()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn data() -> TeamsDialogData {
        TeamsDialogData {
            team_name: "alpha".to_string(),
            teammates: vec![TeammateStatusData {
                agent_id: "1".to_string(),
                name: "reviewer".to_string(),
                status: "idle".to_string(),
                is_hidden: true,
                mode: Some("plan".to_string()),
                model: Some("opus".to_string()),
                color: Some(ThemeColorKey::AgentBlue),
                worktree_path: None,
                cwd: Some("/repo".to_string()),
                prompt: Some("p".repeat(90)),
                tasks: vec![TeammateTaskData {
                    id: "t".to_string(),
                    subject: "Review".to_string(),
                    completed: true,
                }],
            }],
            supports_hide_show: true,
        }
    }
    #[test]
    fn list_renders_status_mode_model_and_complete_footer_without_side_effects() {
        let text = element! { ContextProvider(value: Context::owned(*crate::utils::theme::current())) { TeamsDialog(data: Some(data())) } }.render(Some(140)).to_string();
        assert!(text.contains("Team alpha"));
        assert!(text.contains("[hidden] [idle] ⏸ @reviewer (opus)"));
        assert!(text.contains("k kill"));
        assert!(text.contains("H hide/show all"));
    }
}
