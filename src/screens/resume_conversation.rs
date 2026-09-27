//! Maps to: CC `screens/ResumeConversation.tsx`.
//!
//! Top-level bare-`--resume` screen. It owns log loading, selection,
//! `ProcessedResume` application, and the final swap into REPL.
//! Non-fork session-file adoption/write is live; telemetry, worktree mutation,
//! cost restore, and fork attribution remain explicit owner seams.

use crate::commands::resume;
use crate::components::log_selector::LogSelector;
use crate::components::spinner::Spinner;
use crate::screens::repl::ReplProps;
use crate::utils::cross_project_resume::{CrossProjectResumeResult, check_cross_project_resume};
use crate::utils::session_storage::{SessionLogResult, SessionSelection, SessionSummary};
use iocraft::prelude::*;

#[derive(Clone, Copy, Debug)]
enum SessionLogLoadRequest {
    Replace { show_all: bool, generation: u64 },
    More { count: usize, generation: u64 },
}

async fn run_session_log_task<T, F>(task: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let (sender, receiver) = async_channel::bounded(1);
    std::thread::spawn(move || {
        let value = task();
        let _ = sender.send_blocking(value);
    });
    receiver
        .recv()
        .await
        .map_err(|_| "Session log worker stopped before returning a result".to_string())
}

/// Maps to: CC `screens/ResumeConversation.tsx` `parsePrIdentifier`.
fn parse_pr_identifier(value: &str) -> Option<u64> {
    let trimmed = value.trim();
    if let Ok(number) = trimmed.parse::<u64>() {
        if number > 0 {
            return Some(number);
        }
    }

    let marker = "github.com/";
    let marker_index = trimmed.find(marker)? + marker.len();
    let path = &trimmed[marker_index..];
    let pull_marker = "/pull/";
    let pull_index = path.find(pull_marker)? + pull_marker.len();
    let digits = path[pull_index..]
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect::<String>();
    if digits.is_empty() {
        return None;
    }
    digits.parse::<u64>().ok().filter(|number| *number > 0)
}

/// Rust representation of official `filterByPr?: boolean | number | string`.
/// Maps to: CC `screens/ResumeConversation.tsx` `filterByPr` prop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResumeFilterByPr {
    Any,
    Number(u64),
    Value(String),
}

impl ResumeFilterByPr {
    /// Converts CC commander `options.fromPr` (`true | string`) to the Rust
    /// representation consumed by `ResumeConversationProps.filter_by_pr`.
    pub fn from_cli_value(value: &str) -> Self {
        if value.trim().is_empty() {
            Self::Any
        } else if let Some(number) = parse_pr_identifier(value) {
            Self::Number(number)
        } else {
            Self::Value(value.to_string())
        }
    }
}

/// Maps to: CC `screens/ResumeConversation.tsx` `filteredLogs` memo.
fn filtered_logs(
    logs: &[SessionSummary],
    filter_by_pr: Option<&ResumeFilterByPr>,
) -> Vec<SessionSummary> {
    let mut result = logs
        .iter()
        .filter(|log| !log.is_sidechain)
        .cloned()
        .collect::<Vec<_>>();

    match filter_by_pr {
        None => result,
        Some(ResumeFilterByPr::Any) => {
            result.retain(|log| log.pr_number.is_some());
            result
        }
        Some(ResumeFilterByPr::Number(pr_number)) => {
            result.retain(|log| log.pr_number == Some(*pr_number));
            result
        }
        Some(ResumeFilterByPr::Value(value)) => {
            if let Some(pr_number) = parse_pr_identifier(value) {
                result.retain(|log| log.pr_number == Some(pr_number));
            }
            result
        }
    }
}

#[derive(Default, Props)]
struct NoConversationsMessageProps {}

