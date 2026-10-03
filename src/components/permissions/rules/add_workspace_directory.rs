//! Maps to: CC `components/permissions/rules/AddWorkspaceDirectory.tsx`.
//!
//! Owns input, debounced suggestions, validation submission, and confirmation.
//! Filesystem futures execute on workers; only callback delivery returns to the frame.

use crate::commands::add_dir::validation::{
    AddDirectoryResult, add_dir_help_message, validate_directory_for_workspace,
};
use crate::components::custom_select::{
    Select, SelectInputOptionMeta, SelectLayout, SelectOptionData, UseSelectInputOptions,
    UseSelectStateProps, use_select_input, use_select_state,
};
use crate::components::design_system::dialog::Dialog;
use crate::components::prompt_input::prompt_input_footer_suggestions::{
    SuggestionItem, SuggestionList,
};
use crate::components::text_input::TextInput;
use crate::tool::ToolPermissionContext;
use crate::utils::theme::Theme;
use iocraft::prelude::*;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RememberDirectoryOption {
    YesSession,
    YesRemember,
    No,
}

impl RememberDirectoryOption {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::YesSession => "yes-session",
            Self::YesRemember => "yes-remember",
            Self::No => "no",
        }
    }

    pub fn from_str(value: &str) -> Self {
        match value {
            "yes-remember" => Self::YesRemember,
            "no" => Self::No,
            _ => Self::YesSession,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddWorkspaceDirectorySelection {
    pub path: String,
    pub remember: bool,
    pub accepted: bool,
}

pub const PERMISSION_DESCRIPTION: &str = "Claude Code will be able to read files in this directory and make edits when auto-accept edits is on.";

/// Maps to: CC `REMEMBER_DIRECTORY_OPTIONS`.
pub fn remember_directory_options() -> Vec<SelectOptionData> {
    [
        (RememberDirectoryOption::YesSession, "Yes, for this session"),
        (
            RememberDirectoryOption::YesRemember,
            "Yes, and remember this directory",
        ),
        (RememberDirectoryOption::No, "No"),
    ]
    .into_iter()
    .map(|(value, label)| SelectOptionData {
        label: label.to_string(),
        value: value.as_str().to_string(),
        description: None,
        dim_description: true,
        disabled: false,
        input: None,
    })
    .collect()
}

/// Maps to: CC `AddWorkspaceDirectory.tsx#handleSelect`.
pub fn add_workspace_directory_selection(
    directory_path: &str,
    value: RememberDirectoryOption,
) -> AddWorkspaceDirectorySelection {
    AddWorkspaceDirectorySelection {
        path: directory_path.to_string(),
        remember: value == RememberDirectoryOption::YesRemember,
        accepted: value != RememberDirectoryOption::No,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AddWorkspaceDirectoryViewState {
    DirectoryInput,
    RememberDirectory { path: String },
}

pub fn add_workspace_directory_view_state(
    directory_path: Option<&str>,
) -> AddWorkspaceDirectoryViewState {
    match directory_path.filter(|path| !path.is_empty()) {
        Some(path) => AddWorkspaceDirectoryViewState::RememberDirectory {
            path: path.to_string(),
        },
        None => AddWorkspaceDirectoryViewState::DirectoryInput,
    }
}

/// Maps to: CC `AddWorkspaceDirectory.tsx:55-62#PermissionDescription`.
#[component]
fn PermissionDescription() -> impl Into<AnyElement<'static>> {
    element! { Text(content: PERMISSION_DESCRIPTION, dim: true, wrap: TextWrap::Wrap) }
}

#[derive(Default, Props)]
struct DirectoryDisplayProps {
    path: String,
}

/// Maps to: CC `AddWorkspaceDirectory.tsx:64-71#DirectoryDisplay`.
#[component]
fn DirectoryDisplay(props: &DirectoryDisplayProps, hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    element! {
        View(flex_direction: FlexDirection::Column, padding_left: 2u32, padding_right: 2u32, gap: 1u32) {
            Text(content: props.path.clone(), color: theme.permission)
            PermissionDescription
        }
    }
}

#[derive(Default, Props)]
struct DirectoryInputProps {
    value: Option<State<String>>,
    cursor_offset: Option<State<usize>>,
    error: Option<String>,
    suggestions: Arc<Vec<SuggestionItem>>,
    selected_suggestion: usize,
    on_submit: Option<Arc<dyn Fn(String) + Send + Sync>>,
}

/// Maps to: CC `AddWorkspaceDirectory.tsx:73-111#DirectoryInput`.
#[component]
fn DirectoryInput(props: &DirectoryInputProps, hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    // Preserve this render's submission callback at the TextInput delivery edge;
    // a String-only pending slot would substitute a later props callback.
    let on_submit = props.on_submit.clone();
    element! {
        View(flex_direction: FlexDirection::Column) {
            Text(content: "Enter the path to the directory:")
            View(border_style: BorderStyle::Round, margin_top: 1u32, margin_bottom: 1u32, padding_left: 1u32) {
                TextInput(
                    value: props.value, cursor_offset: props.cursor_offset,
                    show_cursor: true, columns: 80usize,
                    placeholder: Some(format!("Directory path{}", crate::constants::figures::figures().ellipsis)),
                    disable_escape_double_press: true, select_navigation_passthrough: true,
                    on_submit: move |value| { if let Some(callback) = on_submit.as_ref() { callback(value); } },
                )
            }
            #((!props.suggestions.is_empty()).then(|| element! {
                View(margin_bottom: 1u32) {
                    SuggestionList(items: props.suggestions.clone(), selected: props.selected_suggestion as i32)
                }
            }))
            #(props.error.as_ref().map(|error| element! { Text(content: error.clone(), color: theme.error, wrap: TextWrap::Wrap) }))
        }
    }
}

