//! Maps to: CC `utils/exportRenderer.tsx`.

use std::future::Future;
use std::sync::Arc;

use iocraft::prelude::*;

use crate::components::messages_list::Messages;
use crate::keybindings::keybinding_context::KeybindingRuntime;
use crate::keybindings::load_user_bindings::load_keybindings_sync_with_warnings;
use crate::screens::repl::Screen;
use crate::state::app_state::{AppStateProvider, ProviderChildren};
use crate::types::message::Message;
use crate::types::tools::Tool;
use crate::utils::static_render::render_to_ansi_string;

/// Maps to: CC `utils/exportRenderer.tsx#StaticKeybindingProvider` props.
/// Vec<AnyElement> is the existing L1 React children representation.
#[derive(Default, Props)]
struct StaticKeybindingProviderProps {
    children: Vec<AnyElement<'static>>,
}

/// Maps to: CC `utils/exportRenderer.tsx#StaticKeybindingProvider`.
/// The canonical context has bindings, an empty chord, registry and context
/// set; no KeybindingProviderSetup/ChordInterceptor or input loop is mounted.
#[component]
fn StaticKeybindingProvider(
    props: &mut StaticKeybindingProviderProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let runtime =
        hooks.use_state(|| KeybindingRuntime::new(load_keybindings_sync_with_warnings().bindings));
    let children = std::mem::take(&mut props.children);
    element! {
        ContextProvider(value: Context::owned(runtime.read().clone())) {
            #(children)
        }
    }
}

/// Maps to: CC `utils/exportRenderer.tsx#normalizedUpperBound`.
/// L1 typed Message content is always a Vec; original string content arrives
/// as its single Text block through the canonical message adapter.
fn normalized_upper_bound(message: &Message) -> usize {
    match message {
        Message::User(message) => message.content.len(),
        Message::Assistant(message) => message.content.len(),
        _ => 1,
    }
}

/// Maps to: CC `utils/exportRenderer.tsx#streamRenderedMessages` options.
pub struct StreamRenderedMessagesOptions<'a> {
    pub columns: Option<usize>,
    pub verbose: bool,
    pub chunk_size: usize,
    pub on_progress: Option<&'a mut dyn FnMut(usize)>,
}

impl Default for StreamRenderedMessagesOptions<'_> {
    fn default() -> Self {
        Self {
            columns: None,
            verbose: false,
            chunk_size: 40,
            on_progress: None,
        }
    }
}

/// Maps to: CC `utils/exportRenderer.tsx#streamRenderedMessages`.
/// Canonical normalization is the existing Message → RenderableMessage L1
/// projection; the full array is prepared for every new source render tree.
pub async fn stream_rendered_messages<F, Fut, E>(
    messages: Arc<Vec<Message>>,
    tools: Arc<Vec<Tool>>,
    mut sink: F,
    mut options: StreamRenderedMessagesOptions<'_>,
) -> Result<(), E>
where
    F: FnMut(String) -> Fut,
    Fut: Future<Output = Result<(), E>>,
{
    let mut ceiling = options.chunk_size;
    for message in messages.iter() {
        ceiling += normalized_upper_bound(message);
    }
    let mut offset = 0;
    while offset < ceiling {
        let range = (offset, offset + options.chunk_size);
        let normalized = Arc::new(crate::utils::messages::normalize_messages(&messages));
        let tools = Arc::clone(&tools);
        let verbose = options.verbose;
        let node = element! {
            AppStateProvider(children: ProviderChildren::new(move || {
                element! {
                    StaticKeybindingProvider {
                        Messages(
                            messages: Arc::clone(&normalized),
                            tools: Arc::clone(&tools),
                            verbose: verbose,
                            screen: Screen::Prompt,
                            show_all_in_transcript: true,
                            render_range: Some(range),
                        )
                    }
                }.into_any()
            }))
        }
        .into_any();
        let ansi = render_to_ansi_string(node, options.columns).await;
        let plain = strip_ansi_escapes::strip_str(&ansi);
        // JS String.trim's ECMAScript whitespace differs from Rust's trim.
        if plain
            .trim_matches(|c: char| {
                matches!(c,
                    '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}' |
                    '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' |
                    '\u{205f}' | '\u{3000}' | '\u{feff}'
                )
            })
            .is_empty()
        {
            break;
        }
        sink(ansi).await?;
        if let Some(progress) = options.on_progress.as_mut() {
            progress(offset + options.chunk_size);
        }
        offset += options.chunk_size;
    }
    Ok(())
}

