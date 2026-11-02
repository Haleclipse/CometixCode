//! Maps to: CC `components/DesktopHandoff.tsx`.
//!
//! Safety boundary: official code checks desktop install status, opens the
//! browser for downloads, flushes session storage, opens a desktop deep-link,
//! and calls `gracefulShutdown(0, 'other')` after success. This Rust component
//! is intentionally snapshot-driven: it renders the official states and emits
//! official callback results only. It does not probe apps, open URLs, flush
//! transcripts, open deep links, or terminate the process.

use crate::components::design_system::loading_state::LoadingState;
use crate::utils::worktree::CommandResultDisplay;
use iocraft::prelude::*;

pub const DESKTOP_DOCS_URL: &str = "https://clau.de/desktop";
pub const DESKTOP_DOWNLOAD_WIN32_X64: &str =
    "https://claude.ai/api/desktop/win32/x64/exe/latest/redirect";
pub const DESKTOP_DOWNLOAD_DARWIN_UNIVERSAL: &str =
    "https://claude.ai/api/desktop/darwin/universal/dmg/latest/redirect";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Default)]
pub enum DesktopHandoffState {
    #[default]
    Checking,
    PromptDownload,
    Flushing,
    Opening,
    Success,
    Error,
}

/// Maps to CC `getDownloadUrl()` using a caller-provided platform string.
pub fn desktop_download_url_for_platform(platform: &str) -> &'static str {
    match platform {
        "win32" => DESKTOP_DOWNLOAD_WIN32_X64,
        _ => DESKTOP_DOWNLOAD_DARWIN_UNIVERSAL,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DesktopHandoffDone {
    pub result: Option<String>,
    pub display: Option<CommandResultDisplay>,
    /// True when official code would have opened the desktop download URL.
    pub would_open_download_url: bool,
    /// True when official success timer would have called gracefulShutdown.
    pub would_shutdown: bool,
}

#[derive(Default, Props)]
pub struct DesktopHandoffProps<'a> {
    pub state: DesktopHandoffState,
    pub error: Option<String>,
    pub download_message: Option<String>,
    pub on_done: HandlerMut<'a, DesktopHandoffDone>,
}

