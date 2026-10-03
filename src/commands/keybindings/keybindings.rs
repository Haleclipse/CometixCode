//! Maps to: CC `commands/keybindings/keybindings.ts`.

use crate::commands::Command;
use crate::constants::query_source::QuerySource;
use crate::utils::process_user_input::ProcessUserInputBaseResult;
use crate::utils::process_user_input::process_slash_command::{
    SlashCommandAction, SlashCommandInvocation,
};
use std::path::{Path, PathBuf};

pub const DISABLED_MESSAGE: &str =
    "Keybinding customization is not enabled. This feature is currently in preview.";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeybindingsFileResult {
    Disabled,
    WriteDisabled { path: PathBuf },
    Ready { path: PathBuf, file_exists: bool },
}

/// Maps to the mkdir + exclusive `writeFile(..., {flag:'wx'})` boundary.
pub fn prepare_keybindings_file() -> Result<KeybindingsFileResult, String> {
    if !crate::keybindings::load_user_bindings::is_keybinding_customization_enabled() {
        return Ok(KeybindingsFileResult::Disabled);
    }
    let path = crate::keybindings::load_user_bindings::get_keybindings_path();
    if !crate::utils::config::is_config_write_enabled() {
        return Ok(KeybindingsFileResult::WriteDisabled { path });
    }
    let parent = path
        .parent()
        .ok_or_else(|| format!("Invalid keybindings path: {}", path.display()))?;
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let template = crate::keybindings::template::generate_keybindings_template();
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(mut file) => {
            use std::io::Write;
            file.write_all(template.as_bytes())
                .map_err(|error| error.to_string())?;
            file.sync_all().map_err(|error| error.to_string())?;
            Ok(KeybindingsFileResult::Ready {
                path,
                file_exists: false,
            })
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            Ok(KeybindingsFileResult::Ready {
                path,
                file_exists: true,
            })
        }
        Err(error) => Err(error.to_string()),
    }
}

pub fn editor_result_message(path: &Path, file_exists: bool, editor_error: Option<&str>) -> String {
    if let Some(error) = editor_error {
        return format!(
            "{} {}. Could not open in editor: {error}",
            if file_exists { "Opened" } else { "Created" },
            path.display()
        );
    }
    if file_exists {
        format!("Opened {} in your editor.", path.display())
    } else {
        format!(
            "Created {} with template. Opened in your editor.",
            path.display()
        )
    }
}

pub fn write_disabled_message(path: &Path) -> String {
    format!(
        "Keybindings file was not opened because writes are disabled: {}. Set COMETIX_WRITE_ENABLED=1 to enable this explicit config edit.",
        path.display()
    )
}

