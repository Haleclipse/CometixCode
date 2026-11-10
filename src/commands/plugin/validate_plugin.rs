//! Maps to: CC `commands/plugin/ValidatePlugin.tsx`.
use crate::utils::plugins::validate_plugin::validate_manifest;
use iocraft::prelude::*;
use std::sync::{
    Arc,
    atomic::{AtomicI32, Ordering},
};

#[derive(Default, Props)]
pub struct ValidatePluginProps {
    pub path: Option<String>,
    pub on_complete: Handler<Option<String>>,
}

/// Maps to: CC `ValidatePlugin.tsx:17-94#runValidation` (nested effect body).
async fn run_validation(
    path: Option<String>,
    exit_code: Arc<AtomicI32>,
    on_complete: Handler<Option<String>>,
    #[cfg(test)] fixture: Option<ValidateManifestFixture>,
) {
    let Some(path) = path.filter(|path| !path.is_empty()) else {
        on_complete(Some(
            concat!(
                "Usage: /plugin validate <path>\n\n",
                "Validate a plugin or marketplace manifest file or directory.\n\n",
                "Examples:\n",
                "  /plugin validate .claude-plugin/plugin.json\n",
                "  /plugin validate /path/to/plugin-directory\n",
                "  /plugin validate .\n\n",
                "When given a directory, automatically validates .claude-plugin/marketplace.json\n",
                "or .claude-plugin/plugin.json (prefers marketplace if both exist).\n\n",
                "Or from the command line:\n",
                "  nyan plugin validate <path>"
            )
            .to_string(),
        ));
        return;
    };
    let figures = crate::constants::figures::figures();
    #[cfg(test)]
    let result = match fixture {
        Some(fixture) => (fixture.0)(path).await,
        None => validate_manifest(&path).await,
    };
    #[cfg(not(test))]
    let result = validate_manifest(&path).await;
    match result {
        Ok(result) => {
            let file_type = result.file_type.as_str();
            let mut output = format!("Validating {file_type} manifest: {}\n\n", result.file_path);
            if !result.errors.is_empty() {
                output += &format!(
                    "{} Found {} {}:\n\n",
                    figures.cross,
                    result.errors.len(),
                    crate::utils::string_utils::plural(result.errors.len(), "error", None)
                );
                for error in &result.errors {
                    output += &format!("  {} {}: {}\n", figures.pointer, error.path, error.message);
                }
                output.push('\n');
            }
            if !result.warnings.is_empty() {
                output += &format!(
                    "{} Found {} {}:\n\n",
                    figures.warning,
                    result.warnings.len(),
                    crate::utils::string_utils::plural(result.warnings.len(), "warning", None)
                );
                for warning in &result.warnings {
                    output += &format!(
                        "  {} {}: {}\n",
                        figures.pointer, warning.path, warning.message
                    );
                }
                output.push('\n');
            }
            if result.success {
                output += &format!(
                    "{} Validation passed{}\n",
                    figures.tick,
                    if result.warnings.is_empty() {
                        ""
                    } else {
                        " with warnings"
                    }
                );
                exit_code.store(0, Ordering::SeqCst);
            } else {
                output += &format!("{} Validation failed\n", figures.cross);
                exit_code.store(1, Ordering::SeqCst);
            }
            on_complete(Some(output));
        }
        Err(error) => {
            exit_code.store(2, Ordering::SeqCst);
            let message = error.to_string();
            crate::utils::log::log_error(crate::utils::log::LogError::new(message.clone()));
            on_complete(Some(format!(
                "{} Unexpected error during validation: {message}",
                figures.cross
            )));
        }
    }
}

