//! Maps to: CC `commands/add-dir/add-dir.tsx`.
//! `call` retains the captured permission context; confirmation reads the latest
//! store. Worker channels only adapt the async local-JSX boundary to iocraft.

use super::validation::{
    AddDirectoryResult, add_dir_help_message, validate_directory_for_workspace,
};
use crate::bootstrap::state::{
    get_additional_directories_for_claude_md, set_additional_directories_for_claude_md,
};
use crate::components::message_response::MessageResponse;
use crate::components::permissions::rules::add_workspace_directory::{
    AddWorkspaceDirectory, AddWorkspaceDirectorySelection,
};
use crate::state::store::AppStore;
use crate::tool::ToolPermissionContext;
use crate::types::permissions::{PermissionUpdate, PermissionUpdateDestination};
use crate::utils::permissions::permission_update::{
    apply_permission_update, persist_permission_update,
};
use iocraft::prelude::*;
use std::path::PathBuf;
use std::sync::Arc;

/// Typed ReactNode carrier for `call`'s two return components (CC :95-146).
#[derive(Clone, Debug)]
pub struct AddDirCall {
    pub permission_context: Arc<ToolPermissionContext>,
    pub directory_path: Option<String>,
    pub error: Option<String>,
}

/// Maps to: CC `commands/add-dir/add-dir.tsx#call`.
pub async fn call(args: &str, app_store: &AppStore) -> anyhow::Result<AddDirCall> {
    let directory_path = args.trim_matches(|character| {
        matches!(character,
            '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}' |
            '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' |
            '\u{205f}' | '\u{3000}' | '\u{feff}'
        )
    });
    let permission_context = app_store.get().tool_permission_context.clone();
    let mut result = AddDirCall {
        permission_context,
        directory_path: None,
        error: None,
    };
    if !directory_path.is_empty() {
        match validate_directory_for_workspace(directory_path, &result.permission_context).await? {
            AddDirectoryResult::Success { absolute_path } => {
                result.directory_path = Some(absolute_path)
            }
            failure => result.error = Some(add_dir_help_message(&failure)),
        }
    }
    Ok(result)
}

/// Maps to: CC `commands/add-dir/add-dir.tsx#handleAddDirectory`.
/// Invoked off the retained frame: canonical permission persistence and sandbox
/// refresh may perform filesystem work. No validation is duplicated here.
pub fn handle_add_directory(path: &str, remember: bool, app_store: &AppStore) -> String {
    let permission_update = PermissionUpdate::AddDirectories {
        directories: vec![path.to_string()],
        destination: if remember {
            PermissionUpdateDestination::LocalSettings
        } else {
            PermissionUpdateDestination::Session
        },
    };
    // AppStore's synchronous turn keeps CC's getState → apply → setState
    // segment atomic with respect to native tool-execution threads.
    app_store.replace_with(|state| {
        state.tool_permission_context = Arc::new(apply_permission_update(
            &state.tool_permission_context,
            &permission_update,
        ));
    });
    let mut current_dirs = get_additional_directories_for_claude_md();
    let directory = PathBuf::from(path);
    if !current_dirs.contains(&directory) {
        current_dirs.push(directory);
        set_additional_directories_for_claude_md(current_dirs);
    }
    crate::utils::sandbox::sandbox_adapter::refresh_config();
    // Same no-color Chalk projection as validation.rs. The shared Chalk
    // force-color/environment owner remains unavailable; text is source exact.
    let message = if remember {
        match persist_permission_update(&permission_update) {
            Ok(_) => format!("Added {path} as a working directory and saved to local settings"),
            Err(error) => format!(
                "Added {path} as a working directory. Failed to save to local settings: {error}"
            ),
        }
    } else {
        format!("Added {path} as a working directory for this session")
    };
    format!("{message} · /permissions to manage")
}

#[derive(Default, Props)]
pub struct AddDirErrorProps<'a> {
    pub message: String,
    pub args: String,
    pub on_done: HandlerMut<'a, ()>,
}

/// Maps to: CC `commands/add-dir/add-dir.tsx:22-43#AddDirError`.
#[component]
pub fn AddDirError<'a>(
    props: &mut AddDirErrorProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let mut ready = hooks.use_state(|| false);
    hooks.use_future(async move {
        // CC setTimeout(onDone, 0): defer completion until after the error tree
        // has rendered. Dropping this component drops the pending future.
        futures_timer::Delay::new(std::time::Duration::ZERO).await;
        ready.set(true);
    });
    if ready.get() {
        ready.set(false);
        (props.on_done)(());
    }
    element! {
        View(flex_direction: FlexDirection::Column) {
            Text(content: format!("{} /add-dir {}", crate::constants::figures::figures().pointer, props.args), dim: true)
            MessageResponse(content: props.message.clone())
        }
    }
}

