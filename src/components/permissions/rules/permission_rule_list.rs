//! Maps to: CC `components/permissions/rules/PermissionRuleList.tsx`.
//!
//! This module extracts the official permission-rules tab model and rule option
//! shaping from the command panel. Workspace mutations and their accumulated
//! exit messages remain with this source owner; persistence runs off-frame.
//!
//! Two Selects are driven by the CustomSelect hooks (`use_select_state` +
//! `use_select_input`), matching CC's autonomous `<Select>`s:
//! - `RuleDetails` delete confirmation (PermissionRuleList.tsx:153-160)
//! - the per-tab rule list (PermissionRuleList.tsx:211-219)

use super::add_permission_rules::{AddPermissionRules, AddPermissionRulesOutcome};
use super::add_workspace_directory::{AddWorkspaceDirectory, AddWorkspaceDirectorySelection};
use super::permission_rule_description::PermissionRuleDescription;
use super::permission_rule_input::{PermissionRuleInput, PermissionRuleInputSubmit};
use super::recent_denials_tab::{
    RecentDenialsSnapshot, RecentDenialsStateChange, RecentDenialsTab,
};
use super::remove_workspace_directory::RemoveWorkspaceDirectory;
use super::workspace_tab::WorkspaceTab;
use crate::components::custom_select::{
    Select, SelectInputOptionMeta, SelectLayout, SelectOptionData, UseSelectInputOptions,
    UseSelectStateProps, use_select_input, use_select_state,
};
use crate::components::design_system::pane::Pane;
use crate::components::design_system::tabs::{
    TabItem, TabsHeader, next_tab_index, previous_tab_index,
};
use crate::components::search_box::SearchBox;
use crate::hooks::use_exit::use_exit_on_ctrl_cd_with_keybindings;
use crate::hooks::use_search_input::use_search_input;
use crate::keybindings::types::ContextName;
use crate::keybindings::use_keybinding::use_keybinding;
use crate::tool::ToolPermissionContext;
use crate::types::permissions::{
    PermissionBehavior, PermissionRule, PermissionRuleSource, PermissionRuleValue,
};
use crate::types::permissions::{PermissionUpdate, PermissionUpdateDestination};
use crate::utils::auto_mode_denials::{AutoModeDenial, get_auto_mode_denials};
use crate::utils::permissions::permission_rule_parser::permission_rule_value_to_string;
use crate::utils::permissions::permission_update::{
    apply_permission_update, persist_permission_update,
};
use crate::utils::permissions::permissions::permission_rule_source_display_string;
use crate::utils::permissions::permissions::{
    delete_permission_rule, get_allow_rules, get_ask_rules, get_deny_rules,
};
use crate::utils::theme::Theme;
use crate::utils::worktree::CommandResultDisplay;
use chalk::Chalk;
use indexmap::IndexMap;
use iocraft::prelude::*;
use std::sync::Arc;

pub const ADD_NEW_RULE_VALUE: &str = "add-new-rule";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RulesTabType {
    Recent,
    Allow,
    Ask,
    Deny,
    Workspace,
}

impl RulesTabType {
    pub fn label(self) -> &'static str {
        match self {
            Self::Recent => "Recently denied",
            Self::Allow => "Allow",
            Self::Ask => "Ask",
            Self::Deny => "Deny",
            Self::Workspace => "Workspace",
        }
    }

    pub fn behavior(self) -> Option<PermissionBehavior> {
        match self {
            Self::Allow => Some(PermissionBehavior::Allow),
            Self::Ask => Some(PermissionBehavior::Ask),
            Self::Deny => Some(PermissionBehavior::Deny),
            Self::Recent | Self::Workspace => None,
        }
    }

    pub fn subtitle(self) -> Option<&'static str> {
        match self {
            Self::Allow => Some("Claude Code won't ask before using allowed tools."),
            Self::Ask => {
                Some("Claude Code will always ask for confirmation before using these tools.")
            }
            Self::Deny => Some("Claude Code will always reject requests to use denied tools."),
            Self::Recent | Self::Workspace => None,
        }
    }
}

pub fn all_rule_tabs() -> [RulesTabType; 5] {
    [
        RulesTabType::Recent,
        RulesTabType::Allow,
        RulesTabType::Ask,
        RulesTabType::Deny,
        RulesTabType::Workspace,
    ]
}

/// Maps to: CC `getRuleBehaviorLabel(...)`.
pub fn get_rule_behavior_label(rule_behavior: PermissionBehavior) -> &'static str {
    match rule_behavior {
        PermissionBehavior::Allow => "allowed",
        PermissionBehavior::Deny => "denied",
        PermissionBehavior::Ask => "ask",
    }
}