/// Maps to: CC `utils/exportRenderer.tsx#renderMessagesToPlainText`.
pub async fn render_messages_to_plain_text(
    messages: Arc<Vec<Message>>,
    tools: Arc<Vec<Tool>>,
    columns: Option<usize>,
) -> String {
    let mut parts = Vec::new();
    let result: Result<(), std::convert::Infallible> = stream_rendered_messages(
        messages,
        tools,
        |chunk| {
            parts.push(strip_ansi_escapes::strip_str(chunk));
            std::future::ready(Ok(()))
        },
        StreamRenderedMessagesOptions {
            columns,
            ..Default::default()
        },
    )
    .await;
    match result {
        Ok(()) => {}
        Err(impossible) => match impossible {},
    }
    parts.join("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::message::{AssistantContent, AssistantMessage};
    use crate::utils::messages::create_user_message;

    /// CC exportRenderer.tsx:47-51; actual Bun multiblock/empty-array oracle.
    #[test]
    fn normalized_upper_bound_matches_official_block_count() {
        let mut user = create_user_message("one".into());
        assert_eq!(normalized_upper_bound(&Message::User(user.clone())), 1);
        user.content.clear();
        assert_eq!(normalized_upper_bound(&Message::User(user)), 0);
        let assistant = AssistantMessage {
            uuid: "many".into(),
            timestamp: chrono::Utc::now(),
            content: (0..81)
                .map(|i| AssistantContent::Text(format!("block-{i}")))
                .collect(),
            model: None,
            stop_reason: None,
            usage: None,
        };
        assert_eq!(normalized_upper_bound(&Message::Assistant(assistant)), 81);
    }

    /// CC exportRenderer.tsx:109-118, Messages.tsx:677-684,919. More than the
    /// interactive 200-row cap must reach the real Messages tree exactly once.
    #[tokio::test]
    async fn export_renderer_matches_official_chunk_progress_and_complete_history() {
        let messages = Arc::new(
            (0..205)
                .map(|i| Message::User(create_user_message(format!("export-row-{i:03}"))))
                .collect(),
        );
        let mut parts = Vec::new();
        let mut progress = Vec::new();
        let mut on_progress = |count| progress.push(count);
        let result: Result<(), std::convert::Infallible> = stream_rendered_messages(
            messages,
            Arc::new(Vec::new()),
            |chunk| {
                parts.push(strip_ansi_escapes::strip_str(chunk));
                std::future::ready(Ok(()))
            },
            StreamRenderedMessagesOptions {
                columns: Some(80),
                on_progress: Some(&mut on_progress),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(result, Ok(()));
        assert_eq!(progress, [40, 80, 120, 160, 200, 240]);
        assert_eq!(parts.len(), 6);
        let text = parts.join("");
        for i in 0..205 {
            assert_eq!(
                text.matches(&format!("export-row-{i:03}")).count(),
                1,
                "missing/repeated row {i}"
            );
        }
        assert!(!parts[1].contains("export-row-039"));
        assert!(parts[1].contains("export-row-040"));
    }

    /// CC exportRenderer.tsx:116-117: await sink before progress and propagate
    /// its rejection without attempting another chunk. Bun sink_error oracle.
    #[tokio::test]
    async fn export_renderer_matches_official_sink_failure_order() {
        let messages = Arc::new(vec![Message::User(create_user_message("fixture".into()))]);
        let mut progress = Vec::new();
        let mut on_progress = |count| progress.push(count);
        let mut writes = 0;
        let result = stream_rendered_messages(
            messages,
            Arc::new(Vec::new()),
            |_| {
                writes += 1;
                std::future::ready(Err("fixture sink failure"))
            },
            StreamRenderedMessagesOptions {
                on_progress: Some(&mut on_progress),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(result, Err("fixture sink failure"));
        assert_eq!(writes, 1);
        assert!(progress.is_empty());
    }

    /// CC exportRenderer.tsx:82-101 requires both real providers even when
    /// no messages exist; renderRange:0 includes the source logo once.
    #[tokio::test]
    async fn export_renderer_matches_official_empty_session_logo() {
        let text =
            render_messages_to_plain_text(Arc::new(Vec::new()), Arc::new(Vec::new()), Some(80))
                .await;
        assert!(!text.trim().is_empty());
        assert!(!text.contains('\x1b'));
    }
}