/// Native delivery carrier for call's returned node, onDone, and rejected call.
enum AddDirOutcome {
    Prepared(AddDirCall),
    Done(String),
    Failed(String),
}

#[derive(Default, Props)]
pub struct AddDirCommandProps<'a> {
    pub args: String,
    pub app_store: Option<AppStore>,
    pub on_done: HandlerMut<'a, String>,
    pub on_error: HandlerMut<'a, ()>,
}

/// Retained async ReactNode carrier for CC `call`; domain branches stay in call
/// and its source-owned handleAddDirectory closure above (PORTING async mapping).
#[component]
pub fn AddDirCommand<'a>(
    props: &mut AddDirCommandProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let mut prepared = hooks.use_state(|| None::<AddDirCall>);
    let mut pending_done = hooks.use_state(|| None::<String>);
    let mut pending_error = hooks.use_state(|| false);
    let requests =
        hooks.use_const(|| Arc::new(async_channel::unbounded::<AddWorkspaceDirectorySelection>()));
    let outcomes = hooks.use_const({
        let args = props.args.clone();
        let store = props.app_store.clone();
        let receiver = requests.1.clone();
        move || {
            let (tx, rx) = async_channel::unbounded::<AddDirOutcome>();
            tokio::spawn(async move {
                let Some(store) = store else {
                    let _ = tx
                        .send(AddDirOutcome::Failed(
                            "App state is unavailable".to_string(),
                        ))
                        .await;
                    return;
                };
                match call(&args, &store).await {
                    Ok(prepared) => {
                        if tx.send(AddDirOutcome::Prepared(prepared)).await.is_err() {
                            return;
                        }
                    }
                    Err(error) => {
                        let _ = tx.send(AddDirOutcome::Failed(error.to_string())).await;
                        return;
                    }
                }
                while let Ok(selection) = receiver.recv().await {
                    let store = store.clone();
                    let message = tokio::task::spawn_blocking(move || {
                        handle_add_directory(&selection.path, selection.remember, &store)
                    })
                    .await;
                    let outcome = match message {
                        Ok(message) => AddDirOutcome::Done(message),
                        Err(error) => AddDirOutcome::Failed(error.to_string()),
                    };
                    if tx.send(outcome).await.is_err() {
                        break;
                    }
                }
            });
            Arc::new(rx)
        }
    });
    hooks.use_future(async move {
        while let Ok(result) = outcomes.recv().await {
            match result {
                AddDirOutcome::Prepared(result) => prepared.set(Some(result)),
                AddDirOutcome::Done(message) => pending_done.set(Some(message)),
                AddDirOutcome::Failed(message) => {
                    crate::utils::log::log_error(crate::utils::log::LogError::new(message));
                    pending_error.set(true);
                }
            }
        }
    });
    if pending_error.get() {
        pending_error.set(false);
        (props.on_error)(());
    }
    let completed = pending_done.read().clone();
    if let Some(message) = completed {
        pending_done.set(None);
        (props.on_done)(message);
    }
    let Some(prepared) = prepared.read().clone() else {
        return element! { View }.into_any();
    };
    if let Some(message) = prepared.error {
        let done_message = message.clone();
        return element! {
            AddDirError(message: message, args: props.args.clone(), on_done: move |_| pending_done.set(Some(done_message.clone())))
        }.into_any();
    }
    let cancel_message = prepared.directory_path.as_ref().map_or_else(
        || "Did not add a working directory.".to_string(),
        |path| format!("Did not add {path} as a working directory."),
    );
    let sender = requests.0.clone();
    element! {
        AddWorkspaceDirectory(
            directory_path: prepared.directory_path,
            permission_context: prepared.permission_context,
            on_add_directory: Some(Arc::new(move |selection| { let _ = sender.try_send(selection); }) as crate::components::permissions::rules::add_workspace_directory::AddWorkspaceDirectoryCallback),
            on_cancel: move |_| pending_done.set(Some(cancel_message.clone())),
        )
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::app_state_store::AppState;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[tokio::test]
    async fn call_matches_official_empty_input_and_help_branch() {
        let store = AppStore::new(AppState::default(), None);
        for args in ["", " \t\u{feff}"] {
            let result = call(args, &store).await.unwrap();
            assert!(result.directory_path.is_none());
            assert!(result.error.is_none());
            assert!(Arc::ptr_eq(
                &result.permission_context,
                &store.get().tool_permission_context
            ));
        }
        let missing = format!("/tmp/add-dir-missing-{}", uuid::Uuid::new_v4());
        let result = call(&missing, &store).await.unwrap();
        assert_eq!(result.error, Some(format!("Path {missing} was not found.")));
    }

    #[test]
    fn handle_add_directory_matches_official_latest_context_and_bootstrap_order() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let prior_dirs = get_additional_directories_for_claude_md();
        set_additional_directories_for_claude_md(vec![PathBuf::from("/first")]);
        let store = AppStore::new(AppState::default(), None);
        let message = handle_add_directory("/second", false, &store);
        assert_eq!(
            message,
            "Added /second as a working directory for this session · /permissions to manage"
        );
        handle_add_directory("/second", false, &store);
        assert_eq!(
            get_additional_directories_for_claude_md(),
            vec![PathBuf::from("/first"), PathBuf::from("/second")]
        );
        assert!(
            store
                .get()
                .tool_permission_context
                .additional_working_directories
                .contains_key("/second")
        );
        set_additional_directories_for_claude_md(prior_dirs);
    }
    #[test]
    fn handle_add_directory_matches_official_failed_remember_keeps_session_update() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let prior_dirs = get_additional_directories_for_claude_md();
        let previous = crate::bootstrap::state::is_session_persistence_disabled();
        crate::bootstrap::state::set_session_persistence_disabled(true);
        let store = AppStore::new(AppState::default(), None);
        let message = handle_add_directory("/remember-failure", true, &store);
        assert_eq!(
            message,
            format!(
                "Added /remember-failure as a working directory. Failed to save to local settings: {} · /permissions to manage",
                crate::tools::shared::write_gate::PERMISSION_PERSISTENCE_DISABLED_ERROR
            )
        );
        assert_eq!(
            store
                .get()
                .tool_permission_context
                .additional_working_directories["/remember-failure"]
                .source,
            crate::types::permissions::PermissionRuleSource::LocalSettings
        );
        assert!(
            get_additional_directories_for_claude_md()
                .contains(&PathBuf::from("/remember-failure"))
        );
        crate::bootstrap::state::set_session_persistence_disabled(previous);
        set_additional_directories_for_claude_md(prior_dirs);
    }
    #[test]
    fn handle_add_directory_matches_official_remember_writes_local_only() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!("cc-add-dir-save-{}", uuid::Uuid::new_v4()));
        let workdir = root.join("workspace");
        std::fs::create_dir_all(&workdir).unwrap();
        let old_cwd = crate::bootstrap::state::get_original_cwd();
        let old_dirs = get_additional_directories_for_claude_md();
        let _write = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", root.join("config"));
        let _managed = EnvVarGuard::set("CLAUDE_CODE_MANAGED_SETTINGS_PATH", &root);
        let _skip = EnvVarGuard::unset("CLAUDE_CODE_SKIP_PROMPT_HISTORY");
        crate::bootstrap::state::set_original_cwd(&workdir);
        crate::utils::settings::settings_cache::reset_settings_cache();
        let store = AppStore::new(AppState::default(), None);
        assert_eq!(
            handle_add_directory("/saved-extra", true, &store),
            "Added /saved-extra as a working directory and saved to local settings · /permissions to manage"
        );
        let settings: serde_json::Value = serde_json::from_slice(
            &std::fs::read(workdir.join(".claude/settings.local.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            settings["permissions"]["additionalDirectories"],
            serde_json::json!(["/saved-extra"])
        );
        assert!(!workdir.join(".claude/settings.json").exists());
        assert!(!root.join("config/settings.json").exists());
        crate::bootstrap::state::set_original_cwd(old_cwd);
        set_additional_directories_for_claude_md(old_dirs);
        crate::utils::settings::settings_cache::reset_settings_cache();
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn add_dir_rejected_call_matches_official_error_cleanup_without_on_done() {
        use futures::{FutureExt, StreamExt};
        use std::sync::atomic::{AtomicUsize, Ordering};
        let done = Arc::new(AtomicUsize::new(0));
        let errors = Arc::new(AtomicUsize::new(0));
        let observed_done = done.clone();
        let observed_error = errors.clone();
        let store = AppStore::new(AppState::default(), None);
        assert!(call("invalid\0path", &store).await.is_err());
        let mut app = element! {
            AddDirCommand(args: "invalid\0path".to_string(), app_store: Some(store),
                on_done: move |_| { observed_done.fetch_add(1, Ordering::SeqCst); },
                on_error: move |_| { observed_error.fetch_add(1, Ordering::SeqCst); },
            )
        };
        let mut frames = Box::pin(app.mock_terminal_render_loop(
            MockTerminalConfig::with_events(futures::stream::pending()).with_size(80, 10),
        ));
        let timeout = futures_timer::Delay::new(std::time::Duration::from_secs(2)).fuse();
        futures::pin_mut!(timeout);
        loop {
            futures::select! {
                _ = timeout => break,
                _ = frames.next().fuse() => if errors.load(Ordering::SeqCst) > 0 { break; },
            }
        }
        assert_eq!(errors.load(Ordering::SeqCst), 1);
        assert_eq!(done.load(Ordering::SeqCst), 0);
    }
}