/// Maps to: CC `screens/ResumeConversation.tsx` `NoConversationsMessage`.
///
/// iocraft has no process exit-code channel on `App`; `app:interrupt` exits
/// the retained root while preserving the official visible behavior.
#[component]
fn NoConversationsMessage(
    _props: &NoConversationsMessageProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let mut app = hooks.use_app();
    let mut pending_exit = hooks.use_state(|| false);
    let keybinding_runtime = hooks
        .try_use_context::<crate::keybindings::keybinding_context::KeybindingRuntime>()
        .map(|runtime| runtime.clone());
    crate::keybindings::use_keybinding::use_keybinding(
        &mut hooks,
        keybinding_runtime,
        "app:interrupt",
        crate::keybindings::types::ContextName::Global,
        || true,
        move || {
            pending_exit.set(true);
            true
        },
    );
    if pending_exit.get() {
        app.exit();
    }

    element! {
        View(flex_direction: FlexDirection::Column) {
            Text(content: "No conversations found to resume.".to_string())
            Text(content: "Press Ctrl+C to exit and start a new conversation.".to_string(), dim: true)
        }
    }
}

#[derive(Default, Props)]
struct CrossProjectMessageProps {
    command: String,
}

/// Maps to: CC `screens/ResumeConversation.tsx` `CrossProjectMessage`.
///
/// Like CC's mount effect, this screen exits shortly after rendering the
/// command copied by `ResumeConversation.onSelect`.
#[component]
fn CrossProjectMessage(
    props: &CrossProjectMessageProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let mut app = hooks.use_app();
    hooks.use_future(async move {
        futures_timer::Delay::new(std::time::Duration::from_millis(100)).await;
        app.exit();
    });

    element! {
        View(flex_direction: FlexDirection::Column, row_gap: 1u32) {
            Text(content: "This conversation is from a different directory.".to_string())
            View(flex_direction: FlexDirection::Column) {
                Text(content: "To resume, run:".to_string())
                Text(content: format!(" {}", props.command))
            }
            Text(content: "(Command copied to clipboard)".to_string(), dim: true)
        }
    }
}

/// Maps to: CC `screens/ResumeConversation.tsx:63-376` top-level chooser props.
/// Session-config fields are carried together as the same immutable `ReplProps`
/// later spread into REPL.
#[derive(Default, Props)]
pub struct ResumeConversationProps {
    pub repl_props: ReplProps,
    /// Maps to: CC `Props.worktreePaths`.
    pub worktree_paths: Vec<String>,
    /// Maps to: CC `Props.initialSearchQuery`.
    pub initial_search_query: Option<String>,
    /// Maps to: CC `Props.filterByPr`.
    pub filter_by_pr: Option<ResumeFilterByPr>,
}