/// Maps to: CC `ValidatePlugin.tsx:15-103#ValidatePlugin`.
#[component]
pub fn ValidatePlugin(
    props: &mut ValidatePluginProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    // Main publishes its existing process-exit carrier. Validation sets the
    // eventual status; it never calls process::exit while the TUI is running.
    let exit_code = hooks.use_context::<Arc<AtomicI32>>().clone();
    let path = props.path.clone();
    let on_complete = props.on_complete.clone();
    // Handler's owned Fn identity is stable across clones, as is a React
    // callback reference. There is no second callback/validation cache.
    let callback_identity = (&*on_complete as *const dyn Fn(Option<String>)) as *const () as usize;
    let dependencies = (path.clone(), callback_identity);
    #[cfg(test)]
    let fixture = hooks
        .try_use_context::<ValidateManifestFixture>()
        .map(|value| value.clone());
    hooks.use_effect(
        move || {
            crate::utils::process_runtime::runtime_handle_for_detached_work()
                .expect("validation requires the initialized process runtime")
                .spawn(run_validation(
                    path,
                    exit_code,
                    on_complete,
                    #[cfg(test)]
                    fixture,
                ));
        },
        dependencies,
    );
    element! { View(flex_direction: FlexDirection::Column) { Text(content: "Running validation...") } }
}