/// Maps to: CC `RuleSourceText`.
pub fn rule_source_text(rule: &PermissionRule) -> String {
    format!(
        "From {}",
        permission_rule_source_display_string(rule.source)
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuleDetailsState {
    Managed { rule: PermissionRule },
    ConfirmDelete { rule: PermissionRule },
}

/// Maps to: CC `RuleDetails` managed-settings branch.
pub fn rule_details_title(state: &RuleDetailsState) -> String {
    match state {
        RuleDetailsState::Managed { .. } => "Rule details".to_string(),
        RuleDetailsState::ConfirmDelete { rule } => format!(
            "Delete {} tool?",
            get_rule_behavior_label(rule.rule_behavior)
        ),
    }
}

#[derive(Default, Props)]
pub struct RuleDetailsProps<'a> {
    /// Off-frame carrier for deletePermissionRule's synchronous source section.
    /// No second action may enter before its setter/summary transition settles.
    pub action_pending: bool,
    /// Managed vs delete-confirmation branch (CC selects by
    /// `rule.source === 'policySettings'`, PermissionRuleList.tsx:112).
    pub details: Option<RuleDetailsState>,
    /// Maps to: CC `onDelete` (PermissionRuleList.tsx:154 'yes' branch).
    pub on_delete: HandlerMut<'a, ()>,
    /// Maps to: CC `onCancel` (non-yes selection and Select `onCancel`).
    pub on_cancel: HandlerMut<'a, ()>,
}

/// Maps to: CC `RuleDetails` (PermissionRuleList.tsx:80-165) — rule details,
/// interactive deletion, configurable cancel action, and double-press exit
/// footer.
#[component]
pub fn RuleDetails<'a>(
    props: &mut RuleDetailsProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let details_active = props.details.is_some() && !props.action_pending;
    let action_pending = props.action_pending;
    hooks.use_propagated_terminal_events(move |event| {
        if action_pending {
            event.stop_propagation();
        }
    });
    let exit_state = use_exit_on_ctrl_cd_with_keybindings(&mut hooks, details_active);
    let mut pending_managed_cancel = hooks.use_state(|| false);
    let runtime = hooks
        .try_use_context::<crate::keybindings::keybinding_context::KeybindingRuntime>()
        .map(|runtime| runtime.clone());
    use_keybinding(
        &mut hooks,
        runtime,
        "confirm:no",
        ContextName::Confirmation,
        move || details_active,
        move || {
            pending_managed_cancel.set(true);
            true
        },
    );
    if pending_managed_cancel.get() {
        pending_managed_cancel.set(false);
        (props.on_cancel)(());
    }
    let footer = exit_state
        .key_name
        .map(|key| format!("Press {key} again to exit"))
        .unwrap_or_else(|| "Esc to cancel".to_string());
    // Maps to: CC PermissionRuleList.tsx:156-159 — Yes/No options.
    let options = vec![
        SelectOptionData {
            label: "Yes".to_string(),
            value: "yes".to_string(),
            ..Default::default()
        },
        SelectOptionData {
            label: "No".to_string(),
            value: "no".to_string(),
            ..Default::default()
        },
    ];
    let is_confirm = matches!(props.details, Some(RuleDetailsState::ConfirmDelete { .. }));
    // Maps to: CC PermissionRuleList.tsx:153-160 — autonomous `<Select
    // onChange={_ => (_ === 'yes' ? onDelete() : onCancel())}
    // onCancel={onCancel}/>` (visibleOptionCount defaults to 5,
    // select.tsx:207). The managed branch renders no Select in CC; hooks run
    // unconditionally here, so `is_disabled` gates the input layer.
    let state = use_select_state(
        &mut hooks,
        UseSelectStateProps {
            visible_option_count: Some(5),
            values: options.iter().map(|option| option.value.clone()).collect(),
            default_value: None,
            focus_value: None,
        },
    );
    let events = use_select_input(
        &mut hooks,
        state,
        UseSelectInputOptions {
            is_disabled: !is_confirm || props.action_pending,
            has_on_cancel: true,
            option_metas: options
                .iter()
                .map(|option| SelectInputOptionMeta {
                    value: option.value.clone(),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        },
    );

    // Maps to: CC PermissionRuleList.tsx:154 — 'yes' deletes, anything else
    // cancels.
    if let Some(value) = events.take_accepted() {
        if value == "yes" {
            (props.on_delete)(());
        } else {
            (props.on_cancel)(());
        }
    }
    // Maps to: CC PermissionRuleList.tsx:155 `onCancel={onCancel}`.
    if events.take_cancelled() {
        (props.on_cancel)(());
    }

    let navigation = state.navigation.snapshot();
    let focused_index = navigation.focused_index().unwrap_or(0);
    let Some(details) = props.details.clone() else {
        return element!(View).into_any();
    };
    let title = rule_details_title(&details);
    let rule = match &details {
        RuleDetailsState::Managed { rule } | RuleDetailsState::ConfirmDelete { rule } => {
            rule.clone()
        }
    };
    // CC PermissionRuleList.tsx:93-99 — bold rule string + description +
    // source line, shared by both branches.
    let rule_description = |rule: &PermissionRule| {
        element! {
            View(flex_direction: FlexDirection::Column, margin_left: 2u32, margin_right: 2u32) {
                Text(content: permission_rule_value_to_string(&rule.rule_value), weight: Weight::Bold)
                PermissionRuleDescription(rule_value: Some(rule.rule_value.clone()))
                Text(content: rule_source_text(rule), dim: true)
            }
        }
    };

    match details {
        // CC PermissionRuleList.tsx:112-136 — managed settings can't be
        // edited; no Select.
        RuleDetailsState::Managed { .. } => element! {
            View(flex_direction: FlexDirection::Column) {
                View(
                    flex_direction: FlexDirection::Column,
                    row_gap: 1u32,
                    border_style: BorderStyle::Round,
                    border_color: theme.permission,
                    padding_left: 1u32,
                    padding_right: 1u32,
                ) {
                    Text(content: title.clone(), weight: Weight::Bold, color: theme.permission)
                    #(Some(rule_description(&rule)))
                    Text(
                        content: "This rule is configured by managed settings and cannot be modified.\nContact your system administrator for more information.".to_string(),
                        italic: true,
                        wrap: TextWrap::Wrap,
                    )
                }
                View(margin_left: 3u32) {
                    Text(content: footer.clone(), dim: true)
                }
            }
        }
        .into_any(),
        // CC PermissionRuleList.tsx:138-164 — delete confirmation.
        RuleDetailsState::ConfirmDelete { .. } => element! {
            View(flex_direction: FlexDirection::Column) {
                View(
                    flex_direction: FlexDirection::Column,
                    row_gap: 1u32,
                    border_style: BorderStyle::Round,
                    border_color: theme.error,
                    padding_left: 1u32,
                    padding_right: 1u32,
                ) {
                    Text(content: title.clone(), weight: Weight::Bold, color: theme.error)
                    #(Some(rule_description(&rule)))
                    Text(content: "Are you sure you want to delete this permission rule?".to_string())
                    Select(
                        options: options.clone(),
                        focused_index: focused_index,
                        visible_option_count: navigation.visible_option_count,
                        visible_from_index: navigation.visible_from_index,
                        layout: SelectLayout::Compact,
                    )
                }
                View(margin_left: 3u32) {
                    Text(content: footer.clone(), dim: true)
                }
            }
        }
        .into_any(),
    }
}

/// Maps to: CC PermissionRuleList.tsx Props.onExit result/options arguments.
/// Native callback payload; `None` display keeps the command's user default.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PermissionRuleListExit {
    pub result: Option<String>,
    pub display: Option<CommandResultDisplay>,
    pub should_query: bool,
    pub meta_messages: Vec<String>,
}

#[derive(Default, Props)]
pub struct PermissionRuleListProps<'a> {
    /// Isolated fixture carrier; production subscribes to AppStore.
    pub context: ToolPermissionContext,
    pub denials_override: Option<Arc<Vec<AutoModeDenial>>>,
    pub initial_tab: Option<RulesTabType>,
    pub on_retry_denials: HandlerMut<'a, Vec<String>>,
    pub on_exit: HandlerMut<'a, PermissionRuleListExit>,
}

/// Maps to: CC PermissionRuleList.tsx:319-344,568-574 jsonStringify(rule).
/// Preserve the source object field order and JS absent ruleContent key.
fn rule_key(rule: &PermissionRule) -> String {
    let mut value = serde_json::to_value(rule).expect("permission rule is serializable");
    if rule.rule_value.rule_content.is_none() {
        value["ruleValue"]
            .as_object_mut()
            .unwrap()
            .shift_remove("ruleContent");
    }
    serde_json::to_string(&value).expect("permission rule is serializable")
}

/// Maps to: CC PermissionRuleList.tsx:319-413 maps and getRulesOptions callback.
fn get_rules_options(
    context: &ToolPermissionContext,
    tab: RulesTabType,
    query: &str,
) -> (Vec<SelectOptionData>, IndexMap<String, PermissionRule>) {
    let rules = match tab {
        RulesTabType::Allow => get_allow_rules(context),
        RulesTabType::Ask => get_ask_rules(context),
        RulesTabType::Deny => get_deny_rules(context),
        _ => Vec::new(),
    };
    let rules_by_key: IndexMap<_, _> = rules
        .into_iter()
        .map(|rule| (rule_key(&rule), rule))
        .collect();
    let mut options = Vec::new();
    if tab.behavior().is_some() && query.is_empty() {
        options.push(SelectOptionData {
            label: "Add a new rule…".into(),
            value: ADD_NEW_RULE_VALUE.into(),
            ..Default::default()
        });
    }
    let mut sorted: Vec<_> = rules_by_key.iter().collect();
    sorted.sort_by(|(_, a), (_, b)| {
        crate::tools::grep_tool::javascript_locale_compare(
            &permission_rule_value_to_string(&a.rule_value).to_lowercase(),
            &permission_rule_value_to_string(&b.rule_value).to_lowercase(),
        )
    });
    let lower_query = query.to_lowercase();
    for (key, rule) in sorted {
        let label = permission_rule_value_to_string(&rule.rule_value);
        if !query.is_empty() && !label.to_lowercase().contains(&lower_query) {
            continue;
        }
        options.push(SelectOptionData {
            label,
            value: key.clone(),
            ..Default::default()
        });
    }
    (options, rules_by_key)
}

/// Maps to: CC PermissionRuleList.tsx:522-558 handleRulesCancel. The optional
/// first value is delivered to onRetryDenials before onExit, preserving order.
fn handle_rules_cancel(
    snapshot: &RecentDenialsSnapshot,
    changes: &[String],
    chalk: &Chalk,
) -> (Option<Vec<String>>, PermissionRuleListExit) {
    let commands: Vec<_> = snapshot
        .retry
        .iter()
        .filter_map(|index| snapshot.denials.get(*index))
        .map(|denial| denial.display.clone())
        .collect();
    if !commands.is_empty() {
        let meta = format!(
            "Permission granted for: {}. You may now retry {} if you would like.",
            commands.join(", "),
            if commands.len() == 1 {
                "this command"
            } else {
                "these commands"
            }
        );
        return (
            Some(commands),
            PermissionRuleListExit {
                should_query: true,
                meta_messages: vec![meta],
                ..Default::default()
            },
        );
    }
    let approved: Vec<_> = snapshot
        .approved
        .iter()
        .filter_map(|index| snapshot.denials.get(*index))
        .map(|denial| chalk.clone().bold().apply(&denial.display))
        .collect();
    let mut messages = Vec::new();
    if !approved.is_empty() {
        messages.push(format!("Approved {}", approved.join(", ")));
    }
    messages.extend_from_slice(changes);
    (
        None,
        if messages.is_empty() {
            PermissionRuleListExit {
                result: Some("Permissions dialog dismissed".into()),
                display: Some(CommandResultDisplay::System),
                ..Default::default()
            }
        } else {
            PermissionRuleListExit {
                result: Some(messages.join("\n")),
                ..Default::default()
            }
        },
    )
}

#[derive(Default, Props)]
struct RulesTabContentProps<'a> {
    options: Vec<SelectOptionData>,
    search_query: String,
    is_search_mode: bool,
    is_focused: bool,
    header_focused: bool,
    cursor_offset: usize,
    last_focused_rule_key: Option<String>,
    on_select: HandlerMut<'a, String>,
    on_cancel: HandlerMut<'a, ()>,
    on_focus_header: HandlerMut<'a, bool>,
}

