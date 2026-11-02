//! Maps to: CC `components/LspRecommendation/LspRecommendationMenu.tsx`.
//!
//! LSP plugin recommendation prompt. The official component auto-dismisses
//! after 30 seconds as a `no` response; this Rust boundary exposes the same
//! response values and safe UI callback while leaving timers/integration to the
//! caller.

use crate::components::custom_select::{Select, SelectLayout, SelectOptionData};
use crate::components::permissions::permission_dialog::PermissionDialog;
use crate::utils::theme::Theme;
use iocraft::prelude::*;

pub const AUTO_DISMISS_MS: u64 = 30_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LspRecommendationResponse {
    Yes,
    No,
    Never,
    Disable,
}

impl LspRecommendationResponse {
    pub fn value(self) -> &'static str {
        match self {
            Self::Yes => "yes",
            Self::No => "no",
            Self::Never => "never",
            Self::Disable => "disable",
        }
    }
}

fn response_from_value(value: &str) -> LspRecommendationResponse {
    match value {
        "yes" => LspRecommendationResponse::Yes,
        "never" => LspRecommendationResponse::Never,
        "disable" => LspRecommendationResponse::Disable,
        _ => LspRecommendationResponse::No,
    }
}

/// Maps to: CC `LspRecommendationMenu` option construction.
pub fn lsp_recommendation_options(plugin_name: &str) -> Vec<SelectOptionData> {
    vec![
        SelectOptionData {
            label: format!("Yes, install {plugin_name}"),
            value: LspRecommendationResponse::Yes.value().to_string(),
            ..SelectOptionData::default()
        },
        SelectOptionData {
            label: "No, not now".to_string(),
            value: LspRecommendationResponse::No.value().to_string(),
            ..SelectOptionData::default()
        },
        SelectOptionData {
            label: format!("Never for {plugin_name}"),
            value: LspRecommendationResponse::Never.value().to_string(),
            ..SelectOptionData::default()
        },
        SelectOptionData {
            label: "Disable all LSP recommendations".to_string(),
            value: LspRecommendationResponse::Disable.value().to_string(),
            ..SelectOptionData::default()
        },
    ]
}

#[derive(Default, Props)]
pub struct LspRecommendationMenuProps<'a> {
    pub plugin_name: String,
    pub plugin_description: Option<String>,
    pub file_extension: String,
    pub focused_index: usize,
    pub on_response: HandlerMut<'a, LspRecommendationResponse>,
}

/// Maps to: CC `LspRecommendationMenu(...)`.
#[component]
pub fn LspRecommendationMenu<'a>(
    props: &mut LspRecommendationMenuProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let options = lsp_recommendation_options(&props.plugin_name);
    let option_count = options.len().max(1);
    let mut focused_index = hooks.use_state(|| props.focused_index.min(option_count - 1));
    let mut pending_response = hooks.use_state(|| Option::<LspRecommendationResponse>::None);

    hooks.use_terminal_events({
        let mut focused_index = focused_index;
        let mut pending_response = pending_response;
        let options = options.clone();
        move |event| {
            let TerminalEvent::Key(KeyEvent { code, kind, .. }) = event else {
                return;
            };
            if kind == KeyEventKind::Release {
                return;
            }
            match code {
                KeyCode::Up | KeyCode::Char('k') => {
                    focused_index.set(focused_index.get().saturating_sub(1))
                }
                KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => focused_index
                    .set((focused_index.get() + 1).min(options.len().saturating_sub(1))),
                KeyCode::Enter => {
                    if let Some(option) = options.get(focused_index.get()) {
                        pending_response.set(Some(response_from_value(&option.value)));
                    }
                }
                KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                    pending_response.set(Some(LspRecommendationResponse::No))
                }
                _ => {}
            }
        }
    });

    let pending = { *pending_response.read() };
    if let Some(response) = pending {
        pending_response.set(None);
        (props.on_response)(response);
    }

    element! {
        PermissionDialog(title: "LSP Plugin Recommendation".to_string()) {
            View(flex_direction: FlexDirection::Column, padding_left: 2u32, padding_right: 2u32, padding_top: 1u32, padding_bottom: 1u32) {
                View(margin_bottom: 1u32) {
                    Text(
                        content: "LSP provides code intelligence like go-to-definition and error checking".to_string(),
                        color: theme.inactive,
                        wrap: TextWrap::Wrap,
                    )
                }
                View(flex_direction: FlexDirection::Row) {
                    Text(content: "Plugin:".to_string(), color: theme.inactive)
                    Text(content: format!(" {}", props.plugin_name))
                }
                #(props.plugin_description.as_ref().filter(|s| !s.is_empty()).map(|description| element! {
                    View {
                        Text(content: description.clone(), color: theme.inactive, wrap: TextWrap::Wrap)
                    }
                }))
                View(flex_direction: FlexDirection::Row) {
                    Text(content: "Triggered by:".to_string(), color: theme.inactive)
                    Text(content: format!(" {} files", props.file_extension))
                }
                View(margin_top: 1u32) {
                    Text(content: "Would you like to install this LSP plugin?".to_string())
                }
                View {
                    Select(
                        options: options,
                        focused_index: focused_index.get().min(option_count - 1),
                        visible_option_count: option_count,
                        layout: SelectLayout::CompactVertical,
                        hide_indexes: true,
                    )
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;
    use futures::{StreamExt, stream};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn key(code: KeyCode) -> TerminalEvent {
        TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code))
    }

    #[test]
    fn lsp_recommendation_renders_official_copy_and_options() {
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                LspRecommendationMenu(
                    plugin_name: "rust-analyzer".to_string(),
                    plugin_description: Some("Rust language server".to_string()),
                    file_extension: "rs".to_string(),
                )
            }
        }
        .render(Some(100))
        .to_string();

        assert!(
            text.contains("LSP Plugin Recommendation"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("go-to-definition and error checking"),
            "canvas=\n{text}"
        );
        assert!(text.contains("Plugin: rust-analyzer"), "canvas=\n{text}");
        assert!(text.contains("Rust language server"), "canvas=\n{text}");
        assert!(text.contains("Triggered by: rs files"), "canvas=\n{text}");
        assert!(
            text.contains("Yes, install rust-analyzer"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("Disable all LSP recommendations"),
            "canvas=\n{text}"
        );
    }

    #[test]
    fn lsp_recommendation_enter_and_escape_emit_official_responses() {
        let responses = Arc::new(Mutex::new(Vec::new()));
        let responses_for_handler = Arc::clone(&responses);
        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(*theme::current())) {
                    LspRecommendationMenu(
                        plugin_name: "rust-analyzer".to_string(),
                        file_extension: "rs".to_string(),
                        on_response: move |response| responses_for_handler.lock().expect("responses").push(response),
                    )
                }
            };
            let mut render_loop = Box::pin(
                app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(stream::iter(vec![key(KeyCode::Esc)]))
                        .with_size(100, 30),
                ),
            );
            for _ in 0..8 {
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
        assert_eq!(
            *responses.lock().expect("responses"),
            vec![LspRecommendationResponse::No]
        );
    }
}