/// Maps to: CC `screens/ResumeConversation.tsx:89-376`.
///
/// Unlike the `/resume` local-command panel, this is the startup chooser used
/// by bare `--resume`: it loads logs under App, processes the selected session,
/// and only then replaces itself with REPL. Processing adopts the non-fork
/// transcript; worktree/cost/telemetry side effects remain explicit seams.
#[component]
pub fn ResumeConversation(
    props: &ResumeConversationProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let mut app = hooks.use_app();
    let (stdout, _) = hooks.use_output();
    let app_store = hooks
        .try_use_context::<crate::state::store::AppStore>()
        .expect("ResumeConversation requires AppStore")
        .clone();
    let mut logs = hooks.use_state(Vec::<SessionSummary>::new);
    let mut show_all_projects = hooks.use_state(|| false);
    let mut loading = hooks.use_state(|| true);
    let mut loading_more = hooks.use_state(|| false);
    let mut resuming = hooks.use_state(|| false);
    let mut initial_load_sent = hooks.use_state(|| false);
    let mut load_generation = hooks.use_state(|| 0u64);
    let mut selected_repl_props = hooks.use_state(|| Option::<ReplProps>::None);
    let mut cross_project_command = hooks.use_state(|| Option::<String>::None);
    let mut resume_error = hooks.use_state(|| Option::<String>::None);
    let load_channel = hooks
        .use_const(|| std::sync::Arc::new(async_channel::unbounded::<SessionLogLoadRequest>()));
    let load_tx_initial = load_channel.0.clone();
    let load_tx_toggle = load_channel.0.clone();
    let load_rx = load_channel.1.clone();
    let restore_channel = hooks.use_const(|| {
        std::sync::Arc::new(async_channel::unbounded::<
            Result<crate::utils::session_restore::ProcessedResume, String>,
        >())
    });
    let restore_tx = restore_channel.0.clone();
    let restore_rx = restore_channel.1.clone();

    hooks.use_future({
        let worktree_paths = props.worktree_paths.clone();
        async move {
            let mut current_result: Option<(u64, SessionLogResult)> = None;
            while let Ok(request) = load_rx.recv().await {
                match request {
                    SessionLogLoadRequest::Replace {
                        show_all,
                        generation,
                    } => {
                        loading.set(true);
                        loading_more.set(false);
                        let paths = worktree_paths.clone();
                        let loaded = run_session_log_task(move || {
                            if show_all {
                                crate::utils::session_storage::load_all_projects_message_logs_progressive()
                            } else {
                                crate::utils::session_storage::load_same_repo_message_logs_progressive(
                                    &paths,
                                )
                            }
                        })
                        .await;
                        if let Ok(result) = loaded {
                            if generation == load_generation.get() {
                                logs.set(result.logs.clone());
                                current_result = Some((generation, result));
                            }
                        }
                        if generation == load_generation.get() {
                            loading.set(false);
                        }
                    }
                    SessionLogLoadRequest::More { count, generation } => {
                        let Some((current_generation, result)) = current_result.as_mut() else {
                            continue;
                        };
                        if generation != *current_generation
                            || generation != load_generation.get()
                            || result.next_index >= result.all_stat_logs.len()
                        {
                            continue;
                        }
                        loading_more.set(true);
                        let current_generation = *current_generation;
                        let mut result = current_result
                            .take()
                            .expect("validated progressive result must remain present")
                            .1;
                        let enriched = run_session_log_task(move || {
                            let enriched = crate::utils::session_storage::enrich_logs(
                                &result.all_stat_logs,
                                result.next_index,
                                count,
                            );
                            result.next_index = enriched.next_index;
                            (result, enriched.logs)
                        })
                        .await;
                        if let Ok((result, enriched_logs)) = enriched {
                            if !enriched_logs.is_empty()
                                && generation == load_generation.get()
                            {
                                let mut next_logs = logs.read().clone();
                                // CC mutates `LogOption.value` to offset+i.
                                // Rust derives row values from this full Vec on
                                // every render, so append order performs the
                                // same renumbering without a second index.
                                next_logs.extend(enriched_logs);
                                logs.set(next_logs);
                            }
                            current_result = Some((current_generation, result));
                        }
                        loading_more.set(false);
                    }
                }
            }
        }
    });

    hooks.use_future({
        let app_store = app_store.clone();
        let base_repl_props = props.repl_props.clone();
        async move {
            while let Ok(restored) = restore_rx.recv().await {
                resuming.set(false);
                match restored {
                    Ok(processed) => {
                        // B3 flip-audit SEAM: CC builds this into the
                        // pre-mount initialState spread (sessionRestore.ts:
                        // 534-550) with no store event; Cometix applies it to
                        // the mounted store once per resume selection. A
                        // value-equal root here notifies post-flip (one
                        // idempotent version-counter bump) — no CC updater
                        // guard exists to violate. Full pre-provider silent
                        // adoption lands with Contract B (P5).
                        app_store.replace_with(|state| processed.apply_to_app_state(state));
                        let mut repl_props = base_repl_props.clone();
                        repl_props.apply_processed_resume(&processed);
                        selected_repl_props.set(Some(repl_props));
                    }
                    Err(error) => resume_error.set(Some(error)),
                }
            }
        }
    });
    // Maps to: CC ResumeConversation.tsx:227-232 onSelect's clipboard await.
    // Preserve the component-owned crossProjectCommand state and output ordering.
    let on_select_cross_project = Handler::from(move |command: String| {
        let stdout = stdout.clone();
        let copy = stdout.prepare_clipboard(&command);
        crate::utils::process_runtime::runtime_handle_for_detached_work()
            .expect("clipboard requires the initialized process runtime")
            .spawn(async move {
                let raw = copy.await;
                if !raw.is_empty() {
                    if let Err(error) = stdout.write_control_sequence_and_wait(raw).await {
                        crate::utils::debug::log_for_debugging(&error.to_string());
                        return;
                    }
                }
                cross_project_command.set(Some(command));
            });
    });
    // Hooks must remain unconditional across loading/empty/selector/resuming
    // branches. CC may call this inline because React identifies hooks by the
    // component invocation; iocraft enforces the same order on every render.
    let (_, rows) = hooks.use_terminal_size();
    // CC passes `rows`; Rust LogSelector's retained root intentionally renders
    // `max_height - 1`, so `rows + 1` is the established framework transform.
    let max_height = rows.saturating_add(1).max(1) as usize;

    if !initial_load_sent.get() {
        initial_load_sent.set(true);
        let _ = load_tx_initial.try_send(SessionLogLoadRequest::Replace {
            show_all: false,
            generation: 0,
        });
    }

    if let Some(repl_props) = selected_repl_props.read().clone() {
        return repl_props.into_element();
    }

    if let Some(error) = resume_error.read().clone() {
        return element! {
            View(flex_direction: FlexDirection::Column) {
                Text(content: error)
                Text(content: "Press Ctrl+C to exit and start a new conversation.".to_string(), dim: true)
            }
        }
        .into_any();
    }

    if let Some(command) = cross_project_command.read().clone() {
        return element! { CrossProjectMessage(command: command) }.into_any();
    }
    if loading.get() && logs.read().is_empty() {
        return element! {
            View(flex_direction: FlexDirection::Row) {
                Spinner
                Text(content: " Loading conversations…".to_string(), wrap: TextWrap::NoWrap)
            }
        }
        .into_any();
    }
    if resuming.get() {
        return element! {
            View(flex_direction: FlexDirection::Row) {
                Spinner
                Text(content: " Resuming conversation…".to_string(), wrap: TextWrap::NoWrap)
            }
        }
        .into_any();
    }

    let filtered_logs = filtered_logs(&logs.read(), props.filter_by_pr.as_ref());
    if filtered_logs.is_empty() {
        return element! { NoConversationsMessage() }.into_any();
    }

    let initial_agent_for_select = props.repl_props.main_thread_agent_definition.clone();
    let worktree_paths_for_select = props.worktree_paths.clone();
    let app_store_for_select = app_store.clone();
    let load_tx_more = load_channel.0.clone();
    let load_tx_reload = load_channel.0.clone();
    element! {
        LogSelector(
            logs: filtered_logs,
            max_height: max_height,
            initial_search_query: props.initial_search_query.clone(),
            show_all_projects: show_all_projects.get(),
            is_loading: loading.get() || loading_more.get(),
            reload_generation: load_generation.get(),
            on_cancel: move |_| app.exit(),
            on_logs_changed: move |_| {
                let generation = load_generation.get().wrapping_add(1);
                load_generation.set(generation);
                loading.set(true);
                let _ = load_tx_reload.try_send(SessionLogLoadRequest::Replace {
                    show_all: show_all_projects.get(),
                    generation,
                });
            },
            on_select: move |selection: SessionSelection| {
                match check_cross_project_resume(
                    &selection,
                    show_all_projects.get(),
                    &worktree_paths_for_select,
                ) {
                    CrossProjectResumeResult::DifferentProject { command, .. } => {
                        // CC onSelect sets resuming before the clipboard await (:218).
                        resuming.set(true);
                        on_select_cross_project(command);
                    }
                    CrossProjectResumeResult::SameProject
                    | CrossProjectResumeResult::SameRepoWorktree { .. } => {
                        resuming.set(true);
                        resume_error.set(None);
                        let restore_tx = restore_tx.clone();
                        let initial_agent = initial_agent_for_select.clone();
                        let agent_definitions =
                            app_store_for_select.get().agent_definitions.clone();
                        std::thread::spawn(move || {
                            let restored = resume::load_for_picker_selection(&selection)
                                .map_err(|error| error.to_string())
                                .and_then(|target| {
                                    crate::utils::conversation_recovery::load_conversation_for_resume(
                                        &target,
                                    )
                                })
                                .and_then(|loaded| {
                                    crate::utils::session_restore::process_resumed_conversation(
                                        loaded,
                                        initial_agent,
                                        agent_definitions,
                                    )
                                });
                            let _ = restore_tx.send_blocking(restored);
                        });
                    }
                }
            },
            on_load_more: move |count: usize| {
                let _ = load_tx_more.try_send(SessionLogLoadRequest::More {
                    count,
                    generation: load_generation.get(),
                });
            },
            on_toggle_all_projects: move |_| {
                let next = !show_all_projects.get();
                let generation = load_generation.get().wrapping_add(1);
                show_all_projects.set(next);
                load_generation.set(generation);
                loading.set(true);
                let _ = load_tx_toggle.try_send(SessionLogLoadRequest::Replace {
                    show_all: next,
                    generation,
                });
            },
        )
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;
    use std::path::PathBuf;

    fn session(id: &str, pr_number: Option<u64>, is_sidechain: bool) -> SessionSummary {
        SessionSummary {
            session_id: id.to_string(),
            display: format!("Session {id}"),
            pr_number,
            is_sidechain,
            project_path: Some("/repo".to_string()),
            file_path: PathBuf::from(format!("/repo/{id}.jsonl")),
            ..SessionSummary::default()
        }
    }

    #[test]
    fn parse_pr_identifier_matches_official_number_and_github_url_forms() {
        assert_eq!(parse_pr_identifier("42"), Some(42));
        assert_eq!(
            parse_pr_identifier("https://github.com/anthropics/claude-code/pull/123"),
            Some(123)
        );
        assert_eq!(parse_pr_identifier("0"), None);
        assert_eq!(parse_pr_identifier("not-a-pr"), None);
        assert_eq!(ResumeFilterByPr::from_cli_value(""), ResumeFilterByPr::Any);
        assert_eq!(
            ResumeFilterByPr::from_cli_value("42"),
            ResumeFilterByPr::Number(42)
        );
        assert_eq!(
            ResumeFilterByPr::from_cli_value("not-a-pr"),
            ResumeFilterByPr::Value("not-a-pr".to_string())
        );
    }

    #[test]
    fn filter_resume_logs_matches_official_sidechain_and_pr_filters() {
        let logs = vec![
            session("a", Some(7), false),
            session("b", Some(9), false),
            session("c", None, false),
            session("d", Some(7), true),
        ];

        assert_eq!(
            filtered_logs(&logs, None)
                .iter()
                .map(|log| log.session_id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b", "c"]
        );
        assert_eq!(
            filtered_logs(&logs, Some(&ResumeFilterByPr::Any))
                .iter()
                .map(|log| log.session_id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        assert_eq!(
            filtered_logs(&logs, Some(&ResumeFilterByPr::Number(7)))
                .iter()
                .map(|log| log.session_id.as_str())
                .collect::<Vec<_>>(),
            vec!["a"]
        );
        assert_eq!(
            filtered_logs(
                &logs,
                Some(&ResumeFilterByPr::Value(
                    "https://github.com/org/repo/pull/9".to_string()
                ))
            )
            .iter()
            .map(|log| log.session_id.as_str())
            .collect::<Vec<_>>(),
            vec!["b"]
        );
    }

    #[test]
    fn no_conversations_message_preserves_official_visible_copy() {
        let current_theme = *theme::current();
        let empty = element! {
            ContextProvider(value: Context::owned(current_theme)) {
                NoConversationsMessage()
            }
        }
        .render(Some(80))
        .to_string();
        assert!(
            empty.contains("No conversations found to resume."),
            "canvas=\n{empty}"
        );
        assert!(
            empty.contains("Press Ctrl+C to exit and start a new conversation."),
            "canvas=\n{empty}"
        );
    }

    #[test]
    fn cross_project_message_preserves_official_visible_copy() {
        let text = element! {
            CrossProjectMessage(command: "cd /tmp/repo && claude --resume abc".to_string())
        }
        .render(Some(100))
        .to_string();

        assert!(
            text.contains("This conversation is from a different directory."),
            "canvas=\n{text}"
        );
        assert!(text.contains("To resume, run:"), "canvas=\n{text}");
        assert!(
            text.contains(" cd /tmp/repo && claude --resume abc"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("(Command copied to clipboard)"),
            "canvas=\n{text}"
        );
    }
}