/// Maps to: CC PermissionRuleList.tsx:180-224 RulesTabContent.
#[component]
fn RulesTabContent<'a>(
    props: &mut RulesTabContentProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    if props.is_search_mode && props.header_focused {
        (props.on_focus_header)(false);
    }
    let disabled = props.is_search_mode || props.header_focused;
    let state = use_select_state(
        &mut hooks,
        UseSelectStateProps {
            visible_option_count: Some(10.min(props.options.len())),
            values: props
                .options
                .iter()
                .map(|option| option.value.clone())
                .collect(),
            focus_value: props.last_focused_rule_key.clone(),
            default_value: None,
        },
    );
    let events = use_select_input(
        &mut hooks,
        state,
        UseSelectInputOptions {
            is_disabled: disabled,
            has_on_cancel: true,
            has_on_up_from_first_item: true,
            option_metas: props
                .options
                .iter()
                .map(|option| SelectInputOptionMeta {
                    value: option.value.clone(),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        },
    );
    if let Some(value) = events.take_accepted() {
        (props.on_select)(value);
    }
    if events.take_cancelled() {
        (props.on_cancel)(());
    }
    if events.take_up_from_first_item() {
        (props.on_focus_header)(true);
    }
    let navigation = state.navigation.snapshot();
    element! {
        View(flex_direction: FlexDirection::Column) {
            View(margin_bottom: 1u32, flex_direction: FlexDirection::Column) {
                SearchBox(query: props.search_query.clone(), is_focused: props.is_search_mode && !props.header_focused,
                    is_terminal_focused: props.is_focused, cursor_offset: Some(props.cursor_offset))
            }
            Select(options: props.options.clone(), focused_index: navigation.focused_index().unwrap_or(0),
                visible_option_count: navigation.visible_option_count, visible_from_index: navigation.visible_from_index,
                is_disabled: disabled, layout: SelectLayout::Compact)
        }
    }
}

#[derive(Default, Props)]
struct PermissionRulesTabProps<'a> {
    tab: Option<RulesTabType>,
    context: Arc<ToolPermissionContext>,
    search_query: String,
    is_search_mode: bool,
    is_focused: bool,
    header_focused: bool,
    cursor_offset: usize,
    last_focused_rule_key: Option<String>,
    on_select: HandlerMut<'a, String>,
    on_cancel: HandlerMut<'a, ()>,
    on_focus_header: HandlerMut<'a, bool>,
}

/// Maps to: CC PermissionRuleList.tsx:227-265 PermissionRulesTab.
#[component]
fn PermissionRulesTab<'a>(
    props: &mut PermissionRulesTabProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    // Native transport for the source's borrowed callbacks: the retained child
    // queues events, and this owner invokes the current render's handlers.
    let mut selected = hooks.use_state(|| None::<String>);
    let mut cancelled = hooks.use_state(|| false);
    let mut focus_header = hooks.use_state(|| None::<bool>);
    let value = selected.read().clone();
    if let Some(value) = value {
        selected.set(None);
        (props.on_select)(value);
    }
    if cancelled.get() {
        cancelled.set(false);
        (props.on_cancel)(());
    }
    let focused = *focus_header.read();
    if let Some(focused) = focused {
        focus_header.set(None);
        (props.on_focus_header)(focused);
    }
    let tab = props.tab.unwrap_or(RulesTabType::Allow);
    element! {
        View(flex_direction: FlexDirection::Column, flex_shrink: if tab == RulesTabType::Allow { 0.0 } else { 1.0 }) {
            Text(content: tab.subtitle().unwrap_or_default(), wrap: TextWrap::Wrap)
            RulesTabContent(options: get_rules_options(&props.context, tab, &props.search_query).0,
                search_query: props.search_query.clone(), is_search_mode: props.is_search_mode, is_focused: props.is_focused,
                header_focused: props.header_focused, cursor_offset: props.cursor_offset,
                last_focused_rule_key: props.last_focused_rule_key.clone(),
                on_select: move |value| selected.set(Some(value)), on_cancel: move |_| cancelled.set(true), on_focus_header: move |value| focus_header.set(Some(value)))
        }
    }
}

/// Native placement carrier for CC PermissionRuleList.tsx:561-565's
/// parent useKeybinding listener. Ink use-input.ts:67-84 retains registration
/// order; iocraft polls descendants first. Mount before the changing tab body
/// so a later Workspace Select cannot overtake the existing parent listener.
#[derive(Default, Props)]
struct PermissionRuleListCancelBindingProps {
    active: bool,
    pending: Option<State<bool>>,
}

/// Maps to: CC `components/permissions/rules/PermissionRuleList.tsx:561-565`
/// useKeybinding call, represented only as a listener placement boundary.
#[component]
fn PermissionRuleListCancelBinding(
    props: &PermissionRuleListCancelBindingProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let runtime = hooks
        .try_use_context::<crate::keybindings::keybinding_context::KeybindingRuntime>()
        .map(|runtime| runtime.clone());
    let active = props.active;
    let mut pending = props.pending;
    #[cfg(test)]
    if crate::utils::process_env::var_os("COMETIX_PERMISSION_CANCEL_DIAGNOSTIC").is_some() {
        eprintln!("cancel binding render active={active}");
        hooks.use_terminal_events({
            let runtime = runtime.clone();
            move |event| {
                eprintln!(
                    "cancel binding event={event:?} active={active} contexts={:?}",
                    runtime.as_ref().map(|r| r.active_contexts())
                )
            }
        });
    }
    use_keybinding(
        &mut hooks,
        runtime,
        "confirm:no",
        ContextName::Settings,
        move || active,
        move || {
            #[cfg(test)]
            if crate::utils::process_env::var_os("COMETIX_PERMISSION_CANCEL_DIAGNOSTIC").is_some() {
                eprintln!("cancel binding invoked active={active}");
            }
            if let Some(pending) = pending.as_mut() {
                pending.set(true);
            }
            true
        },
    );
    element! { View(width: 0u32, height: 0u32) }
}

