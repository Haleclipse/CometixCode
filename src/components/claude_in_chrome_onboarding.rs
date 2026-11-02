//! Maps to: CC `components/ClaudeInChromeOnboarding.tsx`.
//!
//! Safety boundary: official mount effects log analytics, asynchronously probe
//! `isChromeExtensionInstalled()`, and write
//! `hasCompletedClaudeInChromeOnboarding` to global config. Nyan keeps the
//! dialog copy, links, Enter/Esc completion behavior, and installed/not-installed
//! render branches from explicit snapshot props only. No analytics, extension
//! probe, browser launch, or config write occurs here.

use crate::components::design_system::dialog::Dialog;
use iocraft::prelude::*;

pub const CHROME_EXTENSION_URL: &str = "https://claude.ai/chrome";
pub const CHROME_PERMISSIONS_URL: &str = "https://clau.de/chrome/permissions";
pub const CHROME_DOCS_URL: &str = "https://code.claude.com/docs/en/chrome";

#[derive(Default, Props)]
pub struct ClaudeInChromeOnboardingProps<'a> {
    /// Maps to official `extensionInstalled` state after
    /// `isChromeExtensionInstalled()`, supplied by a safe snapshot producer.
    pub is_extension_installed: bool,
    pub on_done: HandlerMut<'a, ()>,
}

#[component]
pub fn ClaudeInChromeOnboarding<'a>(
    props: &mut ClaudeInChromeOnboardingProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<crate::utils::theme::Theme>();
    let mut pending_done = hooks.use_state(|| false);

    hooks.use_terminal_events({
        let mut pending_done = pending_done;
        move |event| {
            if let TerminalEvent::Key(KeyEvent {
                code: KeyCode::Enter,
                kind,
                ..
            }) = event
                && kind != KeyEventKind::Release
            {
                pending_done.set(true);
            }
        }
    });

    if pending_done.get() {
        pending_done.set(false);
        (props.on_done)(());
    }

    let mut pending_done_for_cancel = pending_done;
    let installed = props.is_extension_installed;

    element! {
        Dialog(
            title: "Claude in Chrome (Beta)".to_string(),
            color: Some(theme.chrome_yellow),
            on_cancel: move |_| pending_done_for_cancel.set(true),
        ) {
            View(flex_direction: FlexDirection::Column, gap: 1u32) {
                View(flex_direction: FlexDirection::Column) {
                    Text(
                        content: "Claude in Chrome works with the Chrome extension to let you control your browser directly from nyan. You can navigate websites, fill forms, capture screenshots, record GIFs, and debug with console logs and network requests.".to_string(),
                        wrap: TextWrap::Wrap,
                    )
                    #(if !installed {
                        Some(element! {
                            View(margin_top: 1u32, flex_direction: FlexDirection::Row) {
                                Text(content: "Requires the Chrome extension. Get started at ".to_string())
                                Link(url: CHROME_EXTENSION_URL.to_string())
                            }
                        })
                    } else {
                        None
                    })
                }
                View(flex_direction: FlexDirection::Column) {
                    #(if installed {
                        Some(element! {
                            Fragment {
                                Text(content: "Site-level permissions are inherited from the Chrome extension. Manage permissions in the Chrome extension settings to control which sites Claude can browse, click, and type on".to_string(), dim: true, wrap: TextWrap::Wrap)
                                View(flex_direction: FlexDirection::Row) {
                                    Text(content: "(".to_string(), dim: true)
                                    Link(url: CHROME_PERMISSIONS_URL.to_string())
                                    Text(content: ").".to_string(), dim: true)
                                }
                            }
                        }.into_any())
                    } else {
                        Some(element! {
                            Text(content: "Site-level permissions are inherited from the Chrome extension. Manage permissions in the Chrome extension settings to control which sites Claude can browse, click, and type on.".to_string(), dim: true, wrap: TextWrap::Wrap)
                        }.into_any())
                    })
                }
                View(flex_direction: FlexDirection::Row) {
                    Text(content: "For more info, use ".to_string(), dim: true)
                    Text(content: "/chrome".to_string(), color: theme.chrome_yellow, weight: Weight::Bold)
                    Text(content: " or visit ".to_string(), dim: true)
                    Link(url: CHROME_DOCS_URL.to_string())
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;
    use futures::StreamExt;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn text(canvas: &Canvas) -> String {
        canvas.to_string()
    }

    #[test]
    fn chrome_onboarding_not_installed_branch_renders_extension_link() {
        let current_theme = *theme::current();
        let canvas = element! {
            ContextProvider(value: Context::owned(current_theme)) {
                ClaudeInChromeOnboarding(is_extension_installed: false)
            }
        }
        .render(Some(100));
        let text = text(&canvas);

        assert!(text.contains("Claude in Chrome (Beta)"), "canvas=\n{text}");
        assert!(
            text.contains("control your browser directly"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("Requires the Chrome extension. Get started at"),
            "canvas=\n{text}"
        );
        assert!(text.contains(CHROME_EXTENSION_URL), "canvas=\n{text}");
        assert!(
            text.contains("Site-level permissions are inherited"),
            "canvas=\n{text}"
        );
        assert!(text.contains("/chrome"), "canvas=\n{text}");
        assert!(!text.contains(CHROME_PERMISSIONS_URL), "canvas=\n{text}");
    }

    #[test]
    fn chrome_onboarding_installed_branch_renders_permissions_link() {
        let current_theme = *theme::current();
        let canvas = element! {
            ContextProvider(value: Context::owned(current_theme)) {
                ClaudeInChromeOnboarding(is_extension_installed: true)
            }
        }
        .render(Some(100));
        let text = text(&canvas);

        assert!(text.contains("Claude in Chrome (Beta)"), "canvas=\n{text}");
        assert!(text.contains(CHROME_PERMISSIONS_URL), "canvas=\n{text}");
        assert!(
            text.contains(&format!("{CHROME_PERMISSIONS_URL}).")),
            "permissions link should keep official period after the link; canvas=\n{text}"
        );
        assert!(
            !text.contains("Requires the Chrome extension. Get started at"),
            "canvas=\n{text}"
        );
    }

    #[test]
    fn chrome_onboarding_enter_completes_without_side_effects() {
        let current_theme = *theme::current();
        let done_count = Arc::new(Mutex::new(0usize));
        let done_for_handler = Arc::clone(&done_count);

        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(current_theme)) {
                    ClaudeInChromeOnboarding(
                        is_extension_installed: false,
                        on_done: move |_| *done_for_handler.lock().expect("done mutex") += 1,
                    )
                }
            };
            let mut render_loop = Box::pin(
                app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(futures::stream::iter(vec![
                        TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Enter)),
                    ]))
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
        });

        assert_eq!(*done_count.lock().expect("done mutex"), 1);
    }
}