/// Native owned form of CC Props.onAddDirectory. The Arc preserves the callback
/// captured by handleSubmit across its await and after this child unmounts.
/// Callback owners transport local-state delivery to their retained frame.
pub type AddWorkspaceDirectoryCallback = Arc<dyn Fn(AddWorkspaceDirectorySelection) + Send + Sync>;

/// Maps to CC `AddWorkspaceDirectory.tsx:151-165#handleSubmit`.
/// A returned validation message transports setError to the mounted frame;
/// successful external callbacks retain source Promise lifetime independently.
async fn handle_submit(
    new_path: String,
    permission_context: Arc<ToolPermissionContext>,
    on_add_directory: Option<AddWorkspaceDirectoryCallback>,
) -> anyhow::Result<Option<String>> {
    #[cfg(test)]
    {
        let barrier = tests::SUBMISSION_BARRIERS
            .lock()
            .unwrap()
            .get(new_path.trim_end_matches('/'))
            .cloned();
        if let Some((started, release)) = barrier {
            let _ = started
                .send((new_path.clone(), permission_context.clone()))
                .await;
            let _ = release.recv().await;
        }
    }
    let result = validate_directory_for_workspace(&new_path, &permission_context).await?;
    match result {
        AddDirectoryResult::Success { absolute_path } => {
            if let Some(on_add_directory) = on_add_directory {
                on_add_directory(AddWorkspaceDirectorySelection {
                    path: absolute_path,
                    remember: false,
                    accepted: true,
                });
            }
            Ok(None)
        }
        failure => Ok(Some(add_dir_help_message(&failure))),
    }
}

#[derive(Default, Props)]
pub struct AddWorkspaceDirectoryProps<'a> {
    pub directory_path: Option<String>,
    pub permission_context: Arc<ToolPermissionContext>,
    /// Maps to: CC `onAddDirectory` — invoked by `handleSelect` for the two
    /// yes options (AddWorkspaceDirectory.tsx:214-233).
    pub on_add_directory: Option<AddWorkspaceDirectoryCallback>,
    /// Maps to: CC `onCancel` — the 'no' option and the Select's
    /// `onCancel={() => handleSelect('no')}` (AddWorkspaceDirectory.tsx:273).
    pub on_cancel: HandlerMut<'a, ()>,
}

