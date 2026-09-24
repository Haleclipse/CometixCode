//! External-editor terminal handoff.
//!
//! Maps to: CC `utils/promptEditor.ts` and `utils/editor.ts`.
//! Framework side: `AppHandle::suspend_terminal` releases raw mode, runs a
//! synchronous closure, then restores modes and force-repaints. Application
//! side (this module): resolve `$VISUAL`/`$EDITOR` without a shell, write
//! prompt temp files, launch the editor inside the handoff closure, and read
//! the result back after the user-triggered child exits.

use iocraft::hooks::AppHandle;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EditorResult {
    pub content: Option<String>,
    pub error: Option<String>,
}

#[derive(Clone, Copy)]
pub struct ExternalEditorRuntime {
    app: AppHandle,
}

impl ExternalEditorRuntime {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }

    pub async fn edit_file(&self, file_path: &Path) -> EditorResult {
        if !file_path.is_file() {
            return EditorResult::default();
        }
        let Some((program, mut args)) = external_editor_command() else {
            return EditorResult::default();
        };
        args.push(file_path.to_string_lossy().into_owned());

        // Framework owns terminal handoff; this layer only resolves the editor
        // argv and launches it. The closure must be synchronous: the render
        // loop blocks until it returns, then reacquires raw mode and repaints.
        let mut app = self.app;
        let program_for_err = program.clone();
        let receiver =
            app.suspend_terminal(move || std::process::Command::new(&program).args(&args).status());

        let status = match receiver.await {
            Ok(Ok(Ok(status))) => status,
            Ok(Ok(Err(_))) => {
                return EditorResult {
                    content: None,
                    error: Some(format!(
                        "Failed to launch {}",
                        editor_display_name(&program_for_err)
                    )),
                };
            }
            Ok(Err(error)) => {
                // release_terminal failed; the closure never ran.
                return EditorResult {
                    content: None,
                    error: Some(format!("Failed to release terminal: {error}")),
                };
            }
            Err(_) => {
                // oneshot dropped (render loop exited mid-handoff).
                return EditorResult {
                    content: None,
                    error: Some("External editor handoff was interrupted".to_string()),
                };
            }
        };

        if !status.success() {
            return EditorResult {
                content: None,
                error: Some(format!(
                    "{} exited with code {}",
                    editor_display_name(&program_for_err),
                    status.code().unwrap_or_default()
                )),
            };
        }
        match std::fs::read_to_string(file_path) {
            Ok(content) => EditorResult {
                content: Some(content),
                error: None,
            },
            Err(error) => EditorResult {
                content: None,
                error: Some(error.to_string()),
            },
        }
    }

    pub async fn edit_prompt(&self, current_prompt: &str) -> EditorResult {
        let path = unique_prompt_path();
        let write_result = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .and_then(|mut file| {
                use std::io::Write;
                file.write_all(current_prompt.as_bytes())?;
                file.sync_all()
            });
        if let Err(error) = write_result {
            return EditorResult {
                content: None,
                error: Some(error.to_string()),
            };
        }

        let mut result = self.edit_file(&path).await;
        let _ = std::fs::remove_file(&path);
        if let Some(content) = result.content.as_mut() {
            // Editors commonly add one final newline. Preserve intentional
            // blank lines, matching the official helper.
            if content.ends_with('\n') && !content.ends_with("\n\n") {
                content.pop();
                if content.ends_with('\r') {
                    content.pop();
                }
            }
        }
        result
    }
}

fn unique_prompt_path() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!("cometix-prompt-{}-{nanos}.md", std::process::id()))
}

fn editor_display_name(program: &str) -> String {
    Path::new(program)
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or(program)
        .to_string()
}

fn command_exists(command: &str) -> bool {
    let candidate = Path::new(command);
    if candidate.components().count() > 1 {
        return candidate.is_file();
    }
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|directory| directory.join(command).is_file())
    })
}