/// Maps to: CC `PermissionRuleList` (PermissionRuleList.tsx:268-799), including
/// its Tabs-owned selected-tab/header-focus state and WorkspaceTab child.
#[component]
pub fn PermissionRuleList<'a>(
    props: &mut PermissionRuleListProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    // CC useAppState/useSetAppState; the explicit context is only an isolated
    // fixture carrier. Production always reads the canonical subscribed store.
    let subscribed =
        crate::state::app_state::use_app_state_maybe_outside_of_provider(&mut hooks, |state| {
            state.tool_permission_context.clone()
        });
    let store = hooks
        .try_use_context::<crate::state::store::AppStore>()
        .map(|store| store.clone());
    let mut fixture_context = hooks.use_state(|| None::<Arc<ToolPermissionContext>>);
    let context = subscribed.unwrap_or_else(|| {
        fixture_context
            .read()
            .clone()
            .unwrap_or_else(|| Arc::new(props.context.clone()))
    });
    // CC PermissionRuleList's chalk.bold/dim/yellow calls, one for one: the
    // stdout singleton level makes apply() a passthrough when colors are off,
    // and applyStyle handles nested closes and line-break encasing.
    let chalk = Chalk::new();
    let mut changes = hooks.use_state(Vec::<String>::new);
    let mut selected_rule = hooks.use_state(|| None::<PermissionRule>);
    let mut deleting_rule = hooks.use_state(|| false);
    let mut last_focused_rule_key = hooks.use_state(|| None::<String>);
    let mut adding_rule_to_tab = hooks.use_state(|| None::<RulesTabType>);
    let mut validated_rule = hooks.use_state(|| None::<PermissionRuleInputSubmit>);
    let mut is_search_mode = hooks.use_state(|| false);
    let mut search = use_search_input(&mut hooks, "");
    let terminal_focused = hooks.use_terminal_focus();
    let denial_state_ref =
        hooks.use_const(|| Arc::new(std::sync::Mutex::new(RecentDenialsSnapshot::default())));
    let handle_denial_state_change: RecentDenialsStateChange = hooks.use_const({
        let reference = denial_state_ref.clone();
        move || {
            Arc::new(move |snapshot| *reference.lock().unwrap() = snapshot)
                as RecentDenialsStateChange
        }
    });
    let rule_results = hooks.use_const(|| {
        Arc::new(async_channel::unbounded::<(
            Option<Arc<ToolPermissionContext>>,
            Option<AddPermissionRulesOutcome>,
            Option<String>,
        )>())
    });
    let receiver = rule_results.1.clone();
    hooks.use_future({
        let chalk = chalk.clone();
        async move {
            while let Ok((fixture, outcome, deleted)) = receiver.recv().await {
                if let Some(updated) = fixture {
                    fixture_context.set(Some(updated));
                }
                if let Some(message) = deleted {
                    let mut next = changes.read().clone();
                    next.push(message);
                    changes.set(next);
                    deleting_rule.set(false);
                    selected_rule.set(None);
                }
                if let Some(outcome) = outcome {
                    validated_rule.set(None);
                    let mut next = changes.read().clone();
                    for rule in outcome.rules {
                        next.push(format!(
                            "Added {} rule {}",
                            match rule.rule_behavior {
                                PermissionBehavior::Allow => "allow",
                                PermissionBehavior::Ask => "ask",
                                PermissionBehavior::Deny => "deny",
                            },
                            chalk
                                .clone()
                                .bold()
                                .apply(&permission_rule_value_to_string(&rule.rule_value))
                        ));
                    }
                    for warning in outcome.unreachable.into_iter().flatten() {
                        next.push(chalk.clone().yellow().apply(&format!(
                    "⚠ Warning: {} is {}",
                    permission_rule_value_to_string(&warning.rule.rule_value),
                    if warning.shadow_type
                        == crate::utils::permissions::shadowed_rule_detection::ShadowType::Deny
                    {
                        "blocked"
                    } else {
                        "shadowed"
                    }
                                        )));
                        next.push(chalk.clone().dim().apply(&format!("  {}", warning.reason)));
                        next.push(
                            chalk
                                .clone()
                                .dim()
                                .apply(&format!("  Fix: {}", warning.fix)),
                        );
                    }
                    changes.set(next);
                }
            }
        }
    });
    let mut is_adding_workspace_directory = hooks.use_state(|| false);
    let mut removing_directory = hooks.use_state(|| None::<String>);
    // Source async callbacks may settle after the child (or this parent) is
    // unmounted. Only local UI delivery uses this inbox; external effects do not.
    let adding_results = hooks.use_const(|| {
        Arc::new(async_channel::unbounded::<(
            Option<Arc<ToolPermissionContext>>,
            String,
        )>())
    });
    let receiver = adding_results.1.clone();
    hooks.use_future(async move {
        while let Ok((fixture, message)) = receiver.recv().await {
            if let Some(updated) = fixture {
                fixture_context.set(Some(updated));
            }
            let mut next = changes.read().clone();
            next.push(message);
            changes.set(next);
            is_adding_workspace_directory.set(false);
        }
    });
    let in_directory_dialog = is_adding_workspace_directory.get()
        || removing_directory.read().is_some()
        || selected_rule.read().is_some()
        || adding_rule_to_tab.read().is_some()
        || validated_rule.read().is_some();
    let has_denials = !props
        .denials_override
        .clone()
        .unwrap_or_else(get_auto_mode_denials)
        .is_empty();
    let exit_state = use_exit_on_ctrl_cd_with_keybindings(&mut hooks, true);
    let initial_tab = props.initial_tab.unwrap_or(if has_denials {
        RulesTabType::Recent
    } else {
        RulesTabType::Allow
    });
    let mut selected_tab_state = hooks.use_state(move || initial_tab);
    let mut header_focused = hooks.use_state(move || !has_denials);
    // CC early returns unmount Tabs; restore its defaults on remount.
    let mut was_directory_dialog = hooks.use_state(|| false);
    if was_directory_dialog.get() != in_directory_dialog {
        was_directory_dialog.set(in_directory_dialog);
        if !in_directory_dialog {
            selected_tab_state.set(initial_tab);
            header_focused.set(!has_denials);
        }
    }
    let mut pending_exit_result = hooks.use_state(|| Option::<PermissionRuleListExit>::None);
    let mut pending_cancel = hooks.use_state(|| false);

    // Maps to: CC PermissionRuleList.tsx:653-680 `<Tabs defaultTab=...>` and
    // Tabs.tsx:152-161. The defining PermissionRuleList owner retains the tab
    // state; the command adapter only mounts this component.
    crate::components::design_system::tabs::use_tabs_keybindings(
        &mut hooks,
        // CC `navFromContent={!isSearchMode}` keeps ←/→/Tab available after
        // Down hands focus to RecentDenialsTab or another tab body.
        !is_search_mode.get() && !in_directory_dialog,
        {
            let mut selected_tab_state = selected_tab_state;
            let mut header_focused = header_focused;
            move || {
                let tabs = all_rule_tabs();
                let current = tabs
                    .iter()
                    .position(|tab| *tab == selected_tab_state.get())
                    .unwrap_or(0);
                selected_tab_state.set(tabs[next_tab_index(current, tabs.len())]);
                header_focused.set(true);
            }
        },
        {
            let mut selected_tab_state = selected_tab_state;
            let mut header_focused = header_focused;
            move || {
                let tabs = all_rule_tabs();
                let current = tabs
                    .iter()
                    .position(|tab| *tab == selected_tab_state.get())
                    .unwrap_or(0);
                selected_tab_state.set(tabs[previous_tab_index(current, tabs.len())]);
                header_focused.set(true);
            }
        },
    );

    // Maps to: CC Tabs.tsx:163-171. All currently mounted PermissionRuleList
    // tab bodies opt into the header/content focus handoff; Down enables the
    // selected child Select and is consumed at the same parent boundary.
    hooks.use_propagated_terminal_events(move |event| {
        let TerminalEvent::Key(KeyEvent { code, kind, .. }) = event.event() else {
            return;
        };
        if !in_directory_dialog
            && *kind != KeyEventKind::Release
            && header_focused.get()
            && matches!(code, KeyCode::Down)
        {
            header_focused.set(false);
            event.stop_propagation();
        }
    });

    let top_level_cancel_active = !in_directory_dialog && !is_search_mode.get();

    if pending_cancel.get() {
        #[cfg(test)]
        if crate::utils::process_env::var_os("COMETIX_PERMISSION_CANCEL_DIAGNOSTIC").is_some() {
            eprintln!("parent pending_cancel");
        }
        pending_cancel.set(false);
        let (commands, result) =
            handle_rules_cancel(&denial_state_ref.lock().unwrap(), &changes.read(), &chalk);
        if let Some(commands) = commands {
            (props.on_retry_denials)(commands);
        }
        pending_exit_result.set(Some(result));
    }
    let completed_exit = { pending_exit_result.read().clone() };
    if let Some(result) = completed_exit {
        #[cfg(test)]
        if crate::utils::process_env::var_os("COMETIX_PERMISSION_CANCEL_DIAGNOSTIC").is_some() {
            eprintln!("parent on_exit={result:?}");
        }
        pending_exit_result.set(None);
        (props.on_exit)(result);
    }
    let search_active = !in_directory_dialog && is_search_mode.get();
    // CC App.tsx:615-620 always dispatches DOM keydown after InputEvent,
    // even when Select handled a numeric shortcut. iocraft propagation has
    // only one stopping lane, so observe entry independently; both source
    // callbacks still see this render's pre-event state. The original Kitty
    // Allow -> Down -> 2 -> Esc oracle retains query "2" on returning.
    let entering_search = !in_directory_dialog && !is_search_mode.get();
    hooks.use_terminal_events(move |event| {
        if !entering_search {
            return;
        }
        let TerminalEvent::Key(KeyEvent {
            code: KeyCode::Char(character),
            modifiers,
            kind,
            ..
        }) = event
        else {
            return;
        };
        if kind == KeyEventKind::Release
            || modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return;
        }
        if character == '/' {
            is_search_mode.set(true);
            search.clear();
        } else if character.len_utf16() == 1
            && !matches!(character, 'j' | 'k' | 'm' | 'i' | 'r' | ' ')
        {
            is_search_mode.set(true);
            search.set(character.to_string());
        }
    });
    // Maps to: CC useSearchInput.ts's InputEvent editing callback.
    hooks.use_propagated_terminal_events(move |event| {
        if in_directory_dialog {
            return;
        }
        match event.event() {
            TerminalEvent::Paste(text) if search_active => {
                search.reset_key_state(&KeyCode::Char(' '), &KeyModifiers::empty());
                search.insert_text(text);
                event.stop_propagation();
            }
            TerminalEvent::Key(KeyEvent {
                code,
                modifiers,
                kind,
                ..
            }) if *kind != KeyEventKind::Release => {
                if search_active {
                    search.reset_key_state(code, modifiers);
                    match code {
                        KeyCode::Esc => {
                            if search.is_empty() {
                                is_search_mode.set(false);
                            } else {
                                search.clear();
                            }
                        }
                        KeyCode::Enter | KeyCode::Down => is_search_mode.set(false),
                        KeyCode::Up => {} // source onExitUp absent
                        KeyCode::Backspace
                            if search.is_empty() && !modifiers.contains(KeyModifiers::ALT) =>
                        {
                            is_search_mode.set(false)
                        }
                        KeyCode::Char('h' | 'H' | 'd' | 'D')
                            if search.is_empty() && modifiers.contains(KeyModifiers::CONTROL) =>
                        {
                            is_search_mode.set(false)
                        }
                        _ => {
                            search.handle_edit_key(code, modifiers);
                        }
                    }
                    event.stop_propagation();
                }
            }
            _ => {}
        }
    });
    let selected_tab = selected_tab_state.get();
    let query = search.text();
    let tabs = all_rule_tabs()
        .iter()
        .map(|tab| TabItem::new(tab.label(), tab.label()))
        .collect::<Vec<_>>();
    let is_header_focused = header_focused.get();
    let footer = if is_header_focused {
        "←/→ tab switch · ↓ return · Esc cancel"
    } else if is_search_mode.get() {
        "Type to filter · Enter/↓ select · ↑ tabs · Esc clear"
    } else if has_denials && initial_tab == RulesTabType::Recent {
        "Enter approve · r retry · ↑↓ navigate · ←/→ switch · Esc cancel"
    } else {
        "↑↓ navigate · Enter select · Type to search · ←/→ switch · Esc cancel"
    };

    let footer = exit_state
        .key_name
        .map(|key| format!("Press {key} again to exit"))
        .unwrap_or_else(|| footer.to_string());
    let selected = selected_rule.read().clone();
    if let Some(rule) = selected {
        let details = if rule.source == PermissionRuleSource::PolicySettings {
            RuleDetailsState::Managed { rule: rule.clone() }
        } else {
            RuleDetailsState::ConfirmDelete { rule: rule.clone() }
        };
        let sender = rule_results.0.clone();
        return element! { RuleDetails(details: Some(details), action_pending: deleting_rule.get(),
            on_cancel: move |_| { if !deleting_rule.get() { selected_rule.set(None); } },
            on_delete: move |_| {
                if deleting_rule.get() { return; }
                deleting_rule.set(true);
                let tab = match rule.rule_behavior { PermissionBehavior::Allow => RulesTabType::Allow, PermissionBehavior::Ask => RulesTabType::Ask, PermissionBehavior::Deny => RulesTabType::Deny };
                let keys: Vec<_> = get_rules_options(&context, tab, "").0.into_iter().filter(|option| option.value != ADD_NEW_RULE_VALUE).map(|option| option.value).collect();
                let next = keys.iter().position(|key| key == &rule_key(&rule)).and_then(|index| keys.get(index + 1).or_else(|| index.checked_sub(1).and_then(|index| keys.get(index)))).cloned();
                last_focused_rule_key.set(next);
                let rule_for_delete = rule.clone(); let context = context.clone(); let store = store.clone(); let sender = sender.clone();
                let message = format!("Deleted {} rule {}", match rule.rule_behavior { PermissionBehavior::Allow => "allow", PermissionBehavior::Ask => "ask", PermissionBehavior::Deny => "deny" }, chalk.clone().bold().apply(&permission_rule_value_to_string(&rule.rule_value)));
                // CC deletePermissionRule is async in type but contains no
                // await: setter completes before this caller appends/closes.
                // Deliver that synchronous ordering through one off-frame
                // completion; a rejected read-only deletion still returns to
                // this source caller's unconditional summary/close path.
                tokio::task::spawn_blocking(move || {
                    let fixture = match delete_permission_rule(&context, &rule_for_delete) {
                        Ok(updated) => {
                            let updated = Arc::new(updated);
                            if let Some(store) = store { store.replace_with(|state| state.tool_permission_context = updated); None }
                            else { Some(updated) }
                        }
                        Err(error) => { crate::utils::log::log_error(crate::utils::log::LogError::new(error.to_string())); None },
                    };
                    let _ = sender.try_send((fixture, None, Some(message)));
                });
            }) }.into_any();
    }
    let adding = *adding_rule_to_tab.read();
    if let Some(behavior) = adding.and_then(RulesTabType::behavior) {
        return element! { PermissionRuleInput(rule_behavior: Some(behavior),
            on_cancel: move |_| adding_rule_to_tab.set(None),
            on_submit: move |value| { validated_rule.set(Some(value)); adding_rule_to_tab.set(None); }) }.into_any();
    }
    let validated = validated_rule.read().clone();
    if let Some(rule) = validated {
        let setter_sender = rule_results.0.clone();
        let result_sender = rule_results.0.clone();
        return element! { AddPermissionRules(rule_behavior: Some(rule.rule_behavior), rule_values: vec![rule.rule_value],
            initial_context: context,
            set_tool_permission_context: Some(Arc::new(move |updated| {
                if let Some(store) = store.as_ref() { store.replace_with(|state| state.tool_permission_context = updated); }
                else { let _ = setter_sender.try_send((Some(updated), None, None)); }
            }) as Arc<dyn Fn(Arc<ToolPermissionContext>) + Send + Sync>),
            on_add_rules: Some(Arc::new(move |outcome| { let _ = result_sender.try_send((None, Some(outcome), None)); }) as Arc<dyn Fn(AddPermissionRulesOutcome) + Send + Sync>),
            on_cancel: move |_| validated_rule.set(None)) }.into_any();
    }
    if is_adding_workspace_directory.get() {
        let store = store.clone();
        let sender = adding_results.0.clone();
        let input_context = context.clone();
        return element! {
            AddWorkspaceDirectory(
                permission_context: context,
                on_cancel: move |_| is_adding_workspace_directory.set(false),
                // CC :657-694 anonymous onAddDirectory: no /add-dir bootstrap
                // or sandbox-refresh side effects belong to this callback.
                on_add_directory: Some(Arc::new(move |selection: AddWorkspaceDirectorySelection| {
                    let update = PermissionUpdate::AddDirectories {
                        directories: vec![selection.path.clone()],
                        destination: if selection.remember { PermissionUpdateDestination::LocalSettings } else { PermissionUpdateDestination::Session },
                    };
                    let updated = Arc::new(apply_permission_update(&input_context, &update));
                    let fixture = if let Some(store) = store.as_ref() {
                        store.replace_with(|state| state.tool_permission_context = updated);
                        None
                    } else { Some(updated) };
                    // Source chalk.bold(path), carried through the canonical
                    // ANSI-aware command result renderer (SGR 1 / 22).
                    let message = format!("Added directory {} to workspace{}", chalk.clone().bold().apply(&selection.path),
                        if selection.remember { " and saved to local settings" } else { " for this session" });
                    if selection.remember {
                        let sender = sender.clone();
                        // This source callback's external continuation must run
                        // even if the parent UI inbox has already been dropped.
                        tokio::task::spawn_blocking(move || {
                            match persist_permission_update(&update) {
                                Ok(_) => { let _ = sender.try_send((fixture, message)); },
                                Err(error) => crate::utils::log::log_error(crate::utils::log::LogError::new(error.to_string())),
                            }
                        });
                    } else { let _ = sender.try_send((fixture, message)); }
                }) as super::add_workspace_directory::AddWorkspaceDirectoryCallback),
            )
        }.into_any();
    }
    let removing = removing_directory.read().clone();
    if let Some(path) = removing {
        return element! {
            RemoveWorkspaceDirectory(
                directory_path: path.clone(), permission_context: context,
                set_permission_context: move |updated| {
                    if let Some(store) = store.as_ref() {
                        store.replace_with(|state| state.tool_permission_context = updated);
                    } else { fixture_context.set(Some(updated)); }
                },
                on_remove: move |_| {
                    let mut next = changes.read().clone();
                    next.push(format!("Removed directory {} from workspace", chalk.clone().bold().apply(&path)));
                    changes.set(next);
                    removing_directory.set(None);
                },
                on_cancel: move |_| removing_directory.set(None),
            )
        }
        .into_any();
    }

    element! {
        Pane(color: theme.permission) {
            PermissionRuleListCancelBinding(active: top_level_cancel_active, pending: Some(pending_cancel))
            TabsHeader(
                title: Some("Permissions:".to_string()),
                color: Some(theme.permission),
                tabs,
                selected_index: all_rule_tabs()
                    .iter()
                    .position(|tab| *tab == selected_tab)
                    .unwrap_or(0),
                header_focused: is_header_focused,
            )
            // CC Tabs' content Box and selected Tab both default to row.
            // With useFullWidth absent their width is intrinsic, so the rule
            // subtitle determines SearchBox width instead of the full header.
            View(flex_direction: FlexDirection::Row, margin_top: 1u32) {
                View(flex_direction: FlexDirection::Row) {
                #(match selected_tab {
                    RulesTabType::Recent => element! {
                        RecentDenialsTab(
                            denials_override: props.denials_override.clone(),
                            header_focused: is_header_focused,
                            on_focus_header: move |_| header_focused.set(true),
                            on_state_change: Some(handle_denial_state_change),
                        )
                    }.into_any(),
                    RulesTabType::Allow | RulesTabType::Ask | RulesTabType::Deny => element! {
                        PermissionRulesTab(key: selected_tab.label(),
                            tab: Some(selected_tab), context: context.clone(), search_query: query,
                            is_search_mode: is_search_mode.get(), is_focused: terminal_focused,
                            header_focused: is_header_focused, cursor_offset: search.offset(),
                            last_focused_rule_key: last_focused_rule_key.read().clone(),
                            on_select: move |value: String| {
                                if value == ADD_NEW_RULE_VALUE { adding_rule_to_tab.set(Some(selected_tab)); }
                                else { selected_rule.set(get_rules_options(&context, selected_tab, "").1.get(&value).cloned()); }
                            },
                            on_cancel: move |_| pending_cancel.set(true),
                            on_focus_header: move |focused| header_focused.set(focused),
                        )
                    }.into_any(),
                    RulesTabType::Workspace => element! {
                        View(flex_direction: FlexDirection::Column) {
                            Text(
                                content: "Claude Code can read files in the workspace, and make edits when auto-accept edits is on.".to_string(),
                                wrap: TextWrap::Wrap,
                            )
                            WorkspaceTab(
                                tool_permission_context: Some((*context).clone()),
                                header_focused: is_header_focused,
                                on_request_add_directory: move |_| is_adding_workspace_directory.set(true),
                                on_request_remove_directory: move |path: String| removing_directory.set(Some(path)),
                                on_cancel: move |_| pending_exit_result.set(Some(PermissionRuleListExit {
                                    result: Some("Workspace dialog dismissed".to_string()), display: Some(CommandResultDisplay::System), ..Default::default()
                                })),
                                on_focus_header: move |_| header_focused.set(true),
                            )
                        }
                    }.into_any(),
                })
                }
            }
            View(margin_top: 1u32, padding_left: 1u32) {
                Text(content: footer.to_string(), dim: true)
            }
        }
    }.into_any()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::permissions::PermissionRuleValue;
    use crate::utils::theme;
    use std::collections::HashMap;

    fn context() -> ToolPermissionContext {
        let mut allow = HashMap::new();
        allow.insert(
            PermissionRuleSource::LocalSettings,
            vec![PermissionRuleValue::new(
                "Bash",
                Some("cargo test:*".to_string()),
            )],
        );
        ToolPermissionContext {
            always_allow_rules: allow,
            ..ToolPermissionContext::default()
        }
    }

    #[test]
    fn permission_rule_list_renders_tabs_subtitle_and_rules() {
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                PermissionRuleList(context: context(), initial_tab: Some(RulesTabType::Allow))
            }
        }
        .render(Some(120))
        .to_string();
        assert!(text.contains("Permissions:"), "canvas=\n{text}");
        assert!(text.contains("Recently denied"), "canvas=\n{text}");
        assert!(text.contains("Claude Code won't ask"), "canvas=\n{text}");
        assert!(
            text.contains("Add a new rule…"),
            "rules tab should render the canonical add option; canvas=\n{text}"
        );
        assert!(text.contains("2. Bash(cargo test:*)"), "canvas=\n{text}");
        assert!(
            !text.contains("From project local settings"),
            "source list has labels only; canvas=\n{text}"
        );
    }

    #[test]
    fn permission_rule_list_duplicate_identity_matches_official_source_keyed_map() {
        let duplicate = PermissionRuleValue::new("Read", Some("/tmp/item".to_string()));
        let context = ToolPermissionContext {
            always_allow_rules: HashMap::from([
                (
                    PermissionRuleSource::LocalSettings,
                    vec![duplicate.clone(), duplicate.clone()],
                ),
                (PermissionRuleSource::Session, vec![duplicate]),
            ]),
            ..ToolPermissionContext::default()
        };
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                PermissionRuleList(context: context, initial_tab: Some(RulesTabType::Allow))
            }
        }
        .render(Some(120))
        .to_string();

        assert_eq!(
            text.matches("Read(/tmp/item)").count(),
            2,
            "same-source duplicates collapse but equal rules from distinct sources remain; canvas=\n{text}"
        );
        assert!(!text.contains("From project local settings"));
        assert!(!text.contains("From current session"));
    }

    #[test]
    fn rule_details_confirm_delete_renders_official_copy_and_select() {
        let rule = PermissionRule {
            rule_value: PermissionRuleValue::new("Bash", Some("cargo test:*".to_string())),
            rule_behavior: PermissionBehavior::Allow,
            source: PermissionRuleSource::LocalSettings,
        };
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                RuleDetails(details: Some(RuleDetailsState::ConfirmDelete { rule }))
            }
        }
        .render(Some(120))
        .to_string();
        assert!(text.contains("Delete allowed tool?"), "canvas=\n{text}");
        assert!(text.contains("Bash(cargo test:*)"), "canvas=\n{text}");
        assert!(
            text.contains("Are you sure you want to delete this permission rule?"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("❯ 1. Yes"),
            "shared Select should render the focused pointer and index; canvas=\n{text}"
        );
        assert!(text.contains("2. No"), "canvas=\n{text}");
        assert!(text.contains("Esc to cancel"), "canvas=\n{text}");
    }

    #[test]
    fn rule_details_managed_branch_renders_without_select() {
        let rule = PermissionRule {
            rule_value: PermissionRuleValue::new("Bash", None),
            rule_behavior: PermissionBehavior::Deny,
            source: PermissionRuleSource::CliArg,
        };
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                RuleDetails(details: Some(RuleDetailsState::Managed { rule }))
            }
        }
        .render(Some(120))
        .to_string();
        assert!(text.contains("Rule details"), "canvas=\n{text}");
        assert!(
            text.contains("configured by managed settings"),
            "canvas=\n{text}"
        );
        assert!(
            !text.contains("❯"),
            "managed branch renders no Select; canvas=\n{text}"
        );
    }

    #[derive(Default, Props)]
    struct WorkspaceLifecycleRootProps {
        children: Vec<AnyElement<'static>>,
    }

    struct WorkspaceLifecycleRoot;

    impl Component for WorkspaceLifecycleRoot {
        type Props<'a> = WorkspaceLifecycleRootProps;

        fn new(_props: &Self::Props<'_>) -> Self {
            Self
        }

        fn update(
            &mut self,
            props: &mut Self::Props<'_>,
            mut hooks: Hooks,
            updater: &mut ComponentUpdater,
        ) {
            let runtime = crate::keybindings::keybinding_provider_setup::use_keybinding_setup(
                &mut hooks,
                crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings(),
            );
            // Preserve the fixture's move-only elements exactly as the native
            // ContextProvider does: borrow children on every update, never drain
            // the static caller's props after the first frame.
            let mut context = Context::owned(runtime);
            updater.set_transparent_layout(true);
            updater.update_children(props.children.iter_mut(), Some(context.borrow()));
        }
    }

    #[tokio::test]
    async fn workspace_add_remove_lifecycle_matches_official_callbacks_and_exit() {
        let _diagnostic =
            crate::utils::env_utils::EnvVarGuard::set("COMETIX_PERMISSION_CANCEL_DIAGNOSTIC", "1");
        chalk::set_stdout_level(3);
        use futures::StreamExt;
        use std::time::Duration;
        let _guard = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let root =
            std::env::temp_dir().join(format!("cc-permissions-workspace-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.to_string_lossy().into_owned();
        let input_leaf = root.file_name().unwrap().to_string_lossy().into_owned();
        let bootstrap = crate::bootstrap::state::get_additional_directories_for_claude_md();
        let store = crate::state::store::AppStore::new(Default::default(), None);
        let exits = Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed = exits.clone();
        let mut app = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                WorkspaceLifecycleRoot {
                    FocusScope(handle_keys: false) {
                        ContextProvider(value: Context::owned(store.clone())) {
                            PermissionRuleList(initial_tab: Some(RulesTabType::Workspace), on_exit: move |result| observed.lock().unwrap().push(result))
                        }
                    }
                }
            }
        };
        let key = |code| TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code));
        // Source :653-722 early-return components unmount Tabs. On returning,
        // its initial header focus must be restored; a child's Esc never exits.
        let steps = vec![
            ("←/→ tab switch", vec![key(KeyCode::Down)]),
            ("↑↓ navigate · Enter select", vec![key(KeyCode::Enter)]),
            ("Enter the path to the directory:", vec![key(KeyCode::Esc)]),
            ("←/→ tab switch", vec![key(KeyCode::Down)]),
            ("↑↓ navigate · Enter select", vec![key(KeyCode::Enter)]),
            (
                "Enter the path to the directory:",
                vec![TerminalEvent::Paste(format!("{path}/"))],
            ),
            (input_leaf.as_str(), vec![key(KeyCode::Enter)]),
            ("←/→ tab switch", vec![key(KeyCode::Down)]),
            ("↑↓ navigate · Enter select", vec![key(KeyCode::Enter)]),
            ("Remove directory from workspace?", vec![key(KeyCode::Esc)]),
            ("←/→ tab switch", vec![key(KeyCode::Down)]),
            ("↑↓ navigate · Enter select", vec![key(KeyCode::Enter)]),
            (
                "Remove directory from workspace?",
                vec![key(KeyCode::Enter)],
            ),
            ("←/→ tab switch", vec![key(KeyCode::Esc)]),
        ];
        let (sender, receiver) = async_channel::unbounded();
        let mut renders = Box::pin(app.mock_terminal_render_loop(
            MockTerminalConfig::with_events(receiver).with_size(130, 25),
        ));
        let started = std::time::Instant::now();
        let deadline = futures_timer::Delay::new(Duration::from_secs(4));
        tokio::pin!(deadline);
        let mut step = 0;
        let mut saw_added = false;
        let mut last_canvas = String::new();
        loop {
            tokio::select! {
                _ = &mut deadline => {
                    eprintln!("workspace deadline step={step} elapsed={:?}", started.elapsed());
                    break;
                },
                canvas = renders.next() => {
                    let Some(canvas) = canvas else { break; };
                    let text = canvas.to_string();
                    last_canvas = text.clone();
                    eprintln!("workspace step={step} elapsed={:?} exits={} canvas={text:?}", started.elapsed(), exits.lock().unwrap().len());
                    saw_added |= store.get().tool_permission_context.additional_working_directories.contains_key(&path);
                    if let Some((needle, keys)) = steps.get(step) {
                        let observed = if step == 6 {
                            text.chars().filter(|character| !character.is_whitespace() && !matches!(*character, '│' | '┃' | '║')).collect::<String>().contains(needle)
                        } else { text.contains(needle) };
                        if observed {
                            if step < 13 { assert!(exits.lock().unwrap().is_empty(), "child cancel escaped parent at step {step}"); }
                            eprintln!("workspace send step={step} elapsed={:?} keys={keys:?}", started.elapsed());
                            for key in keys { sender.send(key.clone()).await.unwrap(); }
                            step += 1;
                        }
                    }
                    if !exits.lock().unwrap().is_empty() { break; }
                }
            }
        }
        assert_eq!(
            step,
            steps.len(),
            "workspace flow stopped at step {step}; last canvas:\n{last_canvas}"
        );
        assert!(
            saw_added,
            "source add callback must publish to live AppStore"
        );
        assert!(
            !store
                .get()
                .tool_permission_context
                .additional_working_directories
                .contains_key(&path)
        );
        assert_eq!(
            *exits.lock().unwrap(),
            vec![PermissionRuleListExit {
                result: Some(format!(
                    "Added directory \x1b[1m{path}\x1b[22m to workspace for this session\nRemoved directory \x1b[1m{path}\x1b[22m from workspace"
                )),
                display: None,
                ..Default::default()
            }]
        );
        assert_eq!(
            crate::bootstrap::state::get_additional_directories_for_claude_md(),
            bootstrap,
            "Workspace must not borrow /add-dir bootstrap effects"
        );
        drop(renders);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn permissions_workspace_route_cancel_matches_official_parent_exit() {
        use futures::{StreamExt, stream};
        use std::time::Duration;
        let exits = Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed = exits.clone();
        let mut app = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                ContextProvider(value: Context::owned(crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings())) {
                    PermissionRuleList(on_exit: move |result| observed.lock().unwrap().push(result))
                }
            }
        };
        let keys = stream::iter([
            KeyCode::Right,
            KeyCode::Right,
            KeyCode::Right,
            KeyCode::Down,
            KeyCode::Esc,
        ])
        .then(|code| async move {
            futures_timer::Delay::new(Duration::from_millis(20)).await;
            TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code))
        })
        .chain(stream::pending());
        let mut renders = Box::pin(
            app.mock_terminal_render_loop(MockTerminalConfig::with_events(keys).with_size(120, 20)),
        );
        let deadline = futures_timer::Delay::new(Duration::from_secs(1));
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                _ = &mut deadline => break,
                canvas = renders.next() => {
                    if canvas.is_none() || !exits.lock().unwrap().is_empty() { break; }
                }
            }
        }
        assert_eq!(
            *exits.lock().unwrap(),
            vec![PermissionRuleListExit {
                result: Some("Permissions dialog dismissed".to_string()),
                display: Some(CommandResultDisplay::System),
                ..Default::default()
            }]
        );
    }

    #[test]
    fn rules_options_and_cancel_keep_source_identity_filtering_and_retry_order() {
        let chalk = Chalk::with_level(3);
        let context = context();
        let (all, by_key) = get_rules_options(&context, RulesTabType::Allow, "");
        assert_eq!(all[0].value, ADD_NEW_RULE_VALUE);
        assert_eq!(by_key.len(), 1);
        let (filtered, _) = get_rules_options(&context, RulesTabType::Allow, "CARGO");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].label, "Bash(cargo test:*)");
        assert!(
            get_rules_options(&context, RulesTabType::Allow, "missing")
                .0
                .is_empty()
        );
        let denials = Arc::new(vec![
            AutoModeDenial {
                tool_name: "Bash".into(),
                display: "first".into(),
                reason: String::new(),
                timestamp: 1.0,
            },
            AutoModeDenial {
                tool_name: "Bash".into(),
                display: "second".into(),
                reason: String::new(),
                timestamp: 2.0,
            },
        ]);
        let snapshot = RecentDenialsSnapshot {
            denials,
            retry: [1, 100, 0].into_iter().collect(),
            approved: [0].into_iter().collect(),
        };
        let (retry, result) = handle_rules_cancel(&snapshot, &["earlier change".into()], &chalk);
        assert_eq!(retry, Some(vec!["second".into(), "first".into()]));
        assert_eq!(result, PermissionRuleListExit {
            should_query: true,
            meta_messages: vec!["Permission granted for: second, first. You may now retry these commands if you would like.".into()],
            ..Default::default()
        });
        let snapshot = RecentDenialsSnapshot {
            retry: Default::default(),
            ..snapshot
        };
        assert_eq!(
            handle_rules_cancel(&snapshot, &["earlier change".into()], &chalk)
                .1
                .result,
            Some("Approved \x1b[1mfirst\x1b[22m\nearlier change".into())
        );
    }

    #[tokio::test]
    async fn permissions_rule_delete_publishes_before_remount_and_preserves_rejected_summary() {
        // The component reads the chalk stdout singleton (as CC reads the chalk
        // import); pin it like JS tests pin `chalk.level`.
        chalk::set_stdout_level(3);
        use futures::StreamExt;
        use std::time::Duration;
        for source in [
            PermissionRuleSource::Session,
            PermissionRuleSource::FlagSettings,
        ] {
            let rule_value = PermissionRuleValue::new("Bash", Some("echo parity:*".into()));
            let context = ToolPermissionContext {
                always_allow_rules: HashMap::from([(source, vec![rule_value])]),
                ..Default::default()
            };
            let mut initial = crate::state::app_state_store::AppState::default();
            initial.tool_permission_context = Arc::new(context);
            let store = crate::state::store::AppStore::new(initial, None);
            let exits = Arc::new(std::sync::Mutex::new(Vec::new()));
            let observed = exits.clone();
            let mut app = element! {
                ContextProvider(value: Context::owned(*theme::current())) {
                    ContextProvider(value: Context::owned(crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings())) {
                        ContextProvider(value: Context::owned(store.clone())) {
                            PermissionRuleList(on_exit: move |result| observed.lock().unwrap().push(result))
                        }
                    }
                }
            };
            let key = |code| TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code));
            let steps = [
                ("←/→ tab switch", KeyCode::Down),
                ("❯ 1. Add a new rule…", KeyCode::Down),
                ("❯ 2. Bash(echo parity:*)", KeyCode::Enter),
                ("Delete allowed tool?", KeyCode::Enter),
                ("←/→ tab switch", KeyCode::Esc),
            ];
            let (sender, receiver) = async_channel::unbounded();
            let mut renders = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(receiver).with_size(120, 25),
            ));
            let deadline = futures_timer::Delay::new(Duration::from_secs(3));
            tokio::pin!(deadline);
            let mut step = 0;
            let mut last_canvas = String::new();
            loop {
                tokio::select! {
                    _ = &mut deadline => break,
                    canvas = renders.next() => {
                        let Some(canvas) = canvas else { break; };
                        last_canvas = canvas.to_string();
                        if let Some((needle, code)) = steps.get(step) {
                            if last_canvas.contains(needle) {
                                if step == 4 {
                                    assert_eq!(get_allow_rules(&store.get().tool_permission_context).is_empty(), source == PermissionRuleSource::Session,
                                        "live setter must precede remount; source={source:?}; canvas=\n{last_canvas}");
                                }
                                sender.send(key(*code)).await.unwrap();
                                step += 1;
                            }
                        }
                        if !exits.lock().unwrap().is_empty() { break; }
                    }
                }
            }
            assert_eq!(
                step,
                steps.len(),
                "source={source:?}; phase={step}; canvas=\n{last_canvas}"
            );
            assert_eq!(
                *exits.lock().unwrap(),
                vec![PermissionRuleListExit {
                    result: Some("Deleted allow rule \x1b[1mBash(echo parity:*)\x1b[22m".into()),
                    ..Default::default()
                }]
            );
        }
    }

    #[tokio::test]
    async fn permissions_numeric_input_preserves_official_dom_search_after_details_cancel() {
        use futures::StreamExt;
        use std::time::Duration;
        let exits = Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed = exits.clone();
        let mut app = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                ContextProvider(value: Context::owned(crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings())) {
                    PermissionRuleList(context: context(), on_exit: move |result| observed.lock().unwrap().push(result))
                }
            }
        };
        let steps = [
            ("←/→ tab switch", KeyCode::Down),
            ("❯ 1. Add a new rule…", KeyCode::Char('2')),
            ("Delete allowed tool?", KeyCode::Esc),
            ("Type to filter", KeyCode::Esc),
            ("Search…", KeyCode::Esc),
            ("↑↓ navigate · Enter select", KeyCode::Esc),
        ];
        let (sender, receiver) = async_channel::unbounded();
        let mut renders = Box::pin(app.mock_terminal_render_loop(
            MockTerminalConfig::with_events(receiver).with_size(120, 25),
        ));
        let deadline = futures_timer::Delay::new(Duration::from_secs(3));
        tokio::pin!(deadline);
        let mut step = 0;
        let mut last_canvas = String::new();
        loop {
            tokio::select! {
                _ = &mut deadline => break,
                canvas = renders.next() => {
                    let Some(canvas) = canvas else { break; };
                    last_canvas = canvas.to_string();
                    if let Some((needle, code)) = steps.get(step) {
                        if last_canvas.contains(needle) {
                            if step == 3 {
                                assert!(last_canvas.contains("⌕ 2"), "source numeric DOM query must survive details; canvas=\n{last_canvas}");
                                assert!(!last_canvas.contains("Add a new rule…"), "nonempty search filters add option");
                            }
                            if step == 4 { assert!(last_canvas.contains("Type to filter"), "first Esc only clears query"); }
                            if step < 5 { assert!(exits.lock().unwrap().is_empty(), "Esc must not escape child/search"); }
                            sender.send(TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, *code))).await.unwrap();
                            step += 1;
                        }
                    }
                    if !exits.lock().unwrap().is_empty() { break; }
                }
            }
        }
        assert_eq!(step, steps.len(), "phase={step}; canvas=\n{last_canvas}");
        assert_eq!(
            *exits.lock().unwrap(),
            vec![PermissionRuleListExit {
                result: Some("Permissions dialog dismissed".into()),
                display: Some(CommandResultDisplay::System),
                ..Default::default()
            }]
        );
    }

    fn assert_summary_output_capability(enabled: bool) {
        // Actual source PermissionRuleList.tsx Chalk → UserLocalCommandOutputMessage
        // → Markdown/Ansi → ink/colorize.ts. Level detection is the chalk
        // crate's tested responsibility; the level is pinned per chalk
        // instance. NO_COLOR indifference is likewise iocraft's contract,
        // verified there (its encoders never read NO_COLOR) — no env
        // manipulation here.
        let chalk = Chalk::with_level(if enabled { 3 } else { 0 });
        let snapshot = RecentDenialsSnapshot {
            denials: Arc::new(vec![AutoModeDenial {
                tool_name: "Bash".into(),
                display: "needle".into(),
                reason: String::new(),
                timestamp: 1.0,
            }]),
            approved: [0].into_iter().collect(),
            retry: Default::default(),
        };
        let result = handle_rules_cancel(&snapshot, &[], &chalk)
            .1
            .result
            .unwrap();
        assert_eq!(
            result,
            if enabled {
                "Approved \x1b[1mneedle\x1b[22m"
            } else {
                "Approved needle"
            }
        );
        let mut output = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                crate::components::messages::user_local_command_output_message::UserLocalCommandOutputMessage(output: result)
            }
        };
        let canvas = output.render(Some(80));
        let plain = canvas.to_string();
        assert!(plain.contains("Approved needle"), "{plain}");
        let (x, y) = (0..canvas.height())
            .find_map(|y| {
                (0..canvas.width()).find_map(|x| {
                    (canvas.cell(x, y).and_then(|c| c.text()) == Some("n")).then_some((x, y))
                })
            })
            .unwrap();
        assert_eq!(
            canvas.resolved_text_style(x, y).unwrap().weight,
            if enabled {
                Weight::Bold
            } else {
                Weight::Normal
            }
        );
        let mut bytes = Vec::new();
        canvas.write_ansi(&mut bytes).unwrap();
        let ansi = String::from_utf8(bytes).unwrap();
        assert_eq!(ansi.contains("\x1b[1m"), enabled, "{ansi:?}");
        // Shape assertion, environment-independent: rendered output must never
        // contain an empty SGR — the signature of crossterm's NO_COLOR-aware
        // `Colored::Display`, which iocraft's encoders deliberately replace.
        assert!(!ansi.contains("\x1b[m"), "{ansi:?}");
    }

    #[test]
    fn permission_summary_matches_official_force_color_and_existing_ansi_consumer() {
        assert_summary_output_capability(true);
    }

    #[test]
    fn permission_summary_matches_official_force_zero_plain_message_and_output() {
        assert_summary_output_capability(false);
    }

    #[test]
    fn permission_summary_matches_official_nested_closes_and_line_boundaries() {
        // applyStyle's nested-close reopening and line-break encasing are the
        // chalk crate's tested responsibility (official-suite port); this test
        // keeps the end-to-end guarantee that such spans survive the
        // UserLocalCommandOutputMessage → Ansi rendering path.
        let chalk = Chalk::with_level(3);
        let snapshot = RecentDenialsSnapshot {
            denials: Arc::new(vec![AutoModeDenial {
                tool_name: "Bash".into(),
                display: "a\x1b[22mb\nc".into(),
                reason: String::new(),
                timestamp: 1.0,
            }]),
            approved: [0].into_iter().collect(),
            retry: Default::default(),
        };
        let result = handle_rules_cancel(&snapshot, &[], &chalk)
            .1
            .result
            .unwrap();
        assert_eq!(
            result,
            "Approved \x1b[1ma\x1b[22m\x1b[1mb\x1b[22m\n\x1b[1mc\x1b[22m"
        );
        let canvas = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                crate::components::messages::user_local_command_output_message::UserLocalCommandOutputMessage(output: result)
            }
        }.render(Some(80));
        for letter in ["b", "c"] {
            let style = (0..canvas.height())
                .find_map(|y| {
                    (0..canvas.width()).find_map(|x| {
                        (canvas.cell(x, y).and_then(|cell| cell.text()) == Some(letter))
                            .then(|| canvas.resolved_text_style(x, y).unwrap())
                    })
                })
                .unwrap();
            assert_eq!(
                style.weight,
                Weight::Bold,
                "nested close/newline must preserve {letter}'s source bold"
            );
        }
    }
}