#[component]
pub fn DesktopHandoff<'a>(
    props: &mut DesktopHandoffProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<crate::utils::theme::Theme>();
    let mut pending = hooks.use_state(|| Option::<DesktopHandoffDone>::None);
    let mut reported_success = hooks.use_state(|| false);

    let state = props.state;
    let error = props.error.clone();
    hooks.use_terminal_events({
        let mut pending = pending;
        move |event| {
            let TerminalEvent::Key(key) = event else {
                return;
            };
            if key.kind == KeyEventKind::Release {
                return;
            }

            match state {
                DesktopHandoffState::Error => {
                    pending.set(Some(DesktopHandoffDone {
                        result: Some(error.clone().unwrap_or_else(|| "Unknown error".to_string())),
                        display: Some(CommandResultDisplay::System),
                        would_open_download_url: false,
                        would_shutdown: false,
                    }));
                }
                DesktopHandoffState::PromptDownload => {
                    if matches!(key.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
                        pending.set(Some(DesktopHandoffDone {
                            result: Some(format!(
                                "Starting download. Re-run /desktop once you’ve installed the app.\nLearn more at {DESKTOP_DOCS_URL}"
                            )),
                            display: Some(CommandResultDisplay::System),
                            would_open_download_url: true,
                            would_shutdown: false,
                        }));
                    } else if matches!(key.code, KeyCode::Char('n') | KeyCode::Char('N')) {
                        pending.set(Some(DesktopHandoffDone {
                            result: Some(format!(
                                "The desktop app is required for /desktop. Learn more at {DESKTOP_DOCS_URL}"
                            )),
                            display: Some(CommandResultDisplay::System),
                            would_open_download_url: false,
                            would_shutdown: false,
                        }));
                    }
                }
                _ => {}
            }
        }
    });

    let pending_done = { pending.read().clone() };
    if let Some(done) = pending_done {
        pending.set(None);
        (props.on_done)(done);
    }

    if props.state == DesktopHandoffState::Success && !reported_success.get() {
        reported_success.set(true);
        (props.on_done)(DesktopHandoffDone {
            result: Some("Session transferred to Claude Desktop".to_string()),
            display: Some(CommandResultDisplay::System),
            would_open_download_url: false,
            would_shutdown: true,
        });
    } else if props.state != DesktopHandoffState::Success && reported_success.get() {
        reported_success.set(false);
    }

    match props.state {
        DesktopHandoffState::Error => element! {
            View(flex_direction: FlexDirection::Column, padding_left: 2u32, padding_right: 2u32) {
                Text(content: format!("Error: {}", props.error.clone().unwrap_or_else(|| "Unknown error".to_string())), color: theme.error)
                Text(content: "Press any key to continue…".to_string(), dim: true)
            }
        }
        .into_any(),
        DesktopHandoffState::PromptDownload => element! {
            View(flex_direction: FlexDirection::Column, padding_left: 2u32, padding_right: 2u32) {
                Text(content: props.download_message.clone().unwrap_or_else(|| "Claude Desktop is not installed.".to_string()))
                Text(content: "Download now? (y/n)".to_string())
            }
        }
        .into_any(),
        DesktopHandoffState::Checking => element! {
            LoadingState(message: "Checking for Claude Desktop…".to_string())
        }
        .into_any(),
        DesktopHandoffState::Flushing => element! {
            LoadingState(message: "Saving session…".to_string())
        }
        .into_any(),
        DesktopHandoffState::Opening => element! {
            LoadingState(message: "Opening Claude Desktop…".to_string())
        }
        .into_any(),
        DesktopHandoffState::Success => element! {
            LoadingState(message: "Opening in Claude Desktop…".to_string())
        }
        .into_any(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;
    use futures::StreamExt;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn key(code: KeyCode) -> TerminalEvent {
        TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code))
    }

    async fn drive_with_key(
        state: DesktopHandoffState,
        key_code: KeyCode,
        results: Arc<Mutex<Vec<DesktopHandoffDone>>>,
    ) {
        let results_for_handler = results.clone();
        let mut app = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                DesktopHandoff(
                    state: state,
                    error: Some("boom".to_string()),
                    download_message: Some("Claude Desktop is not installed.".to_string()),
                    on_done: move |done: DesktopHandoffDone| results_for_handler.lock().unwrap().push(done),
                )
            }
        };
        let mut render_loop = Box::pin(
            app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(futures::stream::iter(vec![key(key_code)]))
                    .with_size(100, 24),
            ),
        );
        for _ in 0..4 {
            let next = crate::utils::race(render_loop.next(), async {
                futures_timer::Delay::new(Duration::from_millis(100)).await;
                None
            })
            .await;
            if next.is_none() {
                break;
            }
        }
    }

    #[test]
    fn desktop_download_url_matches_official_platform_switch() {
        assert_eq!(
            desktop_download_url_for_platform("win32"),
            DESKTOP_DOWNLOAD_WIN32_X64
        );
        assert_eq!(
            desktop_download_url_for_platform("darwin"),
            DESKTOP_DOWNLOAD_DARWIN_UNIVERSAL
        );
        assert_eq!(
            desktop_download_url_for_platform("linux"),
            DESKTOP_DOWNLOAD_DARWIN_UNIVERSAL
        );
    }

    #[test]
    fn desktop_handoff_renders_loading_and_prompt_states() {
        let current_theme = *theme::current();
        let checking = element! {
            ContextProvider(value: Context::owned(current_theme)) {
                DesktopHandoff(state: DesktopHandoffState::Checking)
            }
        }
        .render(Some(100))
        .to_string();
        assert!(
            checking.contains("Checking for Claude Desktop…"),
            "canvas=\n{checking}"
        );

        let prompt = element! {
            ContextProvider(value: Context::owned(current_theme)) {
                DesktopHandoff(
                    state: DesktopHandoffState::PromptDownload,
                    download_message: Some("Claude Desktop needs to be updated (found v1.0.0, need v1.1.2396+).".to_string()),
                )
            }
        }
        .render(Some(100))
        .to_string();
        assert!(prompt.contains("needs to be updated"), "canvas=\n{prompt}");
        assert!(prompt.contains("Download now? (y/n)"), "canvas=\n{prompt}");
    }

    #[test]
    fn desktop_handoff_prompt_y_reports_download_result_without_opening_browser() {
        let results = Arc::new(Mutex::new(Vec::new()));
        futures::executor::block_on(drive_with_key(
            DesktopHandoffState::PromptDownload,
            KeyCode::Char('y'),
            results.clone(),
        ));

        let captured = results.lock().unwrap().clone();
        assert_eq!(captured.len(), 1);
        assert!(
            captured[0]
                .result
                .as_ref()
                .unwrap()
                .contains("Starting download")
        );
        assert!(captured[0].would_open_download_url);
        assert!(!captured[0].would_shutdown);
    }

    #[test]
    fn desktop_handoff_error_any_key_reports_system_error() {
        let results = Arc::new(Mutex::new(Vec::new()));
        futures::executor::block_on(drive_with_key(
            DesktopHandoffState::Error,
            KeyCode::Char('x'),
            results.clone(),
        ));

        assert_eq!(
            results.lock().unwrap().as_slice(),
            &[DesktopHandoffDone {
                result: Some("boom".to_string()),
                display: Some(CommandResultDisplay::System),
                would_open_download_url: false,
                would_shutdown: false,
            }]
        );
    }
}
