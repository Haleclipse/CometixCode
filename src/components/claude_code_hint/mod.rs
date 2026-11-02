//! Maps to: CC `components/ClaudeCodeHint/PluginHintMenu.tsx`.
//!
//! Plugin-installation hint menu, including the official 30-second automatic
//! `no` response.

use crate::components::custom_select::{Select, SelectLayout, SelectOptionData};
use crate::components::permissions::permission_dialog::PermissionDialog;
use crate::utils::theme::Theme;
use iocraft::prelude::*;

pub const AUTO_DISMISS_MS: u64 = 30_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginHintResponse {
    Yes,
    No,
    Disable,
}

impl PluginHintResponse {
    pub fn value(self) -> &'static str {
        match self {
            Self::Yes => "yes",
            Self::No => "no",
            Self::Disable => "disable",
        }
    }
}

fn response_from_value(value: &str) -> PluginHintResponse {
    match value {
        "yes" => PluginHintResponse::Yes,
        "disable" => PluginHintResponse::Disable,
        _ => PluginHintResponse::No,
    }
}

/// Maps to: CC `PluginHintMenu` option construction.
pub fn plugin_hint_options(plugin_name: &str) -> Vec<SelectOptionData> {
    vec![
        SelectOptionData {
            label: format!("Yes, install {plugin_name}"),
            value: PluginHintResponse::Yes.value().to_string(),
            ..SelectOptionData::default()
        },
        SelectOptionData {
            label: "No".to_string(),
            value: PluginHintResponse::No.value().to_string(),
            ..SelectOptionData::default()
        },
        SelectOptionData {
            label: "No, and don't show plugin installation hints again".to_string(),
            value: PluginHintResponse::Disable.value().to_string(),
            ..SelectOptionData::default()
        },
    ]
}

#[derive(Default, Props)]
pub struct PluginHintMenuProps<'a> {
    pub plugin_name: String,
    pub plugin_description: Option<String>,
    pub marketplace_name: String,
    pub source_command: String,
    pub focused_index: usize,
    pub on_response: HandlerMut<'a, PluginHintResponse>,
}

/// Maps to: CC `PluginHintMenu(...)`.
#[component]
pub fn PluginHintMenu<'a>(
    props: &mut PluginHintMenuProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let options = plugin_hint_options(&props.plugin_name);
    let option_count = options.len().max(1);
    let mut focused_index = hooks.use_state(|| props.focused_index.min(option_count - 1));
    let mut pending_response = hooks.use_state(|| Option::<PluginHintResponse>::None);

    hooks.use_interval(
        {
            let mut pending_response = pending_response;
            move || pending_response.set(Some(PluginHintResponse::No))
        },
        Some(std::time::Duration::from_millis(AUTO_DISMISS_MS)),
    );

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
                    pending_response.set(Some(PluginHintResponse::No))
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
        PermissionDialog(title: "Plugin Recommendation".to_string()) {
            View(flex_direction: FlexDirection::Column, padding_left: 2u32, padding_right: 2u32, padding_top: 1u32, padding_bottom: 1u32) {
                View(margin_bottom: 1u32) {
                    Text(content: "The ".to_string(), color: theme.inactive, wrap: TextWrap::NoWrap)
                    Text(content: props.source_command.clone(), color: theme.inactive, weight: Weight::Bold, wrap: TextWrap::NoWrap)
                    Text(content: " command suggests installing a plugin.".to_string(), color: theme.inactive, wrap: TextWrap::Wrap)
                }
                View(flex_direction: FlexDirection::Row) {
                    Text(content: "Plugin:".to_string(), color: theme.inactive)
                    Text(content: format!(" {}", props.plugin_name))
                }
                View(flex_direction: FlexDirection::Row) {
                    Text(content: "Marketplace:".to_string(), color: theme.inactive)
                    Text(content: format!(" {}", props.marketplace_name))
                }
                #(props.plugin_description.as_ref().filter(|s| !s.is_empty()).map(|description| element! {
                    View {
                        Text(content: description.clone(), color: theme.inactive, wrap: TextWrap::Wrap)
                    }
                }))
                View(margin_top: 1u32) {
                    Text(content: "Would you like to install it?".to_string())
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
    fn plugin_hint_renders_official_copy_and_options() {
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                PluginHintMenu(
                    plugin_name: "Example".to_string(),
                    plugin_description: Some("Adds useful commands".to_string()),
                    marketplace_name: "anthropic".to_string(),
                    source_command: "/plugin".to_string(),
                )
            }
        }
        .render(Some(100))
        .to_string();

        assert!(text.contains("Plugin Recommendation"), "canvas=\n{text}");
        assert!(
            text.contains("The /plugin command suggests installing a plugin."),
            "canvas=\n{text}"
        );
        assert!(text.contains("Plugin: Example"), "canvas=\n{text}");
        assert!(text.contains("Marketplace: anthropic"), "canvas=\n{text}");
        assert!(text.contains("Adds useful commands"), "canvas=\n{text}");
        assert!(text.contains("Yes, install Example"), "canvas=\n{text}");
        assert!(
            text.contains("No, and don't show plugin installation hints again"),
            "canvas=\n{text}"
        );
    }

    #[test]
    fn plugin_hint_disable_option_emits_official_response() {
        let responses = Arc::new(Mutex::new(Vec::new()));
        let responses_for_handler = Arc::clone(&responses);
        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(*theme::current())) {
                    PluginHintMenu(
                        plugin_name: "Example".to_string(),
                        marketplace_name: "anthropic".to_string(),
                        source_command: "/plugin".to_string(),
                        focused_index: 2usize,
                        on_response: move |response| responses_for_handler.lock().expect("responses").push(response),
                    )
                }
            };
            let mut render_loop = Box::pin(
                app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(stream::iter(vec![key(KeyCode::Enter)]))
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
            vec![PluginHintResponse::Disable]
        );
    }
}