/// Maps to: CC `commands/keybindings/keybindings.ts#call`; filesystem/editor
/// work is represented by a typed action and executed outside slash dispatch.
pub fn call(
    command: &Command,
    args: &str,
    _uuid: Option<String>,
    _context: &crate::tool::ToolUseContext,
) -> ProcessUserInputBaseResult {
    ProcessUserInputBaseResult {
        messages: Vec::new(),
        should_query: false,
        allowed_tools: None,
        local_action: Some(SlashCommandAction::EditKeybindings {
            invocation: SlashCommandInvocation::new(command.name.as_ref(), args),
        }),
        query_source: QuerySource::Prompt,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    struct RestoreEnv {
        config_dir: Option<EnvVarGuard>,
        write_enabled: Option<EnvVarGuard>,
        root: PathBuf,
    }

    impl Drop for RestoreEnv {
        fn drop(&mut self) {
            drop(self.config_dir.take());
            drop(self.write_enabled.take());
            crate::utils::config::clear_global_config_cache_for_testing();
            crate::keybindings::load_user_bindings::reset_keybinding_loader_for_testing();
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn keybindings_prepare_uses_exclusive_create_and_preserves_existing_file() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-keybindings-command-{}",
            uuid::Uuid::new_v4()
        ));
        let _restore = RestoreEnv {
            config_dir: Some(EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root)),
            write_enabled: Some(EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1")),
            root: root.clone(),
        };
        crate::utils::config::clear_global_config_cache_for_testing();

        let created = prepare_keybindings_file().unwrap();
        let KeybindingsFileResult::Ready { path, file_exists } = created else {
            panic!("expected created keybindings file");
        };
        assert!(!file_exists);
        let template = std::fs::read_to_string(&path).unwrap();
        assert!(template.contains("claude-code-keybindings.json"));
        std::fs::write(&path, "custom-content").unwrap();

        assert_eq!(
            prepare_keybindings_file().unwrap(),
            KeybindingsFileResult::Ready {
                path: path.clone(),
                file_exists: true,
            }
        );
        assert_eq!(std::fs::read_to_string(path).unwrap(), "custom-content");
    }

    #[test]
    fn aggregate_catalog_uses_official_keybindings_descriptor_and_callback() {
        let commands = crate::commands::commands();
        let command = commands
            .iter()
            .find(|command| command.name == super::super::NAME)
            .expect("/keybindings descriptor");
        assert_eq!(command.kind, crate::commands::CommandKind::Local);
        assert_eq!(command.description, super::super::DESCRIPTION);
        assert!(command.is_enabled.is_some_and(|enabled| enabled()));
        assert!(command.call.is_some());
        assert!(!command.supports_non_interactive);
    }

    #[test]
    fn keybindings_prepare_respects_explicit_no_write_mode() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-keybindings-no-write-{}",
            uuid::Uuid::new_v4()
        ));
        let _restore = RestoreEnv {
            config_dir: Some(EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root)),
            write_enabled: Some(EnvVarGuard::set("COMETIX_WRITE_ENABLED", "0")),
            root: root.clone(),
        };
        crate::utils::config::clear_global_config_cache_for_testing();

        assert_eq!(
            prepare_keybindings_file().unwrap(),
            KeybindingsFileResult::WriteDisabled {
                path: root.join("keybindings.json"),
            }
        );
        assert!(!root.exists());
    }

    #[test]
    fn keybindings_call_returns_typed_off_frame_editor_action() {
        let command = Command::local(super::super::NAME, super::super::DESCRIPTION);
        let result = call(
            &command,
            "",
            Some("keybindings-command".to_string()),
            &crate::tool::ToolUseContext::default(),
        );
        assert!(!result.should_query);
        assert!(result.messages.is_empty());
        assert_eq!(
            result.local_action,
            Some(SlashCommandAction::EditKeybindings {
                invocation: SlashCommandInvocation::new("keybindings", ""),
            })
        );
    }

    #[cfg(unix)]
    #[iocraft::component]
    fn LiveReloadEditorHarness(
        mut hooks: iocraft::Hooks,
    ) -> impl Into<iocraft::AnyElement<'static>> {
        use iocraft::prelude::*;
        let runtime = hooks
            .use_context::<crate::keybindings::keybinding_context::KeybindingRuntime>()
            .clone();
        let mut ready = hooks.use_state(|| false);
        let app = hooks.use_app();
        let editor = crate::utils::prompt_editor::ExternalEditorRuntime::new(app);
        let runtime_for_reload = runtime;
        hooks.use_future(async move {
            let path = crate::keybindings::load_user_bindings::get_keybindings_path();
            let edited = editor.edit_file(&path).await;
            if edited.error.is_none() {
                let loaded =
                    crate::keybindings::load_user_bindings::reload_keybindings_sync_with_warnings();
                runtime_for_reload.replace_bindings(loaded.bindings);
                ready.set(true);
            }
        });
        element! { Text(content: format!("ready={}", ready.get())) }
    }

    #[cfg(unix)]
    #[iocraft::component]
    fn LiveReloadDispatchHarness(
        mut hooks: iocraft::Hooks,
    ) -> impl Into<iocraft::AnyElement<'static>> {
        use iocraft::prelude::*;
        let runtime = hooks
            .use_context::<crate::keybindings::keybinding_context::KeybindingRuntime>()
            .clone();
        let mut fired = hooks.use_state(|| false);
        crate::keybindings::use_keybinding::use_keybinding(
            &mut hooks,
            Some(runtime),
            "app:redraw",
            crate::keybindings::types::ContextName::Global,
            || true,
            move || {
                fired.set(true);
                true
            },
        );
        element! { Text(content: format!("fired={}", fired.get())) }
    }

    #[cfg(unix)]
    #[test]
    fn fake_editor_reload_updates_same_runtime_and_dispatches_new_binding() {
        use futures::{StreamExt, stream};
        use iocraft::prelude::*;
        use std::os::unix::fs::PermissionsExt;
        use std::time::Duration;

        struct RemoveScript(PathBuf);
        impl Drop for RemoveScript {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }

        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-keybindings-live-reload-{}",
            uuid::Uuid::new_v4()
        ));
        let _restore = RestoreEnv {
            config_dir: Some(EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root)),
            write_enabled: Some(EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1")),
            root: root.clone(),
        };
        let script = std::env::temp_dir().join(format!(
            "cometix-keybindings-editor-{}",
            uuid::Uuid::new_v4()
        ));
        let _remove_script = RemoveScript(script.clone());
        crate::utils::config::clear_global_config_cache_for_testing();
        crate::keybindings::load_user_bindings::reset_keybinding_loader_for_testing();
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("keybindings.json"), "{\"bindings\":[]}").unwrap();
        std::fs::write(
            &script,
            r#"#!/bin/sh
cat > "$1" <<'JSON'
{"bindings":[{"context":"Global","bindings":{"f3":"app:redraw"}}]}
JSON
"#,
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&script, permissions).unwrap();
        let _visual = EnvVarGuard::set("VISUAL", &script);

        let runtime =
            crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings();
        let editor_runtime = runtime.clone();
        let ready_text = futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(editor_runtime)) {
                    LiveReloadEditorHarness
                }
            };
            let mut render_loop = Box::pin(
                app.mock_terminal_render_loop(MockTerminalConfig::default().with_size(30, 4)),
            );
            let mut last = String::new();
            for _ in 0..24 {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(200)).await;
                    None
                })
                .await;
                let Some(canvas) = next else {
                    break;
                };
                last = canvas.to_string();
                if last.contains("ready=true") {
                    break;
                }
            }
            last
        });
        assert!(ready_text.contains("ready=true"), "canvas=\n{ready_text}");

        let dispatch_runtime = runtime.clone();
        let fired_text = futures::executor::block_on(async move {
            let events = stream::iter(vec![TerminalEvent::Key(KeyEvent::new(
                KeyEventKind::Press,
                KeyCode::F(3),
            ))]);
            let mut app = element! {
                ContextProvider(value: Context::owned(dispatch_runtime)) {
                    LiveReloadDispatchHarness
                }
            };
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(30, 4),
            ));
            let mut last = String::new();
            for _ in 0..12 {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(100)).await;
                    None
                })
                .await;
                let Some(canvas) = next else {
                    break;
                };
                last = canvas.to_string();
                if last.contains("fired=true") {
                    break;
                }
            }
            last
        });
        assert!(
            fired_text.contains("fired=true"),
            "edited binding did not dispatch through the same runtime; canvas=\n{fired_text}"
        );
    }

    #[test]
    fn keybindings_messages_match_official_created_opened_and_editor_error_copy() {
        let path = Path::new("/tmp/keybindings.json");
        assert_eq!(
            editor_result_message(path, false, None),
            "Created /tmp/keybindings.json with template. Opened in your editor."
        );
        assert_eq!(
            editor_result_message(path, true, None),
            "Opened /tmp/keybindings.json in your editor."
        );
        assert_eq!(
            editor_result_message(path, true, Some("editor failed")),
            "Opened /tmp/keybindings.json. Could not open in editor: editor failed"
        );
    }
}
