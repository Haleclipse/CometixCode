//! Maps to: CC `components/ui/TreeSelect.tsx:1-340`.

use crate::components::custom_select::{Select, SelectLayout, SelectOptionData};
use crate::keybindings::keybinding_context::KeybindingRuntime;
use crate::keybindings::types::ContextName;
use crate::keybindings::use_keybinding::use_keybinding;
use iocraft::prelude::*;
use serde_json::{Map, Value};
use std::collections::HashSet;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum TreeNodeId {
    String(String),
    Number(i64),
}

impl TreeNodeId {
    fn option_value(&self) -> String {
        match self {
            Self::String(value) => format!("s:{value}"),
            Self::Number(value) => format!("n:{value}"),
        }
    }
}

impl From<&str> for TreeNodeId {
    fn from(value: &str) -> Self {
        Self::String(value.to_string())
    }
}

impl From<String> for TreeNodeId {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}

impl From<i64> for TreeNodeId {
    fn from(value: i64) -> Self {
        Self::Number(value)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TreeNode {
    pub id: TreeNodeId,
    pub value: Value,
    pub label: String,
    pub description: Option<String>,
    pub dim_description: Option<bool>,
    pub children: Vec<TreeNode>,
    pub metadata: Map<String, Value>,
}

impl TreeNode {
    pub fn leaf(id: impl Into<TreeNodeId>, value: Value, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            value,
            label: label.into(),
            description: None,
            dim_description: None,
            children: Vec::new(),
            metadata: Map::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct FlattenedNode {
    node: TreeNode,
    depth: usize,
    is_expanded: bool,
    has_children: bool,
    parent_id: Option<TreeNodeId>,
}

type ExpansionPredicate = Arc<dyn Fn(&TreeNodeId) -> bool + Send + Sync>;
type ParentPrefix = Arc<dyn Fn(bool) -> String + Send + Sync>;
type ChildPrefix = Arc<dyn Fn(usize) -> String + Send + Sync>;

fn flatten_nodes(
    nodes: &[TreeNode],
    internal_expanded: &HashSet<TreeNodeId>,
    external_expanded: Option<&ExpansionPredicate>,
) -> Vec<FlattenedNode> {
    fn traverse(
        result: &mut Vec<FlattenedNode>,
        node: &TreeNode,
        depth: usize,
        parent_id: Option<TreeNodeId>,
        internal_expanded: &HashSet<TreeNodeId>,
        external_expanded: Option<&ExpansionPredicate>,
    ) {
        let has_children = !node.children.is_empty();
        let is_expanded = external_expanded
            .map(|predicate| predicate(&node.id))
            .unwrap_or_else(|| internal_expanded.contains(&node.id));
        result.push(FlattenedNode {
            node: node.clone(),
            depth,
            is_expanded,
            has_children,
            parent_id,
        });
        if has_children && is_expanded {
            for child in &node.children {
                traverse(
                    result,
                    child,
                    depth + 1,
                    Some(node.id.clone()),
                    internal_expanded,
                    external_expanded,
                );
            }
        }
    }

    let mut result = Vec::new();
    for node in nodes {
        traverse(
            &mut result,
            node,
            0,
            None,
            internal_expanded,
            external_expanded,
        );
    }
    result
}

fn option_label(
    flat: &FlattenedNode,
    parent_prefix: Option<&ParentPrefix>,
    child_prefix: Option<&ChildPrefix>,
) -> String {
    let prefix = if flat.has_children {
        parent_prefix
            .map(|prefix| prefix(flat.is_expanded))
            .unwrap_or_else(|| if flat.is_expanded { "▼ " } else { "▶ " }.to_string())
    } else if flat.depth > 0 {
        child_prefix
            .map(|prefix| prefix(flat.depth))
            .unwrap_or_else(|| "  ▸ ".to_string())
    } else {
        String::new()
    };
    format!("{prefix}{}", flat.node.label)
}

fn visible_from_index(focused: usize, count: usize, visible: usize) -> usize {
    if count == 0 {
        return 0;
    }
    let visible = visible.max(1).min(count);
    focused
        .saturating_add(1)
        .saturating_sub(visible)
        .min(count.saturating_sub(visible))
}

#[derive(Clone, Debug)]
enum TreeSelectAction {
    Previous,
    Next,
    PreviousPage,
    NextPage,
    SelectIndex(usize),
    Accept,
    Cancel,
    Expand,
    Collapse,
}

#[derive(Default, Props)]
pub struct TreeSelectProps<'a> {
    pub nodes: Vec<TreeNode>,
    pub on_select: HandlerMut<'a, TreeNode>,
    pub on_cancel: HandlerMut<'a, ()>,
    pub on_focus: HandlerMut<'a, TreeNode>,
    pub focus_node_id: Option<TreeNodeId>,
    pub visible_option_count: usize,
    pub layout: SelectLayout,
    pub is_disabled: bool,
    pub hide_indexes: bool,
    pub is_node_expanded: Option<ExpansionPredicate>,
    pub on_expand: HandlerMut<'a, TreeNodeId>,
    pub on_collapse: HandlerMut<'a, TreeNodeId>,
    pub get_parent_prefix: Option<ParentPrefix>,
    pub get_child_prefix: Option<ChildPrefix>,
    pub on_up_from_first_item: HandlerMut<'a, ()>,
}

/// Generic tree selection boundary. JSON values preserve TypeScript's generic
/// payload without coupling this UI component to session/agent domain types.
#[component]
pub fn TreeSelect<'a>(
    props: &mut TreeSelectProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let mut internal_expanded = hooks.use_state(HashSet::<TreeNodeId>::new);
    let expanded_snapshot = internal_expanded.read().clone();
    let flattened = flatten_nodes(
        &props.nodes,
        &expanded_snapshot,
        props.is_node_expanded.as_ref(),
    );
    let initial_focus_id = props
        .focus_node_id
        .clone()
        .or_else(|| flattened.first().map(|flat| flat.node.id.clone()));
    let mut focused_index = hooks.use_state({
        let flattened = flattened.clone();
        move || {
            initial_focus_id
                .as_ref()
                .and_then(|id| flattened.iter().position(|flat| &flat.node.id == id))
                .unwrap_or(0)
        }
    });
    let mut last_focused_id = hooks.use_state(|| None::<TreeNodeId>);
    let mut pending_focus = hooks.use_state(|| None::<TreeNode>);
    let mut pending_action = hooks.use_state(|| None::<TreeSelectAction>);

    let focus_to_emit = {
        let pending = pending_focus.read();
        pending.clone()
    };
    if let Some(node) = focus_to_emit {
        pending_focus.set(None);
        (props.on_focus)(node);
    }

    if let Some(controlled_id) = props.focus_node_id.as_ref()
        && last_focused_id.read().as_ref() != Some(controlled_id)
        && let Some(index) = flattened
            .iter()
            .position(|flat| &flat.node.id == controlled_id)
    {
        focused_index.set(index);
        last_focused_id.set(Some(controlled_id.clone()));
        pending_focus.set(Some(flattened[index].node.clone()));
    }
    if focused_index.get() >= flattened.len() {
        focused_index.set(flattened.len().saturating_sub(1));
    }

    if last_focused_id.read().is_none()
        && let Some(flat) = flattened.get(focused_index.get())
    {
        last_focused_id.set(Some(flat.node.id.clone()));
        pending_focus.set(Some(flat.node.clone()));
    }

    let action_to_process = {
        let pending = pending_action.read();
        pending.clone()
    };
    if let Some(action) = action_to_process {
        pending_action.set(None);
        let current_index = focused_index.get().min(flattened.len().saturating_sub(1));
        let current = flattened.get(current_index).cloned();
        let mut focus_node = |index: usize| {
            if let Some(flat) = flattened.get(index) {
                focused_index.set(index);
                last_focused_id.set(Some(flat.node.id.clone()));
                pending_focus.set(Some(flat.node.clone()));
            }
        };
        match action {
            TreeSelectAction::Previous if !flattened.is_empty() => {
                if current_index == 0 && !props.on_up_from_first_item.is_default() {
                    (props.on_up_from_first_item)(());
                } else {
                    focus_node(if current_index == 0 {
                        flattened.len() - 1
                    } else {
                        current_index - 1
                    });
                }
            }
            TreeSelectAction::Next if !flattened.is_empty() => {
                focus_node((current_index + 1) % flattened.len());
            }
            TreeSelectAction::PreviousPage if !flattened.is_empty() => {
                let page = if props.visible_option_count == 0 {
                    5
                } else {
                    props.visible_option_count
                };
                focus_node(current_index.saturating_sub(page));
            }
            TreeSelectAction::NextPage if !flattened.is_empty() => {
                let page = if props.visible_option_count == 0 {
                    5
                } else {
                    props.visible_option_count
                };
                focus_node((current_index + page).min(flattened.len() - 1));
            }
            TreeSelectAction::SelectIndex(index) => {
                if let Some(flat) = flattened.get(index) {
                    (props.on_select)(flat.node.clone());
                }
            }
            TreeSelectAction::Accept => {
                if let Some(flat) = current {
                    (props.on_select)(flat.node);
                }
            }
            TreeSelectAction::Cancel => (props.on_cancel)(()),
            TreeSelectAction::Expand => {
                if let Some(flat) = current.filter(|flat| flat.has_children) {
                    if props.on_expand.is_default() {
                        let mut expanded = expanded_snapshot.clone();
                        expanded.insert(flat.node.id);
                        internal_expanded.set(expanded);
                    } else {
                        (props.on_expand)(flat.node.id);
                    }
                }
            }
            TreeSelectAction::Collapse => {
                if let Some(flat) = current {
                    if flat.has_children && flat.is_expanded {
                        if props.on_collapse.is_default() {
                            let mut expanded = expanded_snapshot.clone();
                            expanded.remove(&flat.node.id);
                            internal_expanded.set(expanded);
                        } else {
                            (props.on_collapse)(flat.node.id);
                        }
                    } else if let Some(parent_id) = flat.parent_id {
                        if props.on_collapse.is_default() {
                            let mut expanded = expanded_snapshot.clone();
                            expanded.remove(&parent_id);
                            internal_expanded.set(expanded);
                        } else {
                            (props.on_collapse)(parent_id.clone());
                        }
                        if let Some(index) = flattened
                            .iter()
                            .position(|candidate| candidate.node.id == parent_id)
                        {
                            focus_node(index);
                        }
                    }
                }
            }
            TreeSelectAction::Previous
            | TreeSelectAction::Next
            | TreeSelectAction::PreviousPage
            | TreeSelectAction::NextPage => {}
        }
    }

    let runtime = hooks
        .try_use_context::<KeybindingRuntime>()
        .map(|runtime| runtime.clone());
    for (action_name, action) in [
        ("select:previous", TreeSelectAction::Previous),
        ("select:next", TreeSelectAction::Next),
        ("select:previousPage", TreeSelectAction::PreviousPage),
        ("select:nextPage", TreeSelectAction::NextPage),
        ("select:index1", TreeSelectAction::SelectIndex(0)),
        ("select:index2", TreeSelectAction::SelectIndex(1)),
        ("select:index3", TreeSelectAction::SelectIndex(2)),
        ("select:index4", TreeSelectAction::SelectIndex(3)),
        ("select:index5", TreeSelectAction::SelectIndex(4)),
        ("select:index6", TreeSelectAction::SelectIndex(5)),
        ("select:index7", TreeSelectAction::SelectIndex(6)),
        ("select:index8", TreeSelectAction::SelectIndex(7)),
        ("select:index9", TreeSelectAction::SelectIndex(8)),
        ("select:accept", TreeSelectAction::Accept),
        ("select:cancel", TreeSelectAction::Cancel),
        ("select:expand", TreeSelectAction::Expand),
        ("select:collapse", TreeSelectAction::Collapse),
    ] {
        let mut pending_action = pending_action;
        let is_disabled = props.is_disabled;
        use_keybinding(
            &mut hooks,
            runtime.clone(),
            action_name,
            ContextName::Select,
            move || !is_disabled,
            move || {
                pending_action.set(Some(action.clone()));
                true
            },
        );
    }

    let options = flattened
        .iter()
        .map(|flat| SelectOptionData {
            label: option_label(
                flat,
                props.get_parent_prefix.as_ref(),
                props.get_child_prefix.as_ref(),
            ),
            description: flat.node.description.clone(),
            dim_description: flat.node.dim_description.unwrap_or(true),
            value: flat.node.id.option_value(),
            ..SelectOptionData::default()
        })
        .collect::<Vec<_>>();
    let focused = focused_index.get().min(options.len().saturating_sub(1));
    let visible_count = if props.visible_option_count == 0 {
        5
    } else {
        props.visible_option_count
    };

    element! {
        View(flex_direction: FlexDirection::Column) {
            Select(
                options: options,
                focused_index: focused,
                visible_option_count: visible_count,
                visible_from_index: visible_from_index(focused, flattened.len(), visible_count),
                layout: props.layout,
                is_disabled: props.is_disabled,
                hide_indexes: props.hide_indexes,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{StreamExt, stream};
    use std::sync::Mutex;
    use std::time::Duration;

    fn nodes() -> Vec<TreeNode> {
        let mut parent = TreeNode::leaf("parent", Value::String("p".to_string()), "Parent");
        let mut child = TreeNode::leaf("child", Value::String("c".to_string()), "Child");
        child.description = Some("child description".to_string());
        parent.children.push(child);
        vec![
            parent,
            TreeNode::leaf(2i64, Value::String("root".to_string()), "Root leaf"),
        ]
    }

    #[test]
    fn collapsed_tree_renders_official_prefix_and_hides_children() {
        let text = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                TreeSelect(nodes: nodes(), visible_option_count: 5usize)
            }
        }
        .render(Some(100))
        .to_string();
        assert!(text.contains("▶ Parent"), "canvas=\n{text}");
        assert!(text.contains("Root leaf"), "canvas=\n{text}");
        assert!(!text.contains("Child"), "canvas=\n{text}");
    }

    #[test]
    fn external_expansion_and_custom_prefixes_shape_flat_options() {
        let expanded: ExpansionPredicate = Arc::new(|id| id == &TreeNodeId::from("parent"));
        let parent_prefix: ParentPrefix =
            Arc::new(|expanded| if expanded { "[-] " } else { "[+] " }.to_string());
        let child_prefix: ChildPrefix = Arc::new(|depth| format!("{}> ", " ".repeat(depth)));
        let text = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                TreeSelect(
                    nodes: nodes(),
                    is_node_expanded: Some(expanded),
                    get_parent_prefix: Some(parent_prefix),
                    get_child_prefix: Some(child_prefix),
                )
            }
        }
        .render(Some(100))
        .to_string();
        assert!(text.contains("[-] Parent"), "canvas=\n{text}");
        assert!(text.contains("> Child"), "canvas=\n{text}");
        assert!(text.contains("child description"), "canvas=\n{text}");
    }

    #[test]
    fn action_keys_expand_navigate_collapse_and_select() {
        let focused = Arc::new(Mutex::new(Vec::<TreeNodeId>::new()));
        let selected = Arc::new(Mutex::new(Vec::<TreeNodeId>::new()));
        let focused_handler = Arc::clone(&focused);
        let selected_handler = Arc::clone(&selected);
        let events = stream::iter(vec![
            KeyCode::Right,
            KeyCode::Down,
            KeyCode::Left,
            KeyCode::Enter,
        ])
        .then(|code| async move {
            futures_timer::Delay::new(Duration::from_millis(45)).await;
            TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code))
        })
        .chain(stream::pending());

        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(
                    crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings()
                )) {
                    ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                        TreeSelect(
                            nodes: nodes(),
                            visible_option_count: 5usize,
                            on_focus: move |node: TreeNode| focused_handler.lock().expect("focus mutex").push(node.id),
                            on_select: move |node: TreeNode| selected_handler.lock().expect("select mutex").push(node.id),
                        )
                    }
                }
            };
            let mut loop_ = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(100, 24),
            ));
            for _ in 0..60 {
                let next = crate::utils::race(loop_.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(80)).await;
                    None
                })
                .await;
                if next.is_none() {
                    break;
                }
            }
        });

        let focused = focused.lock().expect("focus mutex").clone();
        assert!(focused.starts_with(&[TreeNodeId::from("parent"), TreeNodeId::from("child")]));
        assert_eq!(focused.last(), Some(&TreeNodeId::from("parent")));
        assert_eq!(
            *selected.lock().expect("select mutex"),
            vec![TreeNodeId::from("parent")]
        );
    }

    #[test]
    fn numeric_action_selects_one_based_flattened_option() {
        let selected = Arc::new(Mutex::new(Vec::<TreeNodeId>::new()));
        let selected_handler = Arc::clone(&selected);
        let selected_wait = Arc::clone(&selected);
        let events = stream::once(async {
            futures_timer::Delay::new(Duration::from_millis(40)).await;
            TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Char('2')))
        })
        .chain(stream::pending());
        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(
                    crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings()
                )) {
                    ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                        TreeSelect(
                            nodes: nodes(),
                            on_select: move |node: TreeNode| selected_handler.lock().expect("select mutex").push(node.id),
                        )
                    }
                }
            };
            let mut loop_ = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(100, 24),
            ));
            for _ in 0..20 {
                let _ = crate::utils::race(loop_.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(80)).await;
                    None
                })
                .await;
                if !selected_wait.lock().expect("select mutex").is_empty() {
                    break;
                }
            }
        });
        assert_eq!(
            *selected.lock().expect("select mutex"),
            vec![TreeNodeId::Number(2)]
        );
    }

    #[test]
    fn escape_action_cancels() {
        let cancelled = Arc::new(Mutex::new(0usize));
        let cancelled_handler = Arc::clone(&cancelled);
        let cancelled_wait = Arc::clone(&cancelled);
        let events = stream::once(async {
            futures_timer::Delay::new(Duration::from_millis(40)).await;
            TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Esc))
        })
        .chain(stream::pending());
        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(
                    crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings()
                )) {
                    ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                        TreeSelect(
                            nodes: nodes(),
                            on_cancel: move |_| *cancelled_handler.lock().expect("cancel mutex") += 1,
                        )
                    }
                }
            };
            let mut loop_ = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(100, 24),
            ));
            for _ in 0..20 {
                let _ = crate::utils::race(loop_.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(80)).await;
                    None
                })
                .await;
                if *cancelled_wait.lock().expect("cancel mutex") > 0 {
                    break;
                }
            }
        });
        assert_eq!(*cancelled.lock().expect("cancel mutex"), 1);
    }

    #[test]
    fn disabled_tree_ignores_action_keys() {
        let selected = Arc::new(Mutex::new(0usize));
        let selected_handler = Arc::clone(&selected);
        let events = stream::iter(vec![
            TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Right)),
            TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Enter)),
        ]);
        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                    TreeSelect(
                        nodes: nodes(),
                        is_disabled: true,
                        on_select: move |_| *selected_handler.lock().expect("select mutex") += 1,
                    )
                }
            };
            let mut loop_ = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(100, 24),
            ));
            for _ in 0..8 {
                let next = crate::utils::race(loop_.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(80)).await;
                    None
                })
                .await;
                if next.is_none() {
                    break;
                }
            }
        });
        assert_eq!(*selected.lock().expect("select mutex"), 0);
    }
}
