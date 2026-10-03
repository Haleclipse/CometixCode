//! Maps to: CC `components/SessionBackgroundHint.tsx`.
//!
//! The normal-prompt sibling owns the foreground-task binding. The source
//! session-background feature is compiled out; its hint remains hidden in the
//! live caller, while the existing display props support rendering tests.

use crate::components::design_system::keyboard_shortcut_hint::KeyboardShortcutHint;
use iocraft::prelude::*;

#[derive(Default, Props)]
pub struct SessionBackgroundHintProps {
    pub is_loading: bool,
    pub show_session_hint: bool,
    pub base_shortcut: Option<String>,
    pub terminal: Option<String>,
}

/// Maps to: CC `SessionBackgroundHint.tsx` tmux shortcut branch.
pub fn session_background_shortcut(base_shortcut: &str, terminal: Option<&str>) -> String {
    if terminal == Some("tmux") && base_shortcut == "ctrl+b" {
        "ctrl+b ctrl+b".to_string()
    } else {
        base_shortcut.to_string()
    }
}

/// Maps to: CC `SessionBackgroundHint.tsx` final visibility guard.
pub fn session_background_hint_should_render(is_loading: bool, show_session_hint: bool) -> bool {
    is_loading && show_session_hint
}

/// Maps to: CC `components/SessionBackgroundHint.tsx#SessionBackgroundHint`.
#[component]
pub fn SessionBackgroundHint(
    props: &SessionBackgroundHintProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let store = crate::state::app_state::use_app_state_store(&mut hooks);
    let has_foreground = crate::state::app_state::use_app_state(
        &mut hooks,
        crate::tasks::local_shell_task::has_foreground_tasks,
    );
    let runtime = hooks
        .try_use_context::<crate::keybindings::keybinding_context::KeybindingRuntime>()
        .map(|runtime| runtime.clone());
    // CC SessionBackgroundHint.tsx:50-80: registration follows foreground
    // state, while the callback rechecks current state and the disable flag.
    crate::keybindings::use_keybinding::use_keybinding(
        &mut hooks,
        runtime,
        "task:background",
        crate::keybindings::types::ContextName::Task,
        move || has_foreground,
        move || {
            if crate::utils::env_utils::is_env_truthy(
                crate::utils::process_env::var("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS")
                    .as_deref(),
            ) {
                return true;
            }
            if crate::tasks::local_shell_task::has_foreground_tasks(&store.get()) {
                crate::tasks::local_shell_task::background_all(&store);
                if crate::utils::config::load_global_config().has_used_background_task != Some(true)
                {
                    let _ = crate::utils::config::save_global_config(|config| {
                        config.has_used_background_task = Some(true);
                    });
                }
            }
            true
        },
    );
    if !session_background_hint_should_render(props.is_loading, props.show_session_hint) {
        return element! { View(width: 0u32, height: 0u32) }.into_any();
    }

    let shortcut = session_background_shortcut(
        props.base_shortcut.as_deref().unwrap_or("ctrl+b"),
        props.terminal.as_deref(),
    );

    element! {
        View(padding_left: 2u32) {
            KeyboardShortcutHint(shortcut: shortcut, action: "background".to_string())
        }
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};
    use futures::{StreamExt, stream};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[derive(Default, Props)]
    struct BackgroundInputProbeProps {
        parent_owner: bool,
        snapshots: Option<Arc<Mutex<Vec<(String, usize)>>>>,
    }

    #[component]
    fn BackgroundInputProbe(
        props: &BackgroundInputProbeProps,
        mut hooks: Hooks,
    ) -> impl Into<AnyElement<'static>> {
        let value = hooks.use_state(|| "abc".to_string());
        let cursor = hooks.use_state(|| 3usize);
        let store = crate::state::app_state::use_app_state_store(&mut hooks);
        let foreground = crate::state::app_state::use_app_state(
            &mut hooks,
            crate::tasks::local_shell_task::has_foreground_tasks,
        );
        let runtime = hooks
            .use_context::<crate::keybindings::keybinding_context::KeybindingRuntime>()
            .clone();
        let parent_owner = props.parent_owner;
        let snapshots = props.snapshots.clone().unwrap();
        let store_for_background = store.clone();
        // Exercise the stronger child-input -> parent-handler topology too.
        // The callback observes the cursor after the source editing branch.
        crate::keybindings::use_keybinding::use_keybinding(
            &mut hooks,
            Some(runtime),
            "task:background",
            crate::keybindings::types::ContextName::Task,
            move || parent_owner && foreground,
            move || {
                snapshots
                    .lock()
                    .unwrap()
                    .push((value.read().clone(), cursor.get()));
                crate::tasks::local_shell_task::background_all(&store_for_background);
                true
            },
        );
        let state = store.get();
        element! {
            View(flex_direction: FlexDirection::Column) {
                crate::components::text_input::TextInput(
                    value: Some(value), cursor_offset: Some(cursor),
                    focus: true, multiline: true, columns: 80usize,
                )
                #(if !parent_owner {
                    Some(element! { SessionBackgroundHint(is_loading: true) }.into_any())
                } else { None })
                Text(content: format!("value={:?};cursor={};foreground={}", value.read().as_str(), cursor.get(), crate::tasks::local_shell_task::has_foreground_tasks(&state)))
            }
        }
    }

    struct TestShell(
        crate::utils::shell_command::ShellCommand,
        Option<std::process::ChildStdin>,
    );
    impl Drop for TestShell {
        fn drop(&mut self) {
            self.1.take();
            if self.0.wait_result_timeout(Duration::ZERO).is_none() {
                self.0.kill();
                let _ = self.0.wait_result_timeout(Duration::from_secs(2));
            }
            self.0.cleanup();
        }
    }

    #[test]
    fn session_background_matches_official_child_input_and_later_owner() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let _disabled = EnvVarGuard::unset("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS");
        // No configuration write is needed to prove this input path.
        let mut config = crate::utils::config::load_global_config();
        config.has_used_background_task = Some(true);
        let previous_config = crate::utils::config::replace_test_global_config(Some(config));
        for parent_owner in [true, false] {
            for foreground in [false, true] {
                let store = crate::state::store::AppStore::new(
                    crate::state::app_state_store::AppState::default(),
                    None,
                );
                let mut shell = foreground.then(|| {
                    use std::process::{Command, Stdio};
                    let task_id = crate::task::generate_task_id(crate::task::TaskType::LocalBash);
                    let output = crate::utils::task::task_output::TaskOutput::new(&task_id, false);
                    let command = "IFS= read -r marker && printf '%s' \"$marker\"";
                    let mut process = Command::new("/bin/sh");
                    process
                        .args(["-c", command])
                        .stdin(Stdio::piped())
                        .stdout(Stdio::piped())
                        .stderr(Stdio::piped());
                    #[cfg(unix)]
                    {
                        use std::os::unix::process::CommandExt as _;
                        process.process_group(0);
                    }
                    let mut child = process.spawn().unwrap();
                    let stdin = child.stdin.take().unwrap();
                    let shell = crate::utils::shell_command::wrap_spawn(
                        child,
                        crate::tool::AbortController::default(),
                        Duration::from_secs(30),
                        output,
                        true,
                        None,
                    );
                    crate::tasks::local_shell_task::register_foreground(
                        crate::tasks::local_shell_task::LocalShellSpawnInput {
                            command: command.into(),
                            description: "Ctrl+B input probe".into(),
                            shell_command: shell.clone(),
                            tool_use_id: None,
                            agent_id: None,
                            kind: None,
                        },
                        &store,
                    );
                    TestShell(shell, Some(stdin))
                });
                let snapshots = Arc::new(Mutex::new(Vec::new()));
                let captured = Arc::clone(&snapshots);
                let store_for_tree = store.clone();
                let frames = futures::executor::block_on(async move {
                    let mut app = element! {
                        ContextProvider(value: Context::owned(crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings())) {
                            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                                crate::state::app_state::AppStateProvider(
                                    prebuilt_store: Some(store_for_tree),
                                    children: crate::state::app_state::ProviderChildren::new(move || element! {
                                        BackgroundInputProbe(parent_owner: parent_owner, snapshots: Some(Arc::clone(&captured)))
                                    }.into_any()),
                                )
                            }
                        }
                    };
                    let mut ctrl_b = KeyEvent::new(KeyEventKind::Press, KeyCode::Char('b'));
                    ctrl_b.modifiers = KeyModifiers::CONTROL;
                    let events = stream::unfold(
                        vec![
                            TerminalEvent::Key(ctrl_b),
                            TerminalEvent::Key(KeyEvent::new(
                                KeyEventKind::Press,
                                KeyCode::Char(' '),
                            )),
                            TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Left)),
                            TerminalEvent::Key(KeyEvent::new(
                                KeyEventKind::Press,
                                KeyCode::Char('x'),
                            )),
                        ]
                        .into_iter(),
                        |mut events| async move {
                            let event = events.next()?;
                            futures_timer::Delay::new(Duration::from_millis(30)).await;
                            Some((event, events))
                        },
                    );
                    let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                        MockTerminalConfig::with_events(events).with_size(90, 10),
                    ));
                    let mut frames = Vec::new();
                    while let Some(canvas) = crate::utils::race(render_loop.next(), async {
                        futures_timer::Delay::new(Duration::from_millis(150)).await;
                        None
                    })
                    .await
                    {
                        frames.push(canvas.to_string());
                    }
                    frames
                });
                let last = frames.last().unwrap();
                assert!(
                    last.contains("value=\"abx c\";cursor=3;foreground=false"),
                    "parent={parent_owner}, foreground={foreground}: {frames:?}"
                );
                if parent_owner && foreground {
                    assert_eq!(
                        snapshots.lock().unwrap().as_slice(),
                        [("abc".to_string(), 2)]
                    );
                } else {
                    assert!(snapshots.lock().unwrap().is_empty());
                }
                if let Some(shell) = &mut shell {
                    use std::io::Write;
                    assert!(
                        shell.0.was_backgrounded_by_user(),
                        "registered task did not background through input"
                    );
                    assert!(
                        shell.0.wait_result_timeout(Duration::ZERO).is_none(),
                        "backgrounded shell must remain alive awaiting input"
                    );
                    let mut stdin = shell.1.take().unwrap();
                    let marker = format!(
                        "ctrl-b-completed{}",
                        ".".repeat(1024 - "ctrl-b-completed".len())
                    );
                    writeln!(stdin, "{marker}").unwrap();
                    drop(stdin);
                    let result = shell
                        .0
                        .wait_result_timeout(Duration::from_secs(2))
                        .expect("backgrounded shell completes after stdin handshake");
                    assert_eq!(result.code, 0);
                    assert!(!result.interrupted);
                    // CC ShellCommand.ts:349-361 forces pipe output to disk.
                    // Assert raw completion here: TaskOutput's unterminated-line
                    // tail projection is a separate, still partial owner.
                    let output_path = shell.0.task_output().path();
                    assert_eq!(std::fs::read_to_string(output_path).unwrap(), marker);
                }
                drop(shell);
            }
        }
        crate::utils::config::replace_test_global_config(previous_config);
    }

    #[test]
    fn session_background_shortcut_matches_tmux_prefix_branch() {
        assert_eq!(
            session_background_shortcut("ctrl+b", Some("tmux")),
            "ctrl+b ctrl+b"
        );
        assert_eq!(
            session_background_shortcut("ctrl+b", Some("kitty")),
            "ctrl+b"
        );
        assert_eq!(session_background_shortcut("cmd+b", Some("tmux")), "cmd+b");
    }

    #[test]
    fn session_background_uses_only_the_provided_task_snapshot() {
        use crate::tasks::local_agent_task as agents;
        use crate::tasks::local_shell_task::{background_all, has_foreground_tasks};
        use crate::tools::agent_tool::load_agents_dir::{AgentDefinition, AgentDefinitionSource};
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _agent_lock = agents::TEST_LOCAL_AGENT_TASK_LOCK.lock().unwrap();
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        let outside = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        let register = |owner: &crate::state::store::AppStore| {
            agents::register_agent_foreground_with_store(
                agents::RegisterAgentForegroundParams {
                    agent_id: format!("ctrl-b-snapshot-{}", uuid::Uuid::new_v4()),
                    description: "snapshot isolation".into(),
                    prompt: "input probe".into(),
                    selected_agent: AgentDefinition::new(
                        "general-purpose",
                        "input probe",
                        AgentDefinitionSource::BuiltIn,
                    ),
                    auto_background_ms: None,
                    tool_use_id: None,
                },
                Some(owner.clone()),
            )
            .task_id
        };
        struct TestAgents(Vec<String>);
        impl Drop for TestAgents {
            fn drop(&mut self) {
                for id in &self.0 {
                    agents::unregister_agent_foreground(id);
                    agents::remove_local_agent_task_entry(id);
                }
            }
        }
        let mut owned = TestAgents(Vec::new());
        let foreign_id = register(&outside);
        owned.0.push(foreign_id.clone());
        assert!(!has_foreground_tasks(&store.get()));
        assert!(!background_all(&store));

        let removed_id = register(&store);
        owned.0.push(removed_id.clone());
        assert!(has_foreground_tasks(&store.get()));
        crate::utils::task::framework::remove_task(&removed_id, &store);
        assert!(!has_foreground_tasks(&store.get()));
        assert!(!background_all(&store));
        assert!(!store.get().tasks.contains_key(&removed_id));

        let current_id = register(&store);
        owned.0.push(current_id.clone());
        assert!(background_all(&store));
        assert!(!has_foreground_tasks(&store.get()));
        assert!(
            agents::get_local_agent_task(&current_id)
                .unwrap()
                .is_backgrounded
        );
        for id in [&foreign_id, &removed_id] {
            assert!(!agents::get_local_agent_task(id).unwrap().is_backgrounded);
        }

        // JS !undefined is true. A missing richer registry entry does not
        // make an AppState member ineligible; the registry only supplies type.
        agents::remove_local_agent_task_entry(&current_id);
        store.replace_with(|state| {
            let task = Arc::make_mut(&mut state.tasks)
                .get_mut(&current_id)
                .unwrap();
            if let crate::state::app_state_store::TaskState::Other(agent) = Arc::make_mut(task) {
                agent.is_backgrounded = None;
            }
        });
        assert!(has_foreground_tasks(&store.get()));
    }

    #[test]
    fn session_background_hint_visibility_matches_official_guard() {
        assert!(session_background_hint_should_render(true, true));
        assert!(!session_background_hint_should_render(false, true));
        assert!(!session_background_hint_should_render(true, false));
    }

    #[test]
    fn session_background_hint_renders_keyboard_hint() {
        let text = element! {
            ContextProvider(value: Context::owned(crate::state::store::AppStore::new(
                crate::state::app_state_store::AppState::default(), None,
            ))) {
            SessionBackgroundHint(
                is_loading: true,
                show_session_hint: true,
                base_shortcut: Some("ctrl+b".to_string()),
                terminal: Some("tmux".to_string()),
            )
            }
        }
        .render(Some(80))
        .to_string();

        assert!(
            text.contains("ctrl+b ctrl+b to background"),
            "canvas=\n{text}"
        );
    }

    #[test]
    fn session_background_hint_hidden_renders_empty() {
        let text = element! {
            ContextProvider(value: Context::owned(crate::state::store::AppStore::new(
                crate::state::app_state_store::AppState::default(), None,
            ))) {
            SessionBackgroundHint(is_loading: false, show_session_hint: true)
            }
        }
        .render(Some(80))
        .to_string();
        assert_eq!(text, "");
    }
}