/// Resolves `$VISUAL`, `$EDITOR`, then the official `code`, `vi`, `nano`
/// fallback order. The returned argv is executed directly, never by a shell.
///
/// Maps to: CC `utils/editor.ts:164` `getExternalEditor = memoize(...)` —
/// resolved once per process. The fallback probe stats every `PATH`
/// directory, and `PromptInput` asks on every render (to decide whether the
/// external-editor keybinding is live); unmemoized, that probe was about a
/// fifth of each keystroke's update. Tests that vary `$VISUAL`/`$EDITOR`
/// rely on nextest's per-test process, exactly as CC's tests do on a fresh
/// module instance.
pub fn external_editor_command() -> Option<(String, Vec<String>)> {
    static RESOLVED: std::sync::OnceLock<Option<(String, Vec<String>)>> =
        std::sync::OnceLock::new();
    RESOLVED.get_or_init(resolve_external_editor_command).clone()
}

fn resolve_external_editor_command() -> Option<(String, Vec<String>)> {
    let configured = std::env::var("VISUAL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            std::env::var("EDITOR")
                .ok()
                .filter(|value| !value.trim().is_empty())
        });
    let editor = configured.or_else(|| {
        ["code", "vi", "nano"]
            .into_iter()
            .find(|candidate| command_exists(candidate))
            .map(str::to_string)
    })?;

    // This mirrors CC's simple whitespace split. Direct argv execution avoids
    // command substitution and other shell expansion from file paths.
    let mut parts = editor
        .split_ascii_whitespace()
        .map(str::to_string)
        .collect::<Vec<_>>();
    if parts.is_empty() {
        return None;
    }
    let program = parts.remove(0);
    if parts.is_empty() {
        match editor.as_str() {
            "code" => parts.push("-w".to_string()),
            "subl" => parts.push("--wait".to_string()),
            _ => {}
        }
    }
    Some((program, parts))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_display_name_uses_executable_basename() {
        assert_eq!(editor_display_name("/usr/local/bin/code"), "code");
        assert_eq!(editor_display_name("nvim"), "nvim");
    }

    #[test]
    fn unique_prompt_paths_do_not_alias() {
        let one = unique_prompt_path();
        std::thread::sleep(std::time::Duration::from_micros(1));
        let two = unique_prompt_path();
        assert_ne!(one, two);
    }

    #[cfg(unix)]
    #[iocraft::component]
    fn PromptEditorHarness(mut hooks: iocraft::Hooks) -> impl Into<iocraft::AnyElement<'static>> {
        use iocraft::prelude::*;
        let mut app = hooks.use_app();
        let runtime = ExternalEditorRuntime::new(app);
        let mut result = hooks.use_state(|| "pending".to_string());
        let display = result.read().clone();
        hooks.use_future(async move {
            let edited = runtime.edit_prompt("original").await;
            result.set(
                edited
                    .content
                    .unwrap_or_else(|| edited.error.unwrap_or_else(|| "no content".to_string())),
            );
            app.exit();
        });
        element!(Text(content: display))
    }

    #[cfg(unix)]
    #[test]
    fn raw_mode_safe_handoff_reads_back_explicit_editor_changes() {
        use futures::StreamExt;
        use iocraft::prelude::*;
        use std::os::unix::fs::PermissionsExt;

        let _guard = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let script = std::env::temp_dir().join(format!(
            "cometix-fake-editor-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::write(&script, "#!/bin/sh\nprintf 'edited by child\\n' > \"$1\"\n")
            .expect("write fake editor");
        let mut permissions = std::fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&script, permissions).unwrap();
        let _visual = crate::utils::env_utils::EnvVarGuard::set("VISUAL", &script);

        let canvases = futures::executor::block_on(
            element!(PromptEditorHarness)
                .mock_terminal_render_loop(MockTerminalConfig::default())
                .collect::<Vec<_>>(),
        );

        let _ = std::fs::remove_file(script);
        assert!(
            canvases
                .iter()
                .any(|canvas| canvas.to_string().contains("edited by child"))
        );
    }
}
