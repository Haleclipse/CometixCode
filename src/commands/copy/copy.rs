//! Maps to: CC `commands/copy/copy.tsx:23-307`.
//!
//! The command walks typed model history newest-first, extracts fenced code
//! blocks with the same marked lexer used by Markdown, and either copies the
//! full response immediately or mounts [`CopyPicker`]. The `w` shortcut is an
//! L1 keybinding projection of CC's component-local `onKeyDown` handler.
//! `tengu_copy` analytics calls are intentionally absent because telemetry is
//! outside this clone's declared scope; no command behavior depends on them.

use crate::components::custom_select::{
    Select, SelectInputOptionMeta, SelectLayout, SelectOptionData, UseSelectInputOptions,
    UseSelectStateProps, use_select_input, use_select_state,
};
use crate::components::design_system::byline::Byline;
use crate::components::design_system::keyboard_shortcut_hint::{
    KeyboardShortcutHint, KeyboardShortcutHintStyleContext,
};
use crate::components::design_system::pane::Pane;
use crate::keybindings::keybinding_context::KeybindingRuntime;
use crate::keybindings::types::ContextName;
use crate::keybindings::use_keybinding::use_keybinding;
use crate::types::message::{AssistantContent, Message};
use iocraft::prelude::*;
use marked_rs::{MarkedOptions, Token as MarkedToken};
use std::path::{Path, PathBuf};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub const RESPONSE_FILENAME: &str = "response.md";
const MAX_LOOKBACK: usize = 20;
const COPY_PICKER_CONTEXT: &str = "CopyPicker";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeBlock {
    pub code: String,
    pub lang: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CopyContent {
    pub text: String,
    pub filename: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CopyPickerData {
    pub full_text: String,
    pub code_blocks: Vec<CodeBlock>,
    pub message_age: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CopyCall {
    Output { text: String, is_error: bool },
    Direct(CopyContent),
    Picker(CopyPickerData),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum PickerSelection {
    Full,
    Block(usize),
    Always,
}

impl PickerSelection {
    fn value(&self) -> String {
        match self {
            Self::Full => "full".to_string(),
            Self::Block(index) => format!("block:{index}"),
            Self::Always => "always".to_string(),
        }
    }

    fn from_value(value: &str) -> Option<Self> {
        match value {
            "full" => Some(Self::Full),
            "always" => Some(Self::Always),
            _ => value
                .strip_prefix("block:")?
                .parse::<usize>()
                .ok()
                .map(Self::Block),
        }
    }
}

/// Maps to: CC `commands/copy/copy.tsx:43-64` `collectRecentAssistantTexts`.
pub fn collect_recent_assistant_texts(messages: &[Message]) -> Vec<String> {
    let mut texts = Vec::new();
    for message in messages.iter().rev() {
        if texts.len() >= MAX_LOOKBACK {
            break;
        }
        let Message::Assistant(assistant) = message else {
            // API errors are represented by typed System(ApiError) messages in
            // Rust, so this branch also preserves CC's API-error exclusion.
            continue;
        };
        let text = assistant
            .content
            .iter()
            .filter_map(|content| match content {
                AssistantContent::Text(text) => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        if !text.is_empty() {
            texts.push(text);
        }
    }
    texts
}

/// Maps to: CC `commands/copy/copy.tsx:32-41` `extractCodeBlocks`
/// (`marked.lexer(stripPromptXMLTags(...))`).
pub fn extract_code_blocks(markdown: &str) -> Vec<CodeBlock> {
    let markdown = crate::utils::messages::strip_prompt_xml_tags(markdown);
    marked_rs::lexer_with_options(&markdown, MarkedOptions::default())
        .tokens
        .into_iter()
        .filter_map(|token| match token {
            MarkedToken::Code { text, lang, .. } => Some(CodeBlock { code: text, lang }),
            _ => None,
        })
        .collect()
}

/// Maps to: CC `commands/copy/copy.tsx:66-76` `fileExtension`.
pub fn file_extension(lang: Option<&str>) -> String {
    if let Some(lang) = lang {
        let sanitized = lang
            .chars()
            .filter(|character| character.is_ascii_alphanumeric())
            .collect::<String>();
        if !sanitized.is_empty() && sanitized != "plaintext" {
            return format!(".{sanitized}");
        }
    }
    ".txt".to_string()
}

fn js_string_length(text: &str) -> usize {
    text.encode_utf16().count()
}

fn line_count(text: &str) -> usize {
    text.chars().filter(|character| *character == '\n').count() + 1
}

fn parse_js_integer(text: &str) -> Option<usize> {
    let radix_value = [
        ("0x", 16),
        ("0X", 16),
        ("0b", 2),
        ("0B", 2),
        ("0o", 8),
        ("0O", 8),
    ]
    .into_iter()
    .find_map(|(prefix, radix)| {
        text.strip_prefix(prefix)
            .map(|digits| usize::from_str_radix(digits, radix).ok())
    });
    if let Some(value) = radix_value {
        return value;
    }

    let number = text.parse::<f64>().ok()?;
    (number.is_finite() && number >= 0.0 && number.fract() == 0.0 && number <= usize::MAX as f64)
        .then(|| number as usize)
}

/// Maps to: CC `commands/copy/copy.tsx:103-120` `truncateLine`, using
/// terminal cell width rather than bytes.
pub fn truncate_line(text: &str, max_len: usize) -> String {
    let first_line = text.split('\n').next().unwrap_or_default();
    if UnicodeWidthStr::width(first_line) <= max_len {
        return first_line.to_string();
    }

    let target_width = max_len.saturating_sub(1);
    let mut result = String::new();
    let mut width = 0;
    for character in first_line.chars() {
        let character_width = character.width().unwrap_or(0);
        if width + character_width > target_width {
            break;
        }
        result.push(character);
        width += character_width;
    }
    result.push('…');
    result
}

/// Maps to: CC `commands/copy/copy.tsx:264-307` `call` up to the
/// terminal/UI side-effect boundary.
pub fn call(messages: &[Message], args: &str) -> CopyCall {
    // Running `/copy` loads `copy.tsx` (`index.ts:12`), which evaluates
    // `COPY_DIR` whether or not this call writes a file.
    std::sync::LazyLock::force(&COPY_DIR);
    prepare_call(
        messages,
        args,
        crate::utils::config::load_global_config()
            .copy_full_response
            .unwrap_or(false),
    )
}

fn prepare_call(messages: &[Message], args: &str, copy_full_response: bool) -> CopyCall {
    let texts = collect_recent_assistant_texts(messages);
    if texts.is_empty() {
        return CopyCall::Output {
            text: "No assistant message to copy".to_string(),
            is_error: false,
        };
    }

    let mut age = 0;
    let arg = args.trim();
    if !arg.is_empty() {
        let Some(index) = parse_js_integer(arg) else {
            return CopyCall::Output {
                text: format!("Usage: /copy [N] where N is 1 (latest), 2, 3, … Got: {arg}"),
                is_error: false,
            };
        };
        if index < 1 {
            return CopyCall::Output {
                text: format!("Usage: /copy [N] where N is 1 (latest), 2, 3, … Got: {arg}"),
                is_error: false,
            };
        }
        if index > texts.len() {
            return CopyCall::Output {
                text: format!(
                    "Only {} assistant {} available to copy",
                    texts.len(),
                    if texts.len() == 1 {
                        "message"
                    } else {
                        "messages"
                    }
                ),
                is_error: false,
            };
        }
        age = index - 1;
    }

    let text = texts[age].clone();
    let code_blocks = extract_code_blocks(&text);
    if code_blocks.is_empty() || copy_full_response {
        return CopyCall::Direct(CopyContent {
            text,
            filename: RESPONSE_FILENAME.to_string(),
        });
    }

    CopyCall::Picker(CopyPickerData {
        full_text: text,
        code_blocks,
        message_age: age,
    })
}

/// CC `copy.tsx:23` `const COPY_DIR = join(tmpdir(), 'claude')`: a module
/// constant, fixed when the command module first loads (CC loads it lazily,
/// on the first `/copy`), here on first use.
static COPY_DIR: std::sync::LazyLock<PathBuf> =
    std::sync::LazyLock::new(|| crate::utils::node_os::tmpdir().join("claude"));

async fn write_to_dir(text: &str, filename: &str, directory: &Path) -> std::io::Result<PathBuf> {
    tokio::fs::create_dir_all(directory).await?;
    let path = directory.join(filename);
    tokio::fs::write(&path, text.as_bytes()).await?;
    Ok(path)
}

/// Maps to: CC `commands/copy/copy.tsx:78-83` async `writeToFile`.
pub async fn write_to_file(text: &str, filename: &str) -> std::io::Result<PathBuf> {
    write_to_dir(text, filename, &COPY_DIR).await
}

/// Maps to: CC `commands/copy/copy.tsx:85-101` async `copyOrWriteToFile`.
/// The canonical clipboard owner performs native/tmux/OSC dispatch; the
/// existing StdoutHandle queues only the returned raw terminal sequence.
pub fn copy_or_write_to_file(
    stdout: &StdoutHandle,
    text: &str,
    filename: &str,
) -> impl std::future::Future<Output = std::io::Result<String>> + Send + 'static {
    // CC's promise continues even when its picker / observer is unmounted.
    // PORTING A3 process-runtime carrier keeps the entire raw + file fallback
    // chain alive; dropping the JoinHandle waiter does not abort the task.
    let stdout = stdout.clone();
    let text = text.to_owned();
    let filename = filename.to_owned();
    let copy = stdout.prepare_clipboard(&text);
    let task = crate::utils::process_runtime::runtime_handle_for_detached_work()
        .expect("clipboard requires the initialized process runtime")
        .spawn(async move {
            let raw = copy.await;
            if !raw.is_empty() {
                stdout.write_control_sequence_and_wait(raw).await?;
            }
            let copied = format!(
                "Copied to clipboard ({} characters, {} lines)",
                js_string_length(&text),
                line_count(&text)
            );
            Ok(match write_to_file(&text, &filename).await {
                Ok(path) => format!("{copied}\nAlso written to {}", path.display()),
                Err(_) => copied,
            })
        });
    async move { task.await.expect("clipboard process task panicked") }
}

fn picker_options(data: &CopyPickerData) -> Vec<SelectOptionData> {
    let mut options = vec![SelectOptionData {
        label: "Full response".to_string(),
        value: PickerSelection::Full.value(),
        description: Some(format!(
            "{} chars, {} lines",
            js_string_length(&data.full_text),
            line_count(&data.full_text)
        )),
        dim_description: true,
        disabled: false,
        input: None,
    }];

    options.extend(data.code_blocks.iter().enumerate().map(|(index, block)| {
        let block_lines = line_count(&block.code);
        let description = [
            block.lang.clone(),
            (block_lines > 1).then(|| format!("{block_lines} lines")),
        ]
        .into_iter()
        .flatten()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(", ");
        SelectOptionData {
            label: truncate_line(&block.code, 60),
            value: PickerSelection::Block(index).value(),
            description: (!description.is_empty()).then_some(description),
            dim_description: true,
            disabled: false,
            input: None,
        }
    }));

    options.push(SelectOptionData {
        label: "Always copy full response".to_string(),
        value: PickerSelection::Always.value(),
        description: Some("Skip this picker in the future (revert via /config)".to_string()),
        dim_description: true,
        disabled: false,
        input: None,
    });
    options
}

fn selection_content(data: &CopyPickerData, selection: &PickerSelection) -> Option<CopyContent> {
    match selection {
        PickerSelection::Full | PickerSelection::Always => Some(CopyContent {
            text: data.full_text.clone(),
            filename: RESPONSE_FILENAME.to_string(),
        }),
        PickerSelection::Block(index) => {
            let block = data.code_blocks.get(*index)?;
            Some(CopyContent {
                text: block.code.clone(),
                filename: format!("copy{}", file_extension(block.lang.as_deref())),
            })
        }
    }
}

#[derive(Clone, Debug)]
enum CopyPickerAction {
    Copy(PickerSelection),
    Write(PickerSelection),
}

#[derive(Default, Props)]
pub struct CopyPickerProps {
    /// L1 carrier for source process.stdout; the REPL owns its output lifetime.
    pub command_stdout: Option<StdoutHandle>,
    pub data: Option<CopyPickerData>,
    pub on_done: Handler<String>,
}

/// Maps to: CC `commands/copy/copy.tsx:122-262` `CopyPicker`.
#[component]
pub fn CopyPicker(props: &mut CopyPickerProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let data = props.data.clone().unwrap_or(CopyPickerData {
        full_text: String::new(),
        code_blocks: Vec::new(),
        message_age: 0,
    });
    let options = picker_options(&data);
    let select_state = use_select_state(
        &mut hooks,
        UseSelectStateProps {
            visible_option_count: Some(5),
            values: options.iter().map(|option| option.value.clone()).collect(),
            default_value: None,
            focus_value: Some(PickerSelection::Full.value()),
        },
    );
    let select_events = use_select_input(
        &mut hooks,
        select_state,
        UseSelectInputOptions {
            has_on_cancel: true,
            option_metas: options
                .iter()
                .map(|option| SelectInputOptionMeta {
                    value: option.value.clone(),
                    disabled: option.disabled,
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        },
    );
    let mut write_requested = hooks.use_state(|| false);
    let (local_stdout, _) = hooks.use_output();
    let stdout = props.command_stdout.clone().unwrap_or(local_stdout);
    let action = Handler::from({
        let data = data.clone();
        let on_done = props.on_done.clone();
        move |action: CopyPickerAction| {
            let on_done = on_done.clone();
            let runtime = crate::utils::process_runtime::runtime_handle_for_detached_work()
                .expect("copy requires the initialized process runtime");
            match action {
                CopyPickerAction::Copy(selection) => {
                    let Some(content) = selection_content(&data, &selection) else {
                        return;
                    };
                    let always = selection == PickerSelection::Always;
                    if always
                        && !crate::utils::config::load_global_config()
                            .copy_full_response
                            .unwrap_or(false)
                    {
                        if let Err(error) = crate::utils::config::save_global_config(|config| {
                            config.copy_full_response = Some(true);
                        }) {
                            crate::utils::debug::log_for_debugging(&format!(
                                "Failed to save copyFullResponse preference: {error}"
                            ));
                        }
                    }
                    // CC :180-209 executes its pre-await prefix on the event
                    // stack. The eager clipboard future starts native/tmux now.
                    let copy = copy_or_write_to_file(&stdout, &content.text, &content.filename);
                    runtime.spawn(async move {
                        let result = match copy.await {
                            Ok(result) => result,
                            Err(error) => {
                                crate::utils::debug::log_for_debugging(&error.to_string());
                                return;
                            }
                        };
                        let result = if always {
                            format!(
                                "{result}\nPreference saved. Use /config to change copyFullResponse"
                            )
                        } else {
                            result
                        };
                        on_done(result);
                    });
                }
                CopyPickerAction::Write(selection) => {
                    let Some(content) = selection_content(&data, &selection) else {
                        return;
                    };
                    runtime.spawn(async move {
                        let result = match write_to_file(&content.text, &content.filename).await {
                            Ok(path) => format!("Written to {}", path.display()),
                            Err(error) => format!("Failed to write file: {error}"),
                        };
                        on_done(result);
                    });
                }
            }
        }
    });
    let keybinding_runtime = hooks
        .try_use_context::<KeybindingRuntime>()
        .map(|runtime| runtime.clone());
    use_keybinding(
        &mut hooks,
        keybinding_runtime,
        "copy:write",
        ContextName::Custom(COPY_PICKER_CONTEXT.to_string()),
        || true,
        move || {
            write_requested.set(true);
            true
        },
    );

    if let Some(value) = select_events.take_accepted() {
        if let Some(selection) = PickerSelection::from_value(&value) {
            action(CopyPickerAction::Copy(selection));
        }
    }
    if write_requested.get() {
        write_requested.set(false);
        if let Some(value) = select_state.focused_value() {
            if let Some(selection) = PickerSelection::from_value(&value) {
                action(CopyPickerAction::Write(selection));
            }
        }
    }
    if select_events.take_cancelled() {
        (props.on_done)("Copy cancelled".to_string());
    }

    let navigation = select_state.navigation.snapshot();
    element! {
        Pane {
            View(flex_direction: FlexDirection::Column, gap: 1u32) {
                Text(content: "Select content to copy:".to_string(), dim: true)
                Select(
                    options: options,
                    focused_index: navigation.focused_index().unwrap_or(0),
                    visible_option_count: navigation.visible_option_count,
                    visible_from_index: navigation.visible_from_index,
                    hide_indexes: false,
                    layout: SelectLayout::Expanded,
                )
                ContextProvider(value: Context::owned(KeyboardShortcutHintStyleContext {
                    dim: true,
                    italic: false,
                })) {
                    Byline {
                        KeyboardShortcutHint(shortcut: "enter".to_string(), action: "copy".to_string())
                        KeyboardShortcutHint(shortcut: "w".to_string(), action: "write to file".to_string())
                        KeyboardShortcutHint(shortcut: "esc".to_string(), action: "cancel".to_string())
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::message::{AssistantMessage, StopReason, SystemMessage};
    use chrono::Utc;

    fn assistant(content: Vec<AssistantContent>) -> Message {
        Message::Assistant(AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            content,
            model: None,
            stop_reason: Some(StopReason::EndTurn),
            usage: None,
        })
    }

    #[test]
    fn collect_recent_assistant_texts_matches_official_filter_and_order() {
        let mut messages = vec![
            assistant(vec![AssistantContent::Text("old".to_string())]),
            assistant(vec![AssistantContent::ToolUse(
                crate::types::message::ToolUseBlock {
                    id: crate::types::ids::ToolUseId("tool-1".to_string()),
                    name: "Read".to_string(),
                    input: serde_json::json!({}),
                },
            )]),
            Message::System(SystemMessage::ApiError {
                base: crate::types::message::SystemBase::new(),
                error: "API error".to_string(),
                retry_in_ms: 0,
                retry_attempt: 0,
                max_retries: 0,
            }),
            assistant(vec![
                AssistantContent::Text("new one".to_string()),
                AssistantContent::Text("new two".to_string()),
            ]),
        ];
        for index in 0..25 {
            messages.insert(
                0,
                assistant(vec![AssistantContent::Text(format!("archive-{index}"))]),
            );
        }

        let texts = collect_recent_assistant_texts(&messages);
        assert_eq!(texts.len(), 20);
        assert_eq!(texts[0], "new one\n\nnew two");
        assert_eq!(texts[1], "old");
    }

    #[test]
    fn extract_code_blocks_and_extensions_match_official_marked_path() {
        let blocks = extract_code_blocks(
            "<system-reminder>hidden</system-reminder>\n```tsx\nconst x = 1\n```\n\n```../../plaintext\nunsafe\n```",
        );
        assert_eq!(
            blocks,
            vec![
                CodeBlock {
                    code: "const x = 1".to_string(),
                    lang: Some("tsx".to_string()),
                },
                CodeBlock {
                    code: "unsafe".to_string(),
                    lang: Some("../../plaintext".to_string()),
                },
            ]
        );
        assert_eq!(file_extension(Some("tsx")), ".tsx");
        assert_eq!(file_extension(Some("../../etc/passwd")), ".etcpasswd");
        assert_eq!(file_extension(Some("plaintext")), ".txt");
        assert_eq!(file_extension(None), ".txt");
    }

    #[test]
    fn copy_call_matches_official_age_validation_and_picker_gate() {
        let messages = vec![
            assistant(vec![AssistantContent::Text("first".to_string())]),
            assistant(vec![AssistantContent::Text(
                "answer\n```rust\nfn main() {}\n```".to_string(),
            )]),
        ];

        assert!(matches!(
            prepare_call(&messages, "", false),
            CopyCall::Picker(_)
        ));
        assert_eq!(
            prepare_call(&messages, "2", false),
            CopyCall::Direct(CopyContent {
                text: "first".to_string(),
                filename: RESPONSE_FILENAME.to_string(),
            })
        );
        assert!(matches!(
            prepare_call(&messages, "", true),
            CopyCall::Direct(CopyContent { text, .. }) if text.starts_with("answer")
        ));
        assert_eq!(
            prepare_call(&messages, "3", false),
            CopyCall::Output {
                text: "Only 2 assistant messages available to copy".to_string(),
                is_error: false,
            }
        );
        assert_eq!(
            prepare_call(&messages, "1e0", false),
            prepare_call(&messages, "1", false)
        );
        assert_eq!(
            prepare_call(&messages, "0x2", false),
            prepare_call(&messages, "2", false)
        );
        assert_eq!(
            prepare_call(&messages, "1.5", false),
            CopyCall::Output {
                text: "Usage: /copy [N] where N is 1 (latest), 2, 3, … Got: 1.5".to_string(),
                is_error: false,
            }
        );
    }

    #[test]
    fn copy_picker_options_use_utf16_counts_and_official_labels() {
        let data = CopyPickerData {
            full_text: "😀\ntext".to_string(),
            code_blocks: vec![CodeBlock {
                code: "println!(\"hello\");".to_string(),
                lang: Some("rust".to_string()),
            }],
            message_age: 0,
        };
        let options = picker_options(&data);
        assert_eq!(options[0].label, "Full response");
        assert_eq!(options[0].description.as_deref(), Some("7 chars, 2 lines"));
        assert_eq!(options[1].description.as_deref(), Some("rust"));
        assert_eq!(options[2].label, "Always copy full response");
    }

    #[test]
    fn copy_picker_renders_official_prompt_options_and_byline() {
        let data = CopyPickerData {
            full_text: "answer\n```rust\nfn main() {}\n```".to_string(),
            code_blocks: vec![CodeBlock {
                code: "fn main() {}".to_string(),
                lang: Some("rust".to_string()),
            }],
            message_age: 0,
        };
        let canvas = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                CopyPicker(data: Some(data), on_done: |_| {})
            }
        }
        .render(Some(100));
        let text = canvas.to_string();
        assert!(text.contains("Select content to copy:"), "canvas=\n{text}");
        assert!(text.contains("Full response"), "canvas=\n{text}");
        assert!(
            text.contains("Always copy full response"),
            "canvas=\n{text}"
        );
        assert!(text.contains("enter to copy"), "canvas=\n{text}");
        assert!(text.contains("w to write to file"), "canvas=\n{text}");
        assert!(text.contains("esc to cancel"), "canvas=\n{text}");
    }

    #[tokio::test]
    async fn write_to_file_uses_official_filename_under_selected_directory() {
        let directory =
            std::env::temp_dir().join(format!("cometix-copy-write-test-{}", uuid::Uuid::new_v4()));
        let path = write_to_dir("copied text", "copy.rs", &directory)
            .await
            .unwrap();
        assert_eq!(path, directory.join("copy.rs"));
        assert_eq!(
            tokio::fs::read_to_string(&path).await.unwrap(),
            "copied text"
        );
        let _ = tokio::fs::remove_dir_all(directory).await;
    }

    #[test]
    fn truncate_line_matches_official_terminal_width_ellipsis() {
        assert_eq!(truncate_line("short\nignored", 10), "short");
        assert_eq!(truncate_line("你好世界", 5), "你好…");
    }

    #[derive(Default, Props)]
    struct CopyAwaitHarnessProps {
        stdout: Option<StdoutHandle>,
        filename: String,
        result: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    }

    #[component]
    fn CopyAwaitHarness(
        props: &CopyAwaitHarnessProps,
        mut hooks: Hooks,
    ) -> impl Into<AnyElement<'static>> {
        let mut started = hooks.use_state(|| false);
        let stdout = props.stdout.clone().expect("parent owns clipboard output");
        let result = props.result.clone();
        let filename = props.filename.clone();
        let copy = hooks.use_async_handler(move |(): ()| {
            let stdout = stdout.clone();
            let result = result.clone();
            let filename = filename.clone();
            async move {
                let output = copy_or_write_to_file(&stdout, "😀\ncopy fixture", &filename)
                    .await
                    .unwrap();
                *result.lock().unwrap() = Some(output);
            }
        });
        if !started.get() {
            started.set(true);
            copy(());
        }
        element! { Text(content: "copy fixture mounted") }
    }

    #[derive(Default, Props)]
    struct CopyUnmountHarnessProps {
        filename: String,
        result: std::sync::Arc<std::sync::Mutex<Option<String>>>,
        drop_observer: bool,
    }

    #[component]
    fn CopyUnmountHarness(
        props: &CopyUnmountHarnessProps,
        mut hooks: Hooks,
    ) -> impl Into<AnyElement<'static>> {
        let (stdout, _) = hooks.use_output();
        let mut mounted = hooks.use_state(|| true);
        let drop_observer = props.drop_observer;
        hooks.use_future(async move {
            if drop_observer {
                futures_timer::Delay::new(std::time::Duration::from_millis(30)).await;
                mounted.set(false);
            }
        });
        if mounted.get() {
            element! { CopyAwaitHarness(stdout: Some(stdout), filename: props.filename.clone(), result: props.result.clone()) }.into_any()
        } else {
            element! { Text(content: "copy parent after unmount") }.into_any()
        }
    }

    #[derive(Default, Props)]
    struct OutputOwnerProbeProps {
        output: std::sync::Arc<std::sync::Mutex<Option<StdoutHandle>>>,
    }
    #[component]
    fn OutputOwnerProbe(
        props: &OutputOwnerProbeProps,
        mut hooks: Hooks,
    ) -> impl Into<AnyElement<'static>> {
        let (stdout, _) = hooks.use_output();
        *props.output.lock().unwrap() = Some(stdout);
        element! { Text(content: "output owner") }
    }

    /// CC :90 write occurs outside the fallback catch. A terminal failure
    /// rejects the copy operation before any file effect or success result.
    #[tokio::test]
    async fn copy_clipboard_write_failure_prevents_fallback_and_success() {
        use futures::StreamExt;
        crate::utils::process_runtime::initialize_test_process_runtime();
        let _ssh = crate::utils::env_utils::EnvVarGuard::set("SSH_CONNECTION", "fixture");
        let _tmux = crate::utils::env_utils::EnvVarGuard::unset("TMUX");
        let output = std::sync::Arc::new(std::sync::Mutex::new(None));
        {
            let mut app = element! {
                ContextProvider(value: Context::owned(iocraft::Clipboard::new(std::sync::Arc::new(crate::utils::exec_file_no_throw::ExecFileClipboardBackend)))) {
                    OutputOwnerProbe(output: output.clone())
                }
            };
            let mut frames = Box::pin(app.mock_terminal_render_loop(MockTerminalConfig::default()));
            crate::utils::race(frames.next(), async {
                futures_timer::Delay::new(std::time::Duration::from_secs(2)).await;
                None
            })
            .await
            .expect("output owner mounted");
        }
        let stdout = output.lock().unwrap().clone().unwrap();
        let filename = format!("copy-failed-{}.txt", uuid::Uuid::new_v4());
        let error = crate::utils::race(
            async { Some(copy_or_write_to_file(&stdout, "not written", &filename).await) },
            async {
                futures_timer::Delay::new(std::time::Duration::from_secs(2)).await;
                None
            },
        )
        .await
        .expect("unmounted output must reject promptly")
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
        assert!(!COPY_DIR.join(filename).exists());
    }

    /// CC copy.tsx:85-101: copy completes before the reliable temp-file fallback
    /// and feedback. SSH and a fake tmux avoid the real system clipboard.
    #[cfg(unix)]
    #[tokio::test]
    async fn copy_clipboard_matches_official_file_fallback_and_feedback() {
        use futures::StreamExt;
        use std::os::unix::fs::PermissionsExt;
        crate::utils::process_runtime::initialize_test_process_runtime();
        let directory =
            std::env::temp_dir().join(format!("cometix-copy-clipboard-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let tool = directory.join("tmux");
        std::fs::write(&tool, "#!/bin/sh\n/bin/cat >/dev/null\n/bin/sleep 0.25\n").unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _path = crate::utils::env_utils::EnvVarGuard::set("PATH", &directory);
        let _ssh = crate::utils::env_utils::EnvVarGuard::set("SSH_CONNECTION", "fixture");
        let _tmux = crate::utils::env_utils::EnvVarGuard::set("TMUX", "fixture");
        for drop_observer in [false, true] {
            let filename = format!("copy-fixture-{}.txt", uuid::Uuid::new_v4());
            let result = std::sync::Arc::new(std::sync::Mutex::new(None));
            let mut app = element! { CopyUnmountHarness(filename: filename.clone(), result: result.clone(), drop_observer) };
            let mut app = element! {
                ContextProvider(value: Context::owned(iocraft::Clipboard::new(std::sync::Arc::new(crate::utils::exec_file_no_throw::ExecFileClipboardBackend)))) {
                    #(app)
                }
            };
            let mut output = Box::pin(
                app.mock_terminal_render_loop(MockTerminalConfig::default().with_size(80, 20)),
            );
            let collect = async { while output.next().await.is_some() {} };
            let wait = async {
                if drop_observer {
                    futures_timer::Delay::new(std::time::Duration::from_millis(650)).await;
                    return;
                }
                for _ in 0..100 {
                    if result.lock().unwrap().is_some() {
                        return;
                    }
                    futures_timer::Delay::new(std::time::Duration::from_millis(10)).await;
                }
            };
            crate::utils::race(collect, wait).await;
            drop(output);
            // Parent output remains mounted while the child observer is
            // dropped, so acknowledged raw precedes the real file effect.
            let path = COPY_DIR.join(&filename);
            if drop_observer {
                assert_eq!(*result.lock().unwrap(), None);
            } else {
                assert_eq!(
                    result.lock().unwrap().as_deref(),
                    Some(
                        format!(
                            "Copied to clipboard (15 characters, 2 lines)\nAlso written to {}",
                            path.display()
                        )
                        .as_str()
                    )
                );
            }
            assert_eq!(
                tokio::fs::read_to_string(&path).await.unwrap(),
                "😀\ncopy fixture"
            );
            tokio::fs::remove_file(path).await.unwrap();
        }
        std::fs::remove_dir_all(&directory).unwrap();
    }
}
