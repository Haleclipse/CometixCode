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

/// Cometix-specific deviation (10-03, user): `/copy` writes where CC 2.1.285
/// does. 2.1.88 (`copy.tsx:23`) uses `join(tmpdir(), 'claude')`, on macOS the
/// per-user `/var/folders/…/T/claude`. 2.1.285's `writeToFile` takes its
/// shared per-uid temp root, `join(CLAUDE_CODE_TMPDIR || '/tmp', 'claude-' +
/// (getuid() ?? 0))` (`/tmp/claude-<uid>` on macOS too; the variable trimmed,
/// as 2.1.285's environment reads are), created 0700 and checked to be the
/// user's own (`verify_owned_temp_dir`). 2.1.285 checks once per path; this
/// checks on every call (user, 10-03): the root is sandbox-writable, and a
/// sandboxed command could replace it with a symlink after a first check.
/// Its refusal in a diskless session has no 2.1.88 counterpart and is not
/// ported.
fn copy_dir() -> std::io::Result<PathBuf> {
    use crate::utils::process_env::JsTruthy as _;
    let base = crate::utils::process_env::var("CLAUDE_CODE_TMPDIR")
        .map(|value| value.trim().to_owned())
        .truthy()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    let dir = crate::utils::fs_operations::native::join_path(
        &base,
        Path::new(&format!(
            "claude-{}",
            crate::utils::permissions::filesystem::current_uid()
        )),
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)?;
        crate::utils::permissions::filesystem::verify_owned_temp_dir(&dir)?;
    }
    #[cfg(not(unix))]
    let _ = std::fs::create_dir_all(&dir);
    Ok(dir)
}

