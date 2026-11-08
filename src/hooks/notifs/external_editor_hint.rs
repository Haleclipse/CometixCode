//! Maps to the external-editor hint effect in
//! `components/PromptInput/Notifications.tsx`.
//!
//! Official Claude Code shows an immediate footer notification when the prompt
//! input wraps and an external editor is configured. This module keeps that
//! producer as a pure function over already-known UI/runtime inputs; it does not
//! detect editors, read settings, log analytics, or mutate notification state.

#![allow(dead_code)]

use crate::context::notifications::{Notification, NotificationPriority, NotificationSegment};
use std::path::Path;

pub const EXTERNAL_EDITOR_HINT_KEY: &str = "external-editor-hint";
pub const EXTERNAL_EDITOR_HINT_TIMEOUT_MS: u64 = 5_000;

/// `credential_missing`: the footer is already showing the missing-credential
/// row, which the hint must not displace.
pub fn external_editor_hint_notification_from_state(
    is_input_wrapped: bool,
    is_showing_compact_message: bool,
    credential_missing: bool,
    editor: Option<&str>,
) -> Option<Notification> {
    if !is_input_wrapped || is_showing_compact_message || credential_missing {
        return None;
    }

    let editor = editor.map(str::trim).filter(|value| !value.is_empty())?;
    Some(external_editor_hint_notification(editor))
}

pub fn external_editor_hint_notification(editor: &str) -> Notification {
    let text = format!("ctrl+g to edit in {}", to_ide_display_name(editor));
    Notification::text(
        EXTERNAL_EDITOR_HINT_KEY,
        text.clone(),
        NotificationPriority::Immediate,
    )
    .with_segments(vec![NotificationSegment::text(text).with_dim(true)])
    .with_timeout_ms(EXTERNAL_EDITOR_HINT_TIMEOUT_MS)
}

/// Pure counterpart of official `utils/ide.ts#toIDEDisplayName()` for the editor
/// names used by the PromptInput external-editor hint. It intentionally accepts
/// an explicit string instead of probing the environment.
pub fn to_ide_display_name(editor: &str) -> String {
    let trimmed = editor.trim();
    if trimmed.is_empty() {
        return "IDE".to_string();
    }

    if let Some(name) = editor_display_name(trimmed) {
        return name.to_string();
    }

    let command = trimmed.split_whitespace().next().unwrap_or(trimmed);
    let command_name = Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(command)
        .trim()
        .to_ascii_lowercase();
    if let Some(name) = editor_display_name(&command_name) {
        return name.to_string();
    }

    capitalize(&command_name)
}

fn editor_display_name(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "code" | "vscode" => Some("VS Code"),
        "cursor" => Some("Cursor"),
        "windsurf" => Some("Windsurf"),
        "antigravity" => Some("Antigravity"),
        "pycharm" => Some("PyCharm"),
        "intellij" => Some("IntelliJ IDEA"),
        "webstorm" => Some("WebStorm"),
        "phpstorm" => Some("PhpStorm"),
        "rubymine" => Some("RubyMine"),
        "clion" => Some("CLion"),
        "goland" => Some("GoLand"),
        "rider" => Some("Rider"),
        "datagrip" => Some("DataGrip"),
        "appcode" => Some("AppCode"),
        "dataspell" => Some("DataSpell"),
        "aqua" => Some("Aqua"),
        "gateway" => Some("Gateway"),
        "fleet" => Some("Fleet"),
        "androidstudio" => Some("Android Studio"),
        "vi" | "vim" => Some("Vim"),
        "nano" => Some("nano"),
        "notepad" | "start /wait notepad" => Some("Notepad"),
        "emacs" => Some("Emacs"),
        "subl" => Some("Sublime Text"),
        "atom" => Some("Atom"),
        _ => None,
    }
}

fn capitalize(value: &str) -> String {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    first.to_uppercase().collect::<String>() + chars.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_editor_hint_notification_matches_official_key_copy_priority_and_timeout() {
        let notification = external_editor_hint_notification("code");

        assert_eq!(notification.key, EXTERNAL_EDITOR_HINT_KEY);
        assert_eq!(notification.text, "ctrl+g to edit in VS Code");
        assert_eq!(notification.priority, NotificationPriority::Immediate);
        assert_eq!(notification.timeout_ms, Some(5_000));
        assert!(notification.color.is_none());
        assert_eq!(notification.segments.len(), 1);
        assert_eq!(notification.segments[0].text, "ctrl+g to edit in VS Code");
        assert!(notification.segments[0].dim);
    }

    #[test]
    fn external_editor_hint_from_state_matches_prompt_input_gates() {
        assert!(
            external_editor_hint_notification_from_state(true, false, false, Some("cursor"))
                .is_some()
        );
        assert!(
            external_editor_hint_notification_from_state(false, false, false, Some("cursor"))
                .is_none()
        );
        assert!(
            external_editor_hint_notification_from_state(true, true, false, Some("cursor"))
                .is_none()
        );
        assert!(
            external_editor_hint_notification_from_state(true, false, true, Some("cursor"))
                .is_none()
        );
        assert!(external_editor_hint_notification_from_state(true, false, false, None).is_none());
    }

    #[test]
    fn ide_display_name_matches_official_editor_command_mapping() {
        assert_eq!(to_ide_display_name("cursor"), "Cursor");
        assert_eq!(to_ide_display_name("vscode"), "VS Code");
        assert_eq!(to_ide_display_name("pycharm"), "PyCharm");
        assert_eq!(to_ide_display_name("/usr/bin/code --wait"), "VS Code");
        assert_eq!(to_ide_display_name("vim"), "Vim");
        assert_eq!(to_ide_display_name("start /wait notepad"), "Notepad");
        assert_eq!(to_ide_display_name("custom-editor --flag"), "Custom-editor");
        assert_eq!(to_ide_display_name(""), "IDE");
    }
}
