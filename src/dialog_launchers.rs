//! Maps to: CC `dialogLaunchers.tsx`.
//!
//! Dialog launch functions remain separate from `repl_launcher` and own their
//! concrete `App → dialog` mount, matching the official launcher boundary.

use crate::components::app::{App, AppChildren};
use crate::screens::repl::ReplProps;
use crate::screens::resume_conversation::{ResumeConversation, ResumeFilterByPr};
use crate::state::store::AppStore;
use crate::utils::settings::SettingsWithErrors;
use iocraft::prelude::*;
use std::sync::Arc;

/// Maps to: CC `dialogLaunchers.tsx:170-201` `launchResumeChooser`.
///
/// CC awaits `worktreePathsPromise` together with dynamic imports. Rust's
/// retained `main` resolves the same worktree list after setup and passes it
/// here before this function mounts `App → ResumeConversation`.
pub fn launch_resume_chooser(
    app_store: AppStore,
    startup_settings: Arc<SettingsWithErrors>,
    worktree_paths: Vec<String>,
    initial_search_query: Option<String>,
    filter_by_pr: Option<ResumeFilterByPr>,
    repl_props: ReplProps,
) -> AnyElement<'static> {
    // Maps to: CC `dialogLaunchers.tsx:191-197` — `<App><KeybindingSetup>
    // <ResumeConversation/>`. The REPL the picker opens mounts its own
    // KeybindingSetup inside this one, as CC's does.
    let children = AppChildren::new(move || {
        element! {
            crate::keybindings::keybinding_provider_setup::KeybindingSetup {
                ResumeConversation(
                    repl_props: repl_props.clone(),
                    worktree_paths: worktree_paths.clone(),
                    initial_search_query: initial_search_query.clone(),
                    filter_by_pr: filter_by_pr.clone(),
                )
            }
        }
        .into_any()
    });
    // Adopting the prebuilt store here is Contract B clause 1, not clause 6.
    // CC `main.tsx:5090-5118` runs `launchRepl` and `launchResumeChooser` as
    // MUTUALLY EXCLUSIVE branches, each passing one `initialState` and each
    // mounting exactly one `<App>` → one `AppStateProvider` → one store; and
    // `ResumeConversation.tsx:361-380` renders `<REPL/>` inside the picker's
    // own provider tree, so picker→REPL is single-store. CC therefore has one
    // main store per process, created by whichever branch mounts — which the
    // retained launch phase prebuilds and this branch adopts.
    //
    // (Clause 6 previously listed this call site as an auxiliary root that
    // should create its own store from `initialState`. That was written under
    // the two-store reading the 2nd addendum refuted, and is retracted: a
    // fresh store here would lack every launch-phase seed.)
    element! {
        App(
            app_store: Some(app_store),
            startup_settings: startup_settings,
            children: children,
        )
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::TEST_ENV_LOCK;
    use futures::{StreamExt, stream};
    use std::fs;
    use std::time::Duration;

    #[test]
    fn launch_resume_chooser_survives_loading_to_non_empty_selector_and_cancel() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let previous_cwd = std::env::current_dir().unwrap();
        let previous_original_cwd = crate::bootstrap::state::get_original_cwd();
        let root = std::env::temp_dir().join(format!(
            "cometix-dialog-launcher-nonempty-{}",
            uuid::Uuid::new_v4()
        ));
        let requested_cwd = root.join("repo");
        let projects = root.join("projects");
        fs::create_dir_all(&requested_cwd).unwrap();
        let _projects_guard =
            crate::utils::session_storage::set_test_projects_dir_override(&projects);
        std::env::set_current_dir(&requested_cwd).unwrap();
        // macOS canonicalizes /var to /private/var; session-directory keys must
        // use the same cwd spelling that production `current_dir()` returns.
        let cwd = std::env::current_dir().unwrap();
        crate::bootstrap::state::set_original_cwd(&cwd);
        crate::utils::process_env::set("CLAUBBIT", "1");
        crate::utils::config::set_test_global_config(Some(
            crate::utils::config::GlobalConfig::default(),
        ));

        let session_id = "11111111-2222-4333-8444-555555555555";
        let session_file = crate::utils::session_storage::get_session_file_path(
            &cwd.display().to_string(),
            session_id,
        );
        fs::create_dir_all(session_file.parent().unwrap()).unwrap();
        fs::write(
            &session_file,
            format!(
                "{}\n",
                serde_json::json!({
                    "type": "user",
                    "uuid": "resume-picker-user",
                    "sessionId": session_id,
                    "cwd": cwd.display().to_string(),
                    "timestamp": "2026-07-12T00:00:00.000Z",
                    "message": {
                        "role": "user",
                        "content": "non-empty resume selector fixture"
                    }
                })
            ),
        )
        .unwrap();
        assert_eq!(
            crate::utils::session_storage::list_sessions(&cwd.display().to_string()).len(),
            1,
            "fixture must be discoverable at {}",
            session_file.display()
        );
        let settings = Arc::new(SettingsWithErrors {
            settings: crate::utils::settings::SettingsJson::default(),
            errors: Vec::new(),
            policy_settings: None,
        });
        let initial = crate::main::build_initial_app_state(
            &settings.settings,
            &settings.errors,
            true,
            &crate::cli::CliConfig::default(),
        )
        .unwrap();
        let launch = launch_resume_chooser(
            AppStore::new(initial, None),
            settings,
            Vec::new(),
            None,
            None,
            ReplProps::default(),
        );
        let keybinding_runtime =
            crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings();
        let current_theme = *crate::utils::theme::current();
        let mut app = element! {
            ContextProvider(value: Context::owned(keybinding_runtime)) {
                ContextProvider(value: Context::owned(current_theme)) {
                    #(launch)
                }
            }
        };
        let events = stream::once(async {
            futures_timer::Delay::new(Duration::from_millis(200)).await;
            TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Esc))
        });
        let mut render_loop =
            Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(100, 30),
            ));
        let frames = futures::executor::block_on(async {
            let mut frames = Vec::new();
            for _ in 0..24 {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(500)).await;
                    None
                })
                .await;
                let Some(canvas) = next else { break };
                frames.push(canvas.to_string());
            }
            frames
        });
        let text = frames.join("\n--- frame ---\n");
        assert!(
            text.contains("non-empty resume selector fixture"),
            "loading must reach a non-empty selector before Esc cancels; canvases=\n{text}"
        );

        std::env::set_current_dir(previous_cwd).unwrap();
        crate::bootstrap::state::set_original_cwd(previous_original_cwd);
        let _ = fs::remove_dir_all(root);
        crate::utils::config::set_test_global_config(None);
        crate::utils::process_env::remove("CLAUBBIT");
    }

    #[test]
    fn launch_resume_chooser_loads_more_than_the_initial_fifty_near_list_end() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let previous_cwd = std::env::current_dir().unwrap();
        let previous_original_cwd = crate::bootstrap::state::get_original_cwd();
        let root = std::env::temp_dir().join(format!(
            "cometix-dialog-launcher-progressive-{}",
            uuid::Uuid::new_v4()
        ));
        let requested_cwd = root.join("repo");
        fs::create_dir_all(&requested_cwd).unwrap();
        let _projects_guard =
            crate::utils::session_storage::set_test_projects_dir_override(root.join("projects"));
        std::env::set_current_dir(&requested_cwd).unwrap();
        let cwd = std::env::current_dir().unwrap();
        crate::bootstrap::state::set_original_cwd(&cwd);
        crate::utils::process_env::set("CLAUBBIT", "1");
        crate::utils::config::set_test_global_config(Some(
            crate::utils::config::GlobalConfig::default(),
        ));
        let project_path = cwd.display().to_string();
        for index in 0..65 {
            let session_id = format!("progressive-dialog-{index:03}");
            let path =
                crate::utils::session_storage::get_session_file_path(&project_path, &session_id);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(
                path,
                format!(
                    "{}\n",
                    serde_json::json!({
                        "type":"user",
                        "uuid":format!("progressive-message-{index:03}"),
                        "parentUuid":null,
                        "sessionId":session_id,
                        "cwd":project_path,
                        "timestamp":"2026-07-12T00:00:00.000Z",
                        "message":{"role":"user","content":format!("progressive prompt {index}")}
                    })
                ),
            )
            .unwrap();
        }

        let settings = Arc::new(SettingsWithErrors {
            settings: crate::utils::settings::SettingsJson::default(),
            errors: Vec::new(),
            policy_settings: None,
        });
        let initial = crate::main::build_initial_app_state(
            &settings.settings,
            &settings.errors,
            true,
            &crate::cli::CliConfig::default(),
        )
        .unwrap();
        let launch = launch_resume_chooser(
            AppStore::new(initial, None),
            settings,
            vec![project_path],
            None,
            None,
            ReplProps::default(),
        );
        let current_theme = *crate::utils::theme::current();
        let events = stream::unfold(
            vec![(KeyCode::End, 250u64), (KeyCode::Esc, 500u64)].into_iter(),
            |mut events| async move {
                let (code, delay_ms) = events.next()?;
                futures_timer::Delay::new(Duration::from_millis(delay_ms)).await;
                Some((
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code)),
                    events,
                ))
            },
        );
        let mut app = element! {
            ContextProvider(value: Context::owned(
                crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings()
            )) {
                ContextProvider(value: Context::owned(current_theme)) {
                    #(launch)
                }
            }
        };
        let mut render_loop =
            Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(100, 30),
            ));
        let frames = futures::executor::block_on(async {
            let mut frames = Vec::new();
            for _ in 0..48 {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(250)).await;
                    None
                })
                .await;
                let Some(canvas) = next else { break };
                frames.push(canvas.to_string());
            }
            frames
        });
        let text = frames.join("\n--- frame ---\n");
        assert!(
            text.contains("of 65)"),
            "End must trigger onLoadMore and append the remaining stat logs; frames=\n{text}"
        );

        std::env::set_current_dir(previous_cwd).unwrap();
        crate::bootstrap::state::set_original_cwd(previous_original_cwd);
        crate::utils::config::set_test_global_config(None);
        crate::utils::process_env::remove("CLAUBBIT");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn launch_resume_chooser_mounts_resume_conversation_before_repl_like_official() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let previous_cwd = std::env::current_dir().unwrap();
        let previous_original_cwd = crate::bootstrap::state::get_original_cwd();
        let cwd =
            std::env::temp_dir().join(format!("cometix-dialog-launcher-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&cwd).unwrap();
        std::env::set_current_dir(&cwd).unwrap();
        let cwd = std::env::current_dir().unwrap();
        crate::bootstrap::state::set_original_cwd(&cwd);
        let _projects_guard =
            crate::utils::session_storage::set_test_projects_dir_override(cwd.join("projects"));
        crate::utils::process_env::set("CLAUBBIT", "1");
        crate::utils::config::set_test_global_config(Some(
            crate::utils::config::GlobalConfig::default(),
        ));

        let settings = Arc::new(SettingsWithErrors {
            settings: crate::utils::settings::SettingsJson::default(),
            errors: Vec::new(),
            policy_settings: None,
        });
        let initial = crate::main::build_initial_app_state(
            &settings.settings,
            &settings.errors,
            true,
            &crate::cli::CliConfig::default(),
        )
        .unwrap();
        let launch = launch_resume_chooser(
            AppStore::new(initial, None),
            settings,
            Vec::new(),
            None,
            None,
            ReplProps::default(),
        );
        let snapshot = crate::interactive_helpers::SetupScreensSnapshot {
            show_onboarding: false,
            show_claude_in_chrome_onboarding: false,
            ..crate::interactive_helpers::SetupScreensSnapshot::default()
        };
        let keybinding_runtime =
            crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings();
        let current_theme = *crate::utils::theme::current();
        let mut app = element! {
            ContextProvider(value: Context::owned(snapshot)) {
                ContextProvider(value: Context::owned(keybinding_runtime)) {
                    ContextProvider(value: Context::owned(current_theme)) {
                        #(launch)
                    }
                }
            }
        };
        let mut render_loop = Box::pin(app.mock_terminal_render_loop(
            MockTerminalConfig::with_events(stream::empty()).with_size(100, 30),
        ));
        let text = futures::executor::block_on(async {
            let mut last = String::new();
            for _ in 0..10 {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(250)).await;
                    None
                })
                .await;
                let Some(canvas) = next else { break };
                last = canvas.to_string();
                if last.contains("No conversations found to resume.") {
                    break;
                }
            }
            last
        });

        assert!(
            text.contains("No conversations found to resume."),
            "canvas=\n{text}"
        );
        assert!(
            !text.contains("Cometix Code"),
            "REPL mounted early:\n{text}"
        );

        std::env::set_current_dir(previous_cwd).unwrap();
        crate::bootstrap::state::set_original_cwd(previous_original_cwd);
        let _ = fs::remove_dir_all(cwd);
        crate::utils::config::set_test_global_config(None);
        crate::utils::process_env::remove("CLAUBBIT");
    }
}