/// Maps to: CC `AddWorkspaceDirectory` render path
/// (AddWorkspaceDirectory.tsx:113-292).
#[component]
pub fn AddWorkspaceDirectory<'a>(
    props: &mut AddWorkspaceDirectoryProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let mut directory_input = hooks.use_state(String::new);
    let mut cursor_offset = hooks.use_state(|| 0usize);
    let mut error = hooks.use_state(|| None::<String>);
    let mut suggestions = hooks.use_state(|| Arc::new(Vec::<SuggestionItem>::new()));
    let mut selected_suggestion = hooks.use_state(|| 0usize);
    let mut pending_cancel = hooks.use_state(|| false);
    let mut pending_add = hooks.use_state(|| None::<AddWorkspaceDirectorySelection>);
    let input_updates = hooks.use_const(|| Arc::new(async_channel::unbounded::<String>()));
    let submission_errors = hooks.use_const(|| Arc::new(async_channel::unbounded::<String>()));
    let completed_suggestions =
        hooks.use_const(|| Arc::new(async_channel::unbounded::<Vec<SuggestionItem>>()));
    let input_rx = input_updates.1.clone();
    let suggestion_tx = completed_suggestions.0.clone();
    // CC :135-154 fetchSuggestions / useDebounceCallback(..., 100). The
    // retained future only schedules workers; completion scanning stays off-frame.
    hooks.use_future(async move {
        let worker = tokio::spawn(async move {
            while let Ok(mut path) = input_rx.recv().await {
                loop {
                    tokio::select! {
                        value = input_rx.recv() => match value { Ok(value) => path = value, Err(_) => return },
                        _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => break,
                    }
                }
                let sender = suggestion_tx.clone();
                // Source debounce does not cancel an already-started fetch.
                tokio::spawn(async move {
                    let result = if path.is_empty() { Ok(Vec::new()) } else {
                        crate::utils::suggestions::directory_completion::get_directory_completions(
                            &path, crate::utils::suggestions::directory_completion::CompletionOptions::default(),
                        ).await
                    };
                    match result {
                        Ok(values) => { let _ = sender.send(values).await; },
                        Err(error) => crate::utils::log::log_error(crate::utils::log::LogError::new(error.to_string())),
                    }
                });
            }
        });
        // Cancel the timer/receiver when the retained component is dropped.
        struct Abort(tokio::task::AbortHandle);
        impl Drop for Abort { fn drop(&mut self) { self.0.abort(); } }
        let _abort = Abort(worker.abort_handle());
        let _ = worker.await;
    });
    let completed_rx = completed_suggestions.1.clone();
    hooks.use_future(async move {
        while let Ok(values) = completed_rx.recv().await {
            suggestions.set(Arc::new(values));
            selected_suggestion.set(0);
        }
    });
    let error_rx = submission_errors.1.clone();
    hooks.use_future(async move {
        while let Ok(message) = error_rx.recv().await {
            error.set(Some(message));
        }
    });
    let mut last_input = hooks.use_state(|| None::<String>);
    let input = directory_input.read().clone();
    if last_input.read().as_ref() != Some(&input) {
        last_input.set(Some(input.clone()));
        let _ = input_updates.0.try_send(input.clone());
    }
    // CC DirectoryInput passes value.length as a controlled cursor offset.
    // React bails out on equal controlled cursor state; iocraft set always
    // schedules a render, so only publish an actual cursor change.
    if cursor_offset.get() != input.len() {
        cursor_offset.set(input.len());
    }
    let runtime = hooks
        .try_use_context::<crate::keybindings::keybinding_context::KeybindingRuntime>()
        .map(|runtime| runtime.clone());
    crate::keybindings::use_keybinding::use_keybinding(
        &mut hooks,
        runtime,
        "confirm:no",
        crate::keybindings::types::ContextName::Settings,
        || true,
        move || {
            pending_cancel.set(true);
            true
        },
    );
    let state = add_workspace_directory_view_state(props.directory_path.as_deref());
    let in_remember_state = props
        .directory_path
        .as_ref()
        .is_some_and(|path| !path.is_empty());
    let options = remember_directory_options();
    let select_state = use_select_state(
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
        select_state,
        UseSelectInputOptions {
            is_disabled: !in_remember_state,
            has_on_cancel: true,
            option_metas: options
                .iter()
                .map(|option| SelectInputOptionMeta {
                    value: option.value.clone(),
                    disabled: option.disabled,
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        },
    );
    if let Some(value) = events.take_accepted() {
        if let Some(path) = props.directory_path.as_ref() {
            match value.as_str() {
                "no" => pending_cancel.set(true),
                "yes-session" | "yes-remember" => {
                    pending_add.set(Some(add_workspace_directory_selection(
                        path,
                        RememberDirectoryOption::from_str(&value),
                    )))
                }
                _ => {}
            }
        }
    }
    if events.take_cancelled() {
        pending_cancel.set(true);
    }
    if pending_cancel.get() {
        pending_cancel.set(false);
        (props.on_cancel)(());
    }
    let added = pending_add.read().clone();
    if let Some(selection) = added {
        pending_add.set(None);
        if let Some(callback) = props.on_add_directory.as_ref() {
            callback(selection);
        }
    }
    let suggestion_values = suggestions.read().clone();
    let key_suggestions = suggestion_values.clone();
    let error_tx = submission_errors.0.clone();
    let input_callback = props.on_add_directory.clone();
    let input_selected_suggestion = selected_suggestion.get();
    let input_context = props.permission_context.clone();
    let navigation = select_state.navigation.snapshot();
    let focused_index = navigation.focused_index().unwrap_or(0);
    // CC :236-241 focused Box: iocraft requires a local FocusContext for
    // auto_focus/on_key_down. Disable scope defaults so Tab/Esc stay source-owned.
    element! {
        FocusScope(handle_keys: Some(false)) {
        View(flex_direction: FlexDirection::Column, tab_index: Some(0), auto_focus: true,
            on_key_down: move |event: ViewKeyboardEvent| {
                if key_suggestions.is_empty() { return; }
                let selected = selected_suggestion.get();
                match event.key.as_str() {
                    "tab" => {
                        event.prevent_default();
                        if let Some(suggestion) = key_suggestions.get(selected) {
                            directory_input.set(format!("{}/", suggestion.id));
                            error.set(None);
                        }
                    },
                    "up" | "p" if event.key == "up" || event.ctrl => {
                        event.prevent_default();
                        selected_suggestion.set(if selected == 0 { key_suggestions.len() - 1 } else { selected - 1 });
                    },
                    "down" | "n" if event.key == "down" || event.ctrl => {
                        event.prevent_default();
                        selected_suggestion.set(if selected >= key_suggestions.len() - 1 { 0 } else { selected + 1 });
                    },
                    _ => {},
                }
            },
        ) {
        Dialog(
            title: "Add directory to workspace".to_string(),
            color: Some(theme.permission),
            is_cancel_active: Some(false),
            input_guide: (!in_remember_state).then(|| "Tab complete · Enter add · Esc cancel".to_string()),
            on_cancel: move |_| pending_cancel.set(true),
        ) {
            #(match state {
                AddWorkspaceDirectoryViewState::RememberDirectory { ref path } => element! {
                    View(flex_direction: FlexDirection::Column, gap: 1u32) {
                        DirectoryDisplay(path: path.clone())
                        Select(
                            options: options.clone(),
                            focused_index: focused_index,
                            visible_option_count: navigation.visible_option_count,
                            visible_from_index: navigation.visible_from_index,
                            layout: SelectLayout::Compact,
                        )
                    }
                },
                AddWorkspaceDirectoryViewState::DirectoryInput => element! {
                    View(flex_direction: FlexDirection::Column, gap: 1u32, margin_left: 2u32, margin_right: 2u32) {
                        PermissionDescription
                        DirectoryInput(
                            value: Some(directory_input), cursor_offset: Some(cursor_offset),
                            error: error.read().clone(), suggestions: suggestion_values.clone(),
                            selected_suggestion: selected_suggestion.get(),
                            // CC App.tsx:615-620 emits InputEvent before the DOM
                            // keydown. TextInput submits raw text first; a selected
                            // suggestion starts a second promise (CC :184-192).
                            // The native delivery carrier preserves both inputs,
                            // captured callbacks and concurrent continuations.
                            on_submit: Some(Arc::new(move |value| {
                                let mut paths = vec![value];
                                if let Some(suggestion) = suggestion_values.get(input_selected_suggestion) {
                                    paths.push(format!("{}/", suggestion.id));
                                }
                                let context = input_context.clone();
                                let callback = input_callback.clone();
                                let errors = error_tx.clone();
                                // These are event-started promises, not mount effects.
                                // join_all starts them in source order without waiting
                                // for the first validation before starting the second.
                                tokio::spawn(async move {
                                    futures::future::join_all(paths.into_iter().map(|path| {
                                        let context = context.clone();
                                        let callback = callback.clone();
                                        let errors = errors.clone();
                                        async move {
                                            match handle_submit(path, context, callback).await {
                                                Ok(Some(message)) => { let _ = errors.send(message).await; },
                                                Ok(None) => {},
                                                Err(failure) => crate::utils::log::log_error(crate::utils::log::LogError::new(failure.to_string())),
                                            }
                                        }
                                    })).await;
                                });
                            }) as Arc<dyn Fn(String) + Send + Sync>),
                        )
                    }
                },
            })
        }
        }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::TEST_ENV_LOCK;
    use crate::utils::theme;

    #[derive(Default, Props)]
    struct PermissionInputTestProvidersProps {
        children: Vec<AnyElement<'static>>,
    }

    struct PermissionInputTestProviders;

    impl Component for PermissionInputTestProviders {
        type Props<'a> = PermissionInputTestProvidersProps;

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
            // Match ContextProvider's retained move-only children: local runtime
            // updates borrow the same subtree instead of draining its props.
            let mut context = Context::owned(runtime);
            updater.set_transparent_layout(true);
            updater.update_children(props.children.iter_mut(), Some(context.borrow()));
        }
    }

    fn input_test_providers(child: AnyElement<'static>) -> AnyElement<'static> {
        crate::utils::process_runtime::initialize_test_process_runtime();
        element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                PermissionInputTestProviders {
                    ContextProvider(value: Context::owned(crate::state::store::AppStore::new(Default::default(), None))) {
                        FocusScope(handle_keys: false) { #(Some(child)) }
                    }
                }
            }
        }.into_any()
    }

    type SubmissionBarrier = (
        async_channel::Sender<(String, Arc<ToolPermissionContext>)>,
        async_channel::Receiver<()>,
    );
    pub(super) static SUBMISSION_BARRIERS: std::sync::LazyLock<
        std::sync::Mutex<std::collections::HashMap<String, SubmissionBarrier>>,
    > = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

    #[derive(Default, Props)]
    struct SubmissionLifetimeHarnessProps {
        context: Arc<ToolPermissionContext>,
        results: Option<async_channel::Sender<(usize, AddWorkspaceDirectorySelection)>>,
    }

    #[component]
    fn SubmissionLifetimeHarness(
        props: &SubmissionLifetimeHarnessProps,
        mut hooks: Hooks,
    ) -> impl Into<AnyElement<'static>> {
        let mut generation = hooks.use_state(|| 0usize);
        let mut context = hooks.use_state(|| None::<Arc<ToolPermissionContext>>);
        let mut visible = hooks.use_state(|| true);
        // Test-only external update delivered as a real terminal event; avoid
        // seeding a once-only future from iocraft's initial default props.
        hooks.use_terminal_events(move |event| {
            if matches!(
                event,
                TerminalEvent::Key(KeyEvent {
                    code: KeyCode::F(2),
                    kind: KeyEventKind::Press,
                    ..
                })
            ) {
                generation.set(1);
                context.set(Some(Arc::new(ToolPermissionContext::default())));
            }
        });
        let captured_generation = generation.get();
        let results = props.results.clone();
        let current_context = context
            .read()
            .clone()
            .unwrap_or_else(|| props.context.clone());
        let callback: AddWorkspaceDirectoryCallback = Arc::new(move |selection| {
            if let Some(results) = results.as_ref() {
                let _ = results.try_send((captured_generation, selection));
            }
        });
        element! {
            View(flex_direction: FlexDirection::Column) {
                Text(content: format!("callback generation {}", generation.get()))
                #(if visible.get() {
                    element! { AddWorkspaceDirectory(permission_context: current_context, on_add_directory: Some(callback), on_cancel: move |_| visible.set(false)) }.into_any()
                } else { element! { Text(content: "Child closed") }.into_any() })
            }
        }.into_any()
    }

    async fn submission_lifetime_case(cancel_child: bool) {
        use futures::StreamExt;
        use std::time::Duration;
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let root =
            std::env::temp_dir().join(format!("cc-submit-lifetime-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.to_string_lossy().into_owned();
        let initial_context = Arc::new(ToolPermissionContext::default());
        let (started_tx, started_rx) = async_channel::bounded(1);
        let (release_tx, release_rx) = async_channel::bounded(1);
        SUBMISSION_BARRIERS
            .lock()
            .unwrap()
            .insert(path.clone(), (started_tx, release_rx));
        let (result_tx, result_rx) = async_channel::unbounded();
        let (keys, events) = async_channel::unbounded();
        let mut app = input_test_providers(element! {
            SubmissionLifetimeHarness(context: initial_context.clone(), results: Some(result_tx))
        }.into_any());
        let mut frames =
            Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(120, 20),
            ));
        keys.send(TerminalEvent::Paste(format!("{path}/")))
            .await
            .unwrap();
        let mut stage = "waiting for complete pasted path";
        let mut last_text = String::new();
        let exercise = async {
            let mut submitted = false;
            let captured_context = loop {
                tokio::select! {
                    context = started_rx.recv() => break context.unwrap().1,
                    frame = frames.next() => {
                        last_text = frame.expect("retained frame ended").to_string();
                        // TextInput keeps its source 80-column width. Join wrapped
                        // content without whitespace or enclosing box borders,
                        // while still requiring the complete unique leaf.
                        let joined: String = last_text.chars().filter(|character| {
                            !character.is_whitespace() && !('\u{2500}'..='\u{257f}').contains(character)
                        }).collect();
                        if !submitted && joined.contains(root.file_name().unwrap().to_str().unwrap()) {
                            keys.send(TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Enter))).await.unwrap();
                            submitted = true;
                            stage = "waiting for validation start";
                        }
                    }
                }
            };
            assert!(
                Arc::ptr_eq(&captured_context, &initial_context),
                "validate must use submission context"
            );
            stage = if cancel_child {
                "waiting for child unmount"
            } else {
                "waiting for callback replacement"
            };
            if cancel_child {
                keys.send(TerminalEvent::Key(KeyEvent::new(
                    KeyEventKind::Press,
                    KeyCode::Esc,
                )))
                .await
                .unwrap();
            } else {
                keys.send(TerminalEvent::Key(KeyEvent::new(
                    KeyEventKind::Press,
                    KeyCode::F(2),
                )))
                .await
                .unwrap();
            }
            loop {
                last_text = frames
                    .next()
                    .await
                    .expect("retained frame ended")
                    .to_string();
                if last_text.contains(if cancel_child {
                    "Child closed"
                } else {
                    "callback generation 1"
                }) {
                    break;
                }
            }
            stage = "waiting for captured callback after validation release";
            release_tx.send(()).await.unwrap();
            let (generation, selection) = result_rx.recv().await.unwrap();
            assert_eq!(
                generation, 0,
                "source handleSubmit must call its captured callback even after rerender/unmount"
            );
            assert_eq!(
                selection,
                AddWorkspaceDirectorySelection {
                    path: path.clone(),
                    remember: false,
                    accepted: true
                }
            );
        };
        let outcome = tokio::select! {
            _ = exercise => Some(()),
            _ = futures_timer::Delay::new(Duration::from_secs(3)) => None,
        };
        drop(frames);
        SUBMISSION_BARRIERS.lock().unwrap().remove(&path);
        let _ = release_tx.try_send(());
        std::fs::remove_dir_all(root).unwrap();
        assert!(
            outcome.is_some(),
            "submission lifecycle regression timed out: cancel_child={cancel_child}; stage={stage}; last canvas=\n{last_text}"
        );
    }

    #[tokio::test]
    async fn handle_submit_matches_official_callback_capture_across_validation() {
        // AddWorkspaceDirectory.tsx:151-165 captures onAddDirectory before await.
        submission_lifetime_case(false).await;
    }

    #[tokio::test]
    async fn handle_submit_matches_official_continuation_after_child_cancel() {
        // PermissionRuleList.tsx:686 only hides the child; handleSubmit continues.
        submission_lifetime_case(true).await;
    }

    #[test]
    fn add_workspace_directory_options_and_selection_match_official() {
        let options = remember_directory_options();
        assert_eq!(
            options
                .iter()
                .map(|option| option.value.as_str())
                .collect::<Vec<_>>(),
            vec!["yes-session", "yes-remember", "no"]
        );
        let selection =
            add_workspace_directory_selection("/repo/extra", RememberDirectoryOption::YesRemember);
        assert!(selection.accepted);
        assert!(selection.remember);
        assert_eq!(selection.path, "/repo/extra");
        assert!(
            !add_workspace_directory_selection("/repo/extra", RememberDirectoryOption::No).accepted
        );
    }

    #[test]
    fn add_workspace_directory_renders_remember_and_input_states() {
        let remember = input_test_providers(
            element! {
                AddWorkspaceDirectory(directory_path: Some("/repo/extra".to_string()))
            }
            .into_any(),
        )
        .render(Some(100))
        .to_string();
        assert!(remember.contains("/repo/extra"), "canvas=\n{remember}");
        assert!(
            remember.contains("❯ 1. Yes, for this session"),
            "shared Select should render the focused pointer and index; canvas=\n{remember}"
        );
        assert!(
            remember.contains("2. Yes, and remember this directory"),
            "canvas=\n{remember}"
        );

        let input = input_test_providers(element! { AddWorkspaceDirectory }.into_any())
            .render(Some(100))
            .to_string();
        assert!(
            input.contains("Enter the path to the directory"),
            "canvas=\n{input}"
        );
        assert!(input.contains("Directory path…"), "canvas=\n{input}");
    }
    #[tokio::test]
    async fn add_workspace_directory_matches_official_suggestion_keys_and_selected_path_submit() {
        use futures::StreamExt;
        use std::time::Duration;
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!("cc-add-dir-input-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("target-a")).unwrap();
        std::fs::create_dir_all(root.join("target-b")).unwrap();
        let raw_path = root.join("t").to_string_lossy().into_owned();
        let (started_tx, started_rx) = async_channel::unbounded();
        let (release_tx, release_rx) = async_channel::unbounded();
        for path in [
            &raw_path,
            &root.join("target-a").to_string_lossy().into_owned(),
            &root.join("target-b").to_string_lossy().into_owned(),
        ] {
            SUBMISSION_BARRIERS
                .lock()
                .unwrap()
                .insert(path.clone(), (started_tx.clone(), release_rx.clone()));
        }
        let (callback_tx, callback_rx) = async_channel::unbounded();
        let selected = Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed = selected.clone();
        let mut app = input_test_providers(element! {
            AddWorkspaceDirectory(on_add_directory: Some(Arc::new(move |selection| { observed.lock().unwrap().push(selection); let _ = callback_tx.try_send(()); }) as AddWorkspaceDirectoryCallback))
        }.into_any());
        // Drive each key only after observing the actual rendered selection.
        // A fixed expected path can accidentally pass when readdir puts that
        // path first and the Down handler never runs.
        let (keys, events) = async_channel::unbounded();
        keys.send(TerminalEvent::Paste(format!("{}/t", root.display())))
            .await
            .unwrap();
        let mut renders =
            Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(100, 18),
            ));
        let deadline = futures_timer::Delay::new(Duration::from_secs(3));
        tokio::pin!(deadline);
        let mut stage = 0;
        let mut expected_path = None;
        let mut started_paths = Vec::new();
        loop {
            tokio::select! {
                _ = &mut deadline => break,
                started = started_rx.recv() => {
                    started_paths.push(started.unwrap().0);
                    if started_paths.len() == 2 {
                        release_tx.send(()).await.unwrap();
                        release_tx.send(()).await.unwrap();
                    }
                },
                _ = callback_rx.recv() => break,
                canvas = renders.next() => {
                    let Some(canvas) = canvas else { break; };
                    let rows: Vec<_> = (0..canvas.height()).filter_map(|y| {
                        let line = canvas.get_text(0, y, canvas.width(), 1);
                        let name = line.split_whitespace().next()?;
                        if !matches!(name, "target-a/" | "target-b/") { return None; }
                        let x = line.find(name)?;
                        let style = canvas.resolved_text_style(x, y)?;
                        Some((name.to_string(), style.color == Some(theme::current().suggestion) && !style.dim))
                    }).collect();
                    if rows.len() == 2 {
                        if stage == 0 {
                            assert_eq!(rows.iter().map(|(_, selected)| *selected).collect::<Vec<_>>(), vec![true, false]);
                            expected_path = Some(root.join(rows[1].0.trim_end_matches('/')).to_string_lossy().into_owned());
                            keys.send(TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Down))).await.unwrap();
                            stage = 1;
                        } else if stage == 1 && rows[1].1 {
                            assert!(!rows[0].1, "Down must deselect the first actual row");
                            keys.send(TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Enter))).await.unwrap();
                            stage = 2;
                        }
                    }
                    if !selected.lock().unwrap().is_empty() { break; }
                }
            }
        }
        assert_eq!(
            stage, 2,
            "Down did not advance from the first rendered suggestion to the second"
        );
        assert_eq!(
            started_paths,
            vec![
                raw_path.clone(),
                format!("{}/", expected_path.as_ref().unwrap())
            ],
            "source InputEvent must submit typed text before DOM submits the different selected suggestion"
        );
        assert_eq!(
            *selected.lock().unwrap(),
            vec![AddWorkspaceDirectorySelection {
                path: expected_path.expect("source 100ms completion did not render"),
                remember: false,
                accepted: true,
            }]
        );
        drop(renders);
        SUBMISSION_BARRIERS.lock().unwrap().remove(&raw_path);
        SUBMISSION_BARRIERS
            .lock()
            .unwrap()
            .remove(&root.join("target-a").to_string_lossy().into_owned());
        SUBMISSION_BARRIERS
            .lock()
            .unwrap()
            .remove(&root.join("target-b").to_string_lossy().into_owned());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[component]
    fn EmptyDirectoryInputProbe(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let value = hooks.use_state(String::new);
        let cursor = hooks.use_state(|| 0usize);
        element! { DirectoryInput(value: Some(value), cursor_offset: Some(cursor)) }
    }

    #[test]
    fn directory_input_matches_official_omitted_focus_placeholder() {
        let canvas = input_test_providers(element! { EmptyDirectoryInputProbe }.into_any())
            .render(Some(100));
        let text = canvas.to_string();
        let (y, line) = text
            .lines()
            .enumerate()
            .find(|(_, line)| line.contains("Directory path"))
            .expect("source placeholder");
        let x = line[..line.find("Directory path").unwrap()].chars().count();
        let style = canvas.resolved_text_style(x, y).unwrap();
        assert!(style.dim);
        assert!(!style.invert);
    }
}