/// 2.1.285's `writeToFile` writes through its atomic writer, which refuses a
/// symlink at the target: the directory is the sandbox-writable per-uid temp
/// root, where sandboxed commands could plant one. A sibling temp file opened
/// `O_EXCL | O_NOFOLLOW` is renamed over the target, which replaces a link
/// planted in between instead of following it; an existing file keeps its
/// mode. Not ported: its in-place write fallback, taken when the rename fails
/// with certain errors or the temp file cannot be created (`EACCES`) next to
/// an existing target.
async fn write_to_dir(text: &str, filename: &str, directory: &Path) -> std::io::Result<PathBuf> {
    let path = directory.join(filename);
    let mut builder = tokio::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    builder.mode(0o700);
    builder.create(directory).await?;

    let existing = match tokio::fs::symlink_metadata(&path).await {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(std::io::Error::other(format!(
                "Refusing to write through symlink: {}. Resolve the symlink and pass the real target path explicitly.",
                path.display()
            )));
        }
        Ok(metadata) => Some(metadata.permissions()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let mut temp = path.clone().into_os_string();
    temp.push(format!(
        ".tmp.{}.{}",
        std::process::id(),
        &uuid::Uuid::new_v4().simple().to_string()[..12]
    ));
    let temp = PathBuf::from(temp);
    let written = async {
        use tokio::io::AsyncWriteExt as _;
        let mut options = tokio::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.custom_flags(libc::O_NOFOLLOW);
        let mut file = options.open(&temp).await?;
        file.write_all(text.as_bytes()).await?;
        // `pHe`: a filesystem that cannot fchmod or fsync is logged, not fatal.
        let unsupported = |error: &std::io::Error| {
            #[cfg(unix)]
            return matches!(
                error.raw_os_error(),
                Some(libc::EINVAL | libc::ENOTSUP | libc::EPERM | libc::ENOSYS)
            );
            #[cfg(not(unix))]
            {
                let _ = error;
                false
            }
        };
        if let Some(permissions) = existing {
            match file.set_permissions(permissions).await {
                Err(error) if unsupported(&error) => crate::utils::debug::log_for_debugging(
                    &format!("fchmod unsupported on this filesystem: {error}"),
                ),
                result => result?,
            }
        }
        match file.sync_all().await {
            Err(error) if unsupported(&error) => crate::utils::debug::log_for_debugging(&format!(
                "fsync unsupported on this filesystem: {error}"
            )),
            result => result?,
        }
        drop(file);
        tokio::fs::rename(&temp, &path).await
    }
    .await;
    if let Err(error) = written {
        let _ = tokio::fs::remove_file(&temp).await;
        return Err(error);
    }
    Ok(path)
}

/// Maps to: CC `commands/copy/copy.tsx:78-83` async `writeToFile`, into the
/// 2.1.285 directory ([`copy_dir`]).
pub async fn write_to_file(text: &str, filename: &str) -> std::io::Result<PathBuf> {
    write_to_dir(text, filename, &copy_dir()?).await
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
    use crate::utils::test_env::{
        CLIPBOARD_OFF_SYSTEM, EnvVarGuard, TEST_ENV_LOCK, in_child_process,
    };
    use chrono::Utc;

    /// 2.1.285: the per-uid root under `CLAUDE_CODE_TMPDIR`, made 0700.
    #[cfg(unix)]
    #[test]
    fn copy_dir_is_the_per_uid_root_made_private() {
        use std::os::unix::fs::PermissionsExt as _;
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!("cometix-copy-dir-{}", uuid::Uuid::new_v4()));
        let expected = root.join(format!(
            "claude-{}",
            crate::utils::permissions::filesystem::current_uid()
        ));
        std::fs::create_dir_all(&expected).unwrap();
        std::fs::set_permissions(&expected, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _tmp = EnvVarGuard::set("CLAUDE_CODE_TMPDIR", &root);
        assert_eq!(copy_dir().unwrap(), expected);
        assert_eq!(
            std::fs::metadata(&expected).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// 2.1.285's atomic writer refuses a symlink planted at the target, as a
    /// sandboxed command could in the sandbox-writable temp root, and leaves
    /// what it points at alone; a regular file is replaced.
    #[cfg(unix)]
    #[tokio::test]
    async fn write_refuses_a_symlinked_target() {
        let root =
            std::env::temp_dir().join(format!("cometix-copy-target-{}", uuid::Uuid::new_v4()));
        let dir = root.join("dir");
        std::fs::create_dir_all(&dir).unwrap();
        let outside = root.join("outside");
        std::fs::write(&outside, "untouched").unwrap();
        std::os::unix::fs::symlink(&outside, dir.join("response.md")).unwrap();
        let error = write_to_dir("planted", "response.md", &dir)
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Refusing to write through symlink"),
            "{error}"
        );
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "untouched");

        std::fs::write(dir.join("copy.sh"), "old").unwrap();
        let path = write_to_dir("new", "copy.sh", &dir).await.unwrap();
        assert_eq!(std::fs::read_to_string(path).unwrap(), "new");
        let leftovers = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp."))
            .count();
        assert_eq!(leftovers, 0);
        let _ = std::fs::remove_dir_all(root);
    }

    /// 2.1.285 refuses a root that is a symlink, which another user could
    /// plant on a shared `/tmp`.
    /// Checked on every call (user, 10-03; 2.1.285 checks once per path): a
    /// loosened mode is fixed again, and a root replaced by a symlink after
    /// the first call is refused. `CLAUDE_CODE_TMPDIR` is trimmed.
    #[cfg(unix)]
    #[test]
    fn copy_dir_rechecks_the_root_on_every_call() {
        use std::os::unix::fs::PermissionsExt as _;
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let root =
            std::env::temp_dir().join(format!("cometix-copy-again-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let _tmp = EnvVarGuard::set("CLAUDE_CODE_TMPDIR", format!(" {} ", root.display()));
        let dir = copy_dir().unwrap();
        assert_eq!(dir.parent(), Some(root.as_path()));
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        copy_dir().unwrap();
        assert_eq!(
            std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        std::fs::remove_dir(&dir).unwrap();
        let elsewhere = root.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &dir).unwrap();
        assert!(copy_dir().is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    /// 2.1.285: the user's own root that cannot be opened is refused as not
    /// readable.
    #[cfg(unix)]
    #[test]
    fn copy_dir_refuses_an_unreadable_root() {
        use std::os::unix::fs::PermissionsExt as _;
        if crate::utils::permissions::filesystem::current_uid() == 0 {
            return; // root opens it anyway
        }
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let root =
            std::env::temp_dir().join(format!("cometix-copy-unread-{}", uuid::Uuid::new_v4()));
        let dir = root.join(format!(
            "claude-{}",
            crate::utils::permissions::filesystem::current_uid()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o000)).unwrap();
        let _tmp = EnvVarGuard::set("CLAUDE_CODE_TMPDIR", &root);
        let error = copy_dir().unwrap_err();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(error.to_string().contains("is not readable"), "{error}");
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn copy_dir_refuses_a_symlinked_root() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!("cometix-copy-link-{}", uuid::Uuid::new_v4()));
        let target = root.join("elsewhere");
        std::fs::create_dir_all(&target).unwrap();
        std::os::unix::fs::symlink(
            &target,
            root.join(format!(
                "claude-{}",
                crate::utils::permissions::filesystem::current_uid()
            )),
        )
        .unwrap();
        let _tmp = EnvVarGuard::set("CLAUDE_CODE_TMPDIR", &root);
        let error = copy_dir().unwrap_err();
        assert!(error.to_string().contains("not a directory"), "{error}");
        let _ = std::fs::remove_dir_all(root);
    }

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
        // iocraft reads the session's terminal from the OS environment.
        if !in_child_process(
            module_path!(),
            "copy_clipboard_write_failure_prevents_fallback_and_success",
            CLIPBOARD_OFF_SYSTEM,
        ) {
            return;
        }
        crate::utils::process_runtime::initialize_test_process_runtime();
        // Keep `copy_dir()` off the real per-uid temp root.
        let _tmp = EnvVarGuard::set(
            "CLAUDE_CODE_TMPDIR",
            std::env::temp_dir().join(format!("cometix-copy-fail-{}", uuid::Uuid::new_v4())),
        );
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
        assert!(!copy_dir().unwrap().join(filename).exists());
    }

    /// CC copy.tsx:85-101: copy completes before the reliable temp-file fallback
    /// and feedback. SSH and a fake tmux avoid the real system clipboard; iocraft
    /// reads the session's terminal from the OS environment, hence the child.
    #[cfg(unix)]
    #[tokio::test]
    async fn copy_clipboard_matches_official_file_fallback_and_feedback() {
        use futures::StreamExt;
        use std::os::unix::fs::PermissionsExt;
        if !in_child_process(
            module_path!(),
            "copy_clipboard_matches_official_file_fallback_and_feedback",
            &[
                ("SSH_CONNECTION", Some("fixture")),
                ("TMUX", Some("fixture")),
            ],
        ) {
            return;
        }
        crate::utils::process_runtime::initialize_test_process_runtime();
        let directory =
            std::env::temp_dir().join(format!("cometix-copy-clipboard-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let tool = directory.join("tmux");
        std::fs::write(&tool, "#!/bin/sh\n/bin/cat >/dev/null\n/bin/sleep 0.25\n").unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _path = EnvVarGuard::set("PATH", &directory);
        // Keep `copy_dir()` off the real per-uid temp root.
        let _tmp = EnvVarGuard::set("CLAUDE_CODE_TMPDIR", &directory);
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
            let path = copy_dir().unwrap().join(&filename);
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
