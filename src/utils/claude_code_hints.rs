//! Claude Code shell hint protocol.
//!
//! Maps to: CC `utils/claudeCodeHints.ts:1-176`.

use regex::Regex;
use std::sync::{LazyLock, Mutex};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaudeCodeHint {
    pub version: u32,
    pub hint_type: String,
    pub value: String,
    pub source_command: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtractedClaudeCodeHints {
    pub hints: Vec<ClaudeCodeHint>,
    pub stripped: String,
}

static HINT_TAG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^[ \t]*<claude-code-hint\s+([^>]*?)\s*/>[ \t]*$")
        .expect("valid Claude Code hint tag regex")
});
static ATTRIBUTE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(\w+)=(?:"([^"]*)"|([^\s/>]+))"#).expect("valid hint attribute regex")
});
static PENDING_HINT: LazyLock<Mutex<Option<ClaudeCodeHint>>> = LazyLock::new(|| Mutex::new(None));
static SHOWN_THIS_SESSION: LazyLock<std::sync::atomic::AtomicBool> =
    LazyLock::new(|| std::sync::atomic::AtomicBool::new(false));

fn first_command_token(command: &str) -> String {
    command
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_string()
}

fn parse_attrs(tag: &str) -> std::collections::HashMap<String, String> {
    ATTRIBUTE
        .captures_iter(tag)
        .filter_map(|captures| {
            Some((
                captures.get(1)?.as_str().to_string(),
                captures
                    .get(2)
                    .or_else(|| captures.get(3))?
                    .as_str()
                    .to_string(),
            ))
        })
        .collect()
}

fn parse_js_number_version(value: &str) -> Option<u32> {
    let value = value.trim();
    let parsed = if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        u32::from_str_radix(hex, 16).ok().map(f64::from)
    } else if let Some(binary) = value
        .strip_prefix("0b")
        .or_else(|| value.strip_prefix("0B"))
    {
        u32::from_str_radix(binary, 2).ok().map(f64::from)
    } else if let Some(octal) = value
        .strip_prefix("0o")
        .or_else(|| value.strip_prefix("0O"))
    {
        u32::from_str_radix(octal, 8).ok().map(f64::from)
    } else {
        value.parse::<f64>().ok()
    }?;
    (parsed.is_finite() && parsed.fract() == 0.0 && parsed >= 0.0 && parsed <= u32::MAX as f64)
        .then_some(parsed as u32)
}

/// Maps to CC `extractClaudeCodeHints(output, command)`.
pub fn extract_claude_code_hints(output: &str, command: &str) -> ExtractedClaudeCodeHints {
    if !output.contains("<claude-code-hint") {
        return ExtractedClaudeCodeHints {
            hints: Vec::new(),
            stripped: output.to_string(),
        };
    }
    let source_command = first_command_token(command);
    let mut hints = Vec::new();
    let stripped = HINT_TAG
        .replace_all(output, |captures: &regex::Captures<'_>| {
            let attrs = parse_attrs(captures.get(0).map_or("", |value| value.as_str()));
            let version = attrs
                .get("v")
                .and_then(|value| parse_js_number_version(value));
            let hint_type = attrs.get("type").cloned();
            let value = attrs.get("value").cloned();
            if version == Some(1)
                && hint_type.as_deref() == Some("plugin")
                && value.as_ref().is_some_and(|value| !value.is_empty())
            {
                hints.push(ClaudeCodeHint {
                    version: 1,
                    hint_type: "plugin".to_string(),
                    value: value.unwrap_or_default(),
                    source_command: source_command.clone(),
                });
            }
            ""
        })
        .into_owned();
    let collapsed = if stripped != output || !hints.is_empty() {
        let runs = Regex::new(r"\n{3,}").expect("valid blank-line regex");
        runs.replace_all(&stripped, "\n\n").into_owned()
    } else {
        stripped
    };
    ExtractedClaudeCodeHints {
        hints,
        stripped: collapsed,
    }
}

pub fn set_pending_hint(hint: ClaudeCodeHint) {
    if SHOWN_THIS_SESSION.load(std::sync::atomic::Ordering::Acquire) {
        return;
    }
    if let Ok(mut pending) = PENDING_HINT.lock() {
        *pending = Some(hint);
    }
}

pub fn get_pending_hint_snapshot() -> Option<ClaudeCodeHint> {
    PENDING_HINT.lock().ok().and_then(|hint| hint.clone())
}

pub fn clear_pending_hint() {
    if let Ok(mut pending) = PENDING_HINT.lock() {
        *pending = None;
    }
}

pub fn mark_shown_this_session() {
    SHOWN_THIS_SESSION.store(true, std::sync::atomic::Ordering::Release);
}

pub fn has_shown_hint_this_session() -> bool {
    SHOWN_THIS_SESSION.load(std::sync::atomic::Ordering::Acquire)
}

#[cfg(test)]
pub fn reset_claude_code_hint_store_for_test() {
    clear_pending_hint();
    SHOWN_THIS_SESSION.store(false, std::sync::atomic::Ordering::Release);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_supported_whole_line_hints_and_strips_them() {
        let extracted = extract_claude_code_hints(
            "before\n<claude-code-hint v=1 type=plugin value=foo@anthropic />\nafter",
            "foo run",
        );
        assert_eq!(extracted.stripped, "before\n\nafter");
        assert_eq!(extracted.hints.len(), 1);
        assert_eq!(extracted.hints[0].source_command, "foo");
        assert_eq!(extracted.hints[0].value, "foo@anthropic");

        let numeric = extract_claude_code_hints(
            "<claude-code-hint v=1.0 type=plugin value=foo@anthropic />",
            "foo",
        );
        assert_eq!(numeric.hints.len(), 1);
    }

    #[test]
    fn ignores_embedded_or_unsupported_hint_tags() {
        assert!(
            extract_claude_code_hints("log: <claude-code-hint v=1 type=plugin value=x@y />", "x")
                .hints
                .is_empty()
        );
        assert!(
            extract_claude_code_hints("<claude-code-hint v=2 type=plugin value=x@y />", "x")
                .hints
                .is_empty()
        );
    }
}