/// Test-only imported validateManifest boundary; component/effect/formatter are real.
#[cfg(test)]
#[derive(Clone)]
struct ValidateManifestFixture(
    Arc<
        dyn Fn(
                String,
            ) -> futures::future::BoxFuture<
                'static,
                anyhow::Result<crate::utils::plugins::validate_plugin::ValidationResult>,
            > + Send
            + Sync,
    >,
);

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn missing_path_usage_preserves_exit_status_and_matches_official_output() {
        let exit_code = Arc::new(AtomicI32::new(1));
        let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let on_complete = Handler::from({
            let observed = observed.clone();
            let exit_code = exit_code.clone();
            move |output| {
                observed
                    .lock()
                    .unwrap()
                    .push((exit_code.load(Ordering::SeqCst), output))
            }
        });
        run_validation(None, exit_code.clone(), on_complete.clone(), None).await;
        run_validation(Some(String::new()), exit_code, on_complete, None).await;
        let observed = observed.lock().unwrap();
        assert_eq!(observed.len(), 2);
        assert_eq!(observed[0], observed[1]);
        assert_eq!(observed[0].0, 1);
        let output = observed[0].1.as_deref().unwrap();
        assert!(output.starts_with("Usage: /plugin validate <path>\n\n"));
        assert!(output.ends_with("  nyan plugin validate <path>"));
    }

    fn successful_result() -> crate::utils::plugins::validate_plugin::ValidationResult {
        crate::utils::plugins::validate_plugin::ValidationResult {
            success: true,
            errors: Vec::new(),
            warnings: Vec::new(),
            file_path: "/fixture/plugin.json".into(),
            file_type: crate::utils::plugins::validate_plugin::ValidationFileType::Plugin,
        }
    }

    #[tokio::test]
    async fn validation_output_and_callback_exit_status_match_official_bun() {
        use crate::utils::plugins::validate_plugin::{ValidationError, ValidationWarning};
        let mut warning = successful_result();
        warning.warnings.push(ValidationWarning {
            path: "version".into(),
            message: "No version specified".into(),
        });
        let mut failure = warning.clone();
        failure.success = false;
        failure.errors = vec![
            ValidationError {
                path: "name".into(),
                message: "Required".into(),
                code: None,
            },
            ValidationError {
                path: "author".into(),
                message: "Invalid".into(),
                code: None,
            },
        ];
        // Literal outputs from original mounted Bun ValidatePlugin, not a
        // second formatter, figures-derived assertion, or source-file include.
        let cases = [
            (
                Ok(successful_result()),
                0,
                "Validating plugin manifest: /fixture/plugin.json\n\n✔ Validation passed\n",
            ),
            (
                Ok(warning),
                0,
                "Validating plugin manifest: /fixture/plugin.json\n\n⚠ Found 1 warning:\n\n  ❯ version: No version specified\n\n✔ Validation passed with warnings\n",
            ),
            (
                Ok(failure),
                1,
                "Validating plugin manifest: /fixture/plugin.json\n\n✘ Found 2 errors:\n\n  ❯ name: Required\n  ❯ author: Invalid\n\n⚠ Found 1 warning:\n\n  ❯ version: No version specified\n\n✘ Validation failed\n",
            ),
            (
                Err("fixture failure".to_string()),
                2,
                "✘ Unexpected error during validation: fixture failure",
            ),
        ];
        for (answer, expected_code, expected_text) in cases {
            let exit_code = Arc::new(AtomicI32::new(9));
            let observed = Arc::new(std::sync::Mutex::new(None));
            let on_complete = Handler::from({
                let observed = observed.clone();
                let code = exit_code.clone();
                move |output| {
                    *observed.lock().unwrap() = Some((code.load(Ordering::SeqCst), output))
                }
            });
            let fixture = ValidateManifestFixture(Arc::new(move |_| {
                let answer = answer.clone();
                Box::pin(async move { answer.map_err(anyhow::Error::msg) })
            }));
            run_validation(
                Some("fixture".into()),
                exit_code,
                on_complete,
                Some(fixture),
            )
            .await;
            assert_eq!(
                *observed.lock().unwrap(),
                Some((expected_code, Some(expected_text.to_string())))
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn mounted_parent_ignores_escape_and_validation_completes_after_unmount() {
        use futures::StreamExt;
        use std::time::Duration;
        crate::utils::process_runtime::initialize_test_process_runtime();
        let (release, pending) = tokio::sync::oneshot::channel();
        let pending = Arc::new(std::sync::Mutex::new(Some(pending)));
        let (started, starting) = async_channel::bounded(1);
        let fixture = ValidateManifestFixture(Arc::new(move |_| {
            let pending = pending
                .lock()
                .unwrap()
                .take()
                .expect("one effect per callback/path");
            let started = started.clone();
            Box::pin(async move {
                started.send(()).await.unwrap();
                pending.await.unwrap()
            })
        }));
        let exit_code = Arc::new(AtomicI32::new(9));
        let (complete, completed) = async_channel::bounded(1);
        let code = exit_code.clone();
        let callback = Handler::from(move |result| {
            complete
                .try_send((code.load(Ordering::SeqCst), result))
                .unwrap();
        });
        {
            let runtime =
                crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings();
            let store = crate::state::store::AppStore::new(
                crate::state::app_state_store::AppState::default(),
                None,
            );
            let mut app = element! {
                ContextProvider(value: Context::owned(exit_code.clone())) {
                    ContextProvider(value: Context::owned(fixture)) {
                        ContextProvider(value: Context::owned(store)) {
                            ContextProvider(value: Context::owned(runtime)) {
                                FocusScope(handle_keys: false) {
                                    super::super::plugin_settings::PluginSettings(args: Some("validate pending".to_string()), on_complete: callback)
                                }
                            }
                        }
                    }
                }
            };
            let (keys, events) = async_channel::unbounded();
            let mut frames = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(80, 5),
            ));
            let first = tokio::time::timeout(Duration::from_secs(2), frames.next())
                .await
                .unwrap()
                .unwrap();
            assert!(first.to_string().contains("Running validation..."));
            tokio::time::timeout(Duration::from_secs(2), starting.recv())
                .await
                .unwrap()
                .unwrap();
            keys.send(TerminalEvent::Key(KeyEvent::new(
                KeyEventKind::Press,
                KeyCode::Esc,
            )))
            .await
            .unwrap();
            keys.send(TerminalEvent::Resize(79, 5)).await.unwrap();
            let after_escape = tokio::time::timeout(Duration::from_secs(2), frames.next())
                .await
                .unwrap()
                .unwrap();
            assert!(after_escape.to_string().contains("Running validation..."));
            assert!(completed.try_recv().is_err());
            assert_eq!(exit_code.load(Ordering::SeqCst), 9);
            drop(frames);
            drop(app);
            drop(keys);
        }
        release.send(Ok(successful_result())).unwrap();
        let (code, output) = tokio::time::timeout(Duration::from_secs(2), completed.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(code, 0);
        assert_eq!(
            output.as_deref(),
            Some("Validating plugin manifest: /fixture/plugin.json\n\n✔ Validation passed\n")
        );
    }
}
